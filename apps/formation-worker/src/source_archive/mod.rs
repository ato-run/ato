//! Source-only physical materialization. This module never invokes a planner,
//! build, package manager, application, Contract seal, or Schema registration.
use anyhow::{Context, Result, ensure};
use ato_formation::source::{
    FileVerifiedArchive, RESOLVER_CONTRACT_V1, RESOLVER_CONTRACT_V2, SourceLimits,
};
use ato_ipc::formation::{FormationJobV1, RequestedOutputV1};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    time::{Duration, Instant},
};

use crate::job::SourceFetcher;

mod http;
pub use http::{SourceArchiveApi, SourceArchiveClaimOutcome};
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveDescriptor {
    pub archive_format_version: String,
    pub source_archive_digest: String,
    pub size_bytes: u64,
    pub source_tree_digest: String,
    pub resolver_contract_version: String,
    pub subdirectory: String,
    pub source_closure_ref: String,
    pub observed_file_count: u64,
    pub observed_uncompressed_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub job_id: String,
    pub attempt_id: String,
    pub attempt_fence: u64,
    pub job_digest: String,
    pub archive_descriptor: ArchiveDescriptor,
}
impl Binding {
    pub fn digest(&self) -> Result<String> {
        Ok(crate::job::digest(&serde_jcs::to_vec(self)?))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaimedSourceArchive {
    pub job_id: String,
    pub attempt_id: String,
    pub attempt_fence: u64,
    pub job_digest: String,
    pub job: FormationJobV1,
}
impl ClaimedSourceArchive {
    pub fn validate(&self) -> Result<()> {
        self.job.validate()?;
        ensure!(
            self.job.requested_outputs == [RequestedOutputV1::SourceArchive],
            "source_only_output_required"
        );
        for id in [&self.job_id, &self.attempt_id] {
            ensure!(
                !id.is_empty()
                    && id.len() <= 160
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
                "source_only_assignment_invalid"
            );
        }
        ensure!(
            self.attempt_fence > 0
                && self.job_id == self.job.job_id
                && self.job_digest == self.job.canonical_digest()?,
            "source_only_assignment_mismatch"
        );
        Ok(())
    }
}

/// The distinct terminal physical output. There is deliberately no Schema,
/// ContractRef, seal verdict or `fully_satisfied` member here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceArchiveOutput {
    pub kind: String,
    pub status: String,
    pub binding_digest: String,
    pub source_revision_id: String,
    pub source_materialization_id: String,
    pub job_id: String,
    pub attempt_id: String,
    pub attempt_fence: u64,
    pub job_digest: String,
    pub archive_descriptor: ArchiveDescriptor,
}
impl SourceArchiveOutput {
    pub(crate) fn validate(&self, binding: &Binding) -> Result<()> {
        let actual = Binding {
            job_id: self.job_id.clone(),
            attempt_id: self.attempt_id.clone(),
            attempt_fence: self.attempt_fence,
            job_digest: self.job_digest.clone(),
            archive_descriptor: self.archive_descriptor.clone(),
        };
        ensure!(
            self.kind == "source_archive"
                && self.status == "source_materialized"
                && actual == *binding
                && self.binding_digest == binding.digest()?,
            "source_only_output_assignment_mismatch"
        );
        for id in [&self.source_revision_id, &self.source_materialization_id] {
            ensure!(
                !id.is_empty()
                    && id.len() <= 160
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
                "source_only_output_invalid"
            );
        }
        Ok(())
    }
}

/// One claimed source-only request. All filesystem outputs are private owned
/// scratch and are closed before the API can accept the physical output.
pub fn materialize_source_archive(
    api: &SourceArchiveApi,
    fetcher: &dyn SourceFetcher,
    claim: &ClaimedSourceArchive,
    work_root: &Path,
) -> Result<SourceArchiveOutput> {
    claim.validate()?;
    let deadline = Instant::now() + Duration::from_secs(15 * 60);
    let limits = SourceLimits::default();
    let source = serde_json::to_value(&claim.job.source)?;
    let subdirectory = source["subdirectory"].as_str().unwrap_or("");
    let (archive, acquired_digest) = fetcher.fetch_file(
        &claim.job_id,
        &source,
        work_root,
        crate::runtime_network::MAX_SOURCE_OBJECT_BYTES,
        deadline,
    )?;
    let size = archive.metadata()?.len();
    ensure!(
        size <= crate::runtime_network::MAX_SOURCE_OBJECT_BYTES,
        "source_only_archive_size_exceeded"
    );
    let mut verified =
        FileVerifiedArchive::verify_with_guard(archive, &acquired_digest, size, limits, &|| {
            Instant::now() < deadline
        })?;
    ensure!(
        [RESOLVER_CONTRACT_V1, RESOLVER_CONTRACT_V2].contains(&verified.resolver_contract()),
        "source_only_resolver_unsupported"
    );
    let binding = Binding {
        job_id: claim.job_id.clone(),
        attempt_id: claim.attempt_id.clone(),
        attempt_fence: claim.attempt_fence,
        job_digest: claim.job_digest.clone(),
        archive_descriptor: ArchiveDescriptor {
            archive_format_version: verified.archive_format().to_owned(),
            source_archive_digest: verified.archive_digest().to_owned(),
            size_bytes: verified.archive_bytes(),
            source_tree_digest: verified.tree_digest().to_owned(),
            resolver_contract_version: verified.resolver_contract().to_owned(),
            subdirectory: subdirectory.to_owned(),
            source_closure_ref: verified.closure_ref(subdirectory)?.to_string(),
            observed_file_count: verified.expanded_file_count() as u64,
            observed_uncompressed_bytes: verified.expanded_bytes(),
        },
    };
    // Selection uses the same authority as application intake. No program or
    // package script is run; the private tree is removed before transfer.
    let temporary = work_root.join(".tmp");
    ensure!(
        fs2::available_space(&temporary)? >= verified.expanded_bytes().saturating_add(size),
        "source_only_scratch_capacity_exceeded"
    );
    let selection = tempfile::tempdir_in(&temporary)?;
    let selected = verified.materialize_with_guard(selection.path(), subdirectory, limits, &|| {
        Instant::now() < deadline
    });
    let cleanup = selection.close();
    cleanup.context("source_only_local_cleanup_unconfirmed")?;
    selected?;

    let ticket = api.authorize(&binding, deadline)?;
    let transfer = api.transfer_and_verify(&ticket, &binding, &verified, work_root, deadline);
    drop(verified);
    if let Err(error) = transfer {
        api.abort(&ticket, &binding, deadline)
            .context("source_only_storage_cleanup_unconfirmed")?;
        return Err(error);
    }
    // A lost completion response is not fabricated success or permission to
    // delete a shared content-addressed object. The server retains the exact
    // ticket for idempotent status/recovery; abort only targets owned staging.
    match api
        .complete(&ticket, &binding, deadline)
        .and_then(|output| {
            output.validate(&binding)?;
            Ok(output)
        }) {
        Ok(output) => Ok(output),
        Err(error) => {
            api.abort(&ticket, &binding, deadline)
                .context("source_only_storage_cleanup_unconfirmed")?;
            Err(error)
        }
    }
}
