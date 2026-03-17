use anyhow::{Context, Result};
use ctrlc;
use goblin::elf::dynamic::DT_VERNEED;
use goblin::elf::Elf;
use goblin::mach::load_command::CommandVariant;
use goblin::mach::{Mach, SingleArch};
use regex::Regex;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Once};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};
use tracing::debug;

use crate::executors::source::ExecuteMode;
use crate::executors::source::NacelleExecEvent;
use crate::executors::target_runner::{self, TargetLaunchOptions};
use crate::reporters::CliReporter;
use crate::runtime_manager;
use crate::runtime_overrides;
use crate::runtime_tree;
use crate::state::{
    ensure_registered_state_binding, parse_state_reference, resolve_registered_state_reference,
};
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::execution_plan::guard::ExecutorKind;
use capsule_core::lockfile::{
    lockfile_output_path, manifest_external_capsule_dependencies, parse_lockfile_text,
    resolve_existing_lockfile_path, verify_lockfile_external_dependencies, CAPSULE_LOCK_FILE_NAME,
    LEGACY_CAPSULE_LOCK_FILE_NAME,
};
use capsule_core::types::{CapsuleManifest, CapsuleType, StateDurability};
use capsule_core::{router, CapsuleReporter};

mod watch;

const BACKGROUND_READY_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const BACKGROUND_READY_WAIT_TIMEOUT_ENV: &str = "ATO_BACKGROUND_READY_WAIT_TIMEOUT_SECS";

pub struct OpenArgs {
    pub target: PathBuf,
    pub target_label: Option<String>,
    pub watch: bool,
    pub background: bool,
    pub nacelle: Option<PathBuf>,
    pub enforcement: String,
    pub sandbox_mode: bool,
    pub dangerously_skip_permissions: bool,
    pub assume_yes: bool,
    pub state_bindings: Vec<String>,
    pub inject_bindings: Vec<String>,
    pub reporter: Arc<CliReporter>,
}

pub async fn execute(args: OpenArgs) -> Result<()> {
    let target = args.target.clone();
    let target_is_manifest_file =
        target.is_file() && target.file_name().and_then(|n| n.to_str()) == Some("capsule.toml");
    let target_is_manifest_dir = target.is_dir() && target.join("capsule.toml").exists();

    if target
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        == Some("capsule".to_string())
    {
        execute_capsule_file(&args, &target).await
    } else if target_is_manifest_dir || target_is_manifest_file {
        if args.watch {
            execute_watch_mode(args)
        } else {
            execute_normal_mode(args).await
        }
    } else {
        anyhow::bail!(
            "Target is not a valid capsule: {} (expected .capsule file or directory with capsule.toml)",
            target.display()
        );
    }
}

async fn execute_capsule_file(args: &OpenArgs, capsule_path: &PathBuf) -> Result<()> {
    if let Some(manifest_path) = runtime_tree::prepare_store_runtime_for_capsule(capsule_path)? {
        debug!(
            manifest_path = %manifest_path.display(),
            "Running capsule from isolated runtime tree"
        );
        let open_args = OpenArgs {
            target: manifest_path,
            target_label: args.target_label.clone(),
            watch: args.watch,
            background: args.background,
            nacelle: args.nacelle.clone(),
            enforcement: args.enforcement.clone(),
            sandbox_mode: args.sandbox_mode,
            dangerously_skip_permissions: args.dangerously_skip_permissions,
            assume_yes: args.assume_yes,
            state_bindings: args.state_bindings.clone(),
            inject_bindings: args.inject_bindings.clone(),
            reporter: args.reporter.clone(),
        };
        return execute_normal_mode(open_args).await;
    }

    debug!(capsule = %capsule_path.display(), "Extracting capsule archive");

    let extract_dir = capsule_path
        .parent()
        .map(|p| {
            p.join(format!(
                "{}-extracted",
                capsule_path.file_stem().unwrap().to_string_lossy()
            ))
        })
        .context("Failed to determine extraction directory")?;

    if extract_dir.exists() {
        debug!(
            extract_dir = %extract_dir.display(),
            "Removing existing extracted directory before extraction"
        );
        fs::remove_dir_all(&extract_dir)?;
    }

    fs::create_dir_all(&extract_dir).with_context(|| {
        format!(
            "Failed to create extraction directory: {}",
            extract_dir.display()
        )
    })?;

    let mut archive = fs::File::open(capsule_path)
        .with_context(|| format!("Failed to open capsule file: {}", capsule_path.display()))?;

    let mut ar = tar::Archive::new(&mut archive);
    ar.unpack(&extract_dir)
        .with_context(|| format!("Failed to extract capsule to: {}", extract_dir.display()))?;

    debug!(extract_dir = %extract_dir.display(), "Capsule extracted");

    let cas_provider = capsule_core::capsule_v3::CasProvider::from_env();
    let payload_outcome = capsule_core::capsule_v3::unpack_payload_from_capsule_root_with_provider(
        &extract_dir,
        &extract_dir,
        &cas_provider,
    )
    .with_context(|| "Failed to extract payload from capsule root (v2/v3)")?;
    match payload_outcome {
        capsule_core::capsule_v3::PayloadUnpackOutcome::RestoredFromV3
        | capsule_core::capsule_v3::PayloadUnpackOutcome::RestoredFromV2 => {}
        capsule_core::capsule_v3::PayloadUnpackOutcome::RestoredFromV2DueToCasDisabled(reason) => {
            emit_open_cas_disabled_warning_once(&reason);
        }
        capsule_core::capsule_v3::PayloadUnpackOutcome::RestoredFromV2DueToV3Error(err) => {
            emit_open_v3_fallback_warning_once(&err);
        }
    }
    fs::remove_file(extract_dir.join("payload.tar.zst")).ok();
    fs::remove_file(extract_dir.join("payload.tar")).ok();
    debug!("Payload extracted");

    let manifest_path = extract_dir.join("capsule.toml");
    if !manifest_path.exists() {
        anyhow::bail!("Extracted capsule does not contain capsule.toml");
    }

    let original_dir = capsule_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty() && *parent != std::path::Path::new("."))
        .map(std::path::Path::to_path_buf)
        .unwrap_or(std::env::current_dir().context("Failed to get current directory")?);

    let has_source_files = check_has_source_files(&extract_dir);
    let original_has_source = check_has_source_files(&original_dir);

    if !has_source_files && original_has_source {
        debug!("Copying source files to extracted directory");

        copy_source_files(&original_dir, &extract_dir, &args.reporter).await?;

        debug!("Source files copied");
    }

    debug!(extract_dir = %extract_dir.display(), "Running extracted capsule");

    let open_args = OpenArgs {
        target: manifest_path,
        target_label: args.target_label.clone(),
        watch: args.watch,
        background: args.background,
        nacelle: args.nacelle.clone(),
        enforcement: args.enforcement.clone(),
        sandbox_mode: args.sandbox_mode,
        dangerously_skip_permissions: args.dangerously_skip_permissions,
        assume_yes: args.assume_yes,
        state_bindings: args.state_bindings.clone(),
        inject_bindings: args.inject_bindings.clone(),
        reporter: args.reporter.clone(),
    };

    execute_normal_mode(open_args).await
}

