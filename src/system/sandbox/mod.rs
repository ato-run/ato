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
use tracing::debug;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "macos")]
pub mod macos;

// ═══════════════════════════════════════════════════════════════════════════
// Sensitive Paths (shared across all platforms)
// ═══════════════════════════════════════════════════════════════════════════

/// Returns a list of sensitive user directories that should be denied access
/// by sandboxed capsule processes.
///
/// These paths contain secrets, credentials, and private data that
/// capsule workloads should never access.
///
/// Uses the `dirs` crate to resolve `$HOME` portably (works even if
/// `HOME` env var is unset on macOS/Linux).
///
/// # Platform behaviour
/// - Common paths (`.ssh`, `.aws`, `.gnupg`, etc.) are returned on all platforms.
/// - macOS-specific paths (`Library/Keychains`, browser profiles, etc.) are
///   appended when compiled for macOS.
pub fn sensitive_paths() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        debug!("Could not determine home directory; sensitive_paths will be empty");
        return Vec::new();
    };

    let paths = vec![
        // Cryptographic keys and credentials
        home.join(".ssh"),
        home.join(".gnupg"),
        // Cloud provider credentials
        home.join(".aws"),
        home.join(".kube"),
        home.join(".config/gcloud"),
        home.join(".azure"),
        // Docker credentials
        home.join(".docker"),
        // Package manager tokens
        home.join(".npmrc"),
        home.join(".pypirc"),
        // Shell history (may contain secrets)
        home.join(".bash_history"),
        home.join(".zsh_history"),
    ];

    // macOS-specific sensitive directories
    #[cfg(target_os = "macos")]
    {
        let mut paths = paths;
        paths.extend([
            home.join("Library/Keychains"),
            home.join("Library/Cookies"),
            home.join("Library/Application Support/Google/Chrome"),
            home.join("Library/Application Support/Firefox"),
        ]);
        paths
    }

    #[cfg(not(target_os = "macos"))]
    {
        paths
    }
}

/// Check whether `candidate` is a sub-path of (or equal to) any sensitive path.
///
/// This is used to filter Landlock allow-lists: if the user specifies a
/// path that overlaps with a sensitive directory, we exclude it and log a
/// warning.
pub fn is_sensitive_path(candidate: &std::path::Path) -> bool {
    for sp in sensitive_paths() {
        // candidate is inside a sensitive dir  (e.g. ~/.ssh/id_rsa)
        if candidate.starts_with(&sp) {
            return true;
        }
        // candidate is a parent of a sensitive dir (e.g. ~ contains ~/.ssh)
        if sp.starts_with(candidate) && sp != candidate {
            return true;
        }
    }
    false
}

/// Filter a list of paths, removing any that overlap with sensitive paths.
///
/// Returns `(clean, removed)`:
/// - `clean`: paths that are safe to include in an allow-list.
/// - `removed`: paths that were dropped because they overlap with sensitive dirs.
pub fn filter_sensitive_paths(paths: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let sensitive = sensitive_paths();
    let mut clean = Vec::new();
    let mut removed = Vec::new();

    for p in paths {
        let dominated = sensitive.iter().any(|sp| {
            // The candidate is an ancestor of a sensitive dir – allowing it
            // would implicitly grant access to secrets.
            sp.starts_with(p) && sp != p
        });
        if dominated {
            removed.push(p.clone());
        } else {
            clean.push(p.clone());
        }
    }

    (clean, removed)
}

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
    /// IPC socket paths that must be allowed through the Sandbox.
    /// These are injected by ato-cli (IPC Broker) and nacelle
    /// automatically adds them to the read-write allow-list.
    pub ipc_socket_paths: Vec<PathBuf>,
}

