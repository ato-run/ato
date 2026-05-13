use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
// Session record schema and atomic writer now live in `ato-session-core`
// so `ato-desktop` can read records without depending on `ato-cli`. We
// re-export at `pub(crate)` so the rest of this crate continues to see
// these names without prefix.
pub(crate) use ato_session_core::{
    write_session_record_atomic, GuestSessionDisplay, ServiceBackgroundDisplay,
    StoredDependencyContracts, StoredDependencyProvider, StoredOrchestrationService,
    StoredOrchestrationServices, StoredSessionInfo, TerminalSessionDisplay, WebSessionDisplay,
};
use capsule_core::ato_lock;
use capsule_core::handle::{
    normalize_capsule_handle, CapsuleDisplayStrategy, CapsuleRuntimeDescriptor, ResolvedSnapshot,
    TrustState,
};
use capsule_core::launch_spec::derive_launch_spec;
use capsule_core::routing::input_resolver::ATO_LOCK_FILE_NAME;
use serde::Serialize;

use crate::application::pipeline::phases::run::{
    persist_background_dependency_contracts, setup_dependency_contracts_launch_context,
    DependencyContractGuard, DerivedBridgeManifest, PreparedRunContext,
};
use crate::application::session_graph_populate::{EDGE_KIND_PROVIDES, NODE_KIND_PROVIDER};
use crate::executors::source::{CapsuleProcess, ExecuteMode};
use crate::executors::target_runner::{
    prepare_target_execution, resolve_launch_context, TargetLaunchOptions,
};
use crate::install::support::resolve_run_target_or_install;
use crate::reporters;
use crate::reporters::CliReporter;
use crate::runtime::process::{ProcessInfo, ProcessManager, ProcessStatus};
use crate::runtime::tree as runtime_tree;
use crate::ProviderToolchain;

use super::resolve::resolve_local_plan;

const SESSION_ACTION_START: &str = "session_start";
const SESSION_ACTION_STOP: &str = "session_stop";
const SESSION_RUNTIME: &str = "ato-desktop-session";
const DESKTOP_PARENT_PID_ENV: &str = "ATO_DESKTOP_PARENT_PID";
const DESKTOP_PARENT_START_TIME_ENV: &str = "ATO_DESKTOP_PARENT_START_TIME_UNIX_MS";

/// Build a reporter for orchestration-session helpers. In envelope
/// mode (set by `start_session(json=true)` on the orchestrator's
/// stream-pumper redirect flag), notifications must NOT go to stdout
/// — `stdout` is reserved for the SessionStartEnvelope JSON the
/// caller (today: ato-desktop) parses. `CliReporter::new_run(false)`
/// is the existing constructor that pins TextReporter to **stderr**
/// (already used by `ato run` foreground for the same reason).
///
/// Outside envelope mode (interactive `ato app session start` from a
/// terminal), keep the historical stdout-going TextReporter so
/// notifications surface alongside the human-readable summary
/// printed by `print_session_info`.
fn make_orchestration_reporter() -> CliReporter {
    if crate::adapters::runtime::executors::orchestrator::redirect_service_stdout_to_stderr_for_envelope_mode_active() {
        CliReporter::new_run(false)
    } else {
        CliReporter::new(false)
    }
}

/// Resolve the readiness budget for `wait_for_http_ready` from the
/// manifest's `startup_timeout` (per-target → global → 60s default).
/// The previous code path used a hardcoded 10s ceiling and silently
/// ignored the manifest field, which timed out heavy first-launch
/// capsules (Argos/MiniSBD model downloads, build caches, etc.) on
/// their own declared budget.
fn session_ready_timeout(plan: &capsule_core::router::ManifestData) -> Duration {
    Duration::from_secs(plan.execution_startup_timeout() as u64)
}

// `orchestration_supervisor_ready_timeout` and its 180s floor were removed
// in #73 PR-C. The floor only existed because the nested `ato run`
// supervisor ran materialize/build/health checks serialized inside the
// child, so the wrapper had to wait long enough to cover all of them.
// The in-process path (`start_orchestration_session_in_process`) waits per
// service through `ServicePhaseCoordinator` and uses `session_ready_timeout`
// uniformly. The legacy supervisor path (`ATO_LEGACY_SUPERVISOR=1`) also
// uses `session_ready_timeout` now; if that path is exercised on a slow
// host the per-target `startup_timeout` should be raised in the manifest.

/// Interval between HTTP readiness polls while waiting for a freshly spawned
/// session to bind its port. The value trades worst-case wasted wait time
/// against syscall churn: 25ms is short enough that even fast servers
/// (`next start` becomes ready in ~530ms standalone) lose <25ms on average,
/// while still being far above the kernel's connect-syscall granularity.
/// Paired with a per-attempt read/write timeout of 1s in `wait_for_http_ready`,
/// so a hung connect cannot stall progress beyond that.
const SESSION_READY_POLL_INTERVAL: Duration = Duration::from_millis(25);

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
    /// `execution_id` of the v1 or v2 execution receipt emitted alongside the
    /// session start. Lets the desktop UI cross-reference the session with the
    /// portable launch identity stored under `~/.ato/executions/`.
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_id: Option<String>,
    /// `schema_version` of the emitted execution receipt (1 or 2). Surfaced so
    /// the desktop UI can label which identity model the session is currently
    /// running under during the v2 migration window.
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_receipt_schema_version: Option<u32>,
}

impl SessionInfo {
    /// PID of the spawned process. Used by the App Session Materialization
    /// layer to enrich the freshly-written record with its process_start_time.
    pub(crate) fn pid(&self) -> i32 {
        self.pid
    }

    /// Attach the execution receipt identity emitted for this session. Called
    /// by the session start runner after `build_prelaunch_receipt_document`
    /// writes the receipt to `~/.ato/executions/`.
    pub(crate) fn attach_execution_receipt(&mut self, execution_id: String, schema_version: u32) {
        self.execution_id = Some(execution_id);
        self.execution_receipt_schema_version = Some(schema_version);
    }
}

// On-disk session record schema lives in `ato-session-core` (see top-of-
// file `pub(crate) use`). Keep this comment as a back-pointer because
// `git blame` for this file should still surface the design rationale:
// schema is forward-compatible, schema_version < 2 records are
// reuse-ineligible. Refactor of v0.4 (PR 4A.0 — RFC §3.2) moved the
// types out so `ato-desktop` can read records without depending on
// `ato-cli`.

pub fn start_session(handle: &str, target_label: Option<&str>, json: bool) -> Result<()> {
    // Reserve stdout for the SessionStartEnvelope when the caller
    // asked for JSON. Without this, the orchestrator's stream pumper
    // (`adapters/runtime/executors/orchestrator.rs::spawn_prefixed_stream`)
    // writes service stdout (`[main] ...` / `[web] ...`) onto the
    // parent's stdout while the orchestration ramps up, so the
    // captured stdout becomes
    //   `[main] ...service output...\n{...envelope...}`
    // which fails JSON parsing at column 2 (`m` of `[main]`). The
    // desktop, which spawns `ato app session start --json`, then
    // reports `failed to parse session start response: expected
    // value at line 1 column 2` and the launch dead-ends.
    //
    // The redirect routes service stdout to the parent's stderr
    // (with the same `[<service>] ` prefix), so the JSON envelope
    // is the only thing on stdout. `ato run` foreground use is
    // unaffected — it never sets this flag.
    crate::adapters::runtime::executors::orchestrator::redirect_service_stdout_to_stderr_for_envelope_mode(json);

    // Drive the same Hourglass pipeline `ato run` uses, with a
    // `SessionStartPhaseRunner` that swaps Execute for session-specific
    // spawn + ProcessManager registration. Install resolves the handle,
    // Build invokes the materialization layer (so warm starts skip
    // `next build`), and Execute populates a `SessionInfo` we emit as
    // the Desktop's session envelope below.
    //
    // Prepare / Verify / DryRun are no-op for v0; the runner reports
    // them as `result_kind=not-applicable` in PHASE-TIMING so the
    // diagnostic stream stays distinguishable from `ato run`.
    use crate::application::pipeline::consumer::ConsumerRunPipeline;

    // #80: sweep stale PID/socket records from `~/.ato/run/` BEFORE we
    // write our own. Reaps:
    //   - `<id>.pid` files for processes that have exited (existing
    //     `cleanup_dead_processes_with_details` behavior, just hooked
    //     here on session start instead of only on `ato ps`).
    //   - `*.sock.txt` artifacts left behind by older socket-discovery
    //     code paths.
    // ato-desktop's own `*.sock` files are reaped by its automation
    // transport layer (#68) before binding, so the two cleanups are
    // complementary and not redundant. Failures are best-effort —
    // logged via tracing and never abort session start.
    if let Ok(pm) = ProcessManager::new() {
        match pm.sweep_run_dir_orphans() {
            Ok(report) => {
                if report.pid_files_removed > 0 || report.sockets_removed > 0 {
                    tracing::debug!(
                        pid_files = report.pid_files_removed,
                        sockets = report.sockets_removed,
                        "session start sweep removed stale run-dir entries"
                    );
                }
            }
            Err(error) => {
                tracing::debug!(error = %error, "session start sweep failed (best-effort)");
            }
        }
    }

    let mut runner =
        super::session_runner::SessionStartPhaseRunner::new(handle, target_label, json);
    let pipeline = ConsumerRunPipeline::standard();
    // Boundary-level receipt emission (refs #74, #99). On the happy
    // path the pipeline emits its own full v2 receipt before spawn
    // (see `SessionStartPhaseRunner::emit_execution_receipt`); on the
    // failure path the wrapper synthesizes a partial receipt with
    // the typed `AtoExecutionError` envelope so `~/.ato/executions/`
    // contains a record for every session-start attempt.
    let ctx = crate::application::receipt_boundary::ReceiptEmissionContext::for_boundary(
        "ato app session start",
    );
    futures::executor::block_on(
        crate::application::receipt_boundary::emit_receipt_on_result(ctx, async {
            pipeline.run(&mut runner).await
        }),
    )?;

    let info = runner
        .session_info
        .ok_or_else(|| anyhow::anyhow!("session start pipeline did not populate session info"))?;

    if let Err(err) = maybe_spawn_parent_death_watcher(&info.session_id) {
        eprintln!(
            "ATO-WARN failed to start parent-death watcher for {}: {}",
            info.session_id, err
        );
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&SessionStartEnvelope {
                schema_version: super::SCHEMA_VERSION,
                package_id: super::ATO_DESKTOP_PACKAGE_ID,
                action: SESSION_ACTION_START,
                session: info,
            })?
        );
    } else {
        print_session_info(&info);
    }

    Ok(())
}

pub(super) fn start_guest_session(
    handle: &str,
    resolution: &super::resolve::HandleResolution,
    manifest_path: &Path,
    plan: &capsule_core::router::ManifestData,
    guest: super::guest_contract::GuestContract,
    notes: Vec<String>,
) -> Result<SessionInfo> {
    use crate::application::pipeline::executor::PhaseStageTimer;
    use crate::application::pipeline::hourglass::HourglassPhase;

    let timer = PhaseStageTimer::start(HourglassPhase::Execute, "reserve_port");
    let port = reserve_port(guest.default_port)?;
    timer.finish_ok();

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
    command.env("ATO_DESKTOP_SESSION_PORT", port.to_string());
    command.env("ATO_DESKTOP_SESSION_HOST", "127.0.0.1");
    command.env(
        "ATO_DESKTOP_SESSION_ID",
        format!("ato-desktop-session-{port}"),
    );
    command.env("ATO_DESKTOP_SESSION_ADAPTER", &guest.adapter);
    command.env("ATO_DESKTOP_SESSION_RPC_PATH", &guest.rpc_path);
    command.env("ATO_DESKTOP_SESSION_HEALTH_PATH", &guest.health_path);
    command.env("ATO_GUEST_MODE", "1");

    let timer = PhaseStageTimer::start(HourglassPhase::Execute, "spawn_guest_process");
    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to start guest backend '{}' from {}",
            launch.command,
            launch.working_dir.display()
        )
    })?;
    timer.finish_ok();

    let session_id = format!("ato-desktop-session-{}", child.id());
    let runtime = runtime_descriptor(plan);
    let process_info = ProcessInfo {
        id: session_id.clone(),
        name: session_name(plan, "ato-desktop-guest"),
        pid: child.id() as i32,
        workload_pid: None,
        status: ProcessStatus::Starting,
        runtime: SESSION_RUNTIME.to_string(),
        start_time: SystemTime::now(),
        os_start_time_unix_ms: ato_session_core::process::process_start_time_unix_ms(child.id()),
        workload_os_start_time_unix_ms: None,
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
    let timer = PhaseStageTimer::start(HourglassPhase::Execute, "write_pid");
    process_manager.write_pid(&process_info)?;
    timer.finish_ok();

    let healthcheck_url = format!("http://127.0.0.1:{}{}", port, guest.health_path);
    let invoke_url = format!("http://127.0.0.1:{}{}", port, guest.rpc_path);

    let timer = PhaseStageTimer::start(HourglassPhase::Execute, "wait_http_ready");
    let ready_result = wait_for_http_ready(
        &mut child,
        port,
        &guest.health_path,
        session_ready_timeout(plan),
    );
    timer.finish_ok();
    match ready_result {
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

    let timer = PhaseStageTimer::start(HourglassPhase::Execute, "write_session_record");
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
        dependency_contracts: None,
        graph: None,
        orchestration_services: None,
        // App Session Materialization: filled in by run_execute after spawn
        // succeeds (start_time helper takes the freshly-spawned PID + the
        // launch_digest computed before the lock was acquired). Leaving them
        // None here keeps the inner spawn logic decoupled from the
        // materialization layer and matches the legacy schema=1 record shape
        // that older versions of ato-cli expect to read.
        schema_version: None,
        launch_digest: None,
        process_start_time_unix_ms: None,
    };
    write_session_record(&session_root, &session)?;
    timer.finish_ok();
    Ok(session_info_from_stored(session))
}