fn emit_open_cas_disabled_warning_once(reason: &capsule_core::capsule_v3::CasDisableReason) {
    static STDERR_WARN_ONCE: Once = Once::new();
    STDERR_WARN_ONCE.call_once(|| {
        eprintln!(
            "⚠️  Performance warning: CAS is disabled (reason: {}). Falling back to v2 legacy mode.",
            reason
        );
    });
}

fn emit_open_v3_fallback_warning_once(error_message: &str) {
    static STDERR_WARN_ONCE: Once = Once::new();
    STDERR_WARN_ONCE.call_once(|| {
        eprintln!(
            "⚠️  Performance warning: v3 payload reconstruction failed ({}). Falling back to v2 legacy mode.",
            error_message
        );
    });
}

async fn copy_source_files(
    original_dir: &Path,
    extract_dir: &Path,
    _reporter: &Arc<CliReporter>,
) -> Result<()> {
    let entries = fs::read_dir(original_dir).with_context(|| {
        format!(
            "Failed to read original directory: {}",
            original_dir.display()
        )
    })?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();

        if path == extract_dir || path.starts_with(extract_dir) {
            continue;
        }

        if file_name == "capsule.toml"
            || file_name == CAPSULE_LOCK_FILE_NAME
            || file_name == LEGACY_CAPSULE_LOCK_FILE_NAME
            || file_name == "config.json"
        {
            continue;
        }

        if path.is_dir() && file_name.to_string_lossy().ends_with("-extracted") {
            continue;
        }

        if path.is_file() {
            let should_skip_artifact = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| {
                    matches!(
                        ext.to_ascii_lowercase().as_str(),
                        "capsule" | "sig" | "bundle" | "zst" | "tar"
                    )
                })
                .unwrap_or(false);
            if should_skip_artifact {
                continue;
            }
        }

        if file_name == "source" && path.is_dir() {
            let dest = extract_dir.join("source");
            copy_dir_recursive(&path, &dest)?;
            debug!("Copied source/");
        } else if path.is_file() {
            let dest = extract_dir.join(&file_name);
            fs::copy(&path, &dest)?;
            debug!(file = %file_name.to_string_lossy(), "Copied file into extracted capsule");
        } else if path.is_dir() && !is_hidden(&file_name) {
            let dest = extract_dir.join(&file_name);
            copy_dir_recursive(&path, &dest)?;
            debug!(dir = %file_name.to_string_lossy(), "Copied directory into extracted capsule");
        }
    }

    Ok(())
}

fn check_has_source_files(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };

    let mut file_count = 0usize;
    let mut has_actual_source_files = false;

    for entry in entries.flatten() {
        file_count += 1;
        let file_name = entry.file_name();
        let path = entry.path();

        if file_name == "capsule.toml"
            || file_name == CAPSULE_LOCK_FILE_NAME
            || file_name == LEGACY_CAPSULE_LOCK_FILE_NAME
            || file_name == "config.json"
            || file_name == "signature.json"
        {
            continue;
        }

        if path.is_file() {
            let name = file_name.to_string_lossy();
            if name == "package.json"
                || name == "pyproject.toml"
                || name == "requirements.txt"
                || name == "go.mod"
                || name == "Cargo.toml"
            {
                return true;
            }
            if is_source_file(&file_name) {
                return true;
            }
            has_actual_source_files = true;
        }

        if path.is_dir() && !is_hidden(&file_name) {
            if file_name == "source"
                && fs::read_dir(&path)
                    .ok()
                    .and_then(|mut it| it.next())
                    .is_some()
            {
                return true;
            }

            if path.join("package.json").exists()
                || path.join("pyproject.toml").exists()
                || path.join("index.js").exists()
                || path.join("main.py").exists()
            {
                return true;
            }
        }
    }

    has_actual_source_files || (file_count > 5)
}

fn is_source_file(file_name: &std::ffi::OsString) -> bool {
    let exts = [
        "js", "ts", "py", "go", "rs", "json", "html", "css", "mjs", "cjs",
    ];
    if let Some(ext) = file_name.to_str().and_then(|s| s.rsplit('.').next()) {
        exts.contains(&ext)
    } else {
        false
    }
}

fn is_hidden(file_name: &std::ffi::OsString) -> bool {
    let bytes = file_name.as_os_str().as_encoded_bytes();
    bytes.first() == Some(&b'.') && bytes.len() > 1
}

fn copy_dir_recursive(from: &Path, to: &Path) -> Result<()> {
    if !to.exists() {
        fs::create_dir_all(to)?;
    }

    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let path = entry.path();
        let dest = to.join(entry.file_name());

        if path.is_dir() {
            copy_dir_recursive(&path, &dest)?;
        } else {
            fs::copy(&path, &dest)?;
        }
    }

    Ok(())
}

