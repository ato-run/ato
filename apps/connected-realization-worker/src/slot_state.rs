//! A host-local slot stays unavailable while prior lease state is unresolved.

use std::fs::{self, File, OpenOptions};
use std::path::Path;

use anyhow::{Context, Result, ensure};

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
        ensure!(
            fs::read_dir(&leases)?.next().is_none(),
            "worker slot has unresolved lease state; preserve it and verify physical cleanup before recovery"
        );
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
    fn restart_with_unresolved_state_cannot_claim_new_work() {
        let root = tempfile::tempdir().unwrap();
        let first = SlotGuard::acquire(root.path()).unwrap();
        let lease = root.path().join("leases/lease-1");
        fs::create_dir(&lease).unwrap();
        fs::write(lease.join("state"), b"uncommitted").unwrap();
        drop(first);
        assert!(SlotGuard::acquire(root.path()).is_err());
        assert_eq!(fs::read(lease.join("state")).unwrap(), b"uncommitted");
    }
}
