use anyhow::Result;
use std::path::Path;

use crate::install::{self, GitHubCheckout, GitHubInstallDraftResponse};
use crate::preview::{
    DerivedExecutionPlan, GitHubPreviewPreparation, PreviewPromotionEligibility, PreviewSession,
    PreviewTargetKind,
};

use super::manifest::{required_env_from_preview_toml, summarize_preview_toml};
use super::storage::persist_session_with_warning;
use crate::application::dependency_materializer::{
    write_source_resolution_record, SourceResolutionRecord,
};

pub async fn prepare_github_preview_session(
    repository: &str,
    invocation_dir: &Path,
    explicit_commit: Option<&str>,
) -> Result<GitHubPreviewPreparation> {
    let (install_draft, draft_fetch_warning) = match explicit_commit {
        Some(_) => (None, None),
        None => match install::fetch_github_install_draft(repository).await {
            Ok(draft) => (Some(draft), None),
            Err(_) => {
                anyhow::bail!(
                    "network unavailable; cannot resolve <ref> for {}. Run online once, or pass --commit <sha> to skip resolver.",
                    repository
                );
            }
        },
    };

    let checkout = install::download_github_repository_at_ref(
        repository,
        explicit_commit.or_else(|| {
            install_draft
                .as_ref()
                .map(|draft| draft.resolved_ref.sha.as_str())
        }),
    )
    .await?;
    let install_draft = install_draft
        .as_ref()
        .map(|draft| draft.normalize_preview_toml_for_checkout(&checkout.checkout_dir))
        .transpose()?;

    let preview_session = build_github_preview_session(
        repository,
        invocation_dir,
        &checkout,
        install_draft.as_ref(),
        explicit_commit,
    )?;
    let session_persist_warning = persist_session_with_warning(&preview_session);

    Ok(GitHubPreviewPreparation {
        checkout,
        draft_fetch_warning,
        install_draft,
        preview_session,
        session_persist_warning,
    })
}

pub fn draft_requires_manual_review(draft: &GitHubInstallDraftResponse) -> bool {
    let launchability_requires_manual_review = draft
        .capsule_hint
        .as_ref()
        .and_then(|hint| hint.launchability.as_deref())
        == Some("manual_review");
    if draft.retryable {
        return false;
    }

    let has_required_env = draft
        .preview_toml
        .as_deref()
        .map(required_env_from_preview_toml)
        .map(|values| {
            // Skip env vars already satisfied by the process environment.
            // ScopedEnv.apply_map() sets --env-file values into std::env before this
            // check runs, so vars the user already provided do not count as missing.
            values
                .into_iter()
                .filter(|k| std::env::var(k).is_err())
                .collect::<Vec<_>>()
        })
        .map(|unsatisfied| !unsatisfied.is_empty())
        .unwrap_or(false);
    let (has_manual_review_warning, has_soft_preview_warning) = draft
        .capsule_hint
        .as_ref()
        .map(|hint| {
            (
                hint.warnings
                    .iter()
                    .any(|warning| warning_requires_manual_review(warning)),
                hint.warnings
                    .iter()
                    .any(|warning| warning_is_soft_preview_advisory(warning)),
            )
        })
        .unwrap_or((false, false));

    has_required_env
        || has_manual_review_warning
        || (launchability_requires_manual_review && !has_soft_preview_warning)
}

pub fn github_draft_manual_review_reason(draft: &GitHubInstallDraftResponse) -> String {
    if let Some(warning) = draft.capsule_hint.as_ref().and_then(|hint| {
        hint.warnings
            .iter()
            .find(|warning| warning_requires_manual_review(warning))
    }) {
        return warning.clone();
    }

    "Generated draft requires manual review before fail-closed provisioning can continue."
        .to_string()
}

fn warning_requires_manual_review(warning: &str) -> bool {
    if warning_is_soft_preview_advisory(warning) {
        return false;
    }

    let lowered = warning.to_ascii_lowercase();

    lowered.contains("frozen-lockfile")
        || lowered.contains("uv.lock")
        || lowered.contains("package-lock.json")
        || lowered.contains("yarn.lock")
        || lowered.contains("pnpm-lock.yaml")
        || lowered.contains("bun.lock")
        || lowered.contains("multiple node lockfiles")
        || lowered.contains("database")
        || lowered.contains("redis")
        || lowered.contains("credential")
        || lowered.contains("secret")
        || lowered.contains("token")
        || lowered.contains("requires manual intervention")
        || lowered.contains("manual intervention required")
        || lowered.contains("required environment variable")
        || lowered.contains("required environment variables")
        || warning.contains("必須環境変数")
        || warning.contains("環境変数が必要")
        || warning.contains("環境変数を設定")
        || warning.contains("外部DB")
        || warning.contains("認証")
}

