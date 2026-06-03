//! Community Import review surface host window.
//!
//! Opened from the ato-start Featured Apps cards. Unlike the GitHub
//! Import surface (`import_window` / `ato-import` `index.html`, which
//! *infers* a recipe from a repo), this window queries the community
//! registry (`GET /v1/capsule-tomls?source=`) for **published** recipes
//! and lets the user explicitly pick one. The selected `ctoml_id` is
//! threaded into the launch consent flow as
//! `GuestRoute::CapsuleHandle { community_toml_id: Some(..) }` so the CLI
//! resolves the pre-selected recipe instead of silently inferring.
//!
//! The page is served from `assets/system/ato-import/community.html` and
//! posts IPC under the `ato-import` capsule identity (it shares the
//! capsule's `WebviewCreate` / `WindowsClose` capabilities); the
//! community-specific commands live in `ato_import::ImportCommand`.
//!
//! Flow: open immediately with a `loading` snapshot, fetch candidates on
//! the background executor, then push a `ready` / `empty` / `error`
//! snapshot into the live WebView via `evaluate_script`.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, App, Bounds, Context, Entity, IntoElement, Pixels, Render, Size, WeakEntity,
    Window, WindowBounds, WindowDecorations, WindowOptions, div, px, rgb, size,
};
use gpui_component::TitleBar;
use serde::Serialize;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

use crate::community_api::{self, CommunityCandidate};
use crate::localization::{compose_init_script, resolve_locale};
use crate::system_capsule::broker::SystemCapsuleId;
use crate::system_capsule::ipc as system_ipc;
use crate::window::content_windows::{ContentWindowEntry, ContentWindowKind, OpenContentWindows};
use crate::window::webview_paste::{WebViewPasteShell, WebViewPasteSupport};
use crate::{impl_focusable_via_paste, paste_render_wrap};

const COMMUNITY_HTML: &str = include_str!("../../assets/system/ato-import/community.html");
const COMMUNITY_W: f32 = 720.0;
const COMMUNITY_H: f32 = 640.0;

/// Snapshot injected/pushed into the community review page as
/// `window.__ATO_COMMUNITY_IMPORT__` (initial) and via
/// `window.__atoCommunityImport(<json>)` (updates).
#[derive(Debug, Clone, Serialize)]
pub struct CommunitySnapshot {
    /// Normalized-ish source handle the user launched (e.g.
    /// `github.com/excalidraw/excalidraw`). Echoed back so the page can
    /// render it and so the IPC commands carry the original handle.
    pub source: String,
    /// Display label for the app (e.g. `Excalidraw`).
    pub label: String,
    /// `loading` | `ready` | `empty` | `error`.
    pub status: String,
    pub candidates: Vec<CommunityCandidate>,
    pub error: Option<String>,
}

impl CommunitySnapshot {
    fn loading(source: &str, label: &str) -> Self {
        Self {
            source: source.to_string(),
            label: label.to_string(),
            status: "loading".to_string(),
            candidates: Vec::new(),
            error: None,
        }
    }

    fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "null".to_string())
    }
}

pub struct CommunityImportWindowShell {
    _webview: Option<WebView>,
    window_size: Size<Pixels>,
    paste: WebViewPasteSupport,
}

impl_focusable_via_paste!(CommunityImportWindowShell, paste);

impl WebViewPasteShell for CommunityImportWindowShell {
    fn active_paste_target(&self) -> Option<&WebView> {
        self._webview.as_ref()
    }
}

impl Render for CommunityImportWindowShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_webview_bounds(window);
        paste_render_wrap!(
            div().size_full().bg(rgb(0xffffff)),
            cx,
            &self.paste.focus_handle
        )
    }
}

impl CommunityImportWindowShell {
    fn sync_webview_bounds(&mut self, window: &mut Window) {
        let current = window.bounds().size;
        if current == self.window_size {
            return;
        }
        if let Some(webview) = self._webview.as_ref() {
            let _ = webview.set_bounds(Rect {
                position: LogicalPosition::new(0i32, 0i32).into(),
                size: LogicalSize::new(
                    f32::from(current.width) as u32,
                    f32::from(current.height) as u32,
                )
                .into(),
            });
        }
        self.window_size = current;
    }

