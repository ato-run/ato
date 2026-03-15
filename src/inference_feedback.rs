use anyhow::{Context, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::install::{self, GitHubInstallDraftResponse};

const ENV_TELEMETRY: &str = "ATO_TELEMETRY";
const MAX_SMOKE_ERROR_EXCERPT_CHARS: usize = 4000;

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
struct AttemptRepoPayload {
    host: String,
    owner: String,
    name: String,
    visibility: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AttemptResolvedRefPayload {
    sha: String,
    default_branch: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AttemptPlatformPayload {
    os_family: String,
    arch: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AttemptPayload {
    client_event_id: String,
    event_type: &'static str,
    repo: AttemptRepoPayload,
    resolved_ref: AttemptResolvedRefPayload,
    manifest_source: String,
    inferred_toml: String,
    hint_json: serde_json::Value,
    inference_mode: String,
    inference_confidence: String,
    capsule_toml_exists: bool,
    cli_version: String,
    platform: AttemptPlatformPayload,
    consent_state: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttemptResponse {
    attempt_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SmokeFailedPayload {
    client_event_id: String,
    event_type: &'static str,
    attempt_id: String,
    smoke_status: &'static str,
    smoke_error_class: String,
    smoke_error_excerpt: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifiedFixPayload {
    client_event_id: String,
    event_type: &'static str,
    attempt_id: String,
    actual_toml: String,
    fixed_by_type: &'static str,
    share_consent: bool,
}

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

pub fn build_manual_manifest_path(checkout_dir: &Path, attempt_id: &str) -> PathBuf {
    checkout_dir
        .join(".tmp")
        .join("ato-inference")
        .join(attempt_id)
        .join("capsule.toml")
}

pub fn write_manual_manifest(path: &Path, manifest_text: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create temp manifest directory: {}",
                parent.display()
            )
        })?;
    }
    fs::write(path, manifest_text)
        .with_context(|| format!("failed to write temp manifest: {}", path.display()))?;
    Ok(())
}

pub fn read_manual_manifest(path: &Path) -> Result<String> {
    fs::read_to_string(path)
        .with_context(|| format!("failed to read edited manifest: {}", path.display()))
}

pub fn open_editor(path: &Path) -> Result<()> {
    let editor = configured_editor()
        .ok_or_else(|| anyhow::anyhow!("VISUAL or EDITOR must be set for manual fix mode"))?;

    let status = Command::new(&editor)
        .arg(path)
        .status()
        .with_context(|| format!("failed to launch editor '{editor}'"))?;
    if !status.success() {
        anyhow::bail!("editor '{}' exited with status {}", editor, status);
    }
    Ok(())
}

pub fn has_configured_editor() -> bool {
    configured_editor().is_some()
}

pub fn prompt_yes_no(prompt: &str, default_yes: bool) -> Result<bool> {
    print!("{prompt}");
    io::stdout().flush().context("failed to flush prompt")?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read prompt input")?;
    let normalized = input.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Ok(default_yes);
    }
    Ok(matches!(normalized.as_str(), "y" | "yes"))
}

pub fn summarize_manifest_diff(inferred_toml: &str, actual_toml: &str) -> String {
    let inferred_lines: Vec<&str> = inferred_toml.lines().collect();
    let actual_lines: Vec<&str> = actual_toml.lines().collect();
    let max_len = inferred_lines.len().max(actual_lines.len());
    let mut changed_lines = 0usize;
    for index in 0..max_len {
        if inferred_lines.get(index) != actual_lines.get(index) {
            changed_lines += 1;
        }
    }
    format!(
        "Updated {} line(s) ({} -> {}).",
        changed_lines,
        inferred_lines.len(),
        actual_lines.len()
    )
}

fn build_smoke_excerpt(report: &capsule_core::smoke::SmokeFailureReport) -> String {
    let message = report.message.trim();
    let stderr = report.stderr_tail.trim();
    let combined = if stderr.is_empty() {
        message.to_string()
    } else {
        format!("{message}\n{stderr}")
    };
    cap_smoke_excerpt(&combined)
}

pub fn build_manual_intervention_message(
    manifest_path: &Path,
    failure_reason: &str,
    next_steps: &[String],
) -> String {
    let mut message = format!(
        "manual intervention required: {}\nGenerated capsule.toml: {}",
        failure_reason.trim(),
        manifest_path.display()
    );
    if !next_steps.is_empty() {
        message.push_str("\nNext steps:\n");
        for step in next_steps {
            message.push_str("- ");
            message.push_str(step.trim());
            message.push('\n');
        }
        message.pop();
    }
    message
}

fn configured_editor() -> Option<String> {
    std::env::var("VISUAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
}

fn cap_smoke_excerpt(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.chars().count() <= MAX_SMOKE_ERROR_EXCERPT_CHARS {
        return trimmed.to_string();
    }

    let head_len = 1400usize;
    let tail_len = MAX_SMOKE_ERROR_EXCERPT_CHARS.saturating_sub(head_len + 7);
    let head: String = trimmed.chars().take(head_len).collect();
    let tail: String = trimmed
        .chars()
        .rev()
        .take(tail_len)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}\n[...]\n{tail}")
}

fn generate_event_id(prefix: &str) -> String {
    let mut bytes = [0_u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("{prefix}-{}", hex::encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn telemetry_can_be_disabled_via_env() {
        let _guard = EnvGuard::set(ENV_TELEMETRY, Some("0"));
        assert!(!telemetry_enabled());
    }

    #[test]
    fn manifest_diff_summary_counts_changed_lines() {
        let summary = summarize_manifest_diff(
            "schema_version = \"0.2\"\nname = \"demo\"\n",
            "schema_version = \"0.2\"\nname = \"demo-fixed\"\n",
        );
        assert!(summary.contains("Updated 1 line"));
    }

    #[test]
    fn manual_manifest_path_uses_repo_tmp_directory() {
        let path = build_manual_manifest_path(Path::new("/repo"), "attempt1");
        assert_eq!(
            path,
            PathBuf::from("/repo/.tmp/ato-inference/attempt1/capsule.toml")
        );
    }

    #[test]
    fn smoke_excerpt_is_capped_to_store_limit() {
        let report = capsule_core::smoke::SmokeFailureReport {
            class: capsule_core::smoke::SmokeFailureClass::ProcessExitedEarly,
            message: "process exited while waiting for port 8000".to_string(),
            stderr_tail: "x".repeat(5000),
            exit_status: Some(1),
        };

        let excerpt = build_smoke_excerpt(&report);
        assert!(excerpt.chars().count() <= MAX_SMOKE_ERROR_EXCERPT_CHARS);
        assert!(excerpt.contains("process exited while waiting for port 8000"));
        assert!(excerpt.contains("[...]"));
    }

    #[test]
    fn manual_intervention_message_includes_path_and_steps() {
        let message = build_manual_intervention_message(
            Path::new("/repo/.tmp/ato-inference/attempt1/capsule.toml"),
            "DATABASE_URL is required",
            &[
                "Set DATABASE_URL before rerunning.".to_string(),
                "Open the generated manifest and adjust the command if needed.".to_string(),
            ],
        );

        assert!(message.contains("manual intervention required"));
        assert!(message.contains("Generated capsule.toml"));
        assert!(message.contains("DATABASE_URL is required"));
        assert!(message.contains("Set DATABASE_URL before rerunning."));
    }
}
