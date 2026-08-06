//! Authoring Session v1 control-plane client and source-only inference.
//!
//! This module deliberately owns transport and pure inference only. Execution
//! remains in the snapshot builder's existing pinned-source and Firecracker
//! lanes, so the Authoring Session cannot grow a second build contract.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule::authoring_intent::{
    NormalizedProgramIntentEnvelopeV1, ProgramCommandDraftV1, ProgramIntentDraftV1,
    ProgramIntentOrigin, ReadinessIntentV1, WorkspacePathV1, draft_from_capsule_manifest_v1,
    normalize_program_intent, to_capsule_manifest_v1,
};
use capsule::types::manifest_v1::{
    MetadataAssetsV1, SealAtV1, StaticWebOutputV1, StoreMetadataV1,
};
use serde::{Deserialize, Deserializer, Serialize};
use snapshot::archive_only_build::ArchiveOnlyBuildInput;
use snapshot::authoring_evidence::{
    BuilderAuthenticationV1, ClassifiedStateDiffV1, CleanReplayReceiptV1, MediaRepairReceiptV1,
    ReadyStateSealReceiptV1,
};

const AUTHORING_BASE_PATH: &str = "/v1/capsule-snapshots/authoring";
const SCREENSHOT_COMPLETION_INITIAL_RETRY_DELAY: Duration = Duration::from_millis(250);
const SCREENSHOT_COMPLETION_MAX_RETRY_DELAY: Duration = Duration::from_secs(5);
const SCREENSHOT_COMPLETION_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const SCREENSHOT_COMPLETION_DEADLINE_MARGIN: chrono::Duration = chrono::Duration::seconds(1);

#[derive(Clone)]
pub struct AuthoringLeaseToken(String);

impl AuthoringLeaseToken {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AuthoringLeaseToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AuthoringLeaseToken([REDACTED])")
    }
}

