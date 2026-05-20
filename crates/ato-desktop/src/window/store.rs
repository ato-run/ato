use std::borrow::Cow;

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, AnyWindowHandle, App, Bounds, Context, IntoElement, Pixels, Render, Size,
    WindowBounds, WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;
use include_dir::{include_dir, Dir};
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::http::Response;
use wry::{Rect, WebView, WebViewBuilder};

use crate::localization::{compose_init_script, resolve_locale, tr};
use crate::system_capsule::ipc as system_ipc;
use crate::window::webview_paste::{WebViewPasteShell, WebViewPasteSupport};
use crate::{impl_focusable_via_paste, paste_render_wrap};

const STORE_DIST: Dir = include_dir!("$CARGO_MANIFEST_DIR/assets/system/ato-store/dist");
const STORE_SCHEME: &str = "capsule-store";

#[derive(Default)]
pub struct StoreWindowSlot(pub Option<AnyWindowHandle>);
impl gpui::Global for StoreWindowSlot {}

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

pub fn open_store_window(cx: &mut App) -> Result<AnyWindowHandle> {
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

    let init_script = compose_init_script(locale, Some(""));

    let handle = cx.open_window(options, move |window, cx| {
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
            .with_asynchronous_custom_protocol(STORE_SCHEME.to_string(), |_id, req, responder| {
                let path = req.uri().path();
                let file_path = if path == "/" || path.is_empty() {
                    "index.html".to_string()
                } else {
                    path.trim_start_matches('/').to_string()
                };
                let (content_type, body, status) = match resolve_store_file(&file_path) {
                    Some((mime, data)) => (mime, Cow::from(data), 200),
                    None => (
                        "text/plain; charset=utf-8",
                        Cow::Borrowed(b"not found" as &[u8]),
                        404,
                    ),
                };
                let response = Response::builder()
                    .status(status)
                    .header("Content-Type", content_type)
                    .body(body)
                    .expect("store protocol response must build");
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

fn resolve_store_file(file_path: &str) -> Option<(&'static str, Vec<u8>)> {
    let resolved = if file_path.ends_with('/') || file_path.is_empty() {
        format!("{}index.html", file_path)
    } else {
        file_path.to_string()
    };

    STORE_DIST.get_file(&resolved).map(|file| {
        let ext = resolved.rsplit('.').next().unwrap_or("");
        let mime = match ext {
            "html" => "text/html; charset=utf-8",
            "js" => "application/javascript; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "png" => "image/png",
            "svg" => "image/svg+xml",
            "ico" => "image/x-icon",
            "json" => "application/json",
            "ttf" => "font/ttf",
            "woff" => "font/woff",
            "woff2" => "font/woff2",
            _ => "application/octet-stream",
        };
        (mime, file.contents().to_vec())
    })
}
