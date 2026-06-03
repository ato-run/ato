//! Card Switcher — Wry-hosted HTML overlay. The visual layer lives in
//! `assets/launcher/switcher.html` (single-file: inline CSS + JS) and
//! receives open-windows + session snapshots via
//! `window.__ATO_WINDOWS` / `window.__ATO_SESSIONS`
//! initialization scripts. User interaction (card click, dock click,
//! Escape, backdrop click, new-window tile) is signalled back over
//! `window.ipc.postMessage(...)` and routed through `web_bridge` to
//! the `&mut App` dispatcher below.
//!
//! Switched from GPUI rendering because the design reference
//! (.tmp/window-list.png) calls for richer card content (per-kind
//! mock previews, gradients, shadows) than GPUI's element library
//! can express ergonomically.

use std::sync::mpsc;

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, App, Bounds, Context, IntoElement, Pixels, Render, Size, WindowBounds,
    WindowDecorations, WindowKind, WindowOptions, div, px, rgb, size,
};
use serde::Serialize;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

use crate::localization::{compose_init_script, resolve_locale};
use crate::state::session::SessionRegistry;
use crate::system_capsule::broker::SystemCapsuleId;
use crate::system_capsule::ipc as system_ipc;
use crate::window::content_windows::{ContentWindowKind, OpenContentWindows};
use crate::window::webview_paste::{WebViewPasteShell, WebViewPasteSupport};
use crate::{impl_focusable_via_paste, paste_render_wrap};

/// Process-wide slot for the currently-open Card Switcher window so
/// the Control Bar's switcher button can behave as a toggle: a
/// second click closes the open switcher instead of stacking a new
/// overlay on top.
#[derive(Default)]
pub struct CardSwitcherWindowSlot(pub Option<AnyWindowHandle>);
impl gpui::Global for CardSwitcherWindowSlot {}

/// Slot tracking the live `CardSwitcherShell` entity so background
/// screenshot tasks can push results into the WebView asynchronously.
#[derive(Default)]
pub struct CardSwitcherEntitySlot(pub Option<gpui::Entity<CardSwitcherShell>>);
impl gpui::Global for CardSwitcherEntitySlot {}

/// Lightweight GPUI entity whose only job is to keep the Wry WebView
/// alive for the lifetime of the switcher window. Wry mounts the
/// WKWebView as a child NSView of the window's content view, so the
/// GPUI `Render` body just provides a white backdrop in case the page
/// is still loading (browsers typically show transparent before the
/// document layouts).
pub struct CardSwitcherShell {
    _webview: Option<WebView>,
    window_size: Size<Pixels>,
    paste: WebViewPasteSupport,
}

impl_focusable_via_paste!(CardSwitcherShell, paste);

impl WebViewPasteShell for CardSwitcherShell {
    fn active_paste_target(&self) -> Option<&WebView> {
        self._webview.as_ref()
    }
}

impl Render for CardSwitcherShell {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_webview_bounds(window);
        paste_render_wrap!(
            div().size_full().bg(rgb(0xf5f3ff)),
            cx,
            &self.paste.focus_handle
        )
    }
}

impl CardSwitcherShell {
    /// Inject a screenshot data URL into the switcher page for the
    /// card identified by `window_id`. Called from the background
    /// screenshot dispatch loop when WKWebView snapshots arrive.
    fn push_screenshot(&self, window_id: u64, data_url: &str) {
        let escaped = data_url.replace('\\', "\\\\").replace('\'', "\\'");
        let script = format!(
            "window.__ATO_SWITCHER_SCREENSHOT__ && window.__ATO_SWITCHER_SCREENSHOT__({window_id}, '{escaped}');"
        );
        if let Some(webview) = self._webview.as_ref()
            && let Err(e) = webview.evaluate_script(&script)
        {
            tracing::debug!(window_id, ?e, "switcher: screenshot push failed");
        }
    }

    fn push_session_snapshot(&self, sessions_json: &str) {
        let script = format!(
            "window.__ATO_SESSIONS_REFRESH__ && window.__ATO_SESSIONS_REFRESH__({sessions_json});"
        );
        if let Some(webview) = self._webview.as_ref()
            && let Err(error) = webview.evaluate_script(&script)
        {
            tracing::debug!(?error, "switcher: session snapshot push failed");
        }
    }