impl<'de> Deserialize<'de> for AuthoringLeaseToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.len() < 32 || value.len() > 512 {
            return Err(serde::de::Error::custom(
                "authoring lease token length is invalid",
            ));
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedAuthoringSource {
    pub source_revision_id: String,
    #[serde(rename = "source_materialization_id")]
    pub _source_materialization_id: String,
    pub source_archive_digest: String,
    pub source_archive_object_key: String,
    pub source_tree_digest: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoringSourceOverlay {
    #[serde(rename = "source_overlay_id")]
    pub _source_overlay_id: String,
    pub source_revision_id: String,
    #[serde(rename = "overlay_digest")]
    pub _overlay_digest: String,
    pub manifest: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoringStoreMetadata {
    pub name: String,
    pub short_description: String,
    pub full_description: String,
    #[serde(default)]
    pub primary_category: Option<String>,
    #[serde(default)]
    pub primary_subcategory: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub assets: Option<MetadataAssetsV1>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoringWork {
    pub kind: String,
    pub work_id: String,
    /// Per-claim fencing generation for queued builder jobs. Setup leases
    /// predate job fencing and therefore legitimately omit this field.
    #[serde(default)]
    pub worker_claim_id: Option<String>,
    pub authoring_session_id: String,
    pub capsule_revision_id: String,
    pub source_revision_id: String,
    pub source_closure_id: String,
    pub pinned_source: PinnedAuthoringSource,
    #[serde(default)]
    pub source_overlay: Option<AuthoringSourceOverlay>,
    /// Store-facing authored intent projected by the API before this claim is
    /// handed to a builder. The builder merges it into the declaration before
    /// deriving Program Intent or invoking the v1 build lane, so there is one
    /// Effective Manifest for build, lock, revision, and Clean Replay.
    #[serde(default)]
    pub store_metadata: Option<AuthoringStoreMetadata>,
    #[serde(default)]
    pub previous_receipt_digest: Option<String>,
    #[serde(default)]
    pub setup_mode: Option<String>,
    #[serde(default)]
    pub setup_journal_sequence: u64,
    #[serde(default)]
    pub normalized_program_intent: Option<NormalizedProgramIntentEnvelopeV1>,
    #[serde(default)]
    pub resolution_lock_digest: Option<String>,
    #[serde(default)]
    #[serde(rename = "request")]
    pub _request: Option<serde_json::Value>,
    #[serde(default)]
    pub clean_replay_receipt: Option<CleanReplayReceiptV1>,
    #[serde(default)]
    pub classified_state_diff: Option<ClassifiedStateDiffV1>,
    #[serde(default)]
    pub ready_state_seal_receipt: Option<ReadyStateSealReceiptV1>,
    // ── `static-web-bundle-v1` claim extension ─────────────────────────────
    //
    // Emitted by the API only to a builder that advertised the
    // `static-web-bundle-v1` capability, and only on build-operation claims
    // bound to a Build Config Revision. All optional: a claim without them is
    // the snapshot-compute lane, byte-identical to the legacy shape.
    #[serde(default)]
    pub build_config_revision_id: Option<String>,
    #[serde(default)]
    #[serde(rename = "source_build_attempt_id")]
    pub _source_build_attempt_id: Option<serde_json::Value>,
    #[serde(default)]
    #[serde(rename = "build_attempt_number")]
    pub _build_attempt_number: Option<serde_json::Value>,
    #[serde(default)]
    #[serde(rename = "authoring_toml")]
    pub _authoring_toml: Option<String>,
    /// The API's effective build plan. Its `static_web_output` section carries
    /// the server-derived materialization id the producer uses verbatim.
    #[serde(default)]
    pub effective_build_plan: Option<serde_json::Value>,
    #[serde(default)]
    pub plan_digest: Option<String>,
    #[serde(default)]
    #[serde(rename = "authoring_toml_digest")]
    pub _authoring_toml_digest: Option<serde_json::Value>,
    #[serde(default)]
    #[serde(rename = "static_web_output")]
    pub _static_web_output: Option<serde_json::Value>,
    /// The resolved Publication Lane for this Build Config Revision. Parsed
    /// tolerantly — an absent or unrecognized value is the snapshot-compute
    /// lane, never an error, so lane resolution stays server-owned.
    #[serde(default)]
    pub publication: Option<ClaimPublication>,
    pub lease_token: AuthoringLeaseToken,
    pub lease_expires_at: String,
    pub trace_id: String,
}

/// Deliberately NOT `deny_unknown_fields`: the lane classification is
/// server-owned advisory data and must stay forward-extensible.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClaimPublication {
    #[serde(default)]
    pub resolved_publication_lane: Option<String>,
    #[serde(default)]
    #[serde(rename = "classification")]
    pub _classification: Option<serde_json::Value>,
}

impl AuthoringWork {
    /// Whether this claim's Build Config Revision resolved to the Static Web
    /// Publication Lane.
    pub fn is_static_web_lane(&self) -> bool {
        self.publication
            .as_ref()
            .and_then(|publication| publication.resolved_publication_lane.as_deref())
            == Some("static_web")
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimResponse {
    work: Option<AuthoringWork>,
}

#[derive(Debug, Serialize)]
struct ClaimRequest<'a> {
    builder_id: &'a str,
    supported_operations: &'a [&'a str],
    /// Capability advertisement. Rollout is builder-first: advertising
    /// `static-web-bundle-v1` is what lets the API attach the plan-extension
    /// fields to a claim without breaking older builders.
    supported_features: &'a [&'a str],
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScreenshotCompletionError {
    #[error("screenshot completion was refused ({code}, HTTP {status}, trace {trace_id})")]
    Refused {
        status: u16,
        code: String,
        trace_id: String,
    },
    #[error("screenshot completion remains retryable ({code}, HTTP {status}, trace {trace_id})")]
    RetryableHttp {
        status: u16,
        code: String,
        trace_id: String,
    },
    #[error("screenshot completion remains retryable ({code}, trace {trace_id})")]
    RetryableTransport { code: String, trace_id: String },
    #[error("screenshot completion deadline is invalid ({field})")]
    InvalidDeadline { field: &'static str },
    #[error("screenshot completion receipt is invalid")]
    InvalidReceipt,
}

impl ScreenshotCompletionError {
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RetryableHttp { .. } | Self::RetryableTransport { .. }
        )
    }

    fn retryable(status: Option<u16>, code: String, trace_id: String) -> ScreenshotCompletionError {
        match status {
            Some(status) => Self::RetryableHttp {
                status,
                code,
                trace_id,
            },
            None => Self::RetryableTransport { code, trace_id },
        }
    }

    fn diagnostic(&self) -> (&str, &str) {
        match self {
            Self::Refused { code, trace_id, .. }
            | Self::RetryableHttp { code, trace_id, .. }
            | Self::RetryableTransport { code, trace_id } => (code, trace_id),
            Self::InvalidDeadline { .. } => ("invalid_completion_deadline", "none"),
            Self::InvalidReceipt => ("invalid_media_repair_receipt", "none"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ScreenshotCompletionAck {
    pub accepted: bool,
    pub already_completed: bool,
}

pub struct SetupCommandCompletion {
    pub exit_code: i32,
    pub duration_ms: u64,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

pub struct AuthoringApiClient<'a> {
    pub api_url: &'a str,
    pub builder_token: &'a str,
    pub builder_id: &'a str,
}

impl AuthoringApiClient<'_> {
    pub fn claim(&self, supported_operations: &[&str]) -> Result<Option<AuthoringWork>, String> {
        let response = ureq::post(&format!(
            "{}{AUTHORING_BASE_PATH}/claim",
            self.api_url.trim_end_matches('/')
        ))
        .set("authorization", &format!("Bearer {}", self.builder_token))
        .send_json(
            serde_json::to_value(ClaimRequest {
                builder_id: self.builder_id,
                supported_operations,
                supported_features: &["static-web-bundle-v1"],
            })
            .map_err(|error| format!("encode authoring claim: {error}"))?,
        )
        .map_err(|error| http_error("claim authoring work", error))?;
        response
            .into_json::<ClaimResponse>()
            .map(|body| body.work)
            .map_err(|error| format!("decode authoring claim: {error}"))
    }

    pub fn authorize_source_archive(&self, work: &AuthoringWork) -> Result<String, String> {
        let response = ureq::post(&format!(
            "{}{AUTHORING_BASE_PATH}/work/{}/source-archive/download-authorization",
            self.api_url.trim_end_matches('/'),
            work.work_id
        ))
        .set("authorization", &format!("Bearer {}", self.builder_token))
        .set("x-ato-authoring-lease-token", work.lease_token.expose())
        .send_json(serde_json::json!({ "builder_id": self.builder_id }))
        .map_err(|error| http_error("authorize authoring source archive", error))?;
        let body = response
            .into_json::<serde_json::Value>()
            .map_err(|error| format!("decode archive authorization: {error}"))?;
        body.get("download_url")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "archive authorization omitted download_url".to_string())
    }

    pub fn mark_setup_ready(
        &self,
        work: &AuthoringWork,
        input: &SetupReady<'_>,
    ) -> Result<(), String> {
        ureq::post(&format!(
            "{}{AUTHORING_BASE_PATH}/setup/{}/ready",
            self.api_url.trim_end_matches('/'),
            work.work_id
        ))
        .set("authorization", &format!("Bearer {}", self.builder_token))
        .set("x-ato-authoring-lease-token", work.lease_token.expose())
        .send_json(
            serde_json::to_value(input)
                .map_err(|error| format!("encode setup-ready evidence: {error}"))?,
        )
        .map_err(|error| http_error("report setup ready", error))?;
        Ok(())
    }

    pub fn setup_control(&self, work: &AuthoringWork) -> Result<SetupControl, String> {
        let response = ureq::get(&format!(
            "{}{AUTHORING_BASE_PATH}/setup/{}/control",
            self.api_url.trim_end_matches('/'),
            work.work_id
        ))
        .set("authorization", &format!("Bearer {}", self.builder_token))
        .set("x-ato-authoring-lease-token", work.lease_token.expose())
        .query("builder_id", self.builder_id)
        .call()
        .map_err(|error| http_error("poll setup control", error))?;
        response
            .into_json()
            .map_err(|error| format!("decode setup control: {error}"))
    }

    pub fn claim_setup_command(
        &self,
        work: &AuthoringWork,
        builder_slot_id: &str,
    ) -> Result<Option<SetupCommandClaim>, String> {
        let worker_claim_id = work.worker_claim_id.as_deref().unwrap_or(&work.work_id);
        let response = ureq::post(&format!(
            "{}{AUTHORING_BASE_PATH}/setup/{}/commands/claim",
            self.api_url.trim_end_matches('/'),
            work.work_id
        ))
        .set("authorization", &format!("Bearer {}", self.builder_token))
        .set("x-ato-authoring-lease-token", work.lease_token.expose())
        .send_json(serde_json::json!({
            "builder_id": self.builder_id,
            "builder_slot_id": builder_slot_id,
            "worker_claim_id": worker_claim_id,
        }))
        .map_err(|error| http_error("claim setup command", error))?;
        let body = response
            .into_json::<SetupCommandClaimResponse>()
            .map_err(|error| format!("decode setup command claim: {error}"))?;
        Ok(body.command)
    }

    pub fn append_setup_command_output(
        &self,
        work: &AuthoringWork,
        builder_slot_id: &str,
        command: &SetupCommandClaim,
        stream: &str,
        sequence: u64,
        data: &str,
    ) -> Result<(), String> {
        let worker_claim_id = work.worker_claim_id.as_deref().unwrap_or(&work.work_id);
        ureq::post(&format!(
            "{}{AUTHORING_BASE_PATH}/setup/{}/commands/{}/output",
            self.api_url.trim_end_matches('/'),
            work.work_id,
            command.command_id
        ))
        .set("authorization", &format!("Bearer {}", self.builder_token))
        .set("x-ato-authoring-lease-token", work.lease_token.expose())
        .send_json(serde_json::json!({
            "builder_id": self.builder_id,
            "builder_slot_id": builder_slot_id,
            "worker_claim_id": worker_claim_id,
            "lease_generation": command.lease_generation,
            "stream": stream,
            "sequence": sequence,
            "data": data,
        }))
        .map_err(|error| http_error("append setup command output", error))?;
        Ok(())
    }

    pub fn complete_setup_command(
        &self,
        work: &AuthoringWork,
        builder_slot_id: &str,
        command: &SetupCommandClaim,
        completion: &SetupCommandCompletion,
    ) -> Result<(), String> {
        let worker_claim_id = work.worker_claim_id.as_deref().unwrap_or(&work.work_id);
        ureq::post(&format!(
            "{}{AUTHORING_BASE_PATH}/setup/{}/commands/{}/complete",
            self.api_url.trim_end_matches('/'),
            work.work_id,
            command.command_id
        ))
        .set("authorization", &format!("Bearer {}", self.builder_token))
        .set("x-ato-authoring-lease-token", work.lease_token.expose())
        .send_json(serde_json::json!({
            "builder_id": self.builder_id,
            "builder_slot_id": builder_slot_id,
            "worker_claim_id": worker_claim_id,
            "lease_generation": command.lease_generation,
            "exit_code": completion.exit_code.clamp(0, 255),
            "duration_ms": completion.duration_ms,
            "stdout_truncated": completion.stdout_truncated,
            "stderr_truncated": completion.stderr_truncated,
        }))
        .map_err(|error| http_error("complete setup command", error))?;
        Ok(())
    }

    pub fn mark_setup_stopped(&self, work: &AuthoringWork) -> Result<(), String> {
        ureq::post(&format!(
            "{}{AUTHORING_BASE_PATH}/setup/{}/stopped",
            self.api_url.trim_end_matches('/'),
            work.work_id
        ))
        .set("authorization", &format!("Bearer {}", self.builder_token))
        .set("x-ato-authoring-lease-token", work.lease_token.expose())
        .send_json(serde_json::json!({ "builder_id": self.builder_id }))
        .map_err(|error| http_error("report setup stopped", error))?;
        Ok(())
    }

    pub fn mark_setup_failed(
        &self,
        work: &AuthoringWork,
        stage: &str,
        error_code: &str,
        error_message: &str,
    ) -> Result<(), String> {
        let safe_message = error_message
            .chars()
            .map(|character| {
                if character.is_control() {
                    ' '
                } else {
                    character
                }
            })
            .take(2048)
            .collect::<String>();
        ureq::post(&format!(
            "{}{AUTHORING_BASE_PATH}/setup/{}/failed",
            self.api_url.trim_end_matches('/'),
            work.work_id
        ))
        .set("authorization", &format!("Bearer {}", self.builder_token))
        .set("x-ato-authoring-lease-token", work.lease_token.expose())
        .send_json(serde_json::json!({
            "builder_id": self.builder_id,
            "stage": stage,
            "error_code": error_code,
            "error_message": safe_message,
        }))
        .map_err(|error| http_error("report setup failure", error))?;
        Ok(())
    }

    pub fn append_setup_observation(
        &self,
        work: &AuthoringWork,
        sequence: u64,
        event: serde_json::Value,
    ) -> Result<(), String> {
        ureq::post(&format!(
            "{}{AUTHORING_BASE_PATH}/setup/{}/observation",
            self.api_url.trim_end_matches('/'),
            work.work_id
        ))
        .set("authorization", &format!("Bearer {}", self.builder_token))
        .set("x-ato-authoring-lease-token", work.lease_token.expose())
        .send_json(serde_json::json!({
            "builder_id": self.builder_id,
            "sequence": sequence,
            "event": event,
        }))
        .map_err(|error| http_error("append setup observation", error))?;
        Ok(())
    }

    pub fn complete_clean_replay(
        &self,
        work: &AuthoringWork,
        receipt: &CleanReplayReceiptV1,
        classified_state_diff: &ClassifiedStateDiffV1,
        execution_contract_jcs_base64: &str,
    ) -> Result<(), String> {
        let classified = serde_jcs::to_vec(classified_state_diff)
            .map_err(|error| format!("canonicalize classified state diff: {error}"))?;
        ureq::post(&format!(
            "{}{AUTHORING_BASE_PATH}/jobs/{}/clean-replay",
            self.api_url.trim_end_matches('/'),
            work.work_id
        ))
        .set("authorization", &format!("Bearer {}", self.builder_token))
        .set("x-ato-authoring-lease-token", work.lease_token.expose())
        .send_json(serde_json::json!({
            "builder_id": self.builder_id,
            "receipt": receipt,
            "classified_state_diff_jcs_base64": BASE64.encode(classified),
            "execution_contract_jcs_base64": execution_contract_jcs_base64,
        }))
        .map_err(|error| http_error("report Clean Replay completion", error))?;
        Ok(())
    }

    pub fn complete_ready_state_seal(
        &self,
        work: &AuthoringWork,
        receipt: &ReadyStateSealReceiptV1,
        screenshot_png_base64: &str,
    ) -> Result<(), String> {
        ureq::post(&format!(
            "{}{AUTHORING_BASE_PATH}/jobs/{}/ready-state-seal",
            self.api_url.trim_end_matches('/'),
            work.work_id
        ))
        .set("authorization", &format!("Bearer {}", self.builder_token))
        .set("x-ato-authoring-lease-token", work.lease_token.expose())
        .send_json(serde_json::json!({
            "builder_id": self.builder_id,
            "seal_receipt": receipt,
            "preview_run_id": format!("restore_{}", work.work_id),
            "route": "/",
            "viewport": {
                "width": 1240,
                "height": 698,
                "device_scale_factor": 1,
            },
            "post_restore_screenshot_png_base64": screenshot_png_base64,
        }))
        .map_err(|error| http_error("report Ready-State Seal completion", error))?;
        Ok(())
    }

    pub fn complete_screenshot_capture(
        &self,
        work: &AuthoringWork,
        receipt: &MediaRepairReceiptV1,
        screenshot_png_base64: &str,
    ) -> Result<ScreenshotCompletionAck, ScreenshotCompletionError> {
        let receipt_payload = receipt
            .payload()
            .map_err(|_| ScreenshotCompletionError::InvalidReceipt)?;
        let deadline =
            screenshot_completion_deadline(&work.lease_expires_at, &receipt_payload.expires_at)?;
        let request_body = screenshot_completion_request_body(
            self.builder_id,
            &work.work_id,
            receipt,
            screenshot_png_base64,
            receipt_payload.screenshot_quality.width,
            receipt_payload.screenshot_quality.height,
        );
        let url = format!(
            "{}{AUTHORING_BASE_PATH}/jobs/{}/screenshot-capture",
            self.api_url.trim_end_matches('/'),
            work.work_id
        );
        let mut retry_delay = SCREENSHOT_COMPLETION_INITIAL_RETRY_DELAY;

        loop {
            let now = chrono::Utc::now();
            let remaining = deadline.signed_duration_since(now);
            let request_timeout = remaining
                .to_std()
                .map_err(|_| {
                    ScreenshotCompletionError::retryable(
                        None,
                        "media_repair_retry_deadline_exceeded".to_string(),
                        work.trace_id.clone(),
                    )
                })?
                .min(SCREENSHOT_COMPLETION_REQUEST_TIMEOUT);
            let result = ureq::post(&url)
                .timeout(request_timeout)
                .set("authorization", &format!("Bearer {}", self.builder_token))
                .set("x-ato-authoring-lease-token", work.lease_token.expose())
                .send_json(request_body.clone())
                .map_err(|error| screenshot_completion_http_error(error, &work.trace_id))
                .and_then(decode_screenshot_completion_ack);

            match result {
                Ok(ack) => return Ok(ack),
                Err(error) if !error.is_retryable() => return Err(error),
                Err(error) => {
                    let remaining = deadline.signed_duration_since(chrono::Utc::now());
                    let Ok(remaining) = remaining.to_std() else {
                        return Err(error);
                    };
                    let delay = retry_delay.min(remaining);
                    if delay.is_zero() {
                        return Err(error);
                    }
                    let (code, trace_id) = error.diagnostic();
                    eprintln!(
                        "[builder] media repair completion retry: code={code} trace={trace_id} delay_ms={}",
                        delay.as_millis()
                    );
                    std::thread::sleep(delay);
                    retry_delay = retry_delay
                        .checked_mul(2)
                        .unwrap_or(SCREENSHOT_COMPLETION_MAX_RETRY_DELAY)
                        .min(SCREENSHOT_COMPLETION_MAX_RETRY_DELAY);
                }
            }
        }
    }

    pub fn mark_job_failed(
        &self,
        work: &AuthoringWork,
        error_code: &str,
        error_message: &str,
    ) -> Result<(), String> {
        let worker_claim_id = work
            .worker_claim_id
            .as_deref()
            .ok_or_else(|| "authoring job claim omitted worker_claim_id".to_string())?;
        ureq::post(&format!(
            "{}{AUTHORING_BASE_PATH}/jobs/{}/failed",
            self.api_url.trim_end_matches('/'),
            work.work_id
        ))
        .set("authorization", &format!("Bearer {}", self.builder_token))
        .set("x-ato-authoring-lease-token", work.lease_token.expose())
        .send_json(serde_json::json!({
            "builder_id": self.builder_id,
            "worker_claim_id": worker_claim_id,
            "error_code": error_code,
            "error_message": safe_failure_message(error_message),
        }))
        .map_err(|error| http_error("report authoring job failure", error))?;
        Ok(())
    }
}

fn safe_failure_message(message: &str) -> String {
    message
        .chars()
        .map(|character| {
            if character.is_control() && character != '\t' {
                ' '
            } else {
                character
            }
        })
        .take(2048)
        .collect()
}

#[derive(Debug, Serialize)]
pub struct SetupReady<'a> {
    pub builder_id: &'a str,
    pub builder_session_id: &'a str,
    pub builder_slot_id: &'a str,
    pub origin: &'a str,
    pub normalized_program_intent: &'a NormalizedProgramIntentEnvelopeV1,
    pub resolution_lock_digest: &'a str,
    pub source_closure_id: &'a str,
    pub generated_capsule_toml: &'a str,
    pub materialized_assets: &'a [MaterializedSetupAsset],
}

#[derive(Debug, Serialize)]
pub struct MaterializedSetupAsset {
    pub kind: &'static str,
    pub origin_path: String,
    pub content_digest: String,
    pub media_type: String,
    pub bytes_base64: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SetupControl {
    pub action: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupCommandClaimResponse {
    command: Option<SetupCommandClaim>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupCommandClaim {
    pub command_id: String,
    pub shell: Vec<String>,
    pub stdin: String,
    pub cwd: String,
    pub max_runtime_seconds: u64,
    pub max_output_bytes_per_stream: usize,
    #[serde(rename = "policy_digest")]
    pub _policy_digest: String,
    pub lease_generation: u64,
}

pub struct AuthoringArchiveTransport<'a> {
    pub client: &'a AuthoringApiClient<'a>,
    pub work: &'a AuthoringWork,
}

impl crate::source_archive_download::ArchiveDownloadTransport for AuthoringArchiveTransport<'_> {
    fn authorize(
        &self,
        work_id: &str,
    ) -> Result<String, crate::source_archive_download::DownloadFailure> {
        if work_id != self.work.work_id {
            return Err(
                crate::source_archive_download::DownloadFailure::AuthorizationRefused {
                    code: "work_id_mismatch".to_string(),
                    detail: "the requested work is not this lease's work".to_string(),
                },
            );
        }
        self.client
            .authorize_source_archive(self.work)
            .map_err(|detail| {
                crate::source_archive_download::DownloadFailure::AuthorizationRefused {
                    code: "authoring_authorization_refused".to_string(),
                    detail,
                }
            })
    }

    fn get(&self, url: &str, destination: &Path) -> Result<u64, String> {
        let response = ureq::get(url).call().map_err(|error| match error {
            ureq::Error::Status(status, _) => format!("HTTP {status}"),
            ureq::Error::Transport(transport) => {
                format!("transport error: {}", transport.kind())
            }
        })?;
        let mut reader = response.into_reader();
        let mut file = std::fs::File::create(destination).map_err(|error| error.to_string())?;
        std::io::copy(&mut reader, &mut file).map_err(|error| error.to_string())
    }
}

pub fn archive_input(work: &AuthoringWork) -> Result<ArchiveOnlyBuildInput, String> {
    let source = &work.pinned_source;
    if source.source_revision_id != work.source_revision_id {
        return Err("pinned source identity does not match its Authoring Session".to_string());
    }
    ArchiveOnlyBuildInput::new(
        source.source_revision_id.clone(),
        source.source_archive_digest.clone(),
        source.source_archive_object_key.clone(),
        source.source_tree_digest.clone(),
    )
    .map_err(|error| error.to_string())
}

pub struct AuthoringSigner {
    key_id: String,
    signing_key: ed25519_dalek::SigningKey,
}

impl AuthoringSigner {
    pub fn from_env() -> Result<Option<Self>, String> {
        let Some(path) = std::env::var_os("ATO_AUTHORING_BUILDER_SIGNING_KEY_FILE") else {
            return Ok(None);
        };
        let key_id = std::env::var("ATO_AUTHORING_BUILDER_KEY_ID")
            .map_err(|_| "ATO_AUTHORING_BUILDER_KEY_ID is required with the signing key")?;
        if key_id.trim().is_empty() {
            return Err("ATO_AUTHORING_BUILDER_KEY_ID must not be empty".to_string());
        }
        let path = std::path::PathBuf::from(path);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path)
                .map_err(|error| format!("stat Authoring signing key: {error}"))?
                .permissions()
                .mode()
                & 0o777;
            if mode & 0o077 != 0 {
                return Err("Authoring signing key must not be group/world accessible".to_string());
            }
        }
        let encoded = std::fs::read_to_string(&path)
            .map_err(|error| format!("read Authoring signing key: {error}"))?;
        let bytes = BASE64
            .decode(encoded.trim())
            .map_err(|_| "Authoring signing key must be base64".to_string())?;
        let secret: [u8; 32] = bytes
            .try_into()
            .map_err(|_| "Authoring signing key must contain exactly 32 bytes".to_string())?;
        Ok(Some(Self {
            key_id,
            signing_key: ed25519_dalek::SigningKey::from_bytes(&secret),
        }))
    }

    pub fn authenticate(&self, payload: &[u8]) -> BuilderAuthenticationV1 {
        use ed25519_dalek::Signer as _;
        let signature = self.signing_key.sign(payload);
        BuilderAuthenticationV1 {
            key_id: self.key_id.clone(),
            algorithm: "ed25519".to_string(),
            signature: BASE64.encode(signature.to_bytes()),
        }
    }
}

fn screenshot_completion_request_body(
    builder_id: &str,
    work_id: &str,
    receipt: &MediaRepairReceiptV1,
    screenshot_png_base64: &str,
    width: u32,
    height: u32,
) -> serde_json::Value {
    serde_json::json!({
        "builder_id": builder_id,
        "media_repair_receipt": receipt,
        "preview_run_id": format!("media_repair_{work_id}"),
        "route": "/",
        "viewport": {
            "width": width,
            "height": height,
            "device_scale_factor": 1,
        },
        "post_restore_screenshot_png_base64": screenshot_png_base64,
    })
}

fn screenshot_completion_deadline(
    lease_expires_at: &str,
    receipt_expires_at: &str,
) -> Result<chrono::DateTime<chrono::Utc>, ScreenshotCompletionError> {
    let lease_deadline = chrono::DateTime::parse_from_rfc3339(lease_expires_at)
        .map_err(|_| ScreenshotCompletionError::InvalidDeadline {
            field: "lease_expires_at",
        })?
        .with_timezone(&chrono::Utc);
    let receipt_deadline = chrono::DateTime::parse_from_rfc3339(receipt_expires_at)
        .map_err(|_| ScreenshotCompletionError::InvalidDeadline {
            field: "receipt_expires_at",
        })?
        .with_timezone(&chrono::Utc);
    Ok(lease_deadline.min(receipt_deadline) - SCREENSHOT_COMPLETION_DEADLINE_MARGIN)
}

fn screenshot_completion_http_error(
    error: ureq::Error,
    fallback_trace_id: &str,
) -> ScreenshotCompletionError {
    match error {
        ureq::Error::Status(status, response) => {
            let body = response.into_string().unwrap_or_default();
            let (code, trace_id) =
                parse_screenshot_completion_rejection(&body, status, fallback_trace_id);
            if status >= 500 || matches!(status, 408 | 429) {
                ScreenshotCompletionError::retryable(Some(status), code, trace_id)
            } else {
                ScreenshotCompletionError::Refused {
                    status,
                    code,
                    trace_id,
                }
            }
        }
        ureq::Error::Transport(_) => ScreenshotCompletionError::retryable(
            None,
            "media_repair_transport_failed".to_string(),
            sanitize_completion_diagnostic(fallback_trace_id, 128, "none"),
        ),
    }
}

fn decode_screenshot_completion_ack(
    response: ureq::Response,
) -> Result<ScreenshotCompletionAck, ScreenshotCompletionError> {
    let ack = response
        .into_json::<ScreenshotCompletionAck>()
        .map_err(|_| {
            ScreenshotCompletionError::retryable(
                Some(200),
                "media_repair_response_invalid".to_string(),
                "none".to_string(),
            )
        })?;
    if !ack.accepted {
        return Err(ScreenshotCompletionError::retryable(
            Some(200),
            "media_repair_response_not_accepted".to_string(),
            "none".to_string(),
        ));
    }
    Ok(ack)
}

fn parse_screenshot_completion_rejection(
    body: &str,
    status: u16,
    fallback_trace_id: &str,
) -> (String, String) {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    let code = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(serde_json::Value::as_str)
        .map(|value| sanitize_completion_diagnostic(value, 128, &format!("http_{status}")))
        .unwrap_or_else(|| format!("http_{status}"));
    let trace_id = parsed
        .as_ref()
        .and_then(|value| value.get("trace_id"))
        .and_then(serde_json::Value::as_str)
        .map(|value| sanitize_completion_diagnostic(value, 128, fallback_trace_id))
        .unwrap_or_else(|| sanitize_completion_diagnostic(fallback_trace_id, 128, "none"));
    (code, trace_id)
}

fn sanitize_completion_diagnostic(value: &str, limit: usize, fallback: &str) -> String {
    let sanitized = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .take(limit)
        .collect::<String>();
    if sanitized.is_empty() {
        fallback.to_string()
    } else {
        sanitized
    }
}

fn http_error(operation: &str, error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(status, response) => {
            let body = response.into_string().unwrap_or_default();
            let (code, detail) = parse_http_rejection(&body, status);
            match detail {
                Some(detail) => {
                    format!("{operation} was refused ({code}, HTTP {status}): {detail}")
                }
                None => format!("{operation} was refused ({code}, HTTP {status})"),
            }
        }
        ureq::Error::Transport(transport) => {
            format!("{operation} transport failed ({})", transport.kind())
        }
    }
}

fn parse_http_rejection(body: &str, status: u16) -> (String, Option<String>) {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    let code = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("http_{status}"));
    let detail = parsed
        .as_ref()
        .and_then(|value| value.get("message"))
        .and_then(serde_json::Value::as_str)
        .map(|message| {
            message
                .chars()
                .filter(|character| !character.is_control())
                .take(512)
                .collect::<String>()
        })
        .filter(|message| !message.trim().is_empty());
    (code, detail)
}

