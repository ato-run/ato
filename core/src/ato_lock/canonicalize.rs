use serde::Serialize;

use crate::ato_lock::closure::normalize_resolution_closure_entries;
use crate::ato_lock::schema::{AtoLock, ContractSection, ResolutionSection};
use crate::error::Result;

// Canonical lock identity intentionally excludes mutable and validation-only sections.
// In v1, only schema_version + resolution + contract contribute to lock_id.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CanonicalLockProjection {
    pub schema_version: u32,
    pub resolution: ResolutionSection,
    pub contract: ContractSection,
}

pub fn canonical_projection(lock: &AtoLock) -> Result<CanonicalLockProjection> {
    let mut resolution = lock.resolution.clone();
    normalize_resolution_closure_entries(&mut resolution.entries)?;

    Ok(CanonicalLockProjection {
        schema_version: lock.schema_version,
        resolution,
        contract: lock.contract.clone(),
    })
}
