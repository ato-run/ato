use std::cmp::Ordering;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{Context, Result};
use rand::Rng;

use capsule::AtoError;
use capsule::CapsuleReporter;
use capsule::execution_plan::error::AtoExecutionError;
use capsule::input_resolver::{
    CAPSULE_LOCK_FILE_NAME, DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME, ResolveInputOptions,
    ResolvedInput, resolve_authoritative_input,
};
use capsule::smoke::SmokeFailureClass;
use tracing::debug;

use crate::application::ports::OutputPort;
use crate::commands;
use crate::inference_feedback;
use crate::install;
use crate::preview;
use crate::progressive_ui;
use crate::reporters;
use crate::runtime::tree as runtime_tree;
use crate::tui;
use crate::{
    CompatibilityFallbackBackend, DEFAULT_RUN_REGISTRY_URL, EnforcementMode, GitHubAutoFixMode,
    ProviderToolchain,
};

use super::github_archive::{
    github_run_checkout_root, sweep_stale_github_run_checkouts_best_effort,
    write_github_run_checkout_owner_marker,
};

pub(crate) async fn resolve_installed_capsule_archive(
    scoped_ref: &install::ScopedCapsuleRef,
    registry: Option<&str>,
    preferred_version: Option<&str>,
) -> Result<Option<PathBuf>> {
    let store_root = capsule::common::paths::ato_path_or_workspace_tmp("store");
    if let Some(path) = resolve_installed_capsule_archive_in_store(
        &store_root.join(&scoped_ref.publisher),
        &scoped_ref.slug,
        preferred_version,
    )? {
        return Ok(Some(path));
    }

    let legacy_slug_dir = store_root.join(&scoped_ref.slug);
    if !legacy_slug_dir.exists() || !legacy_slug_dir.is_dir() {
        return Ok(None);
    }

    let scoped_slug_dir = store_root
        .join(&scoped_ref.publisher)
        .join(&scoped_ref.slug);
    if scoped_slug_dir.exists() {
        return resolve_installed_capsule_archive_in_store(
            &store_root.join(&scoped_ref.publisher),
            &scoped_ref.slug,
            preferred_version,
        );
    }

    let effective_registry = registry.unwrap_or(DEFAULT_RUN_REGISTRY_URL);
    let suggestions =
        install::suggest_scoped_capsules(&scoped_ref.slug, Some(effective_registry), 10).await?;
    let scoped_matches: Vec<_> = suggestions
        .iter()
        .filter(|candidate| {
            candidate
                .scoped_id
                .ends_with(&format!("/{}", scoped_ref.slug))
        })
        .collect();
    let unique_match =
        scoped_matches.len() == 1 && scoped_matches[0].scoped_id == scoped_ref.scoped_id;

    if !unique_match {
        anyhow::bail!(
            "Legacy installation found at {} but publisher could not be determined safely. Please reinstall using: ato install {}",
            legacy_slug_dir.display(),
            scoped_ref.scoped_id
        );
    }

    if let Some(parent) = scoped_slug_dir.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create scoped store directory: {}",
                parent.display()
            )
        })?;
    }
    std::fs::rename(&legacy_slug_dir, &scoped_slug_dir).with_context(|| {
        format!(
            "Failed to migrate legacy store path {} -> {}",
            legacy_slug_dir.display(),
            scoped_slug_dir.display()
        )
    })?;

    resolve_installed_capsule_archive_in_store(
        &store_root.join(&scoped_ref.publisher),
        &scoped_ref.slug,
        preferred_version,
    )
}

pub(crate) fn show_github_draft_preview(
    preview_session: &preview::PreviewSession,
    json: bool,
) -> Result<()> {
    if json || preview_session.manifest_source.as_deref() != Some("inferred") {
        return Ok(());
    }

    let Some(preview_toml) = preview_session.preview_toml.as_deref() else {
        return Ok(());
    };

    if crate::progressive_ui::can_use_progressive_ui(false) {
        crate::progressive_ui::render_generated_manifest_preview(
            &preview_session.manifest_path,
            preview_toml,
        )?;
    } else {
        eprintln!(
            "   Generated capsule.toml preview: {}",
            preview_session.manifest_path.display()
        );
        eprintln!("   ----- capsule.toml -----");
        for (index, line) in preview_toml.lines().enumerate() {
            eprintln!("   {:>3} | {}", index + 1, line);
        }
        eprintln!("   -----------------------");
    }

    Ok(())
}

pub(crate) fn maybe_keep_failed_github_checkout(
    checkout: &mut install::GitHubCheckout,
    keep_failed_artifacts: bool,
    json: bool,
) {
    if keep_failed_artifacts && !json {
        let kept_checkout = checkout.preserve_for_debugging();
        eprintln!(
            "⚠️  Kept failed GitHub checkout for debugging: {}",
            kept_checkout.display()
        );
    }
}

fn cleanup_successful_github_checkout_best_effort(checkout_dir: &Path) {
    match std::fs::remove_dir_all(checkout_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => debug!(
            path = %checkout_dir.display(),
            error = %error,
            "failed to remove successful GitHub checkout"
        ),
    }
}

pub(crate) async fn run_blocking_github_install_step<T, F>(operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .context("GitHub repository build task failed")?
}

pub(crate) async fn build_github_repository_checkout(
    checkout_dir: PathBuf,
    json: bool,
    injected_manifest: Option<String>,
    keep_failed_artifacts: bool,
    suppress_injected_manifest_warning: bool,
) -> Result<commands::build::BuildResult> {
    run_blocking_github_install_step(move || {
        let reporter = std::sync::Arc::new(reporters::CliReporter::new(json));
        commands::build::execute_pack_command_with_injected_manifest(
            checkout_dir,
            false,
            None,
            false,
            false,
            false,
            keep_failed_artifacts,
            false,
            EnforcementMode::Strict.as_str().to_string(),
            reporter,
            false,
            json,
            None,
            injected_manifest.as_deref(),
            suppress_injected_manifest_warning,
        )
    })
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn retry_github_build_after_manual_fix(
    preview_session: &mut preview::PreviewSession,
    manual_manifest_path: &Path,
    checkout_dir: &Path,
    repository: &str,
    install_draft: &install::GitHubInstallDraftResponse,
    inference_attempt: Option<&inference_feedback::InferenceAttemptHandle>,
    json: bool,
    keep_failed_artifacts: bool,
) -> Result<Option<commands::build::BuildResult>> {
    let should_edit = progressive_ui::confirm_with_fallback(
        "Edit generated capsule manifest and retry? ",
        true,
        progressive_ui::can_use_progressive_ui(false),
    )?;
    if !should_edit {
        return Ok(None);
    }

    let inferred_manifest = install_draft
        .preview_toml
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("store draft previewToml missing for manual fix"))?;
    inference_feedback::write_manual_manifest(manual_manifest_path, inferred_manifest)?;

    eprintln!("Open editor for {}", manual_manifest_path.display());
    if !inference_feedback::can_open_editor_automatically() {
        return Err(build_github_manual_intervention_error(
            manual_manifest_path,
            repository,
            install_draft,
            "No editor launcher is available for manual fix mode",
        )?);
    }
    inference_feedback::open_editor(manual_manifest_path)?;
    let edited_manifest = inference_feedback::read_manual_manifest(manual_manifest_path)?;
    if edited_manifest.trim().is_empty() {
        anyhow::bail!("edited capsule.toml is empty");
    }
    preview_session.record_manual_fix(&edited_manifest);
    let _ = preview::persist_session_with_warning(preview_session);

    let retry_result = build_github_repository_checkout(
        checkout_dir.to_path_buf(),
        json,
        Some(edited_manifest.clone()),
        keep_failed_artifacts,
        false,
    )
    .await?;

    eprintln!(
        "{}",
        inference_feedback::summarize_manifest_diff(inferred_manifest, &edited_manifest)
    );
    if let Some(attempt) = inference_attempt {
        let should_share = progressive_ui::confirm_with_fallback(
            "Share this corrected configuration to improve ato for public GitHub repositories? ",
            true,
            progressive_ui::can_use_progressive_ui(false),
        )?;
        if should_share {
            let _ = inference_feedback::submit_verified_fix(attempt, &edited_manifest).await;
        }
    }

    Ok(Some(retry_result))
}

pub(crate) fn github_build_error_requires_manual_intervention(error: &anyhow::Error) -> bool {
    let combined = error
        .chain()
        .map(|cause| cause.to_string())
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();

    combined.contains("uv.lock is missing")
        || combined.contains("uv.lock is required")
        || combined.contains("requires uv.lock")
        || combined.contains("yarn.lock")
        || combined.contains("pnpm-lock.yaml is missing")
        || combined.contains("package-lock.json")
        || combined.contains("requires one of package-lock.json")
        || combined.contains("multiple node lockfiles detected")
        || combined.contains("fail-closed provisioning")
        || combined.contains("yarn install --frozen-lockfile")
        || combined.contains("bun install --frozen-lockfile")
        || combined.contains("lockfile had changes, but lockfile is frozen")
        || combined.contains("lockfile is frozen")
}

pub(crate) fn github_build_error_manual_review_reason(error: &anyhow::Error) -> String {
    let message = error.to_string();
    if !message.trim().is_empty() {
        return message;
    }

    if github_build_error_requires_manual_intervention(error) {
        "Provisioning failed under inferred fail-closed lockfile checks. Review the generated draft and refresh the repository lockfiles before retrying."
            .to_string()
    } else {
        "GitHub inferred draft build failed and requires manual review.".to_string()
    }
}

pub(crate) fn build_github_manual_intervention_error(
    manual_manifest_path: &Path,
    repository: &str,
    install_draft: &install::GitHubInstallDraftResponse,
    failure_reason: &str,
) -> Result<anyhow::Error> {
    if let Some(preview_toml) = install_draft.preview_toml.as_deref() {
        inference_feedback::write_manual_manifest(manual_manifest_path, preview_toml)?;
    }

    let next_steps = build_github_manual_intervention_next_steps(
        repository,
        install_draft,
        manual_manifest_path,
    );
    let required_env = install_draft
        .preview_toml
        .as_deref()
        .map(preview::required_env_from_preview_toml)
        .unwrap_or_default();
    let lowered = failure_reason.to_ascii_lowercase();

    if !required_env.is_empty()
        && (lowered.contains("required environment")
            || lowered.contains("environment variable")
            || lowered.contains("must be set")
            || required_env.iter().any(|key| failure_reason.contains(key)))
    {
        return Ok(AtoExecutionError::missing_required_env(
            format!(
                "missing required environment variables for inferred GitHub draft: {}",
                required_env.join(", ")
            ),
            required_env,
            Vec::new(),
            Some("github-inference"),
        )
        .into());
    }

    let lockfile_target = if lowered.contains("uv.lock") {
        Some("uv.lock")
    } else if lowered.contains("yarn.lock") {
        Some("yarn.lock")
    } else if lowered.contains("pnpm-lock.yaml") {
        Some("pnpm-lock.yaml")
    } else if lowered.contains("package-lock.json") {
        Some("package-lock.json")
    } else if lowered.contains("bun.lockb") {
        Some("bun.lockb")
    } else if lowered.contains("bun.lock") {
        Some("bun.lock")
    } else if lowered.contains("multiple node lockfiles") {
        Some("node-lockfile")
    } else {
        None
    };
    if let Some(lockfile_target) = lockfile_target {
        return Ok(AtoExecutionError::lock_incomplete(
            failure_reason.to_string(),
            Some(lockfile_target),
        )
        .into());
    }

    if lowered.contains("ambiguous entrypoint")
        || lowered.contains("multiple candidate entrypoints")
        || lowered.contains("more than one entrypoint")
    {
        return Ok(
            AtoExecutionError::ambiguous_entrypoint(failure_reason.to_string(), Vec::new()).into(),
        );
    }

    Ok(inference_feedback::build_manual_intervention_error(
        manual_manifest_path,
        failure_reason,
        &next_steps,
    )
    .into())
}

#[cfg(test)]
pub(crate) fn build_github_manual_intervention_message(
    repository: &str,
    install_draft: &install::GitHubInstallDraftResponse,
    manifest_path: &Path,
    failure_reason: &str,
) -> String {
    inference_feedback::build_manual_intervention_message(
        manifest_path,
        failure_reason,
        &build_github_manual_intervention_next_steps(repository, install_draft, manifest_path),
    )
}

