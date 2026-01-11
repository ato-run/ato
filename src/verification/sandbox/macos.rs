//! macOS Seatbelt (sandbox-exec) Sandbox Implementation
//!
//! Implements process sandboxing using macOS Seatbelt/sandbox-exec.
//! Uses the SBPL (Sandbox Profile Language) to define access policies.
//!
//! ## Features
//! - File system access control (read, write)
//! - Network access control
//! - Process execution control
//!
//! ## References
//! - Apple Sandbox Guide (SBPL syntax)
//! - sandbox_init(3) man page
//!
//! ## Note on API
//! macOS sandbox_init() is DEPRECATED and only supports predefined profiles.
//! For custom profiles, we generate an SBPL file and execute via sandbox-exec
//! wrapper, or use the private sandbox_init_with_parameters() function.
//! In practice, for v2.0 we use a simplified approach with development mode.

use super::{SandboxPolicy, SandboxResult};
use anyhow::{Context, Result};
use std::ffi::CString;
use std::path::Path;
use tracing::{debug, info, warn};

// FFI bindings for sandbox_init
// Note: sandbox_init is deprecated but still functional
mod ffi {
    use std::os::raw::{c_char, c_int};

    // Sandbox profile constants (from sandbox.h)
    pub const SANDBOX_NAMED: u64 = 0x0001;

    // Custom profile string flag (undocumented but used by sandbox-exec)
    // This allows passing SBPL directly instead of a profile name
    pub const SANDBOX_NAMED_EXTERNAL: u64 = 0x0003;

    extern "C" {
        /// Initialize sandbox with a profile
        /// For SANDBOX_NAMED: profile is one of kSBXProfile* constants
        /// For SANDBOX_NAMED_EXTERNAL: profile is a path to .sb file
        /// Returns 0 on success, -1 on failure
        pub fn sandbox_init(
            profile: *const c_char,
            flags: u64,
            errorbuf: *mut *mut c_char,
        ) -> c_int;

        /// Free error buffer from sandbox_init
        pub fn sandbox_free_error(errorbuf: *mut c_char);
    }
}

/// Predefined sandbox profile names (from sandbox.h)
const PROFILE_NO_INTERNET: &str = "kSBXProfileNoInternet";
const PROFILE_NO_NETWORK: &str = "kSBXProfileNoNetwork";
const PROFILE_NO_WRITE: &str = "kSBXProfileNoWrite";
const PROFILE_NO_WRITE_EXCEPT_TEMP: &str = "kSBXProfileNoWriteExceptTemporary";
const PROFILE_PURE_COMPUTATION: &str = "kSBXProfilePureComputation";

/// Apply Seatbelt sandbox to the current process
///
/// This function should be called in a pre_exec hook before executing
/// the child process. It restricts access according to the policy.
///
/// # Arguments
/// * `policy` - Sandbox policy defining allowed paths
///
/// # Returns
/// * `Ok(SandboxResult)` - Sandbox applied
/// * `Err` - Failed to apply sandbox
pub fn apply_seatbelt_sandbox(policy: &SandboxPolicy) -> Result<SandboxResult> {
    debug!("Applying Seatbelt sandbox");

    // For now, use a predefined profile based on policy settings
    // Custom SBPL profiles require writing to a file and using sandbox-exec
    // which is not suitable for pre_exec hooks

    // In development mode, skip sandboxing (macOS sandbox_init is limited)
    if policy.development_mode {
        info!("Skipping sandbox in development mode (macOS)");
        return Ok(SandboxResult::not_enforced(
            "Development mode: macOS sandbox skipped",
        ));
    }

    // Choose the most appropriate predefined profile
    let profile_name = if !policy.allow_network {
        PROFILE_NO_NETWORK
    } else if policy.read_write_paths.is_empty() {
        // No write paths specified - use no-write-except-temp
        PROFILE_NO_WRITE_EXCEPT_TEMP
    } else {
        // For custom policies with specific paths, we can't use predefined profiles
        // Return partially enforced with a warning
        warn!(
            "macOS sandbox: Custom path policies not fully supported via sandbox_init. \
            Using fallback mode."
        );

        // Try no-write-except-temp as a reasonable default
        PROFILE_NO_WRITE_EXCEPT_TEMP
    };

    debug!("Using predefined sandbox profile: {}", profile_name);

    // Convert to C string
    let profile_cstr =
        CString::new(profile_name).context("Failed to convert profile name to C string")?;

    // Apply sandbox
    let mut error_buf: *mut std::os::raw::c_char = std::ptr::null_mut();

    let result =
        unsafe { ffi::sandbox_init(profile_cstr.as_ptr(), ffi::SANDBOX_NAMED, &mut error_buf) };

    if result != 0 {
        // Extract error message
        let error_msg = if !error_buf.is_null() {
            let msg = unsafe { std::ffi::CStr::from_ptr(error_buf) }
                .to_string_lossy()
                .into_owned();
            unsafe { ffi::sandbox_free_error(error_buf) };
            msg
        } else {
            "Unknown sandbox error".to_string()
        };

        warn!("Seatbelt sandbox failed: {}", error_msg);

        // Return not enforced instead of erroring
        // This allows the process to continue in environments where
        // sandbox_init might fail (e.g., already sandboxed)
        return Ok(SandboxResult::not_enforced(format!(
            "macOS sandbox failed: {}",
            error_msg
        )));
    }

    info!(
        "Seatbelt sandbox applied successfully (profile: {})",
        profile_name
    );
    Ok(SandboxResult::partially_enforced(format!(
        "macOS sandbox using predefined profile: {}",
        profile_name
    )))
}

