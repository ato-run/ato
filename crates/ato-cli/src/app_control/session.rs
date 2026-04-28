use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use capsule_core::ato_lock;
use capsule_core::handle::{
    normalize_capsule_handle, CapsuleDisplayStrategy, CapsuleRuntimeDescriptor, ResolvedSnapshot,
    TrustState,
};
use capsule_core::launch_spec::derive_launch_spec;
use capsule_core::routing::input_resolver::ATO_LOCK_FILE_NAME;
use serde::{Deserialize, Serialize};

use crate::application::pipeline::phases::run::DerivedBridgeManifest;
use crate::application::pipeline::phases::run::PreparedRunContext;
use crate::executors::source::{CapsuleProcess, ExecuteMode};
use crate::executors::launch_context::RuntimeLaunchContext;
use crate::executors::target_runner::{
    preflight_required_environment_variables, prepare_target_execution, resolve_launch_context,
    TargetLaunchOptions,
};
use crate::install::support::resolve_run_target_or_install;
use crate::reporters;
use crate::reporters::CliReporter;
use crate::runtime::process::{ProcessInfo, ProcessManager, ProcessStatus};
use crate::runtime::tree as runtime_tree;
use crate::ProviderToolchain;

use super::guest_contract::parse_guest_contract;
use super::resolve::{build_resolution, resolve_local_plan};

const SESSION_ACTION_START: &str = "session_start";
const SESSION_ACTION_STOP: &str = "session_stop";
const SESSION_RUNTIME: &str = "desky-session";
const SESSION_READY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize)]
struct SessionStartEnvelope {
    schema_version: &'static str,
    package_id: &'static str,
    action: &'static str,
    session: SessionInfo,
}

