//! WebLinkView — Desktop's built-in browser view. NOT a capsule.
//!
//! This iteration:
//!   - Multi-tab: each tab owns its own Wry WebView; tab strip lets
//!     users switch / close / add tabs.
//!   - Chrome buttons (back / forward / reload) drive the ACTIVE
//!     tab's WebView via `evaluate_script`.
//!   - capsule:// link interception: `with_navigation_handler`
//!     cancels in-tab navigation when the target scheme is
//!     `capsule://` and instead pushes the URL onto a queue that a
//!     foreground drain loop converts into an `open_app_window`
//!     call — i.e. clicking a capsule:// link inside the browser
//!     pops a separate AppWindow.
//!
//! The Control Bar URL field shows `ato://web-viewer` (set in
//! `orchestrator::url_for_route`) while a WebLinkView is the MRU
//! front — i.e. the bar IDs the feature, the in-page chrome shows
//! the actual page URL. This keeps the bar stable across page
//! navigations inside the browser.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    div, hsla, px, rgb, svg, AnyElement, Context, Entity, IntoElement, MouseButton, Pixels,
    Render, SharedString, Size,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{Icon, IconName};
use url::Url;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

/// Height of the tab strip at the top of the WebLinkView window.
const TAB_STRIP_HEIGHT: f32 = 36.0;
/// Left padding for the tab strip — must clear the macOS traffic-light
/// buttons which are drawn by the OS on top of the client area.
#[cfg(target_os = "macos")]
const TAB_STRIP_LEFT_PAD: f32 = 80.0;
#[cfg(not(target_os = "macos"))]
const TAB_STRIP_LEFT_PAD: f32 = 8.0;
/// Height of the chrome row (back/forward/reload/URL/menu).
const CHROME_HEIGHT: f32 = 44.0;
/// Combined offset — the WebView starts this many pixels below the
/// AppWindow's content origin.
const TOTAL_TOP: f32 = TAB_STRIP_HEIGHT + CHROME_HEIGHT;

/// Default URL for new tabs created via the `+` button. Mirrors
/// Safari's "new tab opens a configured home page" convention.
const NEW_TAB_URL: &str = "https://ato.run/";

type CapsuleNavQueue = Arc<Mutex<Vec<String>>>;

struct Tab {
    id: usize,
    webview: WebView,
    url: SharedString,
    title: SharedString,
}

pub struct WebLinkViewShell {
    tabs: Vec<Tab>,
    active_tab_id: usize,
    next_tab_id: usize,
    capsule_nav_queue: CapsuleNavQueue,
    window_size: Size<Pixels>,
    url_input: Entity<InputState>,
    url_input_focused: bool,
}

