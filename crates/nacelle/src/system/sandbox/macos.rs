//! macOS Seatbelt Sandbox Implementation
//!
//! Implements process sandboxing using macOS Seatbelt via `sandbox_init()`.
//! Generates dynamic SBPL (Sandbox Profile Language) profiles per call so
//! that policy decisions (allowed paths, IPC sockets, network) flow from
//! `SandboxPolicy` into kernel-enforced rules.
//!
//! ## API note: `sandbox_init` and `flags = 0`
//! `sandbox_init()` is annotated as deprecated in Apple's headers, but it
//! remains the supported kernel interface for custom SBPL profiles.
//! When called with `flags = 0`, the profile argument is treated as raw
//! SBPL source rather than a predefined profile name (the latter requires
//! `SANDBOX_NAMED = 0x1`). This pattern is used in production by
//! `always-further/nono` (v0.36+) and is functional through macOS 15.
//!
//! See:
//! - `claudedocs/research_phase13a_sandbox_best_practices_20260429.md`
//! - `docs/rfcs/accepted/ADR-007-macos-sandbox-api-strategy.md`
//! - `docs/rfcs/accepted/NACELLE_TERMINAL_SPEC.md` §4.1

use super::{SandboxPolicy, SandboxResult};
use anyhow::{Context, Result};
use std::ffi::CString;
use std::path::Path;
use tracing::{debug, info, warn};

// FFI bindings for sandbox_init.
//
// `sandbox_init` carries `__OSX_AVAILABLE_BUT_DEPRECATED` in libsandbox's
// header but is still the entry point used by `sandbox-exec(1)` itself
// (via `sandbox_compile_string` + `sandbox_apply`). Until Apple ships a
// supported successor, this is the path we have.
mod ffi {
    use std::os::raw::{c_char, c_int};

    /// `flags` argument: when non-zero (`SANDBOX_NAMED = 0x1`), the profile
    /// argument is interpreted as a predefined profile *name*. With
    /// `flags = 0` it is parsed as raw SBPL source.
    #[allow(dead_code)] // kept for documentation / future named-profile fallbacks
    pub const SANDBOX_NAMED: u64 = 0x0001;

    extern "C" {
        /// Apply a sandbox profile to the current process.
        ///
        /// Returns 0 on success, -1 on failure. `errorbuf` is populated with
        /// a malloc'd C string describing the failure; the caller must free
        /// it via `sandbox_free_error`.
        pub fn sandbox_init(
            profile: *const c_char,
            flags: u64,
            errorbuf: *mut *mut c_char,
        ) -> c_int;

        /// Free an error buffer returned by `sandbox_init`.
        pub fn sandbox_free_error(errorbuf: *mut c_char);
    }
}

/// Apply Seatbelt sandbox to the current process.
///
/// This function should be called in a `pre_exec` hook before executing
/// the child process. It generates an SBPL profile from `policy` and
/// applies it via `sandbox_init(flags = 0)`.
///
/// # Arguments
/// * `policy` - Sandbox policy defining allowed paths, IPC sockets, network.
///
/// # Returns
/// * `Ok(SandboxResult::fully_enforced)` on success.
/// * `Ok(SandboxResult::not_enforced(reason))` on dev-mode skip or kernel
///   rejection. The caller decides whether to abort the workload.
pub fn apply_seatbelt_sandbox(policy: &SandboxPolicy) -> Result<SandboxResult> {
    debug!(
        "Applying Seatbelt sandbox (dynamic SBPL, ipc_paths={})",
        policy.ipc_socket_paths.len()
    );

    if policy.development_mode {
        info!("Skipping Seatbelt sandbox in development mode");
        return Ok(SandboxResult::not_enforced(
            "Development mode: macOS sandbox skipped",
        ));
    }

    let profile = generate_sbpl_profile(policy);
    debug!("Generated SBPL profile ({} bytes)", profile.len());

    let profile_cstr =
        CString::new(profile).context("Generated SBPL profile contained an interior NUL byte")?;

    // flags = 0 → profile is raw SBPL source (NOT a predefined profile name).
    let mut error_buf: *mut std::os::raw::c_char = std::ptr::null_mut();
    let result = unsafe { ffi::sandbox_init(profile_cstr.as_ptr(), 0, &mut error_buf) };

    if result == 0 {
        info!(
            "Seatbelt sandbox applied (dynamic SBPL, {} ipc paths, network={})",
            policy.ipc_socket_paths.len(),
            policy.allow_network
        );
        return Ok(SandboxResult::fully_enforced());
    }

    let error_msg = if !error_buf.is_null() {
        let msg = unsafe { std::ffi::CStr::from_ptr(error_buf) }
            .to_string_lossy()
            .into_owned();
        unsafe { ffi::sandbox_free_error(error_buf) };
        msg
    } else {
        "Unknown sandbox error".to_string()
    };

    warn!("Seatbelt sandbox_init failed: {}", error_msg);
    Ok(SandboxResult::not_enforced(format!(
        "macOS sandbox init failed: {}",
        error_msg
    )))
}

