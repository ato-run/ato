//! Windows system tray (KOH-41).
//!
//! Adds a notification-area icon with a global lifecycle menu:
//!
//! ```text
//! Open Ato
//! Running Apps
//! ──────────────
//! Stop All Running Apps
//! ──────────────
//! Quit Ato
//! ```
//!
//! Window lifecycle and runtime (session) lifecycle are *separate* models —
//! closing an `AppWindow` does not stop its `Session` — so the tray is the
//! escape hatch for inspecting and stopping background-running sessions and
//! for quitting the whole Desktop.
//!
//! ## Event-loop integration
//!
//! The tray icon and its menu are created on the GPUI main thread inside
//! `application.run`, so GPUI's Win32 message loop (`GetMessage`/
//! `DispatchMessage`) pumps the hidden tray/menu windows' messages for free —
//! `DispatchMessage` routes by target HWND, not by owner. Menu selections are
//! delivered on `muda`'s process-global [`MenuEvent`] channel, which a GPUI
//! foreground poll task drains and dispatches onto `&mut App`.

use gpui::App;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};

use crate::state::session::{PresentationState, SessionRegistry};

// Stable menu-item ids matched against `MenuEvent.id`.
const ID_OPEN: &str = "ato.tray.open";
const ID_RUNNING: &str = "ato.tray.running_apps";
const ID_STOP_ALL: &str = "ato.tray.stop_all";
const ID_QUIT: &str = "ato.tray.quit";

