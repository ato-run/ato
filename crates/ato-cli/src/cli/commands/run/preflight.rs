use super::*;

use crate::application::pipeline::phases::run::PreparedRunContext;
#[cfg(test)]
use crate::executors::target_runner;
use capsule_core::importer::{
    probe_required_cargo_lockfile, probe_required_node_lockfile, probe_required_python_lockfile,
    ImportedEvidence, ImporterId, ProbeResult,
};
use capsule_core::lockfile::parse_lockfile_text;

pub(crate) fn preflight_native_sandbox(
    nacelle_override: Option<PathBuf>,
    plan: &capsule_core::router::ManifestData,
    prepared: &PreparedRunContext,
    effective_cwd: Option<&Path>,
    reporter: &Arc<CliReporter>,
) -> Result<PathBuf> {
    preflight_python_uv_lock_for_source_driver(plan)?;
    preflight_python_uv_binary_for_source_driver(plan, prepared.authoritative_lock.as_ref())?;
    preflight_glibc_compat(plan, prepared)?;
    preflight_macos_compat(plan)?;
    preflight_single_script_effective_cwd_compat(plan, prepared, effective_cwd)?;

    let nacelle = resolve_nacelle_for_tier2(nacelle_override, plan, prepared, reporter)?;
    let response = capsule_core::engine::run_internal(
        &nacelle,
        "features",
        &json!({ "spec_version": "0.1.0" }),
    )?;
    let capabilities = response
        .get("data")
        .and_then(|v| v.get("capabilities"))
        .or_else(|| response.get("capabilities"));

    let sandbox = capabilities
        .and_then(|v| v.get("sandbox"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if sandbox.is_empty() {
        return Err(AtoExecutionError::compat_hardware(
            "No compatible native sandbox backend is available",
            Some("sandbox"),
        )
        .into());
    }

    Ok(nacelle)
}

fn preflight_single_script_effective_cwd_compat(
    plan: &capsule_core::router::ManifestData,
    prepared: &PreparedRunContext,
    effective_cwd: Option<&Path>,
) -> Result<()> {
    let Some(effective_cwd) = effective_cwd else {
        return Ok(());
    };
    if !plan
        .execution_runtime()
        .as_deref()
        .is_some_and(|runtime| runtime.eq_ignore_ascii_case("source"))
    {
        return Ok(());
    }
    if prepared.workspace_root == plan.manifest_dir {
        return Ok(());
    }

    let Some(entrypoint) = plan
        .execution_entrypoint()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    if !is_relative_entrypoint_path(&entrypoint) {
        return Ok(());
    }
    if plan.execution_source_layout().as_deref() == Some("anchored_entrypoint") {
        return Ok(());
    }

    Err(AtoExecutionError::execution_contract_invalid(
        format!(
            "single-script source execution with relative entrypoint '{}' and effective cwd '{}' requires an anchored source entrypoint layout before native sandbox launch",
            entrypoint,
            effective_cwd.display()
        ),
        Some("targets.<selected>.source_layout"),
        Some(plan.selected_target_label()),
    )
    .into())
}

fn is_relative_entrypoint_path(entrypoint: &str) -> bool {
    if entrypoint.split_whitespace().count() != 1 {
        return false;
    }

    let candidate = Path::new(entrypoint);
    candidate.is_relative()
        && (entrypoint.contains('/')
            || entrypoint.ends_with(".py")
            || entrypoint.ends_with(".js")
            || entrypoint.ends_with(".mjs")
            || entrypoint.ends_with(".cjs")
            || entrypoint.ends_with(".ts")
            || entrypoint.ends_with(".tsx"))
}

fn resolve_nacelle_for_tier2(
    nacelle_override: Option<PathBuf>,
    plan: &capsule_core::router::ManifestData,
    prepared: &PreparedRunContext,
    reporter: &Arc<CliReporter>,
) -> Result<PathBuf> {
    if should_attempt_nacelle_auto_bootstrap(nacelle_override.as_deref(), prepared)?
        && nacelle_auto_bootstrap_forced()
    {
        return crate::engine_manager::auto_bootstrap_nacelle(&**reporter)
            .map(|installed| installed.path)
            .map_err(|bootstrap_err| {
                AtoExecutionError::engine_missing(
                    format!(
                        "Tier 2 execution requires 'nacelle', and auto-bootstrap failed: {bootstrap_err}"
                    ),
                    Some("nacelle"),
                )
                .into()
            });
    }

    let request = capsule_core::engine::EngineRequest {
        explicit_path: nacelle_override.clone(),
        manifest_path: Some(plan.manifest_path.clone()),
        compat_input: None,
    };

    match capsule_core::engine::discover_nacelle(request) {
        Ok(path) => Ok(path),
        Err(err) => {
            if !should_attempt_nacelle_auto_bootstrap(nacelle_override.as_deref(), prepared)? {
                return Err(AtoExecutionError::engine_missing(
                    format!(
                        "Tier 2 execution requires 'nacelle', but the configured engine is not usable: {err}"
                    ),
                    Some("nacelle"),
                )
                .into());
            }

            crate::engine_manager::auto_bootstrap_nacelle(&**reporter)
                .map(|installed| installed.path)
                .map_err(|bootstrap_err| {
                    AtoExecutionError::engine_missing(
                        format!(
                            "Tier 2 execution requires 'nacelle', and auto-bootstrap failed: {bootstrap_err}"
                        ),
                        Some("nacelle"),
                    )
                    .into()
                })
        }
    }
}

fn should_attempt_nacelle_auto_bootstrap(
    nacelle_override: Option<&Path>,
    prepared: &PreparedRunContext,
) -> Result<bool> {
    if nacelle_override.is_some() {
        return Ok(false);
    }
    if std::env::var("NACELLE_PATH")
        .ok()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
    {
        return Ok(false);
    }
    if manifest_declares_engine_override(prepared) {
        return Ok(false);
    }

    Ok(true)
}

fn nacelle_auto_bootstrap_forced() -> bool {
    std::env::var("ATO_NACELLE_AUTO_BOOTSTRAP")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "on" | "always" | "force" | "enabled"
            )
        })
        .unwrap_or(false)
}

