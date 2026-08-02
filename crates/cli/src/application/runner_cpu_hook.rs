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

/// v1 default request: `standard` class (min 1000m / max 2000m). Until PR3
/// carries a server-resolved `ato.runtime-cpu-request/v1` in the lease command,
/// every entitled launch is standard-class — matching the ADR's v1 default.
pub fn standard_request(lease_id: &str, slot_index: usize) -> CpuRequest {
    CpuRequest {
        lease_id: lease_id.to_string(),
        slot_index,
        min_millis: 1000,
        max_millis: 2000,
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
    fn hook_refusal_is_an_err_that_aborts_launch() {
        let be = Arc::new(FakeCgroupBackend::new());
        // Budget below the floor: every admission must be refused.
        let mgr = CpuEntitlementManager::start(be.clone(), 500, 8).unwrap();
        let hook = RunnerCpuPreResumeHook::new(mgr, standard_request("lease-1", 0));
        assert!(hook.on_vmm_spawned(4242).is_err());
    }
}
