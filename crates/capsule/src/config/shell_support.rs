//! Shared host POSIX-shell detection for the source-build / smoke paths.
//!
//! Source-native recipes run their prestart / build / smoke commands through a
//! Unix shell — either an explicit `/bin/sh -c` (prestart hook) or a smoke
//! `executable = "sh"` entry. On Windows there is no `/bin/sh` unless the user
//! installed Git Bash / MSYS2, so the spawn fails with `os error 2`.
//!
//! These helpers let both the smoke runner (capsule) and the run pipeline
//! (ato-cli) detect that gap and emit a single, marker-tagged message that the
//! ato-cli diagnostics layer maps to a typed `E213 source_build_shell_unavailable`
//! instead of a generic E999.
//!
//! Living in capsule keeps the marker and the detection logic in one place
//! that ato-cli can depend on (the reverse dependency is not allowed).
//!
//! See issue #377.

/// Stable marker embedded in error messages so `ato-cli`'s diagnostics layer
/// (`adapters::output::diagnostics::mapping::from_anyhow`) can recognise this
/// failure and map it to E213. Changing this string requires updating that
/// matching branch.
pub const SOURCE_BUILD_SHELL_UNAVAILABLE_MARKER: &str = "source_build_shell_unavailable";

/// Returns `true` when `executable` names a POSIX shell — `sh`, `/bin/sh`,
/// `sh.exe`, or any path whose final component is one of those. Such entries
/// require a Unix shell, which Windows does not provide by default.
///
/// Only `sh` is matched (not `bash`/`zsh`): the source-build path emits
/// `executable = "sh"` for shell-script hooks, which is exactly the
/// `…\sh` spawn failure reported in #377.
pub fn executable_requires_posix_shell(executable: &str) -> bool {
    let trimmed = executable.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Final path component, splitting on both POSIX and Windows separators so a
    // resolved path like `C:\…\sh` or `/bin/sh` is matched as well as bare `sh`.
    let base = trimmed.rsplit(['/', '\\']).next().unwrap_or(trimmed);
    base.eq_ignore_ascii_case("sh") || base.eq_ignore_ascii_case("sh.exe")
}

/// Whether a host POSIX shell (`/bin/sh` / `sh`) is available to run shell
/// scripts. Always `true` on Unix; on Windows we probe `PATH` for `sh` /
/// `sh.exe` (Git Bash / MSYS2 / Cygwin install one) so users who have a shell
/// keep working — only the no-shell case is gated.
pub fn host_posix_shell_available() -> bool {
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

/// Build the marker-tagged, actionable message describing the missing shell.
/// `context` is the offending command or executable, echoed back so the user
/// can see what failed.
pub fn source_build_shell_unavailable_message(context: &str, os: &str) -> String {
    format!(
        "{SOURCE_BUILD_SHELL_UNAVAILABLE_MARKER}: this source-build / prestart / smoke step \
         requires a POSIX shell (/bin/sh), which is not available on platform={os}. \
         requested: `{context}`.\n\
         Known catalog recipes launch through the OCI/runtime path and never need a host shell — \
         prefer a registered recipe when one exists. For an unregistered repo, add a \
         Windows-compatible build script (PowerShell/cmd) or run on Linux/macOS (or WSL)."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn detects_posix_shell_executables() {
        for exe in [
            "sh",
            " sh ",
            "/bin/sh",
            "/usr/bin/sh",
            "C:\\msys64\\usr\\bin\\sh.exe",
            "sh.exe",
        ] {
            assert!(
                executable_requires_posix_shell(exe),
                "{exe} should be detected as a POSIX shell"
            );
        }
    }

    #[test]
    fn ignores_non_shell_executables() {
        for exe in [
            "",
            "node",
            "python3",
            "uv",
            "bash",
            "/usr/bin/node",
            "pwsh.exe",
        ] {
            assert!(
                !executable_requires_posix_shell(exe),
                "{exe} should not be treated as a POSIX shell"
            );
        }
    }

    #[test]
    fn message_carries_marker_platform_tool_and_hints() {
        let message = source_build_shell_unavailable_message("sh", "windows");
        assert!(message.contains(SOURCE_BUILD_SHELL_UNAVAILABLE_MARKER));
        assert!(message.contains("platform=windows"));
        assert!(message.contains("/bin/sh"));
        assert!(message.contains('`'), "context must be echoed: {message}");
        assert!(message.contains("recipe"), "known-recipe hint missing");
        assert!(
            message.to_lowercase().contains("powershell") || message.contains("WSL"),
            "windows-compatible build hint missing: {message}"
        );
        assert!(
            !message.contains("os error 2"),
            "typed message must not be the raw spawn failure"
        );
    }

    #[test]
    fn path_probe_detects_sh() {
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