impl WebLinkViewShell {
    pub fn new(
        initial_url: Url,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let win_size = window.bounds().size;
        let nav_queue: CapsuleNavQueue = Arc::new(Mutex::new(Vec::new()));
        let first_tab = build_tab(0, initial_url, window, win_size, nav_queue.clone());

        let initial_url_str: SharedString = first_tab.url.clone();
        let url_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://…")
                .default_value(initial_url_str)
        });
        cx.subscribe_in(
            &url_input,
            window,
            |this: &mut Self, input, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let url = input.read(cx).value().to_string();
                    if !url.is_empty() {
                        this.navigate_to(&url, window, cx);
                    }
                }
                InputEvent::Change => cx.notify(),
                InputEvent::Focus => {
                    this.url_input_focused = true;
                }
                InputEvent::Blur => {
                    this.url_input_focused = false;
                    cx.notify();
                }
            },
        )
        .detach();

        spawn_capsule_nav_drain(cx, nav_queue.clone());
        Self {
            tabs: vec![first_tab],
            active_tab_id: 0,
            next_tab_id: 1,
            capsule_nav_queue: nav_queue,
            window_size: win_size,
            url_input,
            url_input_focused: false,
        }
    }

    fn active_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == self.active_tab_id)
    }

    fn switch_to(&mut self, id: usize, cx: &mut Context<Self>) {
        if self.tabs.iter().any(|t| t.id == id) {
            self.active_tab_id = id;
            self.sync_visibility();
            cx.notify();
        }
    }

    fn close(&mut self, id: usize, cx: &mut Context<Self>) {
        let Some(pos) = self.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        // If we are removing the active tab, move focus to a
        // neighbour before drop so we don't end up with a stale
        // active_tab_id pointing nowhere.
        if id == self.active_tab_id && self.tabs.len() > 1 {
            let next_idx = if pos > 0 { pos - 1 } else { pos + 1 };
            self.active_tab_id = self.tabs[next_idx].id;
        }
        let _removed = self.tabs.remove(pos);
        // Drop runs WebView's destructor → tears down the WKWebView.
        self.sync_visibility();
        cx.notify();
    }

    fn add(&mut self, url: Url, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let tab = build_tab(id, url, window, self.window_size, self.capsule_nav_queue.clone());
        self.tabs.push(tab);
        self.active_tab_id = id;
        self.sync_visibility();
        cx.notify();
    }

    fn navigate_back(&self) {
        if let Some(t) = self.active_tab() {
            let _ = t.webview.evaluate_script("history.back();");
        }
    }
    fn navigate_forward(&self) {
        if let Some(t) = self.active_tab() {
            let _ = t.webview.evaluate_script("history.forward();");
        }
    }
    fn reload(&self) {
        if let Some(t) = self.active_tab() {
            let _ = t.webview.evaluate_script("location.reload();");
        }
    }

    fn navigate_to(&mut self, raw: &str, window: &mut gpui::Window, cx: &mut Context<Self>) {
        let normalized = normalize_url(raw);
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == self.active_tab_id) {
            let _ = tab.webview.load_url(&normalized);
            tab.url = SharedString::from(normalized.clone());
        }
        let ss: SharedString = normalized.into();
        self.url_input.update(cx, |state, cx| {
            state.set_value(ss, window, cx);
        });
        cx.notify();
    }

    fn sync_visibility(&self) {
        for tab in &self.tabs {
            let _ = tab.webview.set_visible(tab.id == self.active_tab_id);
        }
    }
}

fn build_tab(
    id: usize,
    url: Url,
    window: &mut gpui::Window,
    win_size: Size<Pixels>,
    queue: CapsuleNavQueue,
) -> Tab {
    let top = TOTAL_TOP as i32;
    let w = f32::from(win_size.width) as u32;
    let h = (f32::from(win_size.height) as u32).saturating_sub(top as u32);
    let webview_rect = Rect {
        position: LogicalPosition::new(0i32, top).into(),
        size: LogicalSize::new(w, h).into(),
    };
    let url_str = url.as_str().to_string();
    let queue_for_nav = queue.clone();
    let webview = WebViewBuilder::new()
        .with_url(&url_str)
        .with_bounds(webview_rect)
        // capsule:// link interception. The closure returns false to
        // cancel the in-tab navigation; the URL is queued for the
        // foreground drain loop which calls open_app_window to spawn
        // a separate AppWindow for it. Any other scheme is allowed.
        .with_navigation_handler(move |target: String| -> bool {
            if target.starts_with("capsule://") {
                if let Ok(mut q) = queue_for_nav.lock() {
                    q.push(target);
                }
                false
            } else {
                true
            }
        })
        .build_as_child(window)
        .expect("WebLinkView tab WebView build_as_child must succeed");
    Tab {
        id,
        webview,
        url: SharedString::from(url_str.clone()),
        title: SharedString::from(short_title_for_url(&url_str)),
    }
}

fn short_title_for_url(url: &str) -> String {
    Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
        .unwrap_or_else(|| url.to_string())
}

