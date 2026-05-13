//! Store window — mounts a Wry WebView pointing at `https://ato.run/`
//! as a standalone top-level GPUI window. The Control Bar's "ストア"
//! button dispatches `OpenStoreWindow` which lands here.
//!
//! Bypasses the heavy `webview::WebViewManager` plumbing because the
//! Store does not need partition isolation, capability gating,
//! preload scripts, or the JSON-RPC host bridge — it is just a
//! plain external page in its own window.

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, AnyWindowHandle, App, Bounds, Context, IntoElement, Render, WindowBounds,
    WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

/// Slot tracking the single open Store window so the Control Bar's
/// Store button focuses an existing window on a 2nd+ click instead
/// of spawning a duplicate.
#[derive(Default)]
pub struct StoreWindowSlot(pub Option<AnyWindowHandle>);
impl gpui::Global for StoreWindowSlot {}

/// Lightweight GPUI entity whose only job is to keep the Wry
/// `WebView` alive for the lifetime of its window. Wry mounts the
/// `WKWebView` as a child NSView of the window's content view, so
/// the GPUI `Render` body just provides a white backdrop in case
/// the page is still loading.
pub struct StoreWebView {
    _webview: WebView,
}

impl Render for StoreWebView {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div().size_full().bg(rgb(0xffffff))
    }
}

const STORE_URL: &str = "https://ato.run/";

/// Open the Store window (Wry WebView on `https://ato.run/`). On a
/// 2nd+ click the existing window gets focused / brought to front
/// rather than a duplicate spawned. Returns the GPUI `WindowHandle`
/// so the Focus-mode boot path can use the Store as its initial
/// window and hand the handle to `focus_dispatcher::start`.
pub fn open_store_window(cx: &mut App) -> Result<AnyWindowHandle> {
    // Focus-on-existing — mirrors `open_launcher_window`.
    let existing = cx.global::<StoreWindowSlot>().0;
    if let Some(handle) = existing {
        let result = handle.update(cx, |_, window, _| window.activate_window());
        match result {
            Ok(()) => return Ok(handle),
            Err(_) => {
                cx.set_global(StoreWindowSlot(None));
            }
        }
    }

    let bounds = Bounds::centered(None, size(px(1100.0), px(760.0)), cx);
    let options = WindowOptions {
        titlebar: Some(TitleBar::title_bar_options()),
        focus: true,
        show: true,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };
    let handle = cx.open_window(options, |window, cx| {
        // Size the WebView to fill the window's content area.
        // Without an explicit `with_bounds`, Wry's `build_as_child`
        // hands the WKWebView some platform-default rectangle that
        // ends up much smaller than the window (~480×320 worth),
        // leaving most of the window blank.
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
            .with_url(STORE_URL)
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for the Store WebView");
        let store = cx.new(|_cx| StoreWebView { _webview: webview });
        cx.new(|cx| gpui_component::Root::new(store, window, cx))
    })?;
    cx.set_global(StoreWindowSlot(Some(*handle)));
    // Register in the cross-window content registry so the Control
    // Bar badge increments AND the Card Switcher renders a card.
    use crate::window::content_windows::{
        ContentWindowEntry, ContentWindowKind, OpenContentWindows,
    };
    cx.global_mut::<OpenContentWindows>().insert(
        handle.window_id().as_u64(),
        ContentWindowEntry {
            handle: *handle,
            kind: ContentWindowKind::Store,
            title: gpui::SharedString::from("ストア"),
            subtitle: gpui::SharedString::from("ato.run"),
            url: gpui::SharedString::from(STORE_URL),
            last_focused_at: std::time::Instant::now(),
        },
    );
    Ok(*handle)
}
