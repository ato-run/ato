//! Single-owner CPU entitlement manager (ADR-016 PR2).
//!
//! The manager is a dedicated **std::thread** actor (`ato-cpu-entitlement`),
//! deliberately NOT a tokio task: its work is exclusively synchronous local
//! kernel-filesystem I/O (cgroup read/write) plus pure allocator math, and its
//! one caller that matters is the synchronous pre-resume hook seam invoked
//! inline on an async lease task — a plain blocking request/reply to an OS
//! thread is safe from ANY runtime context (no `block_in_place`, no
//! `Handle::block_on`, no `blocking_send`), independent of Tokio flavor.
//! Requests wait for transaction completion (no caller-side timeout — a timeout
//! without commit fencing could abandon a request the actor later commits); a
//! FULL queue is refused up-front as [`CpuManagerError::ManagerBusy`] so a
//! guest is never resumed on an unconfirmed quota.
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
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

use super::runner_cgroup::{CgroupError, CpuCgroupBackend};
use super::runner_cpu_allocator::{CpuAllocationError, CpuRequest, allocate_cpu};

/// An entitlement currently applied to a slot cgroup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEntitlement {
    pub slot_index: usize,
    /// Host pid of the VMM attached to this slot's cgroup. Recorded so a
    /// teardown proof is verified against the exact process, not merely the
    /// lease+slot — a stale proof for a reused slot cannot release the new one.
    pub vmm_pid: u32,
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
    /// The teardown proof's identity does not match the recorded entitlement
    /// (wrong slot or pid for that lease). Fail-closed: nothing is released.
    #[error("teardown proof mismatch for lease {lease_id}: {detail}")]
    ProofMismatch { lease_id: String, detail: String },
    /// The freed slot's cgroup could not be confirmed empty-and-removed, so its
    /// CPU budget must NOT be handed to survivors (a lingering process would
    /// keep consuming it). Fail-closed to Unhealthy.
    #[error("slot {slot_index} could not be reclaimed for lease {lease_id}: {detail}")]
    SlotNotReclaimed {
        lease_id: String,
        slot_index: usize,
        detail: String,
    },
    /// The manager's request queue is full. Fail-closed for admissions: the
    /// guest is NOT resumed. The caller may retry or fail the launch.
    #[error("CPU manager request queue is full")]
    ManagerBusy,
    /// The manager thread is gone (channel disconnected) — process shutdown or
    /// a panic in the actor. No entitlement transaction was or will be applied
    /// for this request.
    #[error("CPU manager stopped: {reason}")]
    ManagerStopped { reason: String },
}

/// Outcome of a successful admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuAdmission {
    pub lease_id: String,
    pub slot_index: usize,
    pub quota_millis: u32,
    pub allocation_generation: u64,
}

/// Result of a successful release. The session's slot IS reclaimed — the caller
/// MUST proceed to free the runner slot regardless of `rebalance`, so a
/// survivor-rebalance hiccup can never leak the freed slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuReleaseOutcome {
    pub allocation_generation: u64,
    pub rebalance: RebalanceOutcome,
}

/// What happened to the survivors' quotas after a reclaim. `RolledBack` /
/// `Unhealthy` are informational — the release already succeeded and the slot
/// is gone; they signal that survivors did not grow into the freed budget yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebalanceOutcome {
    /// Survivors raised into the freed budget.
    Applied,
    /// No survivors remained; nothing to rebalance.
    NoSurvivors,
    /// The survivor quota-raise failed and was rolled back to prior quotas; the
    /// manager stays healthy and will re-attempt on the next allocation change.
    RolledBack { error: CgroupError },
    /// The survivor raise AND its rollback failed; the manager is now Unhealthy.
    /// The freed slot is STILL released (it was reclaimed before this step).
    Unhealthy { reason: String },
    /// The manager was already Unhealthy when this release was processed: the
    /// slot was reclaimed and the reservation freed (refusing releases while
    /// Unhealthy would leak slots and cgroups forever), but survivors were NOT
    /// raised — the recorded allocation may not match the cgroup tree, so
    /// growing into it is not provably safe.
    SkippedUnhealthy { reason: String },
}

/// A read-only view of manager state for diagnostics / tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuManagerSnapshot {
    pub health: CpuManagerHealth,
    pub applied: Vec<AppliedEntitlement>,
    pub allocation_generation: u64,
    pub budget_millis: u32,
}

/// Observed evidence that a VMM process has exited — the only thing that mints a
/// [`TeardownObservation`]. Held by value so the proof cannot be forged from a
/// bare marker; the runner constructs it from a real `wait`/`kill` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessExitEvidence {
    /// The VMM child was `wait`ed and is gone.
    Reaped,
    /// The VMM was signalled and confirmed no longer present (`kill -0` ENOENT).
    Killed,
}

/// An observation that a specific session's VM has been torn down, carrying the
/// session identity (`lease_id` / `slot_index` / `vmm_pid`) and the evidence.
///
/// `release()` requires one and verifies ALL THREE identity fields against the
/// recorded entitlement, so a proof for one session — or a stale proof for a
/// slot that has since been reused by a different pid — can never release the
/// current occupant. Not `Clone`: a capability to release should not be
/// duplicable.
///
/// Named "observation" rather than "confirmed" deliberately: `observed()` is
/// `pub(crate)`, which does not by itself prove the caller watched the exit.
/// PR 2b moves the constructor into the real teardown module so the type
/// becomes a genuine capability; until then it is an identity-checked argument.
#[derive(Debug)]
pub struct TeardownObservation {
    lease_id: String,
    slot_index: usize,
    vmm_pid: u32,
    observed_exit: ProcessExitEvidence,
}