async fn execute_normal_mode(args: OpenArgs) -> Result<()> {
    let manifest_path = if args.target.is_dir() {
        args.target.join("capsule.toml")
    } else {
        args.target.clone()
    };

    let manifest = CapsuleManifest::load_from_file(&manifest_path)?;
    if manifest.schema_version.trim() == "0.3" && manifest.capsule_type == CapsuleType::Library {
        anyhow::bail!("schema_version=0.3 type=library package cannot be started with `ato run`");
    }
    let state_source_overrides = resolve_state_source_overrides(&manifest, &args.state_bindings)?;
    let decision = capsule_core::router::route_manifest_with_state_overrides(
        &manifest_path,
        router::ExecutionProfile::Dev,
        args.target_label.as_deref(),
        state_source_overrides,
    )?;
    if decision
        .plan
        .execution_package_type()
        .is_some_and(|value| value.eq_ignore_ascii_case("library"))
    {
        anyhow::bail!(
            "schema_version=0.3 type=library package '{}' cannot be started with `ato run`",
            decision.plan.selected_target_label()
        );
    }
    let external_dependencies = manifest_external_capsule_dependencies(&decision.plan.manifest)?;
    let mut external_capsules = None;
    if !external_dependencies.is_empty() {
        if args.background {
            anyhow::bail!("external capsule dependencies do not support --background yet");
        }
        let lockfile_path = manifest_path
            .parent()
            .and_then(resolve_existing_lockfile_path)
            .ok_or_else(|| {
                AtoExecutionError::lock_incomplete(
                    "external capsule dependencies require capsule.lock.json",
                    Some(CAPSULE_LOCK_FILE_NAME),
                )
            })?;
        let raw = std::fs::read_to_string(&lockfile_path)
            .with_context(|| format!("Failed to read {}", lockfile_path.display()))?;
        let lockfile = parse_lockfile_text(&raw, &lockfile_path)?;
        verify_lockfile_external_dependencies(&decision.plan.manifest, &lockfile)?;
        external_capsules = Some(
            crate::external_capsule::start_external_capsules(
                &decision.plan,
                &lockfile,
                &args.inject_bindings,
                args.reporter.clone(),
                &crate::external_capsule::ExternalCapsuleOptions {
                    enforcement: args.enforcement.clone(),
                    sandbox_mode: args.sandbox_mode,
                    dangerously_skip_permissions: args.dangerously_skip_permissions,
                    assume_yes: args.assume_yes,
                },
            )
            .await?,
        );
    }
    let injected_data =
        crate::data_injection::resolve_and_record(&decision.plan, &args.inject_bindings).await?;
    let mut merged_injected_env = injected_data.env;
    if let Some(external_capsules) = external_capsules.as_ref() {
        merged_injected_env.extend(external_capsules.caller_env().clone());
    }
    let launch_ctx = target_runner::resolve_launch_context(&decision.plan, &args.reporter)
        .await?
        .with_injected_env(merged_injected_env)
        .with_injected_mounts(injected_data.mounts);

    if decision.plan.is_orchestration_mode() {
        if args.background {
            anyhow::bail!("--background is not supported for orchestration mode");
        }

        let exit = crate::executors::orchestrator::execute(
            &decision.plan,
            args.reporter.clone(),
            &launch_ctx,
            crate::executors::orchestrator::OrchestratorOptions {
                enforcement: args.enforcement.clone(),
                sandbox_mode: args.sandbox_mode,
                dangerously_skip_permissions: args.dangerously_skip_permissions,
                assume_yes: args.assume_yes,
                nacelle: args.nacelle.clone(),
            },
        )
        .await?;
        if exit != 0 {
            if let Some(external_capsules) = external_capsules.as_mut() {
                external_capsules.shutdown_now();
            }
            std::process::exit(exit);
        }
        return Ok(());
    }

    if matches!(decision.kind, capsule_core::router::RuntimeKind::Oci) {
        if args.background {
            anyhow::bail!("--background is not supported for runtime=oci");
        }

        target_runner::preflight_required_environment_variables(&decision.plan, &launch_ctx)?;
        let exit =
            crate::executors::oci::execute(&decision.plan, args.reporter.clone(), &launch_ctx)
                .await?;
        if exit != 0 {
            if let Some(external_capsules) = external_capsules.as_mut() {
                external_capsules.shutdown_now();
            }
            std::process::exit(exit);
        }
        return Ok(());
    }

    let prepared = target_runner::prepare_target_execution(
        &decision.plan,
        launch_ctx.clone(),
        &TargetLaunchOptions {
            enforcement: args.enforcement.clone(),
            sandbox_mode: args.sandbox_mode,
            dangerously_skip_permissions: args.dangerously_skip_permissions,
            assume_yes: args.assume_yes,
        },
    )?;
    let execution_plan = prepared.execution_plan;
    let decision = prepared.runtime_decision;
    let tier = prepared.tier;
    let guard_result = prepared.guard_result;
    let launch_ctx = prepared.launch_ctx;

    run_v03_lifecycle_steps(&decision.plan, &args.reporter, &launch_ctx).await?;

    debug!(
        runtime = execution_plan.target.runtime.as_str(),
        driver = execution_plan.target.driver.as_str(),
        ?tier,
        executor = ?guard_result.executor_kind,
        requires_sandbox_opt_in = guard_result.requires_sandbox_opt_in,
        dangerously_skip_permissions = args.dangerously_skip_permissions,
        "ExecutionPlan resolved"
    );

    let sidecar = match crate::common::sidecar::maybe_start_sidecar() {
        Ok(Some(sidecar)) => {
            debug!("Sidecar started");
            Some(sidecar)
        }
        Ok(None) => {
            debug!("Sidecar not available (no TSNET env)");
            None
        }
        Err(err) => {
            debug!(error = %err, "Sidecar start failed");
            None
        }
    };

    let mut sidecar_cleanup = crate::SidecarCleanup::new(sidecar, args.reporter.clone());

    let mode = if args.background {
        ExecuteMode::Background
    } else {
        ExecuteMode::Foreground
    };

    let run_scoped_id = runtime_overrides::scoped_id_override();
    if args.background {
        if let Some(scoped_id) = run_scoped_id.as_deref() {
            cleanup_existing_scoped_processes_before_run(scoped_id, &args.reporter).await?;
        }
    }

    if execution_plan.target.runtime == capsule_core::execution_plan::model::ExecutionRuntime::Web {
        notify_web_endpoint(&decision.plan, &args.reporter).await?;
    }

    let run_command_uses_specialized_executor = decision
        .plan
        .execution_driver()
        .map(|driver| {
            matches!(
                driver.trim().to_ascii_lowercase().as_str(),
                "deno" | "node" | "python"
            )
        })
        .unwrap_or(false);

    if decision.plan.execution_run_command().is_some() && !run_command_uses_specialized_executor {
        let mut process = crate::executors::shell::execute(&decision.plan, mode, &launch_ctx)?;
        if args.background {
            let pid = process.child.id();
            let id = format!("capsule-{}", pid);
            let now = SystemTime::now();

            let info = crate::process_manager::ProcessInfo {
                id: id.clone(),
                name: decision
                    .plan
                    .manifest_path
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string(),
                pid: pid as i32,
                workload_pid: None,
                status: crate::process_manager::ProcessStatus::Ready,
                runtime: "shell".to_string(),
                start_time: now,
                manifest_path: Some(decision.plan.manifest_path.clone()),
                scoped_id: run_scoped_id.clone(),
                target_label: Some(decision.plan.selected_target_label().to_string()),
                requested_port: None,
                log_path: None,
                ready_at: Some(now),
                last_event: Some("spawned".to_string()),
                last_error: None,
                exit_code: None,
            };

            let pm = crate::process_manager::ProcessManager::new()?;
            pm.write_pid(&info)?;
            args.reporter
                .notify(format!("🚀 Capsule started in background (ID: {})", id))
                .await?;
            drop(process.child);
            sidecar_cleanup.stop_now();
            return Ok(());
        }

        let exit_code = crate::executors::source::wait_for_exit(&mut process.child).await?;
        cleanup_process_artifacts(&process.cleanup_paths);
        sidecar_cleanup.stop_now();
        if exit_code != 0 {
            if let Some(external_capsules) = external_capsules.as_mut() {
                external_capsules.shutdown_now();
            }
            std::process::exit(exit_code);
        }
        return Ok(());
    }

    match guard_result.executor_kind {
        ExecutorKind::Native => {
            let mut process = if args.dangerously_skip_permissions {
                crate::executors::source::execute_host(
                    &decision.plan,
                    args.reporter.clone(),
                    mode,
                    &launch_ctx,
                )?
            } else {
                let nacelle =
                    preflight_native_sandbox(args.nacelle.clone(), &decision.plan, &args.reporter)?;
                crate::executors::source::execute(
                    &decision.plan,
                    Some(nacelle),
                    args.reporter.clone(),
                    &args.enforcement,
                    mode,
                    &launch_ctx,
                )?
            };

            if args.background {
                let pid = process.child.id();
                let id = format!("capsule-{}", pid);
                let now = SystemTime::now();

                let info = crate::process_manager::ProcessInfo {
                    id: id.clone(),
                    name: decision
                        .plan
                        .manifest_path
                        .file_stem()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    pid: pid as i32,
                    workload_pid: process.workload_pid.map(|value| value as i32),
                    status: if args.dangerously_skip_permissions {
                        crate::process_manager::ProcessStatus::Ready
                    } else {
                        crate::process_manager::ProcessStatus::Starting
                    },
                    runtime: if args.dangerously_skip_permissions {
                        "host".to_string()
                    } else {
                        "nacelle".to_string()
                    },
                    start_time: now,
                    manifest_path: Some(decision.plan.manifest_path.clone()),
                    scoped_id: run_scoped_id.clone(),
                    target_label: Some(decision.plan.selected_target_label().to_string()),
                    requested_port: None,
                    log_path: process.log_path.clone(),
                    ready_at: if args.dangerously_skip_permissions {
                        Some(now)
                    } else {
                        None
                    },
                    last_event: Some("spawned".to_string()),
                    last_error: None,
                    exit_code: None,
                };

                let pm = crate::process_manager::ProcessManager::new()?;
                pm.write_pid(&info)?;

                let (startup_outcome, event_rx) = if args.dangerously_skip_permissions {
                    (BackgroundStartupOutcome::Ready, None)
                } else {
                    wait_for_background_native_startup(&mut process, &pm, &id)?
                };

                cleanup_process_artifacts(&process.cleanup_paths);

                match startup_outcome {
                    BackgroundStartupOutcome::Ready => {
                        let _ = process.child;
                        let _ = event_rx;
                        let _ = pm.read_pid(&id)?;
                        args.reporter
                            .notify(format!(
                                "🚀 Capsule started in background and is ready (ID: {})",
                                id
                            ))
                            .await?;
                    }
                    BackgroundStartupOutcome::TimedOut => {
                        let _ = process.child;
                        let _ = event_rx;
                        let _ = pm.read_pid(&id)?;
                        args.reporter
                            .warn(format!(
                                "⏳ Capsule is still starting in background (ID: {}). Use `ato ps --all` to inspect readiness.",
                                id
                            ))
                            .await?;
                    }
                    BackgroundStartupOutcome::FailedBeforeReady => {
                        let state = pm.read_pid(&id).ok();
                        let mut message =
                            format!("Background capsule failed before readiness (ID: {})", id);
                        if let Some(state) = state {
                            if let Some(error) = state.last_error {
                                message.push_str(&format!(": {}", error));
                            } else if let Some(code) = state.exit_code {
                                message.push_str(&format!(": exit code {}", code));
                            }
                            if let Some(log_path) = state.log_path {
                                message.push_str(&format!(". See logs at {}", log_path.display()));
                            }
                        }
                        sidecar_cleanup.stop_now();
                        anyhow::bail!(message);
                    }
                }
                sidecar_cleanup.stop_now();
                return Ok(());
            }

            let readiness_notifier = spawn_foreground_native_event_reporter(
                args.reporter.clone(),
                process.event_rx.take(),
                !args.dangerously_skip_permissions,
                launch_ctx
                    .socket_paths()
                    .map(|paths| !paths.is_empty())
                    .unwrap_or(false),
            )?;
            let exit_code = crate::executors::source::wait_for_exit(&mut process.child).await?;
            if let Some(handle) = readiness_notifier {
                let _ = handle.join();
            }
            cleanup_process_artifacts(&process.cleanup_paths);

            sidecar_cleanup.stop_now();

            if exit_code != 0 {
                if let Some(external_capsules) = external_capsules.as_mut() {
                    external_capsules.shutdown_now();
                }
                std::process::exit(exit_code);
            }
        }
        ExecutorKind::Wasm => {
            let exit = crate::executors::wasm::execute(
                &decision.plan,
                args.reporter.clone(),
                &launch_ctx,
            )?;
            sidecar_cleanup.stop_now();
            if exit != 0 {
                if let Some(external_capsules) = external_capsules.as_mut() {
                    external_capsules.shutdown_now();
                }
                std::process::exit(exit);
            }
        }
        ExecutorKind::WebStatic => {
            if args.background {
                let child = crate::executors::open_web::spawn_background(&decision.plan)?;
                let pid = child.id();
                let id = format!("capsule-{}", pid);

                let info = crate::process_manager::ProcessInfo {
                    id: id.clone(),
                    name: decision
                        .plan
                        .manifest_path
                        .file_stem()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    pid: pid as i32,
                    workload_pid: None,
                    status: crate::process_manager::ProcessStatus::Ready,
                    runtime: "web-static".to_string(),
                    start_time: std::time::SystemTime::now(),
                    manifest_path: Some(decision.plan.manifest_path.clone()),
                    scoped_id: run_scoped_id.clone(),
                    target_label: Some(decision.plan.selected_target_label().to_string()),
                    requested_port: None,
                    log_path: None,
                    ready_at: Some(std::time::SystemTime::now()),
                    last_event: Some("spawned".to_string()),
                    last_error: None,
                    exit_code: None,
                };

                let pm = crate::process_manager::ProcessManager::new()?;
                pm.write_pid(&info)?;

                args.reporter
                    .notify(format!("🚀 Capsule started in background (ID: {})", id))
                    .await?;

                drop(child);
                sidecar_cleanup.stop_now();
                return Ok(());
            }

            crate::executors::open_web::execute(&decision.plan, args.reporter.clone())?;
            sidecar_cleanup.stop_now();
        }
        ExecutorKind::Deno => {
            let exit = crate::executors::deno::execute(
                &decision.plan,
                &execution_plan,
                &launch_ctx,
                args.dangerously_skip_permissions,
            )?;
            sidecar_cleanup.stop_now();
            if exit != 0 {
                if let Some(external_capsules) = external_capsules.as_mut() {
                    external_capsules.shutdown_now();
                }
                std::process::exit(exit);
            }
        }
        ExecutorKind::NodeCompat => {
            let exit = crate::executors::node_compat::execute(
                &decision.plan,
                &execution_plan,
                &launch_ctx,
                args.dangerously_skip_permissions,
            )?;
            sidecar_cleanup.stop_now();
            if exit != 0 {
                if let Some(external_capsules) = external_capsules.as_mut() {
                    external_capsules.shutdown_now();
                }
                std::process::exit(exit);
            }
        }
    }

    Ok(())
}

