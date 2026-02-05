//! v0.2.0 Phase 3: OS-native Sandbox (Isolation Layer)
//!
//! Provides OS-native process sandboxing to restrict file system access
//! for child processes spawned by the Supervisor.
//!
//! ## Supported Platforms
//! - **Linux**: Landlock LSM (Linux 5.13+)
//! - **macOS**: Seatbelt/sandbox-exec (SBPL profiles)
//!
//! ## Architecture
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                      Sandbox Module                              │
//! ├─────────────────────────────────────────────────────────────────┤
//! │  apply_sandbox(policy: &SandboxPolicy) -> Result<()>            │
//! │                                                                  │
//! │  ┌─────────────────────┐    ┌─────────────────────┐             │
//! │  │   Linux (Landlock)  │    │   macOS (Seatbelt)  │             │
//! │  │   - Ruleset based   │    │   - SBPL profile    │             │
//! │  │   - Kernel 5.13+    │    │   - sandbox-init    │             │
//! │  └─────────────────────┘    └─────────────────────┘             │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Usage
//! ```ignore
//! use nacelle::system::sandbox::{SandboxPolicy, apply_sandbox};
//!
//! let policy = SandboxPolicy::default()
//!     .allow_read_write(&["/app", "/tmp"])
//!     .allow_read_only(&["/usr", "/lib"]);
//!
//! // Apply in pre_exec hook (before exec)
//! apply_sandbox(&policy)?;
//! ```

use anyhow::Result;
use std::path::PathBuf;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "macos")]
pub mod macos;

// ═══════════════════════════════════════════════════════════════════════════
// Sandbox Policy Configuration
// ═══════════════════════════════════════════════════════════════════════════

/// Sandbox policy configuration
///
/// Defines which paths are allowed for read-only or read-write access.
/// All other paths are denied write access by default.
#[derive(Debug, Clone, Default)]
pub struct SandboxPolicy {
    /// Paths allowed for read-write access (app directories, /tmp, etc.)
    pub read_write_paths: Vec<PathBuf>,
    /// Paths allowed for read-only access (system libraries, /usr, etc.)
    pub read_only_paths: Vec<PathBuf>,
    /// Whether to enable network access (default: true for now)
    pub allow_network: bool,
    /// Whether this sandbox is in "development mode" (more permissive)
    pub development_mode: bool,
}

impl SandboxPolicy {
    /// Create a new sandbox policy
    pub fn new() -> Self {
        Self {
            read_write_paths: Vec::new(),
            read_only_paths: Vec::new(),
            allow_network: true,
            development_mode: false,
        }
    }

    /// Add paths for read-write access
    pub fn allow_read_write<P: Into<PathBuf>>(
        mut self,
        paths: impl IntoIterator<Item = P>,
    ) -> Self {
        self.read_write_paths
            .extend(paths.into_iter().map(|p| p.into()));
        self
    }

    /// Add paths for read-only access
    pub fn allow_read_only<P: Into<PathBuf>>(mut self, paths: impl IntoIterator<Item = P>) -> Self {
        self.read_only_paths
            .extend(paths.into_iter().map(|p| p.into()));
        self
    }

    /// Enable/disable network access
    pub fn with_network(mut self, enabled: bool) -> Self {
        self.allow_network = enabled;
        self
    }

    /// Enable development mode (more permissive)
    pub fn with_development_mode(mut self, enabled: bool) -> Self {
        self.development_mode = enabled;
        self
    }

    /// Create a default policy for capsule applications
    ///
    /// This policy:
    /// - Allows read-write to app directory and /tmp
    /// - Allows read-only to system libraries (/usr, /lib, /etc)
    /// - Enables network access
    pub fn for_capsule(app_dir: impl Into<PathBuf>) -> Self {
        let app_dir = app_dir.into();

        Self::new()
            .allow_read_write([
                app_dir,
                PathBuf::from("/tmp"),
                PathBuf::from("/private/tmp"), // macOS
                PathBuf::from("/var/tmp"),
            ])
            .allow_read_only([
                PathBuf::from("/usr"),
                PathBuf::from("/lib"),
                PathBuf::from("/lib64"),
                PathBuf::from("/etc"),
                PathBuf::from("/dev"),
                PathBuf::from("/proc"),
                PathBuf::from("/sys"),
                // macOS specific
                PathBuf::from("/System"),
                PathBuf::from("/Library"),
                PathBuf::from("/bin"),
                PathBuf::from("/sbin"),
                PathBuf::from("/private/var/db"),
            ])
            .with_network(true)
    }