/// Keeps the [`TrayIcon`] alive for the process lifetime. Dropping the icon
/// removes it from the notification area, so it is parked on an `App` global.
/// The field is never read — it exists purely as a liveness guard.
#[derive(Default)]
pub struct TrayHandle(#[allow(dead_code)] Option<TrayIcon>);
impl gpui::Global for TrayHandle {}

/// Install the Windows system tray. Idempotent-ish: a second call replaces the
/// previous tray icon. Logs and returns on any failure (the tray is a
/// convenience surface; its absence must never block startup).
pub fn install_tray(cx: &mut App) {
    let menu = Menu::new();
    let open = MenuItem::with_id(ID_OPEN, "Open Ato", true, None);
    let running = MenuItem::with_id(ID_RUNNING, "Running Apps", true, None);
    let stop_all = MenuItem::with_id(ID_STOP_ALL, "Stop All Running Apps", true, None);
    let quit = MenuItem::with_id(ID_QUIT, "Quit Ato", true, None);

    if let Err(err) = menu.append_items(&[
        &open,
        &running,
        &PredefinedMenuItem::separator(),
        &stop_all,
        &PredefinedMenuItem::separator(),
        &quit,
    ]) {
        tracing::error!(?err, "tray: failed to build menu");
        return;
    }

    let Some(icon) = build_icon() else {
        tracing::error!("tray: failed to build icon; skipping tray install");
        return;
    };

    let tray = match TrayIconBuilder::new()
        .with_tooltip("Ato Desktop")
        .with_menu(Box::new(menu))
        .with_icon(icon)
        .build()
    {
        Ok(tray) => tray,
        Err(err) => {
            tracing::error!(?err, "tray: failed to build tray icon");
            return;
        }
    };
    // Park the icon on a global so it is not dropped (which would remove it).
    cx.set_global(TrayHandle(Some(tray)));
    tracing::info!("tray: Windows system tray installed");

    // Drain muda's process-global menu-event channel from a GPUI foreground
    // task so handlers run on the main thread with `&mut App`. `receiver()`
    // returns a `'static` reference, so we re-fetch it each iteration rather
    // than depend on the channel type being `Clone`.
    let async_app = cx.to_async();
    async_app
        .foreground_executor()
        .spawn({
            let be = async_app.background_executor().clone();
            let aa = async_app.clone();
            async move {
                use std::time::Duration;
                loop {
                    be.timer(Duration::from_millis(150)).await;
                    while let Ok(event) = MenuEvent::receiver().try_recv() {
                        let menu_id: String = {
                            let id: &str = event.id.as_ref();
                            id.to_string()
                        };
                        let _ = aa.update(|cx| dispatch_tray_action(cx, &menu_id));
                    }
                }
            }
        })
        .detach();
}

fn dispatch_tray_action(cx: &mut App, id: &str) {
    match id {
        ID_OPEN => tray_open_ato(cx),
        ID_RUNNING => tray_running_apps(cx),
        ID_STOP_ALL => {
            let count = tray_stop_all(cx);
            tracing::info!(count, "tray: stop all running apps");
        }
        ID_QUIT => tray_quit(cx),
        other => tracing::debug!(id = %other, "tray: unknown menu id"),
    }
}

/// Bring the main Desktop surface (Focus Control Bar) to the foreground.
fn tray_open_ato(cx: &mut App) {
    if let Err(err) = crate::window::open_focus_control_bar(cx) {
        tracing::warn!(?err, "tray: open_focus_control_bar failed");
    }
}

/// Show the running apps / sessions surface (the Card Switcher renders the same
/// `SessionRegistry` view used by the Start page's running-apps row).
fn tray_running_apps(cx: &mut App) {
    if let Err(err) = crate::window::card_switcher::open_card_switcher_window(cx) {
        tracing::warn!(?err, "tray: open_card_switcher_window failed");
    }
}

/// Stop every running session (source + OCI) through the normal stop path,
/// leaving Desktop itself running. Returns the number of sessions stopped.
fn tray_stop_all(cx: &mut App) -> usize {
    crate::system_capsule::ato_import::stop_active_import_preview_blocking(cx, "tray_stop_all");
    cx.global_mut::<SessionRegistry>().stop_all_running()
}

/// Quit Desktop. If sessions are running, confirm first (native dialog); on
/// confirmation, stop all sessions before quitting.
fn tray_quit(cx: &mut App) {
    let running = running_session_count(cx);
    if running > 0 && !confirm_quit_dialog(running) {
        tracing::info!("tray: quit cancelled by user");
        return;
    }
    // Latch shutdown before stopping so `on_window_closed` does not race to
    // reopen the Start surface as GPUI tears windows down.
    crate::window::begin_shutdown();
    let count = tray_stop_all(cx);
    tracing::info!(count, "tray: quit — stopped running sessions");
    cx.quit();
}

/// Count sessions that are running (not stopped/failed) for the quit prompt.
fn running_session_count(cx: &App) -> usize {
    if !cx.has_global::<SessionRegistry>() {
        return 0;
    }
    cx.global::<SessionRegistry>()
        .view_entries()
        .iter()
        .filter(|e| {
            !matches!(
                e.presentation_state,
                PresentationState::Stopped | PresentationState::Failed
            )
        })
        .count()
}

/// Native confirmation dialog. Returns `true` when the user chooses to proceed.
fn confirm_quit_dialog(running: usize) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDOK, MB_ICONWARNING, MB_OKCANCEL, MessageBoxW,
    };
    let body = format!(
        "{running} running app(s) are still active.\n\nStop them and quit Ato?"
    );
    let text: Vec<u16> = body.encode_utf16().chain(std::iter::once(0)).collect();
    let caption: Vec<u16> = "Quit Ato".encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: null owner HWND, NUL-terminated UTF-16 strings kept alive across
    // the call. MessageBoxW is a synchronous, side-effect-free modal.
    let ret = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            MB_OKCANCEL | MB_ICONWARNING,
        )
    };
    ret == IDOK
}

/// Build a simple 32×32 RGBA tray icon in the Ato accent colour. Using a
/// generated icon avoids depending on a bundled `.ico` resource being present.
fn build_icon() -> Option<tray_icon::Icon> {
    const SIZE: u32 = 32;
    // Ato accent (#6C60F0), fully opaque.
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for _ in 0..(SIZE * SIZE) {
        rgba.extend_from_slice(&[0x6C, 0x60, 0xF0, 0xFF]);
    }
    tray_icon::Icon::from_rgba(rgba, SIZE, SIZE).ok()
}