#[derive(Debug, Clone, Serialize)]
struct SessionStopEnvelope {
    schema_version: &'static str,
    package_id: &'static str,
    action: &'static str,
    session_id: String,
    stopped: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    session_id: String,
    handle: String,
    normalized_handle: String,
    canonical_handle: Option<String>,
    status: String,
    trust_state: TrustState,
    source: Option<String>,
    restricted: bool,
    snapshot: Option<ResolvedSnapshot>,
    runtime: CapsuleRuntimeDescriptor,
    display_strategy: CapsuleDisplayStrategy,
    pid: i32,
    log_path: String,
    manifest_path: String,
    target_label: String,
    notes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    adapter: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frontend_entry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    healthcheck_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    invoke_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capabilities: Option<Vec<String>>,
    guest: Option<GuestSessionDisplay>,
    web: Option<WebSessionDisplay>,
    terminal: Option<TerminalSessionDisplay>,
    service: Option<ServiceBackgroundDisplay>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSessionInfo {
    session_id: String,
    handle: String,
    normalized_handle: String,
    canonical_handle: Option<String>,
    trust_state: TrustState,
    source: Option<String>,
    restricted: bool,
    snapshot: Option<ResolvedSnapshot>,
    runtime: CapsuleRuntimeDescriptor,
    display_strategy: CapsuleDisplayStrategy,
    pid: i32,
    log_path: String,
    manifest_path: String,
    target_label: String,
    notes: Vec<String>,
    guest: Option<GuestSessionDisplay>,
    web: Option<WebSessionDisplay>,
    terminal: Option<TerminalSessionDisplay>,
    service: Option<ServiceBackgroundDisplay>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GuestSessionDisplay {
    adapter: String,
    frontend_entry: String,
    transport: String,
    healthcheck_url: String,
    invoke_url: String,
    capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WebSessionDisplay {
    local_url: String,
    healthcheck_url: String,
    served_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TerminalSessionDisplay {
    log_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ServiceBackgroundDisplay {
    log_path: String,
}

pub fn start_session(handle: &str, target_label: Option<&str>, json: bool) -> Result<()> {
    let resolution = build_resolution(handle, target_label, None)?;
    let (manifest_path, plan, launch, mut notes) =
        resolve_session_launch_plan(handle, target_label)?;
    notes.extend(resolution.notes.clone());
    let raw_manifest = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest_value: toml::Value = toml::from_str(&raw_manifest)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    // Env preflight before any subprocess spawn — the same check
    // `ato run` relies on. Without this, missing required env (e.g.
    // OPENAI_API_KEY) only surfaces as a process-failure stderr,
    // which the Desktop orchestrator cannot route to its
    // PendingConfig modal (it expects an E103 envelope on stderr).
    // RuntimeLaunchContext::empty() matches what an interactive
    // session start sees: no IPC bindings, no extra injected env,
    // so the check falls back to OS env / manifest env entries —
    // which is what the spawned child will actually receive.
    let launch_ctx = RuntimeLaunchContext::empty();
    preflight_required_environment_variables(&plan, &launch_ctx)?;

    // Run the v0.3 provision/build lifecycle (same path `ato run`
    // takes via `application/pipeline/phases/run.rs`). The Desktop
    // launches capsules through `ato app session start`, which used
    // to skip this — so capsules with `[targets.<label>].
    // build_command` (e.g. a Next.js app declaring `npm install &&
    // npm run build`) saw their `.next` build never get materialized
    // and the run_command then failed with `next: command not found`
    // / `Could not find a production build`. Running the lifecycle
    // here makes the desktop launch path a strict superset of the
    // CLI launch path — both materialize node_modules and the
    // production build before invoking run_command.
    let lifecycle_reporter = Arc::new(reporters::CliReporter::new(false));
    futures::executor::block_on(crate::commands::run::run_v03_lifecycle_steps(
        &plan,
        &lifecycle_reporter,
        &launch_ctx,
    ))?;
    let guest = parse_guest_contract(
        &manifest_value,
        manifest_path.parent().unwrap_or_else(|| Path::new(".")),
    );
    let info = if let Some(guest) = guest {
        start_guest_session(handle, &resolution, &manifest_path, &plan, guest, notes)?
    } else {
        start_runtime_session(
            handle,
            &resolution,
            &manifest_path,
            &plan,
            &raw_manifest,
            &launch,
            notes,
        )?
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&SessionStartEnvelope {
                schema_version: super::SCHEMA_VERSION,
                package_id: super::DESKY_PACKAGE_ID,
                action: SESSION_ACTION_START,
                session: info,
            })?
        );
    } else {
        print_session_info(&info);
    }

    Ok(())
}

fn start_guest_session(
    handle: &str,
    resolution: &super::resolve::HandleResolution,
    manifest_path: &Path,
    plan: &capsule_core::router::ManifestData,
    guest: super::guest_contract::GuestContract,
    notes: Vec<String>,
) -> Result<SessionInfo> {
    let port = reserve_port(guest.default_port)?;
    let process_manager = ProcessManager::new()?;
    let session_root = session_root()?;
    fs::create_dir_all(&session_root)
        .with_context(|| format!("failed to create session root {}", session_root.display()))?;

    let log_path = session_root.join(format!("session-{port}.log"));
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("failed to open log file {}", log_path.display()))?;
    let stderr = stdout
        .try_clone()
        .with_context(|| format!("failed to clone log file {}", log_path.display()))?;

    let launch = derive_launch_spec(plan).with_context(|| {
        format!(
            "failed to derive launch spec for {}",
            manifest_path.display()
        )
    })?;
    let mut command = Command::new(&launch.command);
    command
        .args(&launch.args)
        .current_dir(&launch.working_dir)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));