/// Infer the narrow source families supported by Authoring Model v1.
///
/// The inference is intentionally source-based and fail-closed. A repository
/// without a recognized, explicit entrypoint remains unresolved rather than
/// being launched with a guessed framework command.
pub fn infer_authoring_intent(
    source_root: &Path,
) -> Result<NormalizedProgramIntentEnvelopeV1, String> {
    if source_root.join("deno.json").is_file() {
        return infer_deno_fresh_intent(source_root);
    }
    // A `package.json` marks a package-managed application regardless of
    // whether a root `index.html` also exists (many bundler-served apps,
    // including swagger-editor, ship one as their dev-server template). It
    // must never be routed into the dependency-free static-file path below —
    // that would silently skip dependency install and the app's own start
    // script (ato-api#443).
    if source_root.join("package.json").is_file() {
        return infer_package_managed_intent(source_root);
    }
    if !source_root.join("index.html").is_file() {
        return Err(
            "source inference requires deno.json with a plain Fresh start task, package.json for a package-managed application, or a root index.html for a dependency-free static application"
                .to_string(),
        );
    }
    normalize_program_intent(ProgramIntentDraftV1 {
        schema: capsule::authoring_intent::PROGRAM_INTENT_DRAFT_V1_SCHEMA.to_string(),
        origin: ProgramIntentOrigin::Inference,
        toolchains: Vec::new(),
        build_steps: Vec::new(),
        launch: ProgramCommandDraftV1::Argv {
            argv: vec![
                "python3".to_string(),
                "-m".to_string(),
                "http.server".to_string(),
                "8000".to_string(),
                "--bind".to_string(),
                "0.0.0.0".to_string(),
            ],
            cwd: WorkspacePathV1::root(),
            requested_environment: Vec::new(),
            required_tools: vec!["python3".to_string()],
        },
        readiness: ReadinessIntentV1::Http {
            port: 8000,
            path: "/".to_string(),
            timeout_seconds: 60,
        },
        build_output_roots: Vec::new(),
        bindings: Vec::new(),
        unresolved: Vec::new(),
    })
    .map_err(|error| error.to_string())
}

