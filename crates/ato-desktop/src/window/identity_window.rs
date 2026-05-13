//! `ato-identity` system-capsule host window.
//!
//! Small popover invoked when the user clicks the Control Bar avatar
//! button. Renders `assets/system/ato-identity/index.html`. Phase 1
//! menu items either close the popover, hand off to the existing
//! ato-store / ato-settings windows, or are visibly disabled with
//! "近日公開" pills (Phase 2 placeholders).
//!
//! Sized to the panel content — window IS the card, matching the
//! launch wizards' fit.

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

const IDENTITY_HTML: &str = include_str!("../../assets/system/ato-identity/index.html");

const IDENTITY_W: f32 = 280.0;
const IDENTITY_H: f32 = 440.0;

pub struct IdentityWindowShell {
    _webview: WebView,
}

impl Render for IdentityWindowShell {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div().size_full().bg(rgb(0xffffff))
    }
}

pub fn open_identity_window(cx: &mut App) -> Result<()> {
    let bounds = Bounds::centered(None, size(px(IDENTITY_W), px(IDENTITY_H)), cx);
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
            .with_html(IDENTITY_HTML)
            .with_ipc_handler(system_ipc::make_ipc_handler(queue_for_ipc))
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for the Identity WebView");
        let shell = cx.new(|_cx| IdentityWindowShell { _webview: webview });
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;

    system_ipc::spawn_drain_loop(cx, queue, *handle);
    Ok(())
}
