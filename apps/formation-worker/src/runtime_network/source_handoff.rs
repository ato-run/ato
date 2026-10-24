//! Explicit owner source provenance for the existing requester. Local byte
//! checks prove consistency, never ownership, approval or current authorization.
use super::{Client, SourceTransport, Submission};
use crate::source_archive::{Binding, SourceArchiveOutput};
use anyhow::{Context, Result, ensure};
use ato_formation::source::FileVerifiedArchive;
use ato_ipc::formation::FormationSourceV1;
use serde::{Deserialize, Serialize};
use std::{io::Read, time::Duration};

const MAX_RESPONSE_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormationSourcePointer {
    pub source_archive_job_id: String,
    pub binding_digest: String,
}

/// The owner-supplied saved API origin, not a client-issued authorization.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormationSourceOrigin {
    pub result: SourceArchiveOutput,
    pub source_selection: FormationSourceV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Transfer {
    source: SourceTransport,
    source_origin: FormationSourceOrigin,
    formation_started: bool,
}

impl FormationSourceOrigin {
    pub fn validate_local_source(
        &self,
        pointer: &FormationSourcePointer,
        submission: &Submission,
    ) -> Result<()> {
        let result = &self.result;
        let digest = |value: &str| {
            value.strip_prefix("sha256:").is_some_and(|hex| {
                hex.len() == 64
                    && hex
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        };
        let suffix = pointer.source_archive_job_id.strip_prefix("fjob_");
        ensure!(
            suffix.is_some_and(|id| !id.is_empty()
                && id.len() <= 120
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)))
                && digest(&pointer.binding_digest)
                && pointer.source_archive_job_id == result.job_id
                && pointer.binding_digest == result.binding_digest
                && result.attempt_fence > 0
                && digest(&result.job_digest),
            "source handoff pointer does not match the saved origin"
        );
        result.validate(&Binding {
            job_id: result.job_id.clone(),
            attempt_id: result.attempt_id.clone(),
            attempt_fence: result.attempt_fence,
            job_digest: result.job_digest.clone(),
            archive_descriptor: result.archive_descriptor.clone(),
        })?;
        let FormationSourceV1::GitHub(selection) = &self.source_selection else {
            anyhow::bail!("source handoff requires the saved GitHub selection");
        };
        ensure!(
            selection.subdirectory.is_empty() && result.archive_descriptor.subdirectory.is_empty(),
            "source handoff currently requires the repository root"
        );
        submission.validate_proposal_submission()?;
        let source = &submission.request.source;
        let descriptor = &result.archive_descriptor;
        ensure!(
            source.archive_base64.is_none()
                && source.content_ref.as_deref() == Some(descriptor.source_archive_digest.as_str())
                && source.archive_digest == descriptor.source_archive_digest
                && source.archive_bytes == Some(descriptor.size_bytes)
                && source.closure_ref == descriptor.source_closure_ref,
            "source handoff request differs from the saved source tuple"
        );
        // Reuse the canonical source verifier on the actual private requester
        // file. Do not turn the server's descriptor into a client attestation.
        let verified = FileVerifiedArchive::verify(
            submission.source_file()?,
            &descriptor.source_archive_digest,
            descriptor.size_bytes,
            ato_formation::source::SourceLimits::default(),
        )?;
        ensure!(
            verified.archive_format() == descriptor.archive_format_version
                && verified.tree_digest() == descriptor.source_tree_digest
                && verified.resolver_contract() == descriptor.resolver_contract_version
                && verified.closure_ref("")?.as_str() == descriptor.source_closure_ref
                && Some(verified.expanded_file_count())
                    == usize::try_from(descriptor.observed_file_count).ok()
                && verified.expanded_bytes() == descriptor.observed_uncompressed_bytes,
            "source handoff archive measurements differ from the saved origin"
        );
        Ok(())
    }
}

impl Client {
    /// Two explicit existing operations: copy the accepted owner source into
    /// Runtime transport, then submit the unchanged request with its pointer.
    /// Never uploads requester-local bytes, retries a POST or claims K success.
    pub fn submit_from_formation_source(
        &self,
        submission: &Submission,
        pointer: &FormationSourcePointer,
        expected: &FormationSourceOrigin,
    ) -> Result<serde_json::Value> {
        expected.validate_local_source(pointer, submission)?;
        let http = reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(60))
            .build()?;
        let transferred: Transfer = self.source_handoff_json(
            &http,
            http.post(self.url("/sources/from-formation")).json(pointer),
        )?;
        ensure!(
            !transferred.formation_started
                && serde_jcs::to_vec(&transferred.source)?
                    == serde_jcs::to_vec(&submission.request.source)?
                && serde_jcs::to_vec(&transferred.source_origin)? == serde_jcs::to_vec(expected)?,
            "source handoff transfer response differs from the saved origin"
        );
        let accepted: serde_json::Value = self.source_handoff_json(
            &http,
            http.post(self.url("/satisfy"))
                .query(pointer)
                .json(&submission.request),
        )?;
        ensure!(
            accepted["search_id"] == submission.request.search_id
                && accepted["satisfy_id"]
                    .as_str()
                    .is_some_and(|id| !id.is_empty()
                        && id.len() <= 160
                        && id
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))),
            "source handoff returned another request identity"
        );
        Ok(accepted)
    }

    fn source_handoff_json<T: serde::de::DeserializeOwned>(
        &self,
        http: &reqwest::blocking::Client,
        request: reqwest::blocking::RequestBuilder,
    ) -> Result<T> {
        let response = http.execute(self.prepare(request)?)?;
        ensure!(
            response.status().is_success(),
            "source handoff refused ({})",
            response.status()
        );
        let mut bytes = Vec::new();
        response
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_RESPONSE_BYTES,
            "source handoff response exceeds limit"
        );
        serde_json::from_slice(&bytes).context("source handoff response is not valid JSON")
    }
}