pub(super) fn start_runtime_session(
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

    use crate::application::pipeline::executor::PhaseStageTimer;
    use crate::application::pipeline::hourglass::HourglassPhase;

    let process_manager = ProcessManager::new()?;
    let session_root = session_root()?;
    fs::create_dir_all(&session_root)
        .with_context(|| format!("failed to create session root {}", session_root.display()))?;

    let timer = PhaseStageTimer::start(HourglassPhase::Execute, "prepare_session_execution");
    let PreparedSessionExecution {
        prepared,
        dep_contracts,
    } = prepare_session_execution(plan, raw_manifest)?;
    timer.finish_ok();

    // Pre-open the log file under a temporary name so we can wire
    // `Stdio::from(file)` onto the child at spawn time. This replaces the
    // older proxy-thread pattern in `attach_process_logs`, which dropped
    // child output the moment `ato app session start` exited (the threads
    // doing `io::copy` died with the parent process and the kernel sent
    // EPIPE to the child's stdout).
    //
    // The temp suffix is the parent ato process's PID — unique per
    // invocation, so concurrent `ato app session start` calls don't
    // collide on the same temp file. After the child spawns we rename to
    // the canonical `ato-desktop-session-<child_pid>.log` path; rename of
    // an open file is fine on POSIX (the inode is preserved) and on
    // Windows when the file was opened with default share modes.
    let temp_log_path = session_root.join(format!(".tmp-spawn-{}.log", std::process::id()));
    let _ = fs::remove_file(&temp_log_path);

    let timer = PhaseStageTimer::start(HourglassPhase::Execute, "spawn_runtime_process");
    let mut runtime_process =
        spawn_runtime_process(plan, &prepared, &display_strategy, &temp_log_path).with_context(
            || {
                format!(
                    "failed to start capsule session for {}",
                    manifest_path.display()
                )
            },
        )?;
    timer.finish_ok();

    let session_id = format!("ato-desktop-session-{}", runtime_process.child.id());
    let log_path = session_root.join(format!("{}.log", session_id));
    // Some executor kinds (deno, node_compat, web/static) don't honour
    // `ExecuteMode::Logged` because they own their own stdio routing —
    // they never opened `temp_log_path` at all. Treat the rename's ENOENT
    // as "this executor doesn't write a log" and just touch an empty file
    // so the desktop's log-tail UI has a stable path to read from.
    if temp_log_path.exists() {
        if let Err(err) = fs::rename(&temp_log_path, &log_path) {
            let _ = runtime_process.child.kill();
            let _ = runtime_process.child.wait();
            return Err(anyhow::Error::new(err).context(format!(
                "failed to rename session log {} -> {}",
                temp_log_path.display(),
                log_path.display()
            )));
        }
    } else {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("failed to create empty log file {}", log_path.display()))?;
    }

    let runtime = runtime_descriptor(plan);
    let local_url = if matches!(display_strategy, CapsuleDisplayStrategy::WebUrl) {
        let port = launch.port.ok_or_else(|| {
            anyhow::anyhow!(
                "runtime=web target '{}' requires targets.<label>.port",
                plan.selected_target_label()
            )
        })?;
        let health_path = "/";
        let timer = PhaseStageTimer::start(HourglassPhase::Execute, "wait_http_ready");
        let ready_result = wait_for_http_ready(
            &mut runtime_process.child,
            port,
            health_path,
            session_ready_timeout(plan),
        );
        timer.finish_ok();
        match ready_result {
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
        os_start_time_unix_ms: ato_session_core::process::process_start_time_unix_ms(
            runtime_process.child.id(),
        ),
        workload_os_start_time_unix_ms: runtime_process
            .workload_pid
            .and_then(ato_session_core::process::process_start_time_unix_ms),
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
    let timer = PhaseStageTimer::start(HourglassPhase::Execute, "write_pid");
    process_manager.write_pid(&process_info)?;
    timer.finish_ok();

    let timer = PhaseStageTimer::start(HourglassPhase::Execute, "write_session_record");
    let dependency_contracts = dependency_contracts_for_session_record(
        runtime_process.child.id() as i32,
        dep_contracts.as_ref(),
    );
    // Slice A of #125 (umbrella #74): populate the persisted ExecutionGraph
    // subset alongside `dependency_contracts`. Write-only — teardown still
    // reads `dependency_contracts`. Parity is enforced in debug builds by
    // the populator's internal `debug_assert!`.
    let graph =
        crate::application::session_graph_populate::populate_graph_from_dependency_contracts(
            dependency_contracts.as_ref(),
        );
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
        dependency_contracts,
        graph,
        // Single-target session (no `[services]`); orchestration_services
        // is populated only by start_orchestration_session_in_process.
        orchestration_services: None,
        // App Session Materialization: see note on the guest variant above.
        schema_version: None,
        launch_digest: None,
        process_start_time_unix_ms: None,
    };
    write_session_record(&session_root, &session)?;
    timer.finish_ok();

    // Persist a sidecar snapshot of any started dep-contract providers
    // (postgres, redis, …) before we detach so `ato ps` / `ato stop`
    // can find and tear them down later. Then detach the
    // DependencyContractGuard so its `Drop` does NOT SIGTERM the
    // providers when this fn returns — the consumer process needs
    // them to outlive ato-desktop's session-start invocation.
    let session_id_for_snapshot = session.session_id.clone();
    let consumer_pid = runtime_process.child.id() as i32;
    if let Err(err) = persist_background_dependency_contracts(
        &session_id_for_snapshot,
        consumer_pid,
        dep_contracts.as_ref(),
    ) {
        eprintln!(
            "ATO-WARN failed to persist session dependency snapshot ({}): {}",
            session_id_for_snapshot, err
        );
    }
    if let Some(guard) = dep_contracts {
        guard.detach();
    }

    Ok(session_info_from_stored(session))
}

/// In-process orchestration session start (#73 PR-C).
///
/// Replaces the opaque nested `ato run` supervisor on the normal path: the
/// session layer drives the same dependency-contract setup that single-target
/// session start uses, then calls `executors::orchestrator::execute_until_ready_and_detach`
/// to bring the `[services]` graph up through `ServicePhaseCoordinator` (the
/// same coordinator `ato run` orchestration mode uses) without entering
/// `monitor_until_exit`. The returned `DetachedOrchestrationServices` and the
/// `DependencyContractGuard` are kept alive across `start_session` return via
/// `mem::forget` — PR-D replaces both with a session-scoped owner registered
/// into ProcessManager so `stop_session` can tear them down in reverse order.
///
/// The legacy supervisor (`start_orchestration_session_supervisor`) is now
/// only reachable via `ATO_LEGACY_SUPERVISOR=1`.
pub(super) fn start_orchestration_session_in_process(
    handle: &str,
    resolution: &super::resolve::HandleResolution,
    manifest_path: &Path,
    plan: &capsule_core::router::ManifestData,
    raw_manifest: &str,
    mut notes: Vec<String>,
) -> Result<SessionInfo> {
    use crate::application::pipeline::executor::PhaseStageTimer;
    use crate::application::pipeline::hourglass::HourglassPhase;

    let orchestration = plan
        .resolve_services()
        .context("failed to resolve [services] orchestration plan")?;
    let leaf = pick_orchestration_leaf_service(&orchestration)?;
    let leaf_target_label = leaf.runtime.runtime().target.clone();
    let leaf_port = leaf.runtime.runtime().port.ok_or_else(|| {
        anyhow::anyhow!(
            "orchestration leaf service '{}' (target '{}') has no port; cannot bind WebView",
            leaf.name,
            leaf_target_label
        )
    })?;
    let leaf_runtime = leaf.runtime.runtime().runtime.clone();
    let leaf_driver = leaf
        .runtime
        .runtime()
        .driver
        .clone()
        .unwrap_or_else(|| leaf_runtime.clone());
    let leaf_name = leaf.name.clone();

    // Single read of CAPSULE_ALLOW_UNSAFE for this session entry (#73 PR-C).
    // No argv injection into a child supervisor; the gate is carried inside
    // the request types instead.
    let allow_unsafe = std::env::var("CAPSULE_ALLOW_UNSAFE").as_deref() == Ok("1");

    let session_root_path = session_root()?;
    fs::create_dir_all(&session_root_path).with_context(|| {
        format!(
            "failed to create session root {}",
            session_root_path.display()
        )
    })?;

    let reporter = Arc::new(make_orchestration_reporter());
    let (authoritative_lock, lock_path) = try_load_authoritative_lock(&plan.workspace_root);
    let mut prepared = PreparedRunContext {
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

    // Runtime that hosts both `block_on` calls AND the long-lived tokio tasks
    // spawned during `execute_until_ready_and_detach` (per-service `exit_task`
    // for local services, `log_task` for OCI services). Those tasks are
    // referenced by the `RunningService` values inside `detached.inner` below
    // and must outlive this function — `mem::forget(detached)` alone is not
    // enough, because dropping a `current_thread` runtime cancels in-flight
    // tasks. We `Box::leak` the runtime so the worker thread keeps the
    // tokio tasks alive for the rest of the process. PR-D replaces this leak
    // with a `BackgroundSessionOwner` that owns both the runtime and the
    // detached services and is dropped from `stop_session`.
    let runtime_handle: &'static tokio::runtime::Runtime = Box::leak(Box::new(
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("failed to create runtime for orchestration session start")?,
    ));

    let mut launch_ctx =
        runtime_handle.block_on(resolve_launch_context(plan, &prepared, &reporter))?;

    // Step 1: dependency contracts (= top-level [dependencies.<alias>]).
    // Distinct from the [services] graph below; same setup as single-target
    // session start uses.
    let timer = PhaseStageTimer::start(HourglassPhase::Execute, "session_dep_contracts_setup");
    let dep_contracts = runtime_handle
        .block_on(setup_dependency_contracts_launch_context(
            plan,
            &mut prepared,
            &reporter,
            &mut launch_ctx,
            "launching the session",
        ))
        .map_err(|err| err.context("failed to set up dependency contracts for session start"))?;
    timer.finish_ok();

    // Step 2: [services] orchestration in detach mode. The detach API runs
    // ServicePhaseCoordinator (the same one foreground `ato run` uses) and
    // returns control after readiness instead of entering monitor_until_exit.
    let options = crate::executors::orchestrator::OrchestratorOptions {
        enforcement: "strict".to_string(),
        sandbox_mode: true,
        // PR-C: CAPSULE_ALLOW_UNSAFE is read once above and forwarded as the
        // OrchestratorOptions field. PR-D moves this onto the unified
        // RunRequest; until then the option field is the carrier for the
        // session path.
        dangerously_skip_permissions: allow_unsafe,
        assume_yes: true,
        nacelle: None,
    };

    let timer = PhaseStageTimer::start(HourglassPhase::Execute, "orchestration_start_until_ready");
    let bollard_client = capsule_core::runtime::oci::BollardOciRuntimeClient::connect_default()
        .context("failed to connect to OCI engine for orchestration session start")?;
    let detached = runtime_handle
        .block_on(
            crate::executors::orchestrator::execute_until_ready_and_detach(
                plan,
                &prepared,
                reporter.clone(),
                &launch_ctx,
                &options,
                None,
                bollard_client,
            ),
        )
        .context("orchestration services failed to start in-process")?;
    timer.finish_ok();

    // Step 3: leaf service URL — ServicePhaseCoordinator already ran the
    // per-service readiness probes, so the leaf is reachable. We only need
    // the public URL for the session record.
    let local_url = format!("http://127.0.0.1:{}/", leaf_port);

    notes.push(format!(
        "Orchestration mode: launched in-process; WebView bound to leaf service '{}' (target='{}', port={}).",
        leaf_name, leaf_target_label, leaf_port
    ));

    let runtime_descriptor = CapsuleRuntimeDescriptor {
        target_label: leaf_target_label.clone(),
        runtime: Some(leaf_runtime),
        driver: Some(leaf_driver.clone()),
        language: None,
        port: Some(leaf_port),
    };

    // Surface the leaf process to ProcessManager so `stop_session` can find
    // a recorded PID. OCI leaves do not surface a Unix PID; in that case we
    // fall back to the wrapper's own PID for session_id derivation, which
    // matches the legacy supervisor's behavior of using the spawned `ato run`
    // PID. PR-D wires the full materialized graph (including OCI container
    // ids) through SessionRecord.dependency_contracts.
    let leaf_local_pid = detached
        .services
        .iter()
        .find(|s| s.name == leaf_name)
        .and_then(|s| s.local_pid)
        .map(|pid| pid as i32)
        .unwrap_or(0);

    let session_id_seed = if leaf_local_pid > 0 {
        leaf_local_pid as u32
    } else {
        std::process::id()
    };
    let session_id = format!("ato-desktop-session-{}", session_id_seed);
    let log_path = session_root_path.join(format!("{}.log", session_id));

    let process_manager = ProcessManager::new()?;
    let process_info = ProcessInfo {
        id: session_id.clone(),
        name: session_name(plan, "capsule-session"),
        pid: leaf_local_pid,
        workload_pid: None,
        status: ProcessStatus::Ready,
        runtime: runtime_descriptor
            .runtime
            .clone()
            .unwrap_or_else(|| "source".to_string()),
        start_time: SystemTime::now(),
        os_start_time_unix_ms: u32::try_from(leaf_local_pid)
            .ok()
            .and_then(ato_session_core::process::process_start_time_unix_ms),
        workload_os_start_time_unix_ms: None,
        manifest_path: Some(manifest_path.to_path_buf()),
        scoped_id: None,
        target_label: Some(leaf_target_label.clone()),
        requested_port: Some(leaf_port),
        log_path: Some(log_path.clone()),
        ready_at: Some(SystemTime::now()),
        last_event: Some("ready".to_string()),
        last_error: None,
        exit_code: None,
    };
    process_manager.write_pid(&process_info)?;

    // [dependencies.<alias>] subset — same as single-target session.
    let dependency_contracts =
        dependency_contracts_for_session_record(leaf_local_pid, dep_contracts.as_ref());
    // Slice A of #125 (umbrella #74): populate the persisted ExecutionGraph
    // subset alongside `dependency_contracts`. Write-only — teardown still
    // reads `dependency_contracts`. Parity is enforced in debug builds by
    // the populator's internal `debug_assert!`.
    let graph =
        crate::application::session_graph_populate::populate_graph_from_dependency_contracts(
            dependency_contracts.as_ref(),
        );
    let session = StoredSessionInfo {
        session_id: session_id.clone(),
        handle: handle.to_string(),
        normalized_handle: resolution.normalized_handle.clone(),
        canonical_handle: resolution.canonical_handle.clone(),
        trust_state: resolution.trust_state.clone(),
        source: resolution.source.clone(),
        restricted: resolution.restricted,
        snapshot: resolution.snapshot.clone(),
        runtime: runtime_descriptor,
        display_strategy: CapsuleDisplayStrategy::WebUrl,
        pid: leaf_local_pid,
        log_path: log_path.display().to_string(),
        manifest_path: manifest_path.display().to_string(),
        target_label: leaf_target_label,
        notes,
        guest: None,
        web: Some(WebSessionDisplay {
            local_url: local_url.clone(),
            healthcheck_url: local_url,
            served_by: leaf_driver,
        }),
        terminal: None,
        service: None,
        dependency_contracts,
        graph,
        // [services] graph subset (#73 PR-D). Persisted so `stop_session`
        // (and the parent-death watcher from PR-B) can tear services down
        // after the wrapper process exits — the OS keeps the underlying
        // OCI containers / spawned children alive as orphans, but only
        // this record holds the container_ids / pids needed to stop them.
        orchestration_services: orchestration_services_for_session_record(
            std::process::id() as i32,
            &detached.services,
        ),
        schema_version: None,
        launch_digest: None,
        process_start_time_unix_ms: None,
    };
    write_session_record(&session_root_path, &session)?;

    // Lifecycle handoff (#73 PR-C → PR-D).
    //
    // Three things must outlive this function for the session to keep
    // running:
    //
    //   1. `runtime_handle` — the tokio runtime that hosts the spawned
    //      `exit_task` / `log_task` for every running service. Already
    //      `Box::leak`'d above; the worker thread therefore lives for the
    //      rest of the process.
    //   2. `detached` — owns the `RunningService` values (each holding a
    //      `Child`, log threads, lifecycle event channels, and JoinHandles
    //      backed by the leaked runtime). `mem::forget` keeps the OS-level
    //      processes/threads alive.
    //   3. `dep_contracts` — owns the `RunningGraph` for top-level
    //      `[dependencies.<alias>]`. Same reasoning as `detached`.
    //
    // PR-D replaces all three leaks with a `BackgroundSessionOwner`
    // registered into ProcessManager so `stop_session` can drop them in
    // reverse-topological order. Until that lands, providers are stoppable
    // through the dependency-session sidecar fallback added in PR-B
    // (`stop_process` reads the snapshot when the PID file is missing).
    if let Some(g) = dep_contracts {
        std::mem::forget(g);
    }
    std::mem::forget(detached);

    Ok(session_info_from_stored(session))
}

