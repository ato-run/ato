use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::debug;

use crate::reporters;
use crate::{install, CompatibilityFallbackBackend, EnforcementMode};

pub(crate) struct RunLikeCommandArgs {
    pub(crate) path: PathBuf,
    pub(crate) target: Option<String>,
    pub(crate) watch: bool,
    pub(crate) background: bool,
    pub(crate) nacelle: Option<PathBuf>,
    pub(crate) registry: Option<String>,
    pub(crate) state: Vec<String>,
    pub(crate) inject: Vec<String>,
    pub(crate) enforcement: EnforcementMode,
    pub(crate) sandbox_mode: bool,
    pub(crate) unsafe_mode_legacy: bool,
    pub(crate) unsafe_bypass_sandbox_legacy: bool,
    pub(crate) dangerously_skip_permissions: bool,
    pub(crate) compatibility_fallback: Option<CompatibilityFallbackBackend>,
    pub(crate) yes: bool,
    pub(crate) keep_failed_artifacts: bool,
    pub(crate) allow_unverified: bool,
    pub(crate) skill: Option<String>,
    pub(crate) from_skill: Option<PathBuf>,
    pub(crate) deprecation_warning: Option<&'static str>,
    pub(crate) reporter: Arc<reporters::CliReporter>,
}

pub(crate) fn execute_run_like_command(args: RunLikeCommandArgs) -> Result<()> {
    if let Some(warning) = args.deprecation_warning {
        eprintln!("{warning}");
    }

    let rt = tokio::runtime::Runtime::new()?;

    let resolved_skill_path = match (args.skill, args.from_skill) {
        (Some(skill_name), None) => Some(crate::skill_resolver::resolve_skill_path(&skill_name)?),
        (None, Some(path)) => Some(path),
        (None, None) => None,
        (Some(_), Some(_)) => {
            anyhow::bail!("--skill and --from-skill are mutually exclusive");
        }
    };

    if let Some(skill_path) = resolved_skill_path {
        if args.watch {
            anyhow::bail!("--skill/--from-skill does not support --watch in MVP mode");
        }
        if args.background {
            anyhow::bail!("--skill/--from-skill does not support --background in MVP mode");
        }

        let generated = crate::skill::materialize_skill_capsule(&skill_path)?;
        debug!(
            manifest_path = %generated.manifest_path().display(),
            "Translated SKILL.md to capsule"
        );

        let sandbox_requested =
            args.sandbox_mode || args.unsafe_mode_legacy || args.unsafe_bypass_sandbox_legacy;
        let effective_enforcement = crate::enforce_sandbox_mode_flags(
            args.enforcement,
            sandbox_requested,
            args.dangerously_skip_permissions,
            args.compatibility_fallback,
            args.reporter.clone(),
        )?;
        return crate::execute_open_command(
            generated.manifest_path().to_path_buf(),
            args.target,
            args.watch,
            args.background,
            args.nacelle,
            effective_enforcement,
            sandbox_requested,
            args.dangerously_skip_permissions,
            args.compatibility_fallback
                .map(CompatibilityFallbackBackend::as_str)
                .map(str::to_string),
            args.yes,
            args.state,
            args.inject,
            args.reporter,
        );
    }

    let path = rt.block_on(resolve_run_target_or_install(
        args.path,
        args.yes,
        args.keep_failed_artifacts,
        args.allow_unverified,
        args.registry.as_deref(),
        args.reporter.clone(),
    ))?;

    let sandbox_requested =
        args.sandbox_mode || args.unsafe_mode_legacy || args.unsafe_bypass_sandbox_legacy;
    let effective_enforcement = crate::enforce_sandbox_mode_flags(
        args.enforcement,
        sandbox_requested,
        args.dangerously_skip_permissions,
        args.compatibility_fallback,
        args.reporter.clone(),
    )?;
    crate::execute_open_command(
        path,
        args.target,
        args.watch,
        args.background,
        args.nacelle,
        effective_enforcement,
        sandbox_requested,
        args.dangerously_skip_permissions,
        args.compatibility_fallback
            .map(CompatibilityFallbackBackend::as_str)
            .map(str::to_string),
        args.yes,
        args.state,
        args.inject,
        args.reporter,
    )
}