    fn sync_webview_bounds(&mut self, window: &mut gpui::Window) {
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
}

const SWITCHER_HTML: &str = include_str!("../../assets/system/ato-windows/index.html");

/// Per-card payload injected into the WebView at open time. Matches
/// what `switcher.html` reads off `window.__ATO_WINDOWS`.
#[derive(Serialize)]
struct CardDto {
    #[serde(rename = "windowId")]
    window_id: u64,
    title: String,
    subtitle: String,
    /// One of `AppWindow | Store | Start | Launcher`. The HTML keys
    /// off this to pick a preview variant per card when no real
    /// snapshot is available.
    kind: &'static str,
    /// Optional `data:image/png;base64,...` URL containing a fresh
    /// screenshot of the target window. When present the switcher
    /// renders it as an `<img>` inside the card preview area — the
    /// Safari Tab Overview pattern. When `None` we fall back to the
    /// CSS-only kind-specific mock.
    #[serde(rename = "previewDataUrl", skip_serializing_if = "Option::is_none")]
    preview_data_url: Option<String>,
    /// Identifier for the glyph the switcher / dock should render
    /// inside the small badge for this entry. Values map 1:1 to
    /// keys in switcher.html's `GLYPH` library. Mirrors the
    /// per-kind icon vocabulary the legacy sidebar used to render
    /// (`SystemPageIcon::Console` → terminal, etc).
    glyph: &'static str,
}

/// Map a content window's (title, kind) to a glyph identifier
/// rendered as an SVG inside the switcher card badge and the dock
/// tile. Carries forward the visual identity each running app had
/// in the legacy sidebar:
///   - System surfaces (Store / Launcher / Start) get a fixed
///     thematic glyph
///   - AppWindow titles are matched by keyword heuristic against
///     a small fixed glyph palette (chart / terminal / search /
///     chat / cpu / code) — the same palette `start.html`'s dock
///     and "最近使ったカプセル" rows already use
fn glyph_for(title: &str, kind: &ContentWindowKind) -> &'static str {
    match kind {
        ContentWindowKind::Store => "store",
        ContentWindowKind::Start => "sparkle",
        ContentWindowKind::Settings => "panel",
        ContentWindowKind::Dock => "terminal",
        ContentWindowKind::Onboarding => "sparkle",
        ContentWindowKind::Launch => "sparkle",
        ContentWindowKind::Import => "code",
        ContentWindowKind::Auth => "panel",
        ContentWindowKind::AppWindow { .. } => {
            let lower = title.to_lowercase();
            if lower.contains("code") || lower.contains("term") || lower.contains("shell") {
                "terminal"
            } else if lower.contains("query") || lower.contains("search") {
                "search"
            } else if lower.contains("chat") || lower.contains("ai") {
                "chat"
            } else if lower.contains("ml") || lower.contains("model") {
                "cpu"
            } else {
                // Default for capsule-like AppWindows: bar chart —
                // matches WasedaP2P's data-sharing/visualisation role.
                "chart"
            }
        }
    }
}

/// Dispatch an asynchronous WKWebView screenshot request. The result
/// (a `data:image/png;base64,...` URL or `None`) is sent through `tx`
/// when the platform's snapshot API completes.
#[cfg(target_os = "macos")]
fn request_snapshot(cx: &mut App, handle: AnyWindowHandle, tx: mpsc::Sender<Option<String>>) {
    crate::window::macos::request_wkwebview_snapshot(cx, handle, tx);
}

#[cfg(target_os = "windows")]
fn request_snapshot(cx: &mut App, handle: AnyWindowHandle, tx: mpsc::Sender<Option<String>>) {
    crate::window::windows::request_win_window_snapshot(cx, handle, tx);
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn request_snapshot(_cx: &mut App, _handle: AnyWindowHandle, tx: mpsc::Sender<Option<String>>) {
    let _ = tx.send(None);
}

fn kind_tag(kind: &ContentWindowKind) -> &'static str {
    match kind {
        ContentWindowKind::AppWindow { .. } => "AppWindow",
        ContentWindowKind::Store => "Store",
        ContentWindowKind::Start => "Start",
        ContentWindowKind::Settings => "Settings",
        ContentWindowKind::Dock => "Dock",
        ContentWindowKind::Onboarding => "Onboarding",
        ContentWindowKind::Launch => "Launch",
        ContentWindowKind::Import => "Import",
        ContentWindowKind::Auth => "Auth",
    }
}