impl TeardownObservation {
    /// Build from a real process-exit observation (reap or confirmed kill),
    /// never speculatively.
    pub(crate) fn observed(
        lease_id: impl Into<String>,
        slot_index: usize,
        vmm_pid: u32,
        evidence: ProcessExitEvidence,
    ) -> Self {
        Self {
            lease_id: lease_id.into(),
            slot_index,
            vmm_pid,
            observed_exit: evidence,
        }
    }

    pub fn lease_id(&self) -> &str {
        &self.lease_id
    }
}

enum Command {
    AdmitAndAttach {
        request: CpuRequest,
        vmm_pid: u32,
        reply: SyncSender<Result<CpuAdmission, CpuManagerError>>,
    },
    ReleaseAfterTeardown {
        proof: TeardownObservation,
        reply: SyncSender<Result<CpuReleaseOutcome, CpuManagerError>>,
    },
    Snapshot {
        reply: SyncSender<CpuManagerSnapshot>,
    },
    Shutdown {
        reply: SyncSender<()>,
    },
}

/// Health mirror encoding for the lock-free [`CpuEntitlementManager::health`]
/// read (the reason string travels via `snapshot()`).
const HEALTH_HEALTHY: u8 = 0;
const HEALTH_UNHEALTHY: u8 = 1;

/// State shared between the handle(s) and the actor thread: the command queue
/// plus lock-free mirrors the actor updates after every state change, so hot
/// callers (heartbeat capability checks) never queue a round-trip just to ask
/// "are you healthy".
struct ManagerShared {
    tx: SyncSender<Command>,
    health: AtomicU8,
    allocation_generation: AtomicU64,
}

/// Handle to the manager actor. Cloneable; every clone talks to the one owner
/// thread. All methods are synchronous and safe to call from any thread,
/// including tokio runtime workers: the actor does only local kernel-filesystem
/// I/O and allocator math, so a request/reply round-trip is bounded fs-write
/// time, not network time.
#[derive(Clone)]
pub struct CpuEntitlementManager {
    inner: Arc<ManagerShared>,
}

impl CpuEntitlementManager {
    /// Start the owner thread (`ato-cpu-entitlement`) over `backend` with
    /// `budget_millis`. `queue_capacity` bounds in-flight commands (derive it
    /// from max_slots, e.g. `max(8, max_slots * 2 + 4)`); a full queue refuses
    /// new work as [`CpuManagerError::ManagerBusy`] instead of blocking
    /// admissions behind an unbounded backlog.
    pub fn start(
        backend: Arc<dyn CpuCgroupBackend>,
        budget_millis: u32,
        queue_capacity: usize,
    ) -> Result<Self, CpuManagerError> {
        let (tx, rx) = sync_channel(queue_capacity.max(1));
        let inner = Arc::new(ManagerShared {
            tx,
            health: AtomicU8::new(HEALTH_HEALTHY),
            allocation_generation: AtomicU64::new(0),
        });
        let actor = ManagerActor {
            backend,
            budget_millis,
            health: CpuManagerHealth::Healthy,
            applied: BTreeMap::new(),
            allocation_generation: 0,
            shared: Arc::clone(&inner),
        };
        std::thread::Builder::new()
            .name("ato-cpu-entitlement".to_string())
            .spawn(move || actor.run(rx))
            .map_err(|e| CpuManagerError::ManagerStopped {
                reason: format!("spawn manager thread: {e}"),
            })?;
        Ok(Self { inner })
    }

    /// Send a command whose slot in the queue was made by `make`, then wait for
    /// the reply. Waits for TRANSACTION COMPLETION — no caller-side timeout, by
    /// design: a timeout that abandons the wait while the actor later commits
    /// would desynchronize caller and manager state (needs cancel tokens +
    /// commit fencing to add safely; deferred).
    fn request<T>(
        &self,
        make: impl FnOnce(SyncSender<T>) -> Command,
    ) -> Result<T, CpuManagerError> {
        let (reply_tx, reply_rx) = sync_channel::<T>(1);
        match self.inner.tx.try_send(make(reply_tx)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => return Err(CpuManagerError::ManagerBusy),
            Err(TrySendError::Disconnected(_)) => {
                return Err(CpuManagerError::ManagerStopped {
                    reason: "manager thread exited".to_string(),
                });
            }
        }
        reply_rx
            .recv()
            .map_err(|_| CpuManagerError::ManagerStopped {
                reason: "manager thread dropped reply".to_string(),
            })
    }

    /// Admit a new session: compute the new allocation including it, apply the
    /// quotas (decrease-before-increase), attach `vmm_pid` to the slot cgroup,
    /// and commit. On any failure the previous allocation is restored and this
    /// returns an error WITHOUT the pid attached — the caller must not resume
    /// the guest. Synchronous: blocks the calling thread for the fs-write
    /// round-trip; called from the pre-resume hook where the launch is already
    /// serialized on this result.
    pub fn admit_and_attach(
        &self,
        request: CpuRequest,
        vmm_pid: u32,
    ) -> Result<CpuAdmission, CpuManagerError> {
        self.request(|reply| Command::AdmitAndAttach {
            request,
            vmm_pid,
            reply,
        })?
    }

