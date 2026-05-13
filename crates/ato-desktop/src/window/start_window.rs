//! StartWindow — Wry-hosted HTML "compose a new window" surface.
//! The visual layer lives in `assets/launcher/start.html`. Quick
//! actions and dock tiles inside the page post messages back over
//! `window.ipc` which `web_bridge` routes to host actions.
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

use crate::window::content_windows::{ContentWindowEntry, ContentWindowKind, OpenContentWindows};
use crate::window::web_bridge::{self, BridgeAction};

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

const START_HTML: &str = include_str!("../../assets/launcher/start.html");

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
            .with_html(START_HTML)
            .with_ipc_handler(web_bridge::make_ipc_handler(queue_for_ipc))
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

    web_bridge::spawn_drain_loop(cx, queue, *handle, dispatch);

    Ok(())
}

fn dispatch(cx: &mut App, host: gpui::AnyWindowHandle, action: BridgeAction) {
    match action {
        BridgeAction::CloseStartWindow => {
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
        BridgeAction::OpenAppWindow => {
            // Mirror the OpenAppWindowExperiment handler in app.rs:
            // spawn an AppWindow with the demo route. Closing the
            // StartWindow after dispatching avoids leaving a stale
            // composition surface behind.
            let route = crate::state::GuestRoute::CapsuleHandle {
                handle: "github.com/Koh0920/WasedaP2P".to_string(),
                label: "WasedaP2P".to_string(),
            };
            if let Err(err) = crate::window::open_app_window(cx, route) {
                tracing::error!(error = %err, "OpenAppWindow from StartWindow failed");
            }
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
        BridgeAction::OpenStore => {
            if let Err(err) = crate::window::store::open_store_window(cx) {
                tracing::error!(error = %err, "OpenStore from StartWindow failed");
            }
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
        // Switcher-only actions arriving here are a no-op.
        BridgeAction::CloseSwitcher
        | BridgeAction::ActivateWindow { .. }
        | BridgeAction::OpenStartWindow => {
            tracing::debug!(?action, "ignored — not a StartWindow action");
        }
    }
}
