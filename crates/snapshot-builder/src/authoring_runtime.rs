//! Authoring Session v1 control-plane client and source-only inference.
//!
//! This module deliberately owns transport and pure inference only. Execution
//! remains in the snapshot builder's existing pinned-source and Firecracker
//! lanes, so the Authoring Session cannot grow a second build contract.

use std::fmt;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule::authoring_intent::{
    NormalizedProgramIntentEnvelopeV1, ProgramCommandDraftV1, ProgramIntentDraftV1,
    ProgramIntentOrigin, ReadinessIntentV1, WorkspacePathV1, draft_from_capsule_manifest_v1,
    normalize_program_intent, to_capsule_manifest_v1,
};
use capsule::types::manifest_v1::SealAtV1;
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest as _, Sha256};
use snapshot::archive_only_build::ArchiveOnlyBuildInput;
use snapshot::authoring_evidence::{
    BuilderAuthenticationV1, ClassifiedStateDiffV1, CleanReplayReceiptV1, ReadyStateSealReceiptV1,
};

const AUTHORING_BASE_PATH: &str = "/v1/capsule-snapshots/authoring";

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
    pub previous_receipt_digest: Option<String>,
    #[serde(default)]
    pub setup_mode: Option<String>,
    #[serde(default)]
    pub source_overlay: Option<serde_json::Value>,
    #[serde(default, rename = "store_metadata")]
    pub _store_metadata: Option<serde_json::Value>,
    #[serde(default, rename = "setup_journal_sequence")]
    pub _setup_journal_sequence: Option<u64>,
    #[serde(default)]
    pub normalized_program_intent: Option<NormalizedProgramIntentEnvelopeV1>,
    #[serde(default)]
    pub resolution_lock_digest: Option<String>,
    #[serde(default)]
    pub build_config_revision_id: Option<String>,
    #[serde(default)]
    pub build_attempt_number: Option<u64>,
    #[serde(default)]
    pub authoring_toml: Option<String>,
    #[serde(default)]
    pub authoring_toml_digest: Option<String>,
    #[serde(default)]
    pub effective_build_plan: Option<serde_json::Value>,
    #[serde(default)]
    pub plan_digest: Option<String>,
    #[serde(default)]
    #[serde(rename = "request")]
    pub _request: Option<serde_json::Value>,
    #[serde(default)]
    pub clean_replay_receipt: Option<CleanReplayReceiptV1>,
    #[serde(default)]
    pub classified_state_diff: Option<ClassifiedStateDiffV1>,
    /// Existing seal evidence is present on the shared Authoring claim shape
    /// even when the current operation is a clean Build Attempt. Keep it in
    /// the strict decoder so unrelated optional evidence cannot break claims.
    #[serde(default, rename = "ready_state_seal_receipt")]
    pub _ready_state_seal_receipt: Option<ReadyStateSealReceiptV1>,
    pub lease_token: AuthoringLeaseToken,
    #[serde(rename = "lease_expires_at")]
    pub _lease_expires_at: String,
    pub trace_id: String,
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
}

#[derive(Debug, Deserialize)]
pub struct BuildEventAppendAck {
    #[serde(rename = "lastSequence")]
    pub last_sequence: u64,
    pub truncated: bool,
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

