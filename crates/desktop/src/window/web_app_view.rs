//! WebAppView — the dedicated single-WebView surface for web-served
//! apps (cloud/remote session URLs, `ato://open` targets, any
//! ExternalUrl AppWindow).
//!
//! NOT the web-viewer: no tab strip, no URL bar, no back/forward/reload
//! in any build. One Wry WebView fills the window; the window IS the
//! app. Non-web navigations (`capsule://…`, `ato://…`) are cancelled and
//! re-dispatched as [`NavigateToUrl`] so launches always open their own
//! independent window (and Shell Icon Bar tab) instead of navigating
//! this one away.
//!
//! The Ato Home shell reuses this view; its window-level concerns
//! (singleton slot, `ContentWindowKind::Home` registration) stay in
//! [`super::ato_home_shell`].

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{Context, IntoElement, Pixels, Render, Size, div, rgb};
use url::Url;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

use crate::app::NavigateToUrl;
use crate::window::webview_paste::{WebViewPasteShell, WebViewPasteSupport};
use crate::{impl_focusable_via_paste, paste_render_wrap};

type InterceptedNavQueue = Arc<Mutex<Vec<String>>>;

pub struct WebAppView {
    webview: Option<WebView>,
    window_size: Size<Pixels>,
    pub paste: WebViewPasteSupport,
}

impl_focusable_via_paste!(WebAppView, paste);

impl WebViewPasteShell for WebAppView {
    fn active_paste_target(&self) -> Option<&WebView> {
        self.webview.as_ref()
    }
}

impl Render for WebAppView {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_webview_bounds(window);
        paste_render_wrap!(
            div().size_full().bg(rgb(0xffffff)),
            cx,
            &self.paste.focus_handle
        )
    }
}

impl WebAppView {
    /// Build the dedicated app WebView filling `window`, pointed at `url`.
    /// `context_label` names the surface in crash reports.
    pub fn new(
        context_label: &str,
        url: Url,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let win_size = window.bounds().size;
        let webview_rect = Rect {
            position: LogicalPosition::new(0i32, 0i32).into(),
            size: LogicalSize::new(
                f32::from(win_size.width) as u32,
                f32::from(win_size.height) as u32,
            )
            .into(),
        };
        let nav_queue: InterceptedNavQueue = Arc::new(Mutex::new(Vec::new()));
        let queue_for_nav = nav_queue.clone();
        let _wv_guard = crate::webview_init_guard::WebviewInitGuard::new();
        let builder = WebViewBuilder::new()
            .with_url(url.as_str())
            .with_bounds(webview_rect)
            // Desktop marker, layer 1: UA suffix so ato-served pages can
            // render Desktop-specific UX without JS detection.
            .with_user_agent(format!(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
                 (KHTML, like Gecko) Version/17.0 Safari/605.1.15 AtoDesktop/{}",
                env!("CARGO_PKG_VERSION")
            ))
            // Desktop marker, layer 2: JS global injected before page
            // scripts so client code can feature-gate on it.
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
        let webview = crate::window::build_child_webview(context_label, builder, window);
        spawn_intercepted_nav_drain(cx, nav_queue, window.window_handle());
        Self {
            webview,
            window_size: win_size,
            paste: WebViewPasteSupport::new(cx),
        }
    }

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

/// Schemes the app WebView navigates itself; everything else is an
/// app-launch intent that must leave the window.
pub fn is_web_navigation(target: &str) -> bool {
    let lower = target.trim_start().to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("about:")
        || lower.starts_with("data:")
        || lower.starts_with("blob:")
}

/// Forward queued non-web navigations (capsule:// / ato:// …) to the
/// app-level `NavigateToUrl` routing. Dispatches through this view's
/// own window; the loop ends when the window closes.
fn spawn_intercepted_nav_drain(
    cx: &mut Context<WebAppView>,
    queue: InterceptedNavQueue,
    host: gpui::AnyWindowHandle,
) {
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
            let mut live = true;
            aa.update(|cx: &mut gpui::App| {
                for url in drained {
                    tracing::info!(url = %url, "web app view: launch intent intercepted");
                    let result = host.update(cx, |_, window, cx| {
                        window.dispatch_action(Box::new(NavigateToUrl { url: url.clone() }), cx);
                    });
                    if result.is_err() {
                        live = false;
                        return;
                    }
                }
            });
            if !live {
                break;
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_schemes_stay_in_view_and_intents_leave() {
        assert!(is_web_navigation("https://abc.app.ato.run/"));
        assert!(is_web_navigation("about:blank"));
        assert!(!is_web_navigation("capsule://community/hello-capsule"));
        assert!(!is_web_navigation("ato://open?handle=x"));
    }
}