    /// Push an updated snapshot into the live page. The JS side guards
    /// with `typeof` so a call before the DOM is ready is ignored.
    fn push_snapshot(&self, snapshot_json: &str) {
        let script = format!(
            "typeof window.__atoCommunityImport==='function'&&window.__atoCommunityImport({snapshot_json})"
        );
        if let Some(webview) = self._webview.as_ref()
            && let Err(error) = webview.evaluate_script(&script)
        {
            tracing::warn!(
                ?error,
                "community-import: evaluate_script(push_snapshot) failed"
            );
        }
    }

    /// Push a recipe-detail payload (`{ id, toml, error }`) into the live page
    /// so the "View recipe" overlay can render the fetched `capsule.toml`.
    fn push_detail(&self, detail_json: &str) {
        let script = format!(
            "typeof window.__atoCommunityImportDetail==='function'&&window.__atoCommunityImportDetail({detail_json})"
        );
        if let Some(webview) = self._webview.as_ref()
            && let Err(error) = webview.evaluate_script(&script)
        {
            tracing::warn!(
                ?error,
                "community-import: evaluate_script(push_detail) failed"
            );
        }
    }
}

/// Slot for the currently-open community-import window's shell, so the
/// background fetch task can push results into it after it completes.
#[derive(Default, Clone)]
pub struct CommunityImportWindowSlot {
    pub shell: Option<WeakEntity<CommunityImportWindowShell>>,
}
impl gpui::Global for CommunityImportWindowSlot {}

