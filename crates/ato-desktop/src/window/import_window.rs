//! `ato-import` system-capsule host window.
//!
//! Hosts the GitHub Import review surface: a single-file HTML page
//! at `assets/system/ato-import/index.html` that renders the current
//! `GitHubImportSession` snapshot and posts IPC commands
//! (`open`, `edit_recipe`, `run`, `submit_intent`) back to
//! `crate::system_capsule::ato_import::dispatch`.
//!
//! The session itself lives in a process-wide `gpui::Global` so every
//! entry point (control bar URL bar, ato-dock modal, ato-start search)
//! mutates the same session.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, rgb, size, AnyWindowHandle, App, Bounds, Context, Entity, IntoElement, Pixels, Render,
    Size, WeakEntity, Window, WindowBounds, WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

use crate::localization::{compose_init_script, resolve_locale};
use crate::source_import_api::ApiCreds;
use crate::source_import_session::{GitHubImportSession, SessionSnapshot};
use crate::system_capsule::ipc as system_ipc;
use crate::window::webview_paste::{WebViewPasteShell, WebViewPasteSupport};
use crate::{impl_focusable_via_paste, paste_render_wrap};
use gpui::px;

const IMPORT_HTML: &str = include_str!("../../assets/system/ato-import/index.html");
const IMPORT_W: f32 = 720.0;
const IMPORT_H: f32 = 640.0;

/// Process-wide GitHub import session. Initialized on first
/// `open_import_window` call; cloned Arc handed to background threads.
#[derive(Clone)]
pub struct ImportSessionState(pub Arc<Mutex<GitHubImportSession>>);
impl gpui::Global for ImportSessionState {}

/// Slot for the currently-open import window. Reused across entry
/// points so opening from the URL bar while one is already open
/// focuses the existing window instead of stacking.
#[derive(Default, Clone)]
pub struct ImportWindowSlot {
    pub window: Option<AnyWindowHandle>,
    pub shell: Option<WeakEntity<ImportWindowShell>>,
}
impl gpui::Global for ImportWindowSlot {}

/// Cached ato-api credentials for the current import session.
/// Cleared on session reset; refreshed by the dispatch layer at the
/// start of each `begin_open`. Kept out of `SessionSnapshot` so the
/// session token never crosses into JS / the snapshot serialization.
#[derive(Default, Clone)]
pub struct ImportApiCreds(pub Option<ApiCreds>);
impl gpui::Global for ImportApiCreds {}

pub struct ImportWindowShell {
    _webview: WebView,
    window_size: Size<Pixels>,
    paste: WebViewPasteSupport,
}

impl_focusable_via_paste!(ImportWindowShell, paste);

impl WebViewPasteShell for ImportWindowShell {
    fn active_paste_target(&self) -> Option<&WebView> {
        Some(&self._webview)
    }
}

impl Render for ImportWindowShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_webview_bounds(window);
        paste_render_wrap!(
            div().size_full().bg(rgb(0xffffff)),
            cx,
            &self.paste.focus_handle
        )
    }
}

impl ImportWindowShell {
    fn sync_webview_bounds(&mut self, window: &mut Window) {
        let current = window.bounds().size;
        if current == self.window_size {
            return;
        }
        let _ = self._webview.set_bounds(Rect {
            position: LogicalPosition::new(0i32, 0i32).into(),
            size: LogicalSize::new(
                f32::from(current.width) as u32,
                f32::from(current.height) as u32,
            )
            .into(),
        });
        self.window_size = current;
    }

    /// Push a session snapshot to the JS side. JSON must be a valid
    /// JSON object literal. The JS side guards with `typeof` so an
    /// early call before the DOM is ready is silently ignored.
    pub fn push_snapshot(&self, snapshot_json: &str) {
        let script = format!(
            "typeof window.__atoImportSnapshot==='function'&&window.__atoImportSnapshot({})",
            snapshot_json
        );
        if let Err(error) = self._webview.evaluate_script(&script) {
            tracing::warn!(?error, "ato-import: evaluate_script(push_snapshot) failed");
        }
    }
}

