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

// Stable action tokens. Used as both the muda menu-item ids (matched against
// `MenuEvent.id`) and the taskbar Jump List `--jump-action` argument / control
// pipe message, so the tray and the taskbar drive the exact same handlers.
pub(crate) const ID_OPEN: &str = "open";
pub(crate) const ID_RUNNING: &str = "running";
pub(crate) const ID_STOP_ALL: &str = "stop-all";
pub(crate) const ID_QUIT: &str = "quit";

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
                        handle_action(&aa, &menu_id);
                    }
                }
            }
        })
        .detach();
}

/// Dispatch a lifecycle action token (`open` / `running` / `stop-all` /
/// `quit`). Shared by the system-tray menu and the taskbar Jump List (via the
/// control pipe), so both surfaces behave identically.
pub(crate) fn handle_action(aa: &gpui::AsyncApp, action: &str) {
    match action {
        ID_OPEN => {
            let _ = aa.update(tray_open_ato);
        }
        ID_RUNNING => {
            let _ = aa.update(tray_running_apps);
        }
        ID_STOP_ALL => {
            // Stop completes asynchronously; Desktop stays up.
            spawn_stop_all(aa, false);
        }
        ID_QUIT => {
            // Confirm (modal) on the UI thread; proceed only if there are no
            // running sessions or the user accepts. Stops then run to
            // completion *before* the app quits.
            let proceed = aa.update(|cx| {
                let running = running_session_count(cx);
                running == 0 || confirm_quit_dialog(running)
            });
            if proceed {
                crate::window::begin_shutdown();
                spawn_stop_all(aa, true);
            }
        }
        other => tracing::debug!(action = %other, "lifecycle action: unknown token"),
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

/// Spawn the bounded stop-all flow on the foreground executor.
fn spawn_stop_all(aa: &gpui::AsyncApp, quit_after: bool) {
    let aa_task = aa.clone();
    aa.foreground_executor()
        .spawn(async move { stop_all_bounded(aa_task, quit_after).await })
        .detach();
}

/// Stop all running sessions (source + OCI) and wait — up to a bounded
/// timeout — for the stops to actually complete, writing each session's
/// terminal state back to the registry as it finishes.
///
/// This is deliberately *not* `SessionRegistry::stop_all_running`, which is
/// fire-and-forget (spawns detached threads and never reconciles state). The
/// tray needs completion: `Stop All` must empty the running list, and `Quit`
/// must actually stop everything *before* the app exits. Desktop is left
/// running unless `quit_after` is set, in which case `cx.quit()` is called only
/// after the stops have completed (or the timeout elapses).
async fn stop_all_bounded(aa: gpui::AsyncApp, quit_after: bool) {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    const STOP_TIMEOUT: Duration = Duration::from_secs(12);

    // 1. On the UI thread: stop the import preview and mark every running
    //    session `Stopping`, taking ownership of the stop requests so nothing
    //    is left fire-and-forget.
    let requests = aa.update(|cx| {
        crate::system_capsule::ato_import::stop_active_import_preview_blocking(cx, "tray_stop_all");
        cx.global_mut::<SessionRegistry>().begin_stop_all()
    });
    let total = requests.len();
    if total == 0 {
        tracing::info!("tray: stop all — no running sessions");
        if quit_after {
            let _ = aa.update(|cx| cx.quit());
        }
        return;
    }

    // 2. Off the UI thread: run each stop on its own thread (source via the CLI
    //    stop path, OCI via the container stop path) and report results back.
    //    `pending` tracks sessions we have not yet heard back from, so any that
    //    never report can be resolved to a terminal state (not left `Stopping`).
    let mut pending: std::collections::HashSet<String> =
        requests.iter().map(|req| req.session_id.clone()).collect();
    let (tx, rx) = mpsc::channel::<(String, Result<bool, String>)>();
    for req in requests {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let result = if req.is_oci {
                crate::orchestrator::stop_oci_session(&req.session_id)
                    .map(|()| true)
                    .map_err(|e| e.to_string())
            } else {
                crate::orchestrator::stop_guest_session(&req.session_id).map_err(|e| e.to_string())
            };
            let _ = tx.send((req.session_id, result));
        });
    }
    drop(tx);

    // 3. Collect results with a bounded timeout, yielding the executor between
    //    polls so the UI stays responsive while stops run.
    let be = aa.background_executor();
    let deadline = Instant::now() + STOP_TIMEOUT;
    let mut results: Vec<(String, Result<bool, String>)> = Vec::new();
    while results.len() < total && Instant::now() < deadline {
        match rx.try_recv() {
            Ok(item) => {
                pending.remove(&item.0);
                results.push(item);
            }
            Err(mpsc::TryRecvError::Empty) => be.timer(Duration::from_millis(100)).await,
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
    }
    let stopped = results.len();
    let unconfirmed = pending.len();
    if unconfirmed > 0 {
        // We do not force-kill in this PR, so call them "unconfirmed", not
        // "forced". They are still moved out of `Stopping` below.
        tracing::warn!(
            unconfirmed,
            total,
            "tray: stop all — sessions did not confirm stop within timeout"
        );
    }

    // 4. On the UI thread: write terminal states and refresh running surfaces so
    //    `Stop All` empties the running list (Card Switcher / next Start open).
    //    Sessions that confirmed get their real result; sessions that never
    //    reported are resolved to `FailedToStop` so none are stuck `Stopping`.
    let _ = aa.update(|cx| {
        let registry = cx.global_mut::<SessionRegistry>();
        for (sid, result) in &results {
            registry.finish_stop_session(sid, result.clone());
        }
        for sid in &pending {
            registry.finish_stop_session(sid, Err("stop did not confirm within 12s".to_string()));
        }
        crate::window::card_switcher::refresh_session_snapshot(cx);
        tracing::info!(stopped, unconfirmed, "tray: stop all running apps complete");
    });

    // 5. Quit only after stops have completed (or timed out).
    if quit_after {
        let _ = aa.update(|cx| cx.quit());
    }
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
    let body = format!("{running} running app(s) are still active.\n\nStop them and quit Ato?");
    let text: Vec<u16> = body.encode_utf16().chain(std::iter::once(0)).collect();
    let caption: Vec<u16> = "Quit Ato"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
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
