//! StartWindow — Wry-hosted HTML "new window" start surface.
//!
//! The start page is now the `ato-start` system capsule, served from
//! `assets/system/ato-start/index.html`. Real data is pre-injected as
//! `window.__ATO_START_SNAPSHOT__` via `with_initialization_script`
//! at window construction time, so the page renders immediately without
//! a round-trip IPC request.

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, App, Bounds, Context, IntoElement, Pixels, Render, Size, WindowBounds,
    WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;
use wry::dpi::{LogicalPosition, LogicalSize};
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

const START_HTML: &str = include_str!("../../assets/system/ato-start/index.html");

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
        let queue_for_ipc = queue.clone();
        let webview = WebViewBuilder::new()
            .with_html(START_HTML)
            .with_initialization_script(&init_script)
            .with_ipc_handler(system_ipc::make_ipc_handler(queue_for_ipc))
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