    for (key, value) in &launch.env_vars {
        command.env(key, value);
    }
    command.env("PYTHONUNBUFFERED", "1");
    command.env("DESKY_SESSION_PORT", port.to_string());
    command.env("DESKY_SESSION_HOST", "127.0.0.1");
    command.env("DESKY_SESSION_ID", format!("desky-session-{port}"));
    command.env("DESKY_SESSION_ADAPTER", &guest.adapter);
    command.env("DESKY_SESSION_RPC_PATH", &guest.rpc_path);
    command.env("DESKY_SESSION_HEALTH_PATH", &guest.health_path);
    command.env("ATO_GUEST_MODE", "1");

    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to start guest backend '{}' from {}",
            launch.command,
            launch.working_dir.display()
        )
    })?;

    let session_id = format!("desky-session-{}", child.id());
    let runtime = runtime_descriptor(plan);
    let process_info = ProcessInfo {
        id: session_id.clone(),
        name: session_name(plan, "desky-guest"),
        pid: child.id() as i32,
        workload_pid: None,
        status: ProcessStatus::Starting,
        runtime: SESSION_RUNTIME.to_string(),
        start_time: SystemTime::now(),
        manifest_path: Some(manifest_path.to_path_buf()),
        scoped_id: None,
        target_label: Some(plan.selected_target_label().to_string()),
        requested_port: Some(port),
        log_path: Some(log_path.clone()),
        ready_at: None,
        last_event: Some("spawned".to_string()),
        last_error: None,
        exit_code: None,
    };
    process_manager.write_pid(&process_info)?;

    let healthcheck_url = format!("http://127.0.0.1:{}{}", port, guest.health_path);
    let invoke_url = format!("http://127.0.0.1:{}{}", port, guest.rpc_path);

    match wait_for_http_ready(&mut child, port, &guest.health_path, SESSION_READY_TIMEOUT) {
        Ok(()) => {
            let _ = process_manager.update_pid(&session_id, |info| {
                info.status = ProcessStatus::Ready;
                info.ready_at = Some(SystemTime::now());
                info.last_event = Some("ready".to_string());
                info.last_error = None;
            })?;
        }
        Err(err) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = process_manager.update_pid(&session_id, |info| {
                info.status = ProcessStatus::Failed;
                info.last_event = Some("ready_failed".to_string());
                info.last_error = Some(err.to_string());
                info.exit_code = Some(-1);
            })?;
            anyhow::bail!(
                "guest backend failed to become ready: {}. See logs at {}",
                err,
                log_path.display()
            );
        }
    }

    let session = StoredSessionInfo {
        session_id: session_id.clone(),
        handle: handle.to_string(),
        normalized_handle: resolution.normalized_handle.clone(),
        canonical_handle: resolution.canonical_handle.clone(),
        trust_state: resolution.trust_state.clone(),
        source: resolution.source.clone(),
        restricted: resolution.restricted,
        snapshot: resolution.snapshot.clone(),
        runtime: runtime.clone(),
        display_strategy: CapsuleDisplayStrategy::GuestWebview,
        pid: child.id() as i32,
        log_path: log_path.display().to_string(),
        manifest_path: manifest_path.display().to_string(),
        target_label: plan.selected_target_label().to_string(),
        notes,
        guest: Some(GuestSessionDisplay {
            adapter: guest.adapter.clone(),
            frontend_entry: guest.frontend_entry.display().to_string(),
            transport: guest.transport.clone(),
            healthcheck_url: healthcheck_url.clone(),
            invoke_url: invoke_url.clone(),
            capabilities: guest.capabilities.clone(),
        }),
        web: None,
        terminal: None,
        service: None,
    };
    write_session_record(&session_root, &session)?;
    Ok(session_info_from_record(session))
}