/// Infer a launch command for a package-managed (`package.json`-carrying)
/// application.
///
/// Dependency install itself is decided independently, from filesystem
/// evidence, by the v1 build lane (`rootfs_builder::base_image_and_install`)
/// — this function only has to choose how the app is *started* once installed.
/// It only auto-infers a launch command when it can be confident the app will
/// actually bind where Ato expects: today that means `scripts.start` or
/// `scripts.dev` resolves, with no shell syntax, to a plain invocation whose
/// last token is `vite` — a CLI with well-known `--host`/`--port` flags that
/// can be appended safely as trailing argv (via the package manager's `--`
/// argument-forwarding convention) without guessing whether the underlying
/// framework even reads a port argument. Any other shape (missing script,
/// shell operators, an unrecognized dev-server binary, an ambiguous set of
/// lockfiles) fails closed to manual setup rather than assume.
fn infer_package_managed_intent(
    source_root: &Path,
) -> Result<NormalizedProgramIntentEnvelopeV1, String> {
    let package_json: serde_json::Value = serde_json::from_slice(
        &std::fs::read(source_root.join("package.json"))
            .map_err(|error| format!("read package.json: {error}"))?,
    )
    .map_err(|error| format!("parse package.json: {error}"))?;

    let package_manager = resolve_launch_package_manager(source_root, &package_json)?;

    let (script_name, script_command) = package_json
        .get("scripts")
        .and_then(serde_json::Value::as_object)
        .and_then(|scripts| {
            ["start", "dev"].iter().find_map(|name| {
                scripts
                    .get(*name)
                    .and_then(serde_json::Value::as_str)
                    .map(|command| (*name, command))
            })
        })
        .ok_or_else(|| {
            "package-managed inference requires scripts.start or scripts.dev in package.json"
                .to_string()
        })?;

    let script_argv = parse_plain_task_argv(script_command)?;
    if script_argv.last().map(String::as_str) != Some("vite") {
        return Err(format!(
            "package-managed inference cannot guarantee `{package_manager} run {script_name}` ({script_command:?}) binds to Ato's assigned host/port — manual setup required"
        ));
    }

    // Production lane: when the package also declares plain `vite build` +
    // `vite preview` scripts, build once and serve the BUILT app instead of
    // leaving the published capsule on the dev server. Dev serving ships the
    // unbundled module graph (measured for drawdb: 448 requests / 115 MB
    // before first paint through the app proxy — the "ready but blank for
    // 30s" preview); `vite preview` serves the minified dist/. The build runs
    // at IMAGE BUILD time (the guest rootfs is read-only at boot) and a
    // restore resumes the already-serving process — launches never pay for
    // it. Scripts with shell syntax or a non-vite tail keep today's
    // dev-server lane rather than guessing.
    if let Some(production) = infer_vite_production_launch(&package_json, package_manager) {
        return normalize_program_intent(production).map_err(|error| error.to_string());
    }

    normalize_program_intent(ProgramIntentDraftV1 {
        schema: capsule::authoring_intent::PROGRAM_INTENT_DRAFT_V1_SCHEMA.to_string(),
        origin: ProgramIntentOrigin::Inference,
        toolchains: Vec::new(),
        build_steps: Vec::new(),
        launch: ProgramCommandDraftV1::Argv {
            argv: vec![
                package_manager.to_string(),
                "run".to_string(),
                script_name.to_string(),
                "--".to_string(),
                "--host".to_string(),
                "0.0.0.0".to_string(),
                "--port".to_string(),
                "8000".to_string(),
            ],
            cwd: WorkspacePathV1::root(),
            requested_environment: Vec::new(),
            required_tools: vec![package_manager.to_string()],
        },
        readiness: ReadinessIntentV1::Http {
            port: 8000,
            path: "/".to_string(),
            timeout_seconds: 60,
        },
        build_output_roots: Vec::new(),
        bindings: Vec::new(),
        unresolved: Vec::new(),
    })
    .map_err(|error| error.to_string())
}

/// The vite production launch, when the package declares it unambiguously:
/// `scripts.build` is plainly `… vite build` and `scripts.preview` is plainly
/// `… vite preview`. Anything else (a compound build like swagger-editor's
/// `npm run build:app && …`, a missing preview script) returns `None` and the
/// caller keeps the dev-server lane.
///
/// The launch itself is ONLY `<pm> run preview`: the production build cannot
/// run at guest boot — the v1 guest rootfs is read-only, so `vite build`
/// writing `dist/` would fail on EROFS — and instead runs at IMAGE BUILD time,
/// chained after dependency install by the v1 build lane
/// (`rootfs_builder::vite_production_prebuild_cmd`, keyed on this exact
/// launch shape). `--strictPort` makes a port collision fail readiness instead
/// of silently serving on a port Ato never probes.
fn infer_vite_production_launch(
    package_json: &serde_json::Value,
    package_manager: &'static str,
) -> Option<ProgramIntentDraftV1> {
    let script = |name: &str| -> Option<Vec<String>> {
        package_json
            .get("scripts")
            .and_then(serde_json::Value::as_object)
            .and_then(|scripts| scripts.get(name))
            .and_then(serde_json::Value::as_str)
            .and_then(|command| parse_plain_task_argv(command).ok())
    };
    let tail_is = |argv: &[String], tail: [&str; 2]| {
        argv.len() >= 2 && argv[argv.len() - 2..] == tail.map(str::to_string)
    };
    let build = script("build")?;
    let preview = script("preview")?;
    if !tail_is(&build, ["vite", "build"]) || !tail_is(&preview, ["vite", "preview"]) {
        return None;
    }
    Some(ProgramIntentDraftV1 {
        schema: capsule::authoring_intent::PROGRAM_INTENT_DRAFT_V1_SCHEMA.to_string(),
        origin: ProgramIntentOrigin::Inference,
        toolchains: Vec::new(),
        build_steps: Vec::new(),
        launch: ProgramCommandDraftV1::Argv {
            argv: vec![
                package_manager.to_string(),
                "run".to_string(),
                "preview".to_string(),
                "--".to_string(),
                "--host".to_string(),
                "0.0.0.0".to_string(),
                "--port".to_string(),
                "8000".to_string(),
                "--strictPort".to_string(),
            ],
            cwd: WorkspacePathV1::root(),
            requested_environment: Vec::new(),
            required_tools: vec![package_manager.to_string()],
        },
        readiness: ReadinessIntentV1::Http {
            port: 8000,
            path: "/".to_string(),
            timeout_seconds: 60,
        },
        build_output_roots: Vec::new(),
        bindings: Vec::new(),
        unresolved: Vec::new(),
    })
}

/// Infer a `[outputs.static_web]` declaration for a HIGH-CONFIDENCE static
/// repository, or `None` (the snapshot-compute fallback).
///
/// Fail-closed by design, mirroring the Publication Lane contract:
/// - a root `index.html` with no package manager and no server entrypoint
///   (`server.py`/`app.py`/`server.js`) is static served from the workspace
///   root — the generated `http.server` run command COEXISTS with the output;
/// - a Vite package whose `build`/`preview` scripts are plainly `vite build` /
///   `vite preview` (the same gate as [`infer_vite_production_launch`], whose
///   image-build-time prebuild is what materializes the output directory)
///   declares the framework output dir: `build.outDir` when it is a literal
///   string in `vite.config.*`, `dist` when no override exists, and NOTHING
///   when an override is present but not a readable literal;
/// - everything else — server frameworks in the dependency set, dev-server
///   launches, Deno, undecidable configs — emits no declaration. The mere
///   existence of `dist//build//index.html` never flips a repo static.
pub fn infer_static_web_outputs(source_root: &Path) -> Option<StaticWebOutputV1> {
    if source_root.join("deno.json").is_file() {
        return None;
    }
    if source_root.join("package.json").is_file() {
        return infer_vite_static_web_outputs(source_root);
    }
    if !source_root.join("index.html").is_file() {
        return None;
    }
    const SERVER_ENTRYPOINTS: &[&str] = &["server.py", "app.py", "server.js"];
    if SERVER_ENTRYPOINTS
        .iter()
        .any(|entry| source_root.join(entry).is_file())
    {
        return None;
    }
    Some(StaticWebOutputV1 {
        root: ".".to_string(),
        entry_path: "index.html".to_string(),
        spa_fallback: true,
        connect_src: Vec::new(),
    })
}

