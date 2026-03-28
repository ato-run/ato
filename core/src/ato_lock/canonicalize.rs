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

pub const CANONICAL_IDENTITY_INCLUDED_SECTIONS: &[&str] =
    &["schema_version", "resolution", "contract"];
pub const CANONICAL_IDENTITY_EXCLUDED_SECTIONS: &[&str] = &[
    "generated_at",
    "features",
    "binding",
    "policy",
    "attestations",
    "signatures",
];

pub fn canonical_projection(lock: &AtoLock) -> Result<CanonicalLockProjection> {
    let mut resolution = lock.resolution.clone();
    normalize_resolution_closure_entries(&mut resolution.entries)?;

    Ok(CanonicalLockProjection {
        schema_version: lock.schema_version,
        resolution,
        contract: lock.contract.clone(),
    })
}

/// Returns the v1 canonical identity projection used by both `lock_id` and
/// standard lock signatures.
pub fn canonical_identity_projection(lock: &AtoLock) -> Result<CanonicalLockProjection> {
    canonical_projection(lock)
}

pub fn is_canonical_identity_section(section: &str) -> bool {
    CANONICAL_IDENTITY_INCLUDED_SECTIONS.contains(&section)
}
