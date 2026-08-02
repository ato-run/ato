//! Runtime CPU entitlement configuration and manager factory (ADR-016 PR2b).
//!
//! Reads the feature flag and budget from the environment and, ONLY when
//! `enforce` is selected, runs the cgroup preflight and starts the
//! [`CpuEntitlementManager`] thread. When the flag is unset or `off`, [`build`]
//! returns [`CpuEntitlementRuntime::Off`] and NOTHING happens — no cgroup
//! discovery, no manager, no capability. This is the single gate behind which
//! the whole feature lives, so a feature-off runner is byte-for-byte its
//! previous self. `enforce` on a host that cannot deliver it FAULTS (claims
//! stop) instead of silently falling back to unthrottled execution.

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

/// The runtime state of the entitlement feature — a deliberate THREE-state
/// value, because `enforce`-with-a-broken-host must not collapse into `Off`:
///
/// * `Off` — operator did not ask for entitlement. Claims proceed as before,
///   no capability.
/// * `Active` — enforce + preflight proved delegation. Claims proceed under
///   entitlement; the capability is advertised while the manager is Healthy.
/// * `Faulted` — the operator asked for `enforce` but the host cannot deliver
///   it (unresolvable root, failed preflight, manager start failure). The
///   runner process and its heartbeat keep running so the fault is observable,
///   but WORKLOAD CLAIMS STOP and no capability is advertised. Silently
///   running unthrottled when the operator demanded enforcement would invert
///   their intent.
pub enum CpuEntitlementRuntime {
    Off,
    Active(CpuEntitlement),
    Faulted { reason: String },
}

impl CpuEntitlementRuntime {
    /// May the runner claim workload leases at all?
    pub fn claims_allowed(&self) -> bool {
        !matches!(self, CpuEntitlementRuntime::Faulted { .. })
    }

    /// Should the runner advertise `runtime-cpu-entitlement-v1` right now?
    /// (Active AND currently Healthy — health is re-read per heartbeat.)
    pub fn capability_advertised(&self) -> bool {
        match self {
            CpuEntitlementRuntime::Active(e) => matches!(
                e.manager.health(),
                super::runner_cpu_manager::CpuManagerHealth::Healthy
            ),
            _ => false,
        }
    }

    /// May the runner poll for (and thus claim) NEW workload leases right now?
    /// `Off` → yes (legacy behavior). `Active` → only while Healthy: an
    /// Unhealthy manager stops new admissions, so claiming a lease we would
    /// then refuse at pre-resume would just burn a dispatch. `Faulted` → no.
    /// Existing VMs are untouched in every case.
    pub fn polling_allowed(&self) -> bool {
        match self {
            CpuEntitlementRuntime::Off => true,
            CpuEntitlementRuntime::Active(e) => matches!(
                e.manager.health(),
                super::runner_cpu_manager::CpuManagerHealth::Healthy
            ),
            CpuEntitlementRuntime::Faulted { .. } => false,
        }
    }
}

/// Manager queue capacity for a runner with `max_slots` execution slots: room
/// for one admit + one release per slot in flight, plus diagnostics headroom.
pub fn queue_capacity_for(max_slots: usize) -> usize {
    (max_slots * 2 + 4).max(8)
}

/// Process-wide entitlement runtime, initialized ONCE at `ato runner serve`
/// startup. Other entry points (builder, one-shot CLI runs) never initialize
/// it, so [`cpu_entitlement`] returns `None` there and every consumer treats
/// that as Off — the feature exists only inside the serving runner.
static CPU_ENTITLEMENT: std::sync::OnceLock<CpuEntitlementRuntime> = std::sync::OnceLock::new();

/// Initialize the global runtime from the environment. Hard-errors only on an
/// unintelligible config; a host that cannot deliver `enforce` yields
/// `Faulted` (observable via heartbeat, claims stopped) rather than an exit.
pub fn init_cpu_entitlement(max_slots: usize) -> Result<&'static CpuEntitlementRuntime, String> {
    let config = CpuEntitlementConfig::from_env(|k| std::env::var(k).ok())?;
    Ok(CPU_ENTITLEMENT.get_or_init(|| build(&config, max_slots)))
}

/// The runtime, if [`init_cpu_entitlement`] ran in this process.
pub fn cpu_entitlement() -> Option<&'static CpuEntitlementRuntime> {
    CPU_ENTITLEMENT.get()
}

/// Build the entitlement runtime from config.
///
/// * `Off` → `CpuEntitlementRuntime::Off`: complete no-op.
/// * `Enforce` → resolve the delegated cgroup root, run
///   [`CpuCgroupBackend::preflight`], and start the manager thread. ANY failure
///   → `Faulted` (claims stop, no capability, runner keeps heartbeating) —
///   never a silent fallback to unthrottled execution, and never a crash.
///
/// A config PARSE error is a hard `Err` (the operator asked for something
/// unintelligible; guessing would be worse than stopping).
pub fn build(config: &CpuEntitlementConfig, max_slots: usize) -> CpuEntitlementRuntime {
    match config.mode {
        EntitlementMode::Off => CpuEntitlementRuntime::Off,
        EntitlementMode::Enforce => {
            let root = match &config.cgroup_root {
                Some(explicit) => explicit.clone(),
                None => match resolve_delegated_root() {
                    Ok(r) => r,
                    Err(reason) => {
                        return CpuEntitlementRuntime::Faulted {
                            reason: format!("cannot resolve delegated cgroup root: {reason}"),
                        };
                    }
                },
            };
            let backend: Arc<dyn CpuCgroupBackend> = Arc::new(LinuxCgroupV2Backend::new(root));
            if let Err(e) = backend.preflight() {
                return CpuEntitlementRuntime::Faulted {
                    reason: format!("preflight failed: {e}"),
                };
            }
            match CpuEntitlementManager::start(
                backend,
                config.budget_millis,
                queue_capacity_for(max_slots),
            ) {
                Ok(manager) => CpuEntitlementRuntime::Active(CpuEntitlement {
                    manager,
                    budget_millis: config.budget_millis,
                }),
                Err(e) => CpuEntitlementRuntime::Faulted {
                    reason: format!("manager start failed: {e}"),
                },
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
    fn build_off_is_off() {
        let c = CpuEntitlementConfig {
            mode: EntitlementMode::Off,
            budget_millis: 8000,
            cgroup_root: None,
        };
        let rt = build(&c, 4);
        assert!(matches!(rt, CpuEntitlementRuntime::Off));
        assert!(rt.claims_allowed());
        assert!(!rt.capability_advertised());
    }

    #[test]
    fn build_enforce_with_bad_root_is_faulted_not_fallback() {
        // enforce + a host that can't deliver it must FAULT (claims stop, no
        // capability), never silently run unthrottled and never panic.
        let c = CpuEntitlementConfig {
            mode: EntitlementMode::Enforce,
            budget_millis: 8000,
            cgroup_root: Some(PathBuf::from("/nonexistent/ato-cgroup-test")),
        };
        let rt = build(&c, 4);
        assert!(matches!(rt, CpuEntitlementRuntime::Faulted { .. }));
        assert!(!rt.claims_allowed());
        assert!(!rt.capability_advertised());
    }

    #[test]
    fn queue_capacity_scales_with_slots() {
        assert_eq!(queue_capacity_for(0), 8);
        assert_eq!(queue_capacity_for(2), 8);
        assert_eq!(queue_capacity_for(4), 12);
        assert_eq!(queue_capacity_for(8), 20);
    }
}