/// Generate an SBPL (Sandbox Profile Language) profile from `policy`.
///
/// The output is a complete, parseable SBPL document including:
/// - `(version 1)` declaration
/// - `(deny default)` baseline
/// - Essential allow rules (process, signal, sysctl, mach-lookup,
///   ipc-posix-shm) needed for any Unix workload to run
/// - Keychain mach-lookup denies (defense in depth from `nono`)
/// - Network allow rules (when `policy.allow_network`)
/// - `read_write_paths` and `read_only_paths` allow rules
/// - IPC socket allow rules (file ops + Unix-domain socket network ops)
/// - Essential system path read/write rules
pub(crate) fn generate_sbpl_profile(policy: &SandboxPolicy) -> String {
    let mut profile = String::new();

    profile.push_str("(version 1)\n");

    if policy.development_mode {
        profile.push_str("\n; Development mode - permissive sandbox\n");
        profile.push_str("(allow default)\n");

        // Block writes to critical system locations even in dev mode.
        profile.push_str("(deny file-write*\n");
        profile.push_str("    (subpath \"/System\")\n");
        profile.push_str("    (subpath \"/usr\")\n");
        profile.push_str("    (subpath \"/bin\")\n");
        profile.push_str("    (subpath \"/sbin\")\n");
        profile.push_str(")\n");
        return profile;
    }

    profile.push_str("\n; Production mode - restrictive sandbox\n");
    profile.push_str("(deny default)\n");

    profile.push_str("\n; Essential operations\n");
    profile.push_str("(allow process-exec)\n");
    profile.push_str("(allow process-fork)\n");
    profile.push_str("(allow signal (target self))\n");
    profile.push_str("(allow sysctl-read)\n");

    profile.push_str("\n; Essential IPC primitives\n");
    profile.push_str("(allow mach-lookup)\n");
    profile.push_str("(allow ipc-posix-shm)\n");

    // ── Keychain protection (nono pattern) ───────────────────────────────
    // `mach-lookup` is broadly allowed above so workloads can talk to system
    // services they need (dyld helper, notify, etc.). Explicitly deny the
    // mach services that gate Keychain / authorisation. The capsule has its
    // own secret-injection path via env vars; it should never need to ask
    // securityd directly.
    profile.push_str("\n; Keychain & auth protection (deny secd / authd / agent)\n");
    profile.push_str("(deny mach-lookup (global-name \"com.apple.secd\"))\n");
    profile.push_str("(deny mach-lookup (global-name \"com.apple.SecurityServer\"))\n");
    profile.push_str("(deny mach-lookup (global-name \"com.apple.security.agent\"))\n");
    profile.push_str("(deny mach-lookup (global-name \"com.apple.authorizationd\"))\n");

    if policy.allow_network {
        profile.push_str("\n; Network access\n");
        profile.push_str("(allow network-outbound)\n");
        profile.push_str("(allow network-inbound)\n");
        profile.push_str("(allow system-socket)\n");
    }

    if !policy.read_write_paths.is_empty() {
        profile.push_str("\n; Read-write paths\n");
        for path in &policy.read_write_paths {
            if let Some(escaped_path) = escape_path_for_sbpl(path) {
                profile.push_str(&format!(
                    "(allow file-read* file-write* (subpath \"{}\"))\n",
                    escaped_path
                ));
            }
        }
    }

    if !policy.read_only_paths.is_empty() {
        profile.push_str("\n; Read-only paths\n");
        for path in &policy.read_only_paths {
            if let Some(escaped_path) = escape_path_for_sbpl(path) {
                profile.push_str(&format!(
                    "(allow file-read* (subpath \"{}\"))\n",
                    escaped_path
                ));
            }
        }
    }

    // ── IPC socket paths (ato-cli IPC Broker) ────────────────────────────
    // Sockets may not exist yet at sandbox-application time (the service is
    // started after the policy is built). Fall back to the parent directory
    // so bind/connect can succeed when the socket is created.
    //
    // We emit two rule families:
    //   1. file-read*/file-write* — needed for stat()/unlink() on the socket
    //      file inode and for the directory entry.
    //   2. network* (remote/local unix-socket) — needed for connect()/bind()
    //      on the AF_UNIX socket itself. Apple's Seatbelt models AF_UNIX
    //      operations under the network rule family even though Linux
    //      Landlock gates them via filesystem rules.
    if !policy.ipc_socket_paths.is_empty() {
        profile.push_str("\n; IPC socket paths (ato-cli IPC Broker)\n");
        for path in &policy.ipc_socket_paths {
            let target_escaped =
                escape_path_for_sbpl(path).or_else(|| path.parent().and_then(escape_path_for_sbpl));

            if let Some(escaped) = target_escaped {
                profile.push_str(&format!(
                    "(allow file-read* file-write* (subpath \"{}\"))\n",
                    escaped
                ));
                profile.push_str(&format!(
                    "(allow network* (remote unix-socket (subpath \"{}\")))\n",
                    escaped
                ));
                profile.push_str(&format!(
                    "(allow network* (local unix-socket (subpath \"{}\")))\n",
                    escaped
                ));
            }
        }
    }

    profile.push_str("\n; Essential system paths\n");
    profile.push_str("(allow file-read*\n");
    profile.push_str("    (literal \"/\")\n");
    profile.push_str("    (literal \"/dev/null\")\n");
    profile.push_str("    (literal \"/dev/random\")\n");
    profile.push_str("    (literal \"/dev/urandom\")\n");
    profile.push_str("    (subpath \"/dev/fd\")\n");
    profile.push_str("    (subpath \"/private/var/db/dyld\")\n");
    profile.push_str(")\n");

    profile.push_str("\n; Essential write locations\n");
    profile.push_str("(allow file-write*\n");
    profile.push_str("    (literal \"/dev/null\")\n");
    profile.push_str("    (subpath \"/dev/fd\")\n");
    profile.push_str(")\n");

    profile
}

