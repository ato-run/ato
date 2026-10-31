//! One measured intake for ordinary Formation and explicit Search callers.
//! The caller owns authorization; this boundary proves actual pinned bytes,
//! full tree, selected closure and finite download/verification limits only.
use super::*;

#[allow(clippy::too_many_arguments)]
pub fn verify_source(
    fetcher: &dyn SourceFetcher,
    job_id: &str,
    source: &serde_json::Value,
    work_root: &Path,
    limits: SourceLimits,
    maximum: u64,
    deadline: Instant,
) -> Result<FileVerifiedArchive> {
    let maximum = maximum
        .min(limits.max_total_bytes)
        .min(crate::runtime_network::MAX_SOURCE_OBJECT_BYTES);
    let (archive, observed_digest) =
        fetcher.fetch_file(job_id, source, work_root, maximum, deadline)?;
    let archive_bytes = archive.metadata()?.len();
    if archive_bytes > maximum {
        return Err(SourceError::LimitExceeded {
            limit: "archive_bytes",
        }
        .into());
    }
    let verified = FileVerifiedArchive::verify_with_guard(
        archive,
        source["expected_archive_digest"]
            .as_str()
            .unwrap_or(&observed_digest),
        archive_bytes,
        limits,
        &|| Instant::now() < deadline,
    )?;
    if let Some(expected) = source["expected_source_tree_digest"].as_str()
        && expected != verified.tree_digest()
    {
        return Err(SourceError::TreeDigestMismatch {
            expected: expected.into(),
            actual: verified.tree_digest().into(),
        }
        .into());
    }
    verified.closure_ref(source["subdirectory"].as_str().unwrap_or(""))?;
    Ok(verified)
}

/// Existing requester entry for an explicit immutable archive. API ownership
/// and accepted source-job provenance must be established by the requester;
/// this function grants neither a normal Formation result nor execution.
pub fn prepare_source_proposal(
    fetcher: &dyn SourceFetcher,
    job: &ato_ipc::formation::FormationJobV1,
    request: crate::runtime_network::SatisfyRequest,
    work_root: &Path,
) -> Result<crate::runtime_network::Submission> {
    job.validate()?;
    let ato_ipc::formation::FormationSourceV1::UploadedArchive(source) = &job.source else {
        bail!("approved_search_requires_immutable_archive");
    };
    anyhow::ensure!(
        source.expected_source_tree_digest.is_some(),
        "approved_search_requires_verified_tree"
    );
    anyhow::ensure!(
        source.subdirectory.is_empty(),
        "approved_search_subdirectory_unsupported"
    );
    anyhow::ensure!(
        job.policy.network == ato_ipc::formation::FormationNetworkPolicyV1::Denied
            && request.policy.network == "denied",
        "approved_search_network_policy_mismatch"
    );
    request.budget.validate()?;
    crate::runtime_network::proposal::frozen_request(&request)?.canonical_bytes()?;
    let mut limits = SourceLimits::default();
    limits.max_total_bytes = limits
        .max_total_bytes
        .min(request.budget.max_expanded_bytes);
    let deadline = Instant::now()
        + Duration::from_secs(request.budget.deadline_seconds)
            .min(crate::runtime_network::SOURCE_TRANSFER_TIMEOUT);
    let verified = verify_source(
        fetcher,
        &job.job_id,
        &serde_json::to_value(&job.source)?,
        work_root,
        limits,
        request.budget.max_transfer_bytes,
        deadline,
    )?;
    crate::runtime_network::proposal::prepare_pinned_proposal_submission(
        verified, request, work_root,
    )
}

/// Prepare a reviewable zero-D exploration request from one immutable source
/// job. The caller must independently authorize the job and owner, and obtain
/// confirmation of the resulting frozen Search before starting any action.
#[allow(clippy::too_many_arguments)]
pub fn prepare_source_exploration(
    fetcher: &dyn SourceFetcher,
    job: &ato_ipc::formation::FormationJobV1,
    contract: ato_formation::authoring::BoundContract,
    work_root: &Path,
    constraint: crate::runtime_network::RuntimeConstraintWire,
    policy: crate::runtime_network::SatisfyPolicy,
    budget: crate::runtime_network::SatisfyBudget,
    search_id: &str,
    toolchains: std::collections::BTreeMap<String, String>,
) -> Result<crate::runtime_network::Submission> {
    job.validate()?;
    let ato_ipc::formation::FormationSourceV1::UploadedArchive(source) = &job.source else {
        bail!("approved_search_requires_immutable_archive");
    };
    anyhow::ensure!(
        source.expected_source_tree_digest.is_some(),
        "approved_search_requires_verified_tree"
    );
    anyhow::ensure!(
        source.subdirectory.is_empty(),
        "approved_search_subdirectory_unsupported"
    );
    anyhow::ensure!(
        job.policy.network == ato_ipc::formation::FormationNetworkPolicyV1::Denied
            && policy.network == "denied",
        "approved_search_network_policy_mismatch"
    );
    budget.validate()?;
    let mut limits = SourceLimits::default();
    limits.max_total_bytes = limits.max_total_bytes.min(budget.max_expanded_bytes);
    let deadline = Instant::now()
        + Duration::from_secs(budget.deadline_seconds)
            .min(crate::runtime_network::SOURCE_TRANSFER_TIMEOUT);
    let verified = verify_source(
        fetcher,
        &job.job_id,
        &serde_json::to_value(&job.source)?,
        work_root,
        limits,
        budget.max_transfer_bytes,
        deadline,
    )?;
    crate::runtime_network::proposal::prepare_verified_exploration_submission(
        verified, contract, work_root, constraint, policy, budget, search_id, toolchains,
    )
}

/// Explicit requester-local file import, using the same private bounded copy
/// as network intake. It never modifies or builds the caller's directory.
pub struct ArchiveFileFetcher(pub PathBuf);
impl SourceFetcher for ArchiveFileFetcher {
    fn fetch(&self, _job_id: &str, _source: &serde_json::Value) -> Result<Vec<u8>> {
        bail!("file_backed_source_intake_required")
    }
    fn fetch_file(
        &self,
        _job_id: &str,
        _source: &serde_json::Value,
        work_root: &Path,
        maximum: u64,
        deadline: Instant,
    ) -> Result<(File, String)> {
        anyhow::ensure!(
            std::fs::symlink_metadata(&self.0)?.is_file(),
            "archive input must be a regular file"
        );
        let mut file = source_fetch::temporary_file(work_root, maximum, deadline)?;
        let digest =
            source_fetch::copy_bounded(File::open(&self.0)?, &mut file, maximum, deadline)?;
        source_fetch::rewind(file, digest)
    }
}
