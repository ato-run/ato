use super::*;

use crate::adapters::runtime::provisioning::{
    LifecyclePathPlan, LifecyclePhase, build_lifecycle_path_plan, dependency_root,
    materialize_lifecycle_toolchains, python_requirements_lock_sync_command,
};
use crate::application::pipeline::phases::run::PreparedRunContext;
#[cfg(test)]
use crate::executors::target_runner;
use capsule::importer::{
    ImportedEvidence, ImporterId, ProbeResult, probe_required_cargo_lockfile,
    probe_required_node_lockfile, probe_required_python_lockfile,
};
use capsule::lockfile::parse_lockfile_text;
use capsule::types::MANIFEST_SCHEMA_V03;

pub(crate) fn preflight_native_sandbox(
    nacelle_override: Option<PathBuf>,
    plan: &capsule::router::ManifestData,
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
    let response =
        capsule::engine::run_internal(&nacelle, "features", &json!({ "spec_version": "0.1.0" }))?;
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
    plan: &capsule::router::ManifestData,
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
    plan: &capsule::router::ManifestData,
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

    let request = capsule::engine::EngineRequest {
        explicit_path: nacelle_override.clone(),
        manifest_path: Some(plan.manifest_path.clone()),
        compat_input: None,
    };

    match capsule::engine::discover_nacelle(request) {
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
    plan: &capsule::router::ManifestData,
) -> Result<()> {
    target_runner::preflight_required_environment_variables(
        plan,
        &crate::executors::launch_context::RuntimeLaunchContext::empty(),
    )
}

pub(crate) async fn run_v03_lifecycle_steps(
    plan: &capsule::router::ManifestData,
    reporter: &Arc<CliReporter>,
    launch_ctx: &crate::executors::launch_context::RuntimeLaunchContext,
) -> Result<()> {
    let schema_version = plan
        .manifest
        .get("schema_version")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if schema_version != MANIFEST_SCHEMA_V03 {
        return Ok(());
    }

    // In orchestration mode, every service's target needs its own
    // provision (`uv venv`, `npm ci`, ...). The default closure from
    // `selected_target_package_order` only walks `selected_target` plus
    // its workspace `package_dependencies`, which leaves sibling
    // services (e.g. a Vite frontend declared by `[services.web] target =
    // "web"`) un-provisioned and their dev binaries (`vite` from
    // node_modules/.bin) unreachable when the orchestrator launches
    // them. Build a target list that covers both: the selected target's
    // closure plus every distinct `[services.*].target`.
    let mut targets_to_provision: Vec<String> = plan.selected_target_package_order()?;
    if plan.is_orchestration_mode() {
        let mut seen: std::collections::HashSet<String> =
            targets_to_provision.iter().cloned().collect();
        for service in plan.services().values() {
            if let Some(target) = service.target.as_ref() {
                let label = target.trim();
                if !label.is_empty() && seen.insert(label.to_string()) {
                    targets_to_provision.push(label.to_string());
                }
            }
        }
    }

    // Pre-load the typed manifest so the orchestration provisioning loop
    // can fall back to manifest-declared runtime/driver when the lock's
    // resolved_targets/workloads are empty (the auto-lock path that
    // produces the dep-contract derived lock often hasn't filled
    // resolved_targets at preflight time, so plan.with_selected_target
    // followed by execution_runtime/execution_driver returns empty for
    // non-default targets — the provision dispatcher then short-circuits
    // to None and `npm ci` for the Vite frontend never runs).
    let typed_manifest = plan.typed_manifest().ok();

    let lifecycle_targets = build_lifecycle_targets(plan, &targets_to_provision)?;
    let root_install_plan = build_root_install_plan(&lifecycle_targets)?;
    let lifecycle_roots = root_order(&lifecycle_targets);
    let typed_manifest = typed_manifest.as_ref();

    // Warn once, the first time a lifecycle command actually runs unsandboxed on
    // the host (build/install steps execute the capsule's own scripts on the host
    // with no network/filesystem sandbox and no capsule secrets).
    let mut host_lifecycle_warned = false;
    let mut provisioned_roots = std::collections::HashSet::new();
    for root in root_order(&lifecycle_targets) {
        let Some(root_target) = lifecycle_targets
            .iter()
            .find(|target| target.working_dir == root)
        else {
            continue;
        };
        let target_plan = plan.with_selected_target(root_target.label.clone());

        // Log lifecycle phase context for debugging workspace isolation issues.
        // Gate behind ATO_DEBUG_LIFECYCLE=1 to avoid verbose output in normal runs.
        if std::env::var_os("ATO_DEBUG_LIFECYCLE").as_deref() == Some(std::ffi::OsStr::new("1")) {
            let which_bun = std::process::Command::new("which")
                .arg("bun")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .unwrap_or_default();
            let node_modules_at_cwd = root.join("node_modules").is_dir();
            tracing::info!(
                phase = "pre-install", target_label = %root_target.label,
                workspace_root = %plan.workspace_root.display(),
                manifest_dir = %plan.manifest_dir.display(),
                cwd = %root.display(),
                which_bun = %which_bun.trim(),
                node_modules_at_cwd = %node_modules_at_cwd,
            );
        }

        if let Some(install) = root_install_plan.get(&root) {
            reporter
                .notify(format!(
                    "⚙️  Install [{}]: {}",
                    install.label, install.command
                ))
                .await?;
            let install_plan = plan.with_selected_target(install.label.clone());
            let path_plan = build_lifecycle_phase_path_plan(
                &install_plan,
                LifecyclePhase::Install,
                &install.command,
                &lifecycle_roots,
                reporter,
            )
            .await?;
            warn_host_lifecycle_execution_once(reporter, &mut host_lifecycle_warned).await?;
            run_lifecycle_shell_command(
                &install_plan,
                launch_ctx,
                &install.command,
                "install",
                &root,
                &path_plan,
            )?;
        } else if provisioned_roots.insert(root.clone()) {
            let cmd_opt = match plan_v03_provision_command(&target_plan)? {
                Some(cmd) => Some(cmd),
                None => fallback_provision_command_from_manifest(
                    typed_manifest,
                    &root_target.label,
                    &root,
                )?,
            };
            if let Some(command) = cmd_opt {
                reporter
                    .notify(format!(
                        "⚙️  Provision [{}]: {}",
                        root_target.label, command
                    ))
                    .await?;
                let path_plan = build_lifecycle_phase_path_plan(
                    &target_plan,
                    LifecyclePhase::Install,
                    &command,
                    &lifecycle_roots,
                    reporter,
                )
                .await?;
                warn_host_lifecycle_execution_once(reporter, &mut host_lifecycle_warned).await?;
                run_lifecycle_shell_command(
                    &target_plan,
                    launch_ctx,
                    &command,
                    "provision",
                    &root,
                    &path_plan,
                )?;
            }
        }
    }

    for target in &lifecycle_targets {
        let target_plan = plan.with_selected_target(target.label.clone());
        if let Some(command) = target_plan
            .build_lifecycle_build()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            reporter
                .notify(format!("🏗️  Build [{}]: {}", target.label, command))
                .await?;
            let path_plan = build_lifecycle_phase_path_plan(
                &target_plan,
                LifecyclePhase::Build,
                &command,
                &lifecycle_roots,
                reporter,
            )
            .await?;
            warn_host_lifecycle_execution_once(reporter, &mut host_lifecycle_warned).await?;
            run_lifecycle_shell_command(
                &target_plan,
                launch_ctx,
                &command,
                "build",
                &target.working_dir,
                &path_plan,
            )?;
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct LifecycleTarget {
    label: String,
    working_dir: PathBuf,
    install: Option<String>,
}

#[derive(Debug, Clone)]
struct RootInstallCommand {
    label: String,
    command: String,
}

fn build_lifecycle_targets(
    plan: &capsule::router::ManifestData,
    target_labels: &[String],
) -> Result<Vec<LifecycleTarget>> {
    target_labels
        .iter()
        .map(|label| {
            let target_plan = plan.with_selected_target(label.clone());
            Ok(LifecycleTarget {
                label: label.clone(),
                working_dir: dependency_root(&target_plan),
                install: explicit_install_command_string(&target_plan)?,
            })
        })
        .collect()
}

fn root_order(targets: &[LifecycleTarget]) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let mut roots = Vec::new();
    for target in targets {
        if seen.insert(target.working_dir.clone()) {
            roots.push(target.working_dir.clone());
        }
    }
    roots
}

fn build_root_install_plan(
    targets: &[LifecycleTarget],
) -> Result<std::collections::HashMap<PathBuf, RootInstallCommand>> {
    let mut by_root = std::collections::HashMap::<PathBuf, RootInstallCommand>::new();
    for target in targets {
        let Some(command) = target.install.as_ref() else {
            continue;
        };
        if let Some(existing) = by_root.get(&target.working_dir) {
            if existing.command != *command {
                return Err(AtoExecutionError::execution_contract_invalid(
                    format!(
                        "conflicting install lifecycle commands for dependency root '{}': target '{}' declares '{}', target '{}' declares '{}'. Use one root-level install command for targets that share a dependency root.",
                        target.working_dir.display(),
                        existing.label,
                        existing.command,
                        target.label,
                        command
                    ),
                    Some("targets.<label>.install"),
                    Some(&target.label),
                )
                .into());
            }
            continue;
        }
        by_root.insert(
            target.working_dir.clone(),
            RootInstallCommand {
                label: target.label.clone(),
                command: command.clone(),
            },
        );
    }
    Ok(by_root)
}

pub(crate) fn explicit_install_command_string(
    plan: &capsule::router::ManifestData,
) -> Result<Option<String>> {
    let target_command =
        install_command_from_scope(&plan.manifest, &["targets", plan.selected_target_label()])?;
    let top_level_command = install_command_from_scope(&plan.manifest, &[])?;
    Ok(target_command.or(top_level_command))
}

fn install_command_from_scope(manifest: &toml::Value, path: &[&str]) -> Result<Option<String>> {
    let mut scope = manifest;
    for segment in path {
        let Some(next) = scope.get(*segment) else {
            return Ok(None);
        };
        scope = next;
    }

    let has_install = scope.get("install").is_some();
    let has_install_command = scope.get("install_command").is_some();
    if has_install && has_install_command {
        let field = if path.is_empty() {
            "install/install_command".to_string()
        } else {
            format!("{}.install/install_command", path.join("."))
        };
        return Err(AtoExecutionError::execution_contract_invalid(
            format!(
                "install and install_command are aliases and cannot both be declared in the same scope ({field})"
            ),
            Some(&field),
            path.last().copied(),
        )
        .into());
    }

    let value = scope
        .get("install")
        .or_else(|| scope.get("install_command"))
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    Ok(value)
}

/// Build the phase-aware PATH plan used by both consent preview and session start.
/// Install runs with managed toolchains only; build/run are allowed to see
/// dependency output bins materialized by the completed install phase.
async fn build_lifecycle_phase_path_plan(
    plan: &capsule::router::ManifestData,
    phase: LifecyclePhase,
    command: &str,
    lifecycle_roots: &[PathBuf],
    reporter: &Arc<CliReporter>,
) -> Result<LifecyclePathPlan> {
    let toolchains = materialize_lifecycle_toolchains(plan, command, reporter)?;
    build_lifecycle_path_plan(plan, phase, command, lifecycle_roots, toolchains, reporter).await
}

pub(super) fn plan_v03_provision_command(
    plan: &capsule::router::ManifestData,
) -> Result<Option<String>> {
    let runtime = plan.execution_runtime().unwrap_or_default();
    let driver = plan.execution_driver().unwrap_or_default();
    let runtime = runtime.trim().to_ascii_lowercase();
    let driver = driver.trim().to_ascii_lowercase();
    let manifest_dir = plan.manifest_dir.clone();
    let execution_working_directory = dependency_root(plan);

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
        let runtime_version = plan.execution_runtime_version();
        debug!(
            phase = "run",
            runtime,
            driver,
            manifest_dir = %manifest_dir.display(),
            execution_working_directory = %execution_working_directory.display(),
            runtime_version = ?runtime_version,
            "Provision command path diagnostics"
        );
        return provision_command_from_python_importer(
            &execution_working_directory,
            runtime_version.as_deref(),
        );
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

/// When `plan.with_selected_target(label)` can't surface the runtime/driver
/// (lock's `resolved_targets` not yet populated for sibling orchestration
/// service targets), reach into the typed manifest directly and dispatch to
/// the right importer. Mirrors `plan_v03_provision_command`'s
/// driver→importer table but keyed off the manifest's `[targets.<label>]`
/// entry instead of `plan.execution_runtime/driver/runtime_version`.
fn fallback_provision_command_from_manifest(
    manifest: Option<&capsule::types::CapsuleManifest>,
    target_label: &str,
    working_dir: &Path,
) -> Result<Option<String>> {
    let Some(manifest) = manifest else {
        return Ok(None);
    };
    let Some(targets) = manifest.targets.as_ref() else {
        return Ok(None);
    };
    let Some(target) = targets.named.get(target_label) else {
        return Ok(None);
    };
    let driver = target
        .driver
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let runtime = target.runtime.trim().to_ascii_lowercase();
    if runtime == "web" && driver == "static" {
        return Ok(None);
    }
    match driver.as_str() {
        "node" => provision_command_from_node_importer(working_dir),
        "python" => {
            provision_command_from_python_importer(working_dir, target.runtime_version.as_deref())
        }
        "native" => provision_command_from_cargo_importer(working_dir),
        _ => Ok(None),
    }
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
    runtime_version: Option<&str>,
) -> Result<Option<String>> {
    let uv_lock = execution_working_directory.join("uv.lock");
    let pyproject = execution_working_directory.join("pyproject.toml");
    if pyproject.exists() {
        return if uv_lock.exists() {
            Ok(Some("uv sync --frozen".to_string()))
        } else {
            Err(AtoExecutionError::lock_incomplete(
                "source/python target has pyproject.toml but is missing uv.lock for fail-closed provisioning",
                Some("uv.lock"),
            )
            .into())
        };
    }

    if let Some(requirements_path) = resolve_python_requirements_path(execution_working_directory) {
        // Prefer a committed pip-compile lock when one is present (e.g. a
        // GitHub install repaired via `--auto-fix:all`). Local developer runs
        // of a requirements-only project that has no lockfile yet must keep
        // working, so fall back to the historical `uv pip install` path rather
        // than failing closed — tightening that is out of scope here.
        if uv_lock.exists() {
            return Ok(Some(python_requirements_lock_sync_command(runtime_version)));
        }
        let python_pin = runtime_version
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| format!(" --python {value}"))
            .unwrap_or_default();
        let requirements_arg = requirements_path
            .strip_prefix(execution_working_directory)
            .unwrap_or(requirements_path.as_path())
            .to_string_lossy()
            .replace('\\', "/");
        return Ok(Some(format!(
            // setuptools>=72 dropped `pkg_resources`; pin <72 so legacy packages
            // (e.g. gunicorn<21) that import pkg_resources still work.
            // Apps that explicitly pin setuptools>=72 in requirements.txt will
            // get a clear uv conflict error and can remove the implicit constraint.
            //
            // Quote the constraint with double quotes, not single quotes: cmd.exe
            // does not treat `'` as a quote character, so `'setuptools<72'` leaves
            // the `<` exposed and cmd parses it as input redirection (issue #629,
            // Windows-only "Access is denied." / "The system cannot find the file
            // specified."). Double quotes are honoured by both POSIX `sh` and
            // cmd.exe, protecting the `<`. The Windows lifecycle runner passes the
            // command through `cmd /D /S /C` with `raw_arg` so the inner quotes
            // reach cmd verbatim instead of being escaped by Rust's argument quoting.
            "uv venv{python_pin} --seed --clear && uv pip install -r {requirements_arg} \"setuptools<72\""
        )));
    }

    match probe_required_python_lockfile(execution_working_directory)? {
        ProbeResult::Found(_) => {
            // `uv sync --frozen` honours pyproject.toml's requires-python, so
            // we don't inject --python here unless a pin is explicitly set.
            // When the manifest pins a version, surface it via UV_PYTHON via
            // the env, but for now lean on uv.lock metadata.
            Ok(Some("uv sync --frozen".to_string()))
        }
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
            ));
        }
    };
    Ok(command.to_string())
}

