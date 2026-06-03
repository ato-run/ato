//! Host POSIX-shell preflight for the source-native run/build path.
//!
//! Some recipe lifecycle hooks (`prestart`, source `build`/`install`) are
//! POSIX shell scripts that ato runs on the **host** via `/bin/sh -c`. On
//! Windows there is no `/bin/sh` unless the user installed Git Bash / MSYS2,
//! so the bare `Command::new("sh")` spawn fails with `os error 2` (file not
//! found). Left unhandled that surfaces to the user as a generic internal
//! error (E999) with no hint about what went wrong or what to do.
//!
//! This module turns that platform gap into a typed, actionable error
//! *before* (or instead of) the opaque spawn failure. Known catalog recipes
//! resolve through the OCI/runtime path and never reach here — this only
//! fires for genuinely source-native capsules whose host hooks require a
//! Unix shell.
//!
//! See issue #377.

use anyhow::Result;

/// Stable marker embedded in the error message so the diagnostics layer
/// (`adapters::output::diagnostics::mapping`) can recognize this failure and
/// map it to a typed, non-E999 diagnostic. Changing this string requires
/// updating the matching branch in `from_anyhow`.
pub(crate) const SOURCE_BUILD_SHELL_UNAVAILABLE_MARKER: &str = "source_build_shell_unavailable";

/// Returns `true` when a host POSIX shell (`/bin/sh` / `sh`) is available to
/// run shell-script lifecycle hooks.
///
/// On Unix this is always true. On Windows we probe `PATH` for `sh.exe`
/// (Git Bash / MSYS2 / Cygwin install it) so users who *do* have a POSIX
/// shell keep working; only the no-shell case is gated.
pub(crate) fn host_posix_shell_available() -> bool {
    #[cfg(unix)]
    {
        true
    }
    #[cfg(not(unix))]
    {
        path_has_posix_shell(std::env::var_os("PATH").as_deref())
    }
}

/// Pure `PATH`-probe used by [`host_posix_shell_available`] on non-Unix
/// platforms. Split out so it is unit-testable without mutating the process
/// environment.
#[cfg(any(not(unix), test))]
fn path_has_posix_shell(path: Option<&std::ffi::OsStr>) -> bool {
    let Some(path) = path else {
        return false;
    };
    std::env::split_paths(path).any(|dir| dir.join("sh.exe").is_file() || dir.join("sh").is_file())
}

/// Ensure a host POSIX shell is available before running a shell-script
/// lifecycle hook. Returns a typed, actionable error when it is not.
///
/// On platforms that ship `/bin/sh` (Linux, macOS) this is a no-op and the
/// existing behavior is preserved.
pub(crate) fn ensure_host_posix_shell(command: &str) -> Result<()> {
    ensure_host_posix_shell_inner(host_posix_shell_available(), command, std::env::consts::OS)
}

/// Testable core of [`ensure_host_posix_shell`]: the shell-availability check
/// and target platform are injected so both branches can be exercised on any
/// host.
fn ensure_host_posix_shell_inner(shell_available: bool, command: &str, os: &str) -> Result<()> {
    if shell_available {
        return Ok(());
    }
    Err(source_build_shell_unavailable_error(command, os))
}

/// Build the typed, actionable error for "this step needs a Unix shell but
/// the host has none". The message carries the marker, the platform, the
/// required tool, and what the user should do next.
pub(crate) fn source_build_shell_unavailable_error(command: &str, os: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "{marker}: this capsule's source-build / prestart step requires a POSIX shell \
         (/bin/sh), which is not available on platform={os}. command: `{command}`.\n\
         The source-build path runs lifecycle hooks through a Unix shell. Known catalog \
         recipes launch through the OCI/runtime path and never need a host shell — prefer a \
         registered recipe when one exists. For an unregistered repo, add a Windows-compatible \
         build script (PowerShell/cmd) or run on Linux/macOS (or WSL).",
        marker = SOURCE_BUILD_SHELL_UNAVAILABLE_MARKER,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn shell_available_is_ok_and_noop() {
        // Unix-style availability: never errors regardless of platform label.
        ensure_host_posix_shell_inner(true, "cd app && bun install", "windows")
            .expect("available shell must not error");
        ensure_host_posix_shell_inner(true, "make build", "linux")
            .expect("available shell must not error");
    }

    #[test]
    fn windows_without_shell_returns_typed_actionable_error() {
        let err = ensure_host_posix_shell_inner(false, "cd app && bun install", "windows")
            .expect_err("missing shell on windows must error");
        let message = format!("{err}");
        // Typed marker so diagnostics can map it away from generic E999.
        assert!(
            message.contains(SOURCE_BUILD_SHELL_UNAVAILABLE_MARKER),
            "message must carry the typed marker, got: {message}"
        );
        // Platform, required tool, and the failing command are all surfaced.
        assert!(
            message.contains("platform=windows"),
            "platform missing: {message}"
        );
        assert!(
            message.contains("/bin/sh"),
            "required tool missing: {message}"
        );
        assert!(
            message.contains("cd app && bun install"),
            "command missing: {message}"
        );
        // Actionable hints: known recipe path + Windows-compatible build script.
        assert!(
            message.contains("recipe"),
            "known-recipe hint missing: {message}"
        );
        assert!(
            message.to_lowercase().contains("powershell") || message.contains("WSL"),
            "windows-compatible build hint missing: {message}"
        );
    }

    #[test]
    fn error_message_is_not_a_bare_os_error() {
        // Regression guard: the previous behavior surfaced the opaque
        // "No such file or directory (os error 2)" spawn failure. The typed
        // error must replace that, not echo it.
        let err = source_build_shell_unavailable_error("sh ./build.sh", "windows");
        let message = format!("{err}");
        assert!(
            !message.contains("os error 2"),
            "typed error must not be the raw spawn failure: {message}"
        );
    }

    #[test]
    fn path_probe_detects_sh_exe() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("sh.exe"), b"stub").expect("write sh.exe");
        let path = OsString::from(dir.path());
        assert!(path_has_posix_shell(Some(path.as_os_str())));
    }

    #[test]
    fn path_probe_false_when_no_shell() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = OsString::from(dir.path());
        assert!(!path_has_posix_shell(Some(path.as_os_str())));
        assert!(!path_has_posix_shell(None));
    }
}