pub(crate) async fn resolve_run_target_or_install(
    path: PathBuf,
    yes: bool,
    keep_failed_artifacts: bool,
    allow_unverified: bool,
    registry: Option<&str>,
    reporter: Arc<reporters::CliReporter>,
) -> Result<PathBuf> {
    let raw = path.to_string_lossy().to_string();
    let expanded_local = crate::local_input::expand_local_path(&raw);
    if crate::local_input::should_treat_input_as_local(&raw, &expanded_local) {
        return Ok(expanded_local);
    }

    if let Some(repository) = install::parse_github_run_ref(&raw)? {
        let json_mode = matches!(reporter.as_ref(), reporters::CliReporter::Json(_));
        if crate::progressive_ui::can_use_progressive_ui(json_mode) {
            crate::progressive_ui::begin_flow()?;
        }
        if json_mode && !yes {
            anyhow::bail!(
                "Non-interactive JSON mode requires -y/--yes when auto-installing missing capsules"
            );
        }

        if !yes
            && !crate::can_prompt_interactively(
                std::io::stdin().is_terminal(),
                std::io::stdout().is_terminal(),
            )
        {
            anyhow::bail!(
                "Interactive install confirmation requires a TTY. Re-run with -y/--yes in CI or non-interactive environments."
            );
        }

        if yes {
            debug!(
                repository = %repository,
                "GitHub repository not installed locally; continuing with -y auto-install"
            );
        }

        let install_result = install_github_repository(
            &repository,
            None,
            yes,
            install::ProjectionPreference::Skip,
            json_mode,
            !json_mode
                && crate::can_prompt_interactively(
                    std::io::stdin().is_terminal(),
                    std::io::stderr().is_terminal(),
                ),
            keep_failed_artifacts,
        )
        .await?;
        return Ok(install_result.path);
    }

    let scoped_ref = match install::parse_capsule_ref(&raw) {
        Ok(value) => value,
        Err(error) => {
            if install::is_slug_only_ref(&raw) {
                let effective_registry = registry.unwrap_or(crate::DEFAULT_RUN_REGISTRY_URL);
                anyhow::bail!(
                    "{}",
                    crate::scoped_id_prompt::run_scoped_id_prompt(&raw, Some(effective_registry))
                        .await?
                );
            }
            return Err(error).context(
                "Invalid run target. Use a local path or existing .capsule file, or publisher/slug for store capsules.",
            );
        }
    };

    let installed_capsule =
        crate::resolve_installed_capsule_archive(&scoped_ref, registry, None).await?;
    let mut registry_detail = None;
    let mut registry_installable_version = None;

    if let Some(explicit_registry) = registry {
        match install::fetch_capsule_detail(&scoped_ref.scoped_id, Some(explicit_registry)).await {
            Ok(detail) => {
                registry_installable_version = detail
                    .latest_version
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);

                if let Some(version) = registry_installable_version.as_deref() {
                    if let Some(installed_capsule) = crate::resolve_installed_capsule_archive(
                        &scoped_ref,
                        registry,
                        Some(version),
                    )
                    .await?
                    {
                        debug!(
                            capsule = %installed_capsule.display(),
                            version = version,
                            "Using installed capsule matching registry current version"
                        );
                        return Ok(installed_capsule);
                    }
                }

                registry_detail = Some(detail);
            }
            Err(error) => {
                if let Some(installed_capsule) = installed_capsule {
                    debug!(
                        capsule = %installed_capsule.display(),
                        error = %error,
                        "Falling back to installed capsule after registry detail lookup failed"
                    );
                    return Ok(installed_capsule);
                }
                return Err(error);
            }
        }
    } else if let Some(installed_capsule) = installed_capsule {
        debug!(
            capsule = %installed_capsule.display(),
            "Using installed capsule"
        );
        return Ok(installed_capsule);
    }

    let json_mode = matches!(reporter.as_ref(), reporters::CliReporter::Json(_));
    crate::ensure_run_auto_install_allowed(
        yes,
        json_mode,
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
    )?;

    let effective_registry = registry.unwrap_or(crate::DEFAULT_RUN_REGISTRY_URL);
    let detail = if let Some(detail) = registry_detail {
        detail
    } else {
        install::fetch_capsule_detail(&scoped_ref.scoped_id, Some(effective_registry)).await?
    };
    let installable_version = if let Some(version) = registry_installable_version {
        version
    } else {
        detail
            .latest_version
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Cannot auto-install '{}': no installable version is published.",
                    detail.scoped_id
                )
            })?
            .to_string()
    };

    if !yes {
        let approved = crate::prompt_install_confirmation(&detail, &installable_version)?;
        if !approved {
            anyhow::bail!("Installation cancelled by user");
        }
    } else {
        debug!(
            scoped_id = %detail.scoped_id,
            "Capsule not installed; continuing with -y auto-install"
        );
    }

    let install_result = install::install_app(
        &scoped_ref.scoped_id,
        Some(effective_registry),
        Some(installable_version.as_str()),
        None,
        false,
        yes,
        install::ProjectionPreference::Skip,
        allow_unverified,
        false,
        json_mode,
        !json_mode
            && crate::can_prompt_interactively(
                std::io::stdin().is_terminal(),
                std::io::stderr().is_terminal(),
            ),
    )
    .await?;
    Ok(install_result.path)
}