/// Emit a single host-execution security warning the first time a schema-0.3
/// lifecycle command (install/provision/build) is about to run. These commands
/// execute the capsule's own scripts directly on the host with no network or
/// filesystem sandbox (and, since the PR2 fix, without this capsule's secrets),
/// so they are only safe for code the user trusts. `warned` is flipped so the
/// warning fires at most once per `ato run`.
async fn warn_host_lifecycle_execution_once(
    reporter: &Arc<CliReporter>,
    warned: &mut bool,
) -> Result<()> {
    if *warned {
        return Ok(());
    }
    *warned = true;
    reporter
        .notify(
            "⚠️  Build and install steps run directly on your host without a network \
             or filesystem sandbox, and without this capsule's secrets. Only run \
             capsules whose source you trust."
                .to_string(),
        )
        .await?;
    Ok(())
}

fn run_lifecycle_shell_command(
    plan: &capsule::router::ManifestData,
    launch_ctx: &crate::executors::launch_context::RuntimeLaunchContext,
    command: &str,
    phase: &str,
    working_dir: &Path,
    path_plan: &LifecyclePathPlan,
) -> Result<()> {
    // `cmd.exe /D /S /C "<command>"` on Windows, `sh -c <command>` elsewhere;
    // never whitespace-split, so `&&` chains and quoting survive intact. The
    // Windows path uses `/S` + a single outer-quoted payload via `raw_arg` so a
    // command carrying its own double quotes (the Python provision command
    // quotes `"setuptools<72"` to protect the `<` from cmd input-redirection,
    // issue #629) reaches the tool verbatim instead of being re-escaped by
    // Rust's argument quoter. See `host_shell::windows_cmd_shell_command`.
    let mut cmd = crate::common::host_shell::lifecycle_shell_command(command);

    // Ato canonicalizes workspace paths internally, which on Windows yields
    // `\\?\C:\…` extended-length forms that cmd.exe/uv/npm/pnpm mis-handle.
    // Children always get the normal spelling.
    let child_cwd = capsule::common::paths::windows_child_compatible_path(working_dir);

    cmd.current_dir(&child_cwd)
        .stdin(std::process::Stdio::null())
        .env("COREPACK_ENABLE_STRICT", "0")
        // Disable pnpm 10's auto-manage-package-manager-versions to prevent it from
        // attempting to download the pinned pnpm version in offline/CI environments.
        .env("npm_config_manage_package_manager_versions", "false")
        .env("npm_config_approve_builds", "on")
        // Skip git-hooks managers (husky, lefthook, etc.): the capsule workspace
        // has no .git dir so their prepare/postinstall scripts would fail with exit 128.
        .env("HUSKY", "0")
        .env("LEFTHOOK", "0")
        .env("PATH", path_plan.path_env()?);

    for (key, value) in runtime_overrides::merged_env(plan.execution_env()) {
        cmd.env(key, value);
    }
    // SECURITY: the build/prepare/install lifecycle runs the capsule's own
    // (potentially untrusted, e.g. `ato run github.com/...`) install/postinstall
    // and build scripts directly on the host with no network or filesystem
    // sandbox. Apply only the non-secret env here — capsule secrets (`secret.*` /
    // sensitive `env.*` grants) must NOT be exposed to those scripts. Secrets stay
    // scoped to the Execute/run spawn boundary (`apply_allowlisted_env`). Full
    // sandboxing of this phase is the proper follow-up fix.
    launch_ctx.apply_non_secret_env(&mut cmd)?;

    let output = crate::common::host_shell::run_streaming_with_tails(&mut cmd)
        .with_context(|| format!("Failed to execute {} command", phase))?;
    if output.status.success() {
        return Ok(());
    }

    let render_tail = |tail: &str| {
        if tail.trim().is_empty() {
            "(empty)".to_string()
        } else {
            tail.to_string()
        }
    };
    Err(anyhow::anyhow!(
        "lifecycle_command_failed: phase={phase} target={target} exit_code={exit_code}\n\
         command: {command}\n\
         cwd: {cwd}\n\
         stderr_tail:\n{stderr_tail}\n\
         stdout_tail:\n{stdout_tail}",
        target = plan.selected_target_label(),
        exit_code = output.status.code().unwrap_or(1),
        cwd = child_cwd.display(),
        stderr_tail = render_tail(&output.stderr_tail),
        stdout_tail = render_tail(&output.stdout_tail),
    ))
}

