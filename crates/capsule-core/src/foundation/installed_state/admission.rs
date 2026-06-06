//! Storage admission: decide, **before** download/build, whether a capsule's
//! required disk fits in the space left after existing installed claims.
//!
//! The decision is a typed [`StorageAdmission`] so callers handle insufficient
//! space up front instead of failing mid-materialization. The core comparison
//! is the pure, deterministic [`evaluate_storage_admission`]; the I/O-backed
//! free-space probe ([`available_space`]) is separated out so the policy is
//! testable without depending on a machine's actual free space.

use std::path::Path;

use crate::error::{CapsuleError, Result};

/// Result of a storage admission dry-run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageAdmission {
    /// The required storage fits after existing installed claims.
    Admitted {
        required_bytes: u64,
        available_bytes: u64,
        reserved_bytes: u64,
        /// `available_bytes` minus `reserved_bytes` (saturating).
        free_after_claims: u64,
    },
    /// Not enough free space after existing installed claims.
    Rejected {
        required_bytes: u64,
        available_bytes: u64,
        reserved_bytes: u64,
        free_after_claims: u64,
        /// How many more bytes would be needed: `required - free_after_claims`.
        shortfall_bytes: u64,
    },
}

impl StorageAdmission {
    /// Whether the capsule may be admitted (enough storage).
    pub fn is_admitted(&self) -> bool {
        matches!(self, StorageAdmission::Admitted { .. })
    }

    /// Missing bytes when rejected; `0` when admitted.
    pub fn shortfall_bytes(&self) -> u64 {
        match self {
            StorageAdmission::Rejected {
                shortfall_bytes, ..
            } => *shortfall_bytes,
            StorageAdmission::Admitted { .. } => 0,
        }
    }
}

/// Pure storage-admission decision: does `required_bytes` fit in
/// `available_bytes` after `reserved_bytes` already claimed by installed
/// capsules? Deterministic; performs no I/O.
pub fn evaluate_storage_admission(
    required_bytes: u64,
    available_bytes: u64,
    reserved_bytes: u64,
) -> StorageAdmission {
    let free_after_claims = available_bytes.saturating_sub(reserved_bytes);
    if required_bytes <= free_after_claims {
        StorageAdmission::Admitted {
            required_bytes,
            available_bytes,
            reserved_bytes,
            free_after_claims,
        }
    } else {
        StorageAdmission::Rejected {
            required_bytes,
            available_bytes,
            reserved_bytes,
            free_after_claims,
            shortfall_bytes: required_bytes - free_after_claims,
        }
    }
}

/// Available (free) bytes on the filesystem backing `path`. Uses the existing
/// `fs2` dependency — no new crate.
///
/// `path` must exist; probing a not-yet-created path errors. Use
/// [`available_space_for_target`] when the target (e.g. an install dir) may not
/// exist yet.
pub fn available_space(path: impl AsRef<Path>) -> Result<u64> {
    let path = path.as_ref();
    fs2::available_space(path).map_err(|e| {
        CapsuleError::Runtime(format!(
            "failed to read free space at {}: {e}",
            path.display()
        ))
    })
}

/// Free bytes on the volume backing `path`, probing the nearest existing
/// ancestor when `path` itself does not exist yet (an install dir is created
/// during materialization, after admission runs). The probed ancestor is on the
/// same volume the target will be created on in the common case.
pub fn available_space_for_target(path: impl AsRef<Path>) -> Result<u64> {
    let mut current = path.as_ref();
    loop {
        if current.exists() {
            return available_space(current);
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent,
            // No existing ancestor (or reached the root): let `available_space`
            // produce a meaningful error for this path.
            _ => return available_space(current),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn fits_when_required_under_free_after_claims() {
        let decision = evaluate_storage_admission(10 * GB, 100 * GB, 20 * GB);
        assert!(decision.is_admitted());
        assert_eq!(decision.shortfall_bytes(), 0);
        assert_eq!(
            decision,
            StorageAdmission::Admitted {
                required_bytes: 10 * GB,
                available_bytes: 100 * GB,
                reserved_bytes: 20 * GB,
                free_after_claims: 80 * GB,
            }
        );
    }

    #[test]
    fn rejected_when_required_exceeds_available() {
        // 20GB required, only 10GB free, no prior claims → typed Rejected.
        let decision = evaluate_storage_admission(20 * GB, 10 * GB, 0);
        assert!(!decision.is_admitted());
        assert_eq!(decision.shortfall_bytes(), 10 * GB);
        assert!(matches!(decision, StorageAdmission::Rejected { .. }));
    }

    #[test]
    fn existing_claims_reduce_available_space() {
        // 90GB volume, 18.7GB already claimed → only ~71GB left; 20GB fits,
        // but 80GB does not.
        let reserved = 18 * GB;
        assert!(evaluate_storage_admission(20 * GB, 90 * GB, reserved).is_admitted());
        let rejected = evaluate_storage_admission(80 * GB, 90 * GB, reserved);
        assert!(!rejected.is_admitted());
        assert_eq!(rejected.shortfall_bytes(), 80 * GB - (90 * GB - reserved));
    }

    #[test]
    fn boundary_required_equals_free_after_claims_is_admitted() {
        assert!(evaluate_storage_admission(80 * GB, 100 * GB, 20 * GB).is_admitted());
    }

    #[test]
    fn reserved_exceeding_available_saturates_to_zero_free() {
        let decision = evaluate_storage_admission(1, 10 * GB, 50 * GB);
        assert!(!decision.is_admitted());
        match decision {
            StorageAdmission::Rejected {
                free_after_claims,
                shortfall_bytes,
                ..
            } => {
                assert_eq!(free_after_claims, 0);
                assert_eq!(shortfall_bytes, 1);
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[test]
    fn available_space_probes_a_real_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let free = available_space(dir.path()).expect("probe free space");
        assert!(
            free > 0,
            "a writable temp dir should report some free space"
        );
    }
}