    /// Create a policy from IsolationPolicy (from capsule.toml)
    ///
    /// This converts the manifest-level isolation configuration
    /// into a concrete SandboxPolicy for enforcement.
    pub fn from_isolation_policy(
        isolation: &crate::launcher::IsolationPolicy,
        dev_mode: bool,
    ) -> Self {
        Self {
            read_write_paths: isolation.read_write_paths.clone(),
            read_only_paths: isolation.read_only_paths.clone(),
            allow_network: isolation.network_enabled,
            development_mode: dev_mode || !isolation.sandbox_enabled,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Platform-specific Sandbox Application
// ═══════════════════════════════════════════════════════════════════════════

/// Result of sandbox application
#[derive(Debug, Clone)]
pub struct SandboxResult {
    /// Whether sandbox was fully enforced
    pub fully_enforced: bool,
    /// Whether sandbox was partially enforced (some rules couldn't be applied)
    pub partially_enforced: bool,
    /// Human-readable status message
    pub message: String,
}

impl SandboxResult {
    /// Create a fully enforced result
    pub fn fully_enforced() -> Self {
        Self {
            fully_enforced: true,
            partially_enforced: false,
            message: "Sandbox fully enforced".to_string(),
        }
    }

    /// Create a partially enforced result
    pub fn partially_enforced(reason: impl Into<String>) -> Self {
        Self {
            fully_enforced: false,
            partially_enforced: true,
            message: reason.into(),
        }
    }

    /// Create a not enforced result (platform doesn't support sandbox)
    pub fn not_enforced(reason: impl Into<String>) -> Self {
        Self {
            fully_enforced: false,
            partially_enforced: false,
            message: reason.into(),
        }
    }
}

/// Apply sandbox restrictions to the current process
///
/// This function should be called in the child process after fork()
/// but before exec(), typically in a `pre_exec` hook.
///
/// # Platform Behavior
/// - **Linux**: Uses Landlock LSM (requires kernel 5.13+)
/// - **macOS**: Uses Seatbelt/sandbox-exec via sandbox_init()
/// - **Other**: Returns Ok with not_enforced status
///
/// # Safety
/// This function must be called in a pre_exec context on Unix.
/// It will fail if called from a multi-threaded context on some platforms.
#[cfg(target_os = "linux")]
pub fn apply_sandbox(policy: &SandboxPolicy) -> Result<SandboxResult> {
    linux::apply_landlock_sandbox(policy)
}

#[cfg(target_os = "macos")]
pub fn apply_sandbox(policy: &SandboxPolicy) -> Result<SandboxResult> {
    macos::apply_seatbelt_sandbox(policy)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn apply_sandbox(_policy: &SandboxPolicy) -> Result<SandboxResult> {
    Ok(SandboxResult::not_enforced(
        "Sandboxing not supported on this platform",
    ))
}

/// Check if the current platform supports sandboxing
pub fn is_sandbox_supported() -> bool {
    #[cfg(target_os = "linux")]
    {
        linux::is_landlock_supported()
    }
    #[cfg(target_os = "macos")]
    {
        true // macOS always has sandbox-exec
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        false
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandbox_policy_builder() {
        let policy = SandboxPolicy::new()
            .allow_read_write([PathBuf::from("/app")])
            .allow_read_only([PathBuf::from("/usr")])
            .with_network(true);

        assert_eq!(policy.read_write_paths.len(), 1);
        assert_eq!(policy.read_only_paths.len(), 1);
        assert!(policy.allow_network);
    }

    #[test]
    fn test_capsule_policy() {
        let policy = SandboxPolicy::for_capsule("/my/app");

        assert!(policy.read_write_paths.contains(&PathBuf::from("/my/app")));
        assert!(policy.read_write_paths.contains(&PathBuf::from("/tmp")));
        assert!(policy.read_only_paths.contains(&PathBuf::from("/usr")));
        assert!(policy.allow_network);
    }
}
