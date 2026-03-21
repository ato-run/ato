use anyhow::{Context, Result};
use cliclack::ProgressBar;
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
use crate::executors::target_runner::{self, TargetLaunchOptions};
use crate::preview;
use crate::registry::store::RegistryStore;
use crate::reporters::CliReporter;
use crate::runtime::manager as runtime_manager;
use crate::runtime::overrides as runtime_overrides;
use crate::runtime::provisioning::{self as provisioner, AutoProvisioningOptions};
use crate::runtime::tree as runtime_tree;
use crate::state::{
    ensure_registered_state_binding, ensure_registered_state_binding_in_store,
    parse_state_reference, resolve_registered_state_reference,
    resolve_registered_state_reference_in_store,
};
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::execution_plan::guard::ExecutorKind;
use capsule_core::lifecycle::LifecycleEvent;
use capsule_core::lockfile::{
    lockfile_output_path, manifest_external_capsule_dependencies, parse_lockfile_text,
    resolve_existing_lockfile_path, verify_lockfile_external_dependencies, CAPSULE_LOCK_FILE_NAME,
    LEGACY_CAPSULE_LOCK_FILE_NAME,
};
use capsule_core::types::{CapsuleManifest, CapsuleType, StateDurability};
use capsule_core::{router, CapsuleReporter};

mod background;
mod preflight;
mod watch;

