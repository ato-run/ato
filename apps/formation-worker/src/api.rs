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

    /// Publish a workspace artifact through a control-plane grant.
    ///
    /// The worker never learns a bucket name or an object key. It sends bytes;
    /// the control plane derives where they go and returns the content address
    /// it recorded.
    pub fn publish_artifact(&self, bytes: &[u8]) -> Result<String> {
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
        response
            .get("materialization_ref")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .context("publication receipt names no materialization")
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