fn preflight_macos_compat(plan: &capsule::router::ManifestData) -> Result<()> {
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

fn preflight_python_uv_lock_for_source_driver(plan: &capsule::router::ManifestData) -> Result<()> {
    if !is_python_source_target(plan) {
        return Ok(());
    }

    // Probe the same root the provision step will `cd` into. Reaching directly
    // into `plan.manifest_dir` worked only because the legacy probe helpers
    // dual-checked `<root>` and `<root>/source/`; now that callers must pass
    // the resolved dependency root explicitly, this preflight gate stays in
    // sync with `plan_v03_provision_command` instead of relying on a layout
    // accident.
    let dep_root = dependency_root(plan);

    // A requirements-only project is provisionable on the local run path even
    // without a lockfile (`provision_command_from_python_importer` falls back to
    // `uv pip install`). The GitHub install/build path enforces fail-closed
    // lockfiles separately; do not tighten the local `ato run .` gate here.
    if resolve_python_requirements_path(&dep_root).is_some() {
        return Ok(());
    }

    match probe_required_python_lockfile(&dep_root)? {
        ProbeResult::Found(_) => return Ok(()),
        ProbeResult::Missing(_) | ProbeResult::NotApplicable => {}
        ProbeResult::Ambiguous(ambiguity) => {
            return Err(
                AtoExecutionError::lock_incomplete(ambiguity.message, Some("uv.lock")).into(),
            );
        }
    }

    Err(AtoExecutionError::lock_incomplete(
        "source/python target requires uv.lock or requirements.txt for fail-closed provisioning",
        Some("uv.lock"),
    )
    .into())
}

fn preflight_python_uv_binary_for_source_driver(
    plan: &capsule::router::ManifestData,
    authoritative_lock: Option<&capsule::ato_lock::AtoLock>,
) -> Result<()> {
    if !is_python_source_target(plan) {
        return Ok(());
    }

    let dep_root = dependency_root(plan);
    if resolve_python_requirements_path(&dep_root).is_some() {
        // A requirements.txt is present. Prefer a hermetic uv from
        // capsule.lock.json (tools.uv) when one is available — a capsule that
        // ships pyproject.toml + uv.lock + a (vestigial) requirements.txt
        // resolves deps from the lock and needs no system uv at run time.
        // Only fall back to requiring `uv` on PATH for a requirements.txt-only
        // capsule with no hermetic uv. (Previously this always demanded system
        // uv on PATH → E104 on managed/Connected-Runner hosts that have none.)
        if runtime_manager::ensure_uv_binary_with_authority(plan, authoritative_lock).is_ok() {
            return Ok(());
        }
        return which::which("uv").map(|_| ()).map_err(|_| {
            AtoExecutionError::lock_incomplete(
                "source/python target requires uv on PATH (or a hermetic uv in \
                 capsule.lock.json) when using requirements.txt",
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

fn is_python_source_target(plan: &capsule::router::ManifestData) -> bool {
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
    plan: &capsule::router::ManifestData,
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
    if let Ok(lockfile) = typed.as_ref()
        && let Some(required) = lockfile
            .targets
            .values()
            .find_map(|target| target.constraints.as_ref().and_then(|c| c.glibc.clone()))
    {
        return Ok(Some(required));
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
    plan: &capsule::router::ManifestData,
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
    plan: &capsule::router::ManifestData,
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
                if let SingleArch::MachO(binary) = entry
                    && let Some(ver) = extract_min_macos_from_macho(&binary)
                {
                    update_best(ver);
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

    if out.is_empty() { None } else { Some(out) }
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
fn resolve_uv_lock_path(dependency_root: &Path) -> Option<PathBuf> {
    match probe_required_python_lockfile(dependency_root).ok()? {
        ProbeResult::Found(values) => values.first().map(|value| value.primary_path.clone()),
        _ => None,
    }
}

/// Probe the dependency root for a Python `requirements.txt`.
///
/// Callers must pass an already-resolved root (`dependency_root(plan)` or
/// equivalent). Earlier this helper silently dual-probed `<root>` and
/// `<root>/source/`, which papered over wrong roots; that paper-over has
/// been removed so caller mistakes surface as missing-lock errors instead
/// of being absorbed.
fn resolve_python_requirements_path(dependency_root: &Path) -> Option<PathBuf> {
    let path = dependency_root.join("requirements.txt");
    path.exists().then_some(path)
}

#[cfg(test)]
pub(super) fn resolve_python_dependency_lock_path(dependency_root: &Path) -> Option<PathBuf> {
    resolve_uv_lock_path(dependency_root)
        .or_else(|| resolve_python_requirements_path(dependency_root))
}

#[cfg(test)]
mod tests {
    use super::{
        build_lifecycle_targets, build_root_install_plan, detect_required_glibc_from_lock,
        install_command_from_scope, plan_v03_provision_command, preflight_glibc_compat,
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
            validation_mode: capsule::types::ValidationMode::Strict,
            engine_override_declared: false,
            compatibility_legacy_lock: None,
            install_profile_key: None,
        }
    }

    fn build_plan(manifest_dir: &Path, manifest: &str) -> capsule::router::ManifestData {
        capsule::router::execution_descriptor_from_manifest_parts(
            toml::from_str::<toml::Value>(manifest).expect("parse manifest"),
            manifest_dir.join("capsule.toml"),
            manifest_dir.to_path_buf(),
            capsule::router::ExecutionProfile::Dev,
            Some("default"),
            std::collections::HashMap::new(),
        )
        .expect("execution descriptor")
    }

    // Tests for the dependency-root resolver (formerly named
    // `resolve_provision_working_dir` here, then duplicated into
    // `shadow::relative_working_dir_from_manifest_root`) live with the
    // unified resolver itself in `provisioning/dependency_root.rs`.
    // Removing the duplicates here keeps the fixture-heavy filesystem
    // tests in one place and prevents the two suites from drifting.

    #[test]
    fn same_root_identical_install_is_deduped() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("package.json"), "{}\n").expect("write package");
        fs::write(dir.path().join("bun.lock"), "# lock\n").expect("write lock");
        let plan = build_plan(
            dir.path(),
            r#"
name = "demo"
type = "app"
default_target = "default"

[targets.default]
runtime = "source"
driver = "node"
install = "bun install --ignore-scripts"
run_command = "bun run start"
package_dependencies = ["worker"]

[targets.worker]
runtime = "source"
driver = "node"
install = "bun install --ignore-scripts"
run_command = "bun run worker"
"#,
        );
        let targets =
            build_lifecycle_targets(&plan, &["default".to_string(), "worker".to_string()])
                .expect("lifecycle targets");
        let root_plan = build_root_install_plan(&targets).expect("root install plan");

        assert_eq!(root_plan.len(), 1);
        let command = root_plan
            .values()
            .next()
            .expect("root install")
            .command
            .as_str();
        assert_eq!(command, "bun install --ignore-scripts");
        assert_eq!(
            plan_v03_provision_command(&plan)
                .expect("auto provision command")
                .as_deref(),
            Some("bun install")
        );
    }

    #[test]
    fn same_root_conflicting_install_is_rejected() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("package.json"), "{}\n").expect("write package");
        let plan = build_plan(
            dir.path(),
            r#"
name = "demo"
type = "app"
default_target = "default"

[targets.default]
runtime = "source"
driver = "node"
install = "bun install --ignore-scripts"
run_command = "bun run start"
package_dependencies = ["worker"]

[targets.worker]
runtime = "source"
driver = "node"
install = "pnpm install"
run_command = "pnpm worker"
"#,
        );
        let targets =
            build_lifecycle_targets(&plan, &["default".to_string(), "worker".to_string()])
                .expect("lifecycle targets");
        let err = build_root_install_plan(&targets).expect_err("conflicting install should fail");

        assert!(
            err.to_string()
                .contains("conflicting install lifecycle commands")
        );
    }

    /// `install` and `install_command` are TOML-level aliases for the same
    /// concept; declaring both in the same scope must be rejected with a
    /// targeted error rather than the generic "not a valid target table"
    /// surface that the typed `NamedTarget` deserializer produces (the
    /// `#[serde(alias = "install")]` attribute on `NamedTarget::install_command`
    /// makes the typed parse reject the duplicate before reaching the
    /// production check we want to pin here).
    ///
    /// We therefore exercise `install_command_from_scope` directly with a
    /// raw `toml::Value`, which is what `explicit_install_command_string`
    /// itself dispatches to once a `ManifestData` is available. This keeps
    /// the test honest about which layer owns the alias-conflict diagnostic.
    #[test]
    fn install_aliases_in_same_scope_are_rejected() {
        let mut target_table = toml::map::Map::new();
        target_table.insert(
            "runtime".to_string(),
            toml::Value::String("source".to_string()),
        );
        target_table.insert(
            "driver".to_string(),
            toml::Value::String("node".to_string()),
        );
        target_table.insert(
            "install".to_string(),
            toml::Value::String("bun install --ignore-scripts".to_string()),
        );
        target_table.insert(
            "install_command".to_string(),
            toml::Value::String("bun install".to_string()),
        );
        target_table.insert(
            "run_command".to_string(),
            toml::Value::String("bun run start".to_string()),
        );

        let mut targets = toml::map::Map::new();
        targets.insert("default".to_string(), toml::Value::Table(target_table));

        let mut manifest = toml::map::Map::new();
        manifest.insert("name".to_string(), toml::Value::String("demo".to_string()));
        manifest.insert("type".to_string(), toml::Value::String("app".to_string()));
        manifest.insert(
            "default_target".to_string(),
            toml::Value::String("default".to_string()),
        );
        manifest.insert("targets".to_string(), toml::Value::Table(targets));
        let manifest = toml::Value::Table(manifest);

        let err = install_command_from_scope(&manifest, &["targets", "default"])
            .expect_err("alias conflict in same scope should fail");

        assert!(
            err.to_string()
                .contains("install and install_command are aliases"),
            "alias conflict diagnostic must mention aliases, got: {err}"
        );
    }

    #[test]
    fn provision_command_uses_explicit_python_working_dir_requirements() {
        let dir = tempdir().expect("tempdir");
        let backend = dir.path().join("backend");
        fs::create_dir_all(&backend).expect("create backend");
        fs::write(backend.join("requirements.txt"), "fastapi==0.115.6\n")
            .expect("write requirements");
        fs::write(backend.join("serve.py"), "print('ok')\n").expect("write serve");
        let plan = build_plan(
            dir.path(),
            r#"
name = "demo"
type = "app"
default_target = "default"

[targets.default]
runtime = "source"
driver = "python"
runtime_version = "3.11.10"
working_dir = "backend"
run_command = "serve.py"
"#,
        );

        let command = plan_v03_provision_command(&plan)
            .expect("provision command")
            .expect("python requirements should provision");

        assert_eq!(
            command,
            "uv venv --python 3.11.10 --seed --clear && uv pip install -r requirements.txt \"setuptools<72\""
        );
    }

    #[test]
    fn provision_command_omits_python_pin_when_runtime_version_unset() {
        // Local `ato run .` of a requirements-only project with no lockfile must
        // keep provisioning via the `uv pip install` fallback. Fail-closed
        // lockfile enforcement lives on the GitHub install/build path, not here.
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("requirements.txt"), "fastapi==0.115.6\n")
            .expect("write requirements");
        let plan = build_plan(
            dir.path(),
            r#"
name = "demo"
type = "app"
default_target = "default"

[targets.default]
runtime = "source"
driver = "python"
run_command = "main.py"
"#,
        );

        let command = plan_v03_provision_command(&plan)
            .expect("provision command")
            .expect("python requirements should provision");

        assert_eq!(
            command,
            "uv venv --seed --clear && uv pip install -r requirements.txt \"setuptools<72\""
        );
    }

    #[test]
    fn python_provision_command_uses_cmd_safe_quoting_for_setuptools_constraint() {
        // Regression for issue #629: the setuptools upper-bound must be quoted
        // with double quotes, never POSIX single quotes. cmd.exe does not treat
        // `'` as a quote, so `'setuptools<72'` leaves the `<` exposed and cmd
        // parses it as input redirection ("The system cannot find the file
        // specified." / "Access is denied." on windows/x86_64). Double quotes are
        // honoured by both POSIX `sh` and cmd.exe.
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("requirements.txt"), "fastapi==0.115.6\n")
            .expect("write requirements");
        let plan = build_plan(
            dir.path(),
            r#"
name = "demo"
type = "app"
default_target = "default"

[targets.default]
runtime = "source"
driver = "python"
run_command = "main.py"
"#,
        );

        let command = plan_v03_provision_command(&plan)
            .expect("provision command")
            .expect("python requirements should provision");

        assert!(
            command.contains("\"setuptools<72\""),
            "constraint must be double-quoted for cmd.exe safety, got: {command}"
        );
        assert!(
            !command.contains("'setuptools<72'"),
            "POSIX single quotes are not cmd.exe-safe and must not be emitted, got: {command}"
        );
    }

    // The `/D /S /C "<command>"` raw-arg quoting contract (issue #629) is owned
    // and unit-tested by `host_shell::windows_cmd_shell_command`, which this
    // module's lifecycle runner calls via `lifecycle_shell_command`.

    #[test]
    fn provision_command_clears_existing_python_venv_before_install() {
        let dir = tempdir().expect("tempdir");
        fs::write(dir.path().join("requirements.txt"), "fastapi==0.115.6\n")
            .expect("write requirements");
        fs::write(dir.path().join("uv.lock"), "# pip-compile lock\n").expect("write lock");
        let plan = build_plan(
            dir.path(),
            r#"
name = "demo"
type = "app"
default_target = "default"

[targets.default]
runtime = "source"
driver = "python"
runtime_version = "3.11.10"
run_command = "main.py"
"#,
        );

        let command = plan_v03_provision_command(&plan)
            .expect("provision command")
            .expect("python requirements should provision");

        assert!(
            command.starts_with("uv venv --python 3.11.10 --seed --clear &&"),
            "command={command}"
        );
        assert!(command.contains("uv pip sync uv.lock"), "command={command}");
    }

    #[test]
    fn provision_command_prefers_pyproject_uv_lock_over_requirements_lock() {
        let dir = tempdir().expect("tempdir");
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .expect("write pyproject");
        fs::write(dir.path().join("requirements.txt"), "fastapi==0.115.6\n")
            .expect("write requirements");
        fs::write(dir.path().join("uv.lock"), "version = 1\n").expect("write lock");
        let plan = build_plan(
            dir.path(),
            r#"
name = "demo"
type = "app"
default_target = "default"

[targets.default]
runtime = "source"
driver = "python"
run_command = "main.py"
"#,
        );

        let command = plan_v03_provision_command(&plan)
            .expect("provision command")
            .expect("python project lock should provision");

        assert_eq!(command, "uv sync --frozen");
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

        assert!(
            err.to_string()
                .contains("requires an anchored source entrypoint layout")
        );
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
