//! OS-native process sandboxing: Landlock on Linux, Seatbelt on macOS.
//!
//! ## Provenance, and why this is not a fork
//!
//! Taken from `extensions/providers/nacelle/src/system/sandbox/` as DONOR
//! CODE, part by part, which is the standing rule for reusing an older
//! implementation.
//!
//! Extracting rather than depending on nacelle was forced, not preferred:
//! nacelle is excluded from the workspace and does not build in this tree at
//! all — its `ato-semantics-workspace` dependency no longer exists. Editing a
//! crate that cannot be compiled or tested would mean shipping an unverifiable
//! change and could silently break it for whoever revives it, so nacelle is
//! left exactly as it is.
//!
//! The consequence is real and should not be glossed: two copies of this
//! policy exist. De-duplicating them is blocked on nacelle building again, and
//! until then this copy is the one the Connected Runtime Worker uses.
//!
//! What deliberately did NOT come across is nacelle's language and runtime
//! inference. A sandbox decides what a process may touch; deciding what to run
//! belongs to `RuntimeLaunchSpecV1`, and mixing the two is how a sandbox ends
//! up with opinions about Python.
//!
//! Originally: v0.2.0 Phase 3 (Isolation Layer).
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
//! use ato_sandbox::{SandboxPolicy, apply_sandbox};
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
/// A candidate overlaps when it **is**, **contains**, or **is contained by**
/// a sensitive path (same bidirectional check as [`is_sensitive_path`]).
///
/// Returns `(clean, removed)`:
/// - `clean`: paths that are safe to include in an allow-list.
/// - `removed`: paths that were dropped because they overlap with sensitive dirs.
pub fn filter_sensitive_paths(paths: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let sensitive = sensitive_paths();
    let mut clean = Vec::new();
    let mut removed = Vec::new();

    for p in paths {
        let overlapping = sensitive.iter().any(|sp| {
            // The candidate is a sensitive path itself or sits inside one
            // (e.g. ~/.aws/credentials) – allowing it would expose secrets
            // directly.
            p.starts_with(sp)
                // The candidate is an ancestor of a sensitive dir – allowing
                // it would implicitly grant access to secrets.
                || sp.starts_with(p)
        });
        if overlapping {
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
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
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
                PathBuf::from("/opt"),
            ])
            .with_network(true)
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

/// Set `PR_SET_NO_NEW_PRIVS` on the calling process.
///
/// Landlock requires either `CAP_SYS_ADMIN` or this flag, and the flag is
/// inherited across `exec` — which is what makes it usable from a shim that
/// restricts itself and then execs the workload.
///
/// It lives here rather than in the caller because the caller is the Connected
/// Runtime Worker, which denies `unsafe_code` crate-wide. An OS primitive
/// belongs with the other OS primitives, not as an exception carved into a
/// crate that has deliberately forbidden them.
#[cfg(target_os = "linux")]
pub fn set_no_new_privs() -> std::io::Result<()> {
    // SAFETY: `prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)` takes scalar arguments,
    // reads and writes no caller memory, and cannot fail in a way that leaves
    // the process in an inconsistent state.
    let result = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// No-op off Linux, where the flag does not exist and Landlock is not the
/// enforcement mechanism.
#[cfg(not(target_os = "linux"))]
pub fn set_no_new_privs() -> std::io::Result<()> {
    Ok(())
}

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
    fn test_filter_sensitive_paths_removes_exact_match() {
        if let Some(home) = dirs::home_dir() {
            // A path that IS a sensitive dir must be dropped (issue #642).
            let ssh = home.join(".ssh");
            let input = vec![ssh.clone(), PathBuf::from("/tmp")];
            let (clean, removed) = filter_sensitive_paths(&input);
            assert!(
                removed.contains(&ssh),
                "~/.ssh itself should be removed from the allow-list"
            );
            assert!(clean.contains(&PathBuf::from("/tmp")));
        }
    }

    #[test]
    fn test_filter_sensitive_paths_removes_child_of_sensitive_dir() {
        if let Some(home) = dirs::home_dir() {
            // A file inside a sensitive dir must be dropped (issue #642).
            let creds = home.join(".aws/credentials");
            let input = vec![creds.clone(), PathBuf::from("/var/data")];
            let (clean, removed) = filter_sensitive_paths(&input);
            assert!(
                removed.contains(&creds),
                "~/.aws/credentials should be removed from the allow-list"
            );
            assert!(clean.contains(&PathBuf::from("/var/data")));
        }
    }

    #[test]
    fn test_filter_sensitive_paths_keeps_safe() {
        let input = vec![PathBuf::from("/tmp"), PathBuf::from("/var/data")];
        let (clean, removed) = filter_sensitive_paths(&input);
        assert!(removed.is_empty());
        assert_eq!(clean.len(), 2);
    }
}
