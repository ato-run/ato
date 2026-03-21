use std::cmp::Ordering;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::CapsuleReporter;

use crate::commands;
use crate::inference_feedback;
use crate::install;
use crate::preview;
use crate::progressive_ui;
use crate::reporters;
use crate::tui;
use crate::{CompatibilityFallbackBackend, EnforcementMode, DEFAULT_RUN_REGISTRY_URL};

pub(crate) async fn resolve_installed_capsule_archive(
    scoped_ref: &install::ScopedCapsuleRef,
    registry: Option<&str>,
    preferred_version: Option<&str>,
) -> Result<Option<PathBuf>> {
    let store_root = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ato")
        .join("store");
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
        || combined.contains("pnpm-lock.yaml is missing")
        || combined.contains("package-lock.json")
        || combined.contains("requires one of package-lock.json")
        || combined.contains("multiple node lockfiles detected")
        || combined.contains("fail-closed provisioning")
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
            Some("github-inference"),
        )
        .into());
    }

    let lockfile_target = if lowered.contains("uv.lock") {
        Some("uv.lock")
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

    if let Some(isolation) = permissions.isolation.as_ref() {
        if !isolation.allow_env.is_empty() {
            printed_any = true;
            println!("  🔑 Isolation env allowlist:");
            for env in &isolation.allow_env {
                println!("    - {}", env);
            }
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
        anyhow::bail!(
            "Non-interactive JSON mode requires -y/--yes when auto-installing missing capsules"
        );
    }

    if !yes && !can_prompt_interactively(stdin_is_tty, stdout_is_tty) {
        anyhow::bail!(
            "Interactive install confirmation requires a TTY. Re-run with -y/--yes in CI or non-interactive environments."
        );
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
        anyhow::bail!("--enforcement best-effort is no longer supported; use --enforcement strict");
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
        anyhow::bail!(
            "--dangerously-skip-permissions and --compatibility-fallback are mutually exclusive"
        );
    }

    if dangerously_skip_permissions {
        if std::env::var(ENV_ALLOW_UNSAFE).ok().as_deref() != Some("1") {
            anyhow::bail!(
                "--dangerously-skip-permissions requires {}=1",
                ENV_ALLOW_UNSAFE
            );
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_open_command(
    path: PathBuf,
    target: Option<String>,
    watch: bool,
    background: bool,
    nacelle: Option<PathBuf>,
    enforcement: EnforcementMode,
    sandbox_mode: bool,
    dangerously_skip_permissions: bool,
    compatibility_fallback: Option<String>,
    assume_yes: bool,
    state: Vec<String>,
    inject: Vec<String>,
    reporter: std::sync::Arc<reporters::CliReporter>,
) -> Result<()> {
    let target_path = if path.is_file() || path.extension().is_some_and(|ext| ext == "capsule") {
        path.clone()
    } else {
        path.join("capsule.toml")
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(commands::open::execute(commands::open::OpenArgs {
        target: target_path,
        target_label: target,
        watch,
        background,
        nacelle,
        enforcement: enforcement.as_str().to_string(),
        sandbox_mode,
        dangerously_skip_permissions,
        compatibility_fallback,
        assume_yes,
        state_bindings: state,
        inject_bindings: inject,
        reporter,
        preview_mode: false,
    }))
}