fn resolve_state_source_overrides(
    manifest: &CapsuleManifest,
    raw_bindings: &[String],
) -> Result<std::collections::HashMap<String, String>> {
    let mut requested = std::collections::HashMap::new();
    for raw in raw_bindings {
        let (state_name, locator) = raw.split_once('=').ok_or_else(|| {
            anyhow::anyhow!(
                "invalid --state binding '{}'; expected data=/absolute/path or data=state-...",
                raw
            )
        })?;
        let state_name = state_name.trim();
        let locator = locator.trim();
        if state_name.is_empty() || locator.is_empty() {
            anyhow::bail!(
                "invalid --state binding '{}'; expected data=/absolute/path or data=state-...",
                raw
            );
        }
        if requested
            .insert(state_name.to_string(), locator.to_string())
            .is_some()
        {
            anyhow::bail!(
                "state '{}' was bound more than once via --state",
                state_name
            );
        }
    }

    for state_name in requested.keys() {
        let requirement = manifest.state.get(state_name).ok_or_else(|| {
            anyhow::anyhow!(
                "--state references undeclared manifest state '{}'",
                state_name
            )
        })?;
        if requirement.durability != StateDurability::Persistent {
            anyhow::bail!(
                "--state only supports persistent manifest state; '{}' is {:?}",
                state_name,
                requirement.durability
            );
        }
    }

    let persistent_states: Vec<_> = manifest
        .state
        .iter()
        .filter(|(_, requirement)| requirement.durability == StateDurability::Persistent)
        .collect();
    if persistent_states.is_empty() {
        if requested.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        anyhow::bail!(
            "--state was provided but the manifest declares no persistent [state] entries"
        );
    }

    let mut resolved = std::collections::HashMap::new();

    for (state_name, _) in persistent_states {
        let locator = requested.get(state_name.as_str()).ok_or_else(|| {
            anyhow::anyhow!(
                "persistent state '{}' requires an explicit --state {}=/absolute/path or --state {}=state-... binding",
                state_name,
                state_name,
                state_name
            )
        })?;
        let record = if parse_state_reference(locator).is_some() {
            resolve_registered_state_reference(manifest, state_name, locator)?
        } else {
            ensure_registered_state_binding(manifest, state_name, locator)?
        };

        resolved.insert(state_name.clone(), record.backend_locator);
    }

    Ok(resolved)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ForegroundEventMessage {
    Notify(String),
    Warn(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackgroundStartupOutcome {
    Ready,
    TimedOut,
    FailedBeforeReady,
}

fn spawn_foreground_native_event_reporter(
    reporter: Arc<CliReporter>,
    event_rx: Option<Receiver<NacelleExecEvent>>,
    sandbox_initialized: bool,
    ipc_socket_mapped: bool,
) -> Result<Option<JoinHandle<()>>> {
    let Some(event_rx) = event_rx else {
        return Ok(None);
    };

    for message in initial_foreground_native_messages(sandbox_initialized, ipc_socket_mapped) {
        futures::executor::block_on(CapsuleReporter::notify(&*reporter, message))?;
    }

    Ok(Some(std::thread::spawn(move || {
        let mut ready_reported = false;
        for event in event_rx {
            for message in foreground_native_event_messages(&event, ready_reported) {
                match message {
                    ForegroundEventMessage::Notify(message) => {
                        let _ = futures::executor::block_on(CapsuleReporter::notify(
                            &*reporter, message,
                        ));
                    }
                    ForegroundEventMessage::Warn(message) => {
                        let _ =
                            futures::executor::block_on(CapsuleReporter::warn(&*reporter, message));
                    }
                }
            }

            if matches!(event, NacelleExecEvent::IpcReady { .. }) {
                ready_reported = true;
            }
        }
    })))
}

fn wait_for_background_native_startup(
    process: &mut crate::executors::source::CapsuleProcess,
    process_manager: &crate::process_manager::ProcessManager,
    process_id: &str,
) -> Result<(BackgroundStartupOutcome, Option<Receiver<NacelleExecEvent>>)> {
    let Some(event_rx) = process.event_rx.take() else {
        return Ok((BackgroundStartupOutcome::TimedOut, None));
    };
    let event_rx = Some(event_rx);

    let deadline = Instant::now() + background_ready_wait_timeout();

    loop {
        if let Some(status) = process.child.try_wait()? {
            let exit_code = status.code();
            let _ = process_manager.update_pid(process_id, |info| {
                info.exit_code = exit_code;
                info.last_event = Some("process_exited".to_string());
                if matches!(info.status, crate::process_manager::ProcessStatus::Starting) {
                    info.status = crate::process_manager::ProcessStatus::Failed;
                    if info.last_error.is_none() {
                        info.last_error = Some("process exited before readiness".to_string());
                    }
                } else if info.status.is_active() {
                    info.status = crate::process_manager::ProcessStatus::Exited;
                }
            });
            return Ok((BackgroundStartupOutcome::FailedBeforeReady, event_rx));
        }

        let now = Instant::now();
        if now >= deadline {
            let _ = process_manager.update_pid(process_id, |info| {
                info.last_event = Some("startup_timeout".to_string());
            });
            return Ok((BackgroundStartupOutcome::TimedOut, event_rx));
        }

        let wait_for = std::cmp::min(Duration::from_millis(100), deadline - now);
        match event_rx
            .as_ref()
            .expect("event receiver should still be present during startup wait")
            .recv_timeout(wait_for)
        {
            Ok(event) => {
                match persist_background_native_event(process_manager, process_id, &event)? {
                    BackgroundStartupOutcome::Ready => {
                        return Ok((BackgroundStartupOutcome::Ready, event_rx));
                    }
                    BackgroundStartupOutcome::FailedBeforeReady => {
                        return Ok((BackgroundStartupOutcome::FailedBeforeReady, event_rx));
                    }
                    BackgroundStartupOutcome::TimedOut => {}
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                let _ = process_manager.update_pid(process_id, |info| {
                    if matches!(info.status, crate::process_manager::ProcessStatus::Starting) {
                        info.status = crate::process_manager::ProcessStatus::Unknown;
                        info.last_error =
                            Some("event stream disconnected before readiness".to_string());
                    }
                });
                return Ok((BackgroundStartupOutcome::TimedOut, None));
            }
        }
    }
}

fn background_ready_wait_timeout() -> Duration {
    std::env::var(BACKGROUND_READY_WAIT_TIMEOUT_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .filter(|duration| !duration.is_zero())
        .unwrap_or(BACKGROUND_READY_WAIT_TIMEOUT)
}

fn persist_background_native_event(
    process_manager: &crate::process_manager::ProcessManager,
    process_id: &str,
    event: &NacelleExecEvent,
) -> Result<BackgroundStartupOutcome> {
    let now = SystemTime::now();
    let updated = process_manager.update_pid(process_id, |info| match event {
        NacelleExecEvent::IpcReady { .. } => {
            info.status = crate::process_manager::ProcessStatus::Ready;
            info.ready_at = Some(now);
            info.last_event = Some("ipc_ready".to_string());
            info.last_error = None;
        }
        NacelleExecEvent::ServiceExited { service, exit_code } => {
            info.exit_code = *exit_code;
            info.last_event = Some("service_exited".to_string());
            if matches!(info.status, crate::process_manager::ProcessStatus::Starting) {
                info.status = crate::process_manager::ProcessStatus::Failed;
                info.last_error = Some(format!("service '{}' exited before readiness", service));
            } else if info.status.is_active() {
                info.status = crate::process_manager::ProcessStatus::Exited;
            }
        }
    })?;

    Ok(match updated.status {
        crate::process_manager::ProcessStatus::Ready => BackgroundStartupOutcome::Ready,
        crate::process_manager::ProcessStatus::Failed => {
            BackgroundStartupOutcome::FailedBeforeReady
        }
        _ => BackgroundStartupOutcome::TimedOut,
    })
}

fn cleanup_process_artifacts(paths: &[PathBuf]) {
    for path in paths {
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
    }
}

async fn cleanup_existing_scoped_processes_before_run(
    scoped_id: &str,
    reporter: &Arc<CliReporter>,
) -> Result<()> {
    let process_manager = crate::process_manager::ProcessManager::new()?;
    let cleaned = process_manager.cleanup_scoped_processes(scoped_id, true)?;
    if cleaned > 0 {
        reporter
            .warn(format!(
                "🧹 Cleaned up {} existing process record(s) for {} before run",
                cleaned, scoped_id
            ))
            .await?;
    }
    Ok(())
}

fn initial_foreground_native_messages(
    sandbox_initialized: bool,
    ipc_socket_mapped: bool,
) -> Vec<String> {
    let mut messages = Vec::new();
    if sandbox_initialized {
        messages.push("[✓] Sandbox initialized".to_string());
    }
    if ipc_socket_mapped {
        messages.push("[✓] IPC socket mapped".to_string());
    }
    messages
}

fn foreground_native_event_messages(
    event: &NacelleExecEvent,
    ready_reported: bool,
) -> Vec<ForegroundEventMessage> {
    match event {
        NacelleExecEvent::IpcReady { service, .. } if !ready_reported => {
            let ready_message = if service == "main" {
                "[✓] Service is ready (ipc_ready received)".to_string()
            } else {
                format!("[✓] Service '{service}' is ready (ipc_ready received)")
            };
            vec![
                ForegroundEventMessage::Notify(ready_message),
                ForegroundEventMessage::Notify("    Streaming logs...".to_string()),
            ]
        }
        NacelleExecEvent::ServiceExited { service, exit_code } if !ready_reported => {
            let exit_code = exit_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            vec![ForegroundEventMessage::Warn(format!(
                "❌ Service '{service}' exited before readiness (exit code: {exit_code})"
            ))]
        }
        _ => Vec::new(),
    }
}

async fn notify_web_endpoint(
    plan: &capsule_core::router::ManifestData,
    reporter: &Arc<CliReporter>,
) -> Result<()> {
    let port = runtime_overrides::override_port(plan.execution_port()).ok_or_else(|| {
        anyhow::anyhow!(
            "runtime=web target '{}' requires targets.<label>.port",
            plan.selected_target_label()
        )
    })?;

    reporter
        .notify(format!(
            "🌐 Web target '{}' is available at http://127.0.0.1:{}/",
            plan.selected_target_label(),
            port
        ))
        .await?;
    Ok(())
}

fn execute_watch_mode(args: OpenArgs) -> Result<()> {
    let manifest_path = if args.target.is_dir() {
        args.target.join("capsule.toml")
    } else {
        args.target.clone()
    };
    let manifest = CapsuleManifest::load_from_file(&manifest_path)?;
    let state_source_overrides = resolve_state_source_overrides(&manifest, &args.state_bindings)?;
    let decision = capsule_core::router::route_manifest_with_state_overrides(
        &manifest_path,
        router::ExecutionProfile::Dev,
        args.target_label.as_deref(),
        state_source_overrides,
    )?;
    if decision.plan.is_orchestration_mode() {
        anyhow::bail!("--watch is not supported for orchestration mode");
    }
    if matches!(decision.kind, capsule_core::router::RuntimeKind::Oci) {
        anyhow::bail!("--watch is not supported for runtime=oci");
    }

    futures::executor::block_on(CapsuleReporter::notify(
        &*args.reporter,
        "👀 Starting watch mode (foreground)".to_string(),
    ))?;

    let config = watch::WatchConfig::default();

    futures::executor::block_on(CapsuleReporter::notify(
        &*args.reporter,
        format!(
            "📊 Watch config: patterns={}, ignore={}, debounce={}ms",
            config.watch_patterns.join(", "),
            config.ignore_patterns.join(", "),
            config.debounce_ms
        ),
    ))?;

    let (_watcher, capsule_handle) =
        watch::watch_directory(args.target.clone(), config, args.reporter.clone())?;

    let reporter_for_cleanup = args.reporter.clone();

    ctrlc::set_handler(move || {
        let _ = capsule_handle.stop();
        let _ = futures::executor::block_on(CapsuleReporter::warn(
            &*reporter_for_cleanup,
            "👋 Watch mode stopped".to_string(),
        ));
        std::process::exit(0);
    })
    .map_err(|e| anyhow::anyhow!("Failed to set Ctrl+C handler: {:?}", e))?;

    std::thread::park();

    Ok(())
}

pub(crate) fn preflight_native_sandbox(
    nacelle_override: Option<PathBuf>,
    plan: &capsule_core::router::ManifestData,
    reporter: &Arc<CliReporter>,
) -> Result<PathBuf> {
    preflight_python_uv_lock_for_source_driver(plan)?;
    preflight_python_uv_binary_for_source_driver(plan)?;
    preflight_glibc_compat(plan)?;
    preflight_macos_compat(plan)?;

    let nacelle = resolve_nacelle_for_tier2(nacelle_override, plan, reporter)?;
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

fn resolve_nacelle_for_tier2(
    nacelle_override: Option<PathBuf>,
    plan: &capsule_core::router::ManifestData,
    reporter: &Arc<CliReporter>,
) -> Result<PathBuf> {
    let request = capsule_core::engine::EngineRequest {
        explicit_path: nacelle_override.clone(),
        manifest_path: Some(plan.manifest_path.clone()),
    };

    match capsule_core::engine::discover_nacelle(request) {
        Ok(path) => Ok(path),
        Err(err) => {
            if !should_attempt_nacelle_auto_bootstrap(
                nacelle_override.as_deref(),
                &plan.manifest_path,
            )? {
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
    manifest_path: &Path,
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
    if manifest_declares_engine_override(manifest_path)? {
        return Ok(false);
    }

    Ok(true)
}

fn manifest_declares_engine_override(manifest_path: &Path) -> Result<bool> {
    if !manifest_path.exists() {
        return Ok(false);
    }

    let raw = fs::read_to_string(manifest_path)
        .with_context(|| format!("Failed to read manifest: {}", manifest_path.display()))?;
    let parsed = toml::from_str::<toml::Value>(&raw)
        .with_context(|| format!("Failed to parse manifest TOML: {}", manifest_path.display()))?;
    Ok(parsed.get("engine").is_some())
}

#[cfg(test)]
fn preflight_required_environment_variables(
    plan: &capsule_core::router::ManifestData,
) -> Result<()> {
    target_runner::preflight_required_environment_variables(
        plan,
        &crate::executors::launch_context::RuntimeLaunchContext::empty(),
    )
}

async fn run_v03_lifecycle_steps(
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
        let working_dir = target_plan.execution_working_directory();

        if provisioned_roots.insert(working_dir.clone()) {
            if let Some(command) = plan_v03_provision_command(&target_plan)? {
                reporter
                    .notify(format!("⚙️  Provision [{}]: {}", target_label, command))
                    .await?;
                run_lifecycle_shell_command(&target_plan, launch_ctx, &command, "provision")?;
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
            run_lifecycle_shell_command(&target_plan, launch_ctx, &command, "build")?;
        }
    }

    Ok(())
}

fn plan_v03_provision_command(plan: &capsule_core::router::ManifestData) -> Result<Option<String>> {
    let runtime = plan.execution_runtime().unwrap_or_default();
    let driver = plan.execution_driver().unwrap_or_default();
    let runtime = runtime.trim().to_ascii_lowercase();
    let driver = driver.trim().to_ascii_lowercase();
    let manifest_dir = plan.execution_working_directory();

    if runtime == "web" && driver == "static" {
        return Ok(None);
    }

    if matches!(driver.as_str(), "node") {
        let package_lock = manifest_dir.join("package-lock.json");
        let pnpm_lock = manifest_dir.join("pnpm-lock.yaml");
        let bun_lock = manifest_dir.join("bun.lock");
        let bun_lockb = manifest_dir.join("bun.lockb");
        let mut matches = Vec::new();
        if package_lock.exists() {
            matches.push("npm ci");
        }
        if pnpm_lock.exists() {
            matches.push("pnpm install --frozen-lockfile");
        }
        if bun_lock.exists() || bun_lockb.exists() {
            matches.push("bun install --frozen-lockfile");
        }
        return match matches.as_slice() {
            [] => Err(AtoExecutionError::lock_incomplete(
                "source/node target requires one of package-lock.json, pnpm-lock.yaml, bun.lock, or bun.lockb",
                Some("package-lock.json"),
            )
            .into()),
            [command] => Ok(Some((*command).to_string())),
            _ => Err(AtoExecutionError::lock_incomplete(
                "multiple node lockfiles detected; keep only one of package-lock.json, pnpm-lock.yaml, bun.lock, or bun.lockb",
                Some("package-lock.json"),
            )
            .into()),
        };
    }

    if matches!(driver.as_str(), "python") {
        return if manifest_dir.join("uv.lock").exists() {
            Ok(Some("uv sync --frozen".to_string()))
        } else {
            Err(AtoExecutionError::lock_incomplete(
                "source/python target requires uv.lock for fail-closed provisioning",
                Some("uv.lock"),
            )
            .into())
        };
    }

    if matches!(driver.as_str(), "native") && manifest_dir.join("Cargo.lock").exists() {
        return Ok(Some("cargo fetch --locked".to_string()));
    }

    Ok(None)
}

fn run_lifecycle_shell_command(
    plan: &capsule_core::router::ManifestData,
    launch_ctx: &crate::executors::launch_context::RuntimeLaunchContext,
    command: &str,
    phase: &str,
) -> Result<()> {
    #[cfg(windows)]
    let mut cmd = {
        let mut cmd = std::process::Command::new("cmd");
        cmd.args(["/C", command]);
        cmd
    };

    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = std::process::Command::new("sh");
        cmd.args(["-lc", command]);
        cmd
    };

    cmd.current_dir(plan.execution_working_directory())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());

    for (key, value) in runtime_overrides::merged_env(plan.execution_env()) {
        cmd.env(key, value);
    }
    launch_ctx.apply_allowlisted_env(&mut cmd)?;

    let status = cmd
        .status()
        .with_context(|| format!("Failed to execute {} command", phase))?;
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

    if resolve_python_dependency_lock_path(&plan.manifest_dir).is_some() {
        return Ok(());
    }

    Err(AtoExecutionError::lock_incomplete(
        "source/python target requires uv.lock for fail-closed provisioning",
        Some("uv.lock"),
    )
    .into())
}

fn preflight_python_uv_binary_for_source_driver(
    plan: &capsule_core::router::ManifestData,
) -> Result<()> {
    if !is_python_source_target(plan) {
        return Ok(());
    }

    runtime_manager::ensure_uv_binary(plan)
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
        .map(|entry| entry.trim().to_ascii_lowercase().ends_with(".py"))
        .unwrap_or(false)
}

fn preflight_glibc_compat(plan: &capsule_core::router::ManifestData) -> Result<()> {
    let required_from_elf = detect_required_glibc_from_entrypoint(plan)?;

    let lock_path = match plan.manifest_path.parent() {
        Some(parent) => {
            resolve_existing_lockfile_path(parent).unwrap_or_else(|| lockfile_output_path(parent))
        }
        None => {
            if required_from_elf.is_none() {
                return Ok(());
            }
            PathBuf::from(CAPSULE_LOCK_FILE_NAME)
        }
    };

    let required_from_lock = detect_required_glibc_from_lock(&lock_path)?;
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
    let lockfile = parse_lockfile_text(&raw, lock_path)
        .with_context(|| format!("Failed to parse {}", lock_path.display()))?;

    Ok(lockfile
        .targets
        .values()
        .find_map(|target| target.constraints.as_ref().and_then(|c| c.glibc.clone())))
}

fn detect_required_glibc_from_entrypoint(
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

fn resolve_uv_lock_path(manifest_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        manifest_dir.join("uv.lock"),
        manifest_dir.join("source").join("uv.lock"),
    ];
    candidates.into_iter().find(|path| path.exists())
}

fn resolve_python_dependency_lock_path(manifest_dir: &Path) -> Option<PathBuf> {
    resolve_uv_lock_path(manifest_dir)
}

#[cfg(test)]
mod tests {
    use super::{
        foreground_native_event_messages, initial_foreground_native_messages,
        plan_v03_provision_command, preflight_required_environment_variables,
        resolve_python_dependency_lock_path, resolve_state_source_overrides,
        ForegroundEventMessage,
    };
    use crate::executors::source::NacelleExecEvent;
    use capsule_core::router::{ExecutionProfile, ManifestData};
    use capsule_core::types::CapsuleManifest;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};

    /// Serialize tests that mutate process-global environment variables like `HOME`.
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    /// RAII helper that restores the previous `HOME` environment variable on drop.
    struct HomeGuard {
        previous: Option<OsString>,
    }

    impl HomeGuard {
        fn set(path: &std::path::Path) -> Self {
            let previous = std::env::var_os("HOME");
            std::env::set_var("HOME", path);
            Self { previous }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                std::env::set_var("HOME", previous);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    #[test]
    fn resolve_python_dependency_lock_path_prefers_source_uv_lock() {
        let tmp = tempfile::tempdir().expect("create temp dir");
        std::fs::create_dir_all(tmp.path().join("source")).expect("create source dir");
        std::fs::write(tmp.path().join("source").join("uv.lock"), "").expect("write uv.lock");

        let found = resolve_python_dependency_lock_path(tmp.path()).expect("must resolve uv.lock");
        assert_eq!(found, tmp.path().join("source").join("uv.lock"));
    }

    #[test]
    fn v03_node_provision_prefers_single_detected_lockfile() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("pnpm-lock.yaml"), "lockfileVersion: '9.0'")
            .expect("write pnpm lock");

        let plan = manifest_with_schema_and_target(
            "0.3",
            tmp.path().to_path_buf(),
            vec![
                ("runtime", toml::Value::String("source".to_string())),
                ("driver", toml::Value::String("node".to_string())),
                (
                    "run_command",
                    toml::Value::String("pnpm start -- --port $PORT".to_string()),
                ),
            ],
        );

        let command = plan_v03_provision_command(&plan).expect("plan provision");
        assert_eq!(command.as_deref(), Some("pnpm install --frozen-lockfile"));
    }

    #[test]
    fn v03_node_provision_rejects_ambiguous_lockfiles() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("package-lock.json"), "{}").expect("write package lock");
        std::fs::write(tmp.path().join("pnpm-lock.yaml"), "lockfileVersion: '9.0'")
            .expect("write pnpm lock");

        let plan = manifest_with_schema_and_target(
            "0.3",
            tmp.path().to_path_buf(),
            vec![
                ("runtime", toml::Value::String("source".to_string())),
                ("driver", toml::Value::String("node".to_string())),
                (
                    "run_command",
                    toml::Value::String("npm start -- --port $PORT".to_string()),
                ),
            ],
        );

        let err = plan_v03_provision_command(&plan).expect_err("must reject ambiguity");
        assert!(err.to_string().contains("multiple node lockfiles detected"));
    }

    #[test]
    fn v03_node_provision_uses_target_working_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app_dir = tmp.path().join("apps").join("web");
        std::fs::create_dir_all(&app_dir).expect("create app dir");
        std::fs::write(app_dir.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'")
            .expect("write pnpm lock");

        let plan = manifest_with_schema_and_target(
            "0.3",
            tmp.path().to_path_buf(),
            vec![
                ("runtime", toml::Value::String("source".to_string())),
                ("driver", toml::Value::String("node".to_string())),
                ("working_dir", toml::Value::String("apps/web".to_string())),
                (
                    "run_command",
                    toml::Value::String("pnpm start -- --port $PORT".to_string()),
                ),
            ],
        );

        let command = plan_v03_provision_command(&plan).expect("plan provision");
        assert_eq!(command.as_deref(), Some("pnpm install --frozen-lockfile"));
    }

    #[test]
    fn preflight_required_env_fails_when_missing_or_empty() {
        let key_missing = "ATO_TEST_REQUIRED_ENV_MISSING";
        let key_empty = "ATO_TEST_REQUIRED_ENV_EMPTY";
        std::env::remove_var(key_missing);
        std::env::set_var(key_empty, "");

        let plan = manifest_with_required_env(vec![key_missing, key_empty]);
        let err = preflight_required_environment_variables(&plan).expect_err("must fail-closed");
        let msg = err.to_string();
        assert!(msg.contains(key_missing), "msg={msg}");
        assert!(msg.contains(key_empty), "msg={msg}");

        std::env::remove_var(key_empty);
    }

    #[test]
    fn preflight_required_env_passes_when_set() {
        let key = "ATO_TEST_REQUIRED_ENV_SET";
        std::env::set_var(key, "ok");

        let plan = manifest_with_required_env(vec![key]);
        assert!(preflight_required_environment_variables(&plan).is_ok());

        std::env::remove_var(key);
    }

    #[test]
    fn preflight_required_env_passes_with_runtime_override() {
        let key = "ATO_TEST_REQUIRED_ENV_FROM_OVERRIDE";
        std::env::set_var("ATO_UI_OVERRIDE_ENV_JSON", format!(r#"{{"{}":"ok"}}"#, key));

        let plan = manifest_with_required_env(vec![key]);
        assert!(preflight_required_environment_variables(&plan).is_ok());

        std::env::remove_var("ATO_UI_OVERRIDE_ENV_JSON");
    }

    #[test]
    fn foreground_native_messages_include_boot_sequence() {
        let messages = initial_foreground_native_messages(true, true);
        assert_eq!(
            messages,
            vec![
                "[✓] Sandbox initialized".to_string(),
                "[✓] IPC socket mapped".to_string()
            ]
        );
    }

    #[test]
    fn foreground_native_ipc_ready_message_matches_expected_copy() {
        let message = foreground_native_event_messages(
            &NacelleExecEvent::IpcReady {
                service: "main".to_string(),
                endpoint: "unix:///tmp/main.sock".to_string(),
                port: None,
            },
            false,
        );

        assert_eq!(
            message,
            vec![
                ForegroundEventMessage::Notify(
                    "[✓] Service is ready (ipc_ready received)".to_string()
                ),
                ForegroundEventMessage::Notify("    Streaming logs...".to_string())
            ]
        );
    }

    #[test]
    fn foreground_native_service_exited_warns_before_readiness() {
        let message = foreground_native_event_messages(
            &NacelleExecEvent::ServiceExited {
                service: "main".to_string(),
                exit_code: Some(42),
            },
            false,
        );

        assert_eq!(
            message,
            vec![ForegroundEventMessage::Warn(
                "❌ Service 'main' exited before readiness (exit code: 42)".to_string()
            )]
        );
    }

    #[test]
    fn resolve_state_source_overrides_registers_persistent_state_binding() {
        let _guard = env_lock().lock().unwrap();
        let home = tempfile::tempdir().expect("home");
        let _home_guard = HomeGuard::set(home.path());

        let manifest = CapsuleManifest::from_toml(
            r#"
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"

[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "primary-data"
attach = "explicit"
schema_id = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#,
        )
        .unwrap();

        let bind_dir = home.path().join("bind").join("data");
        let overrides =
            resolve_state_source_overrides(&manifest, &[format!("data={}", bind_dir.display())])
                .expect("state override");

        assert_eq!(
            overrides.get("data").map(|value| value.as_str()),
            Some(bind_dir.canonicalize().unwrap().to_string_lossy().as_ref())
        );
        assert!(home.path().join(".ato/state/registry.sqlite3").exists());
    }

    #[test]
    fn resolve_state_source_overrides_accepts_state_id_binding() {
        let _guard = env_lock().lock().unwrap();
        let home = tempfile::tempdir().expect("home");
        let _home_guard = HomeGuard::set(home.path());

        let manifest = CapsuleManifest::from_toml(
            r#"
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"

[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "primary-data"
attach = "explicit"
schema_id = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#,
        )
        .unwrap();

        let bind_dir = home.path().join("bind").join("data");
        let first =
            resolve_state_source_overrides(&manifest, &[format!("data={}", bind_dir.display())])
                .expect("initial state registration");
        let record = crate::state::open_state_store()
            .expect("open state store")
            .list_persistent_states(Some("demo-app"), Some("data"))
            .expect("list states")
            .into_iter()
            .next()
            .expect("registered state");

        let second =
            resolve_state_source_overrides(&manifest, &[format!("data={}", record.state_id)])
                .expect("state id bind");

        assert_eq!(first, second);
    }

    #[test]
    fn resolve_state_source_overrides_rejects_incompatible_registry_entry() {
        let _guard = env_lock().lock().unwrap();
        let home = tempfile::tempdir().expect("home");
        let _home_guard = HomeGuard::set(home.path());

        let manifest_a = CapsuleManifest::from_toml(
            r#"
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"

[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "primary-data"
attach = "explicit"
schema_id = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#,
        )
        .unwrap();
        let manifest_b = CapsuleManifest::from_toml(
            r#"
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"

[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "secondary-data"
attach = "explicit"
schema_id = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#,
        )
        .unwrap();

        let bind_dir = home.path().join("bind").join("data");
        resolve_state_source_overrides(&manifest_a, &[format!("data={}", bind_dir.display())])
            .expect("first bind");

        let err =
            resolve_state_source_overrides(&manifest_b, &[format!("data={}", bind_dir.display())])
                .expect_err("incompatible bind must fail");
        assert!(err
            .to_string()
            .contains("producer/purpose/schema_id must match exactly"));
    }

    fn manifest_with_required_env(keys: Vec<&str>) -> ManifestData {
        manifest_with_schema_and_target(
            "0.2",
            PathBuf::from("/tmp"),
            vec![
                ("runtime", toml::Value::String("source".to_string())),
                ("driver", toml::Value::String("native".to_string())),
                ("entrypoint", toml::Value::String("main.py".to_string())),
                (
                    "required_env",
                    toml::Value::Array(
                        keys.into_iter()
                            .map(|k| toml::Value::String(k.to_string()))
                            .collect(),
                    ),
                ),
            ],
        )
    }

    fn manifest_with_schema_and_target(
        schema_version: &str,
        manifest_dir: PathBuf,
        entries: Vec<(&str, toml::Value)>,
    ) -> ManifestData {
        let mut manifest = toml::map::Map::new();
        manifest.insert(
            "schema_version".to_string(),
            toml::Value::String(schema_version.to_string()),
        );
        manifest.insert("name".to_string(), toml::Value::String("demo".to_string()));
        manifest.insert(
            "default_target".to_string(),
            toml::Value::String("default".to_string()),
        );

        let mut target = toml::map::Map::new();
        for (key, value) in entries {
            target.insert(key.to_string(), value);
        }

        let mut targets = toml::map::Map::new();
        targets.insert("default".to_string(), toml::Value::Table(target));
        manifest.insert("targets".to_string(), toml::Value::Table(targets));

        ManifestData {
            manifest: toml::Value::Table(manifest),
            manifest_path: manifest_dir.join("capsule.toml"),
            manifest_dir,
            profile: ExecutionProfile::Dev,
            selected_target: "default".to_string(),
            state_source_overrides: std::collections::HashMap::new(),
        }
    }
}
