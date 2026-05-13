//! Card Switcher — Wry-hosted HTML overlay. The visual layer lives in
//! `assets/launcher/switcher.html` (single-file: inline CSS + JS) and
//! receives the open-windows snapshot via a `window.__ATO_WINDOWS`
//! initialization script. User interaction (card click, dock click,
//! Escape, backdrop click, new-window tile) is signalled back over
//! `window.ipc.postMessage(...)` and routed through `web_bridge` to
//! the `&mut App` dispatcher below.
//!
//! Switched from GPUI rendering because the design reference
//! (.tmp/window-list.png) calls for richer card content (per-kind
//! mock previews, gradients, shadows) than GPUI's element library
//! can express ergonomically.

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, AnyWindowHandle, App, Bounds, Context, IntoElement, Render, WindowBounds,
    WindowDecorations, WindowKind, WindowOptions,
};
use serde::Serialize;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

use crate::window::content_windows::{ContentWindowKind, OpenContentWindows};
use crate::window::web_bridge::{self, BridgeAction};

/// Process-wide slot for the currently-open Card Switcher window so
/// the Control Bar's switcher button can behave as a toggle: a
/// second click closes the open switcher instead of stacking a new
/// overlay on top.
#[derive(Default)]
pub struct CardSwitcherWindowSlot(pub Option<AnyWindowHandle>);
impl gpui::Global for CardSwitcherWindowSlot {}

/// Lightweight GPUI entity whose only job is to keep the Wry WebView
/// alive for the lifetime of the switcher window. Wry mounts the
/// WKWebView as a child NSView of the window's content view, so the
/// GPUI `Render` body just provides a white backdrop in case the page
/// is still loading (browsers typically show transparent before the
/// document layouts).
pub struct CardSwitcherShell {
    _webview: WebView,
}

impl Render for CardSwitcherShell {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div().size_full().bg(rgb(0xf5f3ff))
    }
}

const SWITCHER_HTML: &str = include_str!("../../assets/launcher/switcher.html");

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
        ContentWindowKind::Launcher => "panel",
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

/// Capture a PNG screenshot of the OS window that backs `handle` and
/// return a data URL ready to embed in an `<img>`. macOS-only — uses
/// the OS-provided `screencapture -l <windowID>` tool which goes via
/// `CGWindowListCreateImage` and works on any window by ID without
/// raising it to the foreground (important: the switcher overlay
/// would obscure them otherwise). Requires the user to have granted
/// Screen Recording permission to the terminal running ato-desktop.
#[cfg(target_os = "macos")]
fn snapshot_window(cx: &mut App, handle: AnyWindowHandle) -> Option<String> {
    use base64::Engine;
    let nswindow = crate::window::macos::ns_window_for(cx, handle)?;
    let win_num = nswindow.windowNumber();
    if win_num <= 0 {
        return None;
    }
    let tmp = std::env::temp_dir().join(format!(
        "ato-snap-{}-{}.png",
        win_num,
        std::process::id()
    ));
    let out = std::process::Command::new("screencapture")
        .args([
            "-l",
            &win_num.to_string(),
            "-t",
            "png",
            "-x",
            tmp.to_str()?,
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        tracing::debug!(
            window_number = win_num,
            stderr = ?String::from_utf8_lossy(&out.stderr),
            "screencapture failed; falling back to CSS preview"
        );
        return None;
    }
    let bytes = std::fs::read(&tmp).ok()?;
    let _ = std::fs::remove_file(&tmp);
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Some(format!("data:image/png;base64,{}", encoded))
}

#[cfg(not(target_os = "macos"))]
fn snapshot_window(_cx: &mut App, _handle: AnyWindowHandle) -> Option<String> {
    None
}

fn kind_tag(kind: &ContentWindowKind) -> &'static str {
    match kind {
        ContentWindowKind::AppWindow { .. } => "AppWindow",
        ContentWindowKind::Store => "Store",
        ContentWindowKind::Start => "Start",
        ContentWindowKind::Launcher => "Launcher",
    }
}

