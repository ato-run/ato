//! Single-owner CPU entitlement manager (ADR-016 PR2).
//!
//! Detached lease tasks never touch the shared allocation directly; they send
//! commands to this one actor, which owns the [`CpuCgroupBackend`] and the
//! current per-slot allocation. It computes a new allocation with
//! [`allocate_cpu`](super::runner_cpu_allocator::allocate_cpu) on every
//! admit/release, applies it as `cpu.max` writes in a budget-safe order
//! (every decrease before any increase, so the sum never transiently exceeds
//! the runner budget), and attaches the VMM pid to its slot cgroup BEFORE the
//! guest is resumed.
//!
//! Fail-closed throughout:
//!   * a request whose floor cannot be met (or any cgroup write that fails) is
//!     rolled back to the previous applied allocation and the new launch is
//!     refused — no guest starts;
//!   * if the rollback ITSELF fails the manager goes [`Unhealthy`], which stops
//!     new admissions and (via the caller) drops the entitlement capability at
//!     the next heartbeat; already-running VMs keep their last-applied quota;
//!   * a slot/CPU reservation is released only on `ReleaseAfterTeardown`, which
//!     the caller sends only after VM teardown is confirmed.
//!
//! [`Unhealthy`]: CpuManagerHealth::Unhealthy

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use super::runner_cgroup::{CgroupError, CpuCgroupBackend};
use super::runner_cpu_allocator::{CpuAllocationError, CpuRequest, allocate_cpu};

/// An entitlement currently applied to a slot cgroup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEntitlement {
    pub slot_index: usize,
    pub request: CpuRequest,
    pub quota_millis: u32,
}

/// Manager liveness. `Unhealthy` is terminal for admissions until the process
/// restarts — it means a rollback failed and the cgroup tree may not match the
/// recorded allocation, so admitting more would compound the drift.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CpuManagerHealth {
    Healthy,
    Unhealthy { reason: String },
}

/// Why an admit/release failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CpuManagerError {
    #[error("CPU allocation rejected: {0}")]
    Allocation(#[from] CpuAllocationError),
    #[error("cgroup operation failed (rolled back): {0}")]
    CgroupRolledBack(CgroupError),
    #[error("manager is unhealthy: {reason}")]
    Unhealthy { reason: String },
    #[error("no entitlement registered for lease {lease_id}")]
    UnknownLease { lease_id: String },
}

/// Outcome of a successful admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuAdmission {
    pub lease_id: String,
    pub slot_index: usize,
    pub quota_millis: u32,
    pub allocation_generation: u64,
}

/// A read-only view of manager state for diagnostics / tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuManagerSnapshot {
    pub health: CpuManagerHealth,
    pub applied: Vec<AppliedEntitlement>,
    pub allocation_generation: u64,
    pub budget_millis: u32,
}

/// Proof that a VM teardown was confirmed. Constructable only inside this crate
/// (its single field is private), so `ReleaseAfterTeardown` cannot be issued
/// from an arbitrary call site that has not actually observed teardown — the
/// type is the permission.
#[derive(Debug, Clone, Copy)]
pub struct TeardownConfirmed {
    _private: (),
}

impl TeardownConfirmed {
    /// Mint the proof. Call ONLY at the runner's confirmed-teardown seam
    /// (`StopCleanup` with `slot_released`), never speculatively.
    pub(crate) fn assert() -> Self {
        Self { _private: () }
    }
}

enum Command {
    AdmitAndAttach {
        request: CpuRequest,
        vmm_pid: u32,
        reply: oneshot::Sender<Result<CpuAdmission, CpuManagerError>>,
    },
    ReleaseAfterTeardown {
        lease_id: String,
        _proof: TeardownConfirmed,
        reply: oneshot::Sender<Result<(), CpuManagerError>>,
    },
    Snapshot {
        reply: oneshot::Sender<CpuManagerSnapshot>,
    },
}

/// Handle to the manager actor. Cloneable; every clone talks to the one owner.
#[derive(Clone)]
pub struct CpuEntitlementManager {
    tx: mpsc::Sender<Command>,
}