fn build_github_manual_intervention_next_steps(
    repository: &str,
    install_draft: &install::GitHubInstallDraftResponse,
    manifest_path: &Path,
) -> Vec<String> {
    let mut next_steps = Vec::new();
    let required_env = install_draft
        .preview_toml
        .as_deref()
        .map(preview::required_env_from_preview_toml)
        .unwrap_or_default();
    if !required_env.is_empty() {
        next_steps.push(format!(
            "Set the required environment variables before rerunning: {}.",
            required_env.join(", ")
        ));
    }
    if let Some(hint) = install_draft.capsule_hint.as_ref() {
        for warning in hint.warnings.iter().take(2) {
            next_steps.push(warning.clone());
        }
    }
    next_steps.push(format!(
        "Review {} and adjust the generated command or target settings as needed.",
        manifest_path.display()
    ));
    if !inference_feedback::can_open_editor_automatically() {
        next_steps.push(
            "Install a text editor or set VISUAL/EDITOR if you want ato to open the file automatically.".to_string(),
        );
    }
    next_steps.push(format!(
        "Rerun `ato run {repository}` after the prerequisites are ready."
    ));
    next_steps
}

pub(crate) fn resolve_installed_capsule_archive_in_store(
    store_root: &Path,
    slug: &str,
    preferred_version: Option<&str>,
) -> Result<Option<PathBuf>> {
    let slug_dir = store_root.join(slug);
    if !slug_dir.exists() || !slug_dir.is_dir() {
        return Ok(None);
    }

    if let Some(version) = preferred_version
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let version_dir = slug_dir.join(version);
        if !version_dir.exists() || !version_dir.is_dir() {
            return Ok(None);
        }
        return select_capsule_file_in_version(&version_dir);
    }

    let mut version_dirs: Vec<(ParsedSemver, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(&slug_dir)
        .with_context(|| format!("Failed to read store directory: {}", slug_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(version_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if let Some(parsed) = ParsedSemver::parse(version_name) {
            version_dirs.push((parsed, path));
        }
    }

    version_dirs.sort_by(|(a, _), (b, _)| compare_semver(a, b).reverse());

    for (_, version_dir) in version_dirs {
        if let Some(capsule_path) = select_capsule_file_in_version(&version_dir)? {
            return Ok(Some(capsule_path));
        }
    }

    Ok(None)
}

pub(crate) fn select_capsule_file_in_version(version_dir: &Path) -> Result<Option<PathBuf>> {
    let mut capsules = Vec::new();
    for entry in std::fs::read_dir(version_dir).with_context(|| {
        format!(
            "Failed to read version directory: {}",
            version_dir.display()
        )
    })? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file()
            && path
                .extension()
                .is_some_and(|ext| ext.to_string_lossy().eq_ignore_ascii_case("capsule"))
        {
            capsules.push(path);
        }
    }

    capsules.sort();
    Ok(capsules.into_iter().next())
}

pub(crate) fn prompt_install_confirmation(
    detail: &install::CapsuleDetailSummary,
    resolved_version: &str,
) -> Result<bool> {
    println!();
    println!("[!] Capsule '{}' is not installed.", detail.scoped_id);
    println!();
    let name = if detail.name.trim().is_empty() {
        detail.slug.as_str()
    } else {
        detail.name.trim()
    };
    println!("📦 {} (v{})", name, resolved_version);
    if !detail.description.trim().is_empty() {
        println!("{}", detail.description.trim());
    }

    print_permission_summary(detail.permissions.as_ref());
    println!();

    loop {
        print!("? Do you want to install and run this capsule? (Y/n): ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .context("Failed to read user input")?;

        match input.trim().to_ascii_lowercase().as_str() {
            "" | "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => println!("Please answer 'y' or 'n'."),
        }
    }
}

fn print_permission_summary(permissions: Option<&install::CapsulePermissions>) {
    println!("This capsule requests the following permissions:");
    let Some(permissions) = permissions else {
        println!("  - No permissions metadata declared");
        return;
    };

    let mut printed_any = false;

    if let Some(network) = permissions.network.as_ref() {
        let endpoints = network.merged_endpoints();
        if !endpoints.is_empty() {
            printed_any = true;
            println!("  🌐 Network:");
            for endpoint in endpoints {
                println!("    - {}", endpoint);
            }
        }
    }

    if let Some(isolation) = permissions.isolation.as_ref()
        && !isolation.allow_env.is_empty()
    {
        printed_any = true;
        println!("  🔑 Isolation env allowlist:");
        for env in &isolation.allow_env {
            println!("    - {}", env);
        }
    }

    if let Some(filesystem) = permissions.filesystem.as_ref() {
        if !filesystem.read_only.is_empty() {
            printed_any = true;
            println!("  📁 Filesystem read-only:");
            for path in &filesystem.read_only {
                println!("    - {}", path);
            }
        }
        if !filesystem.read_write.is_empty() {
            printed_any = true;
            println!("  ✍️  Filesystem read-write:");
            for path in &filesystem.read_write {
                println!("    - {}", path);
            }
        }
    }

    if !printed_any {
        println!("  - No permissions metadata declared");
    }
}

pub(crate) fn can_prompt_interactively(stdin_is_tty: bool, stdout_is_tty: bool) -> bool {
    tui::can_launch_tui(stdin_is_tty, stdout_is_tty)
}

pub(crate) fn ensure_run_auto_install_allowed(
    yes: bool,
    json_mode: bool,
    stdin_is_tty: bool,
    stdout_is_tty: bool,
) -> Result<()> {
    if json_mode && !yes {
        return Err(anyhow::Error::new(AtoExecutionError::from_ato_error(
            AtoError::InstallConsentRequired {
                message: "Non-interactive JSON mode requires -y/--yes when auto-installing missing capsules".to_string(),
                hint: Some("Re-run with -y/--yes in CI or non-interactive environments.".to_string()),
            },
        )));
    }

    if !yes && !can_prompt_interactively(stdin_is_tty, stdout_is_tty) {
        return Err(anyhow::Error::new(AtoExecutionError::from_ato_error(
            AtoError::InstallConsentRequired {
                message: "Interactive install confirmation requires a TTY. Re-run with -y/--yes in CI or non-interactive environments.".to_string(),
                hint: Some("Re-run with -y/--yes in CI or non-interactive environments.".to_string()),
            },
        )));
    }

    Ok(())
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct ParsedSemver {
    major: u64,
    minor: u64,
    patch: u64,
    pre_release: Option<String>,
}

impl ParsedSemver {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }

        let without_build = trimmed.split('+').next()?;
        let (core, pre_release) = if let Some((core, pre)) = without_build.split_once('-') {
            (core, Some(pre.to_string()))
        } else {
            (without_build, None)
        };

        let mut parts = core.split('.');
        let major = parts.next()?.parse::<u64>().ok()?;
        let minor = parts.next()?.parse::<u64>().ok()?;
        let patch = parts.next()?.parse::<u64>().ok()?;
        if parts.next().is_some() {
            return None;
        }

        Some(Self {
            major,
            minor,
            patch,
            pre_release,
        })
    }
}

pub(crate) fn compare_semver(a: &ParsedSemver, b: &ParsedSemver) -> Ordering {
    a.major
        .cmp(&b.major)
        .then_with(|| a.minor.cmp(&b.minor))
        .then_with(|| a.patch.cmp(&b.patch))
        .then_with(|| match (&a.pre_release, &b.pre_release) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a_pre), Some(b_pre)) => a_pre.cmp(b_pre),
        })
}