fn infer_vite_static_web_outputs(source_root: &Path) -> Option<StaticWebOutputV1> {
    let package_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(source_root.join("package.json")).ok()?).ok()?;
    let package_manager = resolve_launch_package_manager(source_root, &package_json).ok()?;
    // Only the production `vite build`+`vite preview` shape is static-positive:
    // it is the exact shape whose output dir the image build materializes (see
    // `rootfs_builder::vite_production_prebuild_cmd`). A dev-server launch
    // ships the unbundled module graph and stays compute.
    infer_vite_production_launch(&package_json, package_manager)?;
    const SERVER_FRAMEWORKS: &[&str] = &[
        "express",
        "fastify",
        "koa",
        "@hapi/hapi",
        "next",
        "nuxt",
        "@remix-run/node",
        "@sveltejs/kit",
        "socket.io",
        "ws",
    ];
    for section in ["dependencies", "devDependencies"] {
        if let Some(dependencies) = package_json
            .get(section)
            .and_then(serde_json::Value::as_object)
            && SERVER_FRAMEWORKS
                .iter()
                .any(|name| dependencies.contains_key(*name))
        {
            return None;
        }
    }
    let root = vite_out_dir(source_root)?;
    Some(StaticWebOutputV1 {
        root,
        entry_path: "index.html".to_string(),
        spa_fallback: true,
        connect_src: Vec::new(),
    })
}