    /// Release a session AFTER its VM teardown is confirmed, then rebalance the
    /// survivors upward into the freed budget. The `proof` carries the session
    /// identity the manager verifies against its record. On `Ok`, the slot IS
    /// reclaimed and the caller must free the runner slot; `outcome.rebalance`
    /// reports what happened to survivors. Processed even when the manager is
    /// Unhealthy (refusing releases would leak slots); only admissions are
    /// refused there.
    pub fn release_after_teardown(
        &self,
        proof: TeardownObservation,
    ) -> Result<CpuReleaseOutcome, CpuManagerError> {
        // A full queue must not lose a release: unlike an admission (where
        // refusing is the safe direction), a refused release leaks the slot. So
        // this uses a BLOCKING send — bounded, because every queued command is
        // local fs I/O, and the actor never blocks on a caller (replies go into
        // a capacity-1 channel whose receiver is already waiting).
        let (reply_tx, reply_rx) = sync_channel(1);
        self.inner
            .tx
            .send(Command::ReleaseAfterTeardown {
                proof,
                reply: reply_tx,
            })
            .map_err(|_| CpuManagerError::ManagerStopped {
                reason: "manager thread exited".to_string(),
            })?;
        reply_rx
            .recv()
            .map_err(|_| CpuManagerError::ManagerStopped {
                reason: "manager thread dropped reply".to_string(),
            })?
    }

    /// Lock-free health read from the atomic mirror (reason via `snapshot()`).
    pub fn health(&self) -> CpuManagerHealth {
        match self.inner.health.load(Ordering::Acquire) {
            HEALTH_HEALTHY => CpuManagerHealth::Healthy,
            _ => CpuManagerHealth::Unhealthy {
                reason: "see snapshot() for detail".to_string(),
            },
        }
    }

    /// Lock-free read of the allocation generation mirror.
    pub fn allocation_generation(&self) -> u64 {
        self.inner.allocation_generation.load(Ordering::Acquire)
    }

    pub fn snapshot(&self) -> Result<CpuManagerSnapshot, CpuManagerError> {
        self.request(|reply| Command::Snapshot { reply })
    }

    /// Ask the actor to stop after draining commands ahead of this one, and
    /// wait until it acknowledges. Further requests return `ManagerStopped`.
    pub fn shutdown(&self) -> Result<(), CpuManagerError> {
        // Blocking send: shutdown must not be refused for a momentarily-full
        // queue.
        let (reply_tx, reply_rx) = sync_channel(1);
        self.inner
            .tx
            .send(Command::Shutdown { reply: reply_tx })
            .map_err(|_| CpuManagerError::ManagerStopped {
                reason: "manager thread already exited".to_string(),
            })?;
        reply_rx
            .recv()
            .map_err(|_| CpuManagerError::ManagerStopped {
                reason: "manager thread dropped shutdown ack".to_string(),
            })
    }
}

struct ManagerActor {
    backend: Arc<dyn CpuCgroupBackend>,
    budget_millis: u32,
    health: CpuManagerHealth,
    /// lease_id → applied entitlement.
    applied: BTreeMap<String, AppliedEntitlement>,
    allocation_generation: u64,
    /// Mirrors for lock-free reads from handles.
    shared: Arc<ManagerShared>,
}

impl ManagerActor {
    fn run(mut self, rx: Receiver<Command>) {
        // Exits when every handle is dropped (recv disconnect) or on Shutdown.
        while let Ok(cmd) = rx.recv() {
            match cmd {
                Command::AdmitAndAttach {
                    request,
                    vmm_pid,
                    reply,
                } => {
                    let result = self.admit(request, vmm_pid);
                    self.mirror();
                    let _ = reply.send(result);
                }
                Command::ReleaseAfterTeardown { proof, reply } => {
                    let result = self.release(proof);
                    self.mirror();
                    let _ = reply.send(result);
                }
                Command::Snapshot { reply } => {
                    let _ = reply.send(self.snapshot());
                }
                Command::Shutdown { reply } => {
                    let _ = reply.send(());
                    break;
                }
            }
        }
    }

    /// Publish health + generation to the lock-free mirrors AFTER a state
    /// change and BEFORE the reply is sent, so a caller that observes a reply
    /// can never read a mirror older than that reply.
    fn mirror(&self) {
        let health = match self.health {
            CpuManagerHealth::Healthy => HEALTH_HEALTHY,
            CpuManagerHealth::Unhealthy { .. } => HEALTH_UNHEALTHY,
        };
        self.shared.health.store(health, Ordering::Release);
        self.shared
            .allocation_generation
            .store(self.allocation_generation, Ordering::Release);
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

        // Create the new slot's cgroup before writing its quota / attaching. A
        // failure here created no candidate cgroup, so only existing slots need
        // restoring.
        if let Err(e) = self.backend.ensure_slot(slot_index) {
            return Err(self.rollback(e, None));
        }
        // BEFORE touching any existing session's quota, confirm the candidate
        // slot is empty. ensure_slot is idempotent, so it could have reused a
        // cgroup that already holds a process; discovering that only after
        // rebalancing existing sessions would have already disturbed them. A
        // lingering pid, or an unreadable proc list, fails closed to Unhealthy
        // and leaves every existing session's quota untouched.
        match self.backend.slot_pids(slot_index) {
            Ok(pids) if pids.is_empty() => {}
            Ok(pids) => {
                // Candidate cgroup already exists and holds a process — remove it
                // was never ours to create, so do NOT tear it down; just refuse.
                self.go_unhealthy(format!(
                    "admit {lease_id}: candidate slot {slot_index} already holds \
                     {} pid(s) before quota change",
                    pids.len()
                ));
                return Err(CpuManagerError::Unhealthy {
                    reason: format!("candidate slot {slot_index} not empty at admit"),
                });
            }
            Err(read_err) => {
                self.go_unhealthy(format!(
                    "admit {lease_id}: candidate slot {slot_index} pid read failed \
                     before quota change: {read_err}"
                ));
                return Err(CpuManagerError::Unhealthy {
                    reason: format!("candidate slot {slot_index} pid read failed at admit"),
                });
            }
        }
        // From here the candidate cgroup exists and is empty; every rollback must
        // also clean it up (Blocker 5).
        // Set the new slot's quota to its target FIRST at floor via write (it
        // holds no live budget). Order among existing slots is decrease-first.
        if let Err(e) = self.apply_quotas(&target) {
            return Err(self.rollback(e, Some(slot_index)));
        }
        // The new slot itself is an "increase from nothing"; write it explicitly
        // (apply_quotas skips leases not yet in `applied`).
        let new_quota = target[&lease_id];
        if let Err(e) = self.backend.write_quota_millis(slot_index, new_quota) {
            return Err(self.rollback(e, Some(slot_index)));
        }
        // Attach the VMM pid BEFORE the guest resumes.
        if let Err(e) = self.backend.attach_pid(slot_index, vmm_pid) {
            return Err(self.rollback(e, Some(slot_index)));
        }

        // Commit: record every lease at its new quota, including the pid.
        self.commit(&target, request, vmm_pid);
        self.allocation_generation += 1;
        Ok(CpuAdmission {
            lease_id,
            slot_index,
            quota_millis: new_quota,
            allocation_generation: self.allocation_generation,
        })
    }

