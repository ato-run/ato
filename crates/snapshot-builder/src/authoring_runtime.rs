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
    pub effective_build_plan: Option<serde_json::Value>,
    #[serde(default)]
    #[serde(rename = "request")]
    pub _request: Option<serde_json::Value>,
    #[serde(default)]
    pub clean_replay_receipt: Option<CleanReplayReceiptV1>,
    #[serde(default)]
    pub classified_state_diff: Option<ClassifiedStateDiffV1>,
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

    pub fn append_build_event(
        &self,
        work: &AuthoringWork,
        event: &serde_json::Value,
    ) -> Result<(), String> {
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
            "event": event,
        }))
        .map_err(|error| http_error("append Build Attempt event", error))?;
        Ok(())
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
}
