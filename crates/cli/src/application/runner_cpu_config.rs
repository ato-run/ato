//! Runtime CPU entitlement configuration and manager factory (ADR-016 PR2b).
//!
//! Reads the feature flag and budget from the environment and, ONLY when
//! `enforce` is selected, runs the cgroup preflight and spawns the
//! [`CpuEntitlementManager`]. When the flag is unset or `off`, [`build`] returns
//! `None` and NOTHING happens — no cgroup discovery, no manager, no capability.
//! This is the single gate behind which the whole feature lives, so a
//! feature-off runner is byte-for-byte its previous self.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use super::runner_cgroup::{CpuCgroupBackend, LinuxCgroupV2Backend};
use super::runner_cpu_manager::CpuEntitlementManager;

/// Env var selecting the mode: `off` (default) | `enforce`.
pub const ENV_MODE: &str = "ATO_RUNNER_CPU_ENTITLEMENT";
/// Env var overriding the per-runner millicore budget.
pub const ENV_BUDGET_MILLIS: &str = "ATO_RUNNER_CPU_BUDGET_MILLIS";
/// Env var overriding the delegated cgroup root (else resolved from
/// `/proc/self/cgroup`). Mainly for tests / non-standard layouts.
pub const ENV_CGROUP_ROOT: &str = "ATO_RUNNER_CPU_CGROUP_ROOT";

/// The capability string advertised only when entitlement is live and healthy.
pub const RUNTIME_CPU_ENTITLEMENT_CAPABILITY: &str = "runtime-cpu-entitlement-v1";

/// Default per-runner budget if `ENV_BUDGET_MILLIS` is unset (8 CPU).
pub const DEFAULT_BUDGET_MILLIS: u32 = 8000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntitlementMode {
    Off,
    Enforce,
}

/// Parsed, validated entitlement configuration.
#[derive(Debug, Clone)]
pub struct CpuEntitlementConfig {
    pub mode: EntitlementMode,
    pub budget_millis: u32,
    pub cgroup_root: Option<PathBuf>,
}

impl CpuEntitlementConfig {
    /// Read config from the environment. Fails closed to `Off` on an unknown
    /// mode string, but a MALFORMED budget under `enforce` is an error (a
    /// silent fallback could hand out the wrong budget).
    pub fn from_env<F: Fn(&str) -> Option<String>>(env: F) -> Result<Self, String> {
        let mode = match env(ENV_MODE).as_deref().map(str::trim) {
            None | Some("") | Some("off") => EntitlementMode::Off,
            Some("enforce") => EntitlementMode::Enforce,
            Some(other) => {
                return Err(format!(
                    "{ENV_MODE}={other:?} is not a valid mode (off|enforce)"
                ));
            }
        };
        let budget_millis =
            match env(ENV_BUDGET_MILLIS).as_deref().map(str::trim) {
                None | Some("") => DEFAULT_BUDGET_MILLIS,
                Some(raw) => raw.parse::<u32>().ok().filter(|&b| b > 0).ok_or_else(|| {
                    format!("{ENV_BUDGET_MILLIS}={raw:?} must be a positive integer")
                })?,
            };
        let cgroup_root = env(ENV_CGROUP_ROOT)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        Ok(Self {
            mode,
            budget_millis,
            cgroup_root,
        })
    }
}

/// A live entitlement subsystem: the manager plus the capability it advertises.
pub struct CpuEntitlement {
    pub manager: CpuEntitlementManager,
    pub budget_millis: u32,
}