pub(crate) fn enforce_sandbox_mode_flags(
    enforcement: EnforcementMode,
    sandbox_requested: bool,
    dangerously_skip_permissions: bool,
    compatibility_fallback: Option<CompatibilityFallbackBackend>,
    reporter: std::sync::Arc<reporters::CliReporter>,
) -> Result<EnforcementMode> {
    const ENV_ALLOW_UNSAFE: &str = "CAPSULE_ALLOW_UNSAFE";

    if matches!(enforcement, EnforcementMode::BestEffort) {
        return Err(anyhow::Error::new(AtoExecutionError::from_ato_error(
            AtoError::SecurityPolicyViolation {
                message: "--enforcement best-effort is no longer supported".to_string(),
                hint: Some("Use --enforcement strict (the default) instead.".to_string()),
                resource: None,
                blocked_host: None,
            },
        )));
    }

    if matches!(enforcement, EnforcementMode::Strict) && sandbox_requested {
        futures::executor::block_on(
            reporter.warn(
                "⚠️  Sandbox mode enabled: Tier2 targets will run under strict native sandboxing"
                    .to_string(),
            ),
        )?;
    }

    if dangerously_skip_permissions && compatibility_fallback.is_some() {
        return Err(anyhow::Error::new(AtoExecutionError::from_ato_error(
            AtoError::SecurityPolicyViolation {
                message: "--dangerously-skip-permissions and --compatibility-fallback are mutually exclusive".to_string(),
                hint: Some("Remove one of the two flags before retrying.".to_string()),
                resource: None,
                blocked_host: None,
            },
        )));
    }

    if dangerously_skip_permissions {
        if std::env::var(ENV_ALLOW_UNSAFE).ok().as_deref() != Some("1") {
            return Err(anyhow::Error::new(AtoExecutionError::from_ato_error(
                AtoError::SecurityPolicyViolation {
                    message: format!(
                        "--dangerously-skip-permissions requires {}=1",
                        ENV_ALLOW_UNSAFE
                    ),
                    hint: Some(
                        "Set CAPSULE_ALLOW_UNSAFE=1 only if you intentionally want to bypass Ato permission and sandbox checks.".to_string()
                    ),
                    resource: None,
                    blocked_host: None,
                },
            )));
        }
        futures::executor::block_on(
            reporter.warn(
                "⚠️  Dangerous mode enabled: bypassing all Ato runtime permission and sandbox barriers"
                    .to_string(),
            ),
        )?;
    }

    if let Some(CompatibilityFallbackBackend::Host) = compatibility_fallback {
        futures::executor::block_on(reporter.warn(
            "⚠ Running in Compatibility Mode (Isolated Host Environment). Nacelle sandbox is disabled."
                .to_string(),
        ))?;
    }

    Ok(enforcement)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedCliExportRequest {
    pub(crate) scoped_id: String,
    pub(crate) export_name: String,
    pub(crate) backend_kind: String,
    pub(crate) target_label: String,
    pub(crate) prefix_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedRunTarget {
    pub(crate) path: PathBuf,
    pub(crate) agent_local_root: Option<PathBuf>,
    pub(crate) desktop_open_path: Option<PathBuf>,
    pub(crate) export_request: Option<ResolvedCliExportRequest>,
    pub(crate) provider_workspace: Option<install::provider_target::ProviderRunWorkspace>,
    pub(crate) transient_workspace_root: Option<PathBuf>,
    pub(crate) community_submit_context: Option<crate::community::CommunitySubmitPromptContext>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalRunManifestPreparationOutcome {
    Ready,
    CreatedManualManifest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LocalRunManifestStatus {
    Valid,
    Missing,
    Invalid { error: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunManifestRecoveryChoice {
    Generate,
    Create,
    Abort,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn resolve_run_target_or_install(
    path: PathBuf,
    yes: bool,
    provider_toolchain: ProviderToolchain,
    use_existing_toml: Option<String>,
    explicit_commit: Option<String>,
    keep_failed_artifacts: bool,
    auto_fix_mode: Option<GitHubAutoFixMode>,
    allow_unverified: bool,
    registry: Option<&str>,
    reporter: Arc<reporters::CliReporter>,
) -> Result<ResolvedRunTarget> {
    let raw = path.to_string_lossy().to_string();
    let export_invocation = raw.trim().starts_with('@');
    let expanded_local = crate::local_input::expand_local_path(&raw);

    // Guard: reject paths that point into the Ato home directory.  These are
    // Ato's own internal state (store, runs, tmp …) and running them directly
    // would fail with a confusing E999.  Emit a helpful typed error instead.
    if let Ok(ato_home) = capsule::common::paths::nacelle_home_dir()
        && expanded_local.starts_with(&ato_home)
        && !is_import_preview_run_target(&expanded_local, &ato_home)
    {
        let capsule_name = expanded_local
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "my-capsule".to_string());
        return Err(anyhow::Error::new(AtoExecutionError::from_ato_error(
            AtoError::EntrypointInvalid {
                message: format!(
                    "'{}' is inside Ato's internal state directory and cannot be used as a run target",
                    raw
                ),
                hint: Some(format!(
                    "Copy the capsule to a local directory first:\n  cp -r {} ./{}\n  ato run ./{}",
                    raw, capsule_name, capsule_name
                )),
                field: None,
            },
        )));
    }
    match install::provider_target::classify_run_target(&raw, &expanded_local)? {
        install::provider_target::ParsedRunTarget::LocalPath(local_path) => {
            if provider_toolchain != ProviderToolchain::Auto {
                anyhow::bail!(
                    "`--via {}` is only supported for provider-backed targets in this MVP. Use `ato run pypi:<package> -- ...` or `ato run npm:<package> -- ...`.",
                    provider_toolchain.as_str()
                );
            }
            return Ok(ResolvedRunTarget {
                agent_local_root: agent_local_root_for_path(&local_path),
                path: local_path,
                desktop_open_path: None,
                export_request: None,
                provider_workspace: None,
                transient_workspace_root: None,
                community_submit_context: None,
            });
        }
        install::provider_target::ParsedRunTarget::GitHubRepository(repository) => {
            sweep_stale_github_run_checkouts_best_effort();
            if provider_toolchain != ProviderToolchain::Auto {
                anyhow::bail!(
                    "`--via {}` is only supported for provider-backed targets in this MVP. Use `ato run pypi:<package> -- ...` or `ato run npm:<package> -- ...`.",
                    provider_toolchain.as_str()
                );
            }
            let json_mode = reporter.is_json();
            if crate::progressive_ui::can_use_progressive_ui(json_mode) {
                crate::progressive_ui::begin_flow_without_logo()?;
            }
            if json_mode && !yes {
                return Err(anyhow::Error::new(AtoExecutionError::from_ato_error(
                    AtoError::InstallConsentRequired {
                        message: "Non-interactive JSON mode requires -y/--yes when auto-installing missing capsules".to_string(),
                        hint: Some("Re-run with -y/--yes in CI or non-interactive environments.".to_string()),
                    },
                )));
            }

            if !yes
                && !can_prompt_interactively(
                    std::io::stdin().is_terminal(),
                    std::io::stdout().is_terminal(),
                )
            {
                return Err(anyhow::Error::new(AtoExecutionError::from_ato_error(
                    AtoError::InstallConsentRequired {
                        message: "Interactive install confirmation requires a TTY. Re-run with -y/--yes in CI or non-interactive environments.".to_string(),
                        hint: Some("Re-run with -y/--yes in CI or non-interactive environments.".to_string()),
                    },
                )));
            }

            if yes {
                debug!(
                    repository = %repository,
                    "GitHub repository not installed locally; continuing with -y auto-install"
                );
            }

            let is_tty = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
            // Set only when a community capsule.toml is chosen SILENTLY (the
            // non-interactive `-y` single-candidate auto-pick). Such a snapshot
            // must defer to a repo that ships its own manifest — see
            // `should_apply_effective_toml`.
            let mut community_auto_selected = false;
            let selected_community_toml: Option<String> = if use_existing_toml.is_some() {
                None
            } else {
                match crate::community::fetch_community_capsule_tomls(&repository).await {
                    Ok(candidates) if !candidates.is_empty() => {
                        let mut candidates = candidates;
                        crate::community::sort_candidates(&mut candidates);
                        let selected = if is_tty && !json_mode {
                            match crate::community::prompt_community_candidate_selection(
                                &repository,
                                &candidates,
                            ) {
                                Ok(n) if n <= candidates.len() => Some(n - 1),
                                Ok(_) => None,
                                Err(_) => None,
                            }
                        } else if candidates.len() == 1 && yes {
                            // Silent auto-pick: applied only if the repo ships
                            // no manifest of its own (see overwrite site below).
                            community_auto_selected = true;
                            Some(0)
                        } else {
                            return Err(anyhow::Error::new(AtoExecutionError::from_ato_error(
                                AtoError::EntrypointInvalid {
                                    message: format!(
                                        "TOML_SELECTION_REQUIRED: multiple community \
                                         capsule.toml candidates exist for '{}'. \
                                         Use -T/--use-existing-toml, or run interactively.",
                                        repository
                                    ),
                                    hint: Some(
                                        "Re-run in a TTY or use -T/--use-existing-toml to \
                                         select a specific capsule.toml."
                                            .to_string(),
                                    ),
                                    field: None,
                                },
                            )));
                        };

                        match selected {
                            Some(idx) => {
                                let candidate = &candidates[idx];
                                crate::community::validate_candidate_source_matches_run_target(
                                    &candidate.source,
                                    &repository,
                                )
                                .with_context(|| {
                                    format!(
                                        "Community candidate '{}' source mismatch with run target",
                                        candidate.id
                                    )
                                })?;
                                let toml_content =
                                    crate::community::fetch_capsule_toml_by_id(&candidate.id)
                                        .await
                                        .with_context(|| {
                                            format!(
                                                "Failed to fetch community capsule.toml '{}'",
                                                candidate.id
                                            )
                                        })?;
                                let validation =
                                    crate::community::validate_capsule_toml_source_with_provenance(
                                        &toml_content,
                                        &repository,
                                        &candidate.source,
                                    );
                                match validation {
                                    crate::community::SourceValidationOutcome::Match => {}
                                    crate::community::SourceValidationOutcome::MissingSource => {
                                        unreachable!(
                                            "validate_capsule_toml_source_with_provenance never returns MissingSource"
                                        )
                                    }
                                    crate::community::SourceValidationOutcome::Mismatch {
                                        toml_source,
                                        expected_source: _,
                                    } => {
                                        anyhow::bail!(
                                            "Community capsule.toml '{}' source mismatch: \
                                             TOML declares '{}', expected '{}'.",
                                            candidate.id,
                                            toml_source,
                                            repository
                                        );
                                    }
                                }
                                Some(toml_content)
                            }
                            None => None,
                        }
                    }
                    Ok(_) => {
                        // API returned zero candidates for this source.
                        // In a TTY give the user a choice; non-TTY/JSON silently infers.
                        if is_tty
                            && !json_mode
                            && !crate::community::prompt_no_candidates_flow(&repository)?
                        {
                            anyhow::bail!("Run cancelled.");
                        }
                        None
                    }
                    Err(err) => {
                        debug!(
                            %err,
                            "community capsule.toml lookup failed; falling back to inference"
                        );
                        None
                    }
                }
            };

            let community_selected = selected_community_toml.is_some();

            let (resolved_existing_toml, local_t_path) = if let Some(toml_input) =
                use_existing_toml.as_deref()
            {
                if toml_input.starts_with("http://") || toml_input.starts_with("https://") {
                    let content = crate::community::fetch_toml_from_url(toml_input).await?;
                    let validation =
                        crate::community::validate_capsule_toml_source_matches_run_target(
                            &content,
                            &repository,
                        );
                    match validation {
                        crate::community::SourceValidationOutcome::Match => {}
                        crate::community::SourceValidationOutcome::MissingSource => {
                            anyhow::bail!(
                                "Remote capsule.toml from {toml_input} does not contain \
                                 source.repository or metadata.repository. A remote TOML \
                                 must declare its source identity."
                            );
                        }
                        crate::community::SourceValidationOutcome::Mismatch {
                            toml_source,
                            expected_source: _,
                        } => {
                            anyhow::bail!(
                                "Remote capsule.toml from {toml_input} source mismatch: \
                                 TOML declares '{toml_source}', expected '{repository}'."
                            );
                        }
                    }
                    (Some(content), None)
                } else {
                    let expanded = crate::local_input::expand_local_path(toml_input);
                    let content = std::fs::read_to_string(&expanded).with_context(|| {
                        format!("Failed to read TOML file: {}", expanded.display())
                    })?;
                    let validation =
                        crate::community::validate_capsule_toml_source_matches_run_target(
                            &content,
                            &repository,
                        );
                    match validation {
                        crate::community::SourceValidationOutcome::Match => {}
                        crate::community::SourceValidationOutcome::Mismatch {
                            toml_source,
                            expected_source: _,
                        } => {
                            anyhow::bail!(
                                "Source identity mismatch: the capsule.toml at '{}' \
                                 declares '{toml_source}', but run target is '{repository}'.",
                                expanded.display(),
                            );
                        }
                        crate::community::SourceValidationOutcome::MissingSource => {
                            if is_tty {
                                if !progressive_ui::confirm_with_fallback(
                                    &format!(
                                        "WARNING: The capsule.toml at '{}' does not declare \
                                         a source.repository. Continue anyway? [y/N] ",
                                        expanded.display()
                                    ),
                                    false,
                                    progressive_ui::can_use_progressive_ui(json_mode),
                                )? {
                                    anyhow::bail!("Aborted due to missing source identity.")
                                }
                            } else if yes {
                                futures::executor::block_on(reporter.warn(format!(
                                    "WARNING: capsule.toml at '{}' has no source.repository; \
                                     continuing because --yes is set.",
                                    expanded.display()
                                )))?;
                            } else {
                                anyhow::bail!(
                                    "capsule.toml at '{}' does not declare a source.repository. \
                                     Re-run with -y/--yes to continue anyway, or add \
                                     [source.repository] to the TOML.",
                                    expanded.display()
                                );
                            }
                        }
                    }
                    (Some(content), Some(expanded))
                }
            } else {
                (None, None)
            };

            let effective_toml = selected_community_toml.or(resolved_existing_toml);

            let resolved_commit = match explicit_commit.clone() {
                Some(commit) => commit,
                None => install::fetch_github_install_draft(&repository)
                    .await
                    .map(|draft| draft.resolved_ref.sha)
                    .map_err(|_| {
                        anyhow::anyhow!(
                            "network unavailable; cannot resolve <ref> for {}. Run online once, or pass --commit <sha> to skip resolver.",
                            repository
                        )
                    })?,
            };
            let checkout =
                install::download_github_repository_at_ref(&repository, Some(&resolved_commit))
                    .await?;
            let checkout_root = checkout.checkout_dir.clone();
            maybe_copy_env_example(&checkout_root, json_mode);

            let repo_ships_manifest = checkout_root.join("capsule.toml").exists();
            // A silently auto-selected community snapshot must not override the
            // repo's own manifest: the executed manifest (and the consent
            // policy derived from it) must be the materialized source, not a
            // stale catalog snapshot. Explicit -T / interactive picks still win.
            let apply_effective_toml = effective_toml.is_some()
                && should_apply_effective_toml(community_auto_selected, repo_ships_manifest);
            if community_auto_selected && repo_ships_manifest {
                futures::executor::block_on(reporter.notify(format!(
                    "Using the repository's own capsule.toml for {repository} (a community snapshot exists; pass -T to use it instead)."
                )))?;
            }

            if let Some(toml_content) = effective_toml.filter(|_| apply_effective_toml) {
                if !community_auto_selected {
                    futures::executor::block_on(
                        reporter.notify("Using the selected capsule.toml override.".to_string()),
                    )?;
                }
                let toml_path = checkout_root.join("capsule.toml");
                std::fs::write(&toml_path, toml_content).with_context(|| {
                    format!("Failed to write capsule.toml to {}", toml_path.display())
                })?;
                let preserved_root = relocate_github_run_checkout(&checkout_root)?;
                let preserved_toml_path = preserved_root.join("capsule.toml");
                let context = if community_selected {
                    None
                } else {
                    local_t_path.map(|_original_local_path| {
                        crate::community::CommunitySubmitPromptContext {
                            source: repository.clone(),
                            capsule_toml_path: preserved_toml_path,
                            origin: crate::community::CommunitySubmitOrigin::LocalOverride,
                        }
                    })
                };
                return Ok(ResolvedRunTarget {
                    path: preserved_root.clone(),
                    agent_local_root: Some(preserved_root.clone()),
                    desktop_open_path: None,
                    export_request: None,
                    provider_workspace: None,
                    transient_workspace_root: Some(preserved_root),
                    community_submit_context: context,
                });
            }

            if checkout_root.join("capsule.toml").exists() {
                let preserved_root = relocate_github_run_checkout(&checkout_root)?;
                let toml_path = preserved_root.join("capsule.toml");
                return Ok(ResolvedRunTarget {
                    path: preserved_root.clone(),
                    agent_local_root: Some(preserved_root.clone()),
                    desktop_open_path: None,
                    export_request: None,
                    provider_workspace: None,
                    transient_workspace_root: Some(preserved_root),
                    community_submit_context: Some(
                        crate::community::CommunitySubmitPromptContext {
                            source: repository.clone(),
                            capsule_toml_path: toml_path,
                            origin: crate::community::CommunitySubmitOrigin::ExistingRepoToml,
                        },
                    ),
                });
            }

            let install_result = install_github_repository(
                &repository,
                None,
                yes,
                install::ProjectionPreference::Skip,
                json_mode,
                !json_mode
                    && can_prompt_interactively(
                        std::io::stdin().is_terminal(),
                        std::io::stderr().is_terminal(),
                    ),
                keep_failed_artifacts,
                auto_fix_mode,
                explicit_commit.clone(),
            )
            .await?;

            let desktop_open_path = launchable_desktop_open_path(&install_result);
            let inferred_toml_path = install_result.path.join("capsule.toml");
            let context = if community_selected || local_t_path.is_some() {
                None
            } else {
                Some(crate::community::CommunitySubmitPromptContext {
                    source: repository.clone(),
                    capsule_toml_path: inferred_toml_path,
                    origin: crate::community::CommunitySubmitOrigin::Inferred,
                })
            };
            return Ok(ResolvedRunTarget {
                path: install_result.path,
                agent_local_root: None,
                desktop_open_path,
                export_request: None,
                provider_workspace: None,
                transient_workspace_root: None,
                community_submit_context: context,
            });
        }
        install::provider_target::ParsedRunTarget::Provider(provider_target) => {
            let workspace = install::provider_target::materialize_provider_run_workspace(
                &provider_target,
                provider_toolchain,
                keep_failed_artifacts,
                reporter.is_json(),
            )?;
            return Ok(ResolvedRunTarget {
                path: workspace.workspace_root.clone(),
                agent_local_root: Some(workspace.workspace_root.clone()),
                desktop_open_path: None,
                export_request: None,
                transient_workspace_root: Some(workspace.workspace_root.clone()),
                provider_workspace: Some(workspace),
                community_submit_context: None,
            });
        }
        install::provider_target::ParsedRunTarget::RegistryReference => {
            if provider_toolchain != ProviderToolchain::Auto {
                anyhow::bail!(
                    "`--via {}` is only supported for provider-backed targets in this MVP. Use `ato run pypi:<package> -- ...` or `ato run npm:<package> -- ...`.",
                    provider_toolchain.as_str()
                );
            }
        }
    }

    if provider_toolchain != ProviderToolchain::Auto {
        anyhow::bail!(
            "`--via {}` is only supported for provider-backed targets in this MVP. Use `ato run pypi:<package> -- ...` or `ato run npm:<package> -- ...`.",
            provider_toolchain.as_str()
        );
    }

    let scoped_ref = match install::parse_capsule_ref(&raw) {
        Ok(value) => value,
        Err(error) => {
            if install::is_slug_only_ref(&raw) {
                // A bare slug like `python` is allowed when exactly one
                // publisher owns a locally installed capsule with that slug
                // (i.e. `~/.ato/store/<publisher>/<slug>/<version>/` exists).
                // This matches the "same mental model as ato run" UX: users
                // who have already fetched a capsule into their CAS can
                // invoke it by its slug without repeating the publisher.
                match install::resolve_local_slug(&raw) {
                    Ok(install::LocalSlugResolution::Unique(scoped_id)) => {
                        debug!(slug = %raw, scoped_id = %scoped_id, "resolved bare slug from local store");
                        install::parse_capsule_ref(&scoped_id).with_context(|| {
                            format!(
                                "Locally resolved scoped id '{scoped_id}' is not a valid capsule reference"
                            )
                        })?
                    }
                    Ok(install::LocalSlugResolution::Ambiguous(candidates)) => {
                        let list = candidates
                            .iter()
                            .map(|scoped_id| format!("  - {scoped_id}"))
                            .collect::<Vec<_>>()
                            .join("\n");
                        anyhow::bail!(
                            "scoped_id_required: '{}' is installed under multiple publishers.\n\nLocal matches:\n{}\n\nRe-run with the publisher/slug form.",
                            raw,
                            list
                        );
                    }
                    _ => {
                        let effective_registry = registry.unwrap_or(DEFAULT_RUN_REGISTRY_URL);
                        anyhow::bail!(
                            "{}",
                            crate::scoped_id_prompt::run_scoped_id_prompt(
                                &raw,
                                Some(effective_registry)
                            )
                            .await?
                        );
                    }
                }
            } else {
                return Err(error).context(
                    "Invalid run target. Use a local path or existing .capsule file, or publisher/slug for store capsules.",
                );
            }
        }
    };

    let installed_capsule = resolve_installed_capsule_archive(&scoped_ref, registry, None).await?;
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

                if let Some(version) = registry_installable_version.as_deref()
                    && let Some(installed_capsule) =
                        resolve_installed_capsule_archive(&scoped_ref, registry, Some(version))
                            .await?
                {
                    debug!(
                        capsule = %installed_capsule.display(),
                        version = version,
                        "Using installed capsule matching registry current version"
                    );
                    return Ok(ResolvedRunTarget {
                        desktop_open_path: detect_desktop_open_path_for_installed_capsule(
                            &installed_capsule,
                        ),
                        path: installed_capsule.clone(),
                        agent_local_root: None,
                        export_request: resolve_cli_export_request(
                            export_invocation,
                            &scoped_ref,
                            &installed_capsule,
                        )?,
                        provider_workspace: None,
                        transient_workspace_root: None,
                        community_submit_context: None,
                    });
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
                    return Ok(ResolvedRunTarget {
                        desktop_open_path: detect_desktop_open_path_for_installed_capsule(
                            &installed_capsule,
                        ),
                        path: installed_capsule.clone(),
                        agent_local_root: None,
                        export_request: resolve_cli_export_request(
                            export_invocation,
                            &scoped_ref,
                            &installed_capsule,
                        )?,
                        provider_workspace: None,
                        transient_workspace_root: None,
                        community_submit_context: None,
                    });
                }
                return Err(error);
            }
        }
    } else if let Some(installed_capsule) = installed_capsule {
        debug!(
            capsule = %installed_capsule.display(),
            "Using installed capsule"
        );
        return Ok(ResolvedRunTarget {
            desktop_open_path: detect_desktop_open_path_for_installed_capsule(&installed_capsule),
            path: installed_capsule.clone(),
            agent_local_root: None,
            export_request: resolve_cli_export_request(
                export_invocation,
                &scoped_ref,
                &installed_capsule,
            )?,
            provider_workspace: None,
            transient_workspace_root: None,
            community_submit_context: None,
        });
    }

    let json_mode = reporter.is_json();
    ensure_run_auto_install_allowed(
        yes,
        json_mode,
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
    )?;

    let effective_registry = registry.unwrap_or(DEFAULT_RUN_REGISTRY_URL);
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
        let approved = prompt_install_confirmation(&detail, &installable_version)?;
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
            && can_prompt_interactively(
                std::io::stdin().is_terminal(),
                std::io::stderr().is_terminal(),
            ),
    )
    .await?;
    let desktop_open_path = launchable_desktop_open_path(&install_result);
    let install_path = install_result.path.clone();
    Ok(ResolvedRunTarget {
        path: install_path.clone(),
        agent_local_root: None,
        desktop_open_path,
        export_request: resolve_cli_export_request(export_invocation, &scoped_ref, &install_path)?,
        provider_workspace: None,
        transient_workspace_root: None,
        community_submit_context: None,
    })
}

/// Whether an `effective_toml` should overwrite the materialized repo's own
/// `capsule.toml`.
///
/// Invariant (P3-B.5 — consent/policy determinism): a community/catalog
/// snapshot that was selected **silently** (the non-interactive `-y`
/// single-candidate auto-pick) MUST NOT override a repo that ships its own
/// manifest. The materialized source is the source of truth; overriding it
/// would make the executed manifest — and therefore the consent policy the
/// gate computes and the user is asked to approve — a stale catalog snapshot
/// (e.g. hello-capsule@0.2.0 / api.openai.com) instead of what the repo
/// actually declares (0.3.0 / loopback). Explicit selections (`-T`, or an
/// interactive choice) are honoured even when the repo ships a manifest,
/// because the user chose them on purpose.
fn should_apply_effective_toml(community_auto_selected: bool, repo_ships_manifest: bool) -> bool {
    if community_auto_selected && repo_ships_manifest {
        return false;
    }
    true
}

fn is_import_preview_run_target(path: &Path, ato_home: &Path) -> bool {
    let has_import_marker = std::env::var_os("ATO_IMPORT_PROBE_ID").is_some()
        || std::env::var_os("ATO_IMPORT_SESSION_ID").is_some();
    has_import_marker && path.starts_with(ato_home.join("tmp").join("import"))
}

fn relocate_github_run_checkout(checkout_root: &Path) -> Result<PathBuf> {
    let transient_root = github_run_checkout_root()?;

    let checkout_name = checkout_root
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("checkout");
    // Append a process-unique suffix so two concurrent `ato run`
    // invocations against the same GitHub source don't race on the
    // shared `gh-run/<checkout_name>` destination. Previously both
    // runs would (a) remove_dir_all the same destination and (b)
    // rename their own checkouts onto it — the second remove_dir_all
    // could wipe the first run's just-renamed tree, and the rename
    // could fail with ENOENT or trample the other run. Each run now
    // owns its own destination subdir; the parent transient_root
    // sweep already covers cleanup at session end.
    let destination = transient_root.join(format!(
        "{checkout_name}-{}-{}",
        std::process::id(),
        rand::thread_rng().r#gen::<u32>()
    ));
    if destination.exists() {
        std::fs::remove_dir_all(&destination).with_context(|| {
            format!(
                "Failed to clear existing transient GitHub run checkout: {}",
                destination.display()
            )
        })?;
    }
    std::fs::rename(checkout_root, &destination).with_context(|| {
        format!(
            "Failed to relocate GitHub run checkout {} -> {}",
            checkout_root.display(),
            destination.display()
        )
    })?;
    write_github_run_checkout_owner_marker(&destination)?;
    Ok(destination)
}

fn resolve_cli_export_request(
    export_invocation: bool,
    scoped_ref: &install::ScopedCapsuleRef,
    capsule_path: &Path,
) -> Result<Option<ResolvedCliExportRequest>> {
    if !export_invocation {
        return Ok(None);
    }

    let manifest = load_export_manifest(capsule_path).with_context(|| {
        format!(
            "Failed to load manifest for exported CLI '{}'.",
            scoped_ref.scoped_id
        )
    })?;
    let export_name = scoped_ref.slug.trim();
    let export = manifest
        .exports
        .as_ref()
        .and_then(|exports| exports.cli.get(export_name))
        .with_context(|| {
            format!(
                "Capsule '{}' does not export CLI tool '{}'.",
                scoped_ref.scoped_id, export_name
            )
        })?;

    let target_label = export.target.trim();
    let target = manifest
        .targets
        .as_ref()
        .and_then(|targets| targets.named_target(target_label))
        .with_context(|| {
            format!(
                "Export '{}.{}' references missing target '{}'.",
                scoped_ref.scoped_id, export_name, target_label
            )
        })?;

    let (runtime, runtime_driver) = split_runtime_driver(&target.runtime);
    if runtime.as_deref() != Some("source") {
        anyhow::bail!(
            "Export '{}.{}' must reference a runtime=source target.",
            scoped_ref.scoped_id,
            export_name
        );
    }

    if runtime_driver.as_deref() != Some("python")
        && !target
            .driver
            .as_deref()
            .map(|driver| driver.eq_ignore_ascii_case("python"))
            .unwrap_or(false)
    {
        anyhow::bail!(
            "Export '{}.{}' must reference a source/python target.",
            scoped_ref.scoped_id,
            export_name
        );
    }

    Ok(Some(ResolvedCliExportRequest {
        scoped_id: scoped_ref.scoped_id.clone(),
        export_name: export_name.to_string(),
        backend_kind: export.kind.trim().to_string(),
        target_label: target_label.to_string(),
        prefix_args: export.args.clone(),
    }))
}

fn split_runtime_driver(runtime: &str) -> (Option<String>, Option<String>) {
    let normalized = runtime.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return (None, None);
    }
    if let Some((base, driver)) = normalized.split_once('/') {
        let base = (!base.trim().is_empty()).then(|| base.trim().to_string());
        let driver = (!driver.trim().is_empty()).then(|| driver.trim().to_string());
        return (base, driver);
    }
    (Some(normalized), None)
}

fn load_export_manifest(capsule_path: &Path) -> Result<capsule::types::CapsuleManifest> {
    if let Some(manifest_path) = runtime_tree::prepare_store_runtime_for_capsule(capsule_path)? {
        let manifest = capsule::manifest::load_manifest_with_validation_mode(
            &manifest_path,
            capsule::types::ValidationMode::Strict,
        )?;
        return Ok(manifest.model);
    }

    let file = std::fs::File::open(capsule_path)
        .with_context(|| format!("Failed to open capsule archive: {}", capsule_path.display()))?;
    let mut archive = tar::Archive::new(file);
    let mut manifest_text = None;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        if path.as_ref() == Path::new("capsule.toml") {
            let mut raw = String::new();
            entry.read_to_string(&mut raw)?;
            manifest_text = Some(raw);
            break;
        }
    }

    let manifest_text = manifest_text.with_context(|| {
        format!(
            "Capsule archive does not contain capsule.toml: {}",
            capsule_path.display()
        )
    })?;

    let manifest = capsule::types::CapsuleManifest::from_toml_with_path(
        &manifest_text,
        Path::new("capsule.toml"),
    )
    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    manifest
        .validate_for_mode(capsule::types::ValidationMode::Strict)
        .map_err(|errors| {
            anyhow::anyhow!(
                errors
                    .into_iter()
                    .map(|error| error.to_string())
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        })?;
    Ok(manifest)
}

fn launchable_desktop_open_path(install_result: &install::InstallResult) -> Option<PathBuf> {
    match install_result.launchable.as_ref() {
        Some(install::LaunchableTarget::DerivedApp { path }) => Some(path.clone()),
        _ => None,
    }
}

fn detect_desktop_open_path_for_installed_capsule(capsule_path: &Path) -> Option<PathBuf> {
    let version = capsule_path.parent()?.file_name()?.to_str()?.to_string();
    let slug = capsule_path
        .parent()?
        .parent()?
        .file_name()?
        .to_str()?
        .to_string();
    let publisher = capsule_path
        .parent()?
        .parent()?
        .parent()?
        .file_name()?
        .to_str()?
        .to_string();
    let scoped_id = format!("{publisher}/{slug}");

    let apps_root = capsule::common::paths::ato_path("apps")
        .ok()?
        .join(&publisher)
        .join(&slug);
    let content_dirs = std::fs::read_dir(apps_root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect::<Vec<_>>();

    let mut derived_dirs = Vec::new();
    for content_dir in content_dirs {
        let entries = std::fs::read_dir(content_dir).ok()?;
        derived_dirs.extend(
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.is_dir()
                        && path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .map(|name| name.starts_with("derived-"))
                            .unwrap_or(false)
                }),
        );
    }
    derived_dirs.sort();
    derived_dirs.reverse();

    for derived_dir in derived_dirs {
        let provenance_path = derived_dir.join("local-derivation.json");
        if !provenance_path.is_file() {
            continue;
        }
        let provenance = std::fs::read_to_string(&provenance_path).ok()?;
        let provenance = serde_json::from_str::<serde_json::Value>(&provenance).ok()?;
        let version_matches = provenance
            .get("version")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            == Some(version.as_str());
        let scoped_matches = provenance
            .get("scoped_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            == Some(scoped_id.as_str());
        if !version_matches || !scoped_matches {
            continue;
        }

        let app_path = std::fs::read_dir(&derived_dir)
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.extension()
                    .map(|ext| ext.to_string_lossy().eq_ignore_ascii_case("app"))
                    .unwrap_or(false)
            });
        if app_path.is_some() {
            return app_path;
        }
    }

    None
}