/// Toggle the Card Switcher overlay. If one is already open
/// (tracked via the `CardSwitcherWindowSlot` global), this closes
/// it. Otherwise it builds card payloads from
/// `OpenContentWindows::mru_order()`, opens a fresh Wry-backed overlay
/// with CSS-only mock previews, then dispatches asynchronous WKWebView
/// snapshots that push real screenshots into the page as they arrive.
/// The Control Bar's switcher button dispatches through here so a
/// second click dismisses the overlay instead of stacking another.
pub fn open_card_switcher_window(cx: &mut App) -> Result<()> {
    let existing = cx.global::<CardSwitcherWindowSlot>().0;
    if let Some(handle) = existing {
        tracing::info!(
            window_id = handle.window_id().as_u64(),
            "switcher: closing existing window (toggle)"
        );
        let close_result = handle.update(cx, |_, window, _| window.remove_window());
        cx.set_global(CardSwitcherWindowSlot(None));
        cx.set_global(CardSwitcherEntitySlot(None));
        if close_result.is_ok() {
            return Ok(());
        }
    }
    tracing::info!("switcher: opening new window");

    let entries: Vec<_> = cx
        .global::<OpenContentWindows>()
        .mru_order()
        .into_iter()
        .collect();

    // Open the card switcher immediately with CSS mock previews.
    // Real screenshots are captured asynchronously and pushed into
    // the WebView via `evaluate_script` as they arrive.
    let cards: Vec<CardDto> = entries
        .iter()
        .map(|e| {
            let window_id = e.handle.window_id().as_u64();
            let glyph = glyph_for(e.title.as_ref(), &e.kind);
            CardDto {
                window_id,
                title: e.title.to_string(),
                subtitle: e.subtitle.to_string(),
                kind: kind_tag(&e.kind),
                preview_data_url: None,
                glyph,
            }
        })
        .collect();
    let cards_json = serde_json::to_string(&cards).unwrap_or_else(|_| "[]".to_string());
    let windows_script = format!("window.__ATO_WINDOWS = {};", cards_json);
    let sessions_json =
        serde_json::to_string(&cx.global::<SessionRegistry>().background_view_entries())
            .unwrap_or_else(|_| "[]".to_string());
    let combined_script = format!("{windows_script}\nwindow.__ATO_SESSIONS = {sessions_json};");
    let locale = resolve_locale(crate::config::load_config().general.language);
    let init_script = compose_init_script(locale, Some(&combined_script));

    let bounds = Bounds::centered(None, size(px(1200.0), px(700.0)), cx);
    let options = WindowOptions {
        titlebar: None,
        focus: true,
        show: true,
        kind: WindowKind::PopUp,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };

    let queue = system_ipc::new_queue();
    let drain_queue = queue.clone();
    let entity_capture: std::rc::Rc<std::cell::RefCell<Option<gpui::Entity<CardSwitcherShell>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let entity_capture2 = entity_capture.clone();

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
        let queue_for_ipc = queue.clone();
        let _wv_guard = crate::webview_init_guard::WebviewInitGuard::new();
        let webview = WebViewBuilder::new()
            .with_html(SWITCHER_HTML)
            .with_initialization_script(init_script.as_str())
            .with_ipc_handler(system_ipc::make_ipc_handler_for_capsule(
                SystemCapsuleId::AtoWindows,
                queue_for_ipc,
            ))
            .with_bounds(webview_rect);
        let webview = crate::window::build_child_webview("Card Switcher window", webview, window);
        let shell = cx.new(|cx| CardSwitcherShell {
            _webview: webview,
            window_size: win_size,
            paste: WebViewPasteSupport::new(cx),
        });
        *entity_capture2.borrow_mut() = Some(shell.clone());
        window.focus(&shell.read(cx).paste.focus_handle.clone(), cx);
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;
    tracing::info!(
        window_id = handle.window_id().as_u64(),
        "switcher: window created, setting slots"
    );
    cx.set_global(CardSwitcherWindowSlot(Some(*handle)));
    cx.set_global(CardSwitcherEntitySlot(entity_capture.borrow_mut().take()));

    cx.global_mut::<crate::system_capsule::window_registry::SystemCapsuleWindowRegistry>()
        .register(SystemCapsuleId::AtoWindows, *handle);
    system_ipc::spawn_drain_loop(cx, drain_queue, *handle);

    // Dispatch asynchronous WKWebView snapshot requests for each card.
    // Results are progressively pushed into the WebView as they arrive.
    // Uses the foreground executor with non-blocking polling (background
    // timer + try_recv) so the main thread is never stalled.
    let window_id_to_handle: Vec<(u64, AnyWindowHandle)> = entries
        .iter()
        .map(|e| (e.handle.window_id().as_u64(), e.handle))
        .collect();
    let async_app = cx.to_async();
    let switcher_handle = *handle;
    async_app
        .foreground_executor()
        .spawn({
            let be = async_app.background_executor().clone();
            let aa = async_app.clone();
            async move {
                use std::time::{Duration, Instant};
                // Brief delay to let the ato-windows page finish loading before
                // we push screenshots via evaluate_script.
                be.timer(Duration::from_millis(300)).await;
                for (window_id, window_handle) in window_id_to_handle {
                    let (tx, rx) = mpsc::channel();
                    crate::webview_init_guard::wait_until_idle(&be).await;
                    aa.update(|cx| request_snapshot(cx, window_handle, tx));
                    let deadline = Instant::now() + Duration::from_millis(1500);
                    loop {
                        be.timer(Duration::from_millis(50)).await;
                        if crate::webview_init_guard::WebviewInitGuard::is_active() {
                            continue;
                        }
                        match rx.try_recv() {
                            Ok(Some(data_url)) => {
                                aa.update(|cx| {
                                    // Guard against a close-reopen race: only push
                                    // into the switcher instance we were spawned for.
                                    let still_open = cx
                                        .global::<CardSwitcherWindowSlot>()
                                        .0
                                        .map(|h| h == switcher_handle)
                                        .unwrap_or(false);
                                    if still_open
                                        && let Some(entity) = cx
                                            .try_global::<CardSwitcherEntitySlot>()
                                            .and_then(|slot| slot.0.clone())
                                    {
                                        entity.update(cx, |shell, _cx| {
                                            shell.push_screenshot(window_id, &data_url);
                                        });
                                    }
                                });
                                break;
                            }
                            Ok(None) => break,
                            Err(mpsc::TryRecvError::Empty) => {
                                if Instant::now() >= deadline {
                                    break;
                                }
                            }
                            Err(mpsc::TryRecvError::Disconnected) => break,
                        }
                    }
                }
            }
        })
        .detach();

    // Asynchronously refresh OCI sessions from the CLI so the session rows
    // reflect the actual container state.  We do this *after* the window opens
    // to avoid blocking the UI: the Card Switcher renders immediately with
    // whatever the registry already knows, then updates once `ato ps` returns.
    {
        let async_app2 = cx.to_async();
        let switcher_handle2 = *handle;
        async_app
            .foreground_executor()
            .spawn({
                let be = async_app2.background_executor().clone();
                let aa = async_app2.clone();
                async move {
                    use std::time::Duration;
                    be.timer(Duration::from_millis(200)).await;
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let _ = tx.send(crate::orchestrator::list_oci_sessions());
                    });
                    loop {
                        be.timer(Duration::from_millis(100)).await;
                        if crate::webview_init_guard::WebviewInitGuard::is_active() {
                            continue;
                        }
                        match rx.try_recv() {
                            Ok(Ok(snapshots)) => {
                                aa.update(|cx| {
                                    cx.global_mut::<SessionRegistry>()
                                        .sync_oci_sessions(snapshots);
                                    let still_open = cx
                                        .global::<CardSwitcherWindowSlot>()
                                        .0
                                        .map(|h| h == switcher_handle2)
                                        .unwrap_or(false);
                                    if still_open {
                                        refresh_session_snapshot(cx);
                                    }
                                });
                                break;
                            }
                            Ok(Err(error)) => {
                                tracing::warn!(
                                    ?error,
                                    "switcher: async OCI session refresh failed"
                                );
                                break;
                            }
                            Err(std::sync::mpsc::TryRecvError::Empty) => continue,
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                        }
                    }
                }
            })
            .detach();
    }

    tracing::info!("switcher: open complete");
    Ok(())
}

pub fn refresh_session_snapshot(cx: &mut App) {
    let sessions_json =
        serde_json::to_string(&cx.global::<SessionRegistry>().background_view_entries())
            .unwrap_or_else(|_| "[]".to_string());
    if let Some(entity) = cx
        .try_global::<CardSwitcherEntitySlot>()
        .and_then(|slot| slot.0.clone())
    {
        entity.update(cx, |shell, _cx| shell.push_session_snapshot(&sessions_json));
    }
}

// Stage B note: the per-window `dispatch` translator from Stage A is
// gone. The HTML now posts `{capsule: "ato-windows", command: {...}}`
// envelopes directly, the system_capsule::ipc handler parses them
// into typed pairs, and the drain loop invokes
// `CapabilityBroker::dispatch` → `ato_windows::dispatch`. No
// per-window glue needed.