/// Escape a path for safe inclusion in an SBPL string literal.
///
/// - Resolves symlinks (e.g. `/tmp` → `/private/tmp` on macOS) so that
///   rules match the kernel's canonical form.
/// - Escapes backslashes and double-quotes for SBPL string syntax.
/// - Returns `None` if the path cannot be canonicalised (does not exist
///   or is otherwise inaccessible). Callers handle the fallback (e.g.
///   IPC sockets fall back to the parent directory).
pub(crate) fn escape_path_for_sbpl(path: &Path) -> Option<String> {
    let resolved = path.canonicalize().ok()?;
    let path_str = resolved.to_str()?;

    // Reject control characters in paths — they would corrupt the SBPL
    // string literal and could be used to inject extra rules.
    if path_str.chars().any(|c| c.is_control()) {
        return None;
    }

    let escaped = path_str.replace('\\', "\\\\").replace('"', "\\\"");
    Some(escaped)
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_generate_sbpl_profile_dev_mode() {
        let policy = SandboxPolicy::new().with_development_mode(true);
        let profile = generate_sbpl_profile(&policy);

        assert!(profile.contains("(version 1)"));
        assert!(profile.contains("(allow default)"));
        assert!(profile.contains("(deny file-write*"));
        // Dev mode short-circuits before production rules — no `(deny default)`.
        assert!(!profile.contains("(deny default)\n"));
    }

    #[test]
    fn test_generate_sbpl_profile_production() {
        let policy = SandboxPolicy::new()
            .allow_read_write([PathBuf::from("/tmp")])
            .allow_read_only([PathBuf::from("/usr")])
            .with_network(true);

        let profile = generate_sbpl_profile(&policy);

        assert!(profile.contains("(version 1)"));
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(allow process-exec)"));
        assert!(profile.contains("(allow network-outbound)"));
        assert!(profile.contains("(allow network-inbound)"));
    }

    #[test]
    fn test_generate_sbpl_profile_denies_keychain() {
        // Production mode should always deny mach-lookup to securityd / authd
        // even though `(allow mach-lookup)` is present, because SBPL evaluates
        // rules in order and later denies override earlier allows.
        let policy = SandboxPolicy::new();
        let profile = generate_sbpl_profile(&policy);

        assert!(profile.contains("(allow mach-lookup)"));
        assert!(
            profile.contains("(deny mach-lookup (global-name \"com.apple.secd\"))"),
            "expected secd deny in profile:\n{profile}"
        );
        assert!(
            profile.contains("(deny mach-lookup (global-name \"com.apple.security.agent\"))"),
            "expected security.agent deny in profile:\n{profile}"
        );
        assert!(
            profile.contains("(deny mach-lookup (global-name \"com.apple.authorizationd\"))"),
            "expected authorizationd deny in profile:\n{profile}"
        );
    }

    #[test]
    fn test_generate_sbpl_profile_no_network_when_disabled() {
        let policy = SandboxPolicy::new().with_network(false);
        let profile = generate_sbpl_profile(&policy);

        assert!(profile.contains("(deny default)"));
        assert!(!profile.contains("(allow network-outbound)"));
        assert!(!profile.contains("(allow network-inbound)"));
    }

    #[test]
    fn test_generate_sbpl_profile_includes_ipc_socket_paths() {
        // /tmp exists on every macOS system; canonicalize() will resolve it
        // to /private/tmp. The profile must mention the resolved tail.
        let socket_path = PathBuf::from("/tmp/capsule-ipc/test-service.sock");
        let policy = SandboxPolicy::new().with_ipc_socket_paths([socket_path]);

        let profile = generate_sbpl_profile(&policy);

        assert!(
            profile.contains("; IPC socket paths"),
            "IPC section header missing:\n{profile}"
        );
        // The socket itself does not exist; fallback uses the parent dir.
        // Parent /tmp/capsule-ipc may or may not exist on the test runner —
        // if it does not, the loop falls back to the path's parent, which
        // for `/tmp/capsule-ipc/foo.sock` is `/tmp/capsule-ipc`. If that
        // also fails, no rule is emitted but the section header still is.
        // We just assert structural correctness here; real path-resolution
        // is covered by the next test.
        let has_file_rule = profile.contains("(allow file-read* file-write* (subpath");
        let has_unix_socket_rule = profile.contains("(allow network* (remote unix-socket");
        // At minimum, the header is emitted; if the parent resolved, both
        // rule families should appear together.
        if has_file_rule {
            assert!(
                has_unix_socket_rule,
                "if a file rule was emitted for IPC, the unix-socket rule must accompany it:\n{profile}"
            );
        }
    }

    #[test]
    fn test_generate_sbpl_profile_emits_ipc_rules_for_existing_parent() {
        // Use a guaranteed-existing parent (/tmp resolves to /private/tmp on macOS).
        let socket_path = PathBuf::from("/tmp/.capsule-ipc-sandbox-test.sock");
        let policy = SandboxPolicy::new().with_ipc_socket_paths([socket_path]);

        let profile = generate_sbpl_profile(&policy);

        // Either /tmp resolves (existing-path branch) or its parent / does
        // (parent-fallback branch). In either case the resolved string
        // should contain "tmp" and we expect three rule lines (file +
        // remote-socket + local-socket).
        let resolved_marker = if cfg!(target_os = "macos") {
            "/private/tmp"
        } else {
            "/tmp"
        };
        assert!(
            profile.contains(resolved_marker),
            "expected resolved tmp path in profile:\n{profile}"
        );
        assert!(
            profile.contains(&format!(
                "(allow file-read* file-write* (subpath \"{resolved_marker}\"))"
            )),
            "expected file rule for resolved tmp:\n{profile}"
        );
        assert!(
            profile.contains(&format!(
                "(allow network* (remote unix-socket (subpath \"{resolved_marker}\")))"
            )),
            "expected remote unix-socket rule for resolved tmp:\n{profile}"
        );
        assert!(
            profile.contains(&format!(
                "(allow network* (local unix-socket (subpath \"{resolved_marker}\")))"
            )),
            "expected local unix-socket rule for resolved tmp:\n{profile}"
        );
    }

    #[test]
    fn test_escape_path_for_sbpl_resolves_symlinks() {
        let escaped = escape_path_for_sbpl(&PathBuf::from("/tmp"));
        assert!(escaped.is_some(), "/tmp should canonicalise");
        let s = escaped.unwrap();
        if cfg!(target_os = "macos") {
            assert_eq!(s, "/private/tmp", "/tmp resolves to /private/tmp on macOS");
        } else {
            assert!(s.contains("tmp"));
        }
    }

    #[test]
    fn test_escape_path_for_sbpl_rejects_nonexistent() {
        // canonicalize() fails for non-existent paths.
        let escaped =
            escape_path_for_sbpl(&PathBuf::from("/nonexistent/path-that-cannot-exist-12345"));
        assert!(escaped.is_none());
    }

    #[test]
    fn test_apply_sandbox_dev_mode_skips() {
        let policy = SandboxPolicy::new().with_development_mode(true);
        let result = apply_seatbelt_sandbox(&policy).unwrap();

        assert!(!result.fully_enforced);
        assert!(!result.partially_enforced);
        assert!(
            result.message.contains("Development mode"),
            "message: {}",
            result.message
        );
    }
}