pub(crate) fn agent_local_root_for_path(path: &Path) -> Option<PathBuf> {
    if path
        .extension()
        .map(|ext| ext.eq_ignore_ascii_case("capsule"))
        .unwrap_or(false)
    {
        return None;
    }

    if path.is_dir() {
        return Some(path.to_path_buf());
    }

    if path.file_name().and_then(|name| name.to_str()) == Some("capsule.toml") {
        return path.parent().map(PathBuf::from);
    }

    if path.file_name().and_then(|name| name.to_str()) == Some(CAPSULE_LOCK_FILE_NAME)
        || path.file_name().and_then(|name| name.to_str())
            == Some(DEPRECATED_CAPSULE_LOCK_ALIAS_FILE_NAME)
    {
        return path.parent().map(PathBuf::from);
    }

    None
}

pub(crate) fn ensure_local_manifest_ready_for_run(
    resolved: &ResolvedRunTarget,
    assume_yes: bool,
    reporter: Arc<reporters::CliReporter>,
) -> Result<LocalRunManifestPreparationOutcome> {
    let Some(local_root) = resolved.agent_local_root.as_ref() else {
        return Ok(LocalRunManifestPreparationOutcome::Ready);
    };

    match resolve_authoritative_input(local_root, ResolveInputOptions::default()) {
        Ok(ResolvedInput::CanonicalLock { .. }) => {
            return Ok(LocalRunManifestPreparationOutcome::Ready);
        }
        Ok(ResolvedInput::CompatibilityProject { .. }) => {
            return Ok(LocalRunManifestPreparationOutcome::Ready);
        }
        Ok(ResolvedInput::SourceOnly { .. }) => {
            return Ok(LocalRunManifestPreparationOutcome::Ready);
        }
        Err(error)
            if error
                .to_string()
                .contains("is not an authoritative command-entry input") =>
        {
            return Err(error.into());
        }
        Err(_) => {}
    }

    let manifest_path = local_root.join("capsule.toml");

    let status = inspect_local_run_manifest(&manifest_path)?;
    if matches!(status, LocalRunManifestStatus::Valid) {
        return Ok(LocalRunManifestPreparationOutcome::Ready);
    }

    let reason = match &status {
        LocalRunManifestStatus::Missing => format!(
            "No valid `capsule.toml` was found for `ato run` at {}.",
            manifest_path.display()
        ),
        LocalRunManifestStatus::Invalid { error } => format!(
            "The existing `capsule.toml` is not valid for `ato run`: {}",
            error
        ),
        LocalRunManifestStatus::Valid => return Ok(LocalRunManifestPreparationOutcome::Ready),
    };

    let can_prompt = can_prompt_interactively(
        std::io::stdin().is_terminal(),
        std::io::stderr().is_terminal(),
    );

    if reporter.is_json() && !assume_yes {
        anyhow::bail!(
            "{} Non-interactive mode requires -y/--yes to auto-generate `capsule.toml`, or create/repair it manually before rerunning `ato run`.",
            reason
        );
    }

    if !assume_yes && !can_prompt {
        anyhow::bail!(
            "{} Non-interactive mode requires -y/--yes to auto-generate `capsule.toml`, or create/repair it manually before rerunning `ato run`.",
            reason
        );
    }

    let use_progressive_ui =
        !reporter.is_json() && crate::progressive_ui::can_use_progressive_ui(false);
    let action = if assume_yes {
        RunManifestRecoveryChoice::Generate
    } else {
        if use_progressive_ui {
            crate::progressive_ui::show_note(
                "Run Manifest Required",
                format!(
                    "{}\n\nOptions:\n1. Generate an inferred `capsule.toml` now\n2. Create a minimal starter `capsule.toml` and stop\n3. Abort",
                    reason
                ),
            )?;
        } else {
            futures::executor::block_on(reporter.warn(reason.clone()))?;
            futures::executor::block_on(reporter.notify(
                "Options:\n1. Generate an inferred `capsule.toml` now\n2. Create a minimal starter `capsule.toml` and stop\n3. Abort"
                    .to_string(),
            ))?;
        }
        prompt_run_manifest_recovery_choice(use_progressive_ui)?
    };

    let backup_path = if matches!(status, LocalRunManifestStatus::Invalid { .. }) {
        let backup_path = backup_invalid_manifest(&manifest_path)?;
        if reporter.is_json() {
            futures::executor::block_on(reporter.warn(format!(
                "Existing manifest is invalid. Backed up to {}.",
                backup_path.display()
            )))?;
        } else if use_progressive_ui {
            crate::progressive_ui::show_note(
                "Manifest Backup",
                format!(
                    "Existing manifest is invalid.\nBacked up to:\n{}",
                    crate::progressive_ui::format_path_for_note(&backup_path)
                ),
            )?;
        } else {
            futures::executor::block_on(reporter.notify(format!(
                "Backed up invalid manifest to {}",
                backup_path.display()
            )))?;
        }
        Some(backup_path)
    } else {
        None
    };

    match action {
        RunManifestRecoveryChoice::Generate => {
            if assume_yes {
                futures::executor::block_on(reporter.notify(format!(
                    "{} Auto-generating `capsule.toml` because -y/--yes was provided.",
                    reason
                )))?;
            } else if backup_path.is_some() {
                futures::executor::block_on(
                    reporter.notify("Attempting regeneration with inferred defaults.".to_string()),
                )?;
            }
            crate::project::init::write_legacy_detected_manifest(
                Some(local_root.clone()),
                reporter,
            )?;
            Ok(LocalRunManifestPreparationOutcome::Ready)
        }
        RunManifestRecoveryChoice::Create => {
            crate::project::init::write_manual_manifest_stub(Some(local_root.clone()), reporter)?;
            Ok(LocalRunManifestPreparationOutcome::CreatedManualManifest)
        }
        RunManifestRecoveryChoice::Abort => {
            anyhow::bail!("Create or repair `capsule.toml` manually, then rerun `ato run`.");
        }
    }
}

