//! The worker's side of the Formation control-plane contract.
//!
//! Every call is authenticated and scoped to one job. The worker holds no R2
//! credential and no database access: it claims work, uploads through a grant
//! the control plane issued, and publishes a result the control plane
//! validates. Anything it could reach on its own would be something an
//! untrusted build could reach through it.

use anyhow::{Context, Result, anyhow};

#[cfg(test)]
mod readiness_tests;
use ato_runtime_attempt::journal::AttemptRecordState;
use serde::{Deserialize, Serialize};

pub use ato_runtime_attempt::text::{FAILURE_REASON_LIMIT, bounded_reason};

/// A failure as the control plane receives it: a code to branch on, the stage
/// that refused it, and one sentence written for the uploader.
#[derive(Debug, Clone)]
pub struct FailureReport {
    pub code: String,
    pub stage: String,
    pub message: String,
    pub attempt_record: Option<AttemptRecordState>,
    pub receipt: Option<serde_json::Value>,
    pub outcomes: Option<serde_json::Value>,
}

impl FailureReport {
    /// A refusal that happened AFTER the build, while registering the result.
    /// The build itself was fine, and saying "build failed" would send whoever
    /// reads it to look in the wrong place.
    pub fn publish(code: &str, message: String) -> Self {
        Self {
            code: code.to_owned(),
            stage: "publish".to_owned(),
            message: bounded_reason(&message),
            attempt_record: None,
            receipt: None,
            outcomes: None,
        }
    }
}

/// A claimed job, plus what its result attaches to.
///
/// The target rides with the claim because a service is handed a job, not a
/// command line.
#[derive(Debug, Clone, Deserialize)]
pub struct ClaimedFormationWork {
    pub job_id: String,
    pub attempt_id: String,
    pub attempt_fence: u64,
    #[serde(default)]
    pub operation_catalog_required: bool,
    pub compute_id: Option<String>,
    pub capsule_revision_id: Option<String>,
    pub job: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct ClaimedFormationJob {
    pub attempt_id: String,
    pub attempt_fence: u64,
    #[serde(default)]
    pub operation_catalog_required: bool,
    /// The canonical FormationJobV1, exactly as submitted.
    pub job: serde_json::Value,
}

/// The identity fields a worker reports with its lifecycle state.
///
/// `binary_sha256` is of the running file's bytes; the rest are the compile
/// time build stamps. The control plane stores them so an operator can ask
/// "what is on that host?" without shelling into it.
#[derive(Debug, Clone, Serialize)]
pub struct WorkerIdentityReport {
    pub commit: String,
    pub dirty: String,
    pub build_id: String,
    pub rustc: String,
    pub binary_sha256: String,
}

/// `POST /v1/internal/formation/workers/self`.
///
/// Reporting is not a request: the control plane stores the state and answers
/// with a directive. A worker cannot clear its own drain or update request by
/// reporting — those stay operator-owned.
#[derive(Debug, Clone, Serialize)]
pub struct WorkerStateReport {
    pub worker_id: String,
    pub agent_version: String,
    pub lifecycle_state: String,
    pub identity: WorkerIdentityReport,
    /// The attempt this process is executing right now, if any.
    pub current_attempt_id: Option<String>,
}

/// What the control plane answers a state report with.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkerDirective {
    #[serde(default)]
    pub ok: bool,
    /// Stop claiming. Set when the runner is drained or an update is required.
    #[serde(default)]
    pub drain: bool,
    #[serde(default)]
    pub target_version: Option<String>,
    #[serde(default)]
    pub target_digest: Option<String>,
    #[serde(default)]
    pub min_version: Option<String>,
    #[serde(default)]
    pub next_heartbeat_seconds: Option<u64>,
}

/// Why a claim was refused by the control plane.
///
/// These are stop signals, not transport failures: a drained worker and a
/// worker being updated must stop claiming instead of retrying, and neither
/// may consume an attempt fence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimRefusal {
    Drained,
    UpdateRequired {
        target_version: Option<String>,
        target_digest: Option<String>,
    },
}

/// The outcome of asking for the next job.
#[derive(Debug)]
pub enum ClaimOutcome {
    Work(Box<ClaimedFormationWork>),
    /// The queue is empty. Normal for a service.
    Idle,
    /// The control plane says stop. Authoritative even if a state report
    /// failed.
    Stopped(ClaimRefusal),
}

