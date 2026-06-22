//! Host POSIX-shell preflight for the source-native run path.
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
//! The shell-detection logic and the diagnostics marker live in
//! [`capsule::shell_support`] so the smoke runner (capsule) and this
//! run pipeline share one implementation; this module is the thin run-pipeline
//! wrapper. See issue #377.

use anyhow::Result;

/// Re-export of the shared marker so the diagnostics layer can match it via
/// `crate::application::shell_preflight::SOURCE_BUILD_SHELL_UNAVAILABLE_MARKER`.
pub(crate) use capsule::shell_support::SOURCE_BUILD_SHELL_UNAVAILABLE_MARKER;

/// Ensure a host POSIX shell is available before running a shell-script
/// lifecycle hook. Returns a typed, actionable error when it is not.
///
/// On platforms that ship `/bin/sh` (Linux, macOS) this is a no-op and the
/// existing behavior is preserved.
pub(crate) fn ensure_host_posix_shell(command: &str) -> Result<()> {
    ensure_host_posix_shell_inner(
        capsule::shell_support::host_posix_shell_available(),
        command,
        std::env::consts::OS,
    )
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

/// Build the typed, actionable error for "this step needs a Unix shell but the
/// host has none". Delegates to the shared message builder so the marker and
/// wording stay aligned with the smoke runner.
pub(crate) fn source_build_shell_unavailable_error(command: &str, os: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "{}",
        capsule::shell_support::source_build_shell_unavailable_message(command, os)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