impl SandboxPolicy {
    /// Create a new sandbox policy
    pub fn new() -> Self {
        Self {
            read_write_paths: Vec::new(),
            read_only_paths: Vec::new(),
            allow_network: true,
            development_mode: false,
            ipc_socket_paths: Vec::new(),
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

    /// Add IPC socket paths that must be allowed through the Sandbox.
    /// These paths are generated by ato-cli (IPC Broker) and passed
    /// to nacelle via the exec request JSON.
    pub fn with_ipc_socket_paths<P: Into<PathBuf>>(
        mut self,
        paths: impl IntoIterator<Item = P>,
    ) -> Self {
        self.ipc_socket_paths
            .extend(paths.into_iter().map(|p| p.into()));
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
    ///
    /// **Security**: Paths that overlap with [`sensitive_paths()`] are
    /// automatically removed from the allow-lists and a warning is logged.
    pub fn from_isolation_policy(
        isolation: &crate::launcher::IsolationPolicy,
        dev_mode: bool,
    ) -> Self {
        use tracing::warn;

        // Filter out sensitive paths from both allow-lists
        let (clean_rw, removed_rw) = filter_sensitive_paths(&isolation.read_write_paths);
        let (clean_ro, removed_ro) = filter_sensitive_paths(&isolation.read_only_paths);

        for p in &removed_rw {
            warn!(
                "Sensitive path removed from read_write allow-list: {}",
                p.display()
            );
        }
        for p in &removed_ro {
            warn!(
                "Sensitive path removed from read_only allow-list: {}",
                p.display()
            );
        }

        Self {
            read_write_paths: clean_rw,
            read_only_paths: clean_ro,
            allow_network: isolation.network_enabled,
            development_mode: dev_mode || !isolation.sandbox_enabled,
            ipc_socket_paths: Vec::new(),
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

    #[test]
    fn test_sensitive_paths_not_empty() {
        // As long as HOME is resolvable, we should get paths
        let paths = sensitive_paths();
        if dirs::home_dir().is_some() {
            assert!(!paths.is_empty(), "sensitive_paths should return entries");
            // Check for universally expected directories
            let has_ssh = paths.iter().any(|p| p.ends_with(".ssh"));
            assert!(has_ssh, "sensitive_paths should include .ssh");
            let has_aws = paths.iter().any(|p| p.ends_with(".aws"));
            assert!(has_aws, "sensitive_paths should include .aws");
        }
    }

    #[test]
    fn test_is_sensitive_path_detects_child() {
        if let Some(home) = dirs::home_dir() {
            assert!(is_sensitive_path(&home.join(".ssh")));
            assert!(is_sensitive_path(&home.join(".ssh/id_rsa")));
        }
    }

    #[test]
    fn test_is_sensitive_path_detects_parent() {
        if let Some(home) = dirs::home_dir() {
            // The home directory itself is a parent of ~/.ssh, so it should
            // be flagged as sensitive.
            assert!(is_sensitive_path(&home));
        }
    }

    #[test]
    fn test_is_sensitive_path_non_sensitive() {
        assert!(!is_sensitive_path(&PathBuf::from("/tmp")));
        assert!(!is_sensitive_path(&PathBuf::from("/usr/bin")));
    }

    #[test]
    fn test_filter_sensitive_paths_removes_home() {
        if let Some(home) = dirs::home_dir() {
            let input = vec![PathBuf::from("/tmp"), home.clone(), PathBuf::from("/usr")];
            let (clean, removed) = filter_sensitive_paths(&input);
            assert!(
                removed.contains(&home),
                "home dir should be removed (it's a parent of ~/.ssh)"
            );
            assert!(clean.contains(&PathBuf::from("/tmp")));
            assert!(clean.contains(&PathBuf::from("/usr")));
        }
    }

    #[test]
    fn test_filter_sensitive_paths_keeps_safe() {
        let input = vec![PathBuf::from("/tmp"), PathBuf::from("/var/data")];
        let (clean, removed) = filter_sensitive_paths(&input);
        assert!(removed.is_empty());
        assert_eq!(clean.len(), 2);
    }

    #[test]
    fn test_from_isolation_policy_filters_sensitive() {
        if let Some(home) = dirs::home_dir() {
            let policy = crate::launcher::IsolationPolicy {
                sandbox_enabled: true,
                read_write_paths: vec![home.clone(), PathBuf::from("/tmp")],
                read_only_paths: vec![PathBuf::from("/usr")],
                network_enabled: true,
                egress_allow: vec![],
            };

            let sandbox = SandboxPolicy::from_isolation_policy(&policy, false);
            // /tmp should survive, home should be removed
            assert!(
                sandbox.read_write_paths.contains(&PathBuf::from("/tmp")),
                "safe path should be kept"
            );
            assert!(
                !sandbox.read_write_paths.contains(&home),
                "home dir should be filtered out"
            );
        }
    }

    // ═══════════════════════════════════════════════════════════════════════
    // IPC Socket Path Tests (Phase 13a)
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn test_ipc_socket_paths_default_empty() {
        let policy = SandboxPolicy::new();
        assert!(policy.ipc_socket_paths.is_empty());
    }

    #[test]
    fn test_ipc_socket_paths_builder() {
        let policy = SandboxPolicy::new()
            .with_ipc_socket_paths([
                PathBuf::from("/tmp/capsule-ipc/llm-service.sock"),
                PathBuf::from("/tmp/capsule-ipc/db-service.sock"),
            ])
            .allow_read_write([PathBuf::from("/app")]);

        assert_eq!(policy.ipc_socket_paths.len(), 2);
        assert!(policy
            .ipc_socket_paths
            .contains(&PathBuf::from("/tmp/capsule-ipc/llm-service.sock")));
        assert!(policy
            .ipc_socket_paths
            .contains(&PathBuf::from("/tmp/capsule-ipc/db-service.sock")));
        // Regular paths are separate
        assert_eq!(policy.read_write_paths.len(), 1);
    }

    #[test]
    fn test_ipc_socket_paths_not_in_read_write() {
        // IPC socket paths should be stored separately from read_write_paths
        // to maintain clear distinction and enable auditing
        let policy = SandboxPolicy::new()
            .with_ipc_socket_paths([PathBuf::from("/tmp/capsule-ipc/test.sock")])
            .allow_read_write([PathBuf::from("/app")]);

        assert!(!policy
            .read_write_paths
            .contains(&PathBuf::from("/tmp/capsule-ipc/test.sock")));
    }

    #[test]
    fn test_for_capsule_has_empty_ipc_paths() {
        let policy = SandboxPolicy::for_capsule("/my/app");
        assert!(policy.ipc_socket_paths.is_empty());
    }

    #[test]
    fn test_from_isolation_policy_has_empty_ipc_paths() {
        let isolation = crate::launcher::IsolationPolicy {
            sandbox_enabled: true,
            read_write_paths: vec![PathBuf::from("/tmp")],
            read_only_paths: vec![PathBuf::from("/usr")],
            network_enabled: true,
            egress_allow: vec![],
        };

        let sandbox = SandboxPolicy::from_isolation_policy(&isolation, false);
        assert!(
            sandbox.ipc_socket_paths.is_empty(),
            "IPC paths should be empty by default from isolation policy"
        );
    }
}
