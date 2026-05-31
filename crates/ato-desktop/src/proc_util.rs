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

/// Extension trait adding [`no_console_window`](CommandNoWindowExt::no_console_window)
/// to [`std::process::Command`].
pub trait CommandNoWindowExt {
    /// Spawn the child without allocating a console window on Windows.
    /// No-op on other platforms. Insert directly into a builder chain:
    ///
    /// ```ignore
    /// Command::new(bin).no_console_window().args(["..."]).output()
    /// ```
    fn no_console_window(&mut self) -> &mut Self;
}

impl CommandNoWindowExt for Command {
    #[cfg(target_os = "windows")]
    fn no_console_window(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW (0x0800_0000): run the console child without a
        // console window. The desktop has no console to inherit, so this is
        // purely "do not pop a new one". Note that `creation_flags` replaces
        // the whole creation-flag set, but no desktop spawn site sets other
        // flags, so this is safe.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        self.creation_flags(CREATE_NO_WINDOW)
    }

    #[cfg(not(target_os = "windows"))]
    fn no_console_window(&mut self) -> &mut Self {
        self
    }
}
