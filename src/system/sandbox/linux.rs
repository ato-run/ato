//! Linux Landlock LSM Sandbox Implementation
//!
//! Implements process sandboxing using Linux Landlock (available since kernel 5.13).
//! Landlock allows unprivileged processes to restrict their own filesystem access.
//!
//! ## Features
//! - File system access control (read, write, execute)
//! - No root privileges required
//! - Graceful degradation on older kernels
//!
//! ## References
//! - https://landlock.io/
//! - https://docs.rs/landlock/

use super::{SandboxPolicy, SandboxResult};
use anyhow::{Context, Result};
use landlock::{
    path_beneath_rules, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
    ABI,
};
use std::path::Path;
use tracing::{debug, info, warn};

/// Check if Landlock is supported on this system
pub fn is_landlock_supported() -> bool {
    // Try to create a minimal ruleset to check support
    match Ruleset::default().handle_access(AccessFs::from_all(ABI::V1)) {
        Ok(_) => true,
        Err(_) => false,
    }
}

/// Apply Landlock sandbox to the current process
///
/// This function should be called in a pre_exec hook before executing
/// the child process. It restricts file system access according to the policy.
///
/// # Arguments
/// * `policy` - Sandbox policy defining allowed paths
///
/// # Returns
/// * `Ok(SandboxResult)` - Sandbox applied (fully or partially)
/// * `Err` - Failed to apply sandbox
pub fn apply_landlock_sandbox(policy: &SandboxPolicy) -> Result<SandboxResult> {
    // Use ABI V3 for best compatibility with modern kernels
    // Falls back gracefully on older kernels
    let abi = ABI::V3;

    debug!("Applying Landlock sandbox with ABI {:?}", abi);

    // Create ruleset handling all file system access rights
    let ruleset = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .context("Failed to create Landlock ruleset")?;

    // Create the ruleset
    let mut created_ruleset = ruleset
        .create()
        .context("Failed to create Landlock ruleset")?;

    // Add read-write paths
    for path in &policy.read_write_paths {
        if path.exists() {
            debug!("Adding read-write access to: {:?}", path);
            created_ruleset = add_path_rules(created_ruleset, path, AccessFs::from_all(abi))
                .with_context(|| format!("Failed to add read-write rule for {:?}", path))?;
        } else {
            debug!("Skipping non-existent read-write path: {:?}", path);
        }
    }

    // Add read-only paths
    for path in &policy.read_only_paths {
        if path.exists() {
            debug!("Adding read-only access to: {:?}", path);
            created_ruleset = add_path_rules(created_ruleset, path, AccessFs::from_read(abi))
                .with_context(|| format!("Failed to add read-only rule for {:?}", path))?;
        } else {
            debug!("Skipping non-existent read-only path: {:?}", path);
        }
    }

    // Add IPC socket paths (injected by ato-cli IPC Broker)
    for path in &policy.ipc_socket_paths {
        if path.exists() || path.parent().map_or(false, |p| p.exists()) {
            debug!("Adding IPC socket read-write access to: {:?}", path);
            // IPC sockets need full read-write access
            let target = if path.exists() {
                path.as_path()
            } else {
                path.parent().unwrap()
            };
            created_ruleset = add_path_rules(created_ruleset, target, AccessFs::from_all(abi))
                .with_context(|| format!("Failed to add IPC socket rule for {:?}", path))?;
        } else {
            debug!("Skipping non-existent IPC socket path: {:?}", path);
        }
    }

    // Restrict the current thread
    let status = created_ruleset
        .restrict_self()
        .context("Failed to restrict process with Landlock")?;

    // Report status
    match status.ruleset {
        RulesetStatus::FullyEnforced => {
            info!("Landlock sandbox fully enforced");
            Ok(SandboxResult::fully_enforced())
        }
        RulesetStatus::PartiallyEnforced => {
            let msg = "Landlock sandbox partially enforced (some rules unavailable on this kernel)";
            warn!("{}", msg);
            Ok(SandboxResult::partially_enforced(msg))
        }
        RulesetStatus::NotEnforced => {
            let msg = "Landlock sandbox not enforced (kernel too old or Landlock disabled)";
            warn!("{}", msg);
            Ok(SandboxResult::not_enforced(msg))
        }
    }
}

/// Add path rules to the ruleset
fn add_path_rules(
    ruleset: landlock::RulesetCreated,
    path: &Path,
    access: impl Into<landlock::BitFlags<AccessFs>>,
) -> Result<landlock::RulesetCreated> {
    let access = access.into();

    // Use path_beneath_rules helper for easy rule creation
    let paths = [path];
    let rules = path_beneath_rules(&paths, access);

    let mut ruleset = ruleset;

    for rule_result in rules {
        match rule_result {
            Ok(rule) => {
                ruleset = ruleset.add_rule(rule)?;
            }
            Err(e) => {
                // Log but don't fail - path might not be accessible
                debug!("Skipping rule for {:?}: {}", path, e);
            }
        }
    }

    Ok(ruleset)
}

/// Create a minimal sandbox for testing
///
/// This is useful for verifying Landlock is working without
/// being too restrictive.
#[allow(dead_code)]
pub fn apply_minimal_sandbox() -> Result<SandboxResult> {
    let policy = SandboxPolicy::new()
        .allow_read_write([
            std::env::current_dir().unwrap_or_default(),
            std::path::PathBuf::from("/tmp"),
        ])
        .allow_read_only([
            std::path::PathBuf::from("/usr"),
            std::path::PathBuf::from("/lib"),
            std::path::PathBuf::from("/lib64"),
            std::path::PathBuf::from("/etc"),
            std::path::PathBuf::from("/dev"),
            std::path::PathBuf::from("/proc"),
            std::path::PathBuf::from("/sys"),
        ]);

    apply_landlock_sandbox(&policy)
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_landlock_support_check() {
        // This test just checks if the support check doesn't panic
        let supported = is_landlock_supported();
        println!("Landlock supported: {}", supported);
    }

    // Note: Full sandbox tests should only run on Linux with Landlock support
    // and in a controlled environment (e.g., in a forked process)
}
