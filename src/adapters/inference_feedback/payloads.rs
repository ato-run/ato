use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct InferenceAttemptHandle {
    pub attempt_id: String,
    #[allow(dead_code)]
    pub repo_ref: String,
    #[allow(dead_code)]
    pub commit_sha: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AttemptRepoPayload {
    pub host: String,
    pub owner: String,
    pub name: String,
    pub visibility: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AttemptResolvedRefPayload {
    pub sha: String,
    pub default_branch: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AttemptPlatformPayload {
    pub os_family: String,
    pub arch: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AttemptPayload {
    pub client_event_id: String,
    pub event_type: &'static str,
    pub repo: AttemptRepoPayload,
    pub resolved_ref: AttemptResolvedRefPayload,
    pub manifest_source: String,
    pub inferred_toml: String,
    pub hint_json: serde_json::Value,
    pub inference_mode: String,
    pub inference_confidence: String,
    pub capsule_toml_exists: bool,
    pub cli_version: String,
    pub platform: AttemptPlatformPayload,
    pub consent_state: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AttemptResponse {
    pub attempt_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SmokeFailedPayload {
    pub client_event_id: String,
    pub event_type: &'static str,
    pub attempt_id: String,
    pub smoke_status: &'static str,
    pub smoke_error_class: String,
    pub smoke_error_excerpt: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct VerifiedFixPayload {
    pub client_event_id: String,
    pub event_type: &'static str,
    pub attempt_id: String,
    pub actual_toml: String,
    pub fixed_by_type: &'static str,
    pub share_consent: bool,
}
