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

use std::sync::{Arc, Mutex};

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, App, Bounds, Context, IntoElement, Render, WeakEntity, WindowBounds,
    WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

use crate::config::load_config;
use crate::settings::settings_snapshot_from_config;
use crate::system_capsule::ipc as system_ipc;
use crate::window::content_windows::{ContentWindowEntry, ContentWindowKind, OpenContentWindows};

pub struct SettingsWindowShell {
    pub(crate) _webview: WebView,
}

impl SettingsWindowShell {
    /// Push a new settings payload to the JS layer via `window.__ATO_SETTINGS_HYDRATE__`.
    pub fn hydrate(&self, payload_json: &str) {
        let script = format!(
            "typeof window.__ATO_SETTINGS_HYDRATE__==='function'&&window.__ATO_SETTINGS_HYDRATE__({})",
            payload_json
        );
        let _ = self._webview.evaluate_script(&script);
    }
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

/// GPUI global that holds a weak reference to the currently open
/// `SettingsWindowShell`, so that the capsule IPC dispatch can push
/// responses back to the WebView without needing `AppState`.
pub struct ActiveSettingsShell(pub Option<WeakEntity<SettingsWindowShell>>);

impl gpui::Global for ActiveSettingsShell {}

const SETTINGS_HTML: &str = include_str!("../../assets/system/ato-settings/index.html");

/// Spawn a fresh Settings window. Mirrors `open_start_window` — no
/// slot, no focus-reuse. Stage D will route the Control Bar's
/// Settings cog through this directly (replacing the legacy
/// `OpenLauncherWindow + ShowSettings` two-step).
pub fn open_settings_window(cx: &mut App) -> Result<()> {
    let bounds = Bounds::centered(None, size(px(1080.0), px(720.0)), cx);

    // Build an initialization script that seeds the snapshot before the
    // page JS runs — same pattern used for consent and launch windows.
    let init_snapshot = {
        let cfg = load_config();
        settings_snapshot_from_config(&cfg)
    };
    let init_script = format!(
        "window.__ATO_SETTINGS_INIT__={};",
        serde_json::to_string(&init_snapshot).unwrap_or_else(|_| "null".to_string())
    );

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
    // Slot to capture the shell entity so we can store it as a Global after
    // `cx.open_window` returns — same pattern as `open_boot_wizard_inner`.
    let shell_slot: Arc<Mutex<Option<WeakEntity<SettingsWindowShell>>>> =
        Arc::new(Mutex::new(None));
    let shell_slot_inner = Arc::clone(&shell_slot);

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
        let webview = WebViewBuilder::new()
            .with_html(SETTINGS_HTML)
            .with_initialization_script(&init_script)
            .with_ipc_handler(system_ipc::make_ipc_handler(queue.clone()))
            .with_bounds(webview_rect)
            .build_as_child(window)
            .expect("build_as_child must succeed for the Settings WebView");
        let shell = cx.new(|_cx| SettingsWindowShell { _webview: webview });
        if let Ok(mut slot) = shell_slot_inner.lock() {
            *slot = Some(shell.downgrade());
        }
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;

    // Store the weak entity as a Global so IPC dispatch can hydrate the WebView.
    if let Ok(slot) = shell_slot.lock() {
        cx.set_global(ActiveSettingsShell(slot.clone()));
    }

    // Register in the cross-window content registry. The Control
    // Bar reads MRU front URL to display `ato://settings` while the
    // window is in focus.
    cx.global_mut::<OpenContentWindows>().insert(
        handle.window_id().as_u64(),
        ContentWindowEntry {
            handle: *handle,
            kind: ContentWindowKind::Settings,
            title: gpui::SharedString::from("設定"),
            subtitle: gpui::SharedString::from("セキュリティ · ランタイム · ストア"),
            url: gpui::SharedString::from("ato://settings"),
            last_focused_at: std::time::Instant::now(),
        },
    );

    system_ipc::spawn_drain_loop(cx, drain_queue, *handle);

    Ok(())
}