pub(crate) fn inspect_local_run_manifest(manifest_path: &Path) -> Result<LocalRunManifestStatus> {
    if !manifest_path.exists() {
        return Ok(LocalRunManifestStatus::Missing);
    }

    match capsule::manifest::load_manifest_with_validation_mode(
        manifest_path,
        capsule::types::ValidationMode::Strict,
    ) {
        Ok(_) => Ok(LocalRunManifestStatus::Valid),
        Err(error) => Ok(LocalRunManifestStatus::Invalid {
            error: error.to_string(),
        }),
    }
}

fn prompt_run_manifest_recovery_choice(
    use_progressive_ui: bool,
) -> Result<RunManifestRecoveryChoice> {
    loop {
        if use_progressive_ui {
            crate::progressive_ui::show_step(
                "Select 1 to generate, 2 to create a starter manifest, or 3 to abort.",
            )?;
        } else {
            eprintln!("Select 1 to generate, 2 to create a starter manifest, or 3 to abort.");
        }

        eprint!("Choice [1/2/3] (default: 1): ");
        io::stderr()
            .flush()
            .context("failed to flush manifest recovery prompt")?;

        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .context("failed to read manifest recovery choice")?;

        match input.trim().to_ascii_lowercase().as_str() {
            "" | "1" | "g" | "generate" => return Ok(RunManifestRecoveryChoice::Generate),
            "2" | "c" | "create" => return Ok(RunManifestRecoveryChoice::Create),
            "3" | "a" | "abort" => return Ok(RunManifestRecoveryChoice::Abort),
            _ => {
                if use_progressive_ui {
                    crate::progressive_ui::show_warning("Please enter 1, 2, or 3.")?;
                } else {
                    eprintln!("Please enter 1, 2, or 3.");
                }
            }
        }
    }
}

