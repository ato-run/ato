//! Developer Console window — mounts a Wry WebView loading the local
//! `ato-dev-console` system capsule HTML from
//! `assets/system/ato-dev-console/index.html`.
//!
//! The HTML is served via a `capsule-dev-console://` custom protocol
//! handler so that WKWebView receives it with a proper origin.
//!
//! The console lets publishers manage their capsules, set up a Dock,
//! and monitor publish status. The URL is:
//!   `capsule://run.ato.desktop/dev-console`

use std::borrow::Cow;

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, AnyWindowHandle, App, Bounds, Context, IntoElement, Render, WindowBounds,
    WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::http::Response;
use wry::{Rect, WebView, WebViewBuilder};

use crate::localization::{compose_init_script, resolve_locale, tr};
use crate::system_capsule::ipc as system_ipc;

const DEV_CONSOLE_SCHEME: &str = "capsule-dev-console";

/// Slot tracking the single open Developer Console window.
#[derive(Default)]
pub struct DevConsoleWindowSlot(pub Option<AnyWindowHandle>);
impl gpui::Global for DevConsoleWindowSlot {}

/// Lightweight GPUI entity whose only job is to keep the Wry
/// `WebView` alive for the lifetime of its window.
pub struct DevConsoleWebView {
    _webview: WebView,
}

impl Render for DevConsoleWebView {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div().size_full().bg(rgb(0xffffff))
    }
}

const DEV_CONSOLE_HTML: &str =
    include_str!("../../assets/system/ato-dev-console/index.html");

/// Open the Developer Console window. On a 2nd+ click the existing
/// window gets focused / brought to front rather than spawning a
/// duplicate. Returns the GPUI `WindowHandle`.
pub fn open_dev_console_window(cx: &mut App) -> Result<AnyWindowHandle> {
    // Focus-on-existing
    let existing = cx.global::<DevConsoleWindowSlot>().0;
    if let Some(handle) = existing {
        let result = handle.update(cx, |_, window, _| window.activate_window());
        match result {
            Ok(()) => return Ok(handle),
            Err(_) => {
                cx.set_global(DevConsoleWindowSlot(None));
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
            Bounds { origin: gpui::point(left, top), size: win_size }
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

    let init_script = compose_init_script(locale, None);

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
        let url = format!("{}://localhost/", DEV_CONSOLE_SCHEME);
        let webview = WebViewBuilder::new()
            .with_asynchronous_custom_protocol(
                DEV_CONSOLE_SCHEME.to_string(),
                |_id, _req, responder| {
                    let body: Cow<'static, [u8]> =
                        Cow::Borrowed(DEV_CONSOLE_HTML.as_bytes());
                    let response = Response::builder()
                        .header("Content-Type", "text/html; charset=utf-8")
                        .body(body)
                        .expect("dev console HTML response must build");
                    responder.respond(response);
                },
            )
            .with_url(&url)
            .with_initialization_script(&init_script)
            .with_ipc_handler(system_ipc::make_ipc_handler(queue.clone()))
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for the Dev Console WebView");
        let view = cx.new(|_cx| DevConsoleWebView { _webview: webview });
        cx.new(|cx| gpui_component::Root::new(view, window, cx))
    })?;
    cx.set_global(DevConsoleWindowSlot(Some(*handle)));

    use crate::window::content_windows::{
        ContentWindowEntry, ContentWindowKind, OpenContentWindows,
    };
    cx.global_mut::<OpenContentWindows>().insert(
        handle.window_id().as_u64(),
        ContentWindowEntry {
            handle: *handle,
            kind: ContentWindowKind::DevConsole,
            title: gpui::SharedString::from(tr(locale, "dev_console.title")),
            subtitle: gpui::SharedString::from(tr(locale, "dev_console.subtitle")),
            url: gpui::SharedString::from("capsule://run.ato.desktop/dev-console"),
            last_focused_at: std::time::Instant::now(),
        },
    );
    system_ipc::spawn_drain_loop(cx, drain_queue, *handle);
    Ok(*handle)
}
