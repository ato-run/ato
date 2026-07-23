//! Ato Desktop — Tauri shell entry.
//!
//! Migration principle: this shell has NO UI of its own. It hosts the bundled
//! `ato-pwa` Desktop build (a local-asset WebView) and exposes a typed
//! [`host::DesktopHost`] command/event adapter over the abstracted `runner`
//! crate (which supervises the `ato` CLI — the sole owner of capsule execution)
//! and the shared `protocol::intent` verb vocabulary.
//!
//! Privileged commands are granted ONLY to the bundled first-party UI window
//! (see `capabilities/default.json`, scoped to the `main` window label). Remote
//! origins (including https://ato.run) are never given Tauri capabilities.

mod host;

/// Run the desktop shell. Mobile-ready entry point (Tauri v2 convention) so the
/// same shell can target mobile/IoT hosts as the `runner` abstraction grows.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(host::DesktopHost::new())
        .invoke_handler(tauri::generate_handler![
            host::runner_status,
            host::runner_start,
            host::runner_stop,
            host::dispatch_privileged_intent,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Ato desktop shell");
}