/// Build the entitlement subsystem from config.
///
/// * `Off` → `Ok(None)`: complete no-op.
/// * `Enforce` → resolve the delegated cgroup root, run [`CpuCgroupBackend::preflight`],
///   and on success spawn the manager. A preflight FAILURE returns `Ok(None)`
///   with the reason logged: the runner still starts and serves, it just does
///   not advertise the capability or claim leases under entitlement — never a
///   hard crash (fail-safe for an optional feature). A config PARSE error is a
///   hard error (the operator asked for something impossible).
pub fn build(config: &CpuEntitlementConfig) -> Result<Option<CpuEntitlement>, String> {
    match config.mode {
        EntitlementMode::Off => Ok(None),
        EntitlementMode::Enforce => {
            let root = match &config.cgroup_root {
                Some(explicit) => explicit.clone(),
                None => match resolve_delegated_root() {
                    Ok(r) => r,
                    Err(reason) => {
                        eprintln!(
                            "⚠️  cpu-entitlement: cannot resolve delegated cgroup root ({reason}); \
                             running WITHOUT entitlement"
                        );
                        return Ok(None);
                    }
                },
            };
            let backend: Arc<dyn CpuCgroupBackend> = Arc::new(LinuxCgroupV2Backend::new(root));
            match backend.preflight() {
                Ok(_caps) => {
                    let manager = CpuEntitlementManager::spawn(backend, config.budget_millis);
                    Ok(Some(CpuEntitlement {
                        manager,
                        budget_millis: config.budget_millis,
                    }))
                }
                Err(e) => {
                    eprintln!(
                        "⚠️  cpu-entitlement: preflight failed ({e}); running WITHOUT entitlement"
                    );
                    Ok(None)
                }
            }
        }
    }
}

/// Resolve this process's delegated cgroup v2 directory under the unified mount.
///
/// cgroup v2 lists exactly one `0::<path>` line in `/proc/self/cgroup`; the
/// delegated root is `/sys/fs/cgroup<path>`.
fn resolve_delegated_root() -> Result<PathBuf, String> {
    let content = std::fs::read_to_string("/proc/self/cgroup")
        .map_err(|e| format!("read /proc/self/cgroup: {e}"))?;
    for line in content.lines() {
        // v2 unified line: "0::/some/path".
        if let Some(rest) = line.strip_prefix("0::") {
            let mount = std::env::var("ATO_RUNNER_CGROUP_MOUNT")
                .unwrap_or_else(|_| "/sys/fs/cgroup".to_string());
            let path = rest.trim_start_matches('/');
            return Ok(PathBuf::from(mount).join(path));
        }
    }
    Err("no cgroup v2 (0::) line in /proc/self/cgroup".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    #[test]
    fn default_is_off() {
        let c = CpuEntitlementConfig::from_env(env_from(&[])).unwrap();
        assert_eq!(c.mode, EntitlementMode::Off);
        assert_eq!(c.budget_millis, DEFAULT_BUDGET_MILLIS);
    }

    #[test]
    fn explicit_off() {
        let c = CpuEntitlementConfig::from_env(env_from(&[(ENV_MODE, "off")])).unwrap();
        assert_eq!(c.mode, EntitlementMode::Off);
    }

    #[test]
    fn enforce_with_budget() {
        let c = CpuEntitlementConfig::from_env(env_from(&[
            (ENV_MODE, "enforce"),
            (ENV_BUDGET_MILLIS, "6000"),
        ]))
        .unwrap();
        assert_eq!(c.mode, EntitlementMode::Enforce);
        assert_eq!(c.budget_millis, 6000);
    }

    #[test]
    fn unknown_mode_is_error() {
        assert!(CpuEntitlementConfig::from_env(env_from(&[(ENV_MODE, "on")])).is_err());
    }

    #[test]
    fn malformed_budget_is_error() {
        assert!(
            CpuEntitlementConfig::from_env(env_from(&[
                (ENV_MODE, "enforce"),
                (ENV_BUDGET_MILLIS, "lots"),
            ]))
            .is_err()
        );
        assert!(
            CpuEntitlementConfig::from_env(env_from(&[
                (ENV_MODE, "enforce"),
                (ENV_BUDGET_MILLIS, "0"),
            ]))
            .is_err()
        );
    }

    #[test]
    fn build_off_is_none() {
        let c = CpuEntitlementConfig {
            mode: EntitlementMode::Off,
            budget_millis: 8000,
            cgroup_root: None,
        };
        assert!(build(&c).unwrap().is_none());
    }

    #[test]
    fn build_enforce_with_bad_root_is_none_not_crash() {
        // A non-existent explicit root fails preflight → Ok(None), never a panic
        // or hard error. The runner keeps serving without entitlement.
        let c = CpuEntitlementConfig {
            mode: EntitlementMode::Enforce,
            budget_millis: 8000,
            cgroup_root: Some(PathBuf::from("/nonexistent/ato-cgroup-test")),
        };
        assert!(build(&c).unwrap().is_none());
    }
}
