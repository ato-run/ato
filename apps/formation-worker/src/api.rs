//! The worker's side of the Formation control-plane contract.
//!
//! Every call is authenticated and scoped to one job. The worker holds no R2
//! credential and no database access: it claims work, uploads through a grant
//! the control plane issued, and publishes a result the control plane
//! validates. Anything it could reach on its own would be something an
//! untrusted build could reach through it.

use anyhow::{Context, Result};
use serde::Deserialize;

/// A claimed job, plus what its result attaches to.
///
/// The target rides with the claim because a service is handed a job, not a
/// command line.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaimedFormationWork {
    pub job_id: String,
    pub attempt_id: String,
    pub attempt_fence: u64,
    pub compute_id: Option<String>,
    pub capsule_revision_id: Option<String>,
    pub job: serde_json::Value,
}

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

    /// Take the next queued job, or nothing.
    ///
    /// An empty queue is the normal case for a service, so it returns `None`
    /// rather than an error: a worker that logged a failure every poll would
    /// bury the one that mattered.
    pub fn claim_next(&self, worker_id: &str) -> Result<Option<ClaimedFormationWork>> {
        let response: serde_json::Value = self
            .client
            .post(format!(
                "{}/v1/internal/formation/claim?worker_id={worker_id}",
                self.base
            ))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({}))
            .send()?
            .error_for_status()
            .context("failed to ask for Formation work")?
            .json()?;
        if response["job_id"].is_null() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_value(response)?))
    }

    /// Say that an attempt produced nothing.
    ///
    /// The reason is short and written for the person who uploaded the source,
    /// not copied from stderr: a build's output can carry a host path or a
    /// credential a tool echoed, and neither belongs in something a user reads.
    pub fn report_failure(&self, attempt_id: &str, reason: &str) -> Result<()> {
        self.client
            .post(format!(
                "{}/v1/internal/formation/attempts/{attempt_id}/failure",
                self.base
            ))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "reason": reason }))
            .send()?
            .error_for_status()
            .context("failed to report a Formation failure")?;
        Ok(())
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
    /// Put a Static Web bundle where the edge reads it.
    ///
    /// A static artifact is a manifest plus content-addressed blobs, and the
    /// edge serves those objects directly — so the bundle is published as its
    /// parts, not as a packed tar. Packing it would produce an artifact nothing
    /// can serve without first unpacking it somewhere, and the somewhere would
    /// have to be the Worker.
    ///
    /// Blobs go first. A manifest that is readable before the bytes it names
    /// would leave a window in which the edge could resolve an App to a
    /// half-published site.
    pub fn publish_static_bundle(
        &self,
        attempt_id: &str,
        bundle_root: &std::path::Path,
        manifest_digest: &str,
        blob_digests: &[String],
    ) -> Result<()> {
        for digest in blob_digests {
            let hex = digest
                .strip_prefix("sha256:")
                .context("blob digest is not sha256")?;
            let path = bundle_root.join("blobs").join("sha256").join(hex);
            let bytes = std::fs::read(&path)
                .with_context(|| format!("cannot read blob {}", path.display()))?;
            self.put_static_object(
                &format!(
                    "{}/v1/internal/formation/attempts/{attempt_id}/static-blobs/{digest}",
                    self.base
                ),
                bytes,
            )?;
        }
        let manifest = std::fs::read(bundle_root.join("manifest.json"))
            .context("cannot read the bundle manifest")?;
        self.put_static_object(
            &format!(
                "{}/v1/internal/formation/attempts/{attempt_id}/static-manifest/{manifest_digest}",
                self.base
            ),
            manifest,
        )
    }

    fn put_static_object(&self, url: &str, bytes: Vec<u8>) -> Result<()> {
        self.client
            .put(url)
            .bearer_auth(&self.token)
            .header("content-type", "application/octet-stream")
            .body(bytes)
            .send()?
            .error_for_status()
            .with_context(|| {
                format!(
                    "failed to publish a static object to {}",
                    ato_formation::source::redact_url(url)
                )
            })?;
        Ok(())
    }

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
            // 200 is not acceptance. A shadow run answers 200 with
            // `registered: false` — it validated the result, compared it and
            // threw it away — and reading that as success is how a lane that
            // registers nothing comes to look like a lane that works.
            if body.get("registered").and_then(serde_json::Value::as_bool) == Some(false) {
                return Ok(PublishOutcome::NotRegistered {
                    mode: body
                        .get("mode")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("unknown")
                        .to_owned(),
                    reason: body
                        .get("reason")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                });
            }
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
    /// Computed, compared, and deliberately not registered: the lane is off
    /// or in shadow for this caller. Not a failure of the build, and not a
    /// success for whoever is waiting on an App.
    NotRegistered {
        mode: String,
        reason: String,
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
            // Retrying would produce the same decision from the same policy.
            Self::NotRegistered { .. } => false,
            Self::Refused { code } => !matches!(
                code.as_str(),
                "formation_attempt_superseded"
                    | "formation_result_already_accepted"
                    | "formation_result_invalid"
            ),
        }
    }
}