pub(crate) fn backup_invalid_manifest(manifest_path: &Path) -> Result<PathBuf> {
    let parent = manifest_path
        .parent()
        .with_context(|| format!("manifest has no parent: {}", manifest_path.display()))?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let backup_dir =
        capsule::common::paths::workspace_tmp_dir(parent).join("run-invalid-manifests");
    std::fs::create_dir_all(&backup_dir).with_context(|| {
        format!(
            "failed to create invalid manifest backup directory {}",
            backup_dir.display()
        )
    })?;
    let backup_path = backup_dir.join(format!("capsule.toml.invalid.{timestamp}"));
    std::fs::rename(manifest_path, &backup_path).with_context(|| {
        format!(
            "failed to move invalid manifest {} -> {}",
            manifest_path.display(),
            backup_path.display()
        )
    })?;
    Ok(backup_path)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn install_github_repository(
    repository: &str,
    output_dir: Option<PathBuf>,
    yes: bool,
    projection_preference: install::ProjectionPreference,
    json: bool,
    can_prompt: bool,
    keep_failed_artifacts: bool,
    auto_fix_mode: Option<GitHubAutoFixMode>,
    explicit_commit: Option<String>,
) -> Result<install::InstallResult> {
    const MAX_GITHUB_DRAFT_RETRIES: u8 = 3;
    sweep_stale_github_run_checkouts_best_effort();
    ensure_supported_github_auto_fix_mode(auto_fix_mode)?;
    let invocation_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let use_progressive_ui = !json && crate::progressive_ui::can_use_progressive_ui(false);

    let prepare_spinner = if use_progressive_ui {
        Some(crate::progressive_ui::start_logo_spinner(
            "Fetching and preparing GitHub source...",
        ))
    } else {
        None
    };

    let preview_preparation = match crate::preview::prepare_github_preview_session(
        repository,
        &invocation_dir,
        explicit_commit.as_deref(),
    )
    .await
    {
        Ok(result) => {
            if let Some(progress) = prepare_spinner {
                progress.stop("GitHub source prepared.");
            }
            result
        }
        Err(error) => {
            if let Some(progress) = prepare_spinner {
                progress.stop("GitHub source preparation failed.");
            }
            return Err(error);
        }
    };
    if let Some(warning) = preview_preparation.draft_fetch_warning.as_deref()
        && !json
    {
        eprintln!("⚠️  {warning}");
    }
    if let Some(warning) = preview_preparation.session_persist_warning.as_deref()
        && !json
    {
        eprintln!("⚠️  {warning}");
    }
    let mut checkout = preview_preparation.checkout;
    let mut install_draft = preview_preparation.install_draft;
    let mut preview_session = preview_preparation.preview_session;
    if let Some(draft) = install_draft.as_mut() {
        // Normalize the store-generated draft BEFORE the first build so that
        // schema_version=0.3 drafts that still use the legacy `entrypoint`/`cmd`
        // fields inside `[targets.*]` are migrated to `run` up-front.  Previously
        // this call only happened inside the retry loop (line ~1914), which meant
        // the very first build attempt always saw un-normalized TOML and hit
        // `reject_v03_legacy_fields`, making every store-draft install fail on the
        // first try.
        *draft = draft.normalize_preview_toml_for_checkout(&checkout.checkout_dir)?;
        apply_github_auto_fix_to_draft(draft, &checkout.checkout_dir, auto_fix_mode, false, json)?;
        // Unconditionally correct port when the run script hard-codes --port <n>
        if let Some(toml) = draft.preview_toml.as_deref() {
            let corrected = super::github_inference::correct_port_from_run_script(
                toml,
                &checkout.checkout_dir,
            )?;
            if corrected != toml {
                draft.preview_toml = Some(corrected);
            }
        }
        preview_session.update_from_install_draft(draft);
    }
    if install_draft.is_some()
        && let Err(error) = show_github_draft_preview(&preview_session, json)
    {
        maybe_keep_failed_github_checkout(&mut checkout, keep_failed_artifacts, json);
        return Err(error);
    }
    let inference_attempt = if let Some(draft) = install_draft.as_ref() {
        crate::inference_feedback::submit_attempt(repository, draft)
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    preview_session.set_inference_attempt_id(
        inference_attempt
            .as_ref()
            .map(|value| value.attempt_id.as_str()),
    );
    if let Some(warning) = crate::preview::persist_session_with_warning(&preview_session)
        && !json
    {
        eprintln!("⚠️  {warning}");
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
            eprintln!(
                "Resolved {}@{} -> {}",
                draft.repo_ref, draft.resolved_ref.ref_name, draft.resolved_ref.sha
            );
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
    if let Some(draft) = install_draft.as_ref()
        && crate::preview::draft_requires_manual_review(draft)
    {
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
        if let Some(warning) = crate::preview::persist_session_with_warning(&preview_session)
            && !json
        {
            eprintln!("⚠️  {warning}");
        }
        maybe_keep_failed_github_checkout(&mut checkout, keep_failed_artifacts, json);
        return Err(build_github_manual_intervention_error(
            &preview_session.manifest_path,
            repository,
            draft,
            &crate::preview::github_draft_manual_review_reason(draft),
        )?);
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
    maybe_copy_env_example_with_prompts(&checkout.checkout_dir, json);
    let mut latest_install_draft = install_draft.clone();
    let build_result = match build_github_repository_checkout(
        checkout.checkout_dir.clone(),
        json,
        latest_install_draft
            .as_ref()
            .and_then(|draft| draft.preview_toml.clone()),
        keep_failed_artifacts,
        true,
    )
    .await
    {
        Ok(result) => result,
        Err(error) => {
            let mut last_error = error;
            if let Some(draft) = install_draft.as_ref()
                && github_build_error_requires_manual_intervention(&last_error)
            {
                maybe_keep_failed_github_checkout(&mut checkout, keep_failed_artifacts, json);
                return Err(build_github_manual_intervention_error(
                    &preview_session.manifest_path,
                    repository,
                    draft,
                    &github_build_error_manual_review_reason(&last_error),
                )?);
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
                    && !json
                {
                    eprintln!("⚠️  {warning}");
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
                        && !json
                    {
                        eprintln!("⚠️  {warning}");
                    }
                    maybe_keep_failed_github_checkout(&mut checkout, keep_failed_artifacts, json);
                    return Err(build_github_manual_intervention_error(
                        &preview_session.manifest_path,
                        repository,
                        draft,
                        &report.message,
                    )?);
                }

                let mut recovered_build = None;
                if should_retry_github_auto_fix_port(auto_fix_mode, report) {
                    let mut port_retry_draft = latest_install_draft
                        .clone()
                        .unwrap_or_else(|| draft.clone());
                    apply_github_auto_fix_to_draft(
                        &mut port_retry_draft,
                        &checkout.checkout_dir,
                        auto_fix_mode,
                        true,
                        json,
                    )?;
                    latest_install_draft = Some(port_retry_draft.clone());
                    preview_session.update_from_install_draft(&port_retry_draft);
                    if let Some(warning) =
                        crate::preview::persist_session_with_warning(&preview_session)
                        && !json
                    {
                        eprintln!("⚠️  {warning}");
                    }

                    if !json {
                        eprintln!("🔁 Retrying build with reassigned Ato web port...");
                    }

                    recovered_build = match build_github_repository_checkout(
                        checkout.checkout_dir.clone(),
                        json,
                        port_retry_draft.preview_toml.clone(),
                        keep_failed_artifacts,
                        true,
                    )
                    .await
                    {
                        Ok(result) => Some(result),
                        Err(retry_error) => {
                            last_error = retry_error;
                            None
                        }
                    };
                }

                let mut current_draft = latest_install_draft
                    .clone()
                    .unwrap_or_else(|| draft.clone());
                let mut current_report = report.clone();
                if let Some(retry_report) = last_error
                    .downcast_ref::<crate::commands::build::InferredManifestSmokeFailure>()
                    .map(|failure| failure.report.clone())
                {
                    current_report = retry_report;
                }
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
                    let mut next_draft = next_draft;
                    apply_github_auto_fix_to_draft(
                        &mut next_draft,
                        &checkout.checkout_dir,
                        auto_fix_mode,
                        false,
                        json,
                    )?;
                    let next_toml = next_draft.preview_toml.clone().unwrap_or_default();
                    let draft_changed = next_toml.trim() != previous_toml.trim();
                    latest_install_draft = Some(next_draft.clone());
                    preview_session.record_retry_draft(&next_draft, retry_ordinal);
                    if let Some(warning) =
                        crate::preview::persist_session_with_warning(&preview_session)
                        && !json
                    {
                        eprintln!("⚠️  {warning}");
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

                    match build_github_repository_checkout(
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
                                && !json
                            {
                                eprintln!("⚠️  {warning}");
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
                        && !json
                    {
                        eprintln!("⚠️  {warning}");
                    }
                    maybe_keep_failed_github_checkout(&mut checkout, keep_failed_artifacts, json);
                    return Err(build_github_manual_intervention_error(
                        &preview_session.manifest_path,
                        repository,
                        latest_install_draft.as_ref().unwrap_or(draft),
                        &current_report.message,
                    )?);
                } else if can_prompt {
                    let draft_for_manual_fix = latest_install_draft.as_ref().unwrap_or(draft);
                    let manual_manifest_path = preview_session.manifest_path.clone();
                    if let Some(recovered) = retry_github_build_after_manual_fix(
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
                        maybe_keep_failed_github_checkout(
                            &mut checkout,
                            keep_failed_artifacts,
                            json,
                        );
                        return Err(last_error);
                    }
                } else {
                    maybe_keep_failed_github_checkout(&mut checkout, keep_failed_artifacts, json);
                    return Err(last_error);
                }
            } else {
                maybe_keep_failed_github_checkout(&mut checkout, keep_failed_artifacts, json);
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
        maybe_keep_failed_github_checkout(&mut checkout, keep_failed_artifacts, json);
    } else {
        cleanup_successful_github_checkout_best_effort(&checkout.checkout_dir);
    }

    result
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_run_command(
    path: PathBuf,
    target: Option<String>,
    args: Vec<String>,
    watch: bool,
    background: bool,
    nacelle: Option<PathBuf>,
    registry: Option<String>,
    enforcement: EnforcementMode,
    sandbox_mode: bool,
    dangerously_skip_permissions: bool,
    compatibility_fallback: Option<String>,
    provider_toolchain: ProviderToolchain,
    use_existing_toml: Option<String>,
    explicit_commit: Option<String>,
    assume_yes: bool,
    verbose: bool,
    agent_mode: crate::RunAgentMode,
    agent_local_root: Option<PathBuf>,
    keep_failed_artifacts: bool,
    auto_fix_mode: Option<GitHubAutoFixMode>,
    allow_unverified: bool,
    read: Vec<String>,
    write: Vec<String>,
    read_write: Vec<String>,
    cwd: Option<PathBuf>,
    state: Vec<String>,
    managed_state_root: Option<PathBuf>,
    inject: Vec<String>,
    build_policy: crate::application::build_materialization::BuildPolicy,
    cache_strategy_arg: crate::cli::shared::CacheStrategyArg,
    plan_only: bool,
    strict_realization: bool,
    install_lifecycle_context: Option<crate::cli::commands::run::InstallLifecycleContext>,
    capsule_launch_inputs: Vec<capsule::installed_state::LaunchConditionInput>,
    pinned_revision_output_dir: Option<std::path::PathBuf>,
    reporter: std::sync::Arc<reporters::CliReporter>,
) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(commands::run::execute(commands::run::RunArgs {
        target: path,
        target_label: target,
        args,
        watch,
        background,
        nacelle,
        registry,
        enforcement: enforcement.as_str().to_string(),
        sandbox_mode,
        dangerously_skip_permissions,
        compatibility_fallback,
        provider_toolchain_requested: provider_toolchain,
        use_existing_toml,
        explicit_commit,
        assume_yes,
        verbose,
        agent_mode,
        agent_local_root,
        keep_failed_artifacts,
        auto_fix_mode,
        allow_unverified,
        read_grants: read,
        write_grants: write,
        read_write_grants: read_write,
        caller_cwd: std::env::current_dir().or_else(|_| {
            dirs::home_dir().ok_or_else(|| {
                anyhow::anyhow!("failed to resolve current working directory and $HOME is unset")
            })
        })?,
        effective_cwd: cwd,
        export_request: None,
        state_bindings: state,
        managed_state_root,
        inject_bindings: inject,
        build_policy,
        cache_strategy: cache_strategy_arg.resolve(),
        reporter,
        preview_mode: false,
        plan_only,
        strict_realization,
        install_lifecycle_context,
        capsule_launch_inputs,
        pinned_revision_output_dir,
    }))
}

fn apply_github_auto_fix_to_draft(
    draft: &mut install::GitHubInstallDraftResponse,
    checkout_dir: &Path,
    auto_fix_mode: Option<GitHubAutoFixMode>,
    force_port_reassignment: bool,
    json: bool,
) -> Result<()> {
    if !auto_fix_mode
        .map(GitHubAutoFixMode::fixes_generated_toml)
        .unwrap_or(false)
    {
        return Ok(());
    }

    let Some(preview_toml) = draft.preview_toml.as_deref() else {
        return Ok(());
    };

    let mut fixed = if force_port_reassignment {
        super::github_inference::reassign_github_install_preview_toml_port(preview_toml)?
    } else {
        super::github_inference::auto_fix_github_install_preview_toml(preview_toml)?
    };
    let mut python_lockfiles_repaired = false;
    if matches!(auto_fix_mode, Some(GitHubAutoFixMode::All)) {
        let repaired =
            auto_fix_github_python_lockfiles_for_preview_toml(&fixed, checkout_dir, json)?;
        python_lockfiles_repaired = repaired.repaired;
        fixed = repaired.preview_toml;
    }

    if fixed.trim() != preview_toml.trim() {
        if !json {
            if force_port_reassignment {
                eprintln!("ℹ️  Reassigned generated GitHub draft to an available Ato web port.");
            } else {
                eprintln!("ℹ️  Auto-fixed generated GitHub draft TOML before build/install.");
            }
        }
        draft.preview_toml = Some(fixed);
    } else if python_lockfiles_repaired && !json {
        eprintln!("ℹ️  Generated Python uv.lock in the GitHub checkout before build/install.");
    }

    Ok(())
}

#[derive(Debug, Clone)]
pub(super) struct GitHubPythonLockAutoFixResult {
    pub(super) preview_toml: String,
    pub(super) repaired: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitHubPythonLockRepairKind {
    Pyproject,
    Requirements,
}

#[derive(Debug, Clone)]
pub(super) struct GitHubPythonLockRepairTarget {
    label: String,
    working_dir: PathBuf,
    kind: GitHubPythonLockRepairKind,
}

pub(super) fn auto_fix_github_python_lockfiles_for_preview_toml(
    preview_toml: &str,
    checkout_dir: &Path,
    json: bool,
) -> Result<GitHubPythonLockAutoFixResult> {
    let targets = github_python_lock_repair_targets(preview_toml, checkout_dir)?;
    if targets.is_empty() {
        return Ok(GitHubPythonLockAutoFixResult {
            preview_toml: preview_toml.to_string(),
            repaired: false,
        });
    }

    let mut repaired = false;
    for target in targets {
        run_github_python_lock_repair(&target, json)?;
        repaired = true;
    }

    let normalized =
        super::github_inference::normalize_github_install_preview_toml(checkout_dir, preview_toml)?;
    Ok(GitHubPythonLockAutoFixResult {
        preview_toml: normalized,
        repaired,
    })
}

pub(super) fn github_python_lock_repair_targets(
    preview_toml: &str,
    checkout_dir: &Path,
) -> Result<Vec<GitHubPythonLockRepairTarget>> {
    let Ok(parsed) = toml::from_str::<toml::Value>(preview_toml) else {
        return Ok(Vec::new());
    };
    let Some(root_table) = parsed.as_table() else {
        return Ok(Vec::new());
    };

    let mut targets = Vec::new();
    if let Some(named_targets) = root_table.get("targets").and_then(toml::Value::as_table) {
        for (label, value) in named_targets {
            let Some(target_table) = value.as_table() else {
                continue;
            };
            if !github_preview_target_is_python(target_table) {
                continue;
            }
            let working_dir = target_table
                .get("working_dir")
                .and_then(toml::Value::as_str)
                .or_else(|| root_table.get("working_dir").and_then(toml::Value::as_str));
            if let Some(target) =
                github_python_lock_repair_target(label, working_dir, checkout_dir)?
            {
                targets.push(target);
            }
        }
        return Ok(targets);
    }

    if github_preview_target_is_python(root_table)
        && let Some(target) = github_python_lock_repair_target(
            root_table
                .get("default_target")
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("app"),
            root_table.get("working_dir").and_then(toml::Value::as_str),
            checkout_dir,
        )?
    {
        targets.push(target);
    }

    Ok(targets)
}

fn github_preview_target_is_python(table: &toml::value::Table) -> bool {
    let driver = table
        .get("driver")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase);
    if matches!(driver.as_deref(), Some("python")) {
        return true;
    }

    table
        .get("runtime")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
        .map(|runtime| runtime == "source/python" || runtime == "web/python")
        .unwrap_or(false)
}

fn github_python_lock_repair_target(
    label: &str,
    working_dir: Option<&str>,
    checkout_dir: &Path,
) -> Result<Option<GitHubPythonLockRepairTarget>> {
    let working_dir = resolve_github_auto_fix_working_dir(checkout_dir, working_dir)?;
    let uv_lock = working_dir.join("uv.lock");
    if uv_lock.exists() {
        return Ok(None);
    }

    let pyproject = working_dir.join("pyproject.toml");
    if pyproject.exists() {
        return Ok(Some(GitHubPythonLockRepairTarget {
            label: label.to_string(),
            working_dir,
            kind: GitHubPythonLockRepairKind::Pyproject,
        }));
    }

    let requirements = working_dir.join("requirements.txt");
    if requirements.exists() {
        return Ok(Some(GitHubPythonLockRepairTarget {
            label: label.to_string(),
            working_dir,
            kind: GitHubPythonLockRepairKind::Requirements,
        }));
    }

    Ok(None)
}

pub(super) fn resolve_github_auto_fix_working_dir(
    checkout_dir: &Path,
    working_dir: Option<&str>,
) -> Result<PathBuf> {
    let canonical_checkout = checkout_dir.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize GitHub checkout {}",
            checkout_dir.display()
        )
    })?;
    let Some(raw) = working_dir.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(canonical_checkout);
    };
    let relative = Path::new(raw);
    // RootDir/Prefix are checked explicitly: on Windows `/srv` is rooted but
    // not `is_absolute()`, and `C:foo` is drive-relative — both would resolve
    // outside the checkout when joined.
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::RootDir | Component::Prefix(_)))
    {
        return Err(github_python_lock_auto_fix_error(format!(
            "GitHub auto-fix refuses absolute working_dir '{}'",
            raw
        ))
        .into());
    }
    if relative
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(github_python_lock_auto_fix_error(format!(
            "GitHub auto-fix refuses working_dir '{}' because it contains '..'",
            raw
        ))
        .into());
    }

    let candidate = checkout_dir.join(relative);
    let canonical_candidate = candidate.canonicalize().with_context(|| {
        format!(
            "failed to canonicalize GitHub auto-fix working_dir {}",
            candidate.display()
        )
    })?;
    if !canonical_candidate.starts_with(&canonical_checkout) {
        return Err(github_python_lock_auto_fix_error(format!(
            "GitHub auto-fix refuses working_dir '{}' because it resolves outside checkout {}",
            raw,
            canonical_checkout.display()
        ))
        .into());
    }
    Ok(canonical_candidate)
}

fn github_python_lock_auto_fix_error(message: String) -> AtoExecutionError {
    crate::adapters::runtime::provisioning::python_requirements_lock_missing(message)
}

fn run_github_python_lock_repair(target: &GitHubPythonLockRepairTarget, json: bool) -> Result<()> {
    let (args, description) = match target.kind {
        GitHubPythonLockRepairKind::Pyproject => (vec!["lock"], "uv lock"),
        GitHubPythonLockRepairKind::Requirements => {
            // Existing Ato source/python packaging keys on the filename `uv.lock`.
            // For requirements-only projects this file is intentionally a
            // pip-compile requirements lock, not uv's project-mode TOML lockfile.
            (
                vec!["pip", "compile", "requirements.txt", "-o", "uv.lock"],
                "uv pip compile requirements.txt -o uv.lock",
            )
        }
    };

    if !json {
        eprintln!(
            "ℹ️  Generating Python lockfile for target '{}' with `{}`...",
            target.label, description
        );
    }

    let output = Command::new("uv")
        .args(&args)
        .current_dir(&target.working_dir)
        .output()
        .map_err(|error| {
            github_python_lock_auto_fix_error(format!(
                "failed to run `{}` in {}: {}",
                description,
                target.working_dir.display(),
                error
            ))
        })?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(github_python_lock_auto_fix_error(format!(
        "failed to generate uv.lock for GitHub target '{}' in {} with `{}` (status {}): {}",
        target.label,
        target.working_dir.display(),
        description,
        output.status,
        if stderr.is_empty() {
            "<no stderr>"
        } else {
            stderr.as_str()
        }
    ))
    .into())
}

fn should_retry_github_auto_fix_port(
    auto_fix_mode: Option<GitHubAutoFixMode>,
    report: &capsule::smoke::SmokeFailureReport,
) -> bool {
    auto_fix_mode
        .map(GitHubAutoFixMode::fixes_generated_toml)
        .unwrap_or(false)
        && matches!(report.class, SmokeFailureClass::RequiredPortUnavailable)
}

fn ensure_supported_github_auto_fix_mode(auto_fix_mode: Option<GitHubAutoFixMode>) -> Result<()> {
    if matches!(auto_fix_mode, Some(GitHubAutoFixMode::Src)) {
        anyhow::bail!(
            "--auto-fix:src is not implemented yet. Use --auto-fix:toml or --auto-fix:all."
        );
    }
    Ok(())
}

/// D2: Copy `.env.example` / `.env.template` / `.env.sample` → `.env` if `.env` is absent.
/// Also checks one level of subdirectories that contain a `package.json` (monorepo apps/).
fn maybe_copy_env_example(dir: &Path, json: bool) {
    const EXAMPLE_NAMES: &[&str] = &[".env.example", ".env.template", ".env.sample"];

    copy_env_example_in_dir(dir, json, false);

    // Check one level of subdirs for monorepo layouts (apps/, packages/, etc.)
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let sub = entry.path();
            if sub.is_dir()
                && sub.join("package.json").exists()
                && !sub.join(".env").exists()
                && EXAMPLE_NAMES.iter().any(|n| sub.join(n).exists())
            {
                copy_env_example_in_dir(&sub, json, false);
            }
        }
    }
}