/// The Vite output directory: `dist` unless `vite.config.*` overrides
/// `build.outDir`, and only a LITERAL string override is honored. An override
/// this cheap read cannot resolve (an expression, a variable) returns `None`
/// so the caller emits no declaration rather than a wrong one.
fn vite_out_dir(source_root: &Path) -> Option<String> {
    let config = ["vite.config.ts", "vite.config.js", "vite.config.mts", "vite.config.mjs"]
        .iter()
        .map(|name| source_root.join(name))
        .find(|path| path.is_file());
    let Some(config) = config else {
        // No config file at all still means the Vite default output dir.
        return Some("dist".to_string());
    };
    let text = std::fs::read_to_string(&config).ok()?;
    let Some(index) = text.find("outDir") else {
        return Some("dist".to_string());
    };
    let rest = text[index + "outDir".len()..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let body = &rest[1..];
    let literal = body.split(quote).next()?;
    // A second `outDir` occurrence is ambiguous — refuse to guess.
    if text[index + "outDir".len()..].contains("outDir") {
        return None;
    }
    capsule::contract::static_web_manifest::validate_relative_path(literal).ok()?;
    Some(literal.to_string())
}

/// Decide which package manager launches the app, preferring the explicit
/// Corepack `packageManager` declaration and otherwise inferring from
/// whichever single lockfile is present. No lockfile defaults to `npm`
/// (always available wherever `package.json` is honored); more than one
/// *distinct* package manager's lockfile is ambiguous and fails closed rather
/// than guessing which one is authoritative.
fn resolve_launch_package_manager(
    source_root: &Path,
    package_json: &serde_json::Value,
) -> Result<&'static str, String> {
    if let Some(declared) = package_json
        .get("packageManager")
        .and_then(serde_json::Value::as_str)
    {
        let name = declared.split('@').next().unwrap_or_default();
        return match name {
            "npm" => Ok("npm"),
            "yarn" => Ok("yarn"),
            "pnpm" => Ok("pnpm"),
            "bun" => Ok("bun"),
            other => Err(format!(
                "unsupported packageManager `{other}` declared in package.json"
            )),
        };
    }
    const LOCKFILES: &[(&str, &str)] = &[
        ("package-lock.json", "npm"),
        ("npm-shrinkwrap.json", "npm"),
        ("yarn.lock", "yarn"),
        ("pnpm-lock.yaml", "pnpm"),
        ("bun.lock", "bun"),
        ("bun.lockb", "bun"),
    ];
    let mut found: Vec<&str> = LOCKFILES
        .iter()
        .filter(|(file, _)| source_root.join(file).is_file())
        .map(|(_, manager)| *manager)
        .collect();
    found.sort_unstable();
    found.dedup();
    match found.as_slice() {
        [] => Ok("npm"),
        [single] => Ok(single),
        multiple => Err(format!(
            "ambiguous package manager: multiple lockfiles disagree ({})",
            multiple.join(", ")
        )),
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum SourceOverlayManifestV1 {
    CapsuleToml {
        schema: String,
        capsule_toml: String,
        #[serde(default)]
        #[serde(rename = "normalized_manifest_digest")]
        _normalized_manifest_digest: Option<String>,
        #[serde(default)]
        #[serde(rename = "base_manifest_digest")]
        _base_manifest_digest: Option<String>,
    },
    ManualCommand {
        schema: String,
        launch_argv: Vec<String>,
        port: u16,
        readiness_path: String,
    },
}

pub fn resolve_authoring_recipe(
    source_root: &Path,
    work: &AuthoringWork,
) -> Result<(NormalizedProgramIntentEnvelopeV1, String), String> {
    // Auto lane inference happens ONLY when there is no authored capsule.toml
    // and no manual overlay — an author-owned manifest is never rewritten.
    let mut inferred_static_web: Option<StaticWebOutputV1> = None;
    let (origin_manifest, preserve_exact_bytes, merge_store_draft) = if let Some(overlay) =
        &work.source_overlay
    {
        if overlay.source_revision_id != work.source_revision_id {
            return Err("Source Overlay targets a different immutable Source Revision".to_string());
        }
        let manifest: SourceOverlayManifestV1 = serde_json::from_value(overlay.manifest.clone())
            .map_err(|error| format!("decode Source Overlay manifest: {error}"))?;
        match manifest {
            SourceOverlayManifestV1::CapsuleToml {
                schema,
                capsule_toml,
                ..
            } => {
                require_overlay_schema(&schema)?;
                // A capsule_toml overlay is already the complete author-edited
                // Manifest. It is applied after the pre-setup Store draft and
                // therefore wins byte-for-byte.
                (capsule_toml, true, false)
            }
            SourceOverlayManifestV1::ManualCommand {
                schema,
                launch_argv,
                port,
                readiness_path,
            } => {
                require_overlay_schema(&schema)?;
                if launch_argv.is_empty() {
                    return Err("manual launch argv is empty".to_string());
                }
                if readiness_path != "/" {
                    return Err(
                        "manual Authoring v1 currently supports only the synthesized root readiness path"
                            .to_string(),
                    );
                }
                let required_tools = vec![launch_argv[0].clone()];
                let normalized = normalize_program_intent(ProgramIntentDraftV1 {
                    schema: capsule::authoring_intent::PROGRAM_INTENT_DRAFT_V1_SCHEMA.to_string(),
                    origin: ProgramIntentOrigin::ManualSetup,
                    toolchains: Vec::new(),
                    build_steps: Vec::new(),
                    launch: ProgramCommandDraftV1::Argv {
                        argv: launch_argv,
                        cwd: WorkspacePathV1::root(),
                        requested_environment: Vec::new(),
                        required_tools,
                    },
                    readiness: ReadinessIntentV1::Http {
                        port,
                        path: readiness_path,
                        timeout_seconds: 60,
                    },
                    build_output_roots: Vec::new(),
                    bindings: Vec::new(),
                    unresolved: Vec::new(),
                })
                .map_err(|error| format!("normalize manual Program Intent: {error}"))?;
                (render_inferred_capsule_toml(&normalized)?, false, true)
            }
        }
    } else {
        let source_manifest = source_root.join("capsule.toml");
        if source_manifest.is_file() {
            let capsule_toml = std::fs::read_to_string(&source_manifest)
                .map_err(|error| format!("read source capsule.toml: {error}"))?;
            (capsule_toml, true, true)
        } else {
            let normalized = infer_authoring_intent(source_root)?;
            inferred_static_web = infer_static_web_outputs(source_root);
            (render_inferred_capsule_toml(&normalized)?, false, true)
        }
    };

    let mut parsed = capsule::types::manifest_v1::CapsuleManifestV1::from_toml(&origin_manifest)
        .map_err(|error| format!("validate Effective capsule.toml: {error}"))?;
    if let Some(static_web) = inferred_static_web {
        // The generated manifest keeps its run command; `[outputs.static_web]`
        // is additive, and re-validation below keeps the declaration inside
        // the same constraints an authored one must satisfy.
        parsed.outputs.static_web = Some(static_web);
        parsed
            .validate()
            .map_err(|error| format!("validate inferred [outputs.static_web]: {error}"))?;
    }
    let effective_manifest =
        if let Some(metadata) = work.store_metadata.as_ref().filter(|_| merge_store_draft) {
            parsed.name = metadata.name.clone();
            parsed.metadata.short_description = Some(metadata.short_description.clone());
            parsed.metadata.description = Some(metadata.full_description.clone());
            parsed.metadata.license = metadata.license.clone();
            parsed.metadata.tags = metadata.tags.clone();
            parsed.metadata.store =
                metadata
                    .primary_category
                    .as_ref()
                    .map(|category| StoreMetadataV1 {
                        category: category.clone(),
                        subcategory: metadata.primary_subcategory.clone(),
                    });
            if metadata.assets.is_some() {
                parsed.metadata.assets = metadata.assets.clone();
            }
            toml::to_string(&parsed)
                .map_err(|error| format!("serialize Effective capsule.toml: {error}"))?
        } else if preserve_exact_bytes {
            origin_manifest
        } else {
            toml::to_string(&parsed)
                .map_err(|error| format!("serialize Effective capsule.toml: {error}"))?
        };
    parsed
        .validate_for_interactive_capture()
        .map_err(|error| format!("Effective capsule.toml is outside Authoring v1: {error}"))?;
    let normalized =
        normalize_program_intent(draft_from_capsule_manifest_v1(&parsed).map_err(|error| {
            format!("derive Program Intent from Effective capsule.toml: {error}")
        })?)
        .map_err(|error| format!("normalize Effective Program Intent: {error}"))?;
    Ok((normalized, effective_manifest))
}

pub fn authoring_recipe_origin(
    source_root: &Path,
    work: &AuthoringWork,
) -> Result<&'static str, String> {
    let Some(overlay) = &work.source_overlay else {
        return Ok(if source_root.join("capsule.toml").is_file() {
            "existing_config"
        } else {
            "inferred"
        });
    };
    match overlay
        .manifest
        .get("kind")
        .and_then(serde_json::Value::as_str)
    {
        Some("capsule_toml") => Ok("existing_config"),
        Some("manual_command") => Ok("manual_setup"),
        _ => Err("Source Overlay recipe kind is missing or unsupported".to_string()),
    }
}

#[cfg(test)]
pub fn replay_capsule_toml(
    work: &AuthoringWork,
    normalized: &NormalizedProgramIntentEnvelopeV1,
) -> Result<String, String> {
    let Some(overlay) = &work.source_overlay else {
        return render_inferred_capsule_toml(normalized);
    };
    if overlay.source_revision_id != work.source_revision_id {
        return Err("Source Overlay targets a different immutable Source Revision".to_string());
    }
    let manifest: SourceOverlayManifestV1 = serde_json::from_value(overlay.manifest.clone())
        .map_err(|error| format!("decode Source Overlay manifest: {error}"))?;
    match manifest {
        SourceOverlayManifestV1::CapsuleToml {
            schema,
            capsule_toml,
            ..
        } => {
            require_overlay_schema(&schema)?;
            Ok(capsule_toml)
        }
        SourceOverlayManifestV1::ManualCommand { schema, .. } => {
            require_overlay_schema(&schema)?;
            render_inferred_capsule_toml(normalized)
        }
    }
}

fn require_overlay_schema(schema: &str) -> Result<(), String> {
    if schema == "ato.source-overlay/v1" {
        Ok(())
    } else {
        Err("unsupported Source Overlay schema".to_string())
    }
}

fn infer_deno_fresh_intent(
    source_root: &Path,
) -> Result<NormalizedProgramIntentEnvelopeV1, String> {
    if !source_root.join("main.ts").is_file() || !source_root.join("dev.ts").is_file() {
        return Err("Deno Fresh inference requires main.ts and dev.ts entrypoints".to_string());
    }
    let config: serde_json::Value = serde_json::from_slice(
        &std::fs::read(source_root.join("deno.json"))
            .map_err(|error| format!("read deno.json: {error}"))?,
    )
    .map_err(|error| format!("parse deno.json: {error}"))?;
    let start = config
        .get("tasks")
        .and_then(|tasks| tasks.get("start"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "Deno Fresh inference requires tasks.start in deno.json".to_string())?;
    let argv = parse_plain_task_argv(start)?;
    if argv.first().map(String::as_str) != Some("deno")
        || argv.last().map(String::as_str) != Some("dev.ts")
    {
        return Err("Deno Fresh tasks.start must directly launch deno with dev.ts".to_string());
    }
    normalize_program_intent(ProgramIntentDraftV1 {
        schema: capsule::authoring_intent::PROGRAM_INTENT_DRAFT_V1_SCHEMA.to_string(),
        origin: ProgramIntentOrigin::Inference,
        toolchains: Vec::new(),
        build_steps: Vec::new(),
        launch: ProgramCommandDraftV1::Argv {
            argv,
            cwd: WorkspacePathV1::root(),
            requested_environment: Vec::new(),
            required_tools: vec!["deno".to_string()],
        },
        readiness: ReadinessIntentV1::Http {
            port: 8000,
            path: "/".to_string(),
            timeout_seconds: 60,
        },
        build_output_roots: Vec::new(),
        bindings: Vec::new(),
        unresolved: Vec::new(),
    })
    .map_err(|error| error.to_string())
}

fn parse_plain_task_argv(command: &str) -> Result<Vec<String>, String> {
    if command.trim().is_empty()
        || command.chars().any(|character| {
            matches!(
                character,
                '\'' | '"' | '\\' | ';' | '|' | '&' | '$' | '<' | '>' | '(' | ')'
            ) || character.is_control()
        })
    {
        return Err(
            "Deno Fresh tasks.start must be a non-empty plain argv without shell syntax"
                .to_string(),
        );
    }
    Ok(command
        .split_ascii_whitespace()
        .map(str::to_string)
        .collect())
}

pub fn render_inferred_capsule_toml(
    normalized: &NormalizedProgramIntentEnvelopeV1,
) -> Result<String, String> {
    let program = normalized
        .intent
        .launch
        .argv
        .first()
        .map(String::as_str)
        .ok_or_else(|| "Program Intent launch argv is empty".to_string())?;
    let (port, path) = match &normalized.intent.readiness {
        ReadinessIntentV1::Http { port, path, .. } => (*port, path.as_str()),
        _ => {
            return Err("Authoring v1 manifest generation requires HTTP readiness".to_string());
        }
    };
    let url = format!("http://127.0.0.1:{port}{path}");
    let url_literal =
        serde_json::to_string(&url).map_err(|error| format!("encode readiness URL: {error}"))?;
    let seal_at = match program {
        "deno" => SealAtV1 {
            command: vec![
                "deno".to_string(),
                "eval".to_string(),
                format!("await fetch({url_literal})"),
            ],
            timeout_seconds: Some(30),
        },
        "node" | "npm" | "npx" | "yarn" | "pnpm" => SealAtV1 {
            command: vec![
                "node".to_string(),
                "--input-type=module".to_string(),
                "-e".to_string(),
                format!("await fetch({url_literal})"),
            ],
            timeout_seconds: Some(30),
        },
        "python" | "python3" => SealAtV1 {
            command: vec![
                "python3".to_string(),
                "-c".to_string(),
                format!(
                    "import urllib.request; urllib.request.urlopen({url_literal}, timeout=10).read()"
                ),
            ],
            timeout_seconds: Some(30),
        },
        _ => {
            return Err(format!(
                "no safe readiness command is available for manual runtime {program:?}"
            ));
        }
    };
    let manifest = to_capsule_manifest_v1(
        "authored-capsule".to_string(),
        "1.0.0".to_string(),
        &normalized.intent,
        seal_at,
    )
    .map_err(|error| error.to_string())?;
    toml::to_string(&manifest).map_err(|error| format!("serialize inferred capsule.toml: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authoring_job_claim_accepts_worker_fencing_generation() {
        let work: AuthoringWork = serde_json::from_value(serde_json::json!({
            "kind": "clean_replay",
            "work_id": "ajob_01KYN2Z",
            "worker_claim_id": "claim_01KYN2Z",
            "authoring_session_id": "auth_01KYN2Z",
            "capsule_revision_id": "caprev_01KYN2Z",
            "source_revision_id": "srev_01KYN2Z",
            "source_closure_id": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "pinned_source": {
                "source_revision_id": "srev_01KYN2Z",
                "source_materialization_id": "smat_01KYN2Z",
                "source_archive_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "source_archive_object_key": "authoring/srev_01KYN2Z.tar.gz",
                "source_tree_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            },
            "lease_token": "lease-token-with-at-least-thirty-two-bytes",
            "lease_expires_at": "2026-07-29T00:00:00.000Z",
            "trace_id": "trace_01KYN2Z"
        }))
        .expect("job claim");

        assert_eq!(work.worker_claim_id.as_deref(), Some("claim_01KYN2Z"));
    }

    #[test]
    fn setup_claim_may_omit_worker_fencing_generation() {
        let work: AuthoringWork = serde_json::from_value(serde_json::json!({
            "kind": "setup",
            "work_id": "setup_01KYN2Z",
            "authoring_session_id": "auth_01KYN2Z",
            "capsule_revision_id": "caprev_01KYN2Z",
            "source_revision_id": "srev_01KYN2Z",
            "source_closure_id": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "pinned_source": {
                "source_revision_id": "srev_01KYN2Z",
                "source_materialization_id": "smat_01KYN2Z",
                "source_archive_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "source_archive_object_key": "authoring/srev_01KYN2Z.tar.gz",
                "source_tree_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            },
            "lease_token": "lease-token-with-at-least-thirty-two-bytes",
            "lease_expires_at": "2026-07-29T00:00:00.000Z",
            "trace_id": "trace_01KYN2Z"
        }))
        .expect("setup claim");

        assert_eq!(work.worker_claim_id, None);
    }

    #[test]
    fn authoring_failure_diagnostic_is_api_safe_and_bounded() {
        let safe = safe_failure_message(&format!("capture\nfailed\u{0000}{}", "x".repeat(4096)));

        assert_eq!(safe.chars().count(), 2048);
        assert!(!safe.contains('\n'));
        assert!(!safe.contains('\u{0000}'));
        assert!(safe.starts_with("capture failed "));
    }

    #[test]
    fn http_rejection_keeps_bounded_safe_diagnostic_detail() {
        let body = serde_json::json!({
            "error": "clean_replay_receipt_rejected",
            "message": format!("binding mismatch\n{}", "x".repeat(600)),
        })
        .to_string();
        let (code, detail) = parse_http_rejection(&body, 409);

        assert_eq!(code, "clean_replay_receipt_rejected");
        let detail = detail.expect("detail");
        assert!(!detail.contains('\n'));
        assert_eq!(detail.chars().count(), 512);
    }

    #[test]
    fn screenshot_completion_retries_storage_failures_without_exposing_the_body() {
        let body = serde_json::json!({
            "error": "media_repair_storage_failed",
            "message": "D1 internal query and secret details",
            "trace_id": "request_01KYN2Z",
        })
        .to_string();
        let response = ureq::Response::new(503, "Service Unavailable", &body).expect("response");

        let error =
            screenshot_completion_http_error(ureq::Error::Status(503, response), "claim_trace");

        assert_eq!(
            error,
            ScreenshotCompletionError::RetryableHttp {
                status: 503,
                code: "media_repair_storage_failed".to_string(),
                trace_id: "request_01KYN2Z".to_string(),
            }
        );
        assert!(error.is_retryable());
        assert!(!error.to_string().contains("D1 internal"));
    }

    #[test]
    fn screenshot_completion_treats_domain_conflicts_as_terminal() {
        let body = serde_json::json!({
            "error": "media_repair_receipt_mismatch",
            "trace_id": "request_01KYN2Z",
        })
        .to_string();
        let response = ureq::Response::new(409, "Conflict", &body).expect("response");

        let error =
            screenshot_completion_http_error(ureq::Error::Status(409, response), "claim_trace");

        assert_eq!(
            error,
            ScreenshotCompletionError::Refused {
                status: 409,
                code: "media_repair_receipt_mismatch".to_string(),
                trace_id: "request_01KYN2Z".to_string(),
            }
        );
        assert!(!error.is_retryable());
    }

    #[test]
    fn screenshot_completion_retries_malformed_success_ack() {
        let response = ureq::Response::new(200, "OK", r#"{"accepted":"yes"}"#).expect("response");

        let error = decode_screenshot_completion_ack(response).expect_err("invalid ack");

        assert!(error.is_retryable());
        assert_eq!(
            error,
            ScreenshotCompletionError::RetryableHttp {
                status: 200,
                code: "media_repair_response_invalid".to_string(),
                trace_id: "none".to_string(),
            }
        );
    }

    #[test]
    fn screenshot_completion_uses_the_earlier_lease_or_receipt_deadline() {
        let deadline =
            screenshot_completion_deadline("2026-07-29T10:00:05.000Z", "2026-07-29T10:00:10.000Z")
                .expect("deadline");

        assert_eq!(
            deadline.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "2026-07-29T10:00:04.000Z"
        );
    }

    #[test]
    fn screenshot_completion_reports_the_signed_png_dimensions() {
        let receipt: MediaRepairReceiptV1 = serde_json::from_value(serde_json::json!({
            "payload_jcs_base64": "e30=",
            "authentication": {
                "key_id": "builder-key",
                "algorithm": "ed25519",
                "signature": "signed",
            },
        }))
        .expect("receipt envelope");

        let body = screenshot_completion_request_body(
            "builder-sugamo",
            "abjob_viewport",
            &receipt,
            "png",
            1280,
            720,
        );

        assert_eq!(
            body.pointer("/viewport/width"),
            Some(&serde_json::json!(1280))
        );
        assert_eq!(
            body.pointer("/viewport/height"),
            Some(&serde_json::json!(720))
        );
    }

    #[test]
    fn lease_token_debug_is_redacted() {
        let token: AuthoringLeaseToken =
            serde_json::from_str(&format!("\"{}\"", "s".repeat(48))).expect("token");
        assert_eq!(format!("{token:?}"), "AuthoringLeaseToken([REDACTED])");
    }

    #[test]
    fn hextris_static_inference_preserves_exact_argv() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(root.path().join("index.html"), "<canvas></canvas>").expect("fixture");
        let normalized = infer_authoring_intent(root.path()).expect("intent");
        assert_eq!(
            normalized.intent.launch.argv,
            ["python3", "-m", "http.server", "8000", "--bind", "0.0.0.0",]
        );
        let manifest = render_inferred_capsule_toml(&normalized).expect("manifest");
        let parsed =
            capsule::types::manifest_v1::CapsuleManifestV1::from_toml(&manifest).expect("v1");
        assert_eq!(parsed.run.command, normalized.intent.launch.argv);
        assert_eq!(parsed.web.expect("surface").port, 8000);
    }

    #[test]
    fn static_web_outputs_are_inferred_for_a_dependency_free_static_root() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(root.path().join("index.html"), "<canvas></canvas>").expect("fixture");
        let outputs = infer_static_web_outputs(root.path()).expect("static");
        assert_eq!(outputs.root, ".");
        assert_eq!(outputs.entry_path, "index.html");
        assert!(outputs.spa_fallback);
        assert!(outputs.connect_src.is_empty());
    }

    #[test]
    fn static_web_outputs_are_withheld_on_server_signals() {
        for server_entry in ["server.py", "app.py", "server.js"] {
            let root = tempfile::tempdir().expect("tempdir");
            std::fs::write(root.path().join("index.html"), "<div></div>").expect("fixture");
            std::fs::write(root.path().join(server_entry), "serve()").expect("server");
            assert!(
                infer_static_web_outputs(root.path()).is_none(),
                "{server_entry} must force the compute lane"
            );
        }
        // No index.html at all — nothing to declare.
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(root.path().join("README.md"), "docs").expect("fixture");
        assert!(infer_static_web_outputs(root.path()).is_none());
    }

    fn vite_fixture(config: Option<&str>) -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            root.path().join("package.json"),
            r#"{"name":"demo","scripts":{"dev":"vite","build":"vite build","preview":"vite preview"}}"#,
        )
        .expect("package");
        std::fs::write(root.path().join("package-lock.json"), "{}").expect("lockfile");
        if let Some(config) = config {
            std::fs::write(root.path().join("vite.config.ts"), config).expect("config");
        }
        root
    }

    #[test]
    fn vite_production_shape_declares_the_framework_output_dir() {
        let default_out = vite_fixture(Some("export default { plugins: [] }\n"));
        assert_eq!(
            infer_static_web_outputs(default_out.path()).expect("static").root,
            "dist"
        );
        let no_config = vite_fixture(None);
        assert_eq!(
            infer_static_web_outputs(no_config.path()).expect("static").root,
            "dist"
        );
        let overridden = vite_fixture(Some(
            "export default { build: { outDir: \"public/site\" } }\n",
        ));
        assert_eq!(
            infer_static_web_outputs(overridden.path()).expect("static").root,
            "public/site"
        );
    }

    #[test]
    fn undecidable_or_server_positive_vite_packages_stay_compute() {
        // outDir override this lane cannot read as a literal → no declaration.
        let dynamic = vite_fixture(Some("export default { build: { outDir: mode } }\n"));
        assert!(infer_static_web_outputs(dynamic.path()).is_none());
        // A server framework in the dependency set → no declaration.
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            root.path().join("package.json"),
            r#"{"name":"demo","dependencies":{"express":"^4"},"scripts":{"build":"vite build","preview":"vite preview"}}"#,
        )
        .expect("package");
        assert!(infer_static_web_outputs(root.path()).is_none());
        // A dev-server-only script shape (no plain build+preview) → compute.
        let dev_only = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dev_only.path().join("package.json"),
            r#"{"name":"demo","scripts":{"dev":"vite"}}"#,
        )
        .expect("package");
        assert!(infer_static_web_outputs(dev_only.path()).is_none());
    }

    #[test]
    fn package_managed_app_with_index_html_is_not_misclassified_as_static() {
        // Regression for ato-api#443: a package.json-carrying repository must
        // never fall into the dependency-free static-file path just because
        // it also has a root index.html (swagger-editor ships one as its
        // Vite dev-server template).
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(root.path().join("index.html"), "<div id=\"root\"></div>").expect("fixture");
        std::fs::write(
            root.path().join("package.json"),
            r#"{"name":"swagger-editor","scripts":{"start":"cross-env DISABLE_ESLINT_PLUGIN=false vite"}}"#,
        )
        .expect("package");
        std::fs::write(root.path().join("package-lock.json"), "{}").expect("lockfile");

        let normalized = infer_authoring_intent(root.path()).expect("intent");

        assert_eq!(
            normalized.intent.launch.argv,
            [
                "npm", "run", "start", "--", "--host", "0.0.0.0", "--port", "8000",
            ]
        );
        assert_eq!(normalized.intent.launch.required_tools, ["npm"]);
        let manifest = render_inferred_capsule_toml(&normalized).expect("manifest");
        let parsed =
            capsule::types::manifest_v1::CapsuleManifestV1::from_toml(&manifest).expect("v1");
        assert_eq!(parsed.run.command, normalized.intent.launch.argv);
        assert_eq!(parsed.seal_at.expect("seal_at").command[0], "node");
        assert_eq!(parsed.web.expect("surface").port, 8000);
    }

    #[test]
    fn vite_app_with_plain_build_and_preview_gets_the_production_lane() {
        // drawdb-shaped package.json: `dev: vite` plus plain `vite build` /
        // `vite preview` scripts. The published capsule must serve the BUILT
        // app — dev serving ships the unbundled module graph (measured 448
        // requests / 115 MB before first paint through the app proxy).
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            root.path().join("package.json"),
            r#"{"name":"drawdb","scripts":{"dev":"vite","build":"vite build","preview":"vite preview"}}"#,
        )
        .expect("package");
        std::fs::write(root.path().join("package-lock.json"), "{}").expect("lockfile");

        let normalized = infer_authoring_intent(root.path()).expect("intent");

        assert_eq!(
            normalized.intent.launch.argv,
            [
                "npm",
                "run",
                "preview",
                "--",
                "--host",
                "0.0.0.0",
                "--port",
                "8000",
                "--strictPort",
            ]
        );
        assert!(!normalized.intent.launch.explicit_shell_escape);
        assert_eq!(normalized.intent.launch.required_tools, ["npm"]);
        match &normalized.intent.readiness {
            ReadinessIntentV1::Http { port, path, .. } => {
                assert_eq!((*port, path.as_str()), (8000, "/"));
            }
            other => panic!("expected HTTP readiness, got {other:?}"),
        }
        let manifest = render_inferred_capsule_toml(&normalized).expect("manifest");
        let parsed =
            capsule::types::manifest_v1::CapsuleManifestV1::from_toml(&manifest).expect("v1");
        assert_eq!(parsed.run.command, normalized.intent.launch.argv);
        assert_eq!(parsed.seal_at.expect("seal_at").command[0], "node");
        assert_eq!(parsed.web.expect("surface").port, 8000);
    }

    #[test]
    fn vite_app_with_compound_build_keeps_the_dev_lane() {
        // swagger-editor-shaped: the build script chains sub-builds with `&&`,
        // so the production lane cannot claim it plainly — fail closed to the
        // dev-server lane rather than guess which sub-build serves.
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            root.path().join("package.json"),
            r#"{"name":"swagger-editor","scripts":{"start":"cross-env A=1 vite","build":"npm run build:app && npm run build:bundle","preview":"vite preview"}}"#,
        )
        .expect("package");
        std::fs::write(root.path().join("package-lock.json"), "{}").expect("lockfile");

        let normalized = infer_authoring_intent(root.path()).expect("intent");
        assert_eq!(normalized.intent.launch.argv[0], "npm");
        assert_eq!(normalized.intent.launch.argv[2], "start");
    }

    #[test]
    fn vite_app_without_a_preview_script_keeps_the_dev_lane() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            root.path().join("package.json"),
            r#"{"name":"app","scripts":{"dev":"vite","build":"vite build"}}"#,
        )
        .expect("package");

        let normalized = infer_authoring_intent(root.path()).expect("intent");
        assert_eq!(normalized.intent.launch.argv[0], "npm");
        assert_eq!(normalized.intent.launch.argv[2], "dev");
    }

    #[test]
    fn package_managed_inference_prefers_declared_package_manager() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            root.path().join("package.json"),
            r#"{"name":"app","packageManager":"pnpm@9.1.0","scripts":{"dev":"vite"}}"#,
        )
        .expect("package");
        // A stray npm lockfile must not win over the explicit declaration.
        std::fs::write(root.path().join("package-lock.json"), "{}").expect("lockfile");

        let normalized = infer_authoring_intent(root.path()).expect("intent");

        assert_eq!(normalized.intent.launch.argv[0], "pnpm");
        assert_eq!(normalized.intent.launch.argv[2], "dev");
        assert_eq!(normalized.intent.launch.required_tools, ["pnpm"]);
    }

    #[test]
    fn package_managed_inference_fails_closed_without_a_start_or_dev_script() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(root.path().join("package.json"), r#"{"name":"static-app"}"#)
            .expect("package");

        assert!(infer_authoring_intent(root.path()).is_err());
    }

    #[test]
    fn package_managed_inference_fails_closed_for_an_unrecognized_dev_server() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            root.path().join("package.json"),
            r#"{"name":"app","scripts":{"start":"node server.js"}}"#,
        )
        .expect("package");

        let error = infer_authoring_intent(root.path()).expect_err("must fail closed");
        assert!(error.contains("manual setup required"));
    }

    #[test]
    fn package_managed_inference_refuses_shell_task_syntax() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            root.path().join("package.json"),
            r#"{"name":"app","scripts":{"start":"PORT=8000 && vite"}}"#,
        )
        .expect("package");

        assert!(infer_authoring_intent(root.path()).is_err());
    }

    #[test]
    fn package_managed_inference_fails_closed_on_ambiguous_lockfiles() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            root.path().join("package.json"),
            r#"{"name":"app","scripts":{"start":"vite"}}"#,
        )
        .expect("package");
        std::fs::write(root.path().join("package-lock.json"), "{}").expect("npm lockfile");
        std::fs::write(root.path().join("yarn.lock"), "").expect("yarn lockfile");

        let error = infer_authoring_intent(root.path()).expect_err("must fail closed");
        assert!(error.contains("ambiguous package manager"));
    }

    #[test]
    fn inference_does_not_guess_without_a_root_entrypoint() {
        let root = tempfile::tempdir().expect("tempdir");
        assert!(infer_authoring_intent(root.path()).is_err());
    }

    #[test]
    fn deno_fresh_inference_preserves_exact_plain_start_argv() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(root.path().join("main.ts"), "").expect("main");
        std::fs::write(root.path().join("dev.ts"), "").expect("dev");
        std::fs::write(
            root.path().join("deno.json"),
            r#"{"tasks":{"start":"deno run -A --unstable --watch=static/,routes/ dev.ts"}}"#,
        )
        .expect("config");

        let normalized = infer_authoring_intent(root.path()).expect("intent");
        assert_eq!(
            normalized.intent.launch.argv,
            [
                "deno",
                "run",
                "-A",
                "--unstable",
                "--watch=static/,routes/",
                "dev.ts"
            ]
        );
        assert_eq!(normalized.intent.launch.required_tools, ["deno"]);
        let manifest = render_inferred_capsule_toml(&normalized).expect("manifest");
        let parsed =
            capsule::types::manifest_v1::CapsuleManifestV1::from_toml(&manifest).expect("v1");
        assert_eq!(parsed.run.command, normalized.intent.launch.argv);
        assert_eq!(parsed.seal_at.expect("seal_at").command[0], "deno");
    }

    #[test]
    fn deno_fresh_inference_refuses_shell_task_syntax() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(root.path().join("main.ts"), "").expect("main");
        std::fs::write(root.path().join("dev.ts"), "").expect("dev");
        std::fs::write(
            root.path().join("deno.json"),
            r#"{"tasks":{"start":"PORT=8000 deno run -A dev.ts"}}"#,
        )
        .expect("config");

        assert!(infer_authoring_intent(root.path()).is_err());
    }

    #[test]
    fn source_capsule_toml_wins_over_inference_and_is_not_overwritten() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(root.path().join("index.html"), "fixture").expect("source");
        let capsule_toml = r#"schema_version = "1"
