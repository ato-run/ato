//! WebLinkView — Desktop's built-in browser view for external Web
//! links. NOT a capsule. The architectural framing (see the user's
//! design note attached to this commit):
//!
//!   - `capsule://...` URIs run inside the capsule runtime
//!   - `https://...` URIs open here, in a Desktop-owned Wry WebView
//!     wrapped by a slim GPUI chrome (back / forward / reload / URL).
//!   - Browser-as-capsule is left as a future extension. Today the
//!     Desktop holds the trust surface for external-fetch navigation,
//!     downloads, cert UI, and external-app handoffs.
//!
//! Visual reference: `crates/ato-desktop/browser-view.png`. This
//! iteration delivers the single-tab base: chrome strip + WebView.
//! A real tab strip can layer in once we have multi-tab routing on
//! top of the per-AppWindow registry.
//!
//! The chrome buttons (back / forward / reload) are decorative for
//! now — Wry exposes `evaluate_script("history.back()")` etc. but
//! wiring them through gpui's click handler into the WebView held
//! inside `&mut self` needs an entity-method on the shell. Slated
//! for the next iteration when the WebView is bound to navigation
//! state we can hand off across renders.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    div, hsla, px, rgb, svg, AnyElement, Context, IntoElement, MouseButton, Render,
    SharedString,
};
use gpui_component::{Icon, IconName};
use url::Url;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

/// Height of the GPUI chrome strip above the WebView. Matches the
/// reference design's tab+URL row density.
const CHROME_HEIGHT: f32 = 48.0;

pub struct WebLinkViewShell {
    // Wry WebView held by Rc<RefCell<>> so the chrome's nav-button
    // click handlers can borrow it without contending with the
    // shell's own &mut self in render. The cell is cheaply cloned
    // into each closure; only one borrow is active at a time
    // because clicks are serialised on the GPUI main thread.
    webview: Rc<RefCell<WebView>>,
    /// Current page URL displayed in the chrome's address pill.
    /// Initialised from the spawn URL; would track real
    /// navigations once we wire Wry's page-load callback.
    url: SharedString,
}

impl WebLinkViewShell {
    pub fn new(
        initial_url: Url,
        window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> Self {
        let win_size = window.bounds().size;
        // WebView fills the AppWindow content area below the chrome.
        // Wry positions its WKWebView as an NSView child of the
        // GPUI window, so the Rect is in window-local coords.
        let chrome_h = CHROME_HEIGHT as i32;
        let webview_rect = Rect {
            position: LogicalPosition::new(0i32, chrome_h).into(),
            size: LogicalSize::new(
                f32::from(win_size.width) as u32,
                f32::from(win_size.height) as u32 - chrome_h as u32,
            )
            .into(),
        };
        let webview = WebViewBuilder::new()
            .with_url(initial_url.as_str())
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("WebLinkView WebView build_as_child must succeed");
        Self {
            webview: Rc::new(RefCell::new(webview)),
            url: SharedString::from(initial_url.as_str().to_string()),
        }
    }
}

impl Render for WebLinkViewShell {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let webview = self.webview.clone();
        let url = self.url.clone();
        div()
            .size_full()
            .bg(rgb(0xffffff))
            .flex()
            .flex_col()
            .child(chrome_strip(webview, url))
        // The WebView is mounted as a sibling NSView via
        // `build_as_child` — it sits underneath this GPUI tree by
        // virtue of its `with_bounds` y-offset, not by being a
        // child node here.
    }
}

fn chrome_strip(webview: Rc<RefCell<WebView>>, url: SharedString) -> impl IntoElement {
    let wv_back = webview.clone();
    let wv_fwd = webview.clone();
    let wv_reload = webview.clone();
    div()
        .id("web-link-chrome")
        .h(px(CHROME_HEIGHT))
        .w_full()
        .px(px(12.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .bg(rgb(0xfafafa))
        .border_b_1()
        .border_color(rgb(0xececf1))
        .child(nav_button(
            "wlv-back",
            NavGlyph::Builtin(IconName::ChevronLeft),
            move || {
                if let Ok(wv) = wv_back.try_borrow() {
                    let _ = wv.evaluate_script("history.back();");
                }
            },
        ))
        .child(nav_button(
            "wlv-fwd",
            NavGlyph::Builtin(IconName::ChevronRight),
            move || {
                if let Ok(wv) = wv_fwd.try_borrow() {
                    let _ = wv.evaluate_script("history.forward();");
                }
            },
        ))
        .child(nav_button(
            "wlv-reload",
            NavGlyph::Custom("icons/reload.svg"),
            move || {
                if let Ok(wv) = wv_reload.try_borrow() {
                    let _ = wv.evaluate_script("location.reload();");
                }
            },
        ))
        .child(url_pill(url))
        .child(menu_dots())
}

enum NavGlyph {
    Builtin(IconName),
    Custom(&'static str),
}

fn nav_button<F>(id: &'static str, glyph: NavGlyph, on_click: F) -> impl IntoElement
where
    F: Fn() + 'static,
{
    let glyph_node: AnyElement = match glyph {
        NavGlyph::Builtin(name) => Icon::new(name)
            .size(px(16.0))
            .text_color(rgb(0x52525b))
            .into_any_element(),
        NavGlyph::Custom(path) => svg()
            .path(SharedString::from(path))
            .size(px(16.0))
            .text_color(rgb(0x52525b))
            .into_any_element(),
    };
    div()
        .id(id)
        .w(px(30.0))
        .h(px(30.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.0))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(0xf0f0f3)))
        .on_mouse_down(MouseButton::Left, move |_, _window, _cx| {
            on_click();
        })
        .child(glyph_node)
}

fn url_pill(url: SharedString) -> impl IntoElement {
    div()
        .flex_1()
        .h(px(32.0))
        .px(px(14.0))
        .ml(px(6.0))
        .mr(px(6.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .rounded_full()
        .bg(rgb(0xffffff))
        .border_1()
        .border_color(rgb(0xe4e4e7))
        .text_sm()
        .text_color(rgb(0x52525b))
        .child(
            Icon::new(IconName::Globe)
                .size(px(14.0))
                .text_color(rgb(0xa1a1aa)),
        )
        .child(div().flex_1().overflow_hidden().child(url))
}

fn menu_dots() -> impl IntoElement {
    let dot = || {
        div()
            .w(px(4.0))
            .h(px(4.0))
            .rounded_full()
            .bg(hsla(0.0, 0.0, 0.5, 1.0))
    };
    div()
        .w(px(30.0))
        .h(px(30.0))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(3.0))
        .child(dot())
        .child(dot())
        .child(dot())
}