fn start_runtime_session(
    handle: &str,
    resolution: &super::resolve::HandleResolution,
    manifest_path: &Path,
    plan: &capsule_core::router::ManifestData,
    raw_manifest: &str,
    launch: &capsule_core::launch_spec::LaunchSpec,
    mut notes: Vec<String>,
) -> Result<SessionInfo> {
    let display_strategy = display_strategy_for_runtime(plan);
    if matches!(display_strategy, CapsuleDisplayStrategy::Unsupported) {
        anyhow::bail!(
            "session start does not support target '{}' (runtime={:?}, driver={:?})",
            plan.selected_target_label(),
            plan.execution_runtime(),
            plan.execution_driver()
        );
    }

    let process_manager = ProcessManager::new()?;
    let session_root = session_root()?;
    fs::create_dir_all(&session_root)
        .with_context(|| format!("failed to create session root {}", session_root.display()))?;
    let prepared = prepare_session_execution(plan, raw_manifest)?;
    let mut runtime_process = spawn_runtime_process(plan, &prepared, &display_strategy)
        .with_context(|| {
            format!(
                "failed to start capsule session for {}",
                manifest_path.display()
            )
        })?;
    let session_id = format!("desky-session-{}", runtime_process.child.id());
    let log_path = session_root.join(format!("{}.log", session_id));
    attach_process_logs(&mut runtime_process.child, &log_path)?;

    let runtime = runtime_descriptor(plan);
    let local_url = if matches!(display_strategy, CapsuleDisplayStrategy::WebUrl) {
        let port = launch.port.ok_or_else(|| {
            anyhow::anyhow!(
                "runtime=web target '{}' requires targets.<label>.port",
                plan.selected_target_label()
            )
        })?;
        let health_path = "/";
        match wait_for_http_ready(
            &mut runtime_process.child,
            port,
            health_path,
            SESSION_READY_TIMEOUT,
        ) {
            Ok(()) => Some(format!("http://127.0.0.1:{port}/")),
            Err(err) => {
                let _ = runtime_process.child.kill();
                let _ = runtime_process.child.wait();
                anyhow::bail!(
                    "web runtime failed to become ready: {}. See logs at {}",
                    err,
                    log_path.display()
                );
            }
        }
    } else {
        None
    };

    if matches!(display_strategy, CapsuleDisplayStrategy::WebUrl) {
        notes.push(format!(
            "Attached runtime=web target '{}' as a capsule-backed web session.",
            plan.selected_target_label()
        ));
    } else {
        notes.push(format!(
            "Attached target '{}' as a non-web capsule session.",
            plan.selected_target_label()
        ));
    }

    let process_info = ProcessInfo {
        id: session_id.clone(),
        name: session_name(plan, "capsule-session"),
        pid: runtime_process.child.id() as i32,
        workload_pid: runtime_process.workload_pid.map(|value| value as i32),
        status: ProcessStatus::Ready,
        runtime: runtime
            .runtime
            .clone()
            .unwrap_or_else(|| "source".to_string()),
        start_time: SystemTime::now(),
        manifest_path: Some(manifest_path.to_path_buf()),
        scoped_id: None,
        target_label: Some(plan.selected_target_label().to_string()),
        requested_port: launch.port,
        log_path: Some(log_path.clone()),
        ready_at: Some(SystemTime::now()),
        last_event: Some("ready".to_string()),
        last_error: None,
        exit_code: None,
    };
    process_manager.write_pid(&process_info)?;

    let session = StoredSessionInfo {
        session_id,
        handle: handle.to_string(),
        normalized_handle: resolution.normalized_handle.clone(),
        canonical_handle: resolution.canonical_handle.clone(),
        trust_state: resolution.trust_state.clone(),
        source: resolution.source.clone(),
        restricted: resolution.restricted,
        snapshot: resolution.snapshot.clone(),
        runtime: runtime.clone(),
        display_strategy: display_strategy.clone(),
        pid: runtime_process.child.id() as i32,
        log_path: log_path.display().to_string(),
        manifest_path: manifest_path.display().to_string(),
        target_label: plan.selected_target_label().to_string(),
        notes,
        guest: None,
        web: local_url.as_ref().map(|url| WebSessionDisplay {
            local_url: url.clone(),
            healthcheck_url: url.clone(),
            served_by: web_served_by(plan),
        }),
        terminal: matches!(display_strategy, CapsuleDisplayStrategy::TerminalStream).then(|| {
            TerminalSessionDisplay {
                log_path: log_path.display().to_string(),
            }
        }),
        service: matches!(display_strategy, CapsuleDisplayStrategy::ServiceBackground).then(|| {
            ServiceBackgroundDisplay {
                log_path: log_path.display().to_string(),
            }
        }),
    };
    write_session_record(&session_root, &session)?;
    Ok(session_info_from_record(session))
}

fn session_info_from_record(session: StoredSessionInfo) -> SessionInfo {
    let guest_compat = session.guest.as_ref().map(|guest| {
        (
            guest.adapter.clone(),
            guest.frontend_entry.clone(),
            guest.transport.clone(),
            guest.healthcheck_url.clone(),
            guest.invoke_url.clone(),
            guest.capabilities.clone(),
        )
    });
    let (adapter, frontend_entry, transport, healthcheck_url, invoke_url, capabilities) =
        guest_compat
            .map(
                |(
                    adapter,
                    frontend_entry,
                    transport,
                    healthcheck_url,
                    invoke_url,
                    capabilities,
                )| {
                    (
                        Some(adapter),
                        Some(frontend_entry),
                        Some(transport),
                        Some(healthcheck_url),
                        Some(invoke_url),
                        Some(capabilities),
                    )
                },
            )
            .unwrap_or((None, None, None, None, None, None));

    SessionInfo {
        session_id: session.session_id,
        handle: session.handle,
        normalized_handle: session.normalized_handle,
        canonical_handle: session.canonical_handle,
        status: "ready".to_string(),
        trust_state: session.trust_state,
        source: session.source,
        restricted: session.restricted,
        snapshot: session.snapshot,
        runtime: session.runtime,
        display_strategy: session.display_strategy,
        pid: session.pid,
        log_path: session.log_path,
        manifest_path: session.manifest_path,
        target_label: session.target_label,
        notes: session.notes,
        adapter,
        frontend_entry,
        transport,
        healthcheck_url,
        invoke_url,
        capabilities,
        guest: session.guest,
        web: session.web,
        terminal: session.terminal,
        service: session.service,
    }
}

