use serde::Serialize;

use crate::capsule_lock::closure::normalize_resolution_closure_entries;
use crate::capsule_lock::schema::{
    CapsuleLock, ContractSection, LockLaunchSection, ResolutionSection,
};
use crate::capsule_program_contract::CapsuleProgramEnvelopeV1;
use crate::error::Result;
use crate::execution_contract::ExecutionContractEnvelopeV1;

// Canonical lock identity intentionally excludes mutable and validation-only sections.
// In v1, only schema_version + resolution + contract contribute to lock_id.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CanonicalLockProjection {
    pub schema_version: u32,
    pub resolution: ResolutionSection,
    pub contract: ContractSection,
}

/// The canonical bytes a standard lock **signature** must cover.
///
/// This is a STRICT SUPERSET of the [`CanonicalLockProjection`] used for
/// `lock_id`: it is the identity projection ∪ the additive sections
/// (`execution_contract` (D2), `launch` (D5), and the ADR-014
/// `program_identity` envelope). `lock_id` stays byte-stable (only the
/// identity projection feeds it), while the signature also binds the
/// execution contract, persisted launch environment, and Capsule Program
/// identity, closing the hole where an attacker could swap any of them for
/// another self-consistent value while `lock_id` AND the signature both
/// stayed valid.
///
/// Back-compat: every additive field is `skip_serializing_if = Option::is_none`,
/// so a lock that never carried them serializes to bytes byte-identical to the
/// identity projection — an already-signed legacy lock (no `execution_contract`,
/// no `launch`, no `program_identity`) verifies over exactly the same bytes as
/// before each split; in particular a `program_identity`-free lock signs over
/// bytes byte-identical to the pre-ADR-014 signature payload.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CanonicalSignatureProjection {
    pub schema_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_contract: Option<ExecutionContractEnvelopeV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch: Option<LockLaunchSection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program_identity: Option<CapsuleProgramEnvelopeV1>,
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
    // D5 / D2 / ADR-014 — additive lock sections that MUST NOT change lock
    // identity. They are excluded by construction (absent from
    // `CanonicalLockProjection`); listing them here keeps the introspection
    // helpers in sync.
    "launch",
    "execution_contract",
    "program_identity",
];

pub fn canonical_projection(lock: &CapsuleLock) -> Result<CanonicalLockProjection> {
    let mut resolution = lock.resolution.clone();
    normalize_resolution_closure_entries(&mut resolution.entries)?;

    Ok(CanonicalLockProjection {
        schema_version: lock.schema_version,
        resolution,
        contract: lock.contract.clone(),
    })
}

/// Returns the v1 canonical identity projection that feeds `lock_id`.
pub fn canonical_identity_projection(lock: &CapsuleLock) -> Result<CanonicalLockProjection> {
    canonical_projection(lock)
}

/// Returns the v1 canonical **signature** projection: the identity projection
/// plus the additive sections (`execution_contract`, `launch`,
/// `program_identity`). When all are absent it is byte-identical to the
/// identity projection.
pub fn canonical_signature_projection(lock: &CapsuleLock) -> Result<CanonicalSignatureProjection> {
    let identity = canonical_projection(lock)?;
    Ok(CanonicalSignatureProjection {
        schema_version: identity.schema_version,
        execution_contract: lock.execution_contract.clone(),
        launch: lock.launch.clone(),
        program_identity: lock.program_identity.clone(),
        resolution: identity.resolution,
        contract: identity.contract,
    })
}

pub fn is_canonical_identity_section(section: &str) -> bool {
    CANONICAL_IDENTITY_INCLUDED_SECTIONS.contains(&section)
}