/// Like `maybe_copy_env_example` but also detects secret-looking keys and prompts for them.
fn maybe_copy_env_example_with_prompts(dir: &Path, json: bool) {
    const EXAMPLE_NAMES: &[&str] = &[".env.example", ".env.template", ".env.sample"];

    copy_env_example_in_dir(dir, json, true);

    // Check one level of subdirs for monorepo layouts (apps/, packages/, etc.)
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let sub = entry.path();
            if sub.is_dir()
                && sub.join("package.json").exists()
                && !sub.join(".env").exists()
                && EXAMPLE_NAMES.iter().any(|n| sub.join(n).exists())
            {
                copy_env_example_in_dir(&sub, json, true);
            }
        }
    }
}

fn is_secret_key_name(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    [
        "_KEY",
        "_SECRET",
        "_TOKEN",
        "_PASSWORD",
        "_API_",
        "_AUTH",
        "_CREDENTIAL",
        "_PRIVATE",
    ]
    .iter()
    .any(|pat| upper.contains(pat))
        || upper.ends_with("_API")
        || upper.ends_with("_KEY")
        || upper.ends_with("_SECRET")
        || upper.ends_with("_TOKEN")
        || upper.ends_with("_PASSWORD")
}

fn is_placeholder_value(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() {
        return true;
    }
    let lower = v.to_ascii_lowercase();
    lower.starts_with("your_")
        || lower.starts_with("your-")
        || lower.starts_with("<")
        || lower.starts_with("${")
        || lower == "change_me"
        || lower == "changeme"
        || lower == "replace_me"
        || lower == "replaceme"
        || lower.contains("example")
        || lower.contains("placeholder")
        || lower.contains("todo")
}

/// Parse secret key names from a `.env` content that need values filled in.
fn detect_secret_placeholders(content: &str) -> Vec<String> {
    let mut keys = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(eq_pos) = line.find('=') {
            let key = line[..eq_pos].trim();
            let value = line[eq_pos + 1..]
                .trim()
                .trim_matches('"')
                .trim_matches('\'');
            if !key.is_empty() && is_secret_key_name(key) && is_placeholder_value(value) {
                keys.push(key.to_string());
            }
        }
    }
    keys
}

fn copy_env_example_in_dir(dir: &Path, json: bool, prompt_for_secrets: bool) {
    const EXAMPLE_NAMES: &[&str] = &[".env.example", ".env.template", ".env.sample"];
    if dir.join(".env").exists() {
        return;
    }
    for name in EXAMPLE_NAMES {
        let src = dir.join(name);
        if !src.exists() {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&src) else {
            let _ = std::fs::copy(&src, dir.join(".env"));
            return;
        };

        let secret_keys = if prompt_for_secrets {
            detect_secret_placeholders(&content)
        } else {
            Vec::new()
        };

        let can_prompt = prompt_for_secrets && io::stdin().is_terminal() && !json;

        if can_prompt && !secret_keys.is_empty() {
            // Prompt interactively for each secret key and write enriched .env
            let mut lines_out: Vec<String> = Vec::new();
            let mut prompted: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();

            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with('#') || trimmed.is_empty() {
                    lines_out.push(line.to_string());
                    continue;
                }
                if let Some(eq_pos) = trimmed.find('=') {
                    let key = trimmed[..eq_pos].trim().to_string();
                    let value = trimmed[eq_pos + 1..]
                        .trim()
                        .trim_matches('"')
                        .trim_matches('\'');
                    if secret_keys.contains(&key) && is_placeholder_value(value) {
                        // If the key is already available in the process environment
                        // (e.g. via --env-file), use that value directly without prompting.
                        if let Ok(env_val) = std::env::var(&key) {
                            prompted.insert(key.clone(), env_val.clone());
                            lines_out.push(format!("{}={}", key, env_val));
                            continue;
                        }
                        let prompt = format!("🔑  Enter value for {} (hidden): ", key);
                        match rpassword::prompt_password(&prompt) {
                            Ok(entered) => {
                                prompted.insert(key.clone(), entered.clone());
                                lines_out.push(format!("{}={}", key, entered));
                            }
                            Err(_) => {
                                lines_out.push(line.to_string());
                            }
                        }
                        continue;
                    }
                }
                lines_out.push(line.to_string());
            }

            let out_content = lines_out.join("\n");
            if std::fs::write(dir.join(".env"), out_content).is_ok() && !json {
                if prompted.is_empty() {
                    eprintln!("ℹ️  Copied {} → .env in {}", name, dir.display());
                } else {
                    eprintln!(
                        "ℹ️  Copied {} → .env in {} (filled: {})",
                        name,
                        dir.display(),
                        prompted.keys().cloned().collect::<Vec<_>>().join(", ")
                    );
                }
            }
        } else {
            // Non-interactive or no secrets: copy as-is, then announce any secrets
            if std::fs::copy(&src, dir.join(".env")).is_ok() && !json {
                eprintln!(
                    "ℹ️  Copied {} → .env in {} (edit with your values before running)",
                    name,
                    dir.display()
                );
                if !secret_keys.is_empty() {
                    eprintln!(
                        "🔑  Secret env var(s) required — set before running: {}",
                        secret_keys.join(", ")
                    );
                }
            }
        }
        return;
    }
}

#[cfg(test)]
mod tests {
    // The env-mutating async tests below hold the crate-wide env lock across
    // `.await` so process-global env stays pinned for the whole resolve. They
    // are `#[tokio::test]` (current-thread) + `#[serial_test::serial]`, so the
    // !Send std guard never crosses threads and serial tests cannot contend —
    // the lint's deadlock/move scenarios cannot occur. Matches the same allow
    // in sibling test modules (install/tests.rs, registry/serve/tests.rs).
    #![allow(clippy::await_holding_lock)]