fn resolve_session_launch_plan(
    handle: &str,
    target_label: Option<&str>,
) -> Result<(
    PathBuf,
    capsule_core::router::ManifestData,
    capsule_core::launch_spec::LaunchSpec,
    Vec<String>,
)> {
    let resolved_path = match normalize_capsule_handle(handle) {
        Ok(canonical) => {
            let cli_ref = canonical
                .to_cli_ref()
                .ok_or_else(|| anyhow::anyhow!("handle cannot be launched through ato-cli"))?;
            let registry_override = canonical.registry_url_override().map(str::to_string);
            let reporter = Arc::new(reporters::CliReporter::new(true));
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            rt.block_on(resolve_run_target_or_install(
                PathBuf::from(cli_ref),
                true,
                ProviderToolchain::Auto,
                false,
                None,
                false,
                registry_override.as_deref(),
                reporter,
            ))?
            .path
        }
        Err(_) => PathBuf::from(handle),
    };

    let manifest_path = if resolved_path.is_dir() {
        resolved_path.join("capsule.toml")
    } else if let Some(manifest_path) =
        runtime_tree::prepare_store_runtime_for_capsule(&resolved_path)?
    {
        manifest_path
    } else {
        resolved_path.clone()
    };

    let (plan, _guest, notes) = resolve_local_plan(&manifest_path, target_label)?;
    let launch = derive_launch_spec(&plan).with_context(|| {
        format!(
            "failed to derive launch spec for {}",
            manifest_path.display()
        )
    })?;
    Ok((manifest_path, plan, launch, notes))
}

/// Try to load `ato.lock.json` from the workspace root.
/// This is the authoritative lock that `ato run` generates via source-inference.
/// Without it, `guard.rs` rejects Tier1 execution because
/// `has_authoritative_lock = false` and no physical `capsule.lock.json` exists.
fn try_load_authoritative_lock(
    workspace_root: &Path,
) -> (Option<ato_lock::AtoLock>, Option<PathBuf>) {
    let lock_path = workspace_root.join(ATO_LOCK_FILE_NAME);
    if !lock_path.exists() {
        return (None, None);
    }
    match ato_lock::load_unvalidated_from_path(&lock_path) {
        Ok(lock) => (Some(lock), Some(lock_path)),
        Err(err) => {
            tracing::warn!(
                path = %lock_path.display(),
                error = %err,
                "failed to load ato.lock.json — session will proceed without authoritative lock"
            );
            (None, None)
        }
    }
}

