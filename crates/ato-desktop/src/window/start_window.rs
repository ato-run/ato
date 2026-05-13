//! StartWindow — Wry-hosted HTML "compose a new window" surface.
//! Stage B: visual layer moved to `assets/system/ato-windows/start.html`
//! and the IPC envelope is now the typed system-capsule shape
//! (`{capsule, command}`). The page is part of the `ato-windows`
//! system capsule even though it owns its own NSWindow — the
//! distinction is "switcher view vs picker view" inside the same
//! capsule.
//!
//! Distinct from Launcher: each invocation spawns a fresh window
//! (no slot, no focus-reuse).

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, App, Bounds, Context, IntoElement, Render, WindowBounds,
    WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

use crate::system_capsule::ipc as system_ipc;
use crate::window::content_windows::{ContentWindowEntry, ContentWindowKind, OpenContentWindows};

pub struct StartWindowShell {
    _webview: WebView,
}

impl Render for StartWindowShell {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Pale-violet backdrop in case the HTML is still painting.
        div().size_full().bg(rgb(0xf5f3ff))
    }
}

const START_HTML: &str = include_str!("../../assets/system/ato-windows/start.html");

/// Spawn a fresh StartWindow. Always opens a new window — there is no
/// slot or focus-reuse pathway here. Callers invoke this directly
/// (e.g. the switcher's new-window tile) so opening the window does
/// not depend on a dispatch queue surviving any close-soon-after on
/// the caller side.
pub fn open_start_window(cx: &mut App) -> Result<()> {
    let bounds = Bounds::centered(None, size(px(1200.0), px(880.0)), cx);
    let options = WindowOptions {
        titlebar: Some(TitleBar::title_bar_options()),
        focus: true,
        show: true,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };

    let queue = system_ipc::new_queue();
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
            .with_html(START_HTML)
            .with_ipc_handler(system_ipc::make_ipc_handler(queue_for_ipc))
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for the Start WebView");
        let shell = cx.new(|_cx| StartWindowShell { _webview: webview });
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;

    // Register in the cross-window content registry so the Control
    // Bar badge increments AND the Card Switcher renders a card.
    cx.global_mut::<OpenContentWindows>().insert(
        handle.window_id().as_u64(),
        ContentWindowEntry {
            handle: *handle,
            kind: ContentWindowKind::Start,
            title: gpui::SharedString::from("新しいウィンドウ"),
            subtitle: gpui::SharedString::from("カプセル / URL / コマンドから始める"),
            url: gpui::SharedString::from("ato://start"),
            last_focused_at: std::time::Instant::now(),
        },
    );

    system_ipc::spawn_drain_loop(cx, queue, *handle);

    Ok(())
}

// Stage B note: the per-window `dispatch` translator from Stage A is
// gone. start.html now posts `{capsule: "ato-windows", command: ...}`
// or `{capsule: "ato-store", command: ...}` envelopes directly. The
// system_capsule::ipc handler resolves the capsule, parses the typed
// command, and the drain loop invokes `CapabilityBroker::dispatch`.