#[derive(Clone)]
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

    /// Ask for the next job, keeping the control plane's stop signals distinct
    /// from transport failures.
    ///
    /// A `409` here means the worker must stop claiming — it is drained, or an
    /// update is required — and is authoritative even when the last state
    /// report failed. Reading it as a generic error would make the worker keep
    /// polling for work the control plane will never hand it.
    pub fn claim_next_controlled(&self, worker_id: &str) -> Result<ClaimOutcome> {
        let response = self
            .client
            .post(format!(
                "{}/v1/internal/formation/claim?worker_id={worker_id}",
                self.base
            ))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({}))
            .send()?;
        let status = response.status();
        let body: serde_json::Value = response.json().unwrap_or_default();
        interpret_claim_response(status, body)
    }

    /// Tell the control plane what this worker is and what phase it is in.
    ///
    /// The response is the only directive channel the control plane has to
    /// this worker; callers keep it and stop claiming when `drain` is set.
    pub fn report_worker_state(&self, report: &WorkerStateReport) -> Result<WorkerDirective> {
        let response = self
            .client
            .post(format!("{}/v1/internal/formation/workers/self", self.base))
            .bearer_auth(&self.token)
            .json(report)
            .send()?;
        let status = response.status();
        let body: serde_json::Value = response.json().unwrap_or_default();
        if !status.is_success() {
            let code = body
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            return Err(anyhow!(
                "the control plane refused this worker state report: HTTP {status} ({code})"
            ));
        }
        Ok(serde_json::from_value(body)?)
    }

    /// Publish only the bounded operation source from a verified closure.
    pub fn register_operation_source(
        &self,
        attempt_id: &str,
        source: &crate::operations::OperationSource,
    ) -> Result<()> {
        self.client
            .post(format!(
                "{}/v1/internal/formation/attempts/{attempt_id}/operation-source",
                self.base
            ))
            .bearer_auth(&self.token)
            .json(source)
            .send()?
            .error_for_status()
            .context("operation source registration rejected")?;
        Ok(())
    }

    /// Say that an attempt produced nothing.
    ///
    /// The reason is short and written for the person who uploaded the source,
    /// not copied from stderr: a build's output can carry a host path or a
    /// credential a tool echoed, and neither belongs in something a user reads.
    /// Report a terminal failure.
    ///
    /// `code` and `stage` are additive on the control plane: an older API that
    /// reads only `reason` behaves exactly as it did, and a code it does not
    /// recognise is dropped there rather than rendered.
    pub fn report_failure(&self, attempt_id: &str, failure: &FailureReport) -> Result<()> {
        // A new endpoint makes an old receiver fail closed rather than silently
        // dropping journal uncertainty from an additive JSON field.
        let endpoint = if failure.attempt_record.is_some() {
            "outcome"
        } else {
            "failure"
        };
        self.client
            .post(format!(
                "{}/v1/internal/formation/attempts/{attempt_id}/{endpoint}",
                self.base
            ))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "reason": failure.message,
                "code": failure.code,
                "stage": failure.stage,
                "attempt_record": failure.attempt_record,
                "receipt": failure.receipt,
                "outcomes": failure.outcomes,
            }))
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

    /// Register the sealed portable bundle a v2 Formation produced.
    ///
    /// Create-once by transport digest: the control plane verifies the bytes,
    /// stores them through the existing validator path and answers the
    /// `capsule_bundles.id` a Hosted Run later imports. Nothing about K is
    /// decided here — the receipt published with the result is the verdict.
    pub fn publish_portable_bundle(
        &self,
        attempt_id: &str,
        digest: &str,
        contract_ref: &str,
        derivation_ref: &str,
        bytes: &[u8],
    ) -> Result<String> {
        // The route refuses a bundle that does not name the Contract and
        // Derivation it claims to be: a transport digest alone does not say
        // which K the bytes satisfy. Both are content addresses, so the
        // control plane can check them against the attempt it authorized.
        let response: serde_json::Value = self
            .client
            .put(format!(
                "{}/v1/internal/formation/attempts/{attempt_id}/portable-bundle/{digest}",
                self.base
            ))
            .bearer_auth(&self.token)
            .header(
                "content-type",
                ato_ipc::formation::PORTABLE_APPLICATION_MEDIA_TYPE,
            )
            .header("x-ato-contract-ref", contract_ref)
            .header("x-ato-derivation-ref", derivation_ref)
            .body(bytes.to_vec())
            .send()?
            .error_for_status()
            .context("failed to register the portable bundle artifact")?
            .json()?;
        response
            .get("bundle_id")
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned)
            .context("portable bundle registration named no bundle_id")
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
        self.publish_result_with_readiness_wait(
            result,
            compute_id,
            capsule_revision_id,
            std::time::Duration::from_secs(60),
            std::time::Duration::from_secs(1),
        )
    }

    fn publish_result_with_readiness_wait(
        &self,
        result: &serde_json::Value,
        compute_id: &str,
        capsule_revision_id: &str,
        budget: std::time::Duration,
        interval: std::time::Duration,
    ) -> Result<PublishOutcome> {
        let started = std::time::Instant::now();
        let mut announced_wait = false;
        loop {
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
            // Registration queues the ordinary Rust bundle validator. Its ack may
            // arrive after the first result POST. Retry only this pending state,
            // with the identical result and fence; every other refusal is final.
            // The API keeps checking ownership, validation and the current fence
            // on every request. No success is published before its acceptance.
            if status == reqwest::StatusCode::CONFLICT
                && result["formation_kind"] == "portable_v2"
                && body["error"] == "portable_bundle_artifact_not_ready"
                && started.elapsed() < budget
            {
                if !announced_wait {
                    eprintln!(
                        "[formation] job={} bundle validator pending; waiting before registering the same fenced result",
                        result["job_id"].as_str().unwrap_or("unknown")
                    );
                    announced_wait = true;
                }
                std::thread::sleep(interval.min(budget.saturating_sub(started.elapsed())));
                continue;
            }
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
            return Ok(PublishOutcome::Refused {
                code: body
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned(),
            });
        }
    }
}