impl CpuEntitlementManager {
    /// Spawn the owner actor over `backend` with `budget_millis`. The returned
    /// handle is cheap to clone across detached lease tasks.
    pub fn spawn(backend: Arc<dyn CpuCgroupBackend>, budget_millis: u32) -> Self {
        let (tx, rx) = mpsc::channel(64);
        let actor = ManagerActor {
            backend,
            budget_millis,
            health: CpuManagerHealth::Healthy,
            applied: BTreeMap::new(),
            allocation_generation: 0,
        };
        tokio::spawn(actor.run(rx));
        Self { tx }
    }

    /// Admit a new session: compute the new allocation including it, apply the
    /// quotas (decrease-before-increase), attach `vmm_pid` to the slot cgroup,
    /// and commit. On any failure the previous allocation is restored and this
    /// returns an error WITHOUT the pid attached — the caller must not resume
    /// the guest.
    pub async fn admit_and_attach(
        &self,
        request: CpuRequest,
        vmm_pid: u32,
    ) -> Result<CpuAdmission, CpuManagerError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::AdmitAndAttach {
                request,
                vmm_pid,
                reply,
            })
            .await
            .map_err(|_| CpuManagerError::Unhealthy {
                reason: "manager actor stopped".to_string(),
            })?;
        rx.await.map_err(|_| CpuManagerError::Unhealthy {
            reason: "manager actor dropped reply".to_string(),
        })?
    }

    /// Release a session AFTER its VM teardown is confirmed, then rebalance the
    /// survivors upward into the freed budget.
    pub async fn release_after_teardown(
        &self,
        lease_id: impl Into<String>,
        proof: TeardownConfirmed,
    ) -> Result<(), CpuManagerError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::ReleaseAfterTeardown {
                lease_id: lease_id.into(),
                _proof: proof,
                reply,
            })
            .await
            .map_err(|_| CpuManagerError::Unhealthy {
                reason: "manager actor stopped".to_string(),
            })?;
        rx.await.map_err(|_| CpuManagerError::Unhealthy {
            reason: "manager actor dropped reply".to_string(),
        })?
    }

    pub async fn snapshot(&self) -> Option<CpuManagerSnapshot> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(Command::Snapshot { reply }).await.ok()?;
        rx.await.ok()
    }
}

struct ManagerActor {
    backend: Arc<dyn CpuCgroupBackend>,
    budget_millis: u32,
    health: CpuManagerHealth,
    /// lease_id → applied entitlement.
    applied: BTreeMap<String, AppliedEntitlement>,
    allocation_generation: u64,
}