fn prepare_session_execution(
    plan: &capsule_core::router::ManifestData,
    raw_manifest: &str,
) -> Result<crate::executors::target_runner::PreparedTargetExecution> {
    let reporter = Arc::new(CliReporter::new(false));
    let (authoritative_lock, lock_path) = try_load_authoritative_lock(&plan.workspace_root);
    let prepared = PreparedRunContext {
        authoritative_lock,
        lock_path,
        workspace_root: plan.workspace_root.clone(),
        effective_state: None,
        execution_override: None,
        bridge_manifest: DerivedBridgeManifest::new(
            toml::from_str(raw_manifest)
                .with_context(|| format!("failed to parse {}", plan.manifest_path.display()))?,
        ),
        validation_mode: capsule_core::types::ValidationMode::Strict,
        engine_override_declared: false,
        compatibility_legacy_lock: None,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create runtime for session execution preparation")?;
    let launch_ctx = runtime.block_on(resolve_launch_context(plan, &prepared, &reporter))?;
    prepare_target_execution(
        plan,
        &prepared,
        launch_ctx,
        &TargetLaunchOptions {
            // source/python requires "strict" enforcement (guard.rs policy).
            // execute_host runs Python directly on the host — nacelle is not required.
            enforcement: "strict".to_string(),
            sandbox_mode: true,
            dangerously_skip_permissions: false,
            assume_yes: true,
            preview_mode: false,
            defer_consent: true,
        },
    )
}

fn spawn_runtime_process(
    plan: &capsule_core::router::ManifestData,
    prepared: &crate::executors::target_runner::PreparedTargetExecution,
    display_strategy: &CapsuleDisplayStrategy,
) -> Result<CapsuleProcess> {
    if matches!(display_strategy, CapsuleDisplayStrategy::WebUrl) {
        let driver = plan
            .execution_driver()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        return match driver.as_str() {
            "static" => Ok(CapsuleProcess {
                child: crate::executors::open_web::spawn_background(plan)?,
                cleanup_paths: Vec::new(),
                event_rx: None,
                workload_pid: None,
                log_path: None,
            }),
            "deno" => Ok(CapsuleProcess {
                child: crate::executors::deno::spawn(
                    plan,
                    None,
                    &prepared.execution_plan,
                    &prepared.launch_ctx,
                    false,
                )?,
                cleanup_paths: Vec::new(),
                event_rx: None,
                workload_pid: None,
                log_path: None,
            }),
            "node" => Ok(CapsuleProcess {
                child: crate::executors::node_compat::spawn(
                    plan,
                    None,
                    &prepared.execution_plan,
                    &prepared.launch_ctx,
                    false,
                )?,
                cleanup_paths: Vec::new(),
                event_rx: None,
                workload_pid: None,
                log_path: None,
            }),
            "python" => crate::executors::source::execute_host(
                plan,
                None,
                Arc::new(CliReporter::new(false)),
                ExecuteMode::Piped,
                &prepared.launch_ctx,
            ),
            _ => anyhow::bail!("unsupported runtime=web driver '{driver}' for session start"),
        };
    }

    if plan.is_orchestration_mode() {
        return crate::executors::shell::execute(plan, ExecuteMode::Piped, &prepared.launch_ctx)
            .or_else(|_| {
                crate::executors::source::execute_host(
                    plan,
                    None,
                    Arc::new(CliReporter::new(false)),
                    ExecuteMode::Piped,
                    &prepared.launch_ctx,
                )
            });
    }

    match prepared.guard_result.executor_kind {
        capsule_core::execution_plan::guard::ExecutorKind::Deno => Ok(CapsuleProcess {
            child: crate::executors::deno::spawn(
                plan,
                None,
                &prepared.execution_plan,
                &prepared.launch_ctx,
                false,
            )?,
            cleanup_paths: Vec::new(),
            event_rx: None,
            workload_pid: None,
            log_path: None,
        }),
        capsule_core::execution_plan::guard::ExecutorKind::NodeCompat => Ok(CapsuleProcess {
            child: crate::executors::node_compat::spawn(
                plan,
                None,
                &prepared.execution_plan,
                &prepared.launch_ctx,
                false,
            )?,
            cleanup_paths: Vec::new(),
            event_rx: None,
            workload_pid: None,
            log_path: None,
        }),
        capsule_core::execution_plan::guard::ExecutorKind::WebStatic => Ok(CapsuleProcess {
            child: crate::executors::open_web::spawn_background(plan)?,
            cleanup_paths: Vec::new(),
            event_rx: None,
            workload_pid: None,
            log_path: None,
        }),
        _ if plan.execution_run_command().is_some() => {
            crate::executors::shell::execute(plan, ExecuteMode::Piped, &prepared.launch_ctx)
        }
        _ => crate::executors::source::execute_host(
            plan,
            None,
            Arc::new(CliReporter::new(false)),
            ExecuteMode::Piped,
            &prepared.launch_ctx,
        ),
    }
}

fn display_strategy_for_runtime(
    plan: &capsule_core::router::ManifestData,
) -> CapsuleDisplayStrategy {
    if plan.is_orchestration_mode() {
        return CapsuleDisplayStrategy::ServiceBackground;
    }

    if plan
        .execution_runtime()
        .is_some_and(|runtime| runtime.eq_ignore_ascii_case("web"))
    {
        return CapsuleDisplayStrategy::WebUrl;
    }

    // Any target that publishes an HTTP port is a web app — the host
    // should open a WebView pointed at it instead of a log-tail
    // terminal. Without this, capsules like `runtime=source,
    // driver=node, port=3000` (a typical Node web app) fall through
    // to TerminalStream and the user sees process logs instead of
    // the served UI.
    if plan.execution_port().is_some() {
        return CapsuleDisplayStrategy::WebUrl;
    }

    CapsuleDisplayStrategy::TerminalStream
}

fn runtime_descriptor(plan: &capsule_core::router::ManifestData) -> CapsuleRuntimeDescriptor {
    CapsuleRuntimeDescriptor {
        target_label: plan.selected_target_label().to_string(),
        runtime: plan.execution_runtime(),
        driver: plan.execution_driver(),
        language: plan.execution_language(),
        port: plan.execution_port(),
    }
}

fn session_name(plan: &capsule_core::router::ManifestData, fallback: &str) -> String {
    plan.manifest
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or(fallback)
        .to_string()
}

fn web_served_by(plan: &capsule_core::router::ManifestData) -> String {
    let driver = plan.execution_driver().unwrap_or_else(|| "web".to_string());
    match driver.to_ascii_lowercase().as_str() {
        "static" => "deno-static-server".to_string(),
        value => value.to_string(),
    }
}

fn attach_process_logs(child: &mut std::process::Child, log_path: &Path) -> Result<()> {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let writer = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("failed to open log file {}", log_path.display()))?;
    let stderr_writer = writer
        .try_clone()
        .with_context(|| format!("failed to clone log file {}", log_path.display()))?;

    if let Some(mut stdout) = stdout {
        let mut writer = writer;
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut stdout, &mut writer);
        });
    }
    if let Some(mut stderr) = stderr {
        let mut writer = stderr_writer;
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut stderr, &mut writer);
        });
    }
    Ok(())
}