pub(crate) async fn install_github_repository(
    repository: &str,
    output_dir: Option<PathBuf>,
    yes: bool,
    projection_preference: install::ProjectionPreference,
    json: bool,
    can_prompt: bool,
    keep_failed_artifacts: bool,
) -> Result<install::InstallResult> {
    const MAX_GITHUB_DRAFT_RETRIES: u8 = 3;
    let invocation_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let preview_preparation =
        crate::preview::prepare_github_preview_session(repository, &invocation_dir).await?;
    if let Some(warning) = preview_preparation.draft_fetch_warning.as_deref() {
        if !json {
            eprintln!("⚠️  {warning}");
        }
    }
    if let Some(warning) = preview_preparation.session_persist_warning.as_deref() {
        if !json {
            eprintln!("⚠️  {warning}");
        }
    }
    let mut checkout = preview_preparation.checkout;
    let install_draft = preview_preparation.install_draft;
    let mut preview_session = preview_preparation.preview_session;
    if install_draft.is_some() {
        if let Err(error) = crate::show_github_draft_preview(&preview_session, json) {
            crate::maybe_keep_failed_github_checkout(&mut checkout, keep_failed_artifacts, json);
            return Err(error);
        }
    }
    let injected_manifest = install_draft
        .as_ref()
        .and_then(|draft| draft.preview_toml.clone());
    let inference_attempt = if let Some(draft) = install_draft.as_ref() {
        crate::inference_feedback::submit_attempt(repository, draft)
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    preview_session.set_inference_attempt(inference_attempt.as_ref());
    if let Some(warning) = crate::preview::persist_session_with_warning(&preview_session) {
        if !json {
            eprintln!("⚠️  {warning}");
        }
    }
    if !json {
        if crate::progressive_ui::can_use_progressive_ui(false) {
            crate::progressive_ui::show_note(
                "GitHub Source Build",
                format!(
                    "Repository   : {}\nCheckout     :\n{}",
                    checkout.repository,
                    crate::progressive_ui::format_path_for_note(&checkout.checkout_dir)
                ),
            )?;
        } else {
            eprintln!(
                "📦 Building {} from GitHub source in {}",
                checkout.repository,
                checkout.checkout_dir.display()
            );
        }
        if let Some(draft) = install_draft.as_ref() {
            if crate::progressive_ui::can_use_progressive_ui(false) {
                crate::progressive_ui::show_note(
                    "Inference Draft",
                    format!(
                        "Revision     : {} ({})\nManifest     : {}\nConfidence   : {}",
                        draft.resolved_ref.sha,
                        draft.resolved_ref.ref_name,
                        draft.manifest_source,
                        draft
                            .capsule_hint
                            .as_ref()
                            .map(|hint| hint.confidence.as_str())
                            .unwrap_or("unknown")
                    ),
                )?;
            } else {
                eprintln!(
                    "   Revision: {} ({})",
                    draft.resolved_ref.sha, draft.resolved_ref.ref_name
                );
            }
            if draft.manifest_source == "inferred" {
                if crate::progressive_ui::can_use_progressive_ui(false) {
                    crate::progressive_ui::show_warning(format!(
                        "Using store-generated capsule draft for {}",
                        draft.repo_ref
                    ))?;
                } else {
                    eprintln!(
                        "   Using store-generated capsule draft for {}",
                        draft.repo_ref
                    );
                }
                if let Some(hint) = draft.capsule_hint.as_ref() {
                    if crate::progressive_ui::can_use_progressive_ui(false) {
                        if !hint.warnings.is_empty() {
                            crate::progressive_ui::show_note(
                                "Draft Warnings",
                                hint.warnings.join("\n"),
                            )?;
                        }
                    } else {
                        eprintln!("   Confidence: {}", hint.confidence);
                        for warning in &hint.warnings {
                            eprintln!("   Warning: {warning}");
                        }
                    }
                }
            }
        }
    }
    if let Some(draft) = install_draft.as_ref() {
        if crate::preview::draft_requires_manual_review(draft) {
            if crate::progressive_ui::can_use_progressive_ui(false) {
                let warnings = draft
                    .capsule_hint
                    .as_ref()
                    .map(|hint| hint.warnings.clone())
                    .unwrap_or_default();
                crate::progressive_ui::render_manual_review_required(
                    &preview_session.manifest_path,
                    &crate::preview::github_draft_manual_review_reason(draft),
                    &warnings,
                )?;
                crate::progressive_ui::show_cancel(
                    "Manual review is required before fail-closed provisioning can continue.",
                )?;
            }
            preview_session.record_manual_intervention_required(
                &crate::preview::github_draft_manual_review_reason(draft),
            );
            if let Some(warning) = crate::preview::persist_session_with_warning(&preview_session) {
                if !json {
                    eprintln!("⚠️  {warning}");
                }
            }
            crate::maybe_keep_failed_github_checkout(&mut checkout, keep_failed_artifacts, json);
            return Err(crate::build_github_manual_intervention_error(
                &preview_session.manifest_path,
                repository,
                draft,
                &crate::preview::github_draft_manual_review_reason(draft),
            )?);
        }
    }
    if can_prompt && !yes {
        let approved = crate::progressive_ui::confirm_with_fallback(
            "Proceed with installation and run? ",
            true,
            crate::progressive_ui::can_use_progressive_ui(json),
        )?;
        if !approved {
            anyhow::bail!("Installation cancelled by user");
        }
    }
    let mut latest_install_draft = install_draft.clone();
    let build_result = match crate::build_github_repository_checkout(
        checkout.checkout_dir.clone(),
        json,
        injected_manifest.clone(),
        keep_failed_artifacts,
        true,
    )
    .await
    {
        Ok(result) => result,
        Err(error) => {
            let mut last_error = error;
            if let Some(draft) = install_draft.as_ref() {
                if crate::github_build_error_requires_manual_intervention(&last_error) {
                    crate::maybe_keep_failed_github_checkout(
                        &mut checkout,
                        keep_failed_artifacts,
                        json,
                    );
                    return Err(crate::build_github_manual_intervention_error(
                        &preview_session.manifest_path,
                        repository,
                        draft,
                        &crate::github_build_error_manual_review_reason(&last_error),
                    )?);
                }
            }
            let smoke_report = last_error
                .downcast_ref::<crate::commands::build::InferredManifestSmokeFailure>()
                .map(|failure| failure.report.clone());

            if let (Some(attempt), Some(report)) =
                (inference_attempt.as_ref(), smoke_report.as_ref())
            {
                let _ = crate::inference_feedback::submit_smoke_failed(attempt, report).await;
            }
            if let Some(report) = smoke_report.as_ref() {
                preview_session.record_smoke_failure(report);
                if let Some(warning) =
                    crate::preview::persist_session_with_warning(&preview_session)
                {
                    if !json {
                        eprintln!("⚠️  {warning}");
                    }
                }
            }

            if let (Some(draft), Some(report)) = (install_draft.as_ref(), smoke_report.as_ref()) {
                if !json {
                    eprintln!("Failed to run with inferred capsule.toml.");
                    eprintln!("Reason: {}", report.message);
                }
                if crate::preview::draft_requires_manual_review(draft) {
                    preview_session.record_manual_intervention_required(&report.message);
                    if let Some(warning) =
                        crate::preview::persist_session_with_warning(&preview_session)
                    {
                        if !json {
                            eprintln!("⚠️  {warning}");
                        }
                    }
                    crate::maybe_keep_failed_github_checkout(
                        &mut checkout,
                        keep_failed_artifacts,
                        json,
                    );
                    return Err(crate::build_github_manual_intervention_error(
                        &preview_session.manifest_path,
                        repository,
                        draft,
                        &report.message,
                    )?);
                }

                let mut recovered_build = None;
                let mut current_draft = draft.clone();
                let mut current_report = report.clone();
                for retry_ordinal in 1..=MAX_GITHUB_DRAFT_RETRIES {
                    let previous_toml = current_draft.preview_toml.clone().unwrap_or_default();
                    if previous_toml.trim().is_empty() {
                        break;
                    }

                    let next_draft = match crate::inference_feedback::request_retry_install_draft(
                        repository,
                        &current_draft,
                        inference_attempt.as_ref(),
                        &current_report,
                        retry_ordinal,
                    )
                    .await
                    {
                        Ok(value) => value,
                        Err(retry_error) => {
                            if !json {
                                eprintln!(
                                    "⚠️  Failed to request retry draft ({retry_ordinal}/{MAX_GITHUB_DRAFT_RETRIES}): {retry_error}"
                                );
                            }
                            break;
                        }
                    };
                    let next_draft =
                        next_draft.normalize_preview_toml_for_checkout(&checkout.checkout_dir)?;
                    let next_toml = next_draft.preview_toml.clone().unwrap_or_default();
                    let draft_changed = next_toml.trim() != previous_toml.trim();
                    latest_install_draft = Some(next_draft.clone());
                    preview_session.record_retry_draft(&next_draft, retry_ordinal);
                    if let Some(warning) =
                        crate::preview::persist_session_with_warning(&preview_session)
                    {
                        if !json {
                            eprintln!("⚠️  {warning}");
                        }
                    }
                    current_draft = next_draft;

                    if !draft_changed {
                        if !json {
                            eprintln!(
                                "ℹ️  Retry draft {retry_ordinal}/{MAX_GITHUB_DRAFT_RETRIES} did not change the generated capsule.toml."
                            );
                        }
                        break;
                    }
                    if !current_draft.retryable {
                        if !json {
                            eprintln!(
                                "ℹ️  Retry draft {retry_ordinal}/{MAX_GITHUB_DRAFT_RETRIES} requested manual review instead of another automatic retry."
                            );
                        }
                        break;
                    }

                    if !json {
                        eprintln!(
                            "🔁 Retrying build with failure-aware draft ({retry_ordinal}/{MAX_GITHUB_DRAFT_RETRIES})..."
                        );
                        if let Some(hint) = current_draft.capsule_hint.as_ref() {
                            eprintln!("   Confidence: {}", hint.confidence);
                            if let Some(launchability) = hint.launchability.as_deref() {
                                eprintln!("   Launchability: {}", launchability);
                            }
                            for warning in &hint.warnings {
                                eprintln!("   Warning: {warning}");
                            }
                        }
                    }

                    match crate::build_github_repository_checkout(
                        checkout.checkout_dir.clone(),
                        json,
                        current_draft.preview_toml.clone(),
                        keep_failed_artifacts,
                        true,
                    )
                    .await
                    {
                        Ok(result) => {
                            recovered_build = Some(result);
                            break;
                        }
                        Err(retry_error) => {
                            let retry_smoke_report = retry_error
                                .downcast_ref::<crate::commands::build::InferredManifestSmokeFailure>()
                                .map(|failure| failure.report.clone());
                            last_error = retry_error;
                            let Some(retry_report) = retry_smoke_report else {
                                break;
                            };
                            current_report = retry_report.clone();
                            preview_session.record_smoke_failure(&retry_report);
                            if let Some(warning) =
                                crate::preview::persist_session_with_warning(&preview_session)
                            {
                                if !json {
                                    eprintln!("⚠️  {warning}");
                                }
                            }
                            if let Some(attempt) = inference_attempt.as_ref() {
                                let _ = crate::inference_feedback::submit_smoke_failed(
                                    attempt,
                                    &retry_report,
                                )
                                .await;
                            }
                        }
                    }
                }

                if let Some(result) = recovered_build {
                    result
                } else if crate::preview::draft_requires_manual_review(
                    latest_install_draft.as_ref().unwrap_or(draft),
                ) {
                    preview_session.record_manual_intervention_required(&current_report.message);
                    if let Some(warning) =
                        crate::preview::persist_session_with_warning(&preview_session)
                    {
                        if !json {
                            eprintln!("⚠️  {warning}");
                        }
                    }
                    crate::maybe_keep_failed_github_checkout(
                        &mut checkout,
                        keep_failed_artifacts,
                        json,
                    );
                    return Err(crate::build_github_manual_intervention_error(
                        &preview_session.manifest_path,
                        repository,
                        latest_install_draft.as_ref().unwrap_or(draft),
                        &current_report.message,
                    )?);
                } else if can_prompt {
                    let draft_for_manual_fix = latest_install_draft.as_ref().unwrap_or(draft);
                    let manual_manifest_path = preview_session.manifest_path.clone();
                    if let Some(recovered) = crate::retry_github_build_after_manual_fix(
                        &mut preview_session,
                        &manual_manifest_path,
                        &checkout.checkout_dir,
                        repository,
                        draft_for_manual_fix,
                        inference_attempt.as_ref(),
                        json,
                        keep_failed_artifacts,
                    )
                    .await?
                    {
                        recovered
                    } else {
                        crate::maybe_keep_failed_github_checkout(
                            &mut checkout,
                            keep_failed_artifacts,
                            json,
                        );
                        return Err(last_error);
                    }
                } else {
                    crate::maybe_keep_failed_github_checkout(
                        &mut checkout,
                        keep_failed_artifacts,
                        json,
                    );
                    return Err(last_error);
                }
            } else {
                crate::maybe_keep_failed_github_checkout(
                    &mut checkout,
                    keep_failed_artifacts,
                    json,
                );
                return Err(last_error);
            }
        }
    };
    let result = async {
        let artifact = build_result.artifact.ok_or_else(|| {
            anyhow::anyhow!("GitHub repository did not produce an installable .capsule artifact")
        })?;
        install::install_built_github_artifact(
            &artifact,
            &checkout.publisher,
            &checkout.repository,
            install::InstallExecutionOptions {
                output_dir,
                yes,
                projection_preference,
                json_output: json,
                can_prompt_interactively: can_prompt,
                promotion_source: Some(install::PromotionSourceInfo {
                    preview_id: preview_session.preview_id.clone(),
                    source_reference: preview_session.target_reference.clone(),
                    source_metadata_path: preview_session.metadata_path.clone(),
                    source_manifest_path: preview_session.manifest_path.clone(),
                    manifest_source: preview_session.manifest_source.clone(),
                    inference_mode: preview_session.inference_mode.clone(),
                    resolved_ref: preview_session.resolved_ref.clone(),
                    derived_plan: install::PromotionDerivedPlanSnapshot {
                        runtime: preview_session.derived_plan.runtime.clone(),
                        driver: preview_session.derived_plan.driver.clone(),
                        resolved_runtime_version: preview_session
                            .derived_plan
                            .resolved_runtime_version
                            .clone(),
                        resolved_port: preview_session.derived_plan.resolved_port,
                        resolved_lock_files: preview_session
                            .derived_plan
                            .resolved_lock_files
                            .clone(),
                        resolved_pack_include: preview_session
                            .derived_plan
                            .resolved_pack_include
                            .clone(),
                        warnings: preview_session.derived_plan.warnings.clone(),
                        deferred_constraints: preview_session
                            .derived_plan
                            .deferred_constraints
                            .clone(),
                        promotion_eligibility: match preview_session
                            .derived_plan
                            .promotion_eligibility
                        {
                            crate::preview::PreviewPromotionEligibility::Eligible => {
                                "eligible".to_string()
                            }
                            crate::preview::PreviewPromotionEligibility::RequiresManualReview => {
                                "requires_manual_review".to_string()
                            }
                            crate::preview::PreviewPromotionEligibility::Blocked => {
                                "blocked".to_string()
                            }
                        },
                    },
                }),
                keep_progressive_flow_open: true,
            },
        )
        .await
    }
    .await;

    if result.is_err() {
        crate::maybe_keep_failed_github_checkout(&mut checkout, keep_failed_artifacts, json);
    }

    result
}
