use serde::Serialize;

use crate::ato_lock::schema::{AtoLock, ContractSection, ResolutionSection};

// Canonical lock identity intentionally excludes mutable and validation-only sections.
// In v1, only schema_version + resolution + contract contribute to lock_id.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CanonicalLockProjection<'a> {
    pub schema_version: u32,
    pub resolution: &'a ResolutionSection,
    pub contract: &'a ContractSection,
}

pub fn canonical_projection(lock: &AtoLock) -> CanonicalLockProjection<'_> {
    CanonicalLockProjection {
        schema_version: lock.schema_version,
        resolution: &lock.resolution,
        contract: &lock.contract,
    }
}
