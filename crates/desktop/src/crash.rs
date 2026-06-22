//! Process-wide crash / panic capture for the desktop shell.
//!
//! The release binary is (or, once the Windows taskbar work lands, will be)
//! built with `windows_subsystem = "windows"` — it has no attached console,
//! so the default panic handler writes to a detached stderr that nobody sees.
//! Worse, most panics in this crate happen inside GPUI's non-unwinding
//! `open_window` callback (every system window builds its Wry child WebView
//! there). A panic in that context cannot unwind, so the runtime turns it into
//! an immediate `abort()` — on Windows the fail-fast path, exit code
//! `0xc0000409` — with no Rust-level message surfaced anywhere.
//!
//! This module installs a panic hook that, for every panic:
//!   1. logs the full panic (message + location) through `tracing`, so it
//!      lands in `~/.ato/logs/ato-desktop.<date>.log`;
//!   2. writes a standalone crash report to
//!      `~/.ato/logs/ato-desktop-crash-<unix-ts>.txt`;
//!   3. on Windows, shows a modal dialog with the message and the report path
//!      so the user can read it and `Ctrl+C`-copy the whole thing to report.
//!
//! [`report_nonfatal`] exposes the same reporting surface to recoverable
//! failures (e.g. a WebView that could not be created) without panicking.

use std::backtrace::Backtrace;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use capsule::common::paths::ato_path_or_workspace_tmp;

/// Re-entrancy guard: a panic *inside* the hook (e.g. while formatting the
/// report or showing the dialog) must fall straight through to the default
/// handler instead of looping forever.
static IN_HOOK: AtomicBool = AtomicBool::new(false);

/// Install the global panic hook. Call once, early in `main`, after logging is
/// initialised so the `tracing::error!` below reaches the log file.
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if IN_HOOK.swap(true, Ordering::SeqCst) {
            default_hook(info);
            return;
        }

        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown location>".to_string());

        // The panic payload is `&str` for `panic!("literal")` and `String` for
        // formatted panics / `.expect("…")` with a `Display` cause.
        let payload = info.payload();
        let message = if let Some(s) = payload.downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "<non-string panic payload>".to_string()
        };

        tracing::error!(
            target: "panic",
            %location,
            %message,
            "ato-desktop panicked"
        );

        let backtrace = Backtrace::force_capture();
        let report = format!(
            "ato-desktop {version} crashed.\n\n\
             panicked at {location}:\n{message}\n\n\
             os: {os} {arch}\n\n\
             backtrace:\n{backtrace}\n",
            version = env!("CARGO_PKG_VERSION"),
            os = std::env::consts::OS,
            arch = std::env::consts::ARCH,
        );
        let report_path = write_crash_report(&report);

        #[cfg(target_os = "windows")]
        show_dialog(
            "Ato Desktop crashed",
            &location,
            &message,
            report_path.as_ref(),
        );

        // Reset before delegating so a future (non-aborting) panic is still
        // captured if the process somehow survives this one.
        IN_HOOK.store(false, Ordering::SeqCst);
        default_hook(info);
    }));
}

/// Report a recoverable failure with the same surface as a panic — logged,
/// written to a crash report file, and (on Windows) shown in a copyable
/// dialog — but without aborting the process. Used by the WebView build helper
/// so a child-WebView creation failure degrades gracefully instead of taking
/// the whole shell down.
pub fn report_nonfatal(title: &str, detail: &str) {
    tracing::error!(target: "panic", title, detail, "non-fatal failure reported");
    let report = format!(
        "ato-desktop {version} reported a non-fatal failure.\n\n\
         {title}\n{detail}\n\n\
         os: {os} {arch}\n",
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
    );
    let report_path = write_crash_report(&report);

    #[cfg(target_os = "windows")]
    show_dialog(title, "", detail, report_path.as_ref());

    #[cfg(not(target_os = "windows"))]
    let _ = report_path;
}

/// Write the report next to the rolling tracing logs. Best-effort: returns
/// `None` if the file could not be created (the dialog / log still fire).
fn write_crash_report(report: &str) -> Option<PathBuf> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = ato_path_or_workspace_tmp(format!("logs/ato-desktop-crash-{ts}.txt"));
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&path, report) {
        Ok(()) => Some(path),
        Err(error) => {
            tracing::warn!(%error, "failed to write crash report file");
            None
        }
    }
}

#[cfg(target_os = "windows")]
fn show_dialog(title: &str, location: &str, message: &str, report_path: Option<&PathBuf>) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW,
    };

    let location_line = if location.is_empty() {
        String::new()
    } else {
        format!("at {location}:\n")
    };
    let path_line = report_path
        .map(|p| format!("\n\nA full crash report was saved to:\n{}", p.display()))
        .unwrap_or_default();
    let body = format!(
        "{location_line}{message}{path_line}\n\n\
         (Press Ctrl+C to copy this message, then paste it into a bug report.)"
    );

    let body_w = to_wide(&body);
    let title_w = to_wide(title);
    // SAFETY: `MessageBoxW` with a null owner window is callable from any
    // thread (including a panicking one). The wide buffers are NUL-terminated
    // and outlive the synchronous call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            body_w.as_ptr(),
            title_w.as_ptr(),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
        );
    }
}

#[cfg(target_os = "windows")]
fn to_wide(s: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    std::ffi::OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
