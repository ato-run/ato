//! Authoring Session v1 control-plane client and source-only inference.
//!
//! This module deliberately owns transport and pure inference only. Execution
//! remains in the snapshot builder's existing pinned-source and Firecracker
//! lanes, so the Authoring Session cannot grow a second build contract.

use std::fmt;
use std::path::Path;

use capsule::authoring_intent::{
    NormalizedProgramIntentEnvelopeV1, ProgramCommandDraftV1, ProgramIntentDraftV1,
    ProgramIntentOrigin, ReadinessIntentV1, WorkspacePathV1, normalize_program_intent,
    to_capsule_manifest_v1,
};
use capsule::types::manifest_v1::SealAtV1;
use serde::{Deserialize, Deserializer, Serialize};
use snapshot::archive_only_build::ArchiveOnlyBuildInput;

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
    pub source_materialization_id: String,
    pub source_archive_digest: String,
    pub source_archive_object_key: String,
    pub source_tree_digest: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthoringWork {
    pub kind: String,
    pub work_id: String,
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
    pub normalized_program_intent: Option<NormalizedProgramIntentEnvelopeV1>,
    #[serde(default)]
    pub resolution_lock_digest: Option<String>,
    #[serde(default)]
    pub request: Option<serde_json::Value>,
    pub lease_token: AuthoringLeaseToken,
    pub lease_expires_at: String,
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
}

#[derive(Debug, Serialize)]
pub struct SetupReady<'a> {
    pub builder_id: &'a str,
    pub builder_session_id: &'a str,
    pub builder_slot_id: &'a str,
    pub origin: &'a str,
    pub normalized_program_intent: &'a NormalizedProgramIntentEnvelopeV1,
    pub resolution_lock_digest: &'a str,
    pub generated_capsule_toml: &'a str,
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

fn http_error(operation: &str, error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(status, response) => {
            let body = response.into_string().unwrap_or_default();
            let code = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| value.get("error")?.as_str().map(str::to_owned))
                .unwrap_or_else(|| format!("http_{status}"));
            format!("{operation} was refused ({code}, HTTP {status})")
        }
        ureq::Error::Transport(transport) => {
            format!("{operation} transport failed ({})", transport.kind())
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn inference_does_not_guess_without_a_root_entrypoint() {
        let root = tempfile::tempdir().expect("tempdir");
        assert!(infer_static_web_intent(root.path()).is_err());
    }
}
