use anyhow::{Context, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use capsule_core::execution_plan::error::AtoExecutionError;

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

#[allow(dead_code)]
pub fn build_manual_manifest_path(base_dir: &Path, repository: &str, attempt_id: &str) -> PathBuf {
    let repo_path = install::normalize_github_repository(repository)
        .ok()
        .and_then(|value| {
            value
                .split_once('/')
                .map(|(owner, repo)| PathBuf::from("github.com").join(owner).join(repo))
        })
        .unwrap_or_else(|| {
            let sanitized = repository
                .chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
                .collect::<String>()
                .split('-')
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>()
                .join("-");
            PathBuf::from("github.com").join(if sanitized.is_empty() {
                "repository".to_string()
            } else {
                sanitized
            })
        });

    base_dir
        .join(".tmp")
        .join("ato")
        .join("inference")
        .join(repo_path)
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
    let editor_command = resolved_editor_command()
        .ok_or_else(|| anyhow::anyhow!("No editor launcher is available for manual fix mode"))?;
    let (program, args) = editor_command
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("editor command was empty"))?;

    let status = Command::new(program)
        .args(args)
        .arg(path)
        .status()
        .with_context(|| format!("failed to launch editor '{}'", editor_command.join(" ")))?;
    if !status.success() {
        anyhow::bail!(
            "editor '{}' exited with status {}",
            editor_command.join(" "),
            status
        );
    }
    Ok(())
}

pub fn can_open_editor_automatically() -> bool {
    resolved_editor_command().is_some()
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

pub fn build_manual_intervention_error(
    manifest_path: &Path,
    failure_reason: &str,
    next_steps: &[String],
) -> AtoExecutionError {
    AtoExecutionError::manual_intervention_required(
        build_manual_intervention_message(manifest_path, failure_reason, next_steps),
        Some(&manifest_path.display().to_string()),
        next_steps.to_vec(),
    )
}

fn resolved_editor_command() -> Option<Vec<String>> {
    configured_editor_command().or_else(automatic_editor_command)
}

fn configured_editor_command() -> Option<Vec<String>> {
    configured_editor_command_from_values(
        std::env::var("VISUAL").ok(),
        std::env::var("EDITOR").ok(),
    )
}

fn configured_editor_command_from_values(
    visual: Option<String>,
    editor: Option<String>,
) -> Option<Vec<String>> {
    normalize_editor_value(visual)
        .or_else(|| normalize_editor_value(editor))
        .and_then(parse_editor_command)
}

fn normalize_editor_value(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn parse_editor_command(value: String) -> Option<Vec<String>> {
    match shell_words::split(&value) {
        Ok(parts) if !parts.is_empty() => Some(parts),
        _ if !value.is_empty() => Some(vec![value]),
        _ => None,
    }
}

fn automatic_editor_command() -> Option<Vec<String>> {
    fallback_editor_command_for(std::env::consts::OS, |command| {
        which::which(command).is_ok()
    })
}

fn fallback_editor_command_for<F>(os: &str, has_command: F) -> Option<Vec<String>>
where
    F: Fn(&str) -> bool,
{
    if os == "macos" && has_command("open") {
        return Some(vec!["open".to_string(), "-W".to_string(), "-t".to_string()]);
    }

    for command in ["sensible-editor", "editor", "nano", "vim", "vi"] {
        if has_command(command) {
            return Some(vec![command.to_string()]);
        }
    }

    None
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
    fn configured_editor_command_prefers_visual_and_splits_args() {
        let command = configured_editor_command_from_values(
            Some("code --wait".to_string()),
            Some("nano".to_string()),
        )
        .expect("visual should resolve");

        assert_eq!(command, vec!["code", "--wait"]);
    }

    #[test]
    fn configured_editor_command_uses_editor_when_visual_is_blank() {
        let command = configured_editor_command_from_values(
            Some("   ".to_string()),
            Some("nano".to_string()),
        )
        .expect("editor should resolve");

        assert_eq!(command, vec!["nano"]);
    }

    #[test]
    fn fallback_editor_command_prefers_macos_open() {
        let command =
            fallback_editor_command_for("macos", |candidate| matches!(candidate, "open" | "nano"))
                .expect("mac fallback should resolve");

        assert_eq!(command, vec!["open", "-W", "-t"]);
    }

    #[test]
    fn fallback_editor_command_prefers_terminal_editor_on_linux() {
        let command = fallback_editor_command_for("linux", |candidate| {
            matches!(candidate, "editor" | "nano")
        })
        .expect("linux fallback should resolve");

        assert_eq!(command, vec!["editor"]);
    }

    #[test]
    fn fallback_editor_command_returns_none_without_candidates() {
        assert!(fallback_editor_command_for("linux", |_| false).is_none());
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
        let path = build_manual_manifest_path(Path::new("/repo"), "koh0920/ato-cli", "attempt1");
        assert_eq!(
            path,
            PathBuf::from(
                "/repo/.tmp/ato/inference/github.com/koh0920/ato-cli/attempt1/capsule.toml"
            )
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
            Path::new("/repo/.tmp/ato/inference/github.com/koh0920/ato-cli/attempt1/capsule.toml"),
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