fn warning_is_soft_preview_advisory(warning: &str) -> bool {
    let lowered = warning.to_ascii_lowercase();
    lowered.contains("could not be normalized to a direct node entrypoint")
        || lowered.contains("a development server command was inferred from package.json")
        // ato run uses plain install (not --frozen-lockfile), so lockfile platform-
        // compatibility warnings from the store draft are not actionable for preview runs.
        || lowered.contains("frozen-lockfile")
        // "source/node requires a lockfile … for reproducible execution" — provision will
        // run `npm install` which generates one, so the warning is not a preview blocker.
        || lowered.contains("requires a lockfile")
}

fn build_github_preview_session(
    repository: &str,
    invocation_dir: &Path,
    checkout: &GitHubCheckout,
    install_draft: Option<&GitHubInstallDraftResponse>,
    explicit_commit: Option<&str>,
) -> Result<PreviewSession> {
    let preview_toml = install_draft.and_then(|draft| draft.preview_toml.clone());
    let mut session = PreviewSession::new(
        repository,
        PreviewTargetKind::GitHubRepository,
        invocation_dir.to_path_buf(),
        derived_plan_from_github_draft(install_draft),
    )?;
    session.checkout_dir = Some(checkout.checkout_dir.clone());
    session.checkout_preserved = false;
    session.repository = Some(checkout.repository.clone());
    session.preview_toml = preview_toml;
    if let Some(draft) = install_draft {
        session.resolved_ref = Some(draft.resolved_ref.clone());
        session.manifest_source = Some(draft.manifest_source.clone());
        session.inference_mode = draft.inference_mode.clone();
        let record = SourceResolutionRecord {
            authority: "github.com".to_string(),
            repository: Some(checkout.repository.clone()),
            requested_ref: Some(draft.resolved_ref.ref_name.clone()),
            resolved_commit: draft.resolved_ref.sha.clone(),
            resolved_at: chrono::Utc::now().to_rfc3339(),
            commit_signature_verdict: None,
        };
        write_source_resolution_record(
            &session.session_root.join("source-resolution.json"),
            &record,
        )?;
        tracing::info!(
            capsule_id = %checkout.repository,
            requested_ref = %draft.resolved_ref.ref_name,
            resolved_commit = %draft.resolved_ref.sha,
            "resolved GitHub source reference"
        );
    } else if let Some(commit) = explicit_commit {
        let record = SourceResolutionRecord {
            authority: "github.com".to_string(),
            repository: Some(checkout.repository.clone()),
            requested_ref: Some(commit.to_string()),
            resolved_commit: commit.to_string(),
            resolved_at: chrono::Utc::now().to_rfc3339(),
            commit_signature_verdict: None,
        };
        write_source_resolution_record(
            &session.session_root.join("source-resolution.json"),
            &record,
        )?;
        tracing::info!(
            capsule_id = %checkout.repository,
            requested_ref = %commit,
            resolved_commit = %commit,
            "resolved GitHub source reference from explicit commit"
        );
    }
    Ok(session)
}

pub(super) fn derived_plan_from_github_draft(
    install_draft: Option<&GitHubInstallDraftResponse>,
) -> DerivedExecutionPlan {
    let mut plan = DerivedExecutionPlan::default();
    let Some(draft) = install_draft else {
        plan.warnings.push(
            "No ato store install draft was available; preview will rely on local zero-config inference."
                .to_string(),
        );
        plan.promotion_eligibility = PreviewPromotionEligibility::RequiresManualReview;
        return plan;
    };

    if let Some(preview_toml) = draft.preview_toml.as_deref() {
        let summary = summarize_preview_toml(preview_toml);
        plan.runtime = summary.runtime;
        plan.driver = summary.driver;
        plan.resolved_runtime_version = summary.runtime_version;
        plan.resolved_port = summary.port;
        plan.resolved_pack_include = summary.pack_include;
    }

    if let Some(hint) = draft.capsule_hint.as_ref() {
        plan.warnings.extend(hint.warnings.clone());
    }
    let required_env = draft
        .preview_toml
        .as_deref()
        .map(required_env_from_preview_toml)
        .unwrap_or_default();
    if !required_env.is_empty() {
        plan.deferred_constraints.push(format!(
            "Required environment variables must be provided before promotion: {}",
            required_env.join(", ")
        ));
    }
    plan.promotion_eligibility = if draft_requires_manual_review(draft) {
        PreviewPromotionEligibility::RequiresManualReview
    } else {
        PreviewPromotionEligibility::Eligible
    };

    plan
}
