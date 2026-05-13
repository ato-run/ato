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

pub mod card_switcher;
pub mod content_windows;
pub mod control_bar;
pub mod focus_dispatcher;
pub mod gestures;
// `pub mod launcher;` was removed in Stage D — the legacy Launcher
// window is retired. Settings lives in `settings_window` as the
// `ato-settings` system capsule.
#[cfg(target_os = "macos")]
pub mod macos;
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

pub use card_switcher::open_card_switcher_window;
pub use control_bar::{
    open_control_bar_window, open_focus_control_bar, ControlBarShellPlaceholder,
};
pub use orchestrator::{open_app_window, AppWindowShell};

/// `ATO_DESKTOP_MULTI_WINDOW=1` opt-in flag. Defaults to **off** until
/// layer 4 (Control Bar UI, #172) lands, at which point the legacy
/// single-window path is removed and this flag goes away.
pub fn is_multi_window_enabled() -> bool {
    match std::env::var("ATO_DESKTOP_MULTI_WINDOW") {
        Ok(v) => {
            let trimmed = v.trim();
            !trimmed.is_empty() && !matches!(trimmed, "0" | "false" | "off" | "no")
        }
        Err(_) => false,
    }
}
