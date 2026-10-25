//! Existing Formation result source evidence, produced from verified intake.
//! These are observations, not a new source/Contract identity or a legacy
//! snapshot materialization receipt. Receiver-first rollout is required.
use anyhow::{Result, ensure};
use ato_formation::source::FileVerifiedArchive;
use serde::Serialize;

/// The existing `source_evidence` payload. New producers emit both metrics;
/// receiver schemas keep the pair optional for historical results.
#[derive(Debug, Serialize)]
pub(super) struct FormationSourceEvidenceV1 {
    archive_digest: String,
    tree_digest: String,
    resolver_contract_version: String,
    subdirectory: String,
    observed_file_count: u64,
    observed_uncompressed_bytes: u64,
}

pub(super) fn capture(
    verified: &FileVerifiedArchive,
    subdirectory: &str,
) -> Result<FormationSourceEvidenceV1> {
    verified.closure_ref(subdirectory)?;
    let count = u64::try_from(verified.expanded_file_count())?;
    let bytes = verified.expanded_bytes();
    ensure!(
        count <= 9_007_199_254_740_991 && bytes <= 9_007_199_254_740_991,
        "formation_source_measurements_not_json_safe"
    );
    Ok(FormationSourceEvidenceV1 {
        archive_digest: verified.archive_digest().to_owned(),
        tree_digest: verified.tree_digest().to_owned(),
        // Never rename this as the removed snapshot producer's projection.
        resolver_contract_version: verified.resolver_contract().to_owned(),
        subdirectory: subdirectory.to_owned(),
        observed_file_count: count,
        observed_uncompressed_bytes: bytes,
    })
}

/// Test-only observation payload. Production callers always derive this from a
/// verified archive via [`capture`]; the result-shape tests only need a
/// serializable value.
#[cfg(test)]
pub(super) fn for_test() -> FormationSourceEvidenceV1 {
    FormationSourceEvidenceV1 {
        archive_digest: format!("sha256:{}", "1".repeat(64)),
        tree_digest: format!("sha256:{}", "2".repeat(64)),
        resolver_contract_version: ato_formation::source::RESOLVER_CONTRACT_V1.to_owned(),
        subdirectory: String::new(),
        observed_file_count: 1,
        observed_uncompressed_bytes: 15,
    }
}
