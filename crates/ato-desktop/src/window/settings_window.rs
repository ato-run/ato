//! `ato-settings` system-capsule host window.
//!
//! Stage C of the system-capsule refactor. The Control Bar's
//! Settings cog used to dispatch `OpenLauncherWindow + ShowSettings`,
//! which flipped the Launcher's `LauncherView::Settings` into a
//! GPUI-rendered settings tree. Stage C splits Settings off into its
//! own Wry-hosted window loading `assets/system/ato-settings/index.html`
//! — the same lifecycle pattern as `start_window.rs`.
//!
//! Each invocation spawns a fresh window; no slot, no focus-reuse.
//! Stage D will retire the Launcher window entirely once Settings
//! lives here permanently.

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, App, Bounds, Context, IntoElement, Render, WindowBounds,
    WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

use crate::system_capsule::ipc as system_ipc;
use crate::window::content_windows::{ContentWindowEntry, ContentWindowKind, OpenContentWindows};

pub struct SettingsWindowShell {
    _webview: WebView,
}

impl Render for SettingsWindowShell {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Pale-violet backdrop in case the HTML is still painting.
        div().size_full().bg(rgb(0xf5f3ff))
    }
}

const SETTINGS_HTML: &str = include_str!("../../assets/system/ato-settings/index.html");

/// Spawn a fresh Settings window. Mirrors `open_start_window` — no
/// slot, no focus-reuse. Stage D will route the Control Bar's
/// Settings cog through this directly (replacing the legacy
/// `OpenLauncherWindow + ShowSettings` two-step).
pub fn open_settings_window(cx: &mut App) -> Result<()> {
    let bounds = Bounds::centered(None, size(px(1080.0), px(720.0)), cx);
    let options = WindowOptions {
        titlebar: Some(TitleBar::title_bar_options()),
        focus: true,
        show: true,
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    };

    let queue = system_ipc::new_queue();
    let handle = cx.open_window(options, |window, cx| {
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
            .with_html(SETTINGS_HTML)
            .with_ipc_handler(system_ipc::make_ipc_handler(queue_for_ipc))
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for the Settings WebView");
        let shell = cx.new(|_cx| SettingsWindowShell { _webview: webview });
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;

    // Register in the cross-window content registry. The Control
    // Bar reads MRU front URL to display `ato://settings` while the
    // window is in focus.
    cx.global_mut::<OpenContentWindows>().insert(
        handle.window_id().as_u64(),
        ContentWindowEntry {
            handle: *handle,
            kind: ContentWindowKind::Launcher, // Phase 1 reuses the Launcher kind for the badge / card visual; Stage D will introduce a dedicated Settings variant if needed.
            title: gpui::SharedString::from("設定"),
            subtitle: gpui::SharedString::from("セキュリティ · ランタイム · ストア"),
            url: gpui::SharedString::from("ato://settings"),
            last_focused_at: std::time::Instant::now(),
        },
    );

    system_ipc::spawn_drain_loop(cx, queue, *handle);

    Ok(())
}