name = "source-owned"
version = "2.0.0"

[metadata]
short_description = "kept byte-for-byte"

[run]
command = ["python3", "-m", "http.server", "4310", "--bind", "0.0.0.0"]

[web]
port = 4310
bind = "0.0.0.0"

[seal_at]
command = ["python3", "-c", "print('ready')"]
timeout_seconds = 30
"#;
        std::fs::write(root.path().join("capsule.toml"), capsule_toml).expect("manifest");
        let work: AuthoringWork = serde_json::from_value(serde_json::json!({
            "kind": "setup",
            "work_id": "setup_source_manifest",
            "authoring_session_id": "auth_source_manifest",
            "capsule_revision_id": "caprev_source_manifest",
            "source_revision_id": "srev_source_manifest",
            "source_closure_id": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "pinned_source": {
                "source_revision_id": "srev_source_manifest",
                "source_materialization_id": "smat_source_manifest",
                "source_archive_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "source_archive_object_key": "authoring/srev_source_manifest.tar.zst",
                "source_tree_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            },
            "setup_mode": "suggested",
            "lease_token": "lease-token-with-at-least-thirty-two-bytes",
            "lease_expires_at": "2026-07-29T00:00:00.000Z",
            "trace_id": "trace_source_manifest"
        }))
        .expect("claim");

        let (normalized, exact_toml) =
            resolve_authoring_recipe(root.path(), &work).expect("source recipe");

        assert_eq!(exact_toml, capsule_toml);
        assert_eq!(normalized.intent.launch.argv[3], "4310");
        assert_eq!(
            std::fs::read_to_string(root.path().join("capsule.toml")).expect("unchanged manifest"),
            capsule_toml
        );
    }

    #[test]
    fn manual_command_overlay_generates_a_replayable_manifest() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(root.path().join("deno.json"), r#"{"tasks":{}}"#).expect("config");
        std::fs::write(root.path().join("main.ts"), "").expect("main");
        std::fs::write(root.path().join("dev.ts"), "").expect("dev");
        let work: AuthoringWork = serde_json::from_value(serde_json::json!({
            "kind": "setup",
            "work_id": "setup_manual",
            "authoring_session_id": "auth_manual",
            "capsule_revision_id": "caprev_manual",
            "source_revision_id": "srev_manual",
            "source_closure_id": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "pinned_source": {
                "source_revision_id": "srev_manual",
                "source_materialization_id": "smat_manual",
                "source_archive_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "source_archive_object_key": "authoring/srev_manual.tar.gz",
                "source_tree_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            },
            "source_overlay": {
                "source_overlay_id": "overlay_manual",
                "source_revision_id": "srev_manual",
                "overlay_digest": "blake3:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                "manifest": {
                    "schema": "ato.source-overlay/v1",
                    "kind": "manual_command",
                    "launch_argv": ["deno", "run", "-A", "dev.ts"],
                    "port": 8000,
                    "readiness_path": "/"
                }
            },
            "setup_mode": "manual",
            "setup_journal_sequence": 4,
            "lease_token": "lease-token-with-at-least-thirty-two-bytes",
            "lease_expires_at": "2026-07-29T00:00:00.000Z",
            "trace_id": "trace_manual"
        }))
        .expect("manual claim");

        let (normalized, capsule_toml) =
            resolve_authoring_recipe(root.path(), &work).expect("manual recipe");

        assert_eq!(
            authoring_recipe_origin(root.path(), &work).expect("origin"),
            "manual_setup"
        );
        assert_eq!(
            normalized.intent.launch.argv,
            ["deno", "run", "-A", "dev.ts"]
        );
        let parsed = capsule::types::manifest_v1::CapsuleManifestV1::from_toml(&capsule_toml)
            .expect("generated v1");
        assert_eq!(parsed.web.expect("web").port, 8000);
        assert_eq!(
            replay_capsule_toml(&work, &normalized).expect("replay manifest"),
            capsule_toml
        );
    }

    #[test]
    fn manual_node_package_manager_commands_use_node_for_readiness_without_rewriting_argv() {
        for program in ["yarn", "pnpm"] {
            let normalized = normalize_program_intent(ProgramIntentDraftV1 {
                schema: capsule::authoring_intent::PROGRAM_INTENT_DRAFT_V1_SCHEMA.to_string(),
                origin: ProgramIntentOrigin::ManualSetup,
                toolchains: Vec::new(),
                build_steps: Vec::new(),
                launch: ProgramCommandDraftV1::Argv {
                    argv: vec![
                        program.to_string(),
                        "start".to_string(),
                        "--host".to_string(),
                        "0.0.0.0".to_string(),
                    ],
                    cwd: WorkspacePathV1::root(),
                    requested_environment: Vec::new(),
                    required_tools: vec![program.to_string()],
                },
                readiness: ReadinessIntentV1::Http {
                    port: 5173,
                    path: "/".to_string(),
                    timeout_seconds: 60,
                },
                build_output_roots: Vec::new(),
                bindings: Vec::new(),
                unresolved: Vec::new(),
            })
            .expect("manual intent");

            let capsule_toml =
                render_inferred_capsule_toml(&normalized).expect("generated manifest");
            let parsed = capsule::types::manifest_v1::CapsuleManifestV1::from_toml(&capsule_toml)
                .expect("v1 manifest");
            assert_eq!(parsed.run.command, normalized.intent.launch.argv);
            assert_eq!(parsed.web.expect("web").port, 5173);
            assert_eq!(parsed.seal_at.expect("seal_at").command[0], "node");
        }
    }

    #[test]
    fn edited_capsule_toml_rebuild_preserves_bytes_and_http_readiness() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(root.path().join("index.html"), "<canvas></canvas>").expect("fixture");
        let capsule_toml = r#"schema_version = "1"
