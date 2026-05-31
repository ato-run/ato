//! Multi-window orchestration — layer 2 of the Focus View redesign (#169).
//!
//! Today's desktop opens exactly one GPUI window from `app::run`. The
//! redesign opens one window per running guest app, with a paired
//! Control Bar child window per app window (#171). This module owns
//! the spawn / despawn machinery and a minimal placeholder GPUI view
//! that gets installed in each new window until later layers (#171,
//! #172, #173) bring the real content.
//!
//! The full cut-over described in #169 also moves the `WebViewManager`
//! from a `HashMap<PaneId, ManagedWebView>` to a per-window singleton
//! and persists window frames under `~/.ato/desktop/windows.json`.
//! Both are deferred to follow-up commits on the same redesign branch
//! and tracked in the consolidated PR description.

pub mod app_capsule_shell;
pub mod auth_login_window;
pub mod capsule_panel;
pub mod card_switcher;
pub mod content_windows;
pub mod control_bar;
pub mod dock;
pub mod focus_dispatcher;
pub mod focus_guest_panes;
pub mod gestures;
pub mod identity_window;
pub mod import_window;
pub mod launch_window;
pub mod webview_paste;
// `pub mod launcher;` was removed in Stage D — the legacy Launcher
// window is retired. Settings lives in `settings_window` as the
// `ato-settings` system capsule.
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;
pub mod onboarding_window;
pub mod orchestrator;
pub mod settings_window;
pub mod start_window;
pub mod store;
pub mod web_bridge;
pub mod web_link_view;

// Make the pure-data `AppWindowRegistry` from `state` accessible
// across windows via `cx.global::<AppWindowRegistry>()`. The impl
// lives here (not in `state/`) so the state module stays free of
// UI-framework dependencies.
impl gpui::Global for crate::state::AppWindowRegistry {}
impl gpui::Global for content_windows::OpenContentWindows {}
impl gpui::Global for crate::state::session::SessionRegistry {}
impl gpui::Global for crate::state::capsule_state::CapsuleStateStore {}
impl gpui::Global for crate::system_capsule::window_registry::SystemCapsuleWindowRegistry {}

pub use card_switcher::open_card_switcher_window;
pub use control_bar::{
    control_bar_mode, focus_control_bar_input, hide_control_bar, install_control_bar_controller,
    open_control_bar_window, open_focus_control_bar, set_control_bar_mode, show_control_bar,
    toggle_control_bar, ControlBarController, ControlBarShellPlaceholder,
};
pub use orchestrator::{open_app_window, AppWindowShell};

/// Build a Wry child WebView, degrading gracefully on failure instead of
/// aborting the process.
///
/// Every system window builds its content WebView with
/// `WebViewBuilder::build_as_child` inside GPUI's non-unwinding `open_window`
/// callback. A `.expect()` there turns a recoverable WebView2/WKWebView error
/// (e.g. E_ACCESSDENIED creating the WebView2 user-data folder when installed
/// under `C:\Program Files`) into a hard process abort. Instead, route the
/// build through this helper: on failure it reports the error (logged + crash
/// report file + a copyable dialog on Windows) and returns `None`, so the
/// window renders empty but the process — and every other open window —
/// survives. Callers hold the result as `Option<WebView>`.
///
/// `context` is a short human label for the surface (e.g. "Start window") used
/// in the reported error.
pub(crate) fn build_child_webview(
    context: &str,
    builder: wry::WebViewBuilder<'_>,
    window: &gpui::Window,
) -> Option<wry::WebView> {
    match builder.build_as_child(window) {
        Ok(webview) => Some(webview),
        Err(err) => {
            crate::crash::report_nonfatal(
                &format!("{context} could not be created"),
                &format!("The embedded WebView failed to start:\n{err}"),
            );
            None
        }
    }
}

pub fn open_configured_startup_surface(
    cx: &mut gpui::App,
    startup_surface: crate::config::StartupSurface,
) -> anyhow::Result<()> {
    match startup_surface {
        crate::config::StartupSurface::Start => {
            start_window::open_start_window(cx)?;
            Ok(())
        }
        crate::config::StartupSurface::Blank => Ok(()),
        crate::config::StartupSurface::RestoreLast => {
            tracing::info!("RestoreLast not yet implemented — falling back to Store");
            store::open_store_window(cx)?;
            Ok(())
        }
        crate::config::StartupSurface::Store => {
            store::open_store_window(cx)?;
            Ok(())
        }
    }
}

/// Returns true if Focus View (multi-window) mode is active.
/// Reads `desktop.focus_view_enabled` from the config file (default: true).
/// The `ATO_DESKTOP_MULTI_WINDOW` env var is no longer honored; use the
/// config key to opt out of Focus View.
pub fn is_multi_window_enabled() -> bool {
    crate::config::load_config().desktop.focus_view_enabled
}
