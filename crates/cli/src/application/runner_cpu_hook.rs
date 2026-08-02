//! Per-launch pre-resume hook bridging Firecracker restore to the CPU
//! entitlement manager (ADR-016 PR2b).
//!
//! One hook is built per launch, carrying that launch's [`CpuRequest`]. The
//! snapshot backend invokes it synchronously with the final VMM host pid in the
//! spawn→`/snapshot/load` window; the hook forwards to the manager's
//! synchronous `admit_and_attach` (std-thread actor — safe from any runtime
//! context). An `Err` from here aborts the launch: the guest never resumes.

#![allow(dead_code)]

use super::runner_cpu_allocator::CpuRequest;
use super::runner_cpu_manager::CpuEntitlementManager;

/// v1 default request: `standard` class (min 1000m / max 2000m) — used when a
/// lease carries no (or an unintelligible) `runtime_cpu_request`.
pub fn standard_request(lease_id: &str, slot_index: usize) -> CpuRequest {
    CpuRequest {
        lease_id: lease_id.to_string(),
        slot_index,
        min_millis: 1000,
        max_millis: 2000,
    }
}

/// Ceiling on a lease-carried max (64 CPU) — a corrupted/hostile value can
/// never widen a request beyond any plausible host.
const LEASE_MAX_MILLIS_CEILING: u64 = 64_000;

/// Resolve the launch's [`CpuRequest`] from the lease command's server-composed
/// `runtime_cpu_request` (ato-api PR3: `ato.runtime-cpu-request/v1`
/// `{schema, class, min_millis, max_millis}`, present only when this runner
/// advertised the capability). Absent field → the standard default (a legacy
/// lease from an older API). An INVALID field (wrong schema tag, non-positive
/// or inverted bounds, absurd max) also falls back to standard — the server
/// already validated the class, so this is defense in depth that fails to the
/// DEFAULT entitlement, never to "unlimited".
pub fn request_from_lease_command(
    command: &serde_json::Value,
    lease_id: &str,
    slot_index: usize,
) -> CpuRequest {
    let fallback = standard_request(lease_id, slot_index);
    let Some(req) = command.get("runtime_cpu_request") else {
        return fallback;
    };
    let schema_ok =
        req.get("schema").and_then(|v| v.as_str()) == Some("ato.runtime-cpu-request/v1");
    let min = req.get("min_millis").and_then(serde_json::Value::as_u64);
    let max = req.get("max_millis").and_then(serde_json::Value::as_u64);
    match (schema_ok, min, max) {
        (true, Some(min), Some(max))
            if min > 0 && min <= max && max <= LEASE_MAX_MILLIS_CEILING =>
        {
            CpuRequest {
                lease_id: lease_id.to_string(),
                slot_index,
                min_millis: min as u32,
                max_millis: max as u32,
            }
        }
        _ => {
            eprintln!(
                "⚠️  cpu-entitlement: lease {lease_id}: unintelligible runtime_cpu_request; using the standard default"
            );
            fallback
        }
    }
}

/// The per-launch hook. Implements [`snapshot::PreResumeHook`]; installed on
/// the per-launch backend clone so it cannot leak across launches.
pub struct RunnerCpuPreResumeHook {
    manager: CpuEntitlementManager,
    request: CpuRequest,
}

impl RunnerCpuPreResumeHook {
    pub fn new(manager: CpuEntitlementManager, request: CpuRequest) -> Self {
        Self { manager, request }
    }
}

impl std::fmt::Debug for RunnerCpuPreResumeHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunnerCpuPreResumeHook")
            .field("lease_id", &self.request.lease_id)
            .field("slot_index", &self.request.slot_index)
            .finish()
    }
}

impl snapshot::PreResumeHook for RunnerCpuPreResumeHook {
    fn on_vmm_spawned(&self, host_pid: u32) -> Result<(), String> {
        self.manager
            .admit_and_attach(self.request.clone(), host_pid)
            .map(|admission| {
                println!(
                    "🧮 cpu-entitlement: lease {} slot {} pid {} admitted at {}m (gen {})",
                    admission.lease_id,
                    admission.slot_index,
                    host_pid,
                    admission.quota_millis,
                    admission.allocation_generation
                );
            })
            .map_err(|e| format!("cpu entitlement admission refused: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::super::runner_cgroup::FakeCgroupBackend;
    use super::*;
    use snapshot::PreResumeHook as _;
    use std::sync::Arc;

    #[test]
    fn hook_admits_through_the_manager() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 8000, 8).unwrap();
        let hook = RunnerCpuPreResumeHook::new(mgr.clone(), standard_request("lease-1", 0));
        hook.on_vmm_spawned(4242).expect("admitted");
        let snap = mgr.snapshot().unwrap();
        assert_eq!(snap.applied.len(), 1);
        assert_eq!(snap.applied[0].vmm_pid, 4242);
        assert_eq!(snap.applied[0].quota_millis, 2000);
    }

    #[test]
    fn request_parses_the_lease_wire_object() {
        let cmd = serde_json::json!({
            "kind": "restore_snapshot_preview",
            "runtime_cpu_request": {
                "schema": "ato.runtime-cpu-request/v1",
                "class": "economy",
                "min_millis": 1000,
                "max_millis": 1000
            }
        });
        let req = request_from_lease_command(&cmd, "l1", 0);
        assert_eq!((req.min_millis, req.max_millis), (1000, 1000));
    }

    #[test]
    fn request_defaults_to_standard_when_absent_or_invalid() {
        let legacy = serde_json::json!({ "kind": "restore_snapshot" });
        let req = request_from_lease_command(&legacy, "l1", 0);
        assert_eq!((req.min_millis, req.max_millis), (1000, 2000));
        for bad in [
            serde_json::json!({"runtime_cpu_request": {"schema": "other/v9", "min_millis": 1, "max_millis": 2}}),
            serde_json::json!({"runtime_cpu_request": {"schema": "ato.runtime-cpu-request/v1", "min_millis": 0, "max_millis": 2000}}),
            serde_json::json!({"runtime_cpu_request": {"schema": "ato.runtime-cpu-request/v1", "min_millis": 2000, "max_millis": 1000}}),
            serde_json::json!({"runtime_cpu_request": {"schema": "ato.runtime-cpu-request/v1", "min_millis": 1000, "max_millis": 999_999}}),
            serde_json::json!({"runtime_cpu_request": "garbage"}),
        ] {
            let req = request_from_lease_command(&bad, "l1", 0);
            assert_eq!(
                (req.min_millis, req.max_millis),
                (1000, 2000),
                "bad shape must fall back: {bad}"
            );
        }
    }

    #[test]
    fn hook_refusal_is_an_err_that_aborts_launch() {
        let be = Arc::new(FakeCgroupBackend::new());
        // Budget below the floor: every admission must be refused.
        let mgr = CpuEntitlementManager::start(be.clone(), 500, 8).unwrap();
        let hook = RunnerCpuPreResumeHook::new(mgr, standard_request("lease-1", 0));
        assert!(hook.on_vmm_spawned(4242).is_err());
    }
}
