//! Store window — mounts a Wry WebView loading the local `ato-store`
//! system capsule HTML from `assets/system/ato-store/index.html`.
//! The Control Bar's "ストア" button dispatches `OpenStoreWindow`
//! which lands here.
//!
//! The HTML is served via a `capsule-store://` custom protocol handler
//! so that WKWebView receives it with a proper origin (avoiding the
//! null-origin / `loadHTMLString:baseURL:nil` path that breaks JS on
//! macOS for larger HTML documents).

use std::borrow::Cow;

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, AnyWindowHandle, App, Bounds, Context, IntoElement, Pixels, Render, Size,
    WindowBounds, WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::http::Response;
use wry::{Rect, WebView, WebViewBuilder};

use crate::localization::{compose_init_script, resolve_locale, tr};
use crate::system_capsule::ipc as system_ipc;
use crate::window::webview_paste::{WebViewPasteShell, WebViewPasteSupport};
use crate::{impl_focusable_via_paste, paste_render_wrap};

/// URI scheme used for serving the store HTML via the custom protocol handler.
const STORE_SCHEME: &str = "capsule-store";

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
    window_size: Size<Pixels>,
    paste: WebViewPasteSupport,
}

impl_focusable_via_paste!(StoreWebView, paste);

impl WebViewPasteShell for StoreWebView {
    fn active_paste_target(&self) -> Option<&WebView> {
        Some(&self._webview)
    }
}

impl Render for StoreWebView {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_webview_bounds(window);
        paste_render_wrap!(
            div().size_full().bg(rgb(0xffffff)),
            cx,
            &self.paste.focus_handle
        )
    }
}

impl StoreWebView {
    fn sync_webview_bounds(&mut self, window: &mut gpui::Window) {
        let current = window.bounds().size;
        if current == self.window_size {
            return;
        }
        let _ = self._webview.set_bounds(Rect {
            position: LogicalPosition::new(0i32, 0i32).into(),
            size: LogicalSize::new(
                f32::from(current.width) as u32,
                f32::from(current.height) as u32,
            )
            .into(),
        });
        self.window_size = current;
    }
}

const STORE_HTML: &str = include_str!("../../assets/system/ato-store/index.html");

/// Fetch the capsule catalog from api.ato.run and return it as a JSON string.
/// Falls back to an empty array on any error.
fn fetch_capsules_json() -> String {
    match ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .get("https://api.ato.run/v1/capsules?limit=50")
        .call()
    {
        Ok(resp) => resp.into_string().unwrap_or_else(|_| "[]".to_string()),
        Err(err) => {
            tracing::warn!(?err, "ato-store: failed to fetch capsule catalog");
            "[]".to_string()
        }
    }
}

/// Open the Store window (local ato-store system capsule). On a 2nd+
/// click the existing window gets focused / brought to front rather
/// than a duplicate spawned. Returns the GPUI `WindowHandle` so the
/// Focus-mode boot path can use the Store as its initial window and
/// hand the handle to `focus_dispatcher::start`.
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

    let config = crate::config::load_config();
    let locale = resolve_locale(config.general.language);

    let win_size = size(px(1100.0), px(760.0));
    // Position just below the Focus-mode Control Bar (36 top + 56 height + 16 gap = 108).
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
    let queue = system_ipc::new_queue();
    let drain_queue = queue.clone();

    // The capsule catalog is injected via initialization script as
    // window.__CAPSULES_DATA__ before the page's init() runs.
    // Currently hardcoded to empty; real data will be fetched and injected
    // via evaluate_script after PageLoadEvent::Finished.
    let init_script = compose_init_script(locale, Some("window.__CAPSULES_DATA__ = [];"));

    let handle = cx.open_window(options, move |window, cx| {
        // Size the WebView to fill the window's content area.
        let win_size = window.bounds().size;
        let webview_rect = Rect {
            position: LogicalPosition::new(0i32, 0i32).into(),
            size: LogicalSize::new(
                f32::from(win_size.width) as u32,
                f32::from(win_size.height) as u32,
            )
            .into(),
        };
        let store_url = format!("{}://localhost/", STORE_SCHEME);
        let webview = WebViewBuilder::new()
            .with_asynchronous_custom_protocol(STORE_SCHEME.to_string(), |_id, _req, responder| {
                let body: Cow<'static, [u8]> = Cow::Borrowed(STORE_HTML.as_bytes());
                let response = Response::builder()
                    .header("Content-Type", "text/html; charset=utf-8")
                    .body(body)
                    .expect("store HTML response must build");
                responder.respond(response);
            })
            .with_url(&store_url)
            .with_initialization_script(&init_script)
            .with_ipc_handler(system_ipc::make_ipc_handler(queue.clone()))
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for the Store WebView");
        let store = cx.new(|cx| StoreWebView {
            _webview: webview,
            window_size: win_size,
            paste: WebViewPasteSupport::new(cx),
        });
        window.focus(&store.read(cx).paste.focus_handle.clone(), cx);
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
            title: gpui::SharedString::from(tr(locale, "store.title")),
            subtitle: gpui::SharedString::from(tr(locale, "store.loading")),
            url: gpui::SharedString::from("capsule://desktop.ato.run/store"),
            capsule: None,
            last_focused_at: std::time::Instant::now(),
        },
    );
    system_ipc::spawn_drain_loop(cx, drain_queue, *handle);
    Ok(*handle)
}