    /// Roll the cgroup tree back to the last committed allocation after a failed
    /// admission. If `candidate_slot` is set, its just-created cgroup is torn
    /// down too — but ONLY after confirming it holds no pid: a candidate that
    /// already has a process (attach partially took, or an unexpected occupant),
    /// or whose pid list can't be read, or that won't remove, means the host
    /// state is uncertain, so the manager goes Unhealthy rather than pretend it
    /// cleaned up. A clean rollback keeps it Healthy and returns the cause.
    fn rollback(&mut self, cause: CgroupError, candidate_slot: Option<usize>) -> CpuManagerError {
        if let Some(slot) = candidate_slot {
            match self.backend.slot_pids(slot) {
                Ok(pids) if pids.is_empty() => {
                    if let Err(remove_err) = self.backend.remove_slot(slot) {
                        self.go_unhealthy(format!(
                            "admission rollback after {cause}: candidate slot {slot} \
                             remove failed: {remove_err}"
                        ));
                        return CpuManagerError::Unhealthy {
                            reason: format!("candidate slot {slot} remove failed: {remove_err}"),
                        };
                    }
                }
                Ok(pids) => {
                    self.go_unhealthy(format!(
                        "admission rollback after {cause}: candidate slot {slot} holds \
                         {} pid(s); refusing to reuse",
                        pids.len()
                    ));
                    return CpuManagerError::Unhealthy {
                        reason: format!("candidate slot {slot} not empty"),
                    };
                }
                Err(read_err) => {
                    self.go_unhealthy(format!(
                        "admission rollback after {cause}: candidate slot {slot} pid \
                         read failed: {read_err}"
                    ));
                    return CpuManagerError::Unhealthy {
                        reason: format!("candidate slot {slot} pid read failed: {read_err}"),
                    };
                }
            }
        }
        if let Err(rollback_err) = self.restore_applied() {
            self.go_unhealthy(format!("rollback after {cause} failed: {rollback_err}"));
            return CpuManagerError::Unhealthy {
                reason: format!("rollback failed: {rollback_err}"),
            };
        }
        CpuManagerError::CgroupRolledBack(cause)
    }