name = "hextris"
version = "1.0.0"

[run]
command = ["python3", "-m", "http.server", "8000", "--bind", "0.0.0.0"]

[web]
port = 8000
bind = "0.0.0.0"

[seal_at]
command = ["python3", "-c", "print('ready')"]
timeout_seconds = 30
"#;
        let work: AuthoringWork = serde_json::from_value(serde_json::json!({
            "kind": "setup",
            "work_id": "setup_edited",
            "authoring_session_id": "auth_edited",
            "capsule_revision_id": "caprev_edited",
            "source_revision_id": "srev_edited",
            "source_closure_id": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "pinned_source": {
                "source_revision_id": "srev_edited",
                "source_materialization_id": "smat_edited",
                "source_archive_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "source_archive_object_key": "authoring/srev_edited.tar.gz",
                "source_tree_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            },
            "source_overlay": {
                "source_overlay_id": "overlay_edited",
                "source_revision_id": "srev_edited",
                "overlay_digest": "blake3:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                "manifest": {
                    "schema": "ato.source-overlay/v1",
                    "kind": "capsule_toml",
                    "capsule_toml": capsule_toml
                }
            },
            "store_metadata": {
                "name": "stale-listing-name",
                "short_description": "stale listing draft",
                "full_description": "must not replace the later Manifest overlay",
                "tags": []
            },
            "setup_mode": "manual",
            "setup_journal_sequence": 2,
            "lease_token": "lease-token-with-at-least-thirty-two-bytes",
            "lease_expires_at": "2026-07-29T00:00:00.000Z",
            "trace_id": "trace_edited"
        }))
        .expect("edited claim");

        let (normalized, exact_toml) =
            resolve_authoring_recipe(root.path(), &work).expect("edited recipe");

        assert_eq!(
            authoring_recipe_origin(root.path(), &work).expect("origin"),
            "existing_config"
        );
        assert_eq!(
            normalized.intent.readiness,
            ReadinessIntentV1::Http {
                port: 8000,
                path: "/".to_string(),
                timeout_seconds: 60,
            }
        );
        assert_eq!(exact_toml, capsule_toml);
        assert_eq!(
            replay_capsule_toml(&work, &normalized).expect("replay manifest"),
            capsule_toml
        );
    }
}