/// Get or initialize the session global.
pub fn session_arc(cx: &mut App) -> Arc<Mutex<GitHubImportSession>> {
    if !cx.has_global::<ImportSessionState>() {
        cx.set_global(ImportSessionState(Arc::new(Mutex::new(
            GitHubImportSession::default(),
        ))));
    }
    cx.global::<ImportSessionState>().0.clone()
}

/// Serialize the current snapshot to a JSON string for `evaluate_script`.
pub fn snapshot_json(snapshot: &SessionSnapshot) -> String {
    serde_json::to_string(snapshot).unwrap_or_else(|_| "null".to_string())
}

/// Push the current session snapshot into the active import window's
/// WebView. Silently no-op if the window is not currently open.
pub fn push_current_snapshot(cx: &mut App) {
    let snapshot = {
        let session = session_arc(cx);
        let guard = match session.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        guard.snapshot()
    };
    let json = snapshot_json(&snapshot);
    let weak = cx
        .try_global::<ImportWindowSlot>()
        .and_then(|s| s.shell.clone());
    if let Some(weak) = weak {
        if let Some(shell) = weak.upgrade() {
            shell.read(cx).push_snapshot(&json);
        }
    }
}

/// Open the ato-import window. If one is already open, activates it
/// and returns the existing handle. The caller is expected to issue an
/// `open` IPC command afterwards if it wants to start a new session.
pub fn open_import_window(cx: &mut App) -> Result<AnyWindowHandle> {
    // Initialize the session global early so the first snapshot has
    // somewhere to come from.
    let session = session_arc(cx);

    if let Some(slot) = cx.try_global::<ImportWindowSlot>() {
        if let Some(handle) = slot.window {
            // Try to bring the existing window forward. If activation
            // fails (handle is stale), fall through and reopen.
            let activate_ok = handle.update(cx, |_, window, _| window.activate_window()).is_ok();
            if activate_ok {
                return Ok(handle);
            }
        }
    }

    let bounds = Bounds::centered(None, size(px(IMPORT_W), px(IMPORT_H)), cx);
    let options = WindowOptions {
        titlebar: Some(TitleBar::title_bar_options()),
        focus: true,
        show: true,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };

    let locale = resolve_locale(crate::config::load_config().general.language);
    let initial_snapshot = {
        let guard = session.lock().expect("session mutex poisoned");
        snapshot_json(&guard.snapshot())
    };
    let init_payload = format!("window.__ATO_IMPORT_SNAPSHOT={};", initial_snapshot);
    let composed = compose_init_script(locale, Some(&init_payload));

    let queue = system_ipc::new_queue();
    let shell_slot: Arc<Mutex<Option<Entity<ImportWindowShell>>>> = Arc::new(Mutex::new(None));
    let shell_slot_inner = Arc::clone(&shell_slot);
    let queue_for_closure = queue.clone();

    let handle = cx.open_window(options, move |window, cx| {
        let win_size = window.bounds().size;
        let webview_rect = Rect {
            position: LogicalPosition::new(0i32, 0i32).into(),
            size: LogicalSize::new(
                f32::from(win_size.width) as u32,
                f32::from(win_size.height) as u32,
            )
            .into(),
        };
        let webview = WebViewBuilder::new()
            .with_html(IMPORT_HTML)
            .with_initialization_script(&composed)
            .with_ipc_handler(system_ipc::make_ipc_handler(queue_for_closure))
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for the Import window");
        let shell = cx.new(|cx| ImportWindowShell {
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

    system_ipc::spawn_drain_loop(cx, queue, *handle);

    let shell = shell_slot
        .lock()
        .unwrap()
        .take()
        .expect("ImportWindowShell entity must be populated by open_window closure");
    cx.set_global(ImportWindowSlot {
        window: Some(*handle),
        shell: Some(shell.downgrade()),
    });
    Ok(*handle)
}

/// Convenience entry point: open the import window (or focus the
/// existing one) and immediately begin a new import session for `url`.
/// Used by the control bar URL bar, ato-dock GitHub URL field, and
/// ato-start search bar.
pub fn open_with_url(cx: &mut App, url: String) -> Result<AnyWindowHandle> {
    let handle = open_import_window(cx)?;
    crate::system_capsule::ato_import::begin_open(cx, url);
    Ok(handle)
}