fn manifest_declares_engine_override(prepared: &PreparedRunContext) -> bool {
    prepared.engine_override_declared
}

#[cfg(test)]
pub(super) fn preflight_required_environment_variables(
    plan: &capsule_core::router::ManifestData,
) -> Result<()> {
    target_runner::preflight_required_environment_variables(
        plan,
        &crate::executors::launch_context::RuntimeLaunchContext::empty(),
    )
}

pub(crate) async fn run_v03_lifecycle_steps(
    plan: &capsule_core::router::ManifestData,
    reporter: &Arc<CliReporter>,
    launch_ctx: &crate::executors::launch_context::RuntimeLaunchContext,
) -> Result<()> {
    let schema_version = plan
        .manifest
        .get("schema_version")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if schema_version != "0.3" {
        return Ok(());
    }

    let mut provisioned_roots = std::collections::HashSet::new();
    for target_label in plan.selected_target_package_order()? {
        let target_plan = plan.with_selected_target(target_label.clone());
        let working_dir = resolve_provision_working_dir(&target_plan);

        if provisioned_roots.insert(working_dir.clone()) {
            if let Some(command) = plan_v03_provision_command(&target_plan)? {
                reporter
                    .notify(format!("⚙️  Provision [{}]: {}", target_label, command))
                    .await?;
                run_lifecycle_shell_command(
                    &target_plan,
                    launch_ctx,
                    &command,
                    "provision",
                    &working_dir,
                )?;
            }
        }

        if let Some(command) = target_plan
            .build_lifecycle_build()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            reporter
                .notify(format!("🏗️  Build [{}]: {}", target_label, command))
                .await?;
            run_lifecycle_shell_command(&target_plan, launch_ctx, &command, "build", &working_dir)?;
        }
    }

    Ok(())
}

/// Returns the working directory for provision/build lifecycle commands.
/// For GitHub-installed capsules, project files live under source/ while
/// execution_working_directory() returns the outer manifest dir (no working_dir
/// in capsule.toml). Detect this layout so npm/pnpm/cargo run where package.json
/// and lockfiles actually are.
fn resolve_provision_working_dir(plan: &capsule_core::router::ManifestData) -> std::path::PathBuf {
    let source_dir = plan.manifest_dir.join("source");
    if source_dir.join("package.json").exists() {
        return source_dir;
    }
    plan.execution_working_directory()
}