pub(super) fn start_orchestration_session_supervisor(
    handle: &str,
    resolution: &super::resolve::HandleResolution,
    manifest_path: &Path,
    plan: &capsule_core::router::ManifestData,
    mut notes: Vec<String>,
) -> Result<SessionInfo> {
    use crate::application::pipeline::executor::PhaseStageTimer;
    use crate::application::pipeline::hourglass::HourglassPhase;

    let orchestration = plan
        .resolve_services()
        .context("failed to resolve [services] orchestration plan")?;

    let leaf = pick_orchestration_leaf_service(&orchestration)?;
    let leaf_target_label = leaf.runtime.runtime().target.clone();
    let leaf_port = leaf.runtime.runtime().port.ok_or_else(|| {
        anyhow::anyhow!(
            "orchestration leaf service '{}' (target '{}') has no port; cannot bind WebView",
            leaf.name,
            leaf_target_label
        )
    })?;
    let leaf_runtime = leaf.runtime.runtime().runtime.clone();
    let leaf_driver = leaf
        .runtime
        .runtime()
        .driver
        .clone()
        .unwrap_or_else(|| leaf_runtime.clone());
    let leaf_name = leaf.name.clone();

    let supervisor_input = handle.to_string();
    let ato_bin = std::env::current_exe()
        .context("failed to resolve current `ato` binary path for orchestration supervisor")?;

    let session_root = session_root()?;
    fs::create_dir_all(&session_root)
        .with_context(|| format!("failed to create session root {}", session_root.display()))?;

    let provisional_log_path = session_root.join("orchestration-supervisor.log");
    let stdout_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&provisional_log_path)
        .with_context(|| {
            format!(
                "failed to open orchestration log file {}",
                provisional_log_path.display()
            )
        })?;
    let stderr_file = stdout_file.try_clone().with_context(|| {
        format!(
            "failed to clone orchestration log handle {}",
            provisional_log_path.display()
        )
    })?;

    let timer = PhaseStageTimer::start(HourglassPhase::Execute, "spawn_orchestration_supervisor");
    let mut cmd = Command::new(&ato_bin);
    cmd.arg("run").arg("-y").arg("--sandbox");
    // PR-C removed the `--dangerously-skip-permissions` argv injection that
    // used to be appended here when CAPSULE_ALLOW_UNSAFE=1. The unsafe gate
    // is now carried on `ConsumerRunRequest.allow_unsafe` and is read once
    // at the session entry point (`start_orchestration_session_in_process`,
    // and in `cli/commands/run::build_consumer_run_request`). The legacy
    // supervisor path inherits CAPSULE_ALLOW_UNSAFE through the env so the
    // nested `ato run` re-reads it on its own entry.
    cmd.arg(&supervisor_input)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    let mut child = cmd.spawn().with_context(|| {
        format!(
            "failed to spawn `ato run` orchestration supervisor for {}",
            supervisor_input
        )
    })?;
    timer.finish_ok();

    let session_id = format!("ato-desktop-session-{}", child.id());
    let log_path = session_root.join(format!("{}.log", session_id));
    let log_path = match fs::rename(&provisional_log_path, &log_path) {
        Ok(()) => log_path,
        Err(_) => provisional_log_path,
    };

    let timer = PhaseStageTimer::start(HourglassPhase::Execute, "wait_orchestration_ready");
    let ready = wait_for_http_ready(&mut child, leaf_port, "/", session_ready_timeout(plan));
    timer.finish_ok();
    if let Err(err) = ready {
        let _ = child.kill();
        let _ = child.wait();
        anyhow::bail!(
            "orchestration leaf service '{}' (port {}) failed to become ready: {}. See logs at {}",
            leaf_name,
            leaf_port,
            err,
            log_path.display()
        );
    }

    let local_url = format!("http://127.0.0.1:{}/", leaf_port);
    notes.push(format!(
        "Orchestration mode: launched run supervisor; WebView bound to leaf service '{}' (target='{}', port={}).",
        leaf_name, leaf_target_label, leaf_port
    ));

    let runtime = CapsuleRuntimeDescriptor {
        target_label: leaf_target_label.clone(),
        runtime: Some(leaf_runtime),
        driver: Some(leaf_driver.clone()),
        language: None,
        port: Some(leaf_port),
    };

    let process_manager = ProcessManager::new()?;
    let process_info = ProcessInfo {
        id: session_id.clone(),
        name: session_name(plan, "capsule-session"),
        pid: child.id() as i32,
        workload_pid: None,
        status: ProcessStatus::Ready,
        runtime: runtime
            .runtime
            .clone()
            .unwrap_or_else(|| "source".to_string()),
        start_time: SystemTime::now(),
        os_start_time_unix_ms: ato_session_core::process::process_start_time_unix_ms(child.id()),
        workload_os_start_time_unix_ms: None,
        manifest_path: Some(manifest_path.to_path_buf()),
        scoped_id: None,
        target_label: Some(leaf_target_label.clone()),
        requested_port: Some(leaf_port),
        log_path: Some(log_path.clone()),
        ready_at: Some(SystemTime::now()),
        last_event: Some("ready".to_string()),
        last_error: None,
        exit_code: None,
    };
    process_manager.write_pid(&process_info)?;

    let session = StoredSessionInfo {
        session_id: session_id.clone(),
        handle: handle.to_string(),
        normalized_handle: resolution.normalized_handle.clone(),
        canonical_handle: resolution.canonical_handle.clone(),
        trust_state: resolution.trust_state.clone(),
        source: resolution.source.clone(),
        restricted: resolution.restricted,
        snapshot: resolution.snapshot.clone(),
        runtime,
        display_strategy: CapsuleDisplayStrategy::WebUrl,
        pid: child.id() as i32,
        log_path: log_path.display().to_string(),
        manifest_path: manifest_path.display().to_string(),
        target_label: leaf_target_label,
        notes,
        guest: None,
        web: Some(WebSessionDisplay {
            local_url: local_url.clone(),
            healthcheck_url: local_url,
            served_by: leaf_driver,
        }),
        terminal: None,
        service: None,
        dependency_contracts: None,
        graph: None,
        // Legacy supervisor path: the nested `ato run` child owns the
        // service lifecycle, so this wrapper has no DetachedServiceSnapshot
        // to persist. Reachable only via ATO_LEGACY_SUPERVISOR=1.
        orchestration_services: None,
        schema_version: None,
        launch_digest: None,
        process_start_time_unix_ms: None,
    };
    write_session_record(&session_root, &session)?;

    std::mem::forget(child);

    Ok(session_info_from_stored(session))
}

/// Pick the leaf service of the orchestration dependency graph — the one
/// no other service `depends_on`. For typical app manifests (e.g. backend
/// + frontend with `web depends_on main`), this is the user-facing UI.
///
/// If multiple leaves exist, prefer one whose target driver is `node` /
/// runtime is `web` (front-end candidates), then fall back to the
/// alphabetically last name.
fn pick_orchestration_leaf_service(
    orchestration: &capsule_core::foundation::types::OrchestrationPlan,
) -> Result<&capsule_core::foundation::types::ResolvedService> {
    use std::collections::HashSet;

    let mut depended: HashSet<&str> = HashSet::new();
    for service in &orchestration.services {
        for dep in &service.depends_on {
            depended.insert(dep.as_str());
        }
    }
    let leaves: Vec<&capsule_core::foundation::types::ResolvedService> = orchestration
        .services
        .iter()
        .filter(|service| !depended.contains(service.name.as_str()))
        .collect();

    match leaves.len() {
        0 => anyhow::bail!(
            "orchestration plan has no leaf service — every service is depended on by another (cycle?)"
        ),
        1 => Ok(leaves[0]),
        _ => {
            if let Some(web_leaf) = leaves.iter().find(|service| {
                let target = service.runtime.runtime();
                target
                    .driver
                    .as_deref()
                    .is_some_and(|driver| driver.eq_ignore_ascii_case("node"))
                    || target.runtime.eq_ignore_ascii_case("web")
            }) {
                return Ok(*web_leaf);
            }
            Ok(*leaves
                .iter()
                .max_by_key(|service| service.name.clone())
                .expect("non-empty leaves vec"))
        }
    }
}

/// Project a Vec of `DetachedServiceSnapshot` (from
/// `executors::orchestrator::execute_until_ready_and_detach`) into the
/// persisted `StoredOrchestrationServices` shape (#73 PR-D, closes #28
/// phase 2).
///
/// Returns `None` when there are no services — keeps the JSON lean for
/// non-orchestration sessions (which call this with an empty slice if
/// they end up here at all).
///
/// `wrapper_pid` should be the PID of the wrapper process that
/// materialized the orchestration graph (`std::process::id()` at the
/// call site). It is recorded so `stop_session` can defend against
/// PID reuse when validating the record.
fn orchestration_services_for_session_record(
    wrapper_pid: i32,
    snapshots: &[crate::executors::orchestrator::DetachedServiceSnapshot],
) -> Option<StoredOrchestrationServices> {
    if snapshots.is_empty() {
        return None;
    }
    let services = snapshots
        .iter()
        .map(|s| StoredOrchestrationService {
            name: s.name.clone(),
            target_label: s.target_label.clone(),
            local_pid: s.local_pid.map(|p| p as i32),
            container_id: s.container_id.clone(),
            host_ports: s.host_ports.iter().map(|(h, c)| (*h, *c)).collect(),
            published_port: s.published_port,
        })
        .collect();
    Some(StoredOrchestrationServices {
        wrapper_pid,
        services,
    })
}

fn dependency_contracts_for_session_record(
    consumer_pid: i32,
    dep_contracts: Option<&DependencyContractGuard>,
) -> Option<StoredDependencyContracts> {
    let graph = dep_contracts.and_then(DependencyContractGuard::graph)?;
    let providers = graph
        .deps()
        .iter()
        .map(|dep| StoredDependencyProvider {
            alias: dep.alias.clone(),
            pid: dep.child.id() as i32,
            state_dir: dep.state_dir.clone(),
            resolved: dep.resolved.clone(),
            allocated_port: dep.allocated_port,
            log_path: dep.log_path.clone(),
            runtime_export_keys: dep.runtime_exports.keys().cloned().collect(),
        })
        .collect::<Vec<_>>();
    if providers.is_empty() {
        return None;
    }

    Some(StoredDependencyContracts {
        consumer_pid,
        providers,
    })
}

pub(crate) fn session_info_from_stored(session: StoredSessionInfo) -> SessionInfo {
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
        execution_id: None,
        execution_receipt_schema_version: None,
    }
}

pub(super) fn resolve_session_launch_plan(
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
                None,
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

/// Output of `prepare_session_execution`. Carries both the prepared target
/// (passed straight to `spawn_runtime_process`) and the optional
/// `DependencyContractGuard` for top-level `[dependencies.<alias>]`
/// providers — the caller must keep the guard alive until either the
/// session is persisted (snapshot + detach so the providers outlive ato)
/// or the session start aborts (Drop tears the providers down).
pub(super) struct PreparedSessionExecution {
    pub(super) prepared: crate::executors::target_runner::PreparedTargetExecution,
    pub(super) dep_contracts: Option<DependencyContractGuard>,
}

fn prepare_session_execution(
    plan: &capsule_core::router::ManifestData,
    raw_manifest: &str,
) -> Result<PreparedSessionExecution> {
    let reporter = Arc::new(make_orchestration_reporter());
    let (authoritative_lock, lock_path) = try_load_authoritative_lock(&plan.workspace_root);
    let mut prepared = PreparedRunContext {
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

    let mut launch_ctx = runtime.block_on(resolve_launch_context(plan, &prepared, &reporter))?;

    // session-start used to skip dependency contracts entirely — the
    // run.rs pipeline is the only path that wires `[dependencies.*]`
    // providers into the consumer's launch context. That meant every
    // Desktop launch of a capsule with `DATABASE_URL =
    // "{{deps.db.runtime_exports.DATABASE_URL}}"` (the WasedaP2P
    // pattern) handed the consumer the literal template string
    // verbatim, which sqlalchemy / equivalent URL parsers immediately
    // reject. We now mirror the run.rs flow: auto-lock if needed,
    // start the providers, render the template, and add their loopback
    // endpoints to the sandbox egress allowlist.
    let dep_contracts = runtime
        .block_on(setup_dependency_contracts_launch_context(
            plan,
            &mut prepared,
            &reporter,
            &mut launch_ctx,
            "launching the session",
        ))
        .map_err(|err| err.context("failed to set up dependency contracts for session start"))?;

    let prepared_target = prepare_target_execution(
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
    )?;

    Ok(PreparedSessionExecution {
        prepared: prepared_target,
        dep_contracts,
    })
}

fn spawn_runtime_process(
    plan: &capsule_core::router::ManifestData,
    prepared: &crate::executors::target_runner::PreparedTargetExecution,
    display_strategy: &CapsuleDisplayStrategy,
    log_path: &Path,
) -> Result<CapsuleProcess> {
    let logged = || ExecuteMode::Logged(log_path.to_path_buf());
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
                logged(),
                &prepared.launch_ctx,
            ),
            _ => anyhow::bail!("unsupported runtime=web driver '{driver}' for session start"),
        };
    }

    // See display_strategy_for_runtime: Desktop session-start treats
    // [services] manifests as single-target launches, so we deliberately
    // skip the orchestration shell::execute branch here. dep contracts
    // are already started in prepare_session_execution; the consumer
    // process below sees the resolved env via prepared.launch_ctx.
    let _ = plan.is_orchestration_mode();

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
        // Native covers source/python and source/native. Always go
        // through `source::execute_host`, which routes through nacelle
        // and `uv run` so the venv built during the build phase is
        // honoured. Falling through to `shell::execute` here strands
        // run_command launches like `python -m uvicorn main:app` on
        // the toolchain Python with no site-packages, producing
        // `No module named uvicorn` for ato Desktop's session-start
        // flow.
        capsule_core::execution_plan::guard::ExecutorKind::Native => {
            crate::executors::source::execute_host(
                plan,
                None,
                Arc::new(CliReporter::new(false)),
                logged(),
                &prepared.launch_ctx,
            )
        }
        _ if plan.execution_run_command().is_some() => {
            crate::executors::shell::execute(plan, logged(), &prepared.launch_ctx)
        }
        _ => crate::executors::source::execute_host(
            plan,
            None,
            Arc::new(CliReporter::new(false)),
            logged(),
            &prepared.launch_ctx,
        ),
    }
}