pub fn stop_session(session_id: &str, json: bool) -> Result<()> {
    let process_manager = ProcessManager::new()?;
    let stopped = process_manager.stop_process(session_id, true)?;
    let session_path = session_root()?.join(format!("{session_id}.json"));
    if session_path.exists() {
        fs::remove_file(&session_path)
            .with_context(|| format!("failed to remove session file {}", session_path.display()))?;
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&SessionStopEnvelope {
                schema_version: super::SCHEMA_VERSION,
                package_id: super::DESKY_PACKAGE_ID,
                action: SESSION_ACTION_STOP,
                session_id: session_id.to_string(),
                stopped,
            })?
        );
        return Ok(());
    }

    if stopped {
        println!("Stopped session: {session_id}");
    } else {
        println!("Session was not active: {session_id}");
    }
    Ok(())
}

fn session_root() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("DESKY_SESSION_ROOT") {
        return Ok(PathBuf::from(path));
    }
    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("failed to resolve home directory"))?;
    Ok(home
        .join(".ato")
        .join("apps")
        .join("desky")
        .join("sessions"))
}

fn write_session_record(root: &Path, session: &StoredSessionInfo) -> Result<()> {
    let path = root.join(format!("{}.json", session.session_id));
    fs::write(&path, serde_json::to_vec_pretty(session)?)
        .with_context(|| format!("failed to write session file {}", path.display()))
}