    pub fn mark_setup_detected(
        &self,
        work: &AuthoringWork,
        input: &SetupDetected<'_>,
    ) -> Result<(), String> {
        ureq::post(&format!(
            "{}{AUTHORING_BASE_PATH}/setup/{}/detected",
            self.api_url.trim_end_matches('/'),
            work.work_id
        ))
        .set("authorization", &format!("Bearer {}", self.builder_token))
        .set("x-ato-authoring-lease-token", work.lease_token.expose())
        .send_json(
            serde_json::to_value(input)
                .map_err(|error| format!("encode setup detection evidence: {error}"))?,
        )
        .map_err(|error| http_error("report setup detection", error))?;
        Ok(())
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
        ureq::get(&format!(
            "{}{AUTHORING_BASE_PATH}/setup/{}/control",
            self.api_url.trim_end_matches('/'),
            work.work_id
        ))
        .set("authorization", &format!("Bearer {}", self.builder_token))
        .set("x-ato-authoring-lease-token", work.lease_token.expose())
        .query("builder_id", self.builder_id)
        .call()
        .map_err(|error| http_error("poll setup control", error))?
        .into_json()
        .map_err(|error| format!("decode setup control: {error}"))
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

    pub fn append_build_events(
        &self,
        work: &AuthoringWork,
        expected_previous_sequence: u64,
        events: &[serde_json::Value],
    ) -> Result<BuildEventAppendAck, String> {
        let worker_claim_id = work
            .worker_claim_id
            .as_deref()
            .ok_or_else(|| "Build Attempt claim omitted worker_claim_id".to_string())?;
        ureq::post(&format!(
            "{}{AUTHORING_BASE_PATH}/jobs/{}/events",
            self.api_url.trim_end_matches('/'),
            work.work_id
        ))
        .set("authorization", &format!("Bearer {}", self.builder_token))
        .set("x-ato-authoring-lease-token", work.lease_token.expose())
        .send_json(serde_json::json!({
            "builder_id": self.builder_id,
            "worker_claim_id": worker_claim_id,
            "expected_previous_sequence": expected_previous_sequence,
            "events": events,
        }))
        .map_err(|error| http_error("append Build Attempt events", error))?
        .into_json::<BuildEventAppendAck>()
        .map_err(|error| format!("decode Build Attempt event acknowledgement: {error}"))
    }

    pub fn fail_job(
        &self,
        work: &AuthoringWork,
        error_code: &str,
        error_message: &str,
    ) -> Result<(), String> {
        let worker_claim_id = work
            .worker_claim_id
            .as_deref()
            .ok_or_else(|| "Authoring job claim omitted worker_claim_id".to_string())?;
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
            "error_message": bounded_diagnostic(error_message),
        }))
        .map_err(|error| http_error("report Authoring job failure", error))?;
        Ok(())
    }

    pub fn fail_setup(
        &self,
        work: &AuthoringWork,
        stage: &str,
        error_code: &str,
        error_message: &str,
    ) -> Result<(), String> {
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
            "error_message": bounded_diagnostic(error_message),
        }))
        .map_err(|error| http_error("report setup detection failure", error))?;
        Ok(())
    }

    pub fn complete_clean_replay(
        &self,
        work: &AuthoringWork,
        receipt: &CleanReplayReceiptV1,
        classified_state_diff: &ClassifiedStateDiffV1,
        execution_contract_jcs: &str,
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
            "execution_contract_jcs_base64": BASE64.encode(execution_contract_jcs.as_bytes()),
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
}

