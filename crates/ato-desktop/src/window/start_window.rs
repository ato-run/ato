//! StartWindow — Wry-hosted HTML "new window" start surface.
//!
//! The start page is the `ato-start` system capsule. The built Astro
//! output is embedded at compile time via `include_dir!` and served
//! through a custom protocol handler. The served subdirectory is read
//! from `assets/system/ato-start/capsule.toml` (`run` field).
//! Real data is pre-injected as `window.__ATO_START_SNAPSHOT__` via
//! `with_initialization_script` at window construction time, so the
//! page renders immediately without a round-trip IPC request.

use std::borrow::Cow;

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, App, Bounds, Context, IntoElement, Pixels, Render, Size, WindowBounds,
    WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;
use include_dir::{include_dir, Dir};
use serde::Deserialize;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::http::Response;
use wry::{Rect, WebView, WebViewBuilder};

use crate::localization::{compose_init_script, resolve_locale, tr};
use crate::system_capsule::ato_start::build_start_snapshot;
use crate::system_capsule::ipc as system_ipc;
use crate::window::content_windows::{ContentWindowEntry, ContentWindowKind, OpenContentWindows};
use crate::window::webview_paste::{WebViewPasteShell, WebViewPasteSupport};
use crate::{impl_focusable_via_paste, paste_render_wrap};

pub struct StartWindowShell {
    _webview: WebView,
    window_size: Size<Pixels>,
    paste: WebViewPasteSupport,
}

impl_focusable_via_paste!(StartWindowShell, paste);

impl WebViewPasteShell for StartWindowShell {
    fn active_paste_target(&self) -> Option<&WebView> {
        Some(&self._webview)
    }
}

impl Render for StartWindowShell {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_webview_bounds(window);
        paste_render_wrap!(
            div().size_full().bg(rgb(0x111111)),
            cx,
            &self.paste.focus_handle
        )
    }
}

impl StartWindowShell {
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

const START_CAPSULE_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/assets/system/ato-start");
const START_CAPSULE_TOML: &str = include_str!("../../assets/system/ato-start/capsule.toml");
const START_SCHEME: &str = "capsule-start";

#[derive(Deserialize)]
struct StartCapsuleManifest {
    run: Option<String>,
}

fn start_run_dir_from_manifest() -> String {
    let run = toml::from_str::<StartCapsuleManifest>(START_CAPSULE_TOML)
        .ok()
        .and_then(|m| m.run)
        .unwrap_or_else(|| "dist".to_string());

    let trimmed = run.trim().trim_matches('/');
    if trimmed.is_empty() {
        return "dist".to_string();
    }
    if trimmed.split('/').any(|seg| seg == ".." || seg.is_empty()) {
        return "dist".to_string();
    }
    trimmed.to_string()
}

/// Spawn a fresh ato-start window. Always opens a new window — there
/// is no slot or focus-reuse pathway. Snapshot data is injected at
/// construction time via `with_initialization_script`.
pub fn open_start_window(cx: &mut App) -> Result<()> {
    let config = crate::config::load_config();
    let locale = resolve_locale(config.general.language);
    let snapshot = build_start_snapshot(cx, &config, locale);
    let snapshot_json = serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".to_string());
    let snapshot_script = format!("window.__ATO_START_SNAPSHOT__ = {};", snapshot_json);
    let init_script = compose_init_script(locale, Some(&snapshot_script));

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
    let queue_for_drain = queue.clone();
    let start_run_dir = start_run_dir_from_manifest();
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
        let start_url = format!("{START_SCHEME}://localhost/");
        let start_run_dir_for_protocol = start_run_dir.clone();
        let webview = WebViewBuilder::new()
            .with_asynchronous_custom_protocol(
                START_SCHEME.to_string(),
                move |_id, req, responder| {
                    let path = req.uri().path();
                    let file_path = if path == "/" || path.is_empty() {
                        "index.html"
                    } else {
                        path.strip_prefix('/').unwrap_or(path)
                    };
                    let content_path = format!("{}/{}", start_run_dir_for_protocol, file_path);
                    let (content_type, body, status) =
                        match START_CAPSULE_DIR.get_file(&content_path) {
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
                        .expect("start protocol response must build");
                    responder.respond(response);
                },
            )
            .with_url(&start_url)
            .with_initialization_script(&init_script)
            .with_ipc_handler(system_ipc::make_ipc_handler(queue.clone()))
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for the Start WebView");
        let shell = cx.new(|cx| StartWindowShell {
            _webview: webview,
            window_size: win_size,
            paste: WebViewPasteSupport::new(cx),
        });
        window.focus(&shell.read(cx).paste.focus_handle.clone(), cx);
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;

    cx.global_mut::<OpenContentWindows>().insert(
        handle.window_id().as_u64(),
        ContentWindowEntry {
            handle: *handle,
            kind: ContentWindowKind::Start,
            title: gpui::SharedString::from(tr(locale, "start.title")),
            subtitle: gpui::SharedString::from(tr(locale, "start.subtitle")),
            url: gpui::SharedString::from("capsule://desktop.ato.run/start"),
            capsule: None,
            last_focused_at: std::time::Instant::now(),
        },
    );

    system_ipc::spawn_drain_loop(cx, queue_for_drain, *handle);

    Ok(())
}