pub(super) fn plan_v03_provision_command(
    plan: &capsule_core::router::ManifestData,
) -> Result<Option<String>> {
    let runtime = plan.execution_runtime().unwrap_or_default();
    let driver = plan.execution_driver().unwrap_or_default();
    let runtime = runtime.trim().to_ascii_lowercase();
    let driver = driver.trim().to_ascii_lowercase();
    let manifest_dir = plan.manifest_dir.clone();
    let execution_working_directory = resolve_provision_working_dir(plan);

    if runtime == "web" && driver == "static" {
        debug!(
            phase = "run",
            runtime,
            driver,
            manifest_dir = %manifest_dir.display(),
            execution_working_directory = %execution_working_directory.display(),
            lockfile_check_paths = ?Vec::<(&str, std::path::PathBuf, bool)>::new(),
            "Provision command path diagnostics"
        );
        return Ok(None);
    }

    if matches!(driver.as_str(), "node") {
        debug!(
            phase = "run",
            runtime,
            driver,
            manifest_dir = %manifest_dir.display(),
            execution_working_directory = %execution_working_directory.display(),
            "Provision command path diagnostics"
        );
        return provision_command_from_node_importer(&execution_working_directory);
    }

    if matches!(driver.as_str(), "python") {
        debug!(
            phase = "run",
            runtime,
            driver,
            manifest_dir = %manifest_dir.display(),
            execution_working_directory = %execution_working_directory.display(),
            "Provision command path diagnostics"
        );
        return provision_command_from_python_importer(&execution_working_directory);
    }

    debug!(
        phase = "run",
        runtime,
        driver,
        manifest_dir = %manifest_dir.display(),
        execution_working_directory = %execution_working_directory.display(),
        "Provision command path diagnostics"
    );
    if matches!(driver.as_str(), "native") {
        return provision_command_from_cargo_importer(&execution_working_directory);
    }

    Ok(None)
}

fn provision_command_from_node_importer(
    execution_working_directory: &Path,
) -> Result<Option<String>> {
    if !execution_working_directory.join("package.json").exists() {
        return Ok(None);
    }
    match probe_required_node_lockfile(execution_working_directory)? {
        ProbeResult::Found(values) => Ok(Some(node_install_command_from_evidence(&values[0])?)),
        ProbeResult::Missing(_) => Ok(None),
        ProbeResult::Ambiguous(ambiguity) => {
            // Multiple lockfiles present; prefer pnpm > npm > yarn > bun.
            let priority_order = [
                ImporterId::Pnpm,
                ImporterId::Npm,
                ImporterId::Yarn,
                ImporterId::Bun,
            ];
            let cmd = priority_order
                .iter()
                .find(|id| ambiguity.importer_ids.contains(id))
                .and_then(|id| match id {
                    ImporterId::Pnpm => Some("pnpm install"),
                    ImporterId::Npm => Some("npm install --legacy-peer-deps"),
                    ImporterId::Yarn => Some("yarn install"),
                    ImporterId::Bun => Some("bun install"),
                    _ => None,
                })
                .unwrap_or("npm install --legacy-peer-deps");
            Ok(Some(cmd.to_string()))
        }
        ProbeResult::NotApplicable => Ok(None),
    }
}

fn provision_command_from_python_importer(
    execution_working_directory: &Path,
) -> Result<Option<String>> {
    if let Some(requirements_path) = resolve_python_requirements_path(execution_working_directory) {
        let requirements_arg = requirements_path
            .strip_prefix(execution_working_directory)
            .unwrap_or(requirements_path.as_path())
            .to_string_lossy()
            .replace('\\', "/");
        return Ok(Some(format!(
            "uv venv && uv pip install -r {requirements_arg}"
        )));
    }

    match probe_required_python_lockfile(execution_working_directory)? {
        ProbeResult::Found(_) => Ok(Some("uv sync --frozen".to_string())),
        ProbeResult::Missing(missing) => {
            Err(AtoExecutionError::lock_incomplete(missing.message, Some("uv.lock")).into())
        }
        ProbeResult::Ambiguous(ambiguity) => {
            Err(AtoExecutionError::lock_incomplete(ambiguity.message, Some("uv.lock")).into())
        }
        ProbeResult::NotApplicable => Ok(None),
    }
}

fn provision_command_from_cargo_importer(
    execution_working_directory: &Path,
) -> Result<Option<String>> {
    match probe_required_cargo_lockfile(execution_working_directory)? {
        ProbeResult::Found(_) => Ok(Some("cargo fetch --locked".to_string())),
        ProbeResult::Missing(_) | ProbeResult::NotApplicable => Ok(None),
        ProbeResult::Ambiguous(ambiguity) => {
            Err(AtoExecutionError::lock_incomplete(ambiguity.message, Some("Cargo.lock")).into())
        }
    }
}

fn node_install_command_from_evidence(evidence: &ImportedEvidence) -> Result<String> {
    // Source/GitHub runs use non-strict install: lockfiles may come from a different
    // platform or OS than the current machine, so --frozen-lockfile / npm ci would fail
    // on checksum mismatches. Plain install is correct for developer-preview mode.
    // --legacy-peer-deps allows older projects that rely on npm v6 conflict-resolution
    // to install without hard errors on peer dependency mismatches.
    let command = match evidence.importer_id {
        ImporterId::Npm => "npm install --legacy-peer-deps",
        ImporterId::Yarn => "yarn install",
        ImporterId::Pnpm => "pnpm install",
        ImporterId::Bun => "bun install",
        other => {
            return Err(anyhow::anyhow!(
                "unsupported node importer '{}' for provision command",
                other.as_str()
            ))
        }
    };
    Ok(command.to_string())
}

