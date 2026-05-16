//! `ato-settings` system-capsule host window.
//!
//! Stage C of the system-capsule refactor. The Control Bar's
//! Settings cog used to dispatch `OpenLauncherWindow + ShowSettings`,
//! which flipped the Launcher's `LauncherView::Settings` into a
//! GPUI-rendered settings tree. Stage C splits Settings off into its
//! own Wry-hosted window loading `assets/system/ato-settings/index.html`
//! — the same lifecycle pattern as `start_window.rs`.
//!
//! Stage D will retire the Launcher window entirely once Settings
//! lives here permanently.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use gpui::prelude::*;
use gpui::{
    div, px, rgb, size, AnyWindowHandle, App, Bounds, Context, IntoElement, Render, WeakEntity,
    WindowBounds, WindowDecorations, WindowOptions,
};
use gpui_component::TitleBar;
use wry::dpi::{LogicalPosition, LogicalSize};
use wry::{Rect, WebView, WebViewBuilder};

use crate::config::load_config;
use crate::localization::{compose_init_script, resolve_locale, tr};
use crate::settings::settings_snapshot_from_config;
use crate::system_capsule::ipc as system_ipc;
use crate::window::content_windows::{ContentWindowEntry, ContentWindowKind, OpenContentWindows};
use crate::window::webview_paste::{WebViewPasteShell, WebViewPasteSupport};
use crate::{impl_focusable_via_paste, paste_render_wrap};

pub struct SettingsWindowShell {
    pub(crate) _webview: WebView,
    paste: WebViewPasteSupport,
}

impl_focusable_via_paste!(SettingsWindowShell, paste);

impl WebViewPasteShell for SettingsWindowShell {
    fn active_paste_target(&self) -> Option<&WebView> {
        Some(&self._webview)
    }
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
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        paste_render_wrap!(
            div().size_full().bg(rgb(0xf5f3ff)),
            cx,
            &self.paste.focus_handle
        )
    }
}

/// GPUI global that holds a weak reference to the currently open
/// `SettingsWindowShell`, so that the capsule IPC dispatch can push
/// responses back to the WebView without needing `AppState`.
pub struct ActiveSettingsShell(pub Option<WeakEntity<SettingsWindowShell>>);

impl gpui::Global for ActiveSettingsShell {}

/// Slot tracking the single open Settings window so repeated
/// `ShowSettings` dispatches focus the existing window instead of
/// spawning duplicates — same pattern as `StoreWindowSlot`.
pub struct SettingsWindowSlot(pub Option<AnyWindowHandle>);

impl gpui::Global for SettingsWindowSlot {}

const SETTINGS_HTML: &str = include_str!("../../assets/system/ato-settings/index.html");

/// Open the Settings window, or focus it if it is already open.
pub fn open_settings_window(cx: &mut App) -> Result<()> {
    // Focus-on-existing: if the window is still alive, bring it to front.
    let existing = cx.global::<SettingsWindowSlot>().0;
    if let Some(handle) = existing {
        let result = handle.update(cx, |_, window, _| window.activate_window());
        match result {
            Ok(()) => return Ok(()),
            Err(_) => {
                // Window was closed without clearing the slot (e.g. user
                // closed via OS chrome). Clear the stale handle and fall
                // through to open a fresh window.
                cx.set_global(SettingsWindowSlot(None));
            }
        }
    }

    let bounds = Bounds::centered(None, size(px(1080.0), px(720.0)), cx);

    // Build an initialization script that seeds the snapshot before the
    // page JS runs — same pattern used for consent and launch windows.
    let (init_snapshot, locale) = {
        let cfg = load_config();
        let locale = resolve_locale(cfg.general.language);
        (settings_snapshot_from_config(&cfg), locale)
    };
    let settings_script = format!(
        "window.__ATO_SETTINGS_INIT__={};",
        serde_json::to_string(&init_snapshot).unwrap_or_else(|_| "null".to_string())
    );
    let init_script = compose_init_script(locale, Some(&settings_script));

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
        let shell = cx.new(|cx| SettingsWindowShell {
            _webview: webview,
            paste: WebViewPasteSupport::new(cx),
        });
        window.focus(&shell.read(cx).paste.focus_handle.clone(), cx);
        if let Ok(mut slot) = shell_slot_inner.lock() {
            *slot = Some(shell.downgrade());
        }
        cx.new(|cx| gpui_component::Root::new(shell, window, cx))
    })?;

    // Store the weak entity as a Global so IPC dispatch can hydrate the WebView.
    if let Ok(slot) = shell_slot.lock() {
        cx.set_global(ActiveSettingsShell(slot.clone()));
    }

    // Record this as the active singleton so focus-reuse works on the next call.
    cx.set_global(SettingsWindowSlot(Some(*handle)));

    // Register in the cross-window content registry. The Control
    // Bar reads MRU front URL to display `capsule://desktop.ato.run/settings`
    // while the window is in focus.
    cx.global_mut::<OpenContentWindows>().insert(
        handle.window_id().as_u64(),
        ContentWindowEntry {
            handle: *handle,
            kind: ContentWindowKind::Settings,
            title: gpui::SharedString::from(tr(locale, "settings.title")),
            subtitle: gpui::SharedString::from(tr(locale, "settings.nav.general")),
            url: gpui::SharedString::from("capsule://desktop.ato.run/settings"),
            capsule: None,
            last_focused_at: std::time::Instant::now(),
        },
    );

    system_ipc::spawn_drain_loop(cx, drain_queue, *handle);

    Ok(())
}