    use super::*;
    use std::io::Cursor;
    use std::sync::Arc;

    // ── P3-B.5: a silent community snapshot must not override a repo's
    //    own materialized manifest (consent/policy determinism). ──

    #[test]
    fn silent_community_snapshot_does_not_override_repo_manifest() {
        // The exact regression: hello-capsule ships its own capsule.toml
        // (0.3.0/loopback), a single community snapshot (0.2.0/openai) was
        // auto-picked by `-y`. The repo manifest must win, so consent is
        // computed from what actually runs.
        assert!(!should_apply_effective_toml(true, true));
    }

    #[test]
    fn silent_community_snapshot_is_used_only_when_repo_has_no_manifest() {
        // Genuine fallback: the repo ships no capsule.toml, so the
        // auto-picked community snapshot is the only manifest available.
        assert!(should_apply_effective_toml(true, false));
    }

    #[test]
    fn explicit_toml_override_is_honoured_even_when_repo_ships_manifest() {
        // -T / interactive selection is community_auto_selected == false:
        // the user chose it on purpose, so it overrides the repo manifest.
        assert!(should_apply_effective_toml(false, true));
        assert!(should_apply_effective_toml(false, false));
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set_path(key: &'static str, value: &Path) -> Self {
            let previous = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }

        fn set_value(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                unsafe {
                    std::env::set_var(self.key, previous);
                }
            } else {
                unsafe {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn relocate_github_run_checkout_uses_ato_home_tmp_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ato_home = temp.path().join(".ato");
        let checkout_root = temp.path().join("checkout");
        std::fs::create_dir_all(&checkout_root).expect("create checkout");
        std::fs::write(
            checkout_root.join("capsule.toml"),
            "schema_version = \"0.3\"\n",
        )
        .expect("write manifest");
        let _ato_home_guard = EnvVarGuard::set_path("ATO_HOME", &ato_home);

        let relocated = relocate_github_run_checkout(&checkout_root).expect("relocate checkout");

        let expected_parent = ato_home.join("tmp").join("gh-run");
        assert_eq!(
            relocated.parent(),
            Some(expected_parent.as_path()),
            "relocated checkout must live under the ATO_HOME gh-run root"
        );
        assert!(
            relocated
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.starts_with("checkout-")),
            "relocated checkout must use a collision-resistant checkout-* suffix: {}",
            relocated.display()
        );
        assert!(relocated.join("capsule.toml").exists());
        assert!(!checkout_root.exists());
        assert!(relocated.join(".ato-owner.json").exists());
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial]
    async fn resolve_run_target_or_install_sweeps_stale_github_run_checkouts_before_download() {
        use filetime::{FileTime, set_file_mtime};

        let temp = tempfile::tempdir().expect("tempdir");
        let ato_home = temp.path().join(".ato");
        let gh_run_root = ato_home.join("tmp").join("gh-run");
        let stale = gh_run_root.join("stale-checkout");
        let fresh = gh_run_root.join("fresh-checkout");
        std::fs::create_dir_all(&stale).expect("create stale checkout");
        std::fs::create_dir_all(&fresh).expect("create fresh checkout");
        set_file_mtime(&stale, FileTime::from_unix_time(1, 0)).expect("age stale checkout");

        let _ato_home_guard = EnvVarGuard::set_path("ATO_HOME", &ato_home);
        let _github_api_guard =
            EnvVarGuard::set_value("ATO_GITHUB_API_BASE_URL", "http://127.0.0.1:9");

        let _error = resolve_run_target_or_install(
            PathBuf::from("github.com/octocat/nope"),
            true,
            ProviderToolchain::Auto,
            None,
            Some("deadbeef".to_string()),
            false,
            None,
            false,
            None,
            Arc::new(crate::adapters::output::reporters::CliReporter::new_run(
                false,
            )),
        )
        .await
        .expect_err("fake GitHub API should fail after startup sweep");
        assert!(
            !stale.exists(),
            "stale gh-run checkout should be swept before download"
        );
        assert!(fresh.exists(), "fresh gh-run checkout should be preserved");
    }

    #[test]
    fn cleanup_successful_github_checkout_best_effort_removes_checkout_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let checkout_dir = temp.path().join("checkout");
        std::fs::create_dir_all(&checkout_dir).expect("create checkout");
        std::fs::write(
            checkout_dir.join("capsule.toml"),
            "schema_version = \"0.3\"\n",
        )
        .expect("write manifest");

        cleanup_successful_github_checkout_best_effort(&checkout_dir);

        assert!(!checkout_dir.exists());
    }

    fn write_installed_capsule(
        home: &Path,
        publisher: &str,
        slug: &str,
        version: &str,
        manifest: &str,
    ) {
        let version_dir = home
            .join(".ato")
            .join("store")
            .join(publisher)
            .join(slug)
            .join(version);
        std::fs::create_dir_all(&version_dir).expect("create version dir");
        let capsule_path = version_dir.join(format!("{slug}.capsule"));
        let file = std::fs::File::create(&capsule_path).expect("create capsule file");
        let mut builder = tar::Builder::new(file);
        let mut header = tar::Header::new_gnu();
        header.set_path("capsule.toml").expect("set capsule path");
        header.set_mode(0o644);
        header.set_size(manifest.len() as u64);
        header.set_mtime(0);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                "capsule.toml",
                Cursor::new(manifest.as_bytes()),
            )
            .expect("append manifest");
        builder.finish().expect("finish capsule");
    }

    async fn resolve_export_target(manifest: &str) -> Result<ResolvedRunTarget> {
        // Crate convention: every test that mutates process-global env vars
        // holds the shared env lock for its whole run.
        let _env_lock = crate::tests::env_lock().lock().expect("env lock");
        let home = tempfile::tempdir().expect("tempdir");
        let _home_guard = EnvVarGuard::set_path("HOME", home.path());
        // Pin ATO_HOME explicitly: it is the first thing nacelle path
        // resolution reads, and on Windows `dirs::home_dir()` consults the
        // Known Folder API — no environment variable can redirect it.
        let _ato_home_guard = EnvVarGuard::set_path("ATO_HOME", &home.path().join(".ato"));
        write_installed_capsule(home.path(), "team", "tool", "1.0.0", manifest);

        resolve_run_target_or_install(
            PathBuf::from("@team/tool"),
            true,
            ProviderToolchain::Auto,
            None,
            None,
            false,
            None,
            false,
            None,
            Arc::new(reporters::CliReporter::new(false)),
        )
        .await
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn resolve_run_target_or_install_resolves_cli_export_from_installed_capsule() {
        let resolved = resolve_export_target(
            r#"
schema_version = "0.3"
name = "tool"
version = "1.0.0"
type = "app"

default_target = "default"

[targets.default]
runtime = "source"
driver = "python"
runtime_version = "3.11"
run_command = "default.py"

[targets.export]
runtime = "source"
driver = "python"
runtime_version = "3.11"
run_command = "python3 tool.py --from-target"
[exports.cli.tool]
kind = "python-tool"
target = "export"
args = ["--from-export"]
"#,
        )
        .await
        .expect("resolve export target");

        let export = resolved.export_request.expect("export request");
        assert_eq!(export.scoped_id, "team/tool");
        assert_eq!(export.export_name, "tool");
        assert_eq!(export.backend_kind, "python-tool");
        assert_eq!(export.target_label, "export");
        assert_eq!(export.prefix_args, vec!["--from-export".to_string()]);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn resolve_run_target_or_install_errors_when_export_missing() {
        let err = resolve_export_target(
            r#"
schema_version = "0.3"
name = "tool"
version = "1.0.0"
type = "app"

runtime = "source/python"
runtime_version = "3.11"
run = "default.py""#,
        )
        .await
        .expect_err("missing export must fail");

        let details = err
            .chain()
            .map(|cause| cause.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            details.contains("does not export CLI tool 'tool'"),
            "unexpected error chain: {details}"
        );
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn resolve_run_target_or_install_errors_when_export_slug_mismatches() {
        let err = resolve_export_target(
            r#"
schema_version = "0.3"
name = "tool"
version = "1.0.0"
type = "app"

runtime = "source/python"
runtime_version = "3.11"
run = "tool.py"
[exports.cli.other-tool]
kind = "python-tool"
target = "app"
"#,
        )
        .await
        .expect_err("slug mismatch must fail");

        let details = err
            .chain()
            .map(|cause| cause.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(details.contains("does not export CLI tool 'tool'"));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn resolve_run_target_or_install_errors_when_export_backend_is_not_python_tool() {
        let err = resolve_export_target(
            r#"
schema_version = "0.3"
name = "tool"
version = "1.0.0"
type = "app"

runtime = "source/python"
runtime_version = "3.11"
run = "tool.py"
[exports.cli.tool]
kind = "node-tool"
target = "export"
"#,
        )
        .await
        .expect_err("unsupported export backend must fail");

        let details = err
            .chain()
            .map(|cause| cause.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(details.contains("expected 'python-tool'"));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn resolve_run_target_or_install_errors_when_export_target_missing() {
        let err = resolve_export_target(
            r#"
schema_version = "0.3"
name = "tool"
version = "1.0.0"
type = "app"

runtime = "source/python"
runtime_version = "3.11"
run = "default.py"
[exports.cli.tool]
kind = "python-tool"
target = "missing"
"#,
        )
        .await
        .expect_err("missing export target must fail");

        let details = err
            .chain()
            .map(|cause| cause.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(details.contains("references missing target 'missing'"));
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn resolve_run_target_or_install_errors_when_export_target_is_not_source_python() {
        let err = resolve_export_target(
            r#"
schema_version = "0.3"
name = "tool"
version = "1.0.0"
type = "app"

runtime = "source/node"
runtime_version = "20"
run = "tool.js"
[exports.cli.tool]
kind = "python-tool"
target = "app"
"#,
        )
        .await
        .expect_err("non-python export target must fail");

        let details = err
            .chain()
            .map(|cause| cause.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(details.contains("must reference a source/python target"));
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn resolve_run_target_rejects_ato_home_paths_with_helpful_error() {
        let ato_home = tempfile::TempDir::new().expect("ato_home");
        let internal_path = ato_home.path().join("store").join("mycapsule");
        std::fs::create_dir_all(&internal_path).expect("create internal path");

        let _guard = EnvVarGuard::set_path("ATO_HOME", ato_home.path());

        let reporter = Arc::new(reporters::CliReporter::new(false));
        let err = resolve_run_target_or_install(
            internal_path.clone(),
            true,
            ProviderToolchain::Auto,
            None,
            None,
            false,
            None,
            false,
            None,
            reporter,
        )
        .await
        .expect_err("path under ato home must be rejected");

        let msg = format!(
            "{}\n{}",
            err,
            err.chain()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("\n")
        );
        assert!(
            msg.contains("internal state directory"),
            "error should mention internal state: {msg}"
        );
        // The hint with cp -r is stored in the AtoExecutionError hint field;
        // verify it via the typed downcast.
        let exe_err = err
            .downcast_ref::<capsule::engine::execution_plan::error::AtoExecutionError>()
            .expect("error should downcast to AtoExecutionError");
        assert!(
            exe_err.hint.as_deref().unwrap_or("").contains("cp -r"),
            "hint should suggest cp workaround: {:?}",
            exe_err.hint
        );
    }

    #[serial_test::serial]
    #[tokio::test]
    async fn resolve_run_target_allows_import_preview_workspace_under_ato_home() {
        let ato_home = tempfile::TempDir::new().expect("ato_home");
        let import_shadow = ato_home
            .path()
            .join("tmp")
            .join("import")
            .join("demo")
            .join("shadow");
        std::fs::create_dir_all(&import_shadow).expect("create import shadow path");
        std::fs::write(
            import_shadow.join("capsule.toml"),
            "schema_version = \"0.3\"\nname = \"demo\"\nversion = \"0.1.0\"\ntype = \"app\"\n",
        )
        .expect("write manifest");

        let _ato_home_guard = EnvVarGuard::set_path("ATO_HOME", ato_home.path());
        let _import_guard = EnvVarGuard::set_value("ATO_IMPORT_PROBE_ID", "probe-demo");

        let reporter = Arc::new(reporters::CliReporter::new(false));
        let resolved = resolve_run_target_or_install(
            import_shadow.clone(),
            true,
            ProviderToolchain::Auto,
            None,
            None,
            false,
            None,
            false,
            None,
            reporter,
        )
        .await
        .expect("import preview workspace under ATO_HOME/tmp/import must be runnable");

        assert_eq!(resolved.path, import_shadow);
    }
}