fn run_lifecycle_shell_command(
    plan: &capsule_core::router::ManifestData,
    launch_ctx: &crate::executors::launch_context::RuntimeLaunchContext,
    command: &str,
    phase: &str,
    working_dir: &Path,
) -> Result<()> {
    // Some packages have preinstall scripts that call `lefthook install` or similar
    // git-hook managers. These fail with exit 128 because the source checkout has no
    // .git directory. Creating a minimal stub allows git-root detection to succeed.
    // We remove the stub after the command so it does not persist in the capsule.
    let fake_git = working_dir.join(".git");
    let created_fake_git = if !fake_git.exists() {
        std::fs::create_dir_all(&fake_git).is_ok()
    } else {
        false
    };

    // Prepend the ato-managed Node bin dir to PATH inside the command string itself
    // (#294). We cannot rely on setting PATH in the subprocess env because `sh -l`
    // sources login profile scripts (e.g. /etc/profile) which unconditionally reset
    // PATH. By prefixing "export PATH=<dir>:$PATH;" we run after the profile reset
    // and guarantee the managed npm/node are found first.
    // Use `ensure_node_binary_with_authority(plan, None)` so provider-backed targets
    // (npm:pkg) that store runtime_version in capsule.toml are handled correctly —
    // `ensure_node_binary` alone requires capsule.lock.json which providers don't create.
    let managed_node_path_prefix: String =
        match runtime_manager::ensure_node_binary_with_authority(plan, None) {
            Ok(node_bin) => node_bin
                .parent()
                .map(|dir| format!("export PATH={}:$PATH; ", dir.display()))
                .unwrap_or_default(),
            Err(_) => String::new(),
        };
    let effective_command = format!("{}{}", managed_node_path_prefix, command);

    #[cfg(windows)]
    let mut cmd = {
        let mut cmd = std::process::Command::new("cmd");
        cmd.args(["/C", &effective_command]);
        cmd
    };

    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-lc", &effective_command]);
        cmd
    };

    cmd.current_dir(working_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .env("COREPACK_ENABLE_STRICT", "0")
        // Disable pnpm 10's auto-manage-package-manager-versions to prevent it from
        // attempting to download the pinned pnpm version in offline/CI environments.
        .env("npm_config_manage_package_manager_versions", "false")
        .env("npm_config_approve_builds", "on")
        // Skip git-hooks managers (husky, lefthook, etc.): the capsule workspace
        // has no .git dir so their prepare/postinstall scripts would fail with exit 128.
        .env("HUSKY", "0")
        .env("LEFTHOOK", "0");

    for (key, value) in runtime_overrides::merged_env(plan.execution_env()) {
        cmd.env(key, value);
    }
    launch_ctx.apply_allowlisted_env(&mut cmd)?;

    let status = cmd
        .status()
        .with_context(|| format!("Failed to execute {} command", phase));

    if created_fake_git {
        let _ = std::fs::remove_dir_all(&fake_git);
    }

    let status = status?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "{} command failed with exit code {}: {}",
            phase,
            status.code().unwrap_or(1),
            command
        ))
    }
}

fn preflight_macos_compat(plan: &capsule_core::router::ManifestData) -> Result<()> {
    let required_raw = match detect_required_macos_from_entrypoint(plan)? {
        Some(value) => value,
        None => return Ok(()),
    };

    let required_version = normalize_version(&required_raw).ok_or_else(|| {
        AtoExecutionError::compat_hardware(
            format!("Invalid macOS version constraint '{}'", required_raw),
            Some("macos"),
        )
    })?;

    let host_os = std::env::consts::OS;
    if host_os != "macos" {
        return Err(AtoExecutionError::compat_hardware(
            format!(
                "macOS {} is required but host OS is {}",
                required_raw, host_os
            ),
            Some("macos"),
        )
        .into());
    }

    let host_raw = detect_host_macos_version().ok_or_else(|| {
        AtoExecutionError::compat_hardware(
            "Unable to detect host macOS version".to_string(),
            Some("macos"),
        )
    })?;

    let host_version = normalize_version(&host_raw).ok_or_else(|| {
        AtoExecutionError::compat_hardware(
            format!("Unable to parse host macOS version '{}'", host_raw),
            Some("macos"),
        )
    })?;

    if compare_versions(&host_version, &required_version) < 0 {
        return Err(AtoExecutionError::compat_hardware(
            format!(
                "macOS {} is required but host has {}",
                required_raw, host_raw
            ),
            Some("macos"),
        )
        .into());
    }

    Ok(())
}