use background::*;
pub(crate) use preflight::preflight_native_sandbox;
use preflight::*;

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
    pub compatibility_fallback: Option<String>,
    pub assume_yes: bool,
    pub state_bindings: Vec<String>,
    pub inject_bindings: Vec<String>,
    pub reporter: Arc<CliReporter>,
    pub preview_mode: bool,
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
            compatibility_fallback: args.compatibility_fallback.clone(),
            assume_yes: args.assume_yes,
            state_bindings: args.state_bindings.clone(),
            inject_bindings: args.inject_bindings.clone(),
            reporter: args.reporter.clone(),
            preview_mode: args.preview_mode,
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
        compatibility_fallback: args.compatibility_fallback.clone(),
        assume_yes: args.assume_yes,
        state_bindings: args.state_bindings.clone(),
        inject_bindings: args.inject_bindings.clone(),
        reporter: args.reporter.clone(),
        preview_mode: args.preview_mode,
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
            crate::fs_copy::copy_path_recursive(&path, &dest)?;
            debug!("Copied source/");
        } else if path.is_file() {
            let dest = extract_dir.join(&file_name);
            fs::copy(&path, &dest)?;
            debug!(file = %file_name.to_string_lossy(), "Copied file into extracted capsule");
        } else if path.is_dir() && !is_hidden(&file_name) {
            let dest = extract_dir.join(&file_name);
            crate::fs_copy::copy_path_recursive(&path, &dest)?;
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

async fn execute_normal_mode(args: OpenArgs) -> Result<()> {
    let manifest_path = if args.target.is_dir() {
        args.target.join("capsule.toml")
    } else {
        args.target.clone()
    };
    let preview_session = preview::load_preview_session_for_manifest(&manifest_path)?;
    let preview_mode = args.preview_mode || preview_session.is_some();
    let use_progressive_ui =
        crate::progressive_ui::can_use_progressive_ui(false) && !args.background;
    let source_label = preview_session
        .as_ref()
        .map(|session| session.target_reference.clone())
        .unwrap_or_else(|| manifest_path.display().to_string());

    if use_progressive_ui {
        crate::progressive_ui::show_run_intro(&source_label)?;
    }

    let manifest = if preview_mode {
        capsule_core::manifest::load_manifest_with_validation_mode(
            &manifest_path,
            capsule_core::types::ValidationMode::Preview,
        )?
        .model
    } else {
        CapsuleManifest::load_from_file(&manifest_path)?
    };
    if manifest.schema_version.trim() == "0.3" && manifest.capsule_type == CapsuleType::Library {
        anyhow::bail!("schema_version=0.3 type=library package cannot be started with `ato run`");
    }
    let state_source_overrides = resolve_state_source_overrides(&manifest, &args.state_bindings)?;
    let decision = capsule_core::router::route_manifest_with_state_overrides_and_validation_mode(
        &manifest_path,
        router::ExecutionProfile::Dev,
        args.target_label.as_deref(),
        state_source_overrides,
        if preview_mode {
            capsule_core::types::ValidationMode::Preview
        } else {
            capsule_core::types::ValidationMode::Strict
        },
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

    let mut decision = decision;
    let mut launch_ctx = launch_ctx;

    let provisioning_outcome = provisioner::run_auto_provisioning_phase(
        &decision.plan,
        &launch_ctx,
        args.reporter.clone(),
        &AutoProvisioningOptions {
            preview_mode,
            background: args.background,
        },
    )
    .await?;
    launch_ctx = launch_ctx
        .with_injected_env(provisioning_outcome.additional_env)
        .with_injected_mounts(provisioning_outcome.additional_mounts);

    if let Some(shadow_workspace) = provisioning_outcome.shadow_workspace.as_ref() {
        debug!(
            issue_count = provisioning_outcome.plan.issues.len(),
            action_count = provisioning_outcome.plan.actions.len(),
            shadow_root = %shadow_workspace.root_dir.display(),
            audit_path = %shadow_workspace.audit_path.display(),
            shadow_manifest = shadow_workspace.manifest_path.as_ref().map(|path| path.display().to_string()),
            "Auto-provisioning shadow workspace prepared"
        );

        if let Some(shadow_manifest_path) = shadow_workspace.manifest_path.as_ref() {
            decision =
                capsule_core::router::route_manifest_with_state_overrides_and_validation_mode(
                    shadow_manifest_path,
                    router::ExecutionProfile::Dev,
                    Some(decision.plan.selected_target_label()),
                    decision.plan.state_source_overrides.clone(),
                    if preview_mode {
                        capsule_core::types::ValidationMode::Preview
                    } else {
                        capsule_core::types::ValidationMode::Strict
                    },
                )?;
            launch_ctx = target_runner::resolve_launch_context(&decision.plan, &args.reporter)
                .await?
                .with_injected_env(launch_ctx.merged_env())
                .with_injected_mounts(launch_ctx.injected_mounts().to_vec());
        }
    }

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
            preview_mode,
            defer_consent: true,
        },
    )?;
    let execution_plan = prepared.execution_plan;
    let decision = prepared.runtime_decision;
    let tier = prepared.tier;
    let guard_result = prepared.guard_result;
    let launch_ctx = prepared.launch_ctx;

    if use_progressive_ui {
        if let Some(preview_session) = preview_session.as_ref() {
            crate::progressive_ui::render_preview_plan(preview_session)?;
            crate::progressive_ui::render_promotion_summary(
                &preview_session.derived_plan.promotion_eligibility,
            )?;
        }
    }

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

            let info = crate::runtime::process::ProcessInfo {
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
                status: crate::runtime::process::ProcessStatus::Ready,
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

            let pm = crate::runtime::process::ProcessManager::new()?;
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

    let compatibility_host_mode = resolve_compatibility_host_mode(
        guard_result.executor_kind,
        args.compatibility_fallback.as_deref(),
    )?;
    let host_fallback_requested = matches!(compatibility_host_mode, CompatibilityHostMode::Enabled);

    if use_progressive_ui {
        if host_fallback_requested {
            crate::progressive_ui::render_host_fallback_warning()?;
        } else {
            crate::progressive_ui::render_security_context(
                guard_result.executor_kind,
                host_fallback_requested,
                args.dangerously_skip_permissions,
                runtime_overrides::override_port(decision.plan.execution_port()),
            )?;
        }
    }

    let consent_already_granted = crate::consent_store::has_consent(&execution_plan)?;
    if !consent_already_granted {
        if use_progressive_ui {
            crate::progressive_ui::render_execution_consent_summary(
                &crate::consent_store::consent_summary(&execution_plan),
            )?;
            let prompt = if host_fallback_requested {
                "Proceed with this Execution Plan and Host Fallback mode?"
            } else {
                "Proceed with this Execution Plan?"
            };
            if !crate::progressive_ui::confirm_action(prompt, false)? {
                crate::progressive_ui::show_cancel("Execution cancelled.")?;
                return Err(AtoExecutionError::from_ato_error(
                    capsule_core::AtoError::ExecutionContractInvalid {
                        message: "ExecutionPlan consent rejected by user".to_string(),
                        hint: Some(
                            "Execution Plan の要約を確認し、許可する場合のみ再実行してください。"
                                .to_string(),
                        ),
                        field: Some("execution_plan.consent".to_string()),
                        service: None,
                    },
                )
                .into());
            }
            crate::consent_store::record_consent(&execution_plan)?;
        } else {
            crate::consent_store::require_consent(&execution_plan, args.assume_yes)?;
        }
    } else if host_fallback_requested {
        if use_progressive_ui {
            if args.assume_yes {
                crate::progressive_ui::show_warning(
                    "Proceeding with Host Fallback mode (--yes specified)",
                )?;
            } else if !crate::progressive_ui::confirm_action(
                "Proceed with Host Fallback mode?",
                false,
            )? {
                crate::progressive_ui::show_cancel("Execution cancelled.")?;
                return Ok(());
            }
        } else if !args.assume_yes {
            anyhow::bail!(
                "Host Fallback mode requires interactive confirmation. Re-run with --yes in non-interactive environments."
            );
        }
    } else if use_progressive_ui
        && preview_mode
        && !args.assume_yes
        && !crate::progressive_ui::confirm_action(
            "Proceed with Preview Run? (Ephemeral Sandbox)",
            true,
        )?
    {
        crate::progressive_ui::show_cancel("Preview cancelled.")?;
        return Ok(());
    }

    match guard_result.executor_kind {
        ExecutorKind::Native => {
            let host_execution = args.dangerously_skip_permissions || host_fallback_requested;
            let process = if host_execution {
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
                let runtime = process_runtime_label(
                    &decision.plan,
                    args.dangerously_skip_permissions,
                    compatibility_host_mode,
                );
                let ready_without_events = host_execution && process.event_rx.is_none();
                complete_background_source_process(
                    process,
                    &decision.plan,
                    runtime,
                    run_scoped_id.clone(),
                    ready_without_events,
                    compatibility_host_mode,
                    &args.reporter,
                )
                .await?;
                sidecar_cleanup.stop_now();
                return Ok(());
            }

            let exit_code = complete_foreground_source_process(
                process,
                args.reporter.clone(),
                !host_execution,
                launch_ctx
                    .socket_paths()
                    .map(|paths| !paths.is_empty())
                    .unwrap_or(false),
                use_progressive_ui,
            )
            .await?;
            sidecar_cleanup.stop_now();

            if exit_code != 0 {
                if let Some(external_capsules) = external_capsules.as_mut() {
                    external_capsules.shutdown_now();
                }
                std::process::exit(exit_code);
            }
        }
        ExecutorKind::NodeCompat if host_fallback_requested => {
            let process = crate::executors::source::execute_host(
                &decision.plan,
                args.reporter.clone(),
                mode,
                &launch_ctx,
            )?;

            if args.background {
                let runtime = process_runtime_label(&decision.plan, false, compatibility_host_mode);
                let ready_without_events = process.event_rx.is_none();
                complete_background_source_process(
                    process,
                    &decision.plan,
                    runtime,
                    run_scoped_id.clone(),
                    ready_without_events,
                    compatibility_host_mode,
                    &args.reporter,
                )
                .await?;
                sidecar_cleanup.stop_now();
                return Ok(());
            }

            let exit_code = complete_foreground_source_process(
                process,
                args.reporter.clone(),
                false,
                launch_ctx
                    .socket_paths()
                    .map(|paths| !paths.is_empty())
                    .unwrap_or(false),
                use_progressive_ui,
            )
            .await?;
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

                let info = crate::runtime::process::ProcessInfo {
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
                    status: crate::runtime::process::ProcessStatus::Ready,
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

                let pm = crate::runtime::process::ProcessManager::new()?;
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
    resolve_state_source_overrides_with_store(manifest, raw_bindings, None)
}

fn resolve_state_source_overrides_with_store(
    manifest: &CapsuleManifest,
    raw_bindings: &[String],
    store: Option<&RegistryStore>,
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
            match store {
                Some(store) => resolve_registered_state_reference_in_store(
                    manifest, state_name, locator, store,
                )?,
                None => resolve_registered_state_reference(manifest, state_name, locator)?,
            }
        } else {
            match store {
                Some(store) => {
                    ensure_registered_state_binding_in_store(manifest, state_name, locator, store)?
                }
                None => ensure_registered_state_binding(manifest, state_name, locator)?,
            }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompatibilityHostMode {
    Disabled,
    Enabled,
}

fn resolve_compatibility_host_mode(
    executor_kind: ExecutorKind,
    compatibility_fallback: Option<&str>,
) -> Result<CompatibilityHostMode> {
    match compatibility_fallback {
        None => Ok(CompatibilityHostMode::Disabled),
        Some("host") if matches!(executor_kind, ExecutorKind::Native | ExecutorKind::NodeCompat) => {
            Ok(CompatibilityHostMode::Enabled)
        }
        Some("host") => anyhow::bail!(
            "--compatibility-fallback host is only supported for native and node-compatible source targets"
        ),
        Some(other) => anyhow::bail!("unsupported compatibility fallback backend: {other}"),
    }
}

fn execute_watch_mode(args: OpenArgs) -> Result<()> {
    let manifest_path = if args.target.is_dir() {
        args.target.join("capsule.toml")
    } else {
        args.target.clone()
    };
    let preview_mode =
        args.preview_mode || preview::load_preview_session_for_manifest(&manifest_path)?.is_some();
    let manifest = if preview_mode {
        capsule_core::manifest::load_manifest_with_validation_mode(
            &manifest_path,
            capsule_core::types::ValidationMode::Preview,
        )?
        .model
    } else {
        CapsuleManifest::load_from_file(&manifest_path)?
    };
    let state_source_overrides = resolve_state_source_overrides(&manifest, &args.state_bindings)?;
    let decision = capsule_core::router::route_manifest_with_state_overrides_and_validation_mode(
        &manifest_path,
        router::ExecutionProfile::Dev,
        args.target_label.as_deref(),
        state_source_overrides,
        if preview_mode {
            capsule_core::types::ValidationMode::Preview
        } else {
            capsule_core::types::ValidationMode::Strict
        },
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

#[cfg(test)]
mod tests {
    use super::{
        background_ready_message, foreground_native_event_messages,
        initial_foreground_native_messages, plan_v03_provision_command,
        preflight_required_environment_variables, process_runtime_label,
        resolve_compatibility_host_mode, resolve_python_dependency_lock_path,
        resolve_state_source_overrides_with_store, CompatibilityHostMode, ForegroundEventMessage,
    };
    use crate::registry::store::RegistryStore;
    use capsule_core::execution_plan::guard::ExecutorKind;
    use capsule_core::lifecycle::LifecycleEvent;
    use capsule_core::router::{ExecutionProfile, ManifestData};
    use capsule_core::types::CapsuleManifest;
    use std::path::PathBuf;

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
            &LifecycleEvent::Ready {
                service: "main".to_string(),
                endpoint: Some("unix:///tmp/main.sock".to_string()),
                port: None,
            },
            false,
        );

        assert_eq!(
            message,
            vec![
                ForegroundEventMessage::Notify(
                    "[✓] Service is ready (ready event received)".to_string()
                ),
                ForegroundEventMessage::Notify("    Streaming logs...".to_string())
            ]
        );
    }

    #[test]
    fn foreground_native_service_exited_warns_before_readiness() {
        let message = foreground_native_event_messages(
            &LifecycleEvent::Exited {
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
    fn compatibility_host_mode_enables_nodecompat_fallback() {
        let mode = resolve_compatibility_host_mode(ExecutorKind::NodeCompat, Some("host"))
            .expect("resolve fallback mode");
        assert_eq!(mode, CompatibilityHostMode::Enabled);
    }

    #[test]
    fn compatibility_host_mode_rejects_deno_fallback() {
        let err = resolve_compatibility_host_mode(ExecutorKind::Deno, Some("host"))
            .expect_err("must reject deno fallback");
        assert!(err.to_string().contains("native and node-compatible"));
    }

    #[test]
    fn compatibility_host_mode_changes_ready_copy() {
        let message = background_ready_message("capsule-42", CompatibilityHostMode::Enabled);
        assert_eq!(
            message,
            "✔ Capsule is ready (Host Fallback, ID: capsule-42)"
        );
    }

    #[test]
    fn process_runtime_label_preserves_runtime_under_host_fallback() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plan = ManifestData {
            manifest: toml::from_str(
                r#"
                [targets.app]
                runtime = "source"
                driver = "node"
                run_command = "node server.js"
                "#,
            )
            .expect("manifest"),
            manifest_path: tmp.path().join("capsule.toml"),
            manifest_dir: tmp.path().to_path_buf(),
            profile: ExecutionProfile::Dev,
            selected_target: "app".to_string(),
            state_source_overrides: std::collections::HashMap::new(),
        };

        let label = process_runtime_label(&plan, false, CompatibilityHostMode::Enabled);
        assert_eq!(label, "source/node [host-fallback]");
    }

    #[test]
    fn resolve_state_source_overrides_registers_persistent_state_binding() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(&tmp.path().join("state-store")).expect("open store");

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

        let bind_dir = tmp.path().join("bind").join("data");
        let overrides = resolve_state_source_overrides_with_store(
            &manifest,
            &[format!("data={}", bind_dir.display())],
            Some(&store),
        )
        .expect("state override");

        assert_eq!(
            overrides.get("data").map(|value| value.as_str()),
            Some(bind_dir.canonicalize().unwrap().to_string_lossy().as_ref())
        );
        assert!(tmp
            .path()
            .join("state-store")
            .join("registry.sqlite3")
            .exists());
    }

    #[test]
    fn resolve_state_source_overrides_accepts_state_id_binding() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(&tmp.path().join("state-store")).expect("open store");

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

        let bind_dir = tmp.path().join("bind").join("data");
        let first = resolve_state_source_overrides_with_store(
            &manifest,
            &[format!("data={}", bind_dir.display())],
            Some(&store),
        )
        .expect("initial state registration");
        let record = store
            .list_persistent_states(Some("demo-app"), Some("data"))
            .expect("list states")
            .into_iter()
            .next()
            .expect("registered state");

        let second = resolve_state_source_overrides_with_store(
            &manifest,
            &[format!("data={}", record.state_id)],
            Some(&store),
        )
        .expect("state id bind");

        assert_eq!(first, second);
    }

    #[test]
    fn resolve_state_source_overrides_rejects_incompatible_registry_entry() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(&tmp.path().join("state-store")).expect("open store");

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

        let bind_dir = tmp.path().join("bind").join("data");
        resolve_state_source_overrides_with_store(
            &manifest_a,
            &[format!("data={}", bind_dir.display())],
            Some(&store),
        )
        .expect("first bind");

        let err = resolve_state_source_overrides_with_store(
            &manifest_b,
            &[format!("data={}", bind_dir.display())],
            Some(&store),
        )
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