fn spawn_capsule_nav_drain(cx: &mut Context<WebLinkViewShell>, queue: CapsuleNavQueue) {
    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();
    fe.spawn(async move {
        loop {
            be.timer(Duration::from_millis(50)).await;
            let drained: Vec<String> = match queue.lock() {
                Ok(mut q) => std::mem::take(&mut *q),
                Err(_) => continue,
            };
            if drained.is_empty() {
                continue;
            }
            for url in drained {
                let _ = aa.update(|cx: &mut gpui::App| handle_capsule_url(cx, url));
            }
        }
    })
    .detach();
}

fn handle_capsule_url(cx: &mut gpui::App, url: String) {
    if !crate::window::is_multi_window_enabled() {
        return;
    }
    let handle = match url.strip_prefix("capsule://") {
        Some(s) => s.trim_end_matches('/').to_string(),
        None => return,
    };
    if handle.is_empty() {
        return;
    }
    let label = handle
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(&handle)
        .to_string();
    tracing::info!(
        target_handle = %handle,
        "WebLinkView capsule:// link → spawning AppWindow"
    );
    let route = crate::state::GuestRoute::CapsuleHandle { handle, label };
    if let Err(err) = crate::window::open_app_window(cx, route) {
        tracing::error!(error = %err, "WebLinkView capsule nav: open_app_window failed");
    }
}

impl Render for WebLinkViewShell {
    fn render(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity();
        let active_url = self
            .active_tab()
            .map(|t| t.url.clone())
            .unwrap_or_else(|| SharedString::from(""));

        // Sync the URL bar to the active tab's URL when the user is not
        // typing. Comparing first avoids a needless set_value every frame.
        if !self.url_input_focused {
            let current: SharedString = self.url_input.read(cx).value().to_string().into();
            if current != active_url {
                self.url_input.update(cx, |state, cx| {
                    state.set_value(active_url, window, cx);
                });
            }
        }

        let tab_snapshots: Vec<(usize, SharedString, bool)> = self
            .tabs
            .iter()
            .map(|t| (t.id, t.title.clone(), t.id == self.active_tab_id))
            .collect();

        div()
            .size_full()
            .bg(rgb(0xffffff))
            .flex()
            .flex_col()
            .child(tab_strip(tab_snapshots, entity.clone()))
            .child(chrome_strip(self.url_input.clone(), entity))
    }
}

fn tab_strip(
    tabs: Vec<(usize, SharedString, bool)>,
    entity: Entity<WebLinkViewShell>,
) -> impl IntoElement {
    let mut row = div()
        .h(px(TAB_STRIP_HEIGHT))
        .w_full()
        .pl(px(TAB_STRIP_LEFT_PAD))
        .pt(px(6.0))
        .gap(px(4.0))
        .flex()
        .items_end()
        .bg(rgb(0xf4f4f6))
        .border_b_1()
        .border_color(rgb(0xececf1));
    for (id, title, is_active) in tabs {
        row = row.child(tab_chip(id, title, is_active, entity.clone()));
    }
    row.child(new_tab_btn(entity))
}