fn digest_of(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// Read a claim response without collapsing "stop" into "error".
///
/// Only the two documented `409` codes are stop signals. Anything else that
/// is not a success keeps the old behaviour — an error the caller may retry —
/// because a worker that treats every refusal as an update instruction would
/// idle forever on a misconfiguration.
fn interpret_claim_response(
    status: reqwest::StatusCode,
    body: serde_json::Value,
) -> Result<ClaimOutcome> {
    if status == reqwest::StatusCode::CONFLICT {
        return match body.get("error").and_then(serde_json::Value::as_str) {
            Some("runner_drained") => Ok(ClaimOutcome::Stopped(ClaimRefusal::Drained)),
            Some("runner_update_required") => {
                Ok(ClaimOutcome::Stopped(ClaimRefusal::UpdateRequired {
                    target_version: string_field(&body, "target_version"),
                    target_digest: string_field(&body, "target_digest"),
                }))
            }
            Some(other) => Err(anyhow!("the control plane refused the claim: {other}")),
            None => Err(anyhow!(
                "the control plane refused the claim (HTTP {status}) without a reason"
            )),
        };
    }
    if !status.is_success() {
        return Err(anyhow!("failed to ask for Formation work: HTTP {status}"));
    }
    if body.get("job_id").is_none_or(serde_json::Value::is_null) {
        return Ok(ClaimOutcome::Idle);
    }
    Ok(ClaimOutcome::Work(Box::new(serde_json::from_value(body)?)))
}

fn string_field(body: &serde_json::Value, name: &str) -> Option<String> {
    body.get(name)
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
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

/// Both daemon and one-shot use this classification and the same reporter.
pub fn classify_failure(error: &anyhow::Error) -> FailureReport {
    if let Some(failure) = error.downcast_ref::<crate::job::HostedAttemptFailure>() {
        return failure.report.clone();
    }
    if let Some(failure) = error.downcast_ref::<ato_formation::failure::FormationFailure>() {
        return FailureReport {
            code: failure.code.clone(),
            stage: failure.stage.as_str().to_owned(),
            message: failure.bounded_message(FAILURE_REASON_LIMIT),
            attempt_record: None,
            receipt: None,
            outcomes: None,
        };
    }
    FailureReport {
        code: "build_failed".into(),
        stage: "build".into(),
        message: "the app could not be built from this source".into(),
        attempt_record: None,
        receipt: None,
        outcomes: None,
    }
}

/// The worker control wire format is shared with the API (PR2a). These tests
// leave the contract in the same place as the code that depends on it: a field
// rename here would otherwise only fail against a live control plane.
#[cfg(test)]
mod worker_lifecycle_contract_tests {
    use super::*;
    use serde_json::json;

    fn report() -> WorkerStateReport {
        WorkerStateReport {
            worker_id: "formation-sugamo".to_owned(),
            agent_version: "0.1.1".to_owned(),
            lifecycle_state: "active".to_owned(),
            identity: WorkerIdentityReport {
                commit: "17305afe0".to_owned(),
                dirty: "clean".to_owned(),
                build_id: "ci-123".to_owned(),
                rustc: "rustc 1.96.0".to_owned(),
                binary_sha256: "a".repeat(64),
            },
            current_attempt_id: None,
        }
    }

    #[test]
    fn the_state_report_uses_the_contract_field_names() {
        let encoded = serde_json::to_value(report()).unwrap();
        assert_eq!(
            encoded,
            json!({
                "worker_id": "formation-sugamo",
                "agent_version": "0.1.1",
                "lifecycle_state": "active",
                "identity": {
                    "commit": "17305afe0",
                    "dirty": "clean",
                    "build_id": "ci-123",
                    "rustc": "rustc 1.96.0",
                    "binary_sha256": "a".repeat(64),
                },
                "current_attempt_id": null,
            })
        );
        let mut with_attempt = report();
        with_attempt.current_attempt_id = Some("attempt-1".to_owned());
        assert_eq!(
            serde_json::to_value(with_attempt).unwrap()["current_attempt_id"],
            json!("attempt-1")
        );
    }

    #[test]
    fn a_directive_deserializes_from_the_contract_response() {
        let target_digest = "b".repeat(64);
        let directive: WorkerDirective = serde_json::from_value(json!({
            "ok": true,
            "drain": true,
            "target_version": "0.1.1",
            "target_digest": target_digest,
            "min_version": "0.1.0",
            "next_heartbeat_seconds": 15,
        }))
        .unwrap();
        assert!(directive.ok);
        assert!(directive.drain);
        assert_eq!(directive.target_version.as_deref(), Some("0.1.1"));
        assert_eq!(
            directive.target_digest.as_deref(),
            Some(target_digest.as_str())
        );
        assert_eq!(directive.min_version.as_deref(), Some("0.1.0"));
        assert_eq!(directive.next_heartbeat_seconds, Some(15));

        // A minimal response must not fail a worker that only needs `drain`.
        let minimal: WorkerDirective =
            serde_json::from_value(json!({"ok": true, "drain": false})).unwrap();
        assert!(!minimal.drain);
        assert_eq!(minimal.target_version, None);
        assert_eq!(minimal.next_heartbeat_seconds, None);
    }

    #[test]
    fn a_conflict_is_a_stop_signal_not_a_transport_error() {
        let drained = interpret_claim_response(
            reqwest::StatusCode::CONFLICT,
            json!({"error": "runner_drained"}),
        )
        .unwrap();
        assert!(matches!(
            drained,
            ClaimOutcome::Stopped(ClaimRefusal::Drained)
        ));

        let update = interpret_claim_response(
            reqwest::StatusCode::CONFLICT,
            json!({
                "error": "runner_update_required",
                "target_version": "0.1.1",
                "target_digest": null,
            }),
        )
        .unwrap();
        match update {
            ClaimOutcome::Stopped(ClaimRefusal::UpdateRequired {
                target_version,
                target_digest,
            }) => {
                assert_eq!(target_version.as_deref(), Some("0.1.1"));
                assert_eq!(target_digest, None);
            }
            other => panic!("expected an update stop signal, got {other:?}"),
        }
    }

    #[test]
    fn other_failures_stay_retryable_errors() {
        let unknown_conflict = interpret_claim_response(
            reqwest::StatusCode::CONFLICT,
            json!({"error": "some_other_conflict"}),
        );
        assert!(unknown_conflict.is_err());
        let anonymous_conflict = interpret_claim_response(reqwest::StatusCode::CONFLICT, json!({}));
        assert!(anonymous_conflict.is_err());
        let server_error = interpret_claim_response(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            json!({"error": "boom"}),
        );
        assert!(server_error.is_err());
    }

    #[test]
    fn an_empty_queue_is_idle_and_a_job_is_work() {
        for body in [json!({"job_id": null}), json!({})] {
            assert!(matches!(
                interpret_claim_response(reqwest::StatusCode::OK, body).unwrap(),
                ClaimOutcome::Idle
            ));
        }
        let work = interpret_claim_response(
            reqwest::StatusCode::OK,
            json!({
                "job_id": "job-1",
                "attempt_id": "attempt-1",
                "attempt_fence": 7,
                "compute_id": null,
                "capsule_revision_id": null,
                "job": {"schema": "ato.formation-job/1"},
            }),
        )
        .unwrap();
        match work {
            ClaimOutcome::Work(work) => {
                assert_eq!(work.job_id, "job-1");
                assert_eq!(work.attempt_fence, 7);
                assert!(!work.operation_catalog_required);
            }
            other => panic!("expected work, got {other:?}"),
        }
    }
}