/// Toggle the Card Switcher overlay. If one is already open
/// (tracked via the `CardSwitcherWindowSlot` global), this closes
/// it. Otherwise it snapshots `OpenContentWindows::mru_order()` into
/// a card payload, opens a fresh Wry-backed overlay, and starts the
/// IPC drain loop. The Control Bar's switcher button dispatches
/// through here so a second click dismisses the overlay instead of
/// stacking another on top.
pub fn open_card_switcher_window(cx: &mut App) -> Result<()> {
    let existing = cx.global::<CardSwitcherWindowSlot>().0;
    if let Some(handle) = existing {
        let close_result = handle.update(cx, |_, window, _| window.remove_window());
        cx.set_global(CardSwitcherWindowSlot(None));
        if close_result.is_ok() {
            return Ok(());
        }
    }

    // Snapshot each window BEFORE opening the switcher overlay so the
    // overlay does not obscure them. screencapture -l works on any
    // window by ID regardless of stacking, but doing it before keeps
    // the visual lineage obvious in the log.
    let entries: Vec<_> = cx
        .global::<OpenContentWindows>()
        .mru_order()
        .into_iter()
        .collect();
    let cards: Vec<CardDto> = entries
        .into_iter()
        .map(|e| {
            let window_id = e.handle.window_id().as_u64();
            let preview_data_url = snapshot_window(cx, e.handle);
            let glyph = glyph_for(e.title.as_ref(), &e.kind);
            CardDto {
                window_id,
                title: e.title.to_string(),
                subtitle: e.subtitle.to_string(),
                kind: kind_tag(&e.kind),
                preview_data_url,
                glyph,
            }
        })
        .collect();
    let cards_json = serde_json::to_string(&cards).unwrap_or_else(|_| "[]".to_string());
    let init_script = format!("window.__ATO_WINDOWS = {};", cards_json);

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

    let queue = web_bridge::new_queue();

    let handle = cx.open_window(options, |window, cx| {
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
        let webview = WebViewBuilder::new()
            .with_html(SWITCHER_HTML)
            .with_initialization_script(init_script.as_str())
            .with_ipc_handler(web_bridge::make_ipc_handler(queue_for_ipc))
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for the Card Switcher WebView");
        let shell = cx.new(|_cx| CardSwitcherShell { _webview: webview });
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;
    cx.set_global(CardSwitcherWindowSlot(Some(*handle)));

    web_bridge::spawn_drain_loop(cx, queue, *handle, dispatch);

    Ok(())
}

/// Translate one bridge action into the corresponding `&mut App`
/// operation. Runs on the GPUI main thread (the drain loop
/// trampolines onto it via `AsyncApp::update`), so it has full
/// access to globals and window APIs.
fn dispatch(cx: &mut App, host: AnyWindowHandle, action: BridgeAction) {
    match action {
        BridgeAction::CloseSwitcher => {
            cx.set_global(CardSwitcherWindowSlot(None));
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
        BridgeAction::ActivateWindow { window_id } => {
            // Look up the target handle in the cross-window registry.
            // Missing IDs (e.g. a window that closed between the
            // snapshot being injected and the click firing) are a
            // no-op — just close the switcher.
            let target = cx
                .global::<OpenContentWindows>()
                .get(window_id)
                .map(|e| e.handle);
            if let Some(target) = target {
                let _ = target.update(cx, |_, window, _| window.activate_window());
            }
            cx.set_global(CardSwitcherWindowSlot(None));
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
        BridgeAction::OpenStartWindow => {
            if let Err(err) = crate::window::start_window::open_start_window(cx) {
                tracing::error!(error = %err, "failed to open start window from switcher");
            }
            cx.set_global(CardSwitcherWindowSlot(None));
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
        // Switcher does not expose these actions; treat as no-ops if
        // they somehow arrive (extra safety, unparseable variants
        // are already dropped at the bridge boundary).
        BridgeAction::CloseStartWindow
        | BridgeAction::OpenAppWindow
        | BridgeAction::OpenStore => {
            tracing::debug!(?action, "ignored — not a switcher action");
        }
    }
}