    fn commit(&mut self, target: &BTreeMap<String, u32>, new_request: CpuRequest, vmm_pid: u32) {
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
                vmm_pid,
                request: new_request,
                quota_millis: q,
            },
        );
    }

    fn release(
        &mut self,
        proof: TeardownObservation,
    ) -> Result<CpuReleaseOutcome, CpuManagerError> {
        // NOTE: releases are processed even when Unhealthy — refusing them would
        // leak slots and cgroups forever. Only NEW admissions are refused there.
        // The reclaim below runs fully verified regardless of health; what an
        // Unhealthy manager skips is the survivor raise (see the tail).
        let lease_id = proof.lease_id.clone();
        // Verify ALL THREE identity fields against the record BEFORE mutating any
        // state — a proof for one session, or a stale proof for a slot since
        // reused by a different pid, can never release the current occupant.
        let Some(entitlement) = self.applied.get(&lease_id) else {
            return Err(CpuManagerError::UnknownLease {
                lease_id: lease_id.clone(),
            });
        };
        if entitlement.slot_index != proof.slot_index {
            return Err(CpuManagerError::ProofMismatch {
                lease_id,
                detail: format!(
                    "proof slot {} != recorded slot {}",
                    proof.slot_index, entitlement.slot_index
                ),
            });
        }
        if entitlement.vmm_pid != proof.vmm_pid {
            return Err(CpuManagerError::ProofMismatch {
                lease_id,
                detail: format!(
                    "proof pid {} != recorded pid {}",
                    proof.vmm_pid, entitlement.vmm_pid
                ),
            });
        }
        let slot_index = entitlement.slot_index;

        // Reclaim BEFORE releasing accounting (Blocker 1): the freed slot's CPU
        // budget may only flow to survivors once we have proven the slot cgroup
        // is empty and removed. Any doubt — a lingering pid, an unreadable proc
        // list, or a remove failure — means a process could still be consuming
        // that budget, so we keep the reservation and go Unhealthy rather than
        // double-allocate it.
        match self.backend.slot_pids(slot_index) {
            Ok(pids) if pids.is_empty() => {}
            Ok(pids) => {
                self.go_unhealthy(format!(
                    "release {lease_id}: slot {slot_index} still holds {} pid(s)",
                    pids.len()
                ));
                return Err(CpuManagerError::SlotNotReclaimed {
                    lease_id,
                    slot_index,
                    detail: "slot cgroup not empty".to_string(),
                });
            }
            Err(read_err) => {
                self.go_unhealthy(format!(
                    "release {lease_id}: slot {slot_index} pid read failed: {read_err}"
                ));
                return Err(CpuManagerError::SlotNotReclaimed {
                    lease_id,
                    slot_index,
                    detail: format!("pid read failed: {read_err}"),
                });
            }
        }
        if let Err(remove_err) = self.backend.remove_slot(slot_index) {
            self.go_unhealthy(format!(
                "release {lease_id}: slot {slot_index} remove failed: {remove_err}"
            ));
            return Err(CpuManagerError::SlotNotReclaimed {
                lease_id,
                slot_index,
                detail: format!("remove failed: {remove_err}"),
            });
        }

        // Slot proven reclaimed — the release has SUCCEEDED and the caller must
        // free the runner slot no matter what the survivor rebalance does next.
        // From here we never return Err: a rebalance hiccup is reported inside
        // CpuReleaseOutcome, not as a release failure (which would leak the slot).
        self.applied.remove(&lease_id);
        self.allocation_generation += 1;

        if self.applied.is_empty() {
            return Ok(CpuReleaseOutcome {
                allocation_generation: self.allocation_generation,
                rebalance: RebalanceOutcome::NoSurvivors,
            });
        }
        if let CpuManagerHealth::Unhealthy { reason } = &self.health {
            // Slot reclaimed and reservation freed, but the recorded allocation
            // may not match the cgroup tree — raising survivors into the freed
            // budget is not provably safe, so leave their quotas as-is.
            return Ok(CpuReleaseOutcome {
                allocation_generation: self.allocation_generation,
                rebalance: RebalanceOutcome::SkippedUnhealthy {
                    reason: reason.clone(),
                },
            });
        }
        let requests: Vec<CpuRequest> = self.applied.values().map(|a| a.request.clone()).collect();
        // Removing a request only shrank the floor sum, so this recompute cannot
        // exceed the budget; a failure here is defensive-only.
        let target = match self.compute(&requests) {
            Ok(t) => t,
            Err(e) => {
                let reason = format!("post-release allocation failed: {e}");
                self.go_unhealthy(reason.clone());
                return Ok(CpuReleaseOutcome {
                    allocation_generation: self.allocation_generation,
                    rebalance: RebalanceOutcome::Unhealthy { reason },
                });
            }
        };
        // Survivors only ever RISE after a release (freed budget), so this is all
        // increases. If the raise fails, roll survivors back to their prior
        // quotas; the release itself still stands (slot already reclaimed).
        let rebalance = if let Err(apply_err) = self.apply_quotas(&target) {
            if let Err(rollback_err) = self.restore_applied() {
                let reason =
                    format!("survivor rebalance {apply_err} rollback failed: {rollback_err}");
                self.go_unhealthy(reason.clone());
                RebalanceOutcome::Unhealthy { reason }
            } else {
                RebalanceOutcome::RolledBack { error: apply_err }
            }
        } else {
            for entitlement in self.applied.values_mut() {
                if let Some(&q) = target.get(&entitlement.request.lease_id) {
                    entitlement.quota_millis = q;
                }
            }
            RebalanceOutcome::Applied
        };
        Ok(CpuReleaseOutcome {
            allocation_generation: self.allocation_generation,
            rebalance,
        })
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

    fn admit(mgr: &CpuEntitlementManager, req: CpuRequest, pid: u32) -> u32 {
        mgr.admit_and_attach(req, pid).expect("admit").quota_millis
    }

    fn proof(lease: &str, slot: usize, pid: u32) -> TeardownObservation {
        TeardownObservation::observed(lease, slot, pid, ProcessExitEvidence::Reaped)
    }

    /// Model a real teardown then release: the VMM leaves the slot cgroup, then
    /// the teardown proof is presented.
    fn teardown_and_release(
        mgr: &CpuEntitlementManager,
        be: &FakeCgroupBackend,
        lease: &str,
        slot: usize,
        pid: u32,
    ) -> Result<CpuReleaseOutcome, CpuManagerError> {
        be.simulate_process_exit(slot, pid);
        mgr.release_after_teardown(proof(lease, slot, pid))
    }

    #[test]
    fn single_standard_gets_max() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 8000, 16).unwrap();
        assert_eq!(admit(&mgr, std_req("a", 0), 100), 2000);
        assert_eq!(be.quota_of(0), Some(2000));
        assert_eq!(be.pids_of(0), vec![100]);
    }

    #[test]
    fn four_standard_constrained_share_evenly() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 6000, 16).unwrap();
        for i in 0..4 {
            admit(&mgr, std_req(&format!("s{i}"), i), 100 + i as u32);
        }
        let snap = mgr.snapshot().unwrap();
        for a in &snap.applied {
            assert_eq!(a.quota_millis, 1500, "slot {}", a.slot_index);
        }
        for i in 0..4 {
            assert_eq!(be.quota_of(i), Some(1500));
        }
    }

    #[test]
    fn three_standard_plus_economy() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 6000, 16).unwrap();
        admit(&mgr, std_req("a", 0), 1);
        admit(&mgr, std_req("b", 1), 2);
        admit(&mgr, std_req("c", 2), 3);
        admit(&mgr, eco_req("eco", 3), 4);
        assert_eq!(be.quota_of(0), Some(1667));
        assert_eq!(be.quota_of(1), Some(1667));
        assert_eq!(be.quota_of(2), Some(1666));
        assert_eq!(be.quota_of(3), Some(1000));
    }

    #[test]
    fn release_rebalances_survivors_upward() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 6000, 16).unwrap();
        for i in 0..4 {
            admit(&mgr, std_req(&format!("s{i}"), i), 100 + i as u32);
        }
        for i in 0..4 {
            assert_eq!(be.quota_of(i), Some(1500));
        }
        teardown_and_release(&mgr, &be, "s3", 3, 103).expect("release");
        // Survivors rise to their 2000m ceiling; freed slot cgroup removed.
        for i in 0..3 {
            assert_eq!(be.quota_of(i), Some(2000), "survivor slot {i}");
        }
        assert_eq!(be.quota_of(3), None, "freed slot removed");
    }

    #[test]
    fn admit_over_floor_sum_is_rejected_and_rolls_back() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 2000, 16).unwrap();
        admit(&mgr, std_req("a", 0), 1);
        admit(&mgr, std_req("b", 1), 2); // 2×1000 floor = budget, ok
        let third = mgr.admit_and_attach(std_req("c", 2), 3);
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

    #[test]
    fn attach_failure_rolls_back_without_starting_guest() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 8000, 16).unwrap();
        admit(&mgr, std_req("a", 0), 100);
        be.fail_at(FakeFailPoint::AttachPid);
        let res = mgr.admit_and_attach(std_req("b", 1), 200);
        assert!(matches!(res, Err(CpuManagerError::CgroupRolledBack(_))));
        // The survivor 'a' is restored to its committed quota; the empty
        // candidate cgroup for 'b' is torn down (Blocker 5).
        assert_eq!(be.quota_of(0), Some(2000));
        assert_eq!(be.quota_of(1), None, "empty candidate slot removed");
        assert_eq!(be.pids_of(1), Vec::<u32>::new());
        // Manager still healthy — a clean rollback keeps it serving.
        let snap = mgr.snapshot().unwrap();
        assert_eq!(snap.health, CpuManagerHealth::Healthy);
        assert_eq!(snap.applied.len(), 1);
    }

    #[test]
    fn admit_candidate_cleanup_remove_failure_goes_unhealthy() {
        // attach fails → rollback inspects the empty candidate and tries to
        // remove it → the remove ALSO fails → Unhealthy. Both points armed at
        // once (the fake now supports a failure set).
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 8000, 16).unwrap();
        admit(&mgr, std_req("a", 0), 100);
        be.fail_at(FakeFailPoint::AttachPid);
        be.fail_at(FakeFailPoint::RemoveSlot);
        let res = mgr.admit_and_attach(std_req("b", 1), 200);
        assert!(matches!(res, Err(CpuManagerError::Unhealthy { .. })));
        let snap = mgr.snapshot().unwrap();
        assert!(matches!(snap.health, CpuManagerHealth::Unhealthy { .. }));
    }

    #[test]
    fn rollback_failure_marks_unhealthy_and_stops_admissions() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 6000, 16).unwrap();
        // Three standard over 6000 → 2000m each. Admitting a fourth forces every
        // existing slot to DECREASE 2000→1500. Arm WriteQuotaAny so both that
        // decrease AND the rollback restore fail → the manager must go unhealthy.
        admit(&mgr, std_req("a", 0), 100);
        admit(&mgr, std_req("b", 1), 200);
        admit(&mgr, std_req("c", 2), 300);
        be.fail_at(FakeFailPoint::WriteQuotaAny);
        let res = mgr.admit_and_attach(std_req("d", 3), 400);
        assert!(matches!(res, Err(CpuManagerError::Unhealthy { .. })));
        let snap = mgr.snapshot().unwrap();
        assert!(matches!(snap.health, CpuManagerHealth::Unhealthy { .. }));
        // Further admissions are refused while unhealthy.
        be.clear_fail();
        let after = mgr.admit_and_attach(std_req("e", 4), 500);
        assert!(matches!(after, Err(CpuManagerError::Unhealthy { .. })));
    }

    #[test]
    fn releasing_unknown_lease_errs() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be, 8000, 16).unwrap();
        let res = mgr.release_after_teardown(proof("ghost", 9, 999));
        assert!(matches!(res, Err(CpuManagerError::UnknownLease { .. })));
    }

    #[test]
    fn release_proof_with_wrong_slot_is_rejected() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 8000, 16).unwrap();
        admit(&mgr, std_req("a", 0), 100);
        be.simulate_process_exit(0, 100);
        // Proof claims slot 3, but 'a' is recorded on slot 0.
        let res = mgr.release_after_teardown(proof("a", 3, 100));
        assert!(matches!(res, Err(CpuManagerError::ProofMismatch { .. })));
        // Nothing released: 'a' still holds its slot.
        assert_eq!(be.quota_of(0), Some(2000));
        assert_eq!(mgr.snapshot().unwrap().applied.len(), 1);
    }

    #[test]
    fn release_proof_with_wrong_pid_is_rejected() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 8000, 16).unwrap();
        admit(&mgr, std_req("a", 0), 100);
        be.simulate_process_exit(0, 100);
        // Correct lease + slot, WRONG pid.
        let res = mgr.release_after_teardown(proof("a", 0, 999));
        assert!(matches!(res, Err(CpuManagerError::ProofMismatch { .. })));
        assert_eq!(be.quota_of(0), Some(2000), "not released");
    }

    #[test]
    fn stale_proof_cannot_release_reused_slot() {
        // A slot is used by pid 100, released, then reused by pid 200. A stale
        // proof carrying pid 100 must not release the new occupant.
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 8000, 16).unwrap();
        admit(&mgr, std_req("a", 0), 100);
        teardown_and_release(&mgr, &be, "a", 0, 100).expect("first release");
        // Reuse slot 0 for a new session 'b' with pid 200.
        admit(&mgr, std_req("b", 0), 200);
        // Stale proof: lease 'b' is on slot 0, but with pid 100 (the old one).
        let res = mgr.release_after_teardown(proof("b", 0, 100));
        assert!(matches!(res, Err(CpuManagerError::ProofMismatch { .. })));
        assert_eq!(be.quota_of(0), Some(2000), "new occupant not released");
    }

    #[test]
    fn admit_rejects_preexisting_pid_before_quota_changes() {
        // A candidate slot that already holds a process (ensure_slot reused a
        // non-empty cgroup) must be refused BEFORE any existing session's quota
        // is touched.
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 6000, 16).unwrap();
        admit(&mgr, std_req("a", 0), 100); // slot 0 → 2000m
        be.seed_slot_pids(1, vec![7777]); // slot 1 already occupied
        let res = mgr.admit_and_attach(std_req("b", 1), 200);
        assert!(matches!(res, Err(CpuManagerError::Unhealthy { .. })));
        // Existing session 'a' quota UNCHANGED (2000, not lowered toward 1500).
        assert_eq!(be.quota_of(0), Some(2000), "existing quota untouched");
    }

    #[test]
    fn admit_pid_read_failure_does_not_rebalance_existing_sessions() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 6000, 16).unwrap();
        admit(&mgr, std_req("a", 0), 100);
        be.fail_at(FakeFailPoint::SlotPidsRead);
        let res = mgr.admit_and_attach(std_req("b", 1), 200);
        assert!(matches!(res, Err(CpuManagerError::Unhealthy { .. })));
        assert_eq!(be.quota_of(0), Some(2000), "existing quota untouched");
    }

    #[test]
    fn survivor_rebalance_failure_still_releases_slot() {
        // Post-reclaim, the survivor raise fails but rolls back cleanly: the
        // release SUCCEEDS (slot reclaimed) with a RolledBack rebalance, and the
        // manager stays Healthy.
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 6000, 16).unwrap();
        for i in 0..4 {
            admit(&mgr, std_req(&format!("s{i}"), i), 100 + i as u32);
        }
        be.simulate_process_exit(3, 103);
        be.fail_at(FakeFailPoint::WriteQuotaIncrease); // survivor raise fails
        let outcome = mgr
            .release_after_teardown(proof("s3", 3, 103))
            .expect("release still succeeds");
        assert!(matches!(
            outcome.rebalance,
            RebalanceOutcome::RolledBack { .. }
        ));
        // Slot 3 IS gone (reclaimed); survivors stayed at their prior 1500m.
        assert_eq!(
            be.quota_of(3),
            None,
            "slot reclaimed despite rebalance fail"
        );
        assert!(matches!(
            mgr.snapshot().unwrap().health,
            CpuManagerHealth::Healthy
        ));
    }

    #[test]
    fn release_with_lingering_pid_keeps_reservation_and_goes_unhealthy() {
        // The VM did NOT actually exit — a pid lingers in the slot cgroup. That
        // budget must not flow to survivors, so release fails closed.
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 6000, 16).unwrap();
        admit(&mgr, std_req("a", 0), 100);
        admit(&mgr, std_req("b", 1), 200); // both 2000m (budget 6000)
        // Do NOT simulate exit for 'b': its pid 200 still occupies slot 1.
        let res = mgr.release_after_teardown(proof("b", 1, 200));
        assert!(matches!(res, Err(CpuManagerError::SlotNotReclaimed { .. })));
        // Survivor 'a' was NOT raised; 'b' still recorded; manager unhealthy.
        assert_eq!(be.quota_of(0), Some(2000), "survivor not raised");
        let snap = mgr.snapshot().unwrap();
        assert!(matches!(snap.health, CpuManagerHealth::Unhealthy { .. }));
    }

    #[test]
    fn release_slot_remove_failure_keeps_reservation() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 6000, 16).unwrap();
        admit(&mgr, std_req("a", 0), 100);
        admit(&mgr, std_req("b", 1), 200);
        be.simulate_process_exit(1, 200); // process is gone…
        be.fail_at(FakeFailPoint::RemoveSlot); // …but the cgroup won't remove.
        let res = mgr.release_after_teardown(proof("b", 1, 200));
        assert!(matches!(res, Err(CpuManagerError::SlotNotReclaimed { .. })));
        assert_eq!(be.quota_of(0), Some(2000), "survivor not raised");
    }

    #[test]
    fn release_slot_pid_read_failure_keeps_reservation() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 6000, 16).unwrap();
        admit(&mgr, std_req("a", 0), 100);
        admit(&mgr, std_req("b", 1), 200);
        be.simulate_process_exit(1, 200);
        be.fail_at(FakeFailPoint::SlotPidsRead);
        let res = mgr.release_after_teardown(proof("b", 1, 200));
        assert!(matches!(res, Err(CpuManagerError::SlotNotReclaimed { .. })));
        assert_eq!(be.quota_of(0), Some(2000), "survivor not raised");
    }

    /// Drive the manager Unhealthy while keeping live sessions, by failing a
    /// later admission's rollback (WriteQuotaAny fails both apply and restore).
    fn force_unhealthy_with_live_sessions(
        mgr: &CpuEntitlementManager,
        be: &Arc<FakeCgroupBackend>,
    ) {
        be.fail_at(FakeFailPoint::WriteQuotaAny);
        let res = mgr.admit_and_attach(std_req("poison", 5), 999);
        assert!(res.is_err());
        be.clear_fail();
        assert!(matches!(mgr.health(), CpuManagerHealth::Unhealthy { .. }));
    }

    #[test]
    fn unhealthy_release_still_reclaims_but_skips_survivor_raise() {
        // Unhealthy must NOT refuse releases (that would leak slots/cgroups
        // forever). It reclaims + frees the reservation, and skips the survivor
        // raise.
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 6000, 16).unwrap();
        for i in 0..3 {
            admit(&mgr, std_req(&format!("s{i}"), i), 100 + i as u32);
        }
        force_unhealthy_with_live_sessions(&mgr, &be);
        be.simulate_process_exit(2, 102);
        let outcome = mgr
            .release_after_teardown(proof("s2", 2, 102))
            .expect("release processed while unhealthy");
        assert!(matches!(
            outcome.rebalance,
            RebalanceOutcome::SkippedUnhealthy { .. }
        ));
        assert_eq!(be.quota_of(2), None, "slot reclaimed");
        // Survivors keep their prior quotas — no raise attempted.
        assert_eq!(be.quota_of(0), Some(2000));
        assert_eq!(be.quota_of(1), Some(2000));
        // A NEW admission is still refused…
        assert!(matches!(
            mgr.admit_and_attach(std_req("new", 3), 300),
            Err(CpuManagerError::Unhealthy { .. })
        ));
        // …and snapshot still answers.
        let snap = mgr.snapshot().unwrap();
        assert_eq!(snap.applied.len(), 2);
    }

    #[test]
    fn shutdown_acks_and_later_requests_fail() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 8000, 16).unwrap();
        admit(&mgr, std_req("a", 0), 100);
        mgr.shutdown().expect("shutdown acked");
        assert!(matches!(
            mgr.admit_and_attach(std_req("b", 1), 200),
            Err(CpuManagerError::ManagerStopped { .. })
        ));
    }

    #[test]
    fn health_and_generation_mirrors_track_actor_state() {
        let be = Arc::new(FakeCgroupBackend::new());
        let mgr = CpuEntitlementManager::start(be.clone(), 8000, 16).unwrap();
        assert!(matches!(mgr.health(), CpuManagerHealth::Healthy));
        assert_eq!(mgr.allocation_generation(), 0);
        admit(&mgr, std_req("a", 0), 100);
        // Mirror is published before the admit reply → visible right after.
        assert_eq!(mgr.allocation_generation(), 1);
        force_unhealthy_with_live_sessions(&mgr, &be);
        assert!(matches!(mgr.health(), CpuManagerHealth::Unhealthy { .. }));
    }

    /// A backend whose ensure_slot parks until released, letting a test hold
    /// the actor busy while the command queue fills behind it.
    #[derive(Debug)]
    struct GatedBackend {
        inner: FakeCgroupBackend,
        entered: SyncSender<()>,
        gate: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl CpuCgroupBackend for GatedBackend {
        fn preflight(
            &self,
        ) -> Result<super::super::runner_cgroup::CgroupCapabilities, CgroupError> {
            self.inner.preflight()
        }
        fn ensure_slot(&self, slot_index: usize) -> Result<(), CgroupError> {
            let _ = self.entered.send(());
            let _ = self.gate.lock().unwrap().recv(); // park until the gate opens
            self.inner.ensure_slot(slot_index)
        }
        fn read_quota_millis(&self, slot_index: usize) -> Result<Option<u32>, CgroupError> {
            self.inner.read_quota_millis(slot_index)
        }
        fn write_quota_millis(&self, slot_index: usize, q: u32) -> Result<(), CgroupError> {
            self.inner.write_quota_millis(slot_index, q)
        }
        fn attach_pid(&self, slot_index: usize, pid: u32) -> Result<(), CgroupError> {
            self.inner.attach_pid(slot_index, pid)
        }
        fn slot_pids(&self, slot_index: usize) -> Result<Vec<u32>, CgroupError> {
            self.inner.slot_pids(slot_index)
        }
        fn remove_slot(&self, slot_index: usize) -> Result<(), CgroupError> {
            self.inner.remove_slot(slot_index)
        }
    }

    #[test]
    fn full_queue_refuses_admission_as_manager_busy() {
        let (entered_tx, entered_rx) = sync_channel(4);
        let (gate_tx, gate_rx) = sync_channel::<()>(4);
        let be = Arc::new(GatedBackend {
            inner: FakeCgroupBackend::new(),
            entered: entered_tx,
            gate: std::sync::Mutex::new(gate_rx),
        });
        // Capacity 1. Deterministic, sleep-free sequence:
        //   1. t1's admit is DEQUEUED and parks the actor in ensure_slot
        //      (confirmed via `entered`), leaving the queue empty;
        //   2. main fills the one queue slot with a probe command whose reply
        //      receiver is dropped (the actor's later reply send is ignored);
        //   3. the next admission must be refused up-front as ManagerBusy.
        let mgr = CpuEntitlementManager::start(be.clone(), 8000, 1).unwrap();
        let m1 = mgr.clone();
        let t1 = std::thread::spawn(move || m1.admit_and_attach(std_req("a", 0), 100));
        entered_rx.recv().expect("actor entered ensure_slot"); // actor busy now
        mgr.inner
            .tx
            .try_send(Command::Snapshot {
                reply: sync_channel(1).0,
            })
            .expect("probe fills the single queue slot");
        assert!(matches!(
            mgr.admit_and_attach(std_req("c", 2), 300),
            Err(CpuManagerError::ManagerBusy)
        ));
        // Open the gate; t1 completes; the probe is drained harmlessly.
        gate_tx.send(()).unwrap();
        t1.join().unwrap().expect("first admit");
    }
}