fn display_strategy_for_runtime(
    plan: &capsule_core::router::ManifestData,
) -> CapsuleDisplayStrategy {
    // ato Desktop's "open this capsule" UX is single-target by design:
    // it launches `default_target` (or `--target`) and points a WebView
    // at it. The full multi-service orchestration that `[services]`
    // unlocks is only invoked from the `ato run` CLI pipeline, which
    // owns the orchestrator executor's lifecycle (provider startup,
    // service-to-service deps, parallel ready probes). Routing through
    // ServiceBackground here would call `shell::execute` which doesn't
    // know how to wait on the consumer's HTTP port and would orphan
    // the providers we just started above. Treat orchestration_mode
    // manifests like single-service ones — the selected target's
    // runtime/driver/port still drives the display strategy below.
    let _ = plan.is_orchestration_mode();

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

// `attach_process_logs` (proxy-thread pattern) was removed in favour of
// `ExecuteMode::Logged`, which connects the child's stdout/stderr directly
// to the log file at `Command::spawn` time via `Stdio::from(File)`. The old
// pattern silently dropped output once `ato app session start` exited
// because the proxy threads doing `io::copy` died with the parent process,
// and the kernel then sent EPIPE to the child's stdout. The replacement
// keeps a kernel-owned file descriptor wired to the log file across the
// parent's exit, so detached children continue logging normally.

fn read_session_record(path: &Path) -> Option<StoredSessionInfo> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Tear down the `[services]` graph subset persisted on the session record
/// (#73 PR-D, closes #28 phase 2). Counterpart of
/// `stop_recorded_dependency_contracts` for orchestration capsules.
///
/// Iterates `services` in reverse insertion order (i.e. reverse-topological
/// because `start_orchestration_session_in_process` records them in start
/// order) and stops each one:
///   - OCI services (`container_id` set): `stop_container` with a short
///     timeout, then `remove_container`. Idempotent — already-gone
///     containers are silently absorbed by Bollard.
///   - Local services (`local_pid` set): SIGTERM (or SIGKILL when `force`),
///     swallowing ESRCH the same way `terminate_process` in `process.rs`
///     does.
///
/// Returns `Ok(true)` if any service was actively stopped this call.
/// Errors during teardown are logged via `eprintln!` and do not abort the
/// loop — `stop_session` must keep going so subsequent services don't
/// leak just because one OCI daemon roundtrip failed.
fn stop_recorded_orchestration_services(
    record: Option<&StoredSessionInfo>,
    force: bool,
) -> Result<bool> {
    let Some(record) = record else {
        return Ok(false);
    };
    let Some(snapshot) = record.orchestration_services.as_ref() else {
        return Ok(false);
    };
    if snapshot.services.is_empty() {
        return Ok(false);
    }

    // Lazy OCI client: only build if we actually have an OCI service.
    // Avoids spinning up a tokio runtime + bollard handshake for the
    // common case of a fully-managed (local-only) orchestration capsule.
    let has_oci = snapshot.services.iter().any(|s| s.container_id.is_some());
    let oci_runtime = if has_oci {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => Some(rt),
            Err(err) => {
                eprintln!(
                    "ATO-WARN failed to build tokio runtime for orchestration teardown: {err}"
                );
                None
            }
        }
    } else {
        None
    };
    let oci_client = oci_runtime.as_ref().and_then(|_| {
        match capsule_core::runtime::oci::BollardOciRuntimeClient::connect_default() {
            Ok(c) => Some(c),
            Err(err) => {
                eprintln!(
                    "ATO-WARN failed to connect to OCI engine for orchestration teardown: {err}"
                );
                None
            }
        }
    });

    let mut any_stopped = false;
    // Reverse-topological: services were started by ServicePhaseCoordinator
    // in topological order, so reverse iteration is the correct teardown
    // order (consumers before providers).
    for service in snapshot.services.iter().rev() {
        if let Some(container_id) = service.container_id.as_deref() {
            let (Some(rt), Some(client)) = (oci_runtime.as_ref(), oci_client.as_ref()) else {
                eprintln!(
                    "ATO-WARN orchestration service '{}' has container_id but no OCI client; skipping",
                    service.name
                );
                continue;
            };
            // Short timeout: the daemon will SIGKILL the container if it
            // doesn't exit gracefully within the budget. 5s matches the
            // `OCI_STOP_TIMEOUT_SECS` constant in `executors::orchestrator`.
            use capsule_core::runtime::oci::OciRuntimeClient as _;
            match rt.block_on(client.stop_container(container_id, 5)) {
                Ok(()) => any_stopped = true,
                Err(err) => {
                    eprintln!(
                        "ATO-WARN failed to stop OCI container {} for service '{}': {}",
                        container_id, service.name, err
                    );
                }
            }
            if let Err(err) = rt.block_on(client.remove_container(container_id, force)) {
                eprintln!(
                    "ATO-WARN failed to remove OCI container {} for service '{}': {}",
                    container_id, service.name, err
                );
            }
        } else if let Some(pid) = service.local_pid {
            #[cfg(unix)]
            {
                if pid > 0 {
                    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };

                    // Strategy in order of preference:
                    //
                    //   1. **Process-group kill** when the recorded
                    //      `local_pid` is currently a pgroup leader
                    //      (`getpgid(pid) == pid`). The
                    //      `nacelle::manager::supervisor` spawn path
                    //      sets this via `cmd.process_group(0)`, so a
                    //      `kill(-pgid, sig)` reaps the wrapper AND
                    //      every descendant atomically.
                    //
                    //   2. **Descendant walk + per-pid kill** when (1)
                    //      doesn't apply — the typical orchestration
                    //      session: ato-cli spawns nacelle (pid recorded
                    //      as `local_pid`), nacelle internally launches
                    //      `uv run` / `npm run dev` wrappers via the
                    //      direct/sandbox-exec launchers (which inherit
                    //      ato-cli's pgroup, not their own). A plain
                    //      per-pid SIGKILL on the recorded pid kills
                    //      nacelle but leaves the wrappers it spawned
                    //      alive as init-reparented orphans (#92 AODD
                    //      Phase 2 → #111). Capture the descendants via
                    //      `pgrep -P` recursively *before* signaling so
                    //      we don't lose them when reparenting happens,
                    //      then signal recorded pid, then signal each
                    //      descendant. Idempotent on stale/dead pids
                    //      (ESRCH is silently swallowed).
                    //
                    //   3. The lsof-by-published-port fallback (#109)
                    //      stays as a belt-and-suspenders for any
                    //      listener we still missed (e.g. a service
                    //      that spawned outside the recorded subtree).
                    let mut signaled_via_pgroup = false;
                    let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
                    if pgid > 0 && pgid == pid as libc::pid_t {
                        let ret = unsafe { libc::kill(-pgid, signal) };
                        if ret == 0 {
                            any_stopped = true;
                            signaled_via_pgroup = true;
                        } else {
                            let err = std::io::Error::last_os_error();
                            if err.raw_os_error() != Some(libc::ESRCH) {
                                eprintln!(
                                    "ATO-WARN failed to signal process group {} for service '{}': {}",
                                    pgid, service.name, err
                                );
                            }
                        }
                    }

                    if !signaled_via_pgroup {
                        // Capture descendants BEFORE signaling — once
                        // the recorded pid is killed, its children are
                        // reparented to init and `pgrep -P recorded`
                        // returns nothing, leaking the wrappers.
                        let descendants = collect_descendant_pids(pid as u32, &service.name);

                        // Per-pid kill on the recorded pid first.
                        let ret = unsafe { libc::kill(pid as libc::pid_t, signal) };
                        if ret == 0 {
                            any_stopped = true;
                        } else {
                            let err = std::io::Error::last_os_error();
                            if err.raw_os_error() != Some(libc::ESRCH) {
                                eprintln!(
                                    "ATO-WARN failed to signal local service '{}' (pid {}): {}",
                                    service.name, pid, err
                                );
                            }
                        }

                        // Then signal every descendant we captured.
                        // Each signal is idempotent — ESRCH means the
                        // process already died (e.g. parent's death
                        // cascaded), which is the desired end state.
                        for child_pid in descendants {
                            let ret = unsafe { libc::kill(child_pid as libc::pid_t, signal) };
                            if ret == 0 {
                                any_stopped = true;
                            } else {
                                let err = std::io::Error::last_os_error();
                                if err.raw_os_error() != Some(libc::ESRCH) {
                                    eprintln!(
                                        "ATO-WARN failed to signal descendant {} (under recorded pid {}, service '{}'): {}",
                                        child_pid, pid, service.name, err
                                    );
                                }
                            }
                        }
                    }
                }
                // Belt-and-suspenders for the wrapper-vs-workload PID gap
                // (#108): even with the pgroup kill above, older
                // session records (no pgroup, or pgid != recorded pid)
                // and any spawn mode that drops out of the recorded
                // pgroup land here. Look up the current listener via
                // `lsof` and SIGKILL anything that's still bound to
                // `published_port`; idempotent (returns false when the
                // port is already free or the resolved pid matches
                // what we just signaled, including via the pgroup).
                if let Some(port) = service.published_port {
                    if kill_listeners_on_published_port(port, pid, force, &service.name) {
                        any_stopped = true;
                    }
                }
            }
            #[cfg(not(unix))]
            {
                let _ = (pid, force);
                eprintln!(
                    "ATO-WARN local orchestration service teardown is unix-only; service '{}' (pid {}) was left running",
                    service.name, pid
                );
            }
        }
    }
    Ok(any_stopped)
}

/// Walk the descendant tree of `root_pid` via `pgrep -P` (BFS) and
/// return every transitive child's pid. Used by
/// `stop_recorded_orchestration_services` to capture the wrapper
/// subtree BEFORE killing the recorded pid (#111). Once the recorded
/// pid dies, its children get reparented to init and `pgrep -P` no
/// longer finds them — by capturing first, we keep an explicit list
/// of pids to follow up on.
///
/// Best-effort: failures (missing `pgrep`, malformed output, fork
/// races) yield an empty / partial list and a debug-level message.
/// The caller still has the lsof-by-published-port fallback (#109)
/// for any listener we miss here.
///
/// Bounded depth (32 levels) and bounded total pids (256) so a
/// pathological process tree can't make teardown loop forever or
/// allocate without limit.
#[cfg(unix)]
fn collect_descendant_pids(root_pid: u32, service_name: &str) -> Vec<u32> {
    use std::collections::VecDeque;

    const MAX_DEPTH: usize = 32;
    const MAX_PIDS: usize = 256;

    let mut collected: Vec<u32> = Vec::new();
    let mut frontier: VecDeque<(u32, usize)> = VecDeque::new();
    frontier.push_back((root_pid, 0));

    while let Some((parent, depth)) = frontier.pop_front() {
        if depth >= MAX_DEPTH || collected.len() >= MAX_PIDS {
            break;
        }
        let output = match Command::new("pgrep")
            .args(["-P", &parent.to_string()])
            .output()
        {
            Ok(o) => o,
            Err(err) => {
                tracing::debug!(
                    parent,
                    service = service_name,
                    error = %err,
                    "collect_descendant_pids: pgrep -P failed"
                );
                continue;
            }
        };
        // pgrep exits 1 when the parent has no children — not an error.
        if !output.status.success() && output.status.code() != Some(1) {
            tracing::debug!(
                parent,
                service = service_name,
                exit = ?output.status.code(),
                stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                "collect_descendant_pids: pgrep returned non-success"
            );
            continue;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        for token in stdout.split_whitespace() {
            let Ok(child) = token.parse::<u32>() else {
                continue;
            };
            if child == 0 || child == parent || collected.contains(&child) {
                continue;
            }
            collected.push(child);
            frontier.push_back((child, depth + 1));
            if collected.len() >= MAX_PIDS {
                break;
            }
        }
    }

    collected
}

/// Kill any process currently bound to `port` on `127.0.0.1` whose pid
/// differs from `recorded_pid` (which the caller already attempted to
/// signal). Used as the wrapper-vs-workload fallback in
/// `stop_recorded_orchestration_services` (#108): when ato spawned the
/// service via `npm run dev` / `uv run` / a shell wrapper, the recorded
/// `local_pid` is the wrapper and the actual listener is its child.
/// `lsof -nP -iTCP:<port> -sTCP:LISTEN` is the host-portable way to
/// resolve the current listener; macOS and Linux both ship it.
///
/// Returns `true` iff at least one previously-unsignaled pid was
/// successfully killed.
#[cfg(unix)]
fn kill_listeners_on_published_port(
    port: u16,
    recorded_pid: i32,
    force: bool,
    service_name: &str,
) -> bool {
    let listener_pids = match listener_pids_on_port(port) {
        Ok(pids) => pids,
        Err(err) => {
            eprintln!(
                "ATO-WARN failed to enumerate listeners on port {} for service '{}': {}",
                port, service_name, err
            );
            return false;
        }
    };
    let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
    let mut killed = false;
    for pid in listener_pids {
        if pid as i32 == recorded_pid {
            // Already handled by the recorded-pid kill above.
            continue;
        }
        let ret = unsafe { libc::kill(pid as libc::pid_t, signal) };
        if ret == 0 {
            killed = true;
        } else {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ESRCH) {
                eprintln!(
                    "ATO-WARN failed to signal port-{} listener (pid {}) for service '{}': {}",
                    port, pid, service_name, err
                );
            }
        }
    }
    killed
}

/// Best-effort resolve "which pids are listening on TCP `port` on the
/// loopback right now?" using `lsof`. Returns the parsed pid list (may
/// be empty if nothing is bound). Limited to TCP / IPv4 LISTEN to match
/// how managed services bind their sockets — the orchestrator's
/// readiness probe only ever waits on TCP listeners on 127.0.0.1.
#[cfg(unix)]
fn listener_pids_on_port(port: u16) -> Result<Vec<u32>> {
    // `-t` prints PIDs only (one per line), bypassing the column-parsing
    // hazard of the default human format.
    let output = Command::new("lsof")
        .args(["-nP", "-t", &format!("-iTCP:{}", port), "-sTCP:LISTEN"])
        .output()
        .with_context(|| format!("failed to invoke lsof for port {}", port))?;
    // `lsof` exits 1 when there are no matches — that is not an error
    // for our purposes, so only fail on unexpected exit codes.
    if !output.status.success() && output.status.code() != Some(1) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "lsof exited {:?} for port {}: {}",
            output.status.code(),
            port,
            stderr.trim()
        );
    }
    let mut pids = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(pid) = trimmed.parse::<u32>() {
            pids.push(pid);
        }
    }
    Ok(pids)
}

