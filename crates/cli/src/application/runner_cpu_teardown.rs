//! Teardown proof minting for CPU entitlement release (ADR-016 PR2b).
//!
//! This module is the ONLY place a [`TeardownConfirmed`] can be constructed:
//! its fields are private and the sole constructor lives here, next to the
//! runner's confirmed-teardown seam. The manager (`runner_cpu_manager`) merely
//! CONSUMES the proof and re-verifies its identity fields against the recorded
//! entitlement. Call [`confirm_vm_teardown`] only after the VM's process exit
//! has actually been observed — a reap (`wait` returned) or a kill that was
//! confirmed to take (the process is gone) — never speculatively.

#![allow(dead_code)]

/// Observed evidence that a VMM process has exited. Held by value inside the
/// proof so it cannot be built from a bare marker; construct it from a real
/// `wait`/`kill` result at the teardown seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessExitEvidence {
    /// The VMM child was `wait`ed and is gone.
    Reaped,
    /// The VMM was signalled and confirmed no longer present.
    Killed,
}

/// An observation that a specific session's VM has been torn down, carrying the
/// session identity (`lease_id` / `slot_index` / `vmm_pid`) and the evidence.
///
/// `release_after_teardown` requires one and verifies ALL THREE identity fields
/// against the recorded entitlement, so a proof for one session — or a stale
/// proof for a slot that has since been reused by a different pid — can never
/// release the current occupant. Not `Clone`: a capability to release should
/// not be duplicable. Constructible only via [`confirm_vm_teardown`] in this
/// module.
#[derive(Debug)]
pub struct TeardownConfirmed {
    lease_id: String,
    slot_index: usize,
    vmm_pid: u32,
    observed_exit: ProcessExitEvidence,
}

impl TeardownConfirmed {
    pub(crate) fn lease_id(&self) -> &str {
        &self.lease_id
    }
    pub(crate) fn slot_index(&self) -> usize {
        self.slot_index
    }
    pub(crate) fn vmm_pid(&self) -> u32 {
        self.vmm_pid
    }
    pub(crate) fn observed_exit(&self) -> &ProcessExitEvidence {
        &self.observed_exit
    }
}

/// Mint the proof at the runner's confirmed-teardown seam. `evidence` must come
/// from a real process-exit observation, never speculatively: the caller has
/// either reaped the VMM child or confirmed a signalled process is gone.
pub(crate) fn confirm_vm_teardown(
    lease_id: impl Into<String>,
    slot_index: usize,
    vmm_pid: u32,
    evidence: ProcessExitEvidence,
) -> TeardownConfirmed {
    TeardownConfirmed {
        lease_id: lease_id.into(),
        slot_index,
        vmm_pid,
        observed_exit: evidence,
    }
}
