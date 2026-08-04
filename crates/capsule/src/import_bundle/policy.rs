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
///
/// # These are importer policy, never format limits
///
/// Every `max_source_*` field below is **this importer's** choice, not something
/// the v3 format says about a bundle. A bundle that exceeds one of them is a
/// perfectly valid v3 bundle that this importer declined to process: the outcome
/// is [`CapsuleImportError::ResourceBudgetExceeded`] (or
/// [`CapsuleImportError::InsufficientLocalStorage`]), and **never**
/// [`CapsuleImportError::CapsuleInvalid`]. The distinction is load-bearing: only
/// `capsule_invalid` means the artifact is permanently bad, so collapsing the two
/// would make a bundle that merely needs a bigger worker look like a corrupt one.
///
/// Leaving them all `None` means the format imposes no source-size limit of its
/// own — which is exactly what RFC §"Resource policy (not a format limit)"
/// requires. (The *existing* `program_source_projection` SSOT still applies its
/// own fixed production caps further down; see
/// [`super::source_policy`] for how those are kept from masquerading as format
/// invalidity.)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapsuleImportPolicy {
    /// Ceiling on cumulative bytes this import may write to temporary storage.
    ///
    /// Cumulative, not peak: bytes stay charged after the directory holding them
    /// is removed. A budget is a "how much work will this cost" bound, and a
    /// peak-tracking version would need every SSOT call to report when its own
    /// transient copies are released.
    pub temporary_storage_budget: Option<u64>,
    /// The caller's measurement of free disk. Staging beyond it is an
    /// [`CapsuleImportError::InsufficientLocalStorage`], not a budget refusal —
    /// the two have different operator remedies.
    pub available_disk_bytes: Option<u64>,
    /// Ceiling on simultaneous in-flight imports in this process.
    pub max_concurrent_imports: Option<u32>,
    /// Ceiling on the compressed size of the `source.tar.zst` member.
    ///
    /// Importer policy, not a format limit — see the type-level note.
    pub max_source_compressed_bytes: Option<u64>,
    /// Ceiling on the source archive's *expanded* size, enforced incrementally
    /// against bytes actually decompressed — never from a declared header size.
    ///
    /// Importer policy, not a format limit — see the type-level note.
    pub max_source_expanded_bytes: Option<u64>,
    /// Ceiling on the number of regular files inside the source archive.
    ///
    /// Importer policy, not a format limit — see the type-level note.
    pub max_source_file_count: Option<u64>,
    /// Ceiling on any single file inside the source archive.
    ///
    /// Importer policy, not a format limit — see the type-level note.
    pub max_source_file_bytes: Option<u64>,
    /// Ceiling on any single **control member** — `index.json`,
    /// `signature.json`, `capsule.toml` — as staged.
    ///
    /// Importer policy, not a format limit: the v3 format sets no bound on a
    /// control member's size, and a bundle over this limit is
    /// [`CapsuleImportError::ResourceBudgetExceeded`], never
    /// [`CapsuleImportError::CapsuleInvalid`].
    ///
    /// It exists because those three members are the only ones read *whole* into
    /// memory — they are parsed, digested, and signed byte-for-byte, so there is
    /// no streaming form of "parse this JSON". Without a bound, an oversized
    /// control member is an unbounded allocation driven by untrusted input, which
    /// is a denial-of-service surface entirely separate from the question of
    /// whether the bundle is well-formed. The limit is therefore checked against
    /// bytes **as they are staged**, before the full read is ever attempted (see
    /// [`super::reader::stage_v3_outer_members`]).
    ///
    /// `source.tar.zst` is deliberately **not** covered: it is never read whole,
    /// and it has its own dedicated `max_source_*` fields above.
    pub max_control_member_bytes: Option<u64>,
}

impl CapsuleImportPolicy {
    /// A policy that bounds nothing. Useful for tests and for callers that
    /// enforce their limits at a different layer.
    #[must_use]
    pub fn unbounded() -> Self {
        Self::default()
    }

    /// Whether any dimension this module can measure is actually bounded.
    ///
    /// The source-archive pre-scan (see [`super::source_policy`]) costs one
    /// extra streaming decompression pass, so a policy that bounds nothing skips
    /// it entirely: there would be no limit for the measurement to serve.
    /// `max_concurrent_imports` is excluded deliberately — it is settled before
    /// any disk is touched and needs no measurement. So is
    /// `max_control_member_bytes`: it is enforced from the outer staging loop's
    /// own byte counter, which runs regardless, and buying a whole extra
    /// decompression pass over the *source* archive to serve a limit that never
    /// applies to it would be pure waste.
    pub(crate) fn bounds_measurable_resources(&self) -> bool {
        self.temporary_storage_budget.is_some()
            || self.available_disk_bytes.is_some()
            || self.max_source_compressed_bytes.is_some()
            || self.max_source_expanded_bytes.is_some()
            || self.max_source_file_count.is_some()
            || self.max_source_file_bytes.is_some()
    }

    /// Refuse a source archive whose compressed member exceeds this policy.
    ///
    /// # Errors
    ///
    /// [`CapsuleImportError::ResourceBudgetExceeded`] — a policy refusal, never a
    /// statement about the bundle's validity.
    pub(crate) fn check_source_compressed_bytes(
        &self,
        compressed_bytes: u64,
    ) -> Result<(), CapsuleImportError> {
        if let Some(limit) = self.max_source_compressed_bytes
            && compressed_bytes > limit
        {
            return Err(CapsuleImportError::ResourceBudgetExceeded(format!(
                "the bundle's source archive is {compressed_bytes} compressed bytes, over this \
                 importer's {limit}-byte source-archive policy limit; the bundle itself is not \
                 malformed"
            )));
        }
        Ok(())
    }

    /// Refuse a control member that has already staged more bytes than policy
    /// allows.
    ///
    /// Called with the running byte count *during* staging, so the refusal lands
    /// before the member is fully written and long before anything reads it whole
    /// — which is the entire point of the limit.
    ///
    /// # Errors
    ///
    /// [`CapsuleImportError::ResourceBudgetExceeded`] — a policy refusal, never a
    /// statement about the bundle's validity.
    pub(crate) fn check_control_member_bytes(
        &self,
        member: &str,
        staged_bytes: u64,
    ) -> Result<(), CapsuleImportError> {
        if let Some(limit) = self.max_control_member_bytes
            && staged_bytes > limit
        {
            return Err(CapsuleImportError::ResourceBudgetExceeded(format!(
                "the bundle's {member} member is over this importer's {limit}-byte \
                 control-member policy limit; the v3 format sets no size limit on a control \
                 member, so the bundle itself is not malformed"
            )));
        }
        Ok(())
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