fn preflight_python_uv_lock_for_source_driver(
    plan: &capsule_core::router::ManifestData,
) -> Result<()> {
    if !is_python_source_target(plan) {
        return Ok(());
    }

    if resolve_python_requirements_path(&plan.manifest_dir).is_some() {
        return Ok(());
    }

    match probe_required_python_lockfile(&plan.manifest_dir)? {
        ProbeResult::Found(_) => return Ok(()),
        ProbeResult::Missing(_) | ProbeResult::NotApplicable => {}
        ProbeResult::Ambiguous(ambiguity) => {
            return Err(
                AtoExecutionError::lock_incomplete(ambiguity.message, Some("uv.lock")).into(),
            )
        }
    }

    Err(AtoExecutionError::lock_incomplete(
        "source/python target requires uv.lock or requirements.txt for fail-closed provisioning",
        Some("uv.lock"),
    )
    .into())
}

fn preflight_python_uv_binary_for_source_driver(
    plan: &capsule_core::router::ManifestData,
    authoritative_lock: Option<&capsule_core::ato_lock::AtoLock>,
) -> Result<()> {
    if !is_python_source_target(plan) {
        return Ok(());
    }

    if resolve_python_requirements_path(&plan.manifest_dir).is_some() {
        return which::which("uv").map(|_| ()).map_err(|_| {
            AtoExecutionError::lock_incomplete(
                "source/python target requires uv on PATH when using requirements.txt",
                Some("uv"),
            )
            .into()
        });
    }

    runtime_manager::ensure_uv_binary_with_authority(plan, authoritative_lock)
        .map(|_| ())
        .map_err(|_| {
            AtoExecutionError::lock_incomplete(
                "source/python target requires hermetic uv from capsule.lock.json (tools.uv)",
                Some(CAPSULE_LOCK_FILE_NAME),
            )
            .into()
        })
}

fn is_python_source_target(plan: &capsule_core::router::ManifestData) -> bool {
    let runtime = plan.execution_runtime().unwrap_or_default();
    if !runtime.eq_ignore_ascii_case("source") {
        return false;
    }

    let driver = plan.execution_driver().unwrap_or_default();
    if !driver.eq_ignore_ascii_case("native") && !driver.eq_ignore_ascii_case("python") {
        return false;
    }

    plan.execution_entrypoint()
        .or_else(|| plan.execution_run_command())
        .map(|entry| entry.trim().to_ascii_lowercase().ends_with(".py"))
        .unwrap_or(false)
}

fn preflight_glibc_compat(
    plan: &capsule_core::router::ManifestData,
    prepared: &PreparedRunContext,
) -> Result<()> {
    let required_from_elf = detect_required_glibc_from_entrypoint(plan)?;
    let required_from_lock = prepared
        .compatibility_legacy_lock
        .as_ref()
        .map(|legacy| detect_required_glibc_from_lock(&legacy.path))
        .transpose()?
        .flatten();
    let required_raw = match required_from_elf.or(required_from_lock) {
        Some(value) => value,
        None => return Ok(()),
    };

    let required_version = normalize_version(&required_raw).ok_or_else(|| {
        AtoExecutionError::compat_hardware(
            format!("Invalid glibc version constraint '{}'", required_raw),
            Some("glibc"),
        )
    })?;

    let host_os = std::env::consts::OS;
    if host_os != "linux" {
        return Err(AtoExecutionError::compat_hardware(
            format!(
                "glibc {} is required but host OS is {}",
                required_raw, host_os
            ),
            Some("glibc"),
        )
        .into());
    }

    let host_raw = detect_host_glibc_version().ok_or_else(|| {
        AtoExecutionError::compat_hardware(
            "Unable to detect host glibc version".to_string(),
            Some("glibc"),
        )
    })?;

    let host_version = normalize_version(&host_raw).ok_or_else(|| {
        AtoExecutionError::compat_hardware(
            format!("Unable to parse host glibc version '{}'", host_raw),
            Some("glibc"),
        )
    })?;

    if compare_versions(&host_version, &required_version) < 0 {
        return Err(AtoExecutionError::compat_hardware(
            format!(
                "glibc {} is required but host has {}",
                required_raw, host_raw
            ),
            Some("glibc"),
        )
        .into());
    }

    Ok(())
}

fn detect_required_glibc_from_lock(lock_path: &Path) -> Result<Option<String>> {
    if !lock_path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(lock_path)
        .with_context(|| format!("Failed to read {}", lock_path.display()))?;
    let typed = parse_lockfile_text(&raw, lock_path);
    if let Ok(lockfile) = typed.as_ref() {
        if let Some(required) = lockfile
            .targets
            .values()
            .find_map(|target| target.constraints.as_ref().and_then(|c| c.glibc.clone()))
        {
            return Ok(Some(required));
        }
    }

    if let Some(required) = extract_glibc_constraint_from_lock_text(&raw) {
        return Ok(Some(required));
    }

    typed
        .with_context(|| format!("Failed to parse {}", lock_path.display()))
        .map(|_| None)
}

