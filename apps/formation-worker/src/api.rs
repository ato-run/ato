//! The worker's side of the Formation control-plane contract.
//!
//! Every call is authenticated and scoped to one job. The worker holds no R2
//! credential and no database access: it claims work, uploads through a grant
//! the control plane issued, and publishes a result the control plane
//! validates. Anything it could reach on its own would be something an
//! untrusted build could reach through it.

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ClaimedFormationJob {
    pub attempt_id: String,
    pub attempt_fence: u64,
    /// The canonical FormationJobV1, exactly as submitted.
    pub job: serde_json::Value,
}

pub struct FormationApi {
    client: reqwest::blocking::Client,
    base: String,
    token: String,
}

impl FormationApi {
    pub fn new(client: reqwest::blocking::Client, base: String, token: String) -> Self {
        Self {
            client,
            base,
            token,
        }
    }

    /// Claim the next attempt for a job, taking the fence with it.
    pub fn claim(&self, job_id: &str, worker_id: &str) -> Result<ClaimedFormationJob> {
        Ok(self
            .client
            .post(format!(
                "{}/v1/internal/formation/jobs/{job_id}/attempts?worker_id={worker_id}",
                self.base
            ))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({}))
            .send()?
            .error_for_status()
            .context("failed to claim a Formation attempt")?
            .json()?)
    }

    /// How much of an artifact goes in one request.
    ///
    /// Well under the control plane's 100 MB request cap, with room for
    /// framing. A process workspace carrying an interpreter and an installed
    /// dependency tree is comfortably over that cap — the acceptance fixture is
    /// 112 MB — so a single POST is not a publication path for this artifact.
    const PART_BYTES: usize = 48 * 1024 * 1024;

    /// Publish a workspace artifact.
    ///
    /// The worker never learns a bucket name or a final object key. It sends
    /// parts to a staging key the control plane derived, and the control plane
    /// digests what actually arrived and files it under that address — so the
    /// artifact's identity comes from its bytes rather than from the worker's
    /// claim about them.
    pub fn publish_artifact(&self, bytes: &[u8]) -> Result<String> {
        if bytes.len() <= Self::PART_BYTES {
            let response: serde_json::Value = self
                .client
                .post(format!("{}/v1/internal/workspaces", self.base))
                .bearer_auth(&self.token)
                .header("content-type", "application/octet-stream")
                .body(bytes.to_vec())
                .send()?
                .error_for_status()
                .context("failed to publish the workspace artifact")?
                .json()?;
            return response
                .get("materialization_ref")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .context("publication receipt names no materialization");
        }

        let started: serde_json::Value = self
            .client
            .post(format!("{}/v1/internal/workspaces/multipart", self.base))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({}))
            .send()?
            .error_for_status()
            .context("failed to begin a multipart publication")?
            .json()?;
        let key = started["key"]
            .as_str()
            .context("no staging key")?
            .to_owned();
        let upload_id = started["upload_id"]
            .as_str()
            .context("no upload id")?
            .to_owned();

        let mut parts = Vec::new();
        for (index, chunk) in bytes.chunks(Self::PART_BYTES).enumerate() {
            // R2 part numbers start at 1.
            let part_number = index + 1;
            let uploaded: serde_json::Value = self
                .client
                .put(format!(
                    "{}/v1/internal/workspaces/multipart?key={key}&upload_id={upload_id}&part={part_number}",
                    self.base
                ))
                .bearer_auth(&self.token)
                .header("content-type", "application/octet-stream")
                .body(chunk.to_vec())
                .send()?
                .error_for_status()
                .with_context(|| format!("failed to upload part {part_number}"))?
                .json()?;
            parts.push(serde_json::json!({
                "part_number": uploaded["part_number"],
                "etag": uploaded["etag"],
            }));
        }

        let completed: serde_json::Value = self
            .client
            .post(format!(
                "{}/v1/internal/workspaces/multipart/complete",
                self.base
            ))
            .bearer_auth(&self.token)
            // The digest travels with the completion because the control
            // plane cannot buffer a workspace-sized object to compute it. The
            // Runner recomputes it on download and refuses a mismatch, so a
            // wrong value yields an artifact nothing will run.
            .json(&serde_json::json!({
                "key": key,
                "upload_id": upload_id,
                "digest": digest_of(bytes),
                "parts": parts,
            }))
            .send()?
            .error_for_status()
            .context("failed to complete the multipart publication")?
            .json()?;
        completed
            .get("materialization_ref")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .context("completion receipt names no materialization")
    }

    /// Offer a result. The control plane decides whether it counts.
    ///
    /// A refusal here is expected, not exceptional: a superseded attempt is
    /// refused by design, and treating that as an error would make the ordinary
    /// retry path look like a failure.
    pub fn publish_result(
        &self,
        result: &serde_json::Value,
        compute_id: &str,
        capsule_revision_id: &str,
    ) -> Result<PublishOutcome> {
        let response = self
            .client
            .post(format!("{}/v1/internal/formation/results", self.base))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "result": result,
                "compute_id": compute_id,
                "capsule_revision_id": capsule_revision_id,
            }))
            .send()?;
        let status = response.status();
        let body: serde_json::Value = response.json().unwrap_or_default();
        if status.is_success() {
            return Ok(PublishOutcome::Accepted {
                compute_schema_id: body
                    .get("compute_schema_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            });
        }
        Ok(PublishOutcome::Refused {
            code: body
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
        })
    }
}

fn digest_of(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[derive(Debug, PartialEq, Eq)]
pub enum PublishOutcome {
    Accepted {
        compute_schema_id: String,
    },
    /// Refused by the control plane — a superseded attempt, a duplicate
    /// completion, a result the contract rejected.
    Refused {
        code: String,
    },
}

impl PublishOutcome {
    /// Whether this outcome means the work should be retried.
    ///
    /// A superseded attempt should NOT be: something newer already did the
    /// work, and retrying would race it again.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Accepted { .. } => false,
            Self::Refused { code } => !matches!(
                code.as_str(),
                "formation_attempt_superseded"
                    | "formation_result_already_accepted"
                    | "formation_result_invalid"
            ),
        }
    }
}