/// Generate SBPL (Sandbox Profile Language) profile from policy
/// This is kept for reference and potential future use with sandbox-exec
#[allow(dead_code)]
fn generate_sbpl_profile(policy: &SandboxPolicy) -> String {
    let mut profile = String::new();

    // Version declaration (required)
    profile.push_str("(version 1)\n");

    if policy.development_mode {
        // Development mode: more permissive
        profile.push_str("\n; Development mode - permissive sandbox\n");
        profile.push_str("(allow default)\n");

        // Only deny writes to system directories
        profile.push_str("(deny file-write*\n");
        profile.push_str("    (subpath \"/System\")\n");
        profile.push_str("    (subpath \"/usr\")\n");
        profile.push_str("    (subpath \"/bin\")\n");
        profile.push_str("    (subpath \"/sbin\")\n");
        profile.push_str(")\n");
    } else {
        // Production mode: restrictive sandbox
        profile.push_str("\n; Production mode - restrictive sandbox\n");

        // Start with deny-all, then allow specific access
        profile.push_str("(deny default)\n");

        // Always allow essential operations
        profile.push_str("\n; Essential operations\n");
        profile.push_str("(allow process-exec)\n");
        profile.push_str("(allow process-fork)\n");
        profile.push_str("(allow signal (target self))\n");
        profile.push_str("(allow sysctl-read)\n");

        // Allow mach ports for IPC (required for basic operation)
        profile.push_str("\n; IPC (required for system operation)\n");
        profile.push_str("(allow mach-lookup)\n");
        profile.push_str("(allow ipc-posix-shm)\n");

        // Network access
        if policy.allow_network {
            profile.push_str("\n; Network access\n");
            profile.push_str("(allow network-outbound)\n");
            profile.push_str("(allow network-inbound)\n");
            profile.push_str("(allow system-socket)\n");
        }

        // Read-write paths
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

        // Read-only paths
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

        // Essential system paths (always needed)
        profile.push_str("\n; Essential system paths\n");
        profile.push_str("(allow file-read*\n");
        profile.push_str("    (literal \"/\")\n");
        profile.push_str("    (literal \"/dev/null\")\n");
        profile.push_str("    (literal \"/dev/random\")\n");
        profile.push_str("    (literal \"/dev/urandom\")\n");
        profile.push_str("    (subpath \"/dev/fd\")\n");
        profile.push_str("    (subpath \"/private/var/db/dyld\")\n");
        profile.push_str(")\n");

        // Allow writes to essential locations
        profile.push_str("\n; Essential write locations\n");
        profile.push_str("(allow file-write*\n");
        profile.push_str("    (literal \"/dev/null\")\n");
        profile.push_str("    (subpath \"/dev/fd\")\n");
        profile.push_str(")\n");
    }

    profile
}

/// Escape path for use in SBPL profile
#[allow(dead_code)]
fn escape_path_for_sbpl(path: &Path) -> Option<String> {
    // Resolve symlinks (e.g., /tmp -> /private/tmp on macOS)
    let resolved = path.canonicalize().ok()?;

    let path_str = resolved.to_str()?;

    // Escape special characters for SBPL
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
        assert!(profile.contains("(allow network-outbound)"));
    }

    #[test]
    fn test_escape_path_for_sbpl() {
        // Test with existing path
        let path = PathBuf::from("/tmp");
        let escaped = escape_path_for_sbpl(&path);

        // On macOS, /tmp is symlinked to /private/tmp
        if let Some(p) = escaped {
            assert!(p.contains("tmp"));
        }
    }

    #[test]
    fn test_apply_sandbox_dev_mode() {
        let policy = SandboxPolicy::new().with_development_mode(true);

        let result = apply_seatbelt_sandbox(&policy).unwrap();

        // In dev mode, sandbox should be skipped
        assert!(!result.fully_enforced);
        assert!(result.message.contains("Development mode"));
    }
}
