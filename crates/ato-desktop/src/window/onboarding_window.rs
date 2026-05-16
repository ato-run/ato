use std::borrow::Cow;

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, App, Bounds, Context, IntoElement, Pixels, Render, Size, WindowBounds,
    WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;
use include_dir::{include_dir, Dir};
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::http::Response;
use wry::{Rect, WebView, WebViewBuilder};

use crate::localization::{compose_init_script, resolve_locale};
use crate::system_capsule::ipc as system_ipc;
use crate::window::content_windows::{ContentWindowEntry, ContentWindowKind, OpenContentWindows};
use crate::window::webview_paste::{WebViewPasteShell, WebViewPasteSupport};
use crate::{impl_focusable_via_paste, paste_render_wrap};

const ONBOARDING_DIST: Dir = include_dir!("$CARGO_MANIFEST_DIR/assets/system/ato-onboarding/dist");
const ONBOARDING_SCHEME: &str = "capsule-onboarding";

pub struct OnboardingWindowShell {
    _webview: WebView,
    window_size: Size<Pixels>,
    paste: WebViewPasteSupport,
}

impl_focusable_via_paste!(OnboardingWindowShell, paste);

impl WebViewPasteShell for OnboardingWindowShell {
    fn active_paste_target(&self) -> Option<&WebView> {
        Some(&self._webview)
    }
}

impl Render for OnboardingWindowShell {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_webview_bounds(window);
        paste_render_wrap!(
            div().size_full().bg(rgb(0xffffff)),
            cx,
            &self.paste.focus_handle
        )
    }
}

impl OnboardingWindowShell {
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

pub fn open_onboarding_window(cx: &mut App) -> Result<()> {
    let config = crate::config::load_config();
    let locale = resolve_locale(config.general.language);
    let init_script = compose_init_script(locale, None);

    let bounds = Bounds::centered(None, size(px(750.0), px(900.0)), cx);
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
        let onboarding_url = format!("{ONBOARDING_SCHEME}://localhost/");
        let webview = WebViewBuilder::new()
            .with_asynchronous_custom_protocol(
                ONBOARDING_SCHEME.to_string(),
                |_id, req, responder| {
                    let path = req.uri().path();
                    let file_path = if path == "/" || path.is_empty() {
                        "index.html"
                    } else {
                        path.strip_prefix('/').unwrap_or(path)
                    };
                    let (content_type, body, status) = match ONBOARDING_DIST.get_file(file_path) {
                        Some(file) => {
                            let ext = file_path.rsplit('.').next().unwrap_or("");
                            let mime = match ext {
                                "html" => "text/html; charset=utf-8",
                                "js" => "application/javascript; charset=utf-8",
                                "css" => "text/css; charset=utf-8",
                                "png" => "image/png",
                                "svg" => "image/svg+xml",
                                "ico" => "image/x-icon",
                                "json" => "application/json",
                                _ => "application/octet-stream",
                            };
                            (mime, Cow::from(file.contents().to_vec()), 200)
                        }
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
                        .expect("onboarding protocol response must build");
                    responder.respond(response);
                },
            )
            .with_url(&onboarding_url)
            .with_initialization_script(&init_script)
            .with_ipc_handler(system_ipc::make_ipc_handler(queue.clone()))
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for onboarding WebView");
        let onboarding = cx.new(|cx| OnboardingWindowShell {
            _webview: webview,
            window_size: win_size,
            paste: WebViewPasteSupport::new(cx),
        });
        window.focus(&onboarding.read(cx).paste.focus_handle.clone(), cx);
        cx.new(|cx| gpui_component::Root::new(onboarding, window, cx))
    })?;

    cx.global_mut::<OpenContentWindows>().insert(
        handle.window_id().as_u64(),
        ContentWindowEntry {
            handle: *handle,
            kind: ContentWindowKind::Onboarding,
            title: gpui::SharedString::from("Onboarding"),
            subtitle: gpui::SharedString::from("Welcome to Ato"),
            url: gpui::SharedString::from("capsule://desktop.ato.run/onboarding"),
            capsule: None,
            last_focused_at: std::time::Instant::now(),
        },
    );

    system_ipc::spawn_drain_loop(cx, drain_queue, *handle);
    Ok(())
}