fn stop_recorded_dependency_contracts(
    record: Option<&StoredSessionInfo>,
    force: bool,
) -> Result<bool> {
    let Some(record) = record else {
        return Ok(false);
    };
    let Some(plan) = dependency_teardown_plan(record)? else {
        return Ok(false);
    };
    let grace = if force {
        Duration::from_secs(0)
    } else {
        Duration::from_secs(10)
    };
    match plan.strategy {
        DependencyTeardownStrategy::Graph => {
            crate::application::dependency_runtime::teardown::teardown_in_order(
                &plan.targets,
                grace,
            )
        }
        DependencyTeardownStrategy::LegacyDependencyContracts => {
            crate::application::dependency_runtime::teardown_reverse_topological(
                plan.targets,
                grace,
            )
        }
    }
    .with_context(|| {
        format!(
            "Failed to stop dependency contracts for {}",
            record.session_id
        )
    })?;
    for state_dir in &plan.state_dirs {
        let _ = crate::application::dependency_runtime::orphan::sweep_stale_sentinel(state_dir);
    }
    Ok(true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DependencyTeardownStrategy {
    Graph,
    LegacyDependencyContracts,
}

#[derive(Debug, Clone)]
struct DependencyTeardownPlan {
    strategy: DependencyTeardownStrategy,
    targets: Vec<crate::application::dependency_runtime::TeardownTarget>,
    state_dirs: Vec<PathBuf>,
}

fn dependency_teardown_plan(record: &StoredSessionInfo) -> Result<Option<DependencyTeardownPlan>> {
    if let Some(plan) = dependency_teardown_plan_from_graph(record) {
        return Ok(Some(plan));
    }

    let Some(snapshot) = record.dependency_contracts.as_ref() else {
        return Ok(None);
    };
    if snapshot.providers.is_empty() {
        return Ok(None);
    }

    Ok(Some(DependencyTeardownPlan {
        strategy: DependencyTeardownStrategy::LegacyDependencyContracts,
        targets: snapshot
            .providers
            .iter()
            .map(
                |provider| crate::application::dependency_runtime::TeardownTarget {
                    dep: provider.alias.clone(),
                    pid: provider.pid,
                    state_dir: provider.state_dir.clone(),
                    needs: Vec::new(),
                },
            )
            .collect(),
        state_dirs: snapshot
            .providers
            .iter()
            .map(|provider| provider.state_dir.clone())
            .collect(),
    }))
}

fn dependency_teardown_plan_from_graph(
    record: &StoredSessionInfo,
) -> Option<DependencyTeardownPlan> {
    let graph = record.graph.as_ref()?;
    let snapshot = record.dependency_contracts.as_ref()?;
    let ordered_aliases = graph_provider_aliases_in_reverse_topological_order(graph);
    if ordered_aliases.is_empty() {
        return None;
    }

    let graph_aliases_sorted =
        sorted_provider_aliases(ordered_aliases.iter().map(|alias| alias.as_str()));
    let contract_aliases_sorted = sorted_provider_aliases(
        snapshot
            .providers
            .iter()
            .map(|provider| provider.alias.as_str()),
    );
    debug_assert_eq!(
        graph_aliases_sorted,
        contract_aliases_sorted,
        "session record graph/provider alias divergence (graph={graph_aliases_sorted:?}, contracts={contract_aliases_sorted:?})"
    );
    if graph_aliases_sorted != contract_aliases_sorted {
        return None;
    }

    let providers_by_alias = snapshot
        .providers
        .iter()
        .map(|provider| (provider.alias.as_str(), provider))
        .collect::<BTreeMap<_, _>>();

    let mut targets = Vec::with_capacity(ordered_aliases.len());
    let mut state_dirs = Vec::with_capacity(ordered_aliases.len());
    for alias in ordered_aliases {
        let provider = providers_by_alias.get(alias.as_str())?;
        targets.push(crate::application::dependency_runtime::TeardownTarget {
            dep: provider.alias.clone(),
            pid: provider.pid,
            state_dir: provider.state_dir.clone(),
            needs: Vec::new(),
        });
        state_dirs.push(provider.state_dir.clone());
    }

    Some(DependencyTeardownPlan {
        strategy: DependencyTeardownStrategy::Graph,
        targets,
        state_dirs,
    })
}

fn graph_provider_aliases_in_reverse_topological_order(
    graph: &ato_session_core::StoredExecutionGraph,
) -> Vec<String> {
    let provider_aliases = graph
        .nodes
        .iter()
        .filter(|node| node.kind == NODE_KIND_PROVIDER)
        .map(|node| node.identifier.clone())
        .collect::<BTreeSet<_>>();
    if provider_aliases.is_empty() {
        return Vec::new();
    }

    let mut adjacency = provider_aliases
        .iter()
        .map(|alias| (alias.clone(), Vec::<String>::new()))
        .collect::<BTreeMap<_, _>>();
    let mut subset_has_provides = false;
    for edge in &graph.edges {
        if edge.kind != EDGE_KIND_PROVIDES {
            continue;
        }
        if !provider_aliases.contains(&edge.source) {
            continue;
        }
        subset_has_provides = true;
        adjacency
            .entry(edge.source.clone())
            .or_default()
            .push(edge.target.clone());
        adjacency.entry(edge.target.clone()).or_default();
    }
    if !subset_has_provides {
        return Vec::new();
    }

    let mut visited = BTreeSet::<String>::new();
    let mut visiting = BTreeSet::<String>::new();
    let mut order = Vec::<String>::new();
    let mut stack = Vec::<String>::new();
    for node in adjacency.keys().cloned().collect::<Vec<_>>() {
        topo_visit(
            &node,
            &adjacency,
            &mut visited,
            &mut visiting,
            &mut order,
            &mut stack,
        );
    }

    order.reverse();
    order
        .into_iter()
        .filter(|node| provider_aliases.contains(node))
        .collect()
}

fn topo_visit(
    node: &str,
    adjacency: &BTreeMap<String, Vec<String>>,
    visited: &mut BTreeSet<String>,
    visiting: &mut BTreeSet<String>,
    order: &mut Vec<String>,
    stack: &mut Vec<String>,
) {
    if visited.contains(node) || visiting.contains(node) {
        return;
    }
    visiting.insert(node.to_string());
    stack.push(node.to_string());
    if let Some(neighbors) = adjacency.get(node) {
        for next in neighbors {
            topo_visit(next, adjacency, visited, visiting, order, stack);
        }
    }
    stack.pop();
    visiting.remove(node);
    visited.insert(node.to_string());
    order.push(node.to_string());
}

fn sorted_provider_aliases<'a>(aliases: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let mut aliases = aliases.collect::<Vec<_>>();
    aliases.sort_unstable();
    aliases
}

fn dependency_sidecar_has_providers(
    snapshot: Option<&crate::runtime::process::DependencyContractSessionSnapshot>,
) -> bool {
    snapshot.is_some_and(|snapshot| !snapshot.providers.is_empty())
}

pub fn stop_session(session_id: &str, json: bool) -> Result<()> {
    let process_manager = ProcessManager::new()?;
    let session_path = session_root()?.join(format!("{session_id}.json"));
    let session_record = read_session_record(&session_path);
    let dependency_sidecar = process_manager
        .read_dependency_session_snapshot(session_id)
        .ok()
        .flatten();
    let sidecar_has_providers = dependency_sidecar_has_providers(dependency_sidecar.as_ref());

    let mut stop_error = None;
    let mut stopped = match process_manager.stop_process(session_id, true) {
        Ok(stopped) => stopped,
        Err(err) => {
            stop_error = Some(err);
            false
        }
    };
    if !sidecar_has_providers || !stopped {
        match stop_recorded_dependency_contracts(session_record.as_ref(), true) {
            Ok(record_stopped) => {
                if record_stopped {
                    let _ = process_manager.delete_pid(session_id);
                }
                stopped |= record_stopped;
            }
            Err(err) => {
                if stop_error.is_none() {
                    stop_error = Some(err);
                }
            }
        }
    }
    // Orchestration `[services]` graph teardown (#73 PR-D, closes #28
    // phase 2). Independent of the dep-contract sidecar — orchestration
    // sessions persist their services subset on the record and there is
    // no sidecar form. `force=true` matches the dep-contract path's
    // behavior on `stop_session`.
    match stop_recorded_orchestration_services(session_record.as_ref(), true) {
        Ok(record_stopped) => {
            stopped |= record_stopped;
        }
        Err(err) => {
            if stop_error.is_none() {
                stop_error = Some(err);
            }
        }
    }
    if let Some(err) = stop_error {
        if !stopped {
            return Err(err);
        }
    }

    if session_path.exists() {
        fs::remove_file(&session_path)
            .with_context(|| format!("failed to remove session file {}", session_path.display()))?;
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&SessionStopEnvelope {
                schema_version: super::SCHEMA_VERSION,
                package_id: super::ATO_DESKTOP_PACKAGE_ID,
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

pub fn watch_parent_and_stop_session(
    session_id: &str,
    parent_pid: u32,
    parent_start_time_unix_ms: Option<u64>,
    poll_interval: Duration,
) -> Result<()> {
    while desktop_parent_process_matches(parent_pid, parent_start_time_unix_ms) {
        std::thread::sleep(poll_interval);
    }

    stop_session(session_id, false).with_context(|| {
        format!("failed to stop session {session_id} after ato-desktop parent exited")
    })
}

fn maybe_spawn_parent_death_watcher(session_id: &str) -> Result<()> {
    let Some(parent_pid) = std::env::var(DESKTOP_PARENT_PID_ENV)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
    else {
        return Ok(());
    };
    if parent_pid == 0 {
        return Ok(());
    }

    let parent_start_time = std::env::var(DESKTOP_PARENT_START_TIME_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok());
    let ato_bin = std::env::current_exe().context("failed to resolve current ato executable")?;
    let mut command = Command::new(ato_bin);
    command
        .args(["app", "session", "watch-parent", session_id, "--parent-pid"])
        .arg(parent_pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(start_time) = parent_start_time {
        command
            .arg("--parent-start-time-unix-ms")
            .arg(start_time.to_string());
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let _child = command.spawn().context("failed to spawn watcher process")?;
    Ok(())
}

fn desktop_parent_process_matches(parent_pid: u32, expected_start_time: Option<u64>) -> bool {
    if parent_pid == 0 || !ato_session_core::process::pid_is_alive(parent_pid) {
        return false;
    }

    match expected_start_time {
        Some(expected) => ato_session_core::process::process_start_time_unix_ms(parent_pid)
            .map(|actual| actual == expected)
            .unwrap_or(false),
        None => true,
    }
}

/// Thin wrapper around `ato_session_core::session_root` so existing
/// CLI call sites keep using the unprefixed name. The shared helper
/// honors the same `ATO_DESKTOP_SESSION_ROOT` env override, which is
/// what the Desktop fast-path tests rely on.
pub(crate) fn session_root() -> Result<PathBuf> {
    ato_session_core::session_root()
}

/// Writes the record atomically (temp + rename) via `ato_session_core`
/// so the Desktop direct-read fast path can never observe a partial
/// record. Replaces the legacy `fs::write` call (RFC v0.3 §9.4
/// prerequisite for Phase 1).
fn write_session_record(root: &Path, session: &StoredSessionInfo) -> Result<()> {
    write_session_record_atomic(root, session)
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

        std::thread::sleep(SESSION_READY_POLL_INTERVAL);
    }
}

pub(crate) fn http_get_ok(port: u16, path: &str) -> bool {
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
    // The probe is "is the consumer's HTTP server alive on this port",
    // not "does the consumer happen to define `path` as a 200 route".
    // FastAPI / framework apps that don't register a `/` handler return
    // 404 — that's still a healthy HTTP server, so accept any
    // well-formed status line in the 1xx-4xx range and treat 5xx as
    // not-yet-ready (the server is listening but the framework's
    // startup hook may still be raising). 3xx (auth redirects), 401/403
    // (auth gates), 404 (no root route) all count as ready.
    let status_line = response.lines().next().unwrap_or_default();
    if !(status_line.starts_with("HTTP/1.0 ") || status_line.starts_with("HTTP/1.1 ")) {
        return false;
    }
    let status_token = status_line.split_whitespace().nth(1).unwrap_or_default();
    let Ok(status_code) = status_token.parse::<u16>() else {
        return false;
    };
    (100..500).contains(&status_code)
}

pub(super) fn print_session_info(info: &SessionInfo) {
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

/// Save the current stdout file descriptor and redirect fd 1 to
/// fd 2 (stderr). Returns the saved descriptor so the caller can
/// later restore it via `restore_stdout`. Used by `start_session`
/// in JSON mode so any output the v0.3 lifecycle hooks produce —
/// both `reporter.notify` lines and the subprocess `Stdio::inherit`
/// output — lands on stderr instead of corrupting the session
/// envelope on stdout.
#[cfg(unix)]
pub(super) fn redirect_stdout_to_stderr() -> Result<i32> {
    // SAFETY: dup/dup2 on standard FDs; failure paths return an
    // error and we never hold the saved FD past `restore_stdout`.
    unsafe {
        std::io::Write::flush(&mut std::io::stdout()).ok();
        let saved = libc::dup(1);
        if saved < 0 {
            anyhow::bail!(
                "dup(STDOUT_FILENO) failed: {}",
                std::io::Error::last_os_error()
            );
        }
        if libc::dup2(2, 1) < 0 {
            let err = std::io::Error::last_os_error();
            libc::close(saved);
            anyhow::bail!("dup2(STDERR_FILENO, STDOUT_FILENO) failed: {err}");
        }
        Ok(saved)
    }
}

#[cfg(unix)]
pub(super) fn restore_stdout(saved: i32) -> Result<()> {
    // SAFETY: `saved` was returned from a successful `dup` in
    // `redirect_stdout_to_stderr`; it is a valid FD that we own.
    unsafe {
        std::io::Write::flush(&mut std::io::stdout()).ok();
        let rc = libc::dup2(saved, 1);
        libc::close(saved);
        if rc < 0 {
            anyhow::bail!(
                "dup2(saved, STDOUT_FILENO) failed: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn redirect_stdout_to_stderr() -> Result<i32> {
    // Windows path: skip the FD redirect for now. The desktop is
    // currently macOS/Linux only, so this only matters for `ato app
    // session start --json` invoked manually on Windows. The
    // session envelope will be correct as long as the lifecycle
    // doesn't emit non-JSON to stdout, which is fine for our test
    // matrix; revisit with `SetStdHandle` if Windows desktop ships.
    Ok(-1)
}

#[cfg(not(unix))]
pub(super) fn restore_stdout(_saved: i32) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::handle::normalize_capsule_handle;
    use serial_test::serial;

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
                session_id: "ato-desktop-session-1".to_string(),
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
                log_path: "/tmp/ato-desktop-session.log".to_string(),
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
                execution_id: None,
                execution_receipt_schema_version: None,
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

    /// Slice A of #125 (umbrella #74), session-start integration shape:
    /// when a session record carries non-empty `dependency_contracts`,
    /// the populator must also emit a non-None `graph` whose provider
    /// node set matches the providers, and the resulting `StoredSessionInfo`
    /// must round-trip through serde unchanged.
    ///
    /// This is the integration counterpart to the unit tests in
    /// `application::session_graph_populate::tests`; it pins the call
    /// shape used by `start_runtime_session` /
    /// `start_orchestration_session_in_process` (build
    /// dependency_contracts → call populator with the same value → store
    /// both on the record).
    #[test]
    fn session_record_with_dep_contracts_carries_populated_graph_and_round_trips() {
        use crate::application::session_graph_populate::populate_graph_from_dependency_contracts;

        let dependency_contracts = Some(StoredDependencyContracts {
            consumer_pid: 4242,
            providers: vec![
                StoredDependencyProvider {
                    alias: "db".to_string(),
                    pid: 5252,
                    state_dir: PathBuf::from("/tmp/db"),
                    resolved: "capsule://example/db@1".to_string(),
                    allocated_port: Some(5432),
                    log_path: None,
                    runtime_export_keys: vec!["DATABASE_URL".to_string()],
                },
                StoredDependencyProvider {
                    alias: "cache".to_string(),
                    pid: 5353,
                    state_dir: PathBuf::from("/tmp/cache"),
                    resolved: "capsule://example/cache@1".to_string(),
                    allocated_port: Some(6379),
                    log_path: None,
                    runtime_export_keys: vec!["CACHE_URL".to_string()],
                },
            ],
        });
        let graph = populate_graph_from_dependency_contracts(dependency_contracts.as_ref());
        assert!(
            graph.is_some(),
            "non-empty dependency_contracts must yield a populated graph (slice A)"
        );

        let record = StoredSessionInfo {
            session_id: "ato-desktop-session-graph-populate".to_string(),
            handle: "capsule://example/demo".to_string(),
            normalized_handle: "capsule://example/demo".to_string(),
            canonical_handle: Some("capsule://example/demo".to_string()),
            trust_state: TrustState::Untrusted,
            source: Some("registry".to_string()),
            restricted: false,
            snapshot: None,
            runtime: CapsuleRuntimeDescriptor {
                target_label: "web".to_string(),
                runtime: Some("source".to_string()),
                driver: None,
                language: None,
                port: None,
            },
            display_strategy: CapsuleDisplayStrategy::WebUrl,
            pid: 4242,
            log_path: "/tmp/x.log".to_string(),
            manifest_path: "/tmp/capsule.toml".to_string(),
            target_label: "web".to_string(),
            notes: vec![],
            guest: None,
            web: None,
            terminal: None,
            service: None,
            dependency_contracts,
            graph,
            orchestration_services: None,
            schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
            launch_digest: Some("d".repeat(64)),
            process_start_time_unix_ms: None,
        };

        // Provider-set parity: graph providers ≡ dependency_contracts providers.
        let contract_providers: std::collections::BTreeSet<&str> = record
            .dependency_contracts
            .as_ref()
            .map(|c| c.providers.iter().map(|p| p.alias.as_str()).collect())
            .unwrap_or_default();
        let graph_providers: std::collections::BTreeSet<&str> = record
            .graph
            .as_ref()
            .map(|g| {
                g.nodes
                    .iter()
                    .filter(|n| n.kind == "provider")
                    .map(|n| n.identifier.as_str())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(graph_providers, contract_providers);

        // Round-trip through serde unchanged: the populated graph survives
        // ato-session-core's atomic-writer JSON serialization.
        let first = serde_json::to_string(&record).expect("serialize");
        let parsed: StoredSessionInfo = serde_json::from_str(&first).expect("parse");
        let second = serde_json::to_string(&parsed).expect("reserialize");
        assert_eq!(first, second, "populated graph must round-trip byte-stable");
        assert!(parsed.graph.is_some(), "graph must survive the round-trip");
        let parsed_graph = parsed.graph.expect("graph present after round-trip");
        assert_eq!(
            parsed_graph.schema_version,
            ato_session_core::StoredExecutionGraph::SCHEMA_VERSION
        );
        assert_eq!(parsed_graph.nodes.len(), 2);
        assert_eq!(parsed_graph.edges.len(), 2);
    }

    #[test]
    fn dependency_teardown_plan_prefers_graph_when_provider_subset_is_present() {
        let temp = tempfile::tempdir().expect("tempdir");
        let record = StoredSessionInfo {
            session_id: "ato-desktop-session-graph-stop".to_string(),
            handle: "capsule://example/demo".to_string(),
            normalized_handle: "capsule://example/demo".to_string(),
            canonical_handle: Some("capsule://example/demo".to_string()),
            trust_state: TrustState::Untrusted,
            source: Some("registry".to_string()),
            restricted: false,
            snapshot: None,
            runtime: CapsuleRuntimeDescriptor {
                target_label: "web".to_string(),
                runtime: Some("source".to_string()),
                driver: None,
                language: None,
                port: None,
            },
            display_strategy: CapsuleDisplayStrategy::WebUrl,
            pid: 4242,
            log_path: temp.path().join("session.log").display().to_string(),
            manifest_path: temp.path().join("capsule.toml").display().to_string(),
            target_label: "web".to_string(),
            notes: vec![],
            guest: None,
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: Some(StoredDependencyContracts {
                consumer_pid: 4242,
                providers: vec![
                    StoredDependencyProvider {
                        alias: "db".to_string(),
                        pid: 1,
                        state_dir: temp.path().join("state/db"),
                        resolved: "capsule://example/db@1".to_string(),
                        allocated_port: Some(5432),
                        log_path: None,
                        runtime_export_keys: vec![],
                    },
                    StoredDependencyProvider {
                        alias: "cache".to_string(),
                        pid: 2,
                        state_dir: temp.path().join("state/cache"),
                        resolved: "capsule://example/cache@1".to_string(),
                        allocated_port: Some(6379),
                        log_path: None,
                        runtime_export_keys: vec![],
                    },
                ],
            }),
            graph: Some(ato_session_core::StoredExecutionGraph {
                schema_version: ato_session_core::StoredExecutionGraph::SCHEMA_VERSION,
                nodes: vec![
                    ato_session_core::StoredGraphNode {
                        kind: NODE_KIND_PROVIDER.to_string(),
                        identifier: "cache".to_string(),
                    },
                    ato_session_core::StoredGraphNode {
                        kind: NODE_KIND_PROVIDER.to_string(),
                        identifier: "db".to_string(),
                    },
                ],
                edges: vec![
                    ato_session_core::StoredGraphEdge {
                        source: "cache".to_string(),
                        target: "output://cache".to_string(),
                        kind: EDGE_KIND_PROVIDES.to_string(),
                    },
                    ato_session_core::StoredGraphEdge {
                        source: "db".to_string(),
                        target: "output://db".to_string(),
                        kind: EDGE_KIND_PROVIDES.to_string(),
                    },
                ],
            }),
            orchestration_services: None,
            schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
            launch_digest: Some("digest".repeat(8)),
            process_start_time_unix_ms: None,
        };

        let plan = super::dependency_teardown_plan(&record)
            .expect("plan result")
            .expect("plan present");
        assert_eq!(plan.strategy, super::DependencyTeardownStrategy::Graph);
        assert_eq!(
            plan.targets
                .iter()
                .map(|target| target.dep.as_str())
                .collect::<Vec<_>>(),
            vec!["db", "cache"]
        );
    }

    #[test]
    fn dependency_teardown_plan_falls_back_to_legacy_when_graph_is_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let record = StoredSessionInfo {
            session_id: "ato-desktop-session-v0_5-stop".to_string(),
            handle: "capsule://example/demo".to_string(),
            normalized_handle: "capsule://example/demo".to_string(),
            canonical_handle: Some("capsule://example/demo".to_string()),
            trust_state: TrustState::Untrusted,
            source: Some("registry".to_string()),
            restricted: false,
            snapshot: None,
            runtime: CapsuleRuntimeDescriptor {
                target_label: "web".to_string(),
                runtime: Some("source".to_string()),
                driver: None,
                language: None,
                port: None,
            },
            display_strategy: CapsuleDisplayStrategy::WebUrl,
            pid: 4242,
            log_path: temp.path().join("session.log").display().to_string(),
            manifest_path: temp.path().join("capsule.toml").display().to_string(),
            target_label: "web".to_string(),
            notes: vec![],
            guest: None,
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: Some(StoredDependencyContracts {
                consumer_pid: 4242,
                providers: vec![StoredDependencyProvider {
                    alias: "db".to_string(),
                    pid: 1,
                    state_dir: temp.path().join("state/db"),
                    resolved: "capsule://example/db@1".to_string(),
                    allocated_port: Some(5432),
                    log_path: None,
                    runtime_export_keys: vec![],
                }],
            }),
            graph: None,
            orchestration_services: None,
            schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
            launch_digest: Some("digest".repeat(8)),
            process_start_time_unix_ms: None,
        };

        let plan = super::dependency_teardown_plan(&record)
            .expect("plan result")
            .expect("plan present");
        assert_eq!(
            plan.strategy,
            super::DependencyTeardownStrategy::LegacyDependencyContracts
        );
        assert_eq!(
            plan.targets
                .iter()
                .map(|target| target.dep.as_str())
                .collect::<Vec<_>>(),
            vec!["db"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn stop_recorded_dependency_contracts_graph_path_kills_provider() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut provider = Command::new("sleep").arg("30").spawn().expect("provider");
        let record = StoredSessionInfo {
            session_id: "ato-desktop-session-graph-kill".to_string(),
            handle: "capsule://example/demo".to_string(),
            normalized_handle: "capsule://example/demo".to_string(),
            canonical_handle: Some("capsule://example/demo".to_string()),
            trust_state: TrustState::Untrusted,
            source: Some("registry".to_string()),
            restricted: false,
            snapshot: None,
            runtime: CapsuleRuntimeDescriptor {
                target_label: "web".to_string(),
                runtime: Some("source".to_string()),
                driver: None,
                language: None,
                port: None,
            },
            display_strategy: CapsuleDisplayStrategy::WebUrl,
            pid: 4242,
            log_path: temp.path().join("session.log").display().to_string(),
            manifest_path: temp.path().join("capsule.toml").display().to_string(),
            target_label: "web".to_string(),
            notes: vec![],
            guest: None,
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: Some(StoredDependencyContracts {
                consumer_pid: 4242,
                providers: vec![StoredDependencyProvider {
                    alias: "db".to_string(),
                    pid: provider.id() as i32,
                    state_dir: temp.path().join("state/db"),
                    resolved: "capsule://example/db@1".to_string(),
                    allocated_port: Some(5432),
                    log_path: None,
                    runtime_export_keys: vec![],
                }],
            }),
            graph: Some(ato_session_core::StoredExecutionGraph {
                schema_version: ato_session_core::StoredExecutionGraph::SCHEMA_VERSION,
                nodes: vec![ato_session_core::StoredGraphNode {
                    kind: NODE_KIND_PROVIDER.to_string(),
                    identifier: "db".to_string(),
                }],
                edges: vec![ato_session_core::StoredGraphEdge {
                    source: "db".to_string(),
                    target: "output://db".to_string(),
                    kind: EDGE_KIND_PROVIDES.to_string(),
                }],
            }),
            orchestration_services: None,
            schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
            launch_digest: Some("digest".repeat(8)),
            process_start_time_unix_ms: None,
        };

        let stopped = super::stop_recorded_dependency_contracts(Some(&record), true)
            .expect("stop graph path");
        assert!(stopped);
        for _ in 0..40 {
            if provider.try_wait().expect("provider wait").is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = provider.kill();
        panic!("graph-backed provider teardown did not stop provider within 1s");
    }

    #[cfg(unix)]
    #[test]
    fn stop_recorded_dependency_contracts_v0_5_fallback_kills_provider() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut provider = Command::new("sleep").arg("30").spawn().expect("provider");
        let record = StoredSessionInfo {
            session_id: "ato-desktop-session-v0_5-kill".to_string(),
            handle: "capsule://example/demo".to_string(),
            normalized_handle: "capsule://example/demo".to_string(),
            canonical_handle: Some("capsule://example/demo".to_string()),
            trust_state: TrustState::Untrusted,
            source: Some("registry".to_string()),
            restricted: false,
            snapshot: None,
            runtime: CapsuleRuntimeDescriptor {
                target_label: "web".to_string(),
                runtime: Some("source".to_string()),
                driver: None,
                language: None,
                port: None,
            },
            display_strategy: CapsuleDisplayStrategy::WebUrl,
            pid: 4242,
            log_path: temp.path().join("session.log").display().to_string(),
            manifest_path: temp.path().join("capsule.toml").display().to_string(),
            target_label: "web".to_string(),
            notes: vec![],
            guest: None,
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: Some(StoredDependencyContracts {
                consumer_pid: 4242,
                providers: vec![StoredDependencyProvider {
                    alias: "db".to_string(),
                    pid: provider.id() as i32,
                    state_dir: temp.path().join("state/db"),
                    resolved: "capsule://example/db@1".to_string(),
                    allocated_port: Some(5432),
                    log_path: None,
                    runtime_export_keys: vec![],
                }],
            }),
            graph: None,
            orchestration_services: None,
            schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
            launch_digest: Some("digest".repeat(8)),
            process_start_time_unix_ms: None,
        };

        let stopped = super::stop_recorded_dependency_contracts(Some(&record), true)
            .expect("stop legacy fallback");
        assert!(stopped);
        for _ in 0..40 {
            if provider.try_wait().expect("provider wait").is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = provider.kill();
        panic!("legacy dependency-contract fallback did not stop provider within 1s");
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

    #[cfg(unix)]
    #[test]
    #[serial]
    #[ignore = "flaky: races SIGTERM delivery against try_wait, and shares HOME/ATO_DESKTOP_SESSION_ROOT with sibling tests; tracked in #82"]
    fn stop_session_uses_record_dependency_contracts_when_sidecar_is_missing() {
        struct EnvGuard {
            ato_home: Option<String>,
            home: Option<String>,
            session_root: Option<String>,
        }

        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match &self.ato_home {
                    Some(value) => std::env::set_var("ATO_HOME", value),
                    None => std::env::remove_var("ATO_HOME"),
                }
                match &self.home {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
                match &self.session_root {
                    Some(value) => std::env::set_var("ATO_DESKTOP_SESSION_ROOT", value),
                    None => std::env::remove_var("ATO_DESKTOP_SESSION_ROOT"),
                }
            }
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let session_root = temp.path().join("sessions");
        fs::create_dir_all(&session_root).expect("create session root");
        let _guard = EnvGuard {
            ato_home: std::env::var("ATO_HOME").ok(),
            home: std::env::var("HOME").ok(),
            session_root: std::env::var("ATO_DESKTOP_SESSION_ROOT").ok(),
        };
        std::env::set_var("ATO_HOME", temp.path());
        std::env::set_var("HOME", temp.path());
        std::env::set_var("ATO_DESKTOP_SESSION_ROOT", &session_root);

        let mut consumer = Command::new("sleep").arg("30").spawn().expect("consumer");
        let mut provider = Command::new("sleep").arg("30").spawn().expect("provider");

        let session_id = format!("ato-desktop-session-{}", consumer.id());
        ProcessManager::new()
            .expect("process manager")
            .write_pid(&ProcessInfo {
                id: session_id.clone(),
                name: "capsule-session".to_string(),
                pid: consumer.id() as i32,
                workload_pid: None,
                status: ProcessStatus::Running,
                runtime: "source".to_string(),
                start_time: SystemTime::now(),
                os_start_time_unix_ms: None,
                workload_os_start_time_unix_ms: None,
                manifest_path: None,
                scoped_id: None,
                target_label: Some("web".to_string()),
                requested_port: None,
                log_path: None,
                ready_at: None,
                last_event: Some("ready".to_string()),
                last_error: None,
                exit_code: None,
            })
            .expect("write pid file");

        write_session_record(
            &session_root,
            &StoredSessionInfo {
                session_id: session_id.clone(),
                handle: "capsule://example/demo".to_string(),
                normalized_handle: "capsule://example/demo".to_string(),
                canonical_handle: Some("capsule://example/demo".to_string()),
                trust_state: TrustState::Untrusted,
                source: Some("registry".to_string()),
                restricted: false,
                snapshot: None,
                runtime: CapsuleRuntimeDescriptor {
                    target_label: "web".to_string(),
                    runtime: Some("source".to_string()),
                    driver: None,
                    language: None,
                    port: None,
                },
                display_strategy: CapsuleDisplayStrategy::WebUrl,
                pid: consumer.id() as i32,
                log_path: session_root.join("session.log").display().to_string(),
                manifest_path: temp.path().join("capsule.toml").display().to_string(),
                target_label: "web".to_string(),
                notes: vec![],
                guest: None,
                web: Some(WebSessionDisplay {
                    local_url: "http://127.0.0.1:9999/".to_string(),
                    healthcheck_url: "http://127.0.0.1:9999/".to_string(),
                    served_by: "ato".to_string(),
                }),
                terminal: None,
                service: None,
                dependency_contracts: Some(StoredDependencyContracts {
                    consumer_pid: consumer.id() as i32,
                    providers: vec![StoredDependencyProvider {
                        alias: "db".to_string(),
                        pid: provider.id() as i32,
                        state_dir: temp.path().join("state/db"),
                        resolved: "capsule://github.com/Koh0920/ato-postgres@main".to_string(),
                        allocated_port: Some(5432),
                        log_path: Some(temp.path().join("db.log")),
                        runtime_export_keys: vec!["DATABASE_URL".to_string()],
                    }],
                }),
                graph: None,
                orchestration_services: None,
                schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
                launch_digest: Some("digest".repeat(8)),
                process_start_time_unix_ms: None,
            },
        )
        .expect("write session record");

        stop_session(&session_id, true).expect("stop session");

        assert!(consumer.try_wait().expect("consumer wait").is_some());
        assert!(provider.try_wait().expect("provider wait").is_some());
        assert!(!session_root.join(format!("{}.json", session_id)).exists());
        assert!(ProcessManager::new()
            .expect("process manager after stop")
            .read_dependency_session_snapshot(&session_id)
            .expect("read dependency session after stop")
            .is_none());
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    #[ignore = "flaky: races SIGTERM delivery against try_wait, and shares HOME/ATO_HOME/ATO_DESKTOP_SESSION_ROOT with sibling tests; tracked in #82"]
    fn stop_session_uses_record_dependency_contracts_when_pid_file_is_missing() {
        struct EnvGuard {
            ato_home: Option<String>,
            home: Option<String>,
            session_root: Option<String>,
        }

        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match &self.ato_home {
                    Some(value) => std::env::set_var("ATO_HOME", value),
                    None => std::env::remove_var("ATO_HOME"),
                }
                match &self.home {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
                match &self.session_root {
                    Some(value) => std::env::set_var("ATO_DESKTOP_SESSION_ROOT", value),
                    None => std::env::remove_var("ATO_DESKTOP_SESSION_ROOT"),
                }
            }
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let session_root = temp.path().join("sessions");
        fs::create_dir_all(&session_root).expect("create session root");
        let _guard = EnvGuard {
            ato_home: std::env::var("ATO_HOME").ok(),
            home: std::env::var("HOME").ok(),
            session_root: std::env::var("ATO_DESKTOP_SESSION_ROOT").ok(),
        };
        std::env::set_var("ATO_HOME", temp.path());
        std::env::set_var("HOME", temp.path());
        std::env::set_var("ATO_DESKTOP_SESSION_ROOT", &session_root);

        let mut provider = Command::new("sleep").arg("30").spawn().expect("provider");
        let session_id = "ato-desktop-session-missing-pid".to_string();
        ProcessManager::new()
            .expect("process manager")
            .write_dependency_session_snapshot(
                &session_id,
                &crate::runtime::process::DependencyContractSessionSnapshot {
                    session_id: session_id.clone(),
                    consumer_pid: 999_999_999,
                    providers: Vec::new(),
                },
            )
            .expect("write empty sidecar");

        write_session_record(
            &session_root,
            &StoredSessionInfo {
                session_id: session_id.clone(),
                handle: "capsule://example/demo".to_string(),
                normalized_handle: "capsule://example/demo".to_string(),
                canonical_handle: Some("capsule://example/demo".to_string()),
                trust_state: TrustState::Untrusted,
                source: Some("registry".to_string()),
                restricted: false,
                snapshot: None,
                runtime: CapsuleRuntimeDescriptor {
                    target_label: "web".to_string(),
                    runtime: Some("source".to_string()),
                    driver: None,
                    language: None,
                    port: None,
                },
                display_strategy: CapsuleDisplayStrategy::WebUrl,
                pid: 999_999_999,
                log_path: session_root.join("session.log").display().to_string(),
                manifest_path: temp.path().join("capsule.toml").display().to_string(),
                target_label: "web".to_string(),
                notes: vec![],
                guest: None,
                web: Some(WebSessionDisplay {
                    local_url: "http://127.0.0.1:9999/".to_string(),
                    healthcheck_url: "http://127.0.0.1:9999/".to_string(),
                    served_by: "ato".to_string(),
                }),
                terminal: None,
                service: None,
                dependency_contracts: Some(StoredDependencyContracts {
                    consumer_pid: 999_999_999,
                    providers: vec![StoredDependencyProvider {
                        alias: "db".to_string(),
                        pid: provider.id() as i32,
                        state_dir: temp.path().join("state/db"),
                        resolved: "capsule://github.com/Koh0920/ato-postgres@main".to_string(),
                        allocated_port: Some(5432),
                        log_path: Some(temp.path().join("db.log")),
                        runtime_export_keys: vec!["DATABASE_URL".to_string()],
                    }],
                }),
                graph: None,
                orchestration_services: None,
                schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
                launch_digest: Some("digest".repeat(8)),
                process_start_time_unix_ms: None,
            },
        )
        .expect("write session record");

        stop_session(&session_id, true).expect("stop session");

        assert!(provider.try_wait().expect("provider wait").is_some());
        assert!(!session_root.join(format!("{}.json", session_id)).exists());
        assert!(ProcessManager::new()
            .expect("process manager after stop")
            .read_dependency_session_snapshot(&session_id)
            .expect("read dependency session after stop")
            .is_none());
    }

    #[test]
    fn desktop_parent_process_matcher_rejects_dead_pid() {
        assert!(!desktop_parent_process_matches(999_999_999, None));
    }

    #[test]
    fn desktop_parent_process_matcher_accepts_current_pid() {
        let pid = std::process::id();
        let start_time = ato_session_core::process::process_start_time_unix_ms(pid);
        assert!(desktop_parent_process_matches(pid, start_time));
    }

    /// PR-D: `stop_recorded_orchestration_services` walks the persisted
    /// `[services]` graph subset in reverse-topological order and stops
    /// each managed (local-pid) service. OCI services are not exercised
    /// here because hosted CI runners don't have a Docker daemon; that
    /// path is verified manually via the desktop integration suite.
    ///
    /// The helper is exercised directly (not through `stop_session`) to
    /// avoid the env-touching test isolation gap tracked in #82.
    #[cfg(unix)]
    #[test]
    fn stop_recorded_orchestration_services_kills_managed_pids_in_reverse_order() {
        use std::collections::BTreeMap;

        let temp = tempfile::tempdir().expect("tempdir");

        // Two long-running sleeps to stand in for a managed `[services]`
        // graph (db started first, web second; teardown should hit web
        // before db).
        let mut db = Command::new("sleep").arg("30").spawn().expect("db");
        let mut web = Command::new("sleep").arg("30").spawn().expect("web");

        let record = StoredSessionInfo {
            session_id: "ato-desktop-session-orch".to_string(),
            handle: "capsule://example/orch".to_string(),
            normalized_handle: "capsule://example/orch".to_string(),
            canonical_handle: Some("capsule://example/orch".to_string()),
            trust_state: TrustState::Untrusted,
            source: Some("registry".to_string()),
            restricted: false,
            snapshot: None,
            runtime: CapsuleRuntimeDescriptor {
                target_label: "web".to_string(),
                runtime: Some("source".to_string()),
                driver: None,
                language: None,
                port: None,
            },
            display_strategy: CapsuleDisplayStrategy::WebUrl,
            pid: std::process::id() as i32,
            log_path: temp.path().join("session.log").display().to_string(),
            manifest_path: temp.path().join("capsule.toml").display().to_string(),
            target_label: "web".to_string(),
            notes: vec![],
            guest: None,
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: None,
            graph: None,
            orchestration_services: Some(StoredOrchestrationServices {
                wrapper_pid: std::process::id() as i32,
                services: vec![
                    StoredOrchestrationService {
                        name: "db".to_string(),
                        target_label: "db".to_string(),
                        local_pid: Some(db.id() as i32),
                        container_id: None,
                        host_ports: BTreeMap::new(),
                        // `None` deliberately: this test exercises the
                        // recorded-pid teardown path. The
                        // published_port fallback in
                        // `stop_recorded_orchestration_services`
                        // (#108) would otherwise call `lsof` for
                        // common dev ports and could SIGKILL whatever
                        // happens to listen on them on the host (e.g.
                        // a sibling test, a postgres provider) — see
                        // the dedicated `..._kills_orphan_listener_..`
                        // test for the fallback path.
                        published_port: None,
                    },
                    StoredOrchestrationService {
                        name: "web".to_string(),
                        target_label: "web".to_string(),
                        local_pid: Some(web.id() as i32),
                        container_id: None,
                        host_ports: BTreeMap::new(),
                        published_port: None,
                    },
                ],
            }),
            schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
            launch_digest: Some("digest".repeat(8)),
            process_start_time_unix_ms: None,
        };

        let stopped = super::stop_recorded_orchestration_services(Some(&record), true)
            .expect("teardown helper");
        assert!(
            stopped,
            "helper must report it stopped at least one service"
        );

        // Both sleeps must be killed. We poll briefly: SIGKILL is delivered
        // synchronously by the kernel but `try_wait` in user space sees the
        // exit only after the next reaping pass. 1 second is overkill on
        // any sane host but tolerates loaded CI runners — without this
        // poll the assertion races SIGKILL delivery the same way #82's
        // sibling test does (avoid that landmine).
        for _ in 0..40 {
            let db_done = db.try_wait().expect("db wait").is_some();
            let web_done = web.try_wait().expect("web wait").is_some();
            if db_done && web_done {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        // Defense: clean up any survivors so the test process doesn't leak
        // children even on assertion failure.
        let _ = db.kill();
        let _ = web.kill();
        panic!("orchestration services were not stopped within 1s after teardown");
    }

    /// #108 regression: full `stop_session` path for an in-process
    /// orchestration session whose recorded `pid` is already dead by the
    /// time stop is called (the canonical PR-C scenario where the wrapper
    /// process exits successfully after detaching the workload runtime
    /// via `Box::leak`). The teardown must fall through to the persisted
    /// `orchestration_services` subset and still report `stopped:true`.
    ///
    /// `#[ignore]` matches the sibling tests above — they mutate
    /// `ATO_HOME`/`HOME`/`ATO_DESKTOP_SESSION_ROOT` process-globally and
    /// race with each other on shared CI runners (#82). Run locally
    /// with `cargo test … -- --ignored`.
    #[cfg(unix)]
    #[test]
    #[serial]
    #[ignore = "mutates HOME/ATO_HOME/ATO_DESKTOP_SESSION_ROOT (#82)"]
    fn stop_session_kills_orchestration_services_when_recorded_pid_is_dead() {
        use std::collections::BTreeMap;

        struct EnvGuard {
            ato_home: Option<String>,
            home: Option<String>,
            session_root: Option<String>,
        }

        impl Drop for EnvGuard {
            fn drop(&mut self) {
                match &self.ato_home {
                    Some(value) => std::env::set_var("ATO_HOME", value),
                    None => std::env::remove_var("ATO_HOME"),
                }
                match &self.home {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
                match &self.session_root {
                    Some(value) => std::env::set_var("ATO_DESKTOP_SESSION_ROOT", value),
                    None => std::env::remove_var("ATO_DESKTOP_SESSION_ROOT"),
                }
            }
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let session_root = temp.path().join("sessions");
        fs::create_dir_all(&session_root).expect("create session root");
        let _guard = EnvGuard {
            ato_home: std::env::var("ATO_HOME").ok(),
            home: std::env::var("HOME").ok(),
            session_root: std::env::var("ATO_DESKTOP_SESSION_ROOT").ok(),
        };
        std::env::set_var("ATO_HOME", temp.path());
        std::env::set_var("HOME", temp.path());
        std::env::set_var("ATO_DESKTOP_SESSION_ROOT", &session_root);

        // Stand-ins for the live `[services]` workloads that survived the
        // wrapper exit (e.g. uvicorn for `app`, vite for `web`). `db` is
        // started first so reverse-topological teardown hits `web` before
        // `db`.
        let mut db = Command::new("sleep").arg("30").spawn().expect("db");
        let mut web = Command::new("sleep").arg("30").spawn().expect("web");

        // The id was minted from the leaf's pid at session start, but by
        // the time stop is called that pid is dead — a generic dead pid
        // here mirrors the same observable state.
        let dead_recorded_pid: i32 = 999_999_999;
        let session_id = format!("ato-desktop-session-{}", web.id());

        ProcessManager::new()
            .expect("process manager")
            .write_pid(&ProcessInfo {
                id: session_id.clone(),
                name: "capsule-session".to_string(),
                pid: dead_recorded_pid,
                workload_pid: None,
                status: ProcessStatus::Running,
                runtime: "source".to_string(),
                start_time: SystemTime::now(),
                os_start_time_unix_ms: None,
                workload_os_start_time_unix_ms: None,
                manifest_path: None,
                scoped_id: None,
                target_label: Some("web".to_string()),
                requested_port: Some(5173),
                log_path: None,
                ready_at: None,
                last_event: Some("ready".to_string()),
                last_error: None,
                exit_code: None,
            })
            .expect("write pid file");

        write_session_record(
            &session_root,
            &StoredSessionInfo {
                session_id: session_id.clone(),
                handle: "capsule://example/orch".to_string(),
                normalized_handle: "capsule://example/orch".to_string(),
                canonical_handle: Some("capsule://example/orch".to_string()),
                trust_state: TrustState::Untrusted,
                source: Some("registry".to_string()),
                restricted: false,
                snapshot: None,
                runtime: CapsuleRuntimeDescriptor {
                    target_label: "web".to_string(),
                    runtime: Some("source".to_string()),
                    driver: None,
                    language: None,
                    port: Some(5173),
                },
                display_strategy: CapsuleDisplayStrategy::WebUrl,
                pid: dead_recorded_pid,
                log_path: session_root.join("session.log").display().to_string(),
                manifest_path: temp.path().join("capsule.toml").display().to_string(),
                target_label: "web".to_string(),
                notes: vec![],
                guest: None,
                web: Some(WebSessionDisplay {
                    local_url: "http://127.0.0.1:5173/".to_string(),
                    healthcheck_url: "http://127.0.0.1:5173/".to_string(),
                    served_by: "ato".to_string(),
                }),
                terminal: None,
                service: None,
                dependency_contracts: None,
                graph: None,
                orchestration_services: Some(StoredOrchestrationServices {
                    wrapper_pid: dead_recorded_pid,
                    services: vec![
                        StoredOrchestrationService {
                            name: "db".to_string(),
                            target_label: "db".to_string(),
                            local_pid: Some(db.id() as i32),
                            container_id: None,
                            host_ports: BTreeMap::new(),
                            // See note in the sibling
                            // `..._kills_managed_pids_in_reverse_order`
                            // test: deliberately `None` so the
                            // published_port fallback doesn't reach
                            // for whatever sibling test happens to
                            // listen on 5432/5173.
                            published_port: None,
                        },
                        StoredOrchestrationService {
                            name: "web".to_string(),
                            target_label: "web".to_string(),
                            local_pid: Some(web.id() as i32),
                            container_id: None,
                            host_ports: BTreeMap::new(),
                            published_port: None,
                        },
                    ],
                }),
                schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
                launch_digest: Some("digest".repeat(8)),
                process_start_time_unix_ms: None,
            },
        )
        .expect("write session record");

        stop_session(&session_id, true).expect("stop session");

        // SIGKILL is delivered by the kernel synchronously, but `try_wait`
        // sees the exit only after the next reaping pass. Same poll
        // pattern as `stop_recorded_orchestration_services_kills_managed_pids_in_reverse_order`.
        for _ in 0..40 {
            let db_done = db.try_wait().expect("db wait").is_some();
            let web_done = web.try_wait().expect("web wait").is_some();
            if db_done && web_done {
                assert!(
                    !session_root.join(format!("{}.json", session_id)).exists(),
                    "stopped session must remove its record file"
                );
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = db.kill();
        let _ = web.kill();
        panic!("orchestration services were not stopped within 1s after stop_session");
    }

    /// #108 wrapper-vs-workload fallback: when ato spawned the service via
    /// `npm run dev` / `uv run` / a shell wrapper, the wrapper exits or is
    /// killed but the actual listener (vite/uvicorn) survives as an orphan
    /// still bound to `published_port`. The teardown helper must then
    /// resolve the live listener via `lsof` and kill it.
    ///
    /// Repro shape:
    ///   - Recorded `local_pid` is dead (mimics the wrapper having exited).
    ///   - A live "workload" listens on the recorded `published_port`.
    ///
    /// Expected: helper reports `stopped:true` and the workload is killed.
    ///
    /// `#[ignore]`d because the test depends on a real `lsof` invocation
    /// against a kernel-allocated port plus a python3 cold-start, which
    /// is flaky on a fully-loaded `cargo test` job (the 1200+ siblings
    /// can starve the python startup past its bind window). Run locally
    /// or in a serialized lane via `cargo test … -- --ignored`.
    #[cfg(unix)]
    #[test]
    #[ignore = "depends on lsof + python3 cold-start; flaky under loaded `cargo test`"]
    fn stop_recorded_orchestration_services_kills_orphan_listener_via_published_port() {
        use std::collections::BTreeMap;
        use std::net::TcpListener;

        // Bind a real listener on a kernel-allocated port so `lsof`
        // resolves it back to our test process. Holding the listener
        // across the teardown call is fine — the port is what's being
        // probed, not the FD.
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("bind workload listener for fallback test");
        let port = listener.local_addr().expect("listener addr").port();

        // Spawn a child sleep that holds the socket open for `lsof` to
        // see. We pass the listener fd to the child via a small helper:
        // `nc -l <port>` is not portable enough across BSD/GNU netcat,
        // and we'd rather not depend on it. Instead the child process is
        // a `sh -c` that re-binds the same port (we drop our listener
        // first so the child can claim it). This avoids needing fd
        // inheritance plumbing in the test.
        drop(listener);

        // Use Python's stdlib http.server as the orphan listener: it's
        // present on every macOS / Linux dev box (the same hosts the
        // capsule itself runs on) and blocks until killed. Avoids
        // depending on `nc -l` whose flag set differs between BSD and
        // GNU netcat.
        let workload = Command::new("python3")
            .args([
                "-c",
                &format!(
                    "import http.server, socketserver; \
                     socketserver.TCPServer.allow_reuse_address = True; \
                     httpd = socketserver.TCPServer(('127.0.0.1', {port}), http.server.SimpleHTTPRequestHandler); \
                     httpd.serve_forever()"
                ),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn orphan workload");
        let workload_pid = workload.id();

        // Wait for the listener to bind. The cold-import + bind path is
        // ~30-80ms on an idle macOS/Linux dev box but balloons under a
        // saturated `cargo test` job (1200+ siblings competing for the
        // CPU), so we give python3 up to 10s before treating it as a
        // setup failure.
        let bound_deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(pids) = listener_pids_on_port(port) {
                if pids.contains(&workload_pid) {
                    break;
                }
            }
            if std::time::Instant::now() >= bound_deadline {
                let _ = unsafe { libc::kill(workload_pid as libc::pid_t, libc::SIGKILL) };
                panic!(
                    "orphan workload (pid {workload_pid}) failed to bind 127.0.0.1:{port} within 10s"
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        // The recorded local_pid is one we know is dead — the very high
        // pid mirrors the post-wrapper-exit state from the issue.
        let dead_recorded_pid: i32 = 999_999_999;

        let record = StoredSessionInfo {
            session_id: "ato-desktop-session-fallback".to_string(),
            handle: "capsule://example/orch".to_string(),
            normalized_handle: "capsule://example/orch".to_string(),
            canonical_handle: Some("capsule://example/orch".to_string()),
            trust_state: TrustState::Untrusted,
            source: Some("registry".to_string()),
            restricted: false,
            snapshot: None,
            runtime: CapsuleRuntimeDescriptor {
                target_label: "web".to_string(),
                runtime: Some("source".to_string()),
                driver: None,
                language: None,
                port: Some(port),
            },
            display_strategy: CapsuleDisplayStrategy::WebUrl,
            pid: dead_recorded_pid,
            log_path: format!("/tmp/session-fallback-{port}.log"),
            manifest_path: format!("/tmp/capsule-fallback-{port}.toml"),
            target_label: "web".to_string(),
            notes: vec![],
            guest: None,
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: None,
            graph: None,
            orchestration_services: Some(StoredOrchestrationServices {
                wrapper_pid: dead_recorded_pid,
                services: vec![StoredOrchestrationService {
                    name: "web".to_string(),
                    target_label: "web".to_string(),
                    local_pid: Some(dead_recorded_pid),
                    container_id: None,
                    host_ports: BTreeMap::new(),
                    published_port: Some(port),
                }],
            }),
            schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
            launch_digest: Some("digest".repeat(8)),
            process_start_time_unix_ms: None,
        };

        let stopped = super::stop_recorded_orchestration_services(Some(&record), true)
            .expect("teardown helper");
        assert!(
            stopped,
            "helper must report it stopped the orphan port-{port} listener via the published_port fallback"
        );

        // Poll for the workload to actually exit. Same race window as
        // `stop_recorded_orchestration_services_kills_managed_pids_in_reverse_order`.
        let mut workload = workload;
        for _ in 0..40 {
            if workload.try_wait().expect("workload wait").is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = workload.kill();
        panic!("orphan workload (pid {workload_pid}) was not killed within 1s");
    }

    /// #111 wrapper-and-workload pgroup teardown: the recorded `local_pid`
    /// is a process-group leader (nacelle spawns every local service with
    /// `cmd.process_group(0)` — `supervisor.rs:267`), and the workload
    /// listener is its child in the same pgroup. Before this fix,
    /// `stop_recorded_orchestration_services` only signaled the recorded
    /// pid; if it was the wrapper (`uv run`, `npm run dev`, …) it died but
    /// the workload child became an orphan listener — handled by the #109
    /// `lsof published_port` fallback. The reverse case — recorded pid is
    /// the workload's wrapper but the wrapper itself sits in a
    /// wait-for-child loop — leaked the wrapper as an init-reparented
    /// orphan even after the workload died (#92 AODD Phase 2 evidence
    /// pattern).
    ///
    /// Repro shape:
    ///   - sh wrapper spawned with `process_group(0)` is the pgroup leader.
    ///   - python3 child (the listener) inherits that pgroup.
    ///   - Recorded `local_pid` is the wrapper.
    ///
    /// Expected: helper reports `stopped:true`, both wrapper AND workload
    /// are dead afterwards, no orphan in either branch.
    ///
    /// `#[ignore]`d for the same reason as the orphan-listener test: the
    /// python3 cold-start under a saturated `cargo test` job can starve
    /// past its bind window. Run locally or in a serialized lane via
    /// `cargo test … -- --ignored`.
    #[cfg(unix)]
    #[test]
    #[ignore = "depends on python3 cold-start + Command::process_group; flaky under loaded `cargo test`"]
    fn stop_recorded_orchestration_services_kills_wrapper_and_child_via_pgroup() {
        use std::collections::BTreeMap;
        use std::net::TcpListener;
        use std::os::unix::process::CommandExt;

        // Reserve a kernel-allocated port; drop the listener so the child
        // can bind it. (Same pattern as the `..._orphan_listener_..` test.)
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("bind probe listener for pgroup test");
        let port = listener.local_addr().expect("listener addr").port();
        drop(listener);

        // Wrapper: a sh that backgrounds the python listener and waits
        // on it. The whole tree is in a fresh pgroup (process_group(0)),
        // mirroring how nacelle spawns services. SIGKILL on the wrapper
        // alone would leave the python child as an orphan; SIGKILL on
        // the python alone would leave the sh sitting in `wait`. Only
        // the negative-pid pgroup signal takes both out atomically.
        let mut wrapper = Command::new("sh")
            .args([
                "-c",
                &format!(
                    "python3 -c 'import http.server, socketserver; \
                     socketserver.TCPServer.allow_reuse_address = True; \
                     httpd = socketserver.TCPServer((\"127.0.0.1\", {port}), http.server.SimpleHTTPRequestHandler); \
                     httpd.serve_forever()' & \
                     wait $!"
                ),
            ])
            .process_group(0)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn wrapper sh");
        let wrapper_pid = wrapper.id();

        // Wait for the python child to actually bind the port — same
        // 10s budget as the sibling fallback test.
        let bound_deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(pids) = listener_pids_on_port(port) {
                if !pids.is_empty() {
                    break;
                }
            }
            if std::time::Instant::now() >= bound_deadline {
                let pgid = unsafe { libc::getpgid(wrapper_pid as libc::pid_t) };
                if pgid > 0 {
                    let _ = unsafe { libc::kill(-pgid, libc::SIGKILL) };
                }
                let _ = wrapper.kill();
                panic!(
                    "wrapper child failed to bind 127.0.0.1:{port} within 10s (wrapper pid {wrapper_pid})"
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        // Sanity: the wrapper IS its own pgroup leader before stop —
        // this is the precondition the new branch relies on. Documenting
        // it here so a future regression in `cmd.process_group(0)` (e.g.
        // accidentally inheriting cargo-test's pgroup) shows up as a
        // setup failure rather than a flaky teardown assertion.
        let pgid_before = unsafe { libc::getpgid(wrapper_pid as libc::pid_t) };
        assert_eq!(
            pgid_before as u32, wrapper_pid,
            "wrapper must be its own pgroup leader for this test to be meaningful"
        );

        let record = StoredSessionInfo {
            session_id: "ato-desktop-session-pgroup".to_string(),
            handle: "capsule://example/orch".to_string(),
            normalized_handle: "capsule://example/orch".to_string(),
            canonical_handle: Some("capsule://example/orch".to_string()),
            trust_state: TrustState::Untrusted,
            source: Some("registry".to_string()),
            restricted: false,
            snapshot: None,
            runtime: CapsuleRuntimeDescriptor {
                target_label: "web".to_string(),
                runtime: Some("source".to_string()),
                driver: None,
                language: None,
                port: Some(port),
            },
            display_strategy: CapsuleDisplayStrategy::WebUrl,
            pid: wrapper_pid as i32,
            log_path: format!("/tmp/session-pgroup-{port}.log"),
            manifest_path: format!("/tmp/capsule-pgroup-{port}.toml"),
            target_label: "web".to_string(),
            notes: vec![],
            guest: None,
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: None,
            graph: None,
            orchestration_services: Some(StoredOrchestrationServices {
                wrapper_pid: wrapper_pid as i32,
                services: vec![StoredOrchestrationService {
                    name: "web".to_string(),
                    target_label: "web".to_string(),
                    local_pid: Some(wrapper_pid as i32),
                    container_id: None,
                    host_ports: BTreeMap::new(),
                    published_port: Some(port),
                }],
            }),
            schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
            launch_digest: Some("digest".repeat(8)),
            process_start_time_unix_ms: None,
        };

        let stopped = super::stop_recorded_orchestration_services(Some(&record), true)
            .expect("teardown helper");
        assert!(
            stopped,
            "helper must report it stopped the wrapper+child pgroup"
        );

        // Wrapper must be reaped — same poll budget as the sibling tests.
        for _ in 0..40 {
            if wrapper.try_wait().expect("wrapper wait").is_some() {
                // Workload child died inside the pgroup kill; verify the
                // port is free as well so we know we didn't leave an
                // unsignaled descendant.
                if listener_pids_on_port(port)
                    .map(|p| p.is_empty())
                    .unwrap_or(false)
                {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        // Cleanup before failing so we don't leak the test's own orphan.
        let pgid = unsafe { libc::getpgid(wrapper_pid as libc::pid_t) };
        if pgid > 0 {
            let _ = unsafe { libc::kill(-pgid, libc::SIGKILL) };
        }
        let _ = wrapper.kill();
        panic!(
            "wrapper (pid {wrapper_pid}) and/or its child listener on port {port} were not reaped within 1s of pgroup stop"
        );
    }
}