impl ManagerActor {
    async fn run(mut self, mut rx: mpsc::Receiver<Command>) {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                Command::AdmitAndAttach {
                    request,
                    vmm_pid,
                    reply,
                } => {
                    let _ = reply.send(self.admit(request, vmm_pid));
                }
                Command::ReleaseAfterTeardown {
                    lease_id, reply, ..
                } => {
                    let _ = reply.send(self.release(&lease_id));
                }
                Command::Snapshot { reply } => {
                    let _ = reply.send(self.snapshot());
                }
            }
        }
    }

    fn snapshot(&self) -> CpuManagerSnapshot {
        let mut applied: Vec<_> = self.applied.values().cloned().collect();
        applied.sort_by_key(|a| a.slot_index);
        CpuManagerSnapshot {
            health: self.health.clone(),
            applied,
            allocation_generation: self.allocation_generation,
            budget_millis: self.budget_millis,
        }
    }

    /// Compute allocation over `requests`.
    fn compute(
        &self,
        requests: &[CpuRequest],
    ) -> Result<BTreeMap<String, u32>, CpuAllocationError> {
        allocate_cpu(self.budget_millis, requests)
    }

    /// Apply a target allocation by writing quotas decrease-first then
    /// increase, so the live sum never exceeds the budget mid-transition.
    /// Returns the first cgroup error (leaving partial writes for the caller to
    /// roll back).
    fn apply_quotas(&self, target: &BTreeMap<String, u32>) -> Result<(), CgroupError> {
        // Split by direction relative to the currently-applied quota.
        let mut decreases: Vec<(usize, u32)> = Vec::new();
        let mut increases: Vec<(usize, u32)> = Vec::new();
        for (lease_id, &new_quota) in target {
            let Some(applied) = self.applied.get(lease_id) else {
                // A lease in the target but not yet applied is a brand-new slot;
                // its cgroup starts unlimited-or-zero, so treat as an increase
                // (it holds no live budget to shrink first).
                continue;
            };
            if new_quota < applied.quota_millis {
                decreases.push((applied.slot_index, new_quota));
            } else if new_quota > applied.quota_millis {
                increases.push((applied.slot_index, new_quota));
            }
        }
        for (slot, quota) in decreases {
            self.backend.write_quota_millis(slot, quota)?;
        }
        for (slot, quota) in increases {
            self.backend.write_quota_millis(slot, quota)?;
        }
        Ok(())
    }

    /// Restore every currently-applied lease to its recorded quota. Used to roll
    /// back after a failed apply. Returns Err if a restore write itself fails —
    /// that is the unhealthy path.
    fn restore_applied(&self) -> Result<(), CgroupError> {
        for entitlement in self.applied.values() {
            self.backend
                .write_quota_millis(entitlement.slot_index, entitlement.quota_millis)?;
        }
        Ok(())
    }

    fn go_unhealthy(&mut self, reason: String) {
        self.health = CpuManagerHealth::Unhealthy { reason };
    }

    fn admit(
        &mut self,
        request: CpuRequest,
        vmm_pid: u32,
    ) -> Result<CpuAdmission, CpuManagerError> {
        if let CpuManagerHealth::Unhealthy { reason } = &self.health {
            return Err(CpuManagerError::Unhealthy {
                reason: reason.clone(),
            });
        }
        let lease_id = request.lease_id.clone();
        let slot_index = request.slot_index;

        // The candidate active set = current requests + the new one.
        let mut requests: Vec<CpuRequest> =
            self.applied.values().map(|a| a.request.clone()).collect();
        requests.push(request.clone());
        let target = self.compute(&requests)?;

        // Create the new slot's cgroup before writing its quota / attaching.
        if let Err(e) = self.backend.ensure_slot(slot_index) {
            return Err(self.rollback(e));
        }
        // Set the new slot's quota to its target FIRST at floor via write (it
        // holds no live budget). Order among existing slots is decrease-first.
        if let Err(e) = self.apply_quotas(&target) {
            return Err(self.rollback(e));
        }
        // The new slot itself is an "increase from nothing"; write it explicitly
        // (apply_quotas skips leases not yet in `applied`).
        let new_quota = target[&lease_id];
        if let Err(e) = self.backend.write_quota_millis(slot_index, new_quota) {
            return Err(self.rollback(e));
        }
        // Attach the VMM pid BEFORE the guest resumes.
        if let Err(e) = self.backend.attach_pid(slot_index, vmm_pid) {
            return Err(self.rollback(e));
        }

        // Commit: record every lease at its new quota.
        self.commit(&target, request);
        self.allocation_generation += 1;
        Ok(CpuAdmission {
            lease_id,
            slot_index,
            quota_millis: new_quota,
            allocation_generation: self.allocation_generation,
        })
    }

    /// Roll the cgroup tree back to the last committed allocation. If the
    /// rollback writes succeed the manager stays healthy and returns the
    /// original error; if a rollback write fails, go unhealthy.
    fn rollback(&mut self, cause: CgroupError) -> CpuManagerError {
        if let Err(rollback_err) = self.restore_applied() {
            self.go_unhealthy(format!("rollback after {cause} failed: {rollback_err}"));
            return CpuManagerError::Unhealthy {
                reason: format!("rollback failed: {rollback_err}"),
            };
        }
        CpuManagerError::CgroupRolledBack(cause)
    }

    fn commit(&mut self, target: &BTreeMap<String, u32>, new_request: CpuRequest) {
        // Update existing entries' quotas.
        for entitlement in self.applied.values_mut() {
            if let Some(&q) = target.get(&entitlement.request.lease_id) {
                entitlement.quota_millis = q;
            }
        }
        // Insert the new lease.
        let q = target[&new_request.lease_id];
        self.applied.insert(
            new_request.lease_id.clone(),
            AppliedEntitlement {
                slot_index: new_request.slot_index,
                request: new_request,
                quota_millis: q,
            },
        );
    }

    fn release(&mut self, lease_id: &str) -> Result<(), CpuManagerError> {
        if let CpuManagerHealth::Unhealthy { reason } = &self.health {
            return Err(CpuManagerError::Unhealthy {
                reason: reason.clone(),
            });
        }
        let Some(removed) = self.applied.remove(lease_id) else {
            return Err(CpuManagerError::UnknownLease {
                lease_id: lease_id.to_string(),
            });
        };
        // Remove the freed slot's cgroup (best-effort — it must be empty; VM is
        // gone). A remove failure is not fatal: the slot budget is already
        // reclaimed in accounting, and the survivors' rebalance is what matters.
        let _ = self.backend.remove_slot(removed.slot_index);

        if self.applied.is_empty() {
            self.allocation_generation += 1;
            return Ok(());
        }
        let requests: Vec<CpuRequest> = self.applied.values().map(|a| a.request.clone()).collect();
        // Cannot exceed a floor here: we removed a request, so min-sum only
        // shrank; recompute must succeed.
        let target = match self.compute(&requests) {
            Ok(t) => t,
            Err(e) => {
                // Should be unreachable (removing a request never breaks the
                // floor sum), but treat defensively as unhealthy rather than
                // leaving survivors with stale quotas.
                self.go_unhealthy(format!("post-release allocation failed: {e}"));
                return Err(CpuManagerError::Unhealthy {
                    reason: format!("post-release allocation failed: {e}"),
                });
            }
        };
        // Survivors only ever RISE after a release (freed budget), so this is
        // all increases — still safe to apply directly.
        if let Err(e) = self.apply_quotas(&target) {
            return Err(self.rollback(e));
        }
        for entitlement in self.applied.values_mut() {
            if let Some(&q) = target.get(&entitlement.request.lease_id) {
                entitlement.quota_millis = q;
            }
        }
        self.allocation_generation += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::runner_cgroup::{FakeCgroupBackend, FakeFailPoint};
    use super::*;

    fn std_req(lease: &str, slot: usize) -> CpuRequest {
        CpuRequest {
            lease_id: lease.to_string(),
            slot_index: slot,
            min_millis: 1000,
            max_millis: 2000,
        }
    }
    fn eco_req(lease: &str, slot: usize) -> CpuRequest {
        CpuRequest {
            lease_id: lease.to_string(),
            slot_index: slot,
            min_millis: 1000,
            max_millis: 1000,
        }
    }

    async fn admit(mgr: &CpuEntitlementManager, req: CpuRequest, pid: u32) -> u32 {
        mgr.admit_and_attach(req, pid)
            .await
            .expect("admit")
            .quota_millis
    }

    #[tokio::test]
    async fn single_standard_gets_max() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::spawn(be.clone(), 8000);
        assert_eq!(admit(&mgr, std_req("a", 0), 100).await, 2000);
        assert_eq!(be.quota_of(0), Some(2000));
        assert_eq!(be.pids_of(0), vec![100]);
    }

    #[tokio::test]
    async fn four_standard_constrained_share_evenly() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::spawn(be.clone(), 6000);
        for i in 0..4 {
            admit(&mgr, std_req(&format!("s{i}"), i), 100 + i as u32).await;
        }
        let snap = mgr.snapshot().await.unwrap();
        for a in &snap.applied {
            assert_eq!(a.quota_millis, 1500, "slot {}", a.slot_index);
        }
        for i in 0..4 {
            assert_eq!(be.quota_of(i), Some(1500));
        }
    }

    #[tokio::test]
    async fn three_standard_plus_economy() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::spawn(be.clone(), 6000);
        admit(&mgr, std_req("a", 0), 1).await;
        admit(&mgr, std_req("b", 1), 2).await;
        admit(&mgr, std_req("c", 2), 3).await;
        admit(&mgr, eco_req("eco", 3), 4).await;
        assert_eq!(be.quota_of(0), Some(1667));
        assert_eq!(be.quota_of(1), Some(1667));
        assert_eq!(be.quota_of(2), Some(1666));
        assert_eq!(be.quota_of(3), Some(1000));
    }

    #[tokio::test]
    async fn release_rebalances_survivors_upward() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::spawn(be.clone(), 6000);
        for i in 0..4 {
            admit(&mgr, std_req(&format!("s{i}"), i), 100 + i as u32).await;
        }
        for i in 0..4 {
            assert_eq!(be.quota_of(i), Some(1500));
        }
        mgr.release_after_teardown("s3", TeardownConfirmed::assert())
            .await
            .expect("release");
        // Survivors rise to their 2000m ceiling; freed slot cgroup removed.
        for i in 0..3 {
            assert_eq!(be.quota_of(i), Some(2000), "survivor slot {i}");
        }
        assert_eq!(be.quota_of(3), None, "freed slot removed");
    }

    #[tokio::test]
    async fn admit_over_floor_sum_is_rejected_and_rolls_back() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::spawn(be.clone(), 2000);
        admit(&mgr, std_req("a", 0), 1).await;
        admit(&mgr, std_req("b", 1), 2).await; // 2×1000 floor = budget, ok
        let third = mgr.admit_and_attach(std_req("c", 2), 3).await;
        assert!(matches!(
            third,
            Err(CpuManagerError::Allocation(
                CpuAllocationError::InsufficientMinimumCapacity { .. }
            ))
        ));
        // Existing two untouched; third slot never created/attached.
        assert_eq!(be.quota_of(0), Some(1000));
        assert_eq!(be.quota_of(1), Some(1000));
        assert_eq!(be.pids_of(2), Vec::<u32>::new());
    }

    #[tokio::test]
    async fn attach_failure_rolls_back_without_starting_guest() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::spawn(be.clone(), 8000);
        admit(&mgr, std_req("a", 0), 100).await;
        be.fail_at(FakeFailPoint::AttachPid);
        let res = mgr.admit_and_attach(std_req("b", 1), 200).await;
        assert!(matches!(res, Err(CpuManagerError::CgroupRolledBack(_))));
        // The survivor 'a' is restored to its committed quota; 'b' not attached.
        assert_eq!(be.quota_of(0), Some(2000));
        assert_eq!(be.pids_of(1), Vec::<u32>::new());
        // Manager still healthy — a clean rollback keeps it serving.
        let snap = mgr.snapshot().await.unwrap();
        assert_eq!(snap.health, CpuManagerHealth::Healthy);
        assert_eq!(snap.applied.len(), 1);
    }

    #[tokio::test]
    async fn rollback_failure_marks_unhealthy_and_stops_admissions() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::spawn(be.clone(), 6000);
        // Three standard over 6000 → 2000m each. Admitting a fourth forces every
        // existing slot to DECREASE 2000→1500. Arm WriteQuotaAny so both that
        // decrease AND the rollback restore fail → the manager must go unhealthy.
        admit(&mgr, std_req("a", 0), 100).await;
        admit(&mgr, std_req("b", 1), 200).await;
        admit(&mgr, std_req("c", 2), 300).await;
        be.fail_at(FakeFailPoint::WriteQuotaAny);
        let res = mgr.admit_and_attach(std_req("d", 3), 400).await;
        assert!(matches!(res, Err(CpuManagerError::Unhealthy { .. })));
        let snap = mgr.snapshot().await.unwrap();
        assert!(matches!(snap.health, CpuManagerHealth::Unhealthy { .. }));
        // Further admissions are refused while unhealthy.
        be.clear_fail();
        let after = mgr.admit_and_attach(std_req("e", 4), 500).await;
        assert!(matches!(after, Err(CpuManagerError::Unhealthy { .. })));
    }

    #[tokio::test]
    async fn releasing_unknown_lease_errs() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::spawn(be, 8000);
        let res = mgr
            .release_after_teardown("ghost", TeardownConfirmed::assert())
            .await;
        assert!(matches!(res, Err(CpuManagerError::UnknownLease { .. })));
    }
}
