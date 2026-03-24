use anyhow::Result;
use std::path::Path;

use crate::install::{self, GitHubCheckout, GitHubInstallDraftResponse};
use crate::preview::{
    DerivedExecutionPlan, GitHubPreviewPreparation, PreviewPromotionEligibility, PreviewSession,
    PreviewTargetKind,
};

use super::manifest::{required_env_from_preview_toml, summarize_preview_toml};
use super::storage::persist_session_with_warning;

pub async fn prepare_github_preview_session(
    repository: &str,
    invocation_dir: &Path,
) -> Result<GitHubPreviewPreparation> {
    let (install_draft, draft_fetch_warning) =
        match install::fetch_github_install_draft(repository).await {
            Ok(draft) => (Some(draft), None),
            Err(error) => (
                None,
                Some(format!(
                    "Failed to fetch ato store install draft: {error}. Falling back to local zero-config inference."
                )),
            ),
        };

    let checkout = install::download_github_repository_at_ref(
        repository,
        install_draft
            .as_ref()
            .map(|draft| draft.resolved_ref.sha.as_str()),
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
    if draft
        .capsule_hint
        .as_ref()
        .and_then(|hint| hint.launchability.as_deref())
        == Some("manual_review")
    {
        return true;
    }
    if draft.retryable {
        return false;
    }

    let has_required_env = draft
        .preview_toml
        .as_deref()
        .map(required_env_from_preview_toml)
        .map(|values| !values.is_empty())
        .unwrap_or(false);
    let has_manual_review_warning = draft
        .capsule_hint
        .as_ref()
        .map(|hint| {
            hint.warnings
                .iter()
                .any(|warning| warning_requires_manual_review(warning))
        })
        .unwrap_or(false);

    has_required_env || has_manual_review_warning
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
    let lowered = warning.to_ascii_lowercase();

    lowered.contains("frozen-lockfile")
        || lowered.contains("uv.lock")
        || lowered.contains("package-lock.json")
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

fn build_github_preview_session(
    repository: &str,
    invocation_dir: &Path,
    checkout: &GitHubCheckout,
    install_draft: Option<&GitHubInstallDraftResponse>,
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
