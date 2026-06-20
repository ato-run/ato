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
pub mod community_import_window;
pub mod content_windows;
pub mod control_bar;
pub mod dock;
pub mod focus_dispatcher;
pub mod focus_guest_panes;
pub mod gestures;
pub mod import_window;
pub mod launch_window;
pub mod webview_paste;
// `pub mod launcher;` was removed in Stage D — the legacy Launcher
// window is retired. Settings lives in `settings_window` as the
// `ato-settings` system capsule.
#[cfg(target_os = "macos")]
pub mod macos;
pub mod onboarding_window;
pub mod orchestrator;
pub mod settings_window;
pub mod start_window;
pub mod store;
#[cfg(target_os = "windows")]
pub mod taskbar;
#[cfg(target_os = "windows")]
pub mod tray;
pub mod web_bridge;
pub mod web_link_view;
#[cfg(target_os = "windows")]
pub mod windows;

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
    ControlBarController, control_bar_mode, focus_control_bar_input, hide_control_bar,
    install_control_bar_controller, open_focus_control_bar, set_control_bar_mode, show_control_bar,
    toggle_control_bar,
};
pub use orchestrator::open_app_window;

pub(crate) fn stop_session_once_with_ui_completion(cx: &mut gpui::App, session_id: &str) {
    let request = cx
        .global_mut::<crate::state::session::SessionRegistry>()
        .begin_stop_session_once(session_id);
    let Some(request) = request else {
        tracing::debug!(
            session_id,
            "stop_session_once_with_ui_completion: stop already in progress or complete"
        );
        return;
    };

    tracing::info!(
        session_id = %request.session_id,
        is_oci = request.is_oci,
        "session stop requested"
    );

    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();

    fe.spawn(async move {
        let stop_session_id = request.session_id.clone();
        let is_oci = request.is_oci;
        let completion = be
            .spawn(async move {
                if is_oci {
                    crate::orchestrator::stop_oci_session(&stop_session_id).map(|()| true)
                } else {
                    crate::orchestrator::stop_guest_session(&stop_session_id)
                }
                .map_err(|error| format!("{error:#}"))
            })
            .await;

        let session_id = request.session_id;
        aa.update(move |cx| {
            let outcome = match &completion {
                Ok(true) => "stopped",
                Ok(false) => "already-inactive",
                Err(_) => "failed",
            };
            let error = completion.as_ref().err().cloned();
            cx.global_mut::<crate::state::session::SessionRegistry>()
                .finish_stop_session(&session_id, completion);
            crate::window::card_switcher::refresh_session_snapshot(cx);
            tracing::info!(
                session_id = %session_id,
                outcome,
                error = ?error,
                "session stop completed"
            );
        });
    })
    .detach();
}

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

/// Window caption shown in the OS taskbar / window list. GPUI creates its
/// windows with an empty title unless `TitlebarOptions::title` is set (it
/// is `None` in `TitleBar::title_bar_options()`), so on Windows the taskbar
/// thumbnail shows no app name. Every taskbar-visible window sets this via
/// `window.set_window_title` at construction time.
pub const WINDOW_TITLE: &str = "Ato Desktop";

/// Process-wide shutdown latch. Set once when an explicit quit begins (the
/// Start capsule's quit button, or an abnormal-exit path) so that
/// `on_window_closed` does not try to reopen the Start landing surface
/// while GPUI is already tearing the windows down.
static SHUTTING_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Mark the process as shutting down. Idempotent.
pub fn begin_shutdown() {
    SHUTTING_DOWN.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// True once [`begin_shutdown`] has been called.
pub fn is_shutting_down() -> bool {
    SHUTTING_DOWN.load(std::sync::atomic::Ordering::SeqCst)
}
