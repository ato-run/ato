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

/// Stage A: translate the legacy `BridgeAction` into the typed
/// system-capsule vocabulary and route through `CapabilityBroker`.
/// The actual side effects (window remove, open_app_window,
/// open_store_window) live behind per-capsule modules in
/// `crate::system_capsule::{ato_windows, ato_store}`. Stage B will
/// switch the HTML envelope so this translation step disappears.
fn dispatch(cx: &mut App, host: gpui::AnyWindowHandle, action: BridgeAction) {
    use crate::system_capsule::ato_store::StoreCommand;
    use crate::system_capsule::ato_windows::WindowsCommand;
    use crate::system_capsule::{CapabilityBroker, SystemCapsuleId, SystemCommand};

    let (capsule, command) = match action {
        BridgeAction::CloseStartWindow => (
            SystemCapsuleId::AtoWindows,
            SystemCommand::AtoWindows(WindowsCommand::CloseStartWindow),
        ),
        BridgeAction::OpenAppWindow => (
            SystemCapsuleId::AtoWindows,
            SystemCommand::AtoWindows(WindowsCommand::OpenAppWindow),
        ),
        BridgeAction::OpenStore => (
            SystemCapsuleId::AtoStore,
            SystemCommand::AtoStore(StoreCommand::Open),
        ),
        // Switcher-only IPC arriving here is a no-op.
        BridgeAction::CloseSwitcher
        | BridgeAction::ActivateWindow { .. }
        | BridgeAction::OpenStartWindow => {
            tracing::debug!(?action, "start_window: ignored — not a StartWindow action");
            return;
        }
    };
    if let Err(err) = CapabilityBroker::dispatch(cx, host, capsule, command) {
        tracing::warn!(?err, "start_window: broker rejected dispatch");
    }
}