fn tab_chip(
    id: usize,
    title: SharedString,
    is_active: bool,
    entity: Entity<WebLinkViewShell>,
) -> impl IntoElement {
    let entity_for_click = entity.clone();
    let entity_for_close = entity;
    let bg = if is_active { rgb(0xffffff) } else { rgb(0xeaeaee) };
    let text = if is_active { rgb(0x18181b) } else { rgb(0x6b6b78) };
    div()
        .id(SharedString::from(format!("tab-{id}")))
        .h(px(TAB_STRIP_HEIGHT - 6.0))
        .px(px(12.0))
        .min_w(px(160.0))
        .max_w(px(220.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .rounded_t(px(8.0))
        .bg(bg)
        .text_size(px(12.0))
        .font_weight(if is_active {
            gpui::FontWeight(600.0)
        } else {
            gpui::FontWeight(500.0)
        })
        .text_color(text)
        .cursor_pointer()
        .on_mouse_down(MouseButton::Left, move |_e, _w, cx| {
            entity_for_click.update(cx, |this, ctx| this.switch_to(id, ctx));
        })
        .child(div().flex_1().overflow_hidden().child(title))
        .child(
            div()
                .id(SharedString::from(format!("tab-close-{id}")))
                .w(px(16.0))
                .h(px(16.0))
                .rounded(px(4.0))
                .flex()
                .items_center()
                .justify_center()
                .hover(|s| s.bg(rgb(0xe4e4e7)))
                .on_mouse_down(MouseButton::Left, move |_, _w, cx| {
                    cx.stop_propagation();
                    entity_for_close.update(cx, |this, ctx| this.close(id, ctx));
                })
                .child(
                    Icon::new(IconName::Close)
                        .size(px(11.0))
                        .text_color(rgb(0x71717a)),
                ),
        )
}

fn new_tab_btn(entity: Entity<WebLinkViewShell>) -> impl IntoElement {
    div()
        .id("new-tab")
        .w(px(28.0))
        .h(px(28.0))
        .ml(px(4.0))
        .mb(px(2.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(6.0))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(0xe4e4e7)))
        .on_mouse_down(MouseButton::Left, move |_e, window, cx| {
            if let Ok(url) = Url::parse(NEW_TAB_URL) {
                entity.update(cx, |this, ctx| this.add(url, window, ctx));
            }
        })
        .child(
            Icon::new(IconName::Plus)
                .size(px(14.0))
                .text_color(rgb(0x52525b)),
        )
}

fn chrome_strip(
    url_input: Entity<InputState>,
    entity: Entity<WebLinkViewShell>,
) -> impl IntoElement {
    let e_back = entity.clone();
    let e_fwd = entity.clone();
    let e_reload = entity;
    div()
        .h(px(CHROME_HEIGHT))
        .w_full()
        .px(px(12.0))
        .flex()
        .items_center()
        .gap(px(6.0))
        .bg(rgb(0xfafafa))
        .border_b_1()
        .border_color(rgb(0xececf1))
        .child(nav_btn(
            "nav-back",
            NavGlyph::Builtin(IconName::ChevronLeft),
            move |cx| {
                e_back.update(cx, |this, _| this.navigate_back());
            },
        ))
        .child(nav_btn(
            "nav-fwd",
            NavGlyph::Builtin(IconName::ChevronRight),
            move |cx| {
                e_fwd.update(cx, |this, _| this.navigate_forward());
            },
        ))
        .child(nav_btn(
            "nav-reload",
            NavGlyph::Custom("icons/reload.svg"),
            move |cx| {
                e_reload.update(cx, |this, _| this.reload());
            },
        ))
        .child(url_pill(url_input))
        .child(menu_dots())
}

enum NavGlyph {
    Builtin(IconName),
    Custom(&'static str),
}

fn nav_btn<F>(id: &'static str, glyph: NavGlyph, on_click: F) -> impl IntoElement
where
    F: Fn(&mut gpui::App) + 'static,
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
        .on_mouse_down(MouseButton::Left, move |_, _window, cx| {
            on_click(cx);
        })
        .child(glyph_node)
}

fn url_pill(url_input: Entity<InputState>) -> impl IntoElement {
    div()
        .id("url-pill")
        .flex_1()
        .h(px(30.0))
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
        .child(
            div().flex_1().h_full().flex().items_center().child(
                Input::new(&url_input)
                    .appearance(false)
                    .bordered(false)
                    .focus_bordered(false)
                    .bg(hsla(0.0, 0.0, 0.0, 0.0))
                    .text_size(px(13.0))
                    .text_color(rgb(0x52525b)),
            ),
        )
}

fn normalize_url(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.contains("://") || trimmed.starts_with("about:") || trimmed.starts_with("data:") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    }
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