/// Open the Community Import review window for `source` and kick off the
/// candidate fetch. The window appears immediately in a loading state;
/// results are pushed in when the background fetch returns.
pub fn open_for_source(cx: &mut App, source: String, label: String) -> Result<AnyWindowHandle> {
    let bounds = Bounds::centered(None, size(px(COMMUNITY_W), px(COMMUNITY_H)), cx);
    let options = WindowOptions {
        titlebar: Some(TitleBar::title_bar_options()),
        focus: true,
        show: true,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };

    let locale = resolve_locale(crate::config::load_config().general.language);
    let initial = CommunitySnapshot::loading(&source, &label);
    // Define an early queuing bridge in the initialization script (runs
    // before the page's own scripts). Without this, a fast background
    // fetch can `evaluate_script(__atoCommunityImport(..))` before
    // community.html has defined that function — the snapshot would be
    // dropped and the window would hang on "loading". The stub buffers the
    // latest snapshot into `__ATO_COMMUNITY_IMPORT_PENDING__`; the page's
    // inline script consumes it and replaces the stub with the real
    // renderer (atomically, on the single JS thread).
    let init_payload = format!(
        "window.__ATO_COMMUNITY_IMPORT__={};\
         window.__ATO_COMMUNITY_IMPORT_PENDING__=null;\
         window.__atoCommunityImport=function(next){{\
           window.__ATO_COMMUNITY_IMPORT__=next;\
           window.__ATO_COMMUNITY_IMPORT_PENDING__=next;\
         }};",
        initial.to_json()
    );
    let composed = compose_init_script(locale, Some(&init_payload));

    let queue = system_ipc::new_queue();
    let shell_slot: Arc<Mutex<Option<Entity<CommunityImportWindowShell>>>> =
        Arc::new(Mutex::new(None));
    let shell_slot_inner = Arc::clone(&shell_slot);
    let queue_for_closure = queue.clone();

    let handle = cx.open_window(options, move |window, cx| {
        window.set_window_title(crate::window::WINDOW_TITLE);
        let win_size = window.bounds().size;
        let webview_rect = Rect {
            position: LogicalPosition::new(0i32, 0i32).into(),
            size: LogicalSize::new(
                f32::from(win_size.width) as u32,
                f32::from(win_size.height) as u32,
            )
            .into(),
        };
        let _wv_guard = crate::webview_init_guard::WebviewInitGuard::new();
        let webview = WebViewBuilder::new()
            .with_html(COMMUNITY_HTML)
            .with_initialization_script(&composed)
            .with_ipc_handler(system_ipc::make_ipc_handler_for_capsule(
                SystemCapsuleId::AtoImport,
                queue_for_closure,
            ))
            .with_bounds(webview_rect);
        let webview =
            crate::window::build_child_webview("Community import window", webview, window);
        let shell = cx.new(|cx| CommunityImportWindowShell {
            _webview: webview,
            window_size: win_size,
            paste: WebViewPasteSupport::new(cx),
        });
        if let Ok(mut slot) = shell_slot_inner.lock() {
            *slot = Some(shell.clone());
        }
        window.focus(&shell.read(cx).paste.focus_handle.clone(), cx);
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;

    cx.global_mut::<crate::system_capsule::window_registry::SystemCapsuleWindowRegistry>()
        .register(SystemCapsuleId::AtoImport, *handle);
    cx.global_mut::<OpenContentWindows>().insert(
        handle.window_id().as_u64(),
        ContentWindowEntry {
            handle: *handle,
            kind: ContentWindowKind::Import,
            title: gpui::SharedString::from(format!("Review {}", label)),
            subtitle: gpui::SharedString::from(source.clone()),
            url: gpui::SharedString::from("capsule://desktop.ato.run/import/community"),
            capsule: None,
            last_focused_at: std::time::Instant::now(),
        },
    );
    system_ipc::spawn_drain_loop(cx, queue, *handle);

    let shell = shell_slot
        .lock()
        .unwrap()
        .take()
        .expect("CommunityImportWindowShell entity must be populated by open_window closure");
    cx.set_global(CommunityImportWindowSlot {
        shell: Some(shell.downgrade()),
    });

    spawn_candidate_fetch(cx, source, label);
    Ok(*handle)
}

/// Re-run candidate discovery for an already-open community-import window
/// (the Retry action after a transient fetch error). The page has already
/// reset itself to the loading state; this just kicks off a fresh fetch
/// whose result is pushed into the existing window.
pub fn refetch(cx: &mut App, source: String, label: String) {
    if !cx.has_global::<CommunityImportWindowSlot>() {
        tracing::warn!("community-import: refetch with no open window — ignoring");
        return;
    }
    spawn_candidate_fetch(cx, source, label);
}

/// Fetch a single recipe's raw `capsule.toml` by id and push it into the live
/// window's detail overlay. Used by the "View recipe" action so the user can
/// see exactly how two same-titled community recipes differ.
pub fn fetch_candidate_detail(cx: &mut App, ctoml_id: String) {
    if !cx.has_global::<CommunityImportWindowSlot>() {
        tracing::warn!("community-import: view detail with no open window — ignoring");
        return;
    }
    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();

    fe.spawn(async move {
        let id_for_fetch = ctoml_id.clone();
        let result = be
            .spawn(async move { community_api::fetch_candidate_toml(&id_for_fetch) })
            .await;

        let detail = match result {
            Ok(toml) => serde_json::json!({ "id": ctoml_id, "toml": toml, "error": null }),
            Err(err) => {
                tracing::warn!(error = %err, "community-import: recipe detail fetch failed");
                serde_json::json!({ "id": ctoml_id, "toml": null, "error": err.to_string() })
            }
        };
        let json = detail.to_string();

        let _ = aa.update(|cx| {
            let weak = cx
                .try_global::<CommunityImportWindowSlot>()
                .and_then(|s| s.shell.clone());
            if let Some(weak) = weak
                && let Some(shell) = weak.upgrade()
            {
                shell.read(cx).push_detail(&json);
            }
        });
    })
    .detach();
}

/// Fetch community candidates on the background executor, then push the
/// resulting snapshot into the live window on the foreground executor.
fn spawn_candidate_fetch(cx: &mut App, source: String, label: String) {
    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();

    fe.spawn(async move {
        let source_for_fetch = source.clone();
        let result = be
            .spawn(async move { community_api::fetch_candidates(&source_for_fetch) })
            .await;

        let snapshot = match result {
            Ok(candidates) if candidates.is_empty() => CommunitySnapshot {
                source,
                label,
                status: "empty".to_string(),
                candidates: Vec::new(),
                error: None,
            },
            Ok(candidates) => CommunitySnapshot {
                source,
                label,
                status: "ready".to_string(),
                candidates,
                error: None,
            },
            Err(err) => {
                tracing::warn!(error = %err, "community-import: candidate fetch failed");
                CommunitySnapshot {
                    source,
                    label,
                    status: "error".to_string(),
                    candidates: Vec::new(),
                    error: Some(err.to_string()),
                }
            }
        };
        let json = snapshot.to_json();

        let _ = aa.update(|cx| {
            let weak = cx
                .try_global::<CommunityImportWindowSlot>()
                .and_then(|s| s.shell.clone());
            if let Some(weak) = weak
                && let Some(shell) = weak.upgrade()
            {
                shell.read(cx).push_snapshot(&json);
            }
        });
    })
    .detach();
}