fn reserve_port(default_port: Option<u16>) -> Result<u16> {
    if let Some(port) = default_port {
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }

    let listener = TcpListener::bind(("127.0.0.1", 0)).context("failed to allocate local port")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

fn wait_for_http_ready(
    child: &mut std::process::Child,
    port: u16,
    path: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("process exited before readiness with status {status}");
        }

        if http_get_ok(port, path) {
            return Ok(());
        }

        if std::time::Instant::now() >= deadline {
            anyhow::bail!("readiness timed out for http://127.0.0.1:{port}{path}");
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

fn http_get_ok(port: u16, path: &str) -> bool {
    // Treat any I/O hiccup (EAGAIN/ECONNRESET/timeout) as "not ready
    // yet" so the caller keeps polling. The previous version
    // propagated `?` errors out of the probe, which surfaced the
    // first transient socket error (EAGAIN once the listener was
    // bound but the accept queue hadn't drained yet) as a permanent
    // "web runtime failed to become ready" — even when the child
    // process printed "Ready" milliseconds earlier.
    let Ok(mut stream) = std::net::TcpStream::connect(("127.0.0.1", port)) else {
        return false;
    };
    if stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .is_err()
        || stream
            .set_write_timeout(Some(Duration::from_secs(1)))
            .is_err()
    {
        return false;
    }
    if write!(
        stream,
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        path
    )
    .is_err()
        || stream.flush().is_err()
    {
        return false;
    }

    let mut response = String::new();
    if stream.read_to_string(&mut response).is_err() {
        return false;
    }
    response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200")
}

fn print_session_info(info: &SessionInfo) {
    println!("Session: {}", info.session_id);
    println!("Handle: {}", info.handle);
    println!("Display: {}", info.display_strategy.as_str());
    if let Some(runtime) = info.runtime.runtime.as_deref() {
        println!("Runtime: {runtime}");
    }
    if let Some(web) = info.web.as_ref() {
        println!("URL: {}", web.local_url);
        println!("Health URL: {}", web.healthcheck_url);
    }
    if let Some(guest) = info.guest.as_ref() {
        println!("Adapter: {}", guest.adapter);
        println!("Frontend: {}", guest.frontend_entry);
        println!("Invoke URL: {}", guest.invoke_url);
        println!("Health URL: {}", guest.healthcheck_url);
    }
    if let Some(terminal) = info.terminal.as_ref() {
        println!("Log: {}", terminal.log_path);
    }
    if let Some(service) = info.service.as_ref() {
        println!("Log: {}", service.log_path);
    }
    println!("PID: {}", info.pid);
    println!("Log: {}", info.log_path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::handle::normalize_capsule_handle;

    #[test]
    fn reserve_port_returns_requested_port_when_available() {
        let port = reserve_port(Some(43291)).expect("reserve port");
        assert_eq!(port, 43291);
    }

    #[test]
    fn session_start_envelope_serializes_snapshot_and_frontend_entry() {
        let envelope = SessionStartEnvelope {
            schema_version: super::super::SCHEMA_VERSION,
            package_id: "ato.desktop",
            action: SESSION_ACTION_START,
            session: SessionInfo {
                session_id: "desky-session-1".to_string(),
                handle: "capsule://ato.run/koh0920/ato-onboarding".to_string(),
                normalized_handle: "capsule://ato.run/koh0920/ato-onboarding".to_string(),
                canonical_handle: Some("capsule://ato.run/koh0920/ato-onboarding".to_string()),
                status: "ready".to_string(),
                trust_state: TrustState::Untrusted,
                source: Some("registry".to_string()),
                restricted: true,
                snapshot: Some(ResolvedSnapshot::RegistryRelease {
                    version: "0.1.0".to_string(),
                    release_id: None,
                    content_hash: Some("sha256:abc123".to_string()),
                    fetched_at: "2026-04-09T00:00:00Z".to_string(),
                }),
                runtime: CapsuleRuntimeDescriptor {
                    target_label: "web".to_string(),
                    runtime: Some("source".to_string()),
                    driver: Some("tauri".to_string()),
                    language: Some("tauri".to_string()),
                    port: Some(9000),
                },
                display_strategy: CapsuleDisplayStrategy::GuestWebview,
                pid: 42,
                log_path: "/tmp/desky-session.log".to_string(),
                manifest_path: "/tmp/capsule.toml".to_string(),
                target_label: "web".to_string(),
                notes: vec!["materialized".to_string()],
                adapter: Some("tauri".to_string()),
                frontend_entry: Some("dist/index.html".to_string()),
                transport: Some("http".to_string()),
                healthcheck_url: Some("http://127.0.0.1:9000/health".to_string()),
                invoke_url: Some("http://127.0.0.1:9000/rpc".to_string()),
                capabilities: Some(vec!["read-file".to_string()]),
                guest: Some(GuestSessionDisplay {
                    adapter: "tauri".to_string(),
                    frontend_entry: "dist/index.html".to_string(),
                    transport: "http".to_string(),
                    healthcheck_url: "http://127.0.0.1:9000/health".to_string(),
                    invoke_url: "http://127.0.0.1:9000/rpc".to_string(),
                    capabilities: vec!["read-file".to_string()],
                }),
                web: None,
                terminal: None,
                service: None,
            },
        };

        let json = serde_json::to_value(&envelope).expect("serialize envelope");
        assert_eq!(
            json["session"]["snapshot"]["version"],
            serde_json::json!("0.1.0")
        );
        assert_eq!(
            json["session"]["guest"]["frontend_entry"],
            serde_json::json!("dist/index.html")
        );
        assert_eq!(
            json["session"]["manifest_path"],
            serde_json::json!("/tmp/capsule.toml")
        );
        assert_eq!(json["session"]["source"], serde_json::json!("registry"));
        assert_eq!(
            json["session"]["display_strategy"],
            serde_json::json!("guest_webview")
        );
        // CCP v0.5 wire-contract regression: schema_version must be `ccp/v1`.
        // See `docs/specs/CCP_SPEC.md` for the additive-only versioning rule.
        assert_eq!(json["schema_version"], serde_json::json!("ccp/v1"));
    }

    #[test]
    fn ccp_schema_version_is_canonical_v1() {
        // Wire-contract pin: prevents accidental rename or version bump within v1.
        // Bumping to ccp/v2 is a major-version event requiring desktop coordination.
        assert_eq!(super::super::SCHEMA_VERSION, "ccp/v1");
    }

    #[test]
    fn loopback_registry_handle_exposes_registry_override_for_materialization() {
        let canonical =
            normalize_capsule_handle("capsule://localhost:8787/acme/chat").expect("canonical");
        assert_eq!(canonical.to_cli_ref().as_deref(), Some("acme/chat"));
        assert_eq!(
            canonical.registry_url_override(),
            Some("http://localhost:8787")
        );
    }
}
