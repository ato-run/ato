//! Shared subprocess helpers for the desktop runtime.
//!
//! `ato-desktop` is built as a Windows GUI-subsystem binary
//! (`#![windows_subsystem = "windows"]`), so it owns no console. When such a
//! process spawns a *console* subprocess (`ato`, `nacelle`, a package
//! manager, `git`, `taskkill`, …) Windows allocates a brand-new console
//! window for the child, which flashes on screen for the child's lifetime —
//! e.g. the empty command prompt that appears every time a capsule is
//! launched. Passing `CREATE_NO_WINDOW` suppresses that window.
//!
//! This module centralises the flag behind an extension trait so it can be
//! dropped into existing `Command` builder chains with a single call that is
//! a no-op everywhere except Windows.

use std::process::Command;

// `CommandNoWindowExt` (no-console-window spawn) is single-sourced in the
// runner crate's OS module — it is an OS execution primitive every host's
// process spawns want, not a desktop-shell concern. Re-exported here so the
// desktop's existing `crate::proc_util::CommandNoWindowExt` call sites are
// unchanged.
pub use runner::os::CommandNoWindowExt;

/// Reveal a local path (file or directory) in the OS file manager.
///
/// Uses `open` on macOS, `explorer` on Windows, `xdg-open` on Linux. Shares the
/// console-window suppression with the rest of the desktop's subprocess spawns.
pub(crate) fn open_path(path: &std::path::Path) -> std::io::Result<()> {
    let mut command = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(target_os = "windows") {
        Command::new("explorer")
    } else {
        Command::new("xdg-open")
    };
    CommandNoWindowExt::no_console_window(&mut command);
    // `explorer` returns a non-zero exit code even on success, so on Windows we
    // only treat a failure to *spawn* as an error.
    let status = command.arg(path).status()?;
    if cfg!(not(target_os = "windows")) && !status.success() {
        return Err(std::io::Error::other(format!(
            "file-manager open exited with status {status}"
        )));
    }
    Ok(())
}

/// Open a URL in the user's default browser using the OS shell.
///
/// Uses `open` on macOS, `cmd /C start` on Windows, `xdg-open` on Linux.
pub(crate) fn open_external_url(url: &str) -> std::io::Result<()> {
    let mut command = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        Command::new("xdg-open")
    };
    CommandNoWindowExt::no_console_window(&mut command);
    let status = command.arg(url).status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "browser-open exited with status {status}"
        )));
    }
    Ok(())
}