#[derive(Debug, Serialize)]
pub struct DetectorProvenance<'a> {
    pub producer: &'a str,
    pub inputs: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SetupDetected<'a> {
    pub builder_id: &'a str,
    pub origin: &'a str,
    pub normalized_program_intent: &'a NormalizedProgramIntentEnvelopeV1,
    pub source_closure_id: &'a str,
    pub generated_capsule_toml: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detector: Option<DetectorProvenance<'a>>,
    pub materialized_assets: Vec<serde_json::Value>,
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
    pub materialized_assets: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SetupControl {
    pub action: String,
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
    if source.source_revision_id != work.source_revision_id
        || source.source_tree_digest != work.source_closure_id
    {
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

fn bounded_diagnostic(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() || *character == '\t')
        .take(2_048)
        .collect()
}

/// Infer the narrow static-Web subset used by the first browser E2E.
///
/// The inference is intentionally source-based and fail-closed. A repository
/// without a root `index.html` remains unresolved rather than being launched
/// with a guessed framework command.
pub fn infer_static_web_intent(
    source_root: &Path,
) -> Result<NormalizedProgramIntentEnvelopeV1, String> {
    if !source_root.join("index.html").is_file() {
        return Err("static Web inference requires a root index.html".to_string());
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

pub fn render_static_web_capsule_toml(
    normalized: &NormalizedProgramIntentEnvelopeV1,
) -> Result<String, String> {
    let manifest = to_capsule_manifest_v1(
        "hextris".to_string(),
        "1.0.0".to_string(),
        &normalized.intent,
        SealAtV1 {
            command: vec![
                "python3".to_string(),
                "-c".to_string(),
                "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8000/', timeout=10).read()"
                    .to_string(),
            ],
            timeout_seconds: Some(30),
        },
    )
    .map_err(|error| error.to_string())?;
    toml::to_string(&manifest).map_err(|error| format!("serialize inferred capsule.toml: {error}"))
}

pub fn normalize_capsule_toml(
    capsule_toml: &str,
) -> Result<NormalizedProgramIntentEnvelopeV1, String> {
    let manifest = capsule::types::manifest_v1::CapsuleManifestV1::from_toml(capsule_toml)
        .map_err(|error| format!("parse authored capsule.toml: {error}"))?;
    let draft = draft_from_capsule_manifest_v1(&manifest)
        .map_err(|error| format!("derive Program Intent from capsule.toml: {error}"))?;
    normalize_program_intent(draft).map_err(|error| error.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedBuildContract {
    pub timeout_seconds: u64,
}

fn sha256_digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn effective_plan_digest(plan: &serde_json::Value) -> Result<String, String> {
    let mut execution_plan = plan.clone();
    execution_plan
        .as_object_mut()
        .ok_or_else(|| "Effective Build Plan must be an object".to_string())?
        .remove("identities");
    let canonical = serde_jcs::to_vec(&execution_plan)
        .map_err(|error| format!("canonicalize Effective Build Plan: {error}"))?;
    Ok(sha256_digest(&canonical))
}

fn json_string_array(
    value: Option<&serde_json::Value>,
    field: &str,
) -> Result<Vec<String>, String> {
    value
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("Effective Build Plan {field} must be an array"))?
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("Effective Build Plan {field}[{index}] must be a string"))
        })
        .collect()
}

fn secret_shaped_name(name: &str) -> bool {
    let normalized = name.to_ascii_uppercase();
    [
        "SECRET",
        "TOKEN",
        "PASSWORD",
        "PASSWD",
        "PRIVATE_KEY",
        "API_KEY",
        "CREDENTIAL",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn credential_shaped_value(value: &str) -> bool {
    value.contains("-----BEGIN ")
        || value.to_ascii_lowercase().starts_with("bearer ")
        || value.starts_with("ghp_")
        || value.starts_with("github_pat_")
        || value.starts_with("sk-")
}

/// Revalidate the immutable Authoring Draft and Effective Build Plan at the
/// execution boundary. The API's projection is useful UX, but it is not a
/// substitute for the Builder proving that the commands it will run are the
/// commands committed by the claimed revision.
pub fn validate_build_contract(work: &AuthoringWork) -> Result<ValidatedBuildContract, String> {
    let authored_toml = work
        .authoring_toml
        .as_deref()
        .ok_or_else(|| "Build claim omitted immutable authoring_toml".to_string())?;
    let authored_digest = work
        .authoring_toml_digest
        .as_deref()
        .ok_or_else(|| "Build claim omitted authoring_toml_digest".to_string())?;
    if sha256_digest(authored_toml.as_bytes()) != authored_digest {
        return Err("Build claim authoring_toml_digest mismatch".to_string());
    }

    let supplied_intent = work
        .normalized_program_intent
        .as_ref()
        .ok_or_else(|| "Build claim omitted Normalized Program Intent".to_string())?;
    let recomputed_intent = normalize_capsule_toml(authored_toml)?;
    if &recomputed_intent != supplied_intent {
        return Err("capsule.toml and Normalized Program Intent differ".to_string());
    }
    let manifest = capsule::types::manifest_v1::CapsuleManifestV1::from_toml(authored_toml)
        .map_err(|error| format!("parse immutable capsule.toml: {error}"))?;
    if let Some((name, _)) = manifest
        .env
        .iter()
        .find(|(name, value)| secret_shaped_name(name) || credential_shaped_value(value))
    {
        return Err(format!(
            "[env].{name} looks secret-bearing; use a declared secret reference"
        ));
    }

    let plan = work
        .effective_build_plan
        .as_ref()
        .ok_or_else(|| "Build claim omitted Effective Build Plan".to_string())?;
    let supplied_plan_digest = work
        .plan_digest
        .as_deref()
        .ok_or_else(|| "Build claim omitted plan_digest".to_string())?;
    if effective_plan_digest(plan)? != supplied_plan_digest {
        return Err("Effective Build Plan digest mismatch".to_string());
    }
    if plan.get("schema").and_then(serde_json::Value::as_str) != Some("ato.effective-build-plan/v1")
    {
        return Err("unsupported Effective Build Plan schema".to_string());
    }
    let identities = plan
        .get("identities")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "Effective Build Plan omitted identities".to_string())?;
    if identities
        .get("source_revision_id")
        .and_then(serde_json::Value::as_str)
        != Some(work.source_revision_id.as_str())
        || identities
            .get("program_intent_digest")
            .and_then(serde_json::Value::as_str)
            != Some(supplied_intent.digest.as_str())
    {
        return Err("Effective Build Plan identity binding mismatch".to_string());
    }

    let steps = plan
        .get("steps")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Effective Build Plan omitted steps".to_string())?;
    let command_step = |step_id: &str| {
        steps
            .iter()
            .find(|step| step.get("step_id").and_then(serde_json::Value::as_str) == Some(step_id))
    };
    let planned_build = steps
        .iter()
        .filter(|step| {
            step.get("step_id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|step_id| step_id.starts_with("build.user."))
        })
        .collect::<Vec<_>>();
    if planned_build.len() != supplied_intent.intent.build_steps.len() {
        return Err("Effective Build Plan build step count differs from capsule.toml".to_string());
    }
    for (index, (planned, normalized)) in planned_build
        .iter()
        .zip(&supplied_intent.intent.build_steps)
        .enumerate()
    {
        let argv = json_string_array(planned.get("command_argv"), "command_argv")?;
        let cwd = planned
            .get("cwd")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("build.user.{} omitted cwd", index + 1))?;
        if argv != normalized.argv || cwd != normalized.cwd.as_str() {
            return Err(format!(
                "Effective Build Plan build.user.{} differs from capsule.toml",
                index + 1
            ));
        }
    }
    let launch =
        command_step("launch").ok_or_else(|| "Effective Build Plan omitted launch".to_string())?;
    let launch_argv = json_string_array(launch.get("command_argv"), "launch.command_argv")?;
    let launch_cwd = launch
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "Effective Build Plan launch omitted cwd".to_string())?;
    if launch_argv != supplied_intent.intent.launch.argv
        || launch_cwd != supplied_intent.intent.launch.cwd.as_str()
    {
        return Err("Effective Build Plan launch differs from capsule.toml".to_string());
    }
    let readiness = plan
        .pointer("/conditions/readiness")
        .ok_or_else(|| "Effective Build Plan omitted readiness".to_string())?;
    if readiness
        != &serde_json::to_value(&supplied_intent.intent.readiness)
            .map_err(|error| format!("serialize normalized readiness: {error}"))?
    {
        return Err("Effective Build Plan readiness differs from capsule.toml".to_string());
    }
    let network = plan
        .pointer("/conditions/network")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "Effective Build Plan omitted network condition".to_string())?;
    let expected_network = if supplied_intent.intent.build_steps.is_empty() {
        "disabled"
    } else {
        "enabled"
    };
    if network != expected_network {
        return Err("Effective Build Plan network condition differs from execution".to_string());
    }
    let timeout_seconds = plan
        .pointer("/conditions/timeout_seconds")
        .and_then(serde_json::Value::as_u64)
        .filter(|timeout| (1..=3600).contains(timeout))
        .ok_or_else(|| "Effective Build Plan timeout is invalid".to_string())?;

    Ok(ValidatedBuildContract { timeout_seconds })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_plan_digest_matches_the_api_fixed_jcs_fixture() {
        let fixture = serde_json::json!({
            "schema": "ato.effective-build-plan/v1",
            "identities": {
                "source_revision_id": "srev_fixture",
                "program_intent_digest": "blake3:fixture"
            },
            "conditions": { "timeout_seconds": 60, "network": "disabled" },
            "steps": [{
                "step_id": "launch",
                "command_argv": ["python3", "-m", "http.server", "8000", ""],
                "cwd": "."
            }]
        });

        assert_eq!(
            effective_plan_digest(&fixture).expect("fixture digest"),
            "sha256:d7dd11c86d13078e2ad4a98fc6c1e908485c51c5c422ff4235ec56a7d2fc87eb"
        );
    }

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
    fn lease_token_debug_is_redacted() {
        let token: AuthoringLeaseToken =
            serde_json::from_str(&format!("\"{}\"", "s".repeat(48))).expect("token");
        assert_eq!(format!("{token:?}"), "AuthoringLeaseToken([REDACTED])");
    }

    #[test]
    fn hextris_static_inference_preserves_exact_argv() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(root.path().join("index.html"), "<canvas></canvas>").expect("fixture");
        let normalized = infer_static_web_intent(root.path()).expect("intent");
        assert_eq!(
            normalized.intent.launch.argv,
            ["python3", "-m", "http.server", "8000", "--bind", "0.0.0.0",]
        );
        let manifest = render_static_web_capsule_toml(&normalized).expect("manifest");
        let parsed =
            capsule::types::manifest_v1::CapsuleManifestV1::from_toml(&manifest).expect("v1");
        assert_eq!(parsed.run.command, normalized.intent.launch.argv);
        assert_eq!(parsed.web.expect("surface").port, 8000);
        let authored = normalize_capsule_toml(&manifest).expect("normalize authored manifest");
        assert_eq!(authored.intent.launch.argv, normalized.intent.launch.argv);
    }

    #[test]
    fn inference_does_not_guess_without_a_root_entrypoint() {
        let root = tempfile::tempdir().expect("tempdir");
        assert!(infer_static_web_intent(root.path()).is_err());
    }

    fn build_contract_work() -> AuthoringWork {
        let authored_toml = r#"schema_version = "1"
name = "contract-test"
version = "1.0.0"

[source]
root = "."
ignore = []

[run]
command = ["python3", "-m", "http.server", "8000", ""]

[web]
port = 8000
bind = "0.0.0.0"

[seal_at]
command = ["python3", "-c", "print('ready')"]
timeout_seconds = 60
"#;
        let normalized = normalize_capsule_toml(authored_toml).expect("normalize fixture");
        let provenance = serde_json::json!({
            "kind": "explicit",
            "producer": "capsule.toml",
            "inputs": ["capsule.toml"],
            "status": "explicit"
        });
        let system = serde_json::json!({
            "kind": "system_policy",
            "producer": "Ato authoring policy",
            "inputs": [],
            "status": "system"
        });
        let plan = serde_json::json!({
            "schema": "ato.effective-build-plan/v1",
            "identities": {
                "source_revision_id": "srev_contract",
                "source_overlay_id": "overlay_contract",
                "program_intent_id": "intent_contract",
                "program_intent_digest": normalized.digest,
            },
            "builder": {
                "image": "ato/snapshot-builder",
                "image_digest": null,
                "runtime": "firecracker",
                "source": system,
            },
            "source": { "root": ".", "effective_ignore": [], "source": provenance },
            "conditions": {
                "network": "disabled",
                "timeout_seconds": 60,
                "readiness": normalized.intent.readiness,
                "surfaces": [{"kind":"web","protocol":"http","port":8000,"bind":"0.0.0.0"}],
            },
            "toolchains": [],
            "steps": [
                {"step_id":"source.materialization","name":"Source materialization","source":system,"environment_names":[],"initial_status":"pending"},
                {"step_id":"metadata.validation","name":"Metadata validation","source":provenance,"environment_names":[],"initial_status":"pending"},
                {"step_id":"builder.provisioning","name":"Builder provisioning","source":system,"environment_names":[],"initial_status":"pending"},
                {"step_id":"runtime.provisioning","name":"Runtime provisioning","source":provenance,"environment_names":[],"initial_status":"pending"},
                {"step_id":"dependencies.installation","name":"Dependency installation","source":provenance,"environment_names":[],"initial_status":"skipped"},
                {"step_id":"build.user","name":"User build commands","source":provenance,"environment_names":[],"initial_status":"skipped"},
                {"step_id":"launch","name":"Launch","source":provenance,"command_argv":["python3","-m","http.server","8000",""],"cwd":".","environment_names":[],"initial_status":"pending"},
                {"step_id":"readiness","name":"Readiness check","source":provenance,"command_argv":["python3","-c","print('ready')"],"cwd":".","environment_names":[],"initial_status":"pending"},
                {"step_id":"preview.preparation","name":"Preview preparation","source":system,"environment_names":[],"initial_status":"pending"},
                {"step_id":"ready_state.seal","name":"Ready-State Seal","source":system,"environment_names":[],"initial_status":"pending"}
            ],
            "field_sources": {}
        });
        let mut execution_plan = plan.clone();
        execution_plan
            .as_object_mut()
            .expect("plan object")
            .remove("identities");
        let plan_digest = sha256_digest(&serde_jcs::to_vec(&execution_plan).expect("JCS"));
        let work: AuthoringWork = serde_json::from_value(serde_json::json!({
            "kind": "clean_replay",
            "work_id": "job_contract",
            "worker_claim_id": "claim_contract",
            "authoring_session_id": "session_contract",
            "capsule_revision_id": "caprev_contract",
            "source_revision_id": "srev_contract",
            "source_closure_id": format!("sha256:{}", "1".repeat(64)),
            "pinned_source": {
                "source_revision_id": "srev_contract",
                "source_materialization_id": "smat_contract",
                "source_archive_digest": format!("sha256:{}", "2".repeat(64)),
                "source_archive_object_key": "authoring/contract.tar.gz",
                "source_tree_digest": format!("sha256:{}", "1".repeat(64))
            },
            "build_config_revision_id": "bcrev_contract",
            "build_attempt_number": 1,
            "authoring_toml": authored_toml,
            "authoring_toml_digest": sha256_digest(authored_toml.as_bytes()),
            "normalized_program_intent": normalized,
            "effective_build_plan": plan,
            "plan_digest": plan_digest,
            "ready_state_seal_receipt": null,
            "lease_token": "lease-token-with-at-least-thirty-two-bytes",
            "lease_expires_at": "2026-08-03T00:00:00.000Z",
            "trace_id": "trace_contract"
        }))
        .expect("contract work");
        work
    }

    #[test]
    fn immutable_build_contract_accepts_exact_plan_and_empty_non_program_argument() {
        let work = build_contract_work();
        assert_eq!(
            validate_build_contract(&work),
            Ok(ValidatedBuildContract {
                timeout_seconds: 60
            })
        );
    }

    #[test]
    fn immutable_build_contract_rejects_a_rehashed_plan_that_changes_launch_argv() {
        let mut work = build_contract_work();
        let plan = work.effective_build_plan.as_mut().expect("plan");
        let launch = plan
            .get_mut("steps")
            .and_then(serde_json::Value::as_array_mut)
            .and_then(|steps| {
                steps.iter_mut().find(|step| {
                    step.get("step_id").and_then(serde_json::Value::as_str) == Some("launch")
                })
            })
            .expect("launch step");
        launch["command_argv"] = serde_json::json!(["python3", "different.py"]);
        let mut execution_plan = plan.clone();
        execution_plan
            .as_object_mut()
            .expect("object")
            .remove("identities");
        work.plan_digest = Some(sha256_digest(
            &serde_jcs::to_vec(&execution_plan).expect("JCS"),
        ));

        assert!(
            validate_build_contract(&work)
                .expect_err("plan must fail")
                .contains("launch differs")
        );
    }
}
