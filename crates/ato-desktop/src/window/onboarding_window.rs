use std::borrow::Cow;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, IntoElement, Pixels, Render, Size, WeakEntity, WindowBounds,
    WindowDecorations, WindowOptions, div, px, rgb, size,
};
use gpui_component::TitleBar;
use include_dir::{Dir, include_dir};
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::http::Response;
use wry::{Rect, WebView, WebViewBuilder};

use crate::localization::{compose_init_script, resolve_locale};
use crate::system_capsule::broker::SystemCapsuleId;
use crate::system_capsule::ipc as system_ipc;
use crate::window::content_windows::{ContentWindowEntry, ContentWindowKind, OpenContentWindows};
use crate::window::webview_paste::{WebViewPasteShell, WebViewPasteSupport};
use crate::{impl_focusable_via_paste, paste_render_wrap};

const ONBOARDING_DIST: Dir = include_dir!("$CARGO_MANIFEST_DIR/assets/system/ato-onboarding/dist");
const ONBOARDING_SCHEME: &str = "capsule-onboarding";

pub struct OnboardingWindowShell {
    pub(crate) _webview: Option<WebView>,
    window_size: Size<Pixels>,
    paste: WebViewPasteSupport,
}

impl_focusable_via_paste!(OnboardingWindowShell, paste);

impl WebViewPasteShell for OnboardingWindowShell {
    fn active_paste_target(&self) -> Option<&WebView> {
        self._webview.as_ref()
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
    /// Push a runtime-setup payload to the React app.
    pub fn hydrate(&self, payload_json: &str) {
        let script = format!(
            "typeof window.__ATO_ONBOARDING_HYDRATE__==='function'&&window.__ATO_ONBOARDING_HYDRATE__({})",
            payload_json
        );
        if let Some(webview) = self._webview.as_ref() {
            let _ = webview.evaluate_script(&script);
        }
    }

    fn sync_webview_bounds(&mut self, window: &mut gpui::Window) {
        let current = window.bounds().size;
        if current == self.window_size {
            return;
        }
        if let Some(webview) = self._webview.as_ref() {
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

/// Weak reference to the currently open onboarding shell so system-capsule IPC
/// can push foreground runtime-setup status/progress into the WebView.
pub struct ActiveOnboardingShell(pub Option<WeakEntity<OnboardingWindowShell>>);

impl gpui::Global for ActiveOnboardingShell {}

/// Push a Runtime Setup payload to the onboarding WebView, if one is open.
/// No-op when onboarding is not the active surface. Used by
/// [`crate::runtime_setup`] to fan a payload out to whichever surface is live.
pub fn hydrate_active_runtime_setup(cx: &mut App, payload_json: &str) {
    let weak = cx
        .try_global::<ActiveOnboardingShell>()
        .and_then(|g| g.0.clone());
    if let Some(entity) = weak.and_then(|w| w.upgrade()) {
        entity.update(cx, |shell, _cx| shell.hydrate(payload_json));
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
    let shell_slot: Arc<Mutex<Option<WeakEntity<OnboardingWindowShell>>>> =
        Arc::new(Mutex::new(None));
    let shell_slot_inner = Arc::clone(&shell_slot);

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
        let onboarding_url = format!("{ONBOARDING_SCHEME}://localhost/");
        let _wv_guard = crate::webview_init_guard::WebviewInitGuard::new();
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
            .with_ipc_handler(system_ipc::make_ipc_handler_for_capsule(
                SystemCapsuleId::AtoOnboarding,
                queue.clone(),
            ))
            .with_bounds(webview_rect);
        let webview = crate::window::build_child_webview("Onboarding window", webview, window);
        let onboarding = cx.new(|cx| OnboardingWindowShell {
            _webview: webview,
            window_size: win_size,
            paste: WebViewPasteSupport::new(cx),
        });
        window.focus(&onboarding.read(cx).paste.focus_handle.clone(), cx);
        if let Ok(mut slot) = shell_slot_inner.lock() {
            *slot = Some(onboarding.downgrade());
        }
        cx.new(|cx| gpui_component::Root::new(onboarding, window, cx))
    })?;

    if let Ok(slot) = shell_slot.lock() {
        cx.set_global(ActiveOnboardingShell(slot.clone()));
    }

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

    cx.global_mut::<crate::system_capsule::window_registry::SystemCapsuleWindowRegistry>()
        .register(SystemCapsuleId::AtoOnboarding, *handle);
    system_ipc::spawn_drain_loop(cx, drain_queue, *handle);
    Ok(())
}
