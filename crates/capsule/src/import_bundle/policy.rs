//! The importer's resource policy.
//!
//! RFC §"Resource policy (not a format limit)": the *format* defines no upper
//! bound on bundle size, member size, expanded size, or member count. An
//! importer applies its own budget instead, and exceeding it is **not** a
//! malformed bundle — it is a distinct, retryable outcome
//! ([`CapsuleImportError::ResourceBudgetExceeded`] /
//! [`CapsuleImportError::InsufficientLocalStorage`]), never
//! [`CapsuleImportError::CapsuleInvalid`].
//!
//! Everything is enforced **incrementally, as bytes are processed**, and never
//! from a declared `size_bytes`. A bundle that lies about its own size is caught
//! by the digest/size mismatch check (`capsule_invalid`), not by an allocation
//! failure.

use std::sync::atomic::{AtomicU32, Ordering};

use super::CapsuleImportError;

/// An importer's implementation-defined budget. All fields are optional; `None`
/// means "this importer does not bound that dimension".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapsuleImportPolicy {
    /// Ceiling on bytes this import may stage in temporary storage.
    pub temporary_storage_budget: Option<u64>,
    /// The caller's measurement of free disk. Staging beyond it is an
    /// [`CapsuleImportError::InsufficientLocalStorage`], not a budget refusal —
    /// the two have different operator remedies.
    pub available_disk_bytes: Option<u64>,
    /// Ceiling on simultaneous in-flight imports in this process.
    pub max_concurrent_imports: Option<u32>,
}

impl CapsuleImportPolicy {
    /// A policy that bounds nothing. Useful for tests and for callers that
    /// enforce their limits at a different layer.
    #[must_use]
    pub fn unbounded() -> Self {
        Self::default()
    }

    /// Charge `additional` newly staged bytes against a running total.
    ///
    /// # Errors
    ///
    /// [`CapsuleImportError::ResourceBudgetExceeded`] or
    /// [`CapsuleImportError::InsufficientLocalStorage`] — deliberately distinct
    /// categories, and neither is a statement about the bundle's validity.
    pub(crate) fn charge_staged_bytes(
        &self,
        staged_total: &mut u64,
        additional: u64,
    ) -> Result<(), CapsuleImportError> {
        *staged_total = staged_total.saturating_add(additional);
        if let Some(budget) = self.temporary_storage_budget
            && *staged_total > budget
        {
            return Err(CapsuleImportError::ResourceBudgetExceeded(format!(
                "staging {staged_total} bytes exceeds this importer's \
                 {budget}-byte temporary storage budget"
            )));
        }
        if let Some(available) = self.available_disk_bytes
            && *staged_total > available
        {
            return Err(CapsuleImportError::InsufficientLocalStorage(format!(
                "staging {staged_total} bytes exceeds the {available} bytes reported free \
                 on this device"
            )));
        }
        Ok(())
    }

    /// Take one of this process's import slots, if the policy bounds them.
    ///
    /// # Errors
    ///
    /// [`CapsuleImportError::ResourceBudgetExceeded`] when the slots are full.
    pub(crate) fn acquire_import_slot(&self) -> Result<ImportSlot, CapsuleImportError> {
        let Some(limit) = self.max_concurrent_imports else {
            return Ok(ImportSlot { held: false });
        };
        // Compare-and-swap rather than fetch_add-then-check: an over-limit
        // fetch_add is briefly visible to a concurrent caller, which would make
        // the limit off-by-N under contention.
        let mut current = ACTIVE_IMPORTS.load(Ordering::Acquire);
        loop {
            if current >= limit {
                return Err(CapsuleImportError::ResourceBudgetExceeded(format!(
                    "{current} capsule imports are already in flight, at this importer's \
                     limit of {limit}"
                )));
            }
            match ACTIVE_IMPORTS.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(ImportSlot { held: true }),
                Err(observed) => current = observed,
            }
        }
    }
}

static ACTIVE_IMPORTS: AtomicU32 = AtomicU32::new(0);

/// An in-flight import's claim on a concurrency slot, released on drop.
///
/// It is held by the envelope rather than by the verify call, so the slot covers
/// the whole staging lifetime — the disk the import is actually occupying — not
/// just the parse.
#[derive(Debug)]
pub(crate) struct ImportSlot {
    held: bool,
}

impl Drop for ImportSlot {
    fn drop(&mut self) {
        if self.held {
            ACTIVE_IMPORTS.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_and_disk_are_distinct_categories() {
        let budget = CapsuleImportPolicy {
            temporary_storage_budget: Some(10),
            ..CapsuleImportPolicy::default()
        };
        let mut total = 0;
        assert!(matches!(
            budget.charge_staged_bytes(&mut total, 11),
            Err(CapsuleImportError::ResourceBudgetExceeded(_))
        ));

        let disk = CapsuleImportPolicy {
            available_disk_bytes: Some(10),
            ..CapsuleImportPolicy::default()
        };
        let mut total = 0;
        assert!(matches!(
            disk.charge_staged_bytes(&mut total, 11),
            Err(CapsuleImportError::InsufficientLocalStorage(_))
        ));
    }

    #[test]
    fn charges_accumulate_across_members() {
        let policy = CapsuleImportPolicy {
            temporary_storage_budget: Some(10),
            ..CapsuleImportPolicy::default()
        };
        let mut total = 0;
        policy
            .charge_staged_bytes(&mut total, 6)
            .expect("first fits");
        assert!(policy.charge_staged_bytes(&mut total, 6).is_err());
    }

    #[test]
    fn slots_are_released_on_drop() {
        let policy = CapsuleImportPolicy {
            max_concurrent_imports: Some(1),
            ..CapsuleImportPolicy::default()
        };
        {
            let _slot = policy.acquire_import_slot().expect("first slot");
            assert!(policy.acquire_import_slot().is_err());
        }
        let _slot = policy.acquire_import_slot().expect("released on drop");
    }
}
