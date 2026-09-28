//! One worker process per host-local slot.
//!
//! The guard is ONLY the process-exclusion lock: it stops a second worker
//! from sharing one work root. Leftover `leases/` state is a recovery
//! concern, not a lock concern — the worker must still start so recovery can
//! reconcile what a previous incarnation left behind. While reconciliation
//! is unfinished the slot stays in its recovering state and claims no new
//! work; see `runtime_launch::recovery`.

use std::fs::{self, File, OpenOptions};
use std::path::Path;

use anyhow::{Context, Result};

/// The OS releases the process lock on exit. Lease directories survive a crash.
pub struct SlotGuard {
    _lock: File,
}

impl SlotGuard {
    pub fn acquire(work_root: &Path) -> Result<Self> {
        fs::create_dir_all(work_root)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(work_root.join("slot.lock"))?;
        lock.try_lock().context("worker slot is already in use")?;
        let leases = work_root.join("leases");
        fs::create_dir_all(&leases)?;
        Ok(Self { _lock: lock })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_worker_can_hold_the_same_slot() {
        let root = tempfile::tempdir().unwrap();
        let first = SlotGuard::acquire(root.path()).unwrap();
        assert!(SlotGuard::acquire(root.path()).is_err());
        drop(first);
        assert!(SlotGuard::acquire(root.path()).is_ok());
    }

    #[test]
    fn unresolved_state_does_not_block_the_process_lock() {
        // The lock says "one process per slot", nothing more. Leftover lease
        // state is reconciled by recovery AFTER the process is running —
        // refusing to start would strand the only component that can settle it.
        let root = tempfile::tempdir().unwrap();
        let first = SlotGuard::acquire(root.path()).unwrap();
        let lease = root.path().join("leases/lease-1");
        fs::create_dir(&lease).unwrap();
        fs::write(lease.join("state"), b"uncommitted").unwrap();
        drop(first);
        assert!(SlotGuard::acquire(root.path()).is_ok());
        assert_eq!(fs::read(lease.join("state")).unwrap(), b"uncommitted");
    }
}