fn extract_glibc_constraint_from_lock_text(raw: &str) -> Option<String> {
    extract_glibc_constraint_from_json(&serde_json::from_str::<serde_json::Value>(raw).ok()?)
        .or_else(|| extract_glibc_constraint_from_toml(&toml::from_str::<toml::Value>(raw).ok()?))
}

fn extract_glibc_constraint_from_json(value: &serde_json::Value) -> Option<String> {
    value
        .get("targets")?
        .as_object()?
        .values()
        .find_map(|target| {
            target
                .get("constraints")
                .and_then(|constraints| constraints.get("glibc"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
}

fn extract_glibc_constraint_from_toml(value: &toml::Value) -> Option<String> {
    value
        .get("targets")?
        .as_table()?
        .values()
        .find_map(|target| {
            target
                .get("constraints")
                .and_then(|constraints| constraints.get("glibc"))
                .and_then(toml::Value::as_str)
                .map(str::to_string)
        })
}

fn detect_required_glibc_from_entrypoint(
    plan: &capsule_core::router::ManifestData,
) -> Result<Option<String>> {
    let entrypoint = match plan
        .execution_entrypoint()
        .or_else(|| {
            plan.execution_run_command()
                .and_then(|command| first_command_token(&command))
        })
        .filter(|value| !value.trim().is_empty())
    {
        Some(value) => value,
        None => return Ok(None),
    };

    let path = {
        let candidate = PathBuf::from(entrypoint);
        if candidate.is_absolute() {
            candidate
        } else {
            plan.manifest_dir.join(candidate)
        }
    };

    if !path.exists() || !path.is_file() {
        return Ok(None);
    }

    let bytes = fs::read(&path)
        .with_context(|| format!("Failed to read native entrypoint {}", path.display()))?;
    if bytes.len() < 4 || &bytes[0..4] != b"\x7FELF" {
        return Ok(None);
    }

    let elf = Elf::parse(&bytes).map_err(|err| {
        AtoExecutionError::compat_hardware(
            format!(
                "Failed to parse ELF entrypoint '{}': {}",
                path.display(),
                err
            ),
            Some("glibc"),
        )
    })?;

    let has_verneed = elf
        .dynamic
        .as_ref()
        .map(|dynamic| dynamic.dyns.iter().any(|entry| entry.d_tag == DT_VERNEED))
        .unwrap_or(false);
    if !has_verneed {
        return Ok(None);
    }

    let regex =
        Regex::new(r"GLIBC_[0-9]+(?:\.[0-9]+)+").expect("failed to compile GLIBC version regex");
    let corpus = String::from_utf8_lossy(&bytes);

    let mut best_raw: Option<String> = None;
    let mut best_parts: Option<Vec<u32>> = None;
    for matched in regex.find_iter(&corpus).map(|m| m.as_str().to_string()) {
        let Some(parts) = normalize_version(&matched) else {
            continue;
        };
        if best_parts
            .as_ref()
            .map(|current| compare_versions(current, &parts) < 0)
            .unwrap_or(true)
        {
            best_raw = Some(matched);
            best_parts = Some(parts);
        }
    }

    Ok(best_raw)
}

fn first_command_token(command: &str) -> Option<String> {
    shell_words::split(command)
        .ok()
        .and_then(|tokens| tokens.into_iter().next())
        .or_else(|| {
            let trimmed = command.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
}

fn detect_required_macos_from_entrypoint(
    plan: &capsule_core::router::ManifestData,
) -> Result<Option<String>> {
    let entrypoint = match plan
        .execution_entrypoint()
        .filter(|value| !value.trim().is_empty())
    {
        Some(value) => value,
        None => return Ok(None),
    };

    let path = {
        let candidate = PathBuf::from(entrypoint);
        if candidate.is_absolute() {
            candidate
        } else {
            plan.manifest_dir.join(candidate)
        }
    };

    if !path.exists() || !path.is_file() {
        return Ok(None);
    }

    let bytes = fs::read(&path)
        .with_context(|| format!("Failed to read native entrypoint {}", path.display()))?;
    let mach = match Mach::parse(&bytes) {
        Ok(parsed) => parsed,
        Err(_) => return Ok(None),
    };

    let mut best_raw: Option<String> = None;
    let mut best_parts: Option<Vec<u32>> = None;

    let mut update_best = |candidate: String| {
        let Some(parts) = normalize_version(&candidate) else {
            return;
        };
        if best_parts
            .as_ref()
            .map(|current| compare_versions(current, &parts) < 0)
            .unwrap_or(true)
        {
            best_raw = Some(candidate);
            best_parts = Some(parts);
        }
    };

    match mach {
        Mach::Binary(binary) => {
            if let Some(ver) = extract_min_macos_from_macho(&binary) {
                update_best(ver);
            }
        }
        Mach::Fat(fat) => {
            for entry in fat.into_iter() {
                let Ok(entry) = entry else {
                    continue;
                };
                if let SingleArch::MachO(binary) = entry {
                    if let Some(ver) = extract_min_macos_from_macho(&binary) {
                        update_best(ver);
                    }
                }
            }
        }
    }

    Ok(best_raw)
}

fn extract_min_macos_from_macho(binary: &goblin::mach::MachO<'_>) -> Option<String> {
    let mut best_raw: Option<String> = None;
    let mut best_parts: Option<Vec<u32>> = None;

    for cmd in &binary.load_commands {
        let raw = match &cmd.command {
            CommandVariant::BuildVersion(build) => decode_macho_version(build.minos),
            CommandVariant::VersionMinMacosx(min) => decode_macho_version(min.version),
            _ => None,
        };

        let Some(candidate) = raw else {
            continue;
        };
        let Some(parts) = normalize_version(&candidate) else {
            continue;
        };

        if best_parts
            .as_ref()
            .map(|current| compare_versions(current, &parts) < 0)
            .unwrap_or(true)
        {
            best_parts = Some(parts);
            best_raw = Some(candidate);
        }
    }

    best_raw
}

fn decode_macho_version(encoded: u32) -> Option<String> {
    let major = (encoded >> 16) & 0xffff;
    let minor = (encoded >> 8) & 0xff;
    let patch = encoded & 0xff;
    if major == 0 {
        return None;
    }
    Some(format!("{}.{}.{}", major, minor, patch))
}

fn normalize_version(value: &str) -> Option<Vec<u32>> {
    let normalized = value
        .trim()
        .trim_start_matches("GLIBC_")
        .trim_start_matches("GLIBC")
        .trim_start_matches("glibc")
        .trim_start_matches('-')
        .trim_start_matches('=')
        .trim();
    if normalized.is_empty() {
        return None;
    }

    let mut out = Vec::new();
    for segment in normalized.split('.') {
        if segment.is_empty() {
            continue;
        }
        let digits = segment
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>();
        if digits.is_empty() {
            break;
        }
        let parsed = digits.parse::<u32>().ok()?;
        out.push(parsed);
    }

    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn compare_versions(left: &[u32], right: &[u32]) -> i32 {
    let max_len = left.len().max(right.len());
    for idx in 0..max_len {
        let l = *left.get(idx).unwrap_or(&0);
        let r = *right.get(idx).unwrap_or(&0);
        if l < r {
            return -1;
        }
        if l > r {
            return 1;
        }
    }
    0
}

fn detect_host_glibc_version() -> Option<String> {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        let ptr = unsafe { libc::gnu_get_libc_version() };
        if ptr.is_null() {
            return None;
        }
        let cstr = unsafe { std::ffi::CStr::from_ptr(ptr) };
        Some(cstr.to_string_lossy().to_string())
    }

    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    {
        None
    }
}

fn detect_host_macos_version() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if version.is_empty() {
            None
        } else {
            Some(version)
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(test)]
fn resolve_uv_lock_path(manifest_dir: &Path) -> Option<PathBuf> {
    match probe_required_python_lockfile(manifest_dir).ok()? {
        ProbeResult::Found(values) => values.first().map(|value| value.primary_path.clone()),
        _ => None,
    }
}

fn resolve_python_requirements_path(manifest_dir: &Path) -> Option<PathBuf> {
    [
        manifest_dir.join("requirements.txt"),
        manifest_dir.join("source").join("requirements.txt"),
    ]
    .into_iter()
    .find(|path| path.exists())
}

#[cfg(test)]
pub(super) fn resolve_python_dependency_lock_path(manifest_dir: &Path) -> Option<PathBuf> {
    resolve_uv_lock_path(manifest_dir).or_else(|| resolve_python_requirements_path(manifest_dir))
}

#[cfg(test)]
mod tests {
    use super::{
        detect_required_glibc_from_lock, preflight_glibc_compat,
        preflight_single_script_effective_cwd_compat,
    };
    use crate::application::pipeline::phases::run::DerivedBridgeManifest;
    use crate::application::pipeline::phases::run::PreparedRunContext;
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    fn prepared_context(workspace_root: &Path) -> PreparedRunContext {
        PreparedRunContext {
            authoritative_lock: None,
            lock_path: None,
            workspace_root: workspace_root.to_path_buf(),
            effective_state: None,
            execution_override: None,
            bridge_manifest: DerivedBridgeManifest::new(toml::Value::Table(toml::map::Map::new())),
            validation_mode: capsule_core::types::ValidationMode::Strict,
            engine_override_declared: false,
            compatibility_legacy_lock: None,
        }
    }

    fn build_plan(manifest_dir: &Path, manifest: &str) -> capsule_core::router::ManifestData {
        capsule_core::router::execution_descriptor_from_manifest_parts(
            toml::from_str::<toml::Value>(manifest).expect("parse manifest"),
            manifest_dir.join("capsule.toml"),
            manifest_dir.to_path_buf(),
            capsule_core::router::ExecutionProfile::Dev,
            Some("default"),
            std::collections::HashMap::new(),
        )
        .expect("execution descriptor")
    }

    #[test]
    fn detect_required_glibc_from_lock_reads_target_constraints_from_json() {
        let dir = tempdir().expect("tempdir");
        let lock_path = dir.path().join("capsule.lock.json");
        fs::write(
            &lock_path,
            r#"{
  "version": "1",
  "meta": {
    "created_at": "2026-02-23T00:00:00Z",
    "manifest_hash": "blake3:test"
  },
  "targets": {
    "x86_64-unknown-linux-gnu": {
      "constraints": {
        "glibc": "glibc-999.0"
      }
    }
  }
}"#,
        )
        .expect("write lock");

        let detected = detect_required_glibc_from_lock(&lock_path).expect("detect glibc");
        assert_eq!(detected.as_deref(), Some("glibc-999.0"));
    }

    #[test]
    fn preflight_glibc_ignores_stray_legacy_lock_without_compatibility_context() {
        let dir = tempdir().expect("tempdir");
        let manifest_dir = dir.path().to_path_buf();
        let lock_path = dir.path().join("capsule.lock.json");
        fs::write(
            &lock_path,
            r#"{
  "version": "1",
  "meta": {
    "created_at": "2026-02-23T00:00:00Z",
    "manifest_hash": "blake3:test"
  },
  "targets": {
    "x86_64-unknown-linux-gnu": {
      "constraints": {
        "glibc": "glibc-999.0"
      }
    }
  }
}"#,
        )
        .expect("write lock");

        let plan = build_plan(
            &manifest_dir,
            r#"
name = "demo"
type = "app"
default_target = "default"

[targets.default]
runtime = "source"
driver = "native"
entrypoint = "demo.sh"
"#,
        );
        let prepared = prepared_context(&manifest_dir);

        preflight_glibc_compat(&plan, &prepared).expect("ignore stray legacy lock");
    }

    #[test]
    fn materialized_single_script_requires_anchored_layout_when_effective_cwd_is_set() {
        let dir = tempdir().expect("tempdir");
        let manifest_dir = dir.path().join("materialized");
        fs::create_dir_all(&manifest_dir).expect("create manifest dir");
        let workspace_root = dir.path().join("caller-workspace");
        fs::create_dir_all(&workspace_root).expect("create workspace root");

        let plan = build_plan(
            &manifest_dir,
            r#"
name = "demo"
type = "job"
default_target = "default"

[targets.default]
runtime = "source"
driver = "python"
entrypoint = "main.py"
"#,
        );
        let prepared = prepared_context(&workspace_root);
        let effective_cwd = PathBuf::from("/caller/workspace/reference");

        let err = preflight_single_script_effective_cwd_compat(
            &plan,
            &prepared,
            Some(effective_cwd.as_path()),
        )
        .expect_err("missing anchored layout should fail closed");

        assert!(err
            .to_string()
            .contains("requires an anchored source entrypoint layout"));
    }

    #[test]
    fn materialized_single_script_accepts_anchored_layout_when_effective_cwd_is_set() {
        let dir = tempdir().expect("tempdir");
        let manifest_dir = dir.path().join("materialized");
        fs::create_dir_all(&manifest_dir).expect("create manifest dir");
        let workspace_root = dir.path().join("caller-workspace");
        fs::create_dir_all(&workspace_root).expect("create workspace root");

        let plan = build_plan(
            &manifest_dir,
            r#"
name = "demo"
type = "job"
default_target = "default"

[targets.default]
runtime = "source"
driver = "python"
entrypoint = "main.py"
source_layout = "anchored_entrypoint"
"#,
        );
        let prepared = prepared_context(&workspace_root);
        let effective_cwd = PathBuf::from("/caller/workspace/reference");

        preflight_single_script_effective_cwd_compat(
            &plan,
            &prepared,
            Some(effective_cwd.as_path()),
        )
        .expect("anchored layout should pass preflight");
    }
}
