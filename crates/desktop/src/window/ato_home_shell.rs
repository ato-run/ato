//! AtoHomeShell — the dedicated window hosting the `ato-pwa` Home.
//!
//! NOT the web-viewer: unlike [`super::web_link_view::WebLinkViewShell`]
//! this window has no tab strip, no URL bar and no browser chrome in any
//! build — a single Wry WebView pointed at the configured PWA origin
//! fills the whole window. The PWA is the Ato control surface (login,
//! Discover, Run, runner settings), so its window is a first-class
//! surface, not a wrapped browser tab.
//!
//! App launches initiated inside the PWA do NOT embed in this window:
//! any non-web navigation (`capsule://…`, `ato://…`) is cancelled and
//! re-dispatched as a [`NavigateToUrl`] action, which routes through the
//! normal launch flow and opens an independent capsule AppWindow — which
//! in turn appears as its own icon in the Shell Icon Bar.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, App, Bounds, Context, IntoElement, Pixels, Render, Size, WindowBounds,
    WindowDecorations, WindowOptions, div, px, rgb, size,
};
use gpui_component::TitleBar;
use url::Url;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

use crate::app::NavigateToUrl;
use crate::window::webview_paste::{WebViewPasteShell, WebViewPasteSupport};
use crate::{impl_focusable_via_paste, paste_render_wrap};

/// Tracks the singleton Ato Home window so repeat opens focus the
/// existing window instead of stacking duplicates.
#[derive(Default)]
pub struct AtoHomeWindowSlot(pub Option<AnyWindowHandle>);
impl gpui::Global for AtoHomeWindowSlot {}

type InterceptedNavQueue = Arc<Mutex<Vec<String>>>;

pub struct AtoHomeWebView {
    webview: Option<WebView>,
    window_size: Size<Pixels>,
    paste: WebViewPasteSupport,
}

impl_focusable_via_paste!(AtoHomeWebView, paste);

impl WebViewPasteShell for AtoHomeWebView {
    fn active_paste_target(&self) -> Option<&WebView> {
        self.webview.as_ref()
    }
}

impl Render for AtoHomeWebView {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_webview_bounds(window);
        paste_render_wrap!(
            div().size_full().bg(rgb(0xffffff)),
            cx,
            &self.paste.focus_handle
        )
    }
}

impl AtoHomeWebView {
    fn sync_webview_bounds(&mut self, window: &mut gpui::Window) {
        let current = window.bounds().size;
        if current == self.window_size {
            return;
        }
        if let Some(webview) = self.webview.as_ref() {
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

/// Schemes the Home WebView navigates itself; everything else is an
/// app-launch intent that must leave this window.
fn is_web_navigation(target: &str) -> bool {
    let lower = target.trim_start().to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("about:")
        || lower.starts_with("data:")
        || lower.starts_with("blob:")
}

/// Open (or focus) the dedicated Ato Home window on `url`.
pub fn open_ato_home_window(cx: &mut App, url: Url) -> Result<AnyWindowHandle> {
    let existing = cx.global::<AtoHomeWindowSlot>().0;
    if let Some(handle) = existing {
        match handle.update(cx, |_, window, _| window.activate_window()) {
            Ok(()) => return Ok(handle),
            Err(_) => cx.set_global(AtoHomeWindowSlot(None)),
        }
    }

    let win_size = size(px(1100.0), px(760.0));
    let bounds = match cx.primary_display() {
        Some(d) => {
            let db = d.bounds();
            let left = db.origin.x + (db.size.width - win_size.width) / 2.0;
            let top = db.origin.y + px(108.0);
            Bounds {
                origin: gpui::point(left, top),
                size: win_size,
            }
        }
        None => Bounds::centered(None, win_size, cx),
    };
    let options = WindowOptions {
        titlebar: Some(TitleBar::title_bar_options()),
        focus: true,
        show: true,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };

    let nav_queue: InterceptedNavQueue = Arc::new(Mutex::new(Vec::new()));
    let queue_for_nav = nav_queue.clone();
    let url_str = url.to_string();

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
        let builder = WebViewBuilder::new()
            .with_url(&url_str)
            .with_bounds(webview_rect)
            // Desktop marker, layer 1: UA suffix so the PWA server can
            // render Desktop-specific UX without JS detection.
            .with_user_agent(format!(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
                 (KHTML, like Gecko) Version/17.0 Safari/605.1.15 AtoDesktop/{}",
                env!("CARGO_PKG_VERSION")
            ))
            // Desktop marker, layer 2: JS global injected before page
            // scripts so the PWA client can feature-gate on it.
            .with_initialization_script(&format!(
                "window.__ATO_DESKTOP__ = {{ version: \"{}\", platform: \"{}\" }};",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
            ))
            // App-launch intents leave this window: cancel the in-page
            // navigation and queue it for the NavigateToUrl dispatch loop.
            .with_navigation_handler(move |target: String| -> bool {
                if is_web_navigation(&target) {
                    true
                } else {
                    if let Ok(mut q) = queue_for_nav.lock() {
                        q.push(target);
                    }
                    false
                }
            });
        let webview = crate::window::build_child_webview("Ato Home window", builder, window);
        let shell = cx.new(|cx| AtoHomeWebView {
            webview,
            window_size: win_size,
            paste: WebViewPasteSupport::new(cx),
        });
        window.focus(&shell.read(cx).paste.focus_handle.clone(), cx);
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;

    cx.set_global(AtoHomeWindowSlot(Some(*handle)));
    use crate::window::content_windows::{
        ContentWindowEntry, ContentWindowKind, OpenContentWindows,
    };
    cx.global_mut::<OpenContentWindows>().insert(
        handle.window_id().as_u64(),
        ContentWindowEntry {
            handle: *handle,
            kind: ContentWindowKind::Home,
            title: gpui::SharedString::from("Ato"),
            subtitle: gpui::SharedString::from("Home"),
            url: gpui::SharedString::from("capsule://desktop.ato.run/home"),
            capsule: None,
            last_focused_at: std::time::Instant::now(),
        },
    );
    spawn_intercepted_nav_drain(cx, nav_queue, *handle);
    Ok(*handle)
}

/// Forward queued non-web navigations (capsule:// / ato:// …) to the
/// app-level `NavigateToUrl` routing, which opens independent capsule
/// AppWindows — never embeds inside the Home WebView.
fn spawn_intercepted_nav_drain(cx: &mut App, queue: InterceptedNavQueue, home: AnyWindowHandle) {
    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();
    fe.spawn(async move {
        loop {
            be.timer(Duration::from_millis(50)).await;
            if crate::webview_init_guard::WebviewInitGuard::is_active() {
                continue;
            }
            let drained: Vec<String> = match queue.lock() {
                Ok(mut q) => std::mem::take(&mut *q),
                Err(_) => continue,
            };
            if drained.is_empty() {
                continue;
            }
            let dispatched = aa.update(|cx: &mut gpui::App| {
                for url in drained {
                    tracing::info!(url = %url, "Ato Home: app-launch intent intercepted");
                    let result = home.update(cx, |_, window, cx| {
                        window.dispatch_action(Box::new(NavigateToUrl { url: url.clone() }), cx);
                    });
                    if result.is_err() {
                        tracing::warn!("Ato Home nav drain: home window gone — stopping loop");
                        return false;
                    }
                }
                true
            });
            if !dispatched {
                break;
            }
        }
    })
    .detach();
}
