use anyhow::{Context, Result};

use crate::install::{self, GitHubInstallDraftResponse};

use super::format::{build_smoke_excerpt, generate_event_id};
use super::payloads::{
    AttemptPayload, AttemptPlatformPayload, AttemptRepoPayload, AttemptResolvedRefPayload,
    AttemptResponse, InferenceAttemptHandle, SmokeFailedPayload, VerifiedFixPayload,
};
use super::ENV_TELEMETRY;

pub fn telemetry_enabled() -> bool {
    match std::env::var(ENV_TELEMETRY) {
        Ok(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            !matches!(normalized.as_str(), "0" | "false" | "off" | "no")
        }
        Err(_) => true,
    }
}

pub fn should_collect_feedback(
    repository: &str,
    install_draft: &GitHubInstallDraftResponse,
) -> bool {
    telemetry_enabled()
        && install_draft.manifest_source == "inferred"
        && install::normalize_github_repository(repository).is_ok()
}

pub async fn submit_attempt(
    repository: &str,
    install_draft: &GitHubInstallDraftResponse,
) -> Result<Option<InferenceAttemptHandle>> {
    if !should_collect_feedback(repository, install_draft) {
        return Ok(None);
    }

    let normalized = install::normalize_github_repository(repository)?;
    let (owner, repo) = normalized
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("repository must include owner/repo"))?;
    let inferred_toml = match install_draft.preview_toml.clone() {
        Some(value) if !value.trim().is_empty() => value,
        _ => return Ok(None),
    };
    let hint_json = serde_json::json!({
        "confidence": install_draft
            .capsule_hint
            .as_ref()
            .map(|hint| hint.confidence.clone())
            .unwrap_or_else(|| "medium".to_string()),
        "launchability": install_draft
            .capsule_hint
            .as_ref()
            .and_then(|hint| hint.launchability.clone())
            .unwrap_or_else(|| "runnable".to_string()),
        "warnings": install_draft
            .capsule_hint
            .as_ref()
            .map(|hint| hint.warnings.clone())
            .unwrap_or_default(),
    });

    let payload = AttemptPayload {
        client_event_id: generate_event_id("attempt"),
        event_type: "attempt",
        repo: AttemptRepoPayload {
            host: "github.com".to_string(),
            owner: owner.to_string(),
            name: repo.to_string(),
            visibility: "public".to_string(),
        },
        resolved_ref: AttemptResolvedRefPayload {
            sha: install_draft.resolved_ref.sha.clone(),
            default_branch: install_draft.repo.default_branch.clone(),
        },
        manifest_source: install_draft.manifest_source.clone(),
        inferred_toml,
        hint_json,
        inference_mode: install_draft
            .inference_mode
            .clone()
            .unwrap_or_else(|| "rules".to_string()),
        inference_confidence: install_draft
            .capsule_hint
            .as_ref()
            .map(|hint| hint.confidence.clone())
            .unwrap_or_else(|| "medium".to_string()),
        capsule_toml_exists: install_draft.capsule_toml.exists,
        cli_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: AttemptPlatformPayload {
            os_family: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        },
        consent_state: "default_enabled".to_string(),
    };

    let client = reqwest::Client::new();
    let response = client
        .post(format!(
            "{}/v1/inference/feedback",
            install::resolve_store_api_base_url()
        ))
        .json(&payload)
        .send()
        .await
        .context("failed to submit inference attempt")?;
    if !response.status().is_success() {
        return Ok(None);
    }
    let body = response
        .json::<AttemptResponse>()
        .await
        .context("failed to parse inference attempt response")?;
    Ok(Some(InferenceAttemptHandle {
        attempt_id: body.attempt_id,
        repo_ref: install_draft.repo_ref.clone(),
        commit_sha: install_draft.resolved_ref.sha.clone(),
    }))
}

pub async fn submit_smoke_failed(
    attempt: &InferenceAttemptHandle,
    report: &capsule_core::smoke::SmokeFailureReport,
) -> Result<()> {
    if !telemetry_enabled() {
        return Ok(());
    }

    let excerpt = build_smoke_excerpt(report);
    let payload = SmokeFailedPayload {
        client_event_id: generate_event_id("smoke-failed"),
        event_type: "smoke_failed",
        attempt_id: attempt.attempt_id.clone(),
        smoke_status: "failed",
        smoke_error_class: report.class.as_str().to_string(),
        smoke_error_excerpt: excerpt,
    };

    let client = reqwest::Client::new();
    let _ = client
        .post(format!(
            "{}/v1/inference/feedback",
            install::resolve_store_api_base_url()
        ))
        .json(&payload)
        .send()
        .await
        .context("failed to submit smoke failure")?;
    Ok(())
}

pub async fn submit_verified_fix(
    attempt: &InferenceAttemptHandle,
    actual_toml: &str,
) -> Result<()> {
    if !telemetry_enabled() {
        return Ok(());
    }

    let payload = VerifiedFixPayload {
        client_event_id: generate_event_id("verified-fix"),
        event_type: "verified_fix",
        attempt_id: attempt.attempt_id.clone(),
        actual_toml: actual_toml.to_string(),
        fixed_by_type: "user",
        share_consent: true,
    };
    let client = reqwest::Client::new();
    let _ = client
        .post(format!(
            "{}/v1/inference/feedback",
            install::resolve_store_api_base_url()
        ))
        .json(&payload)
        .send()
        .await
        .context("failed to submit verified fix")?;
    Ok(())
}

pub async fn request_retry_install_draft(
    repository: &str,
    install_draft: &GitHubInstallDraftResponse,
    attempt: Option<&InferenceAttemptHandle>,
    report: &capsule_core::smoke::SmokeFailureReport,
    retry_ordinal: u8,
) -> Result<GitHubInstallDraftResponse> {
    let request = install::GitHubInstallDraftRetryRequest {
        attempt_id: attempt.map(|value| value.attempt_id.clone()),
        resolved_ref_sha: install_draft.resolved_ref.sha.clone(),
        previous_toml: install_draft
            .preview_toml
            .clone()
            .ok_or_else(|| anyhow::anyhow!("store draft previewToml missing for retry request"))?,
        smoke_error_class: report.class.as_str().to_string(),
        smoke_error_excerpt: build_smoke_excerpt(report),
        retry_ordinal,
    };
    install::retry_github_install_draft(repository, &request).await
}
