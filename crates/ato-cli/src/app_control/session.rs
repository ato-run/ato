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
    GuestSessionDisplay, MaterializedLaunchRecord, ServiceBackgroundDisplay,
    StoredDependencyContracts, StoredDependencyProvider, StoredOrchestrationService,
    StoredOrchestrationServices, StoredSessionInfo, TerminalSessionDisplay, WebSessionDisplay,
    launch_cache_root, write_materialized_launch_record_atomic, write_session_record_atomic,
};
use capsule_core::ato_lock;
use capsule_core::handle::{
    CanonicalHandle, CapsuleDisplayStrategy, CapsuleRuntimeDescriptor, ResolvedSnapshot,
    TrustState, normalize_capsule_handle,
};
use capsule_core::launch_spec::derive_launch_spec;
use capsule_core::routing::input_resolver::ATO_LOCK_FILE_NAME;
use serde::Serialize;

use crate::ProviderToolchain;
use crate::adapters::runtime::oci_provider::select_ready_runtime_oci_provider;
#[cfg(unix)]
use crate::application::orchestration_teardown::{collect_descendant_pids, listener_pids_on_port};
use crate::application::pipeline::phases::run::{
    DependencyContractGuard, DerivedBridgeManifest, PreparedRunContext,
    persist_background_dependency_contracts, setup_dependency_contracts_launch_context,
};
use crate::application::session_graph_populate::{
    EDGE_KIND_PROVIDES, NODE_KIND_PROVIDER, NODE_KIND_SERVICE,
};
use crate::application::state_bindings::{
    require_explicit_persistent_state_bindings, resolve_attach_state_source_overrides,
};
use crate::executors::source::{CapsuleProcess, ExecuteMode};
use crate::executors::target_runner::{
    TargetLaunchOptions, prepare_target_execution, resolve_launch_context,
};
use crate::install::support::resolve_run_target_or_install;
use crate::reporters;
use crate::reporters::CliReporter;
use crate::runtime::process::{ProcessInfo, ProcessManager, ProcessStatus};
use crate::runtime::tree as runtime_tree;
use crate::runtime::{overrides as runtime_overrides, port_manager::PortManager};

use super::guest_contract::GuestContract;
use super::resolve::resolve_local_plan_with_state_overrides;

// Thread-local install lifecycle context set by `ato launch` before calling
// `execute_run_command`. Using a thread-local (rather than a process-global
// slot or process env vars) means:
// - Multiple sequential launches in the same process work correctly (daemon / Desktop)
// - Concurrent launches cannot cross-stamp each other's session records
// - No control-plane metadata leaks into the process environment (and thus into
//   guest / child processes)
//
// This thread-local only stamps records written on the same thread that called
// `ato launch`. The cross-thread `ato launch` session record (written by the run
// pipeline's dependency materializer on a tokio worker thread) is stamped from
// the typed `DependencyMaterializationRequest::install_lifecycle_context` instead
// — explicit data flow, not ambient process state.
//
// Always use [`ScopedInstallLifecycleGuard`] to set/clear the context so it is
// guaranteed to be cleaned up on return, even if the run pipeline panics.
thread_local! {
    static INSTALL_LIFECYCLE_CONTEXT: std::cell::RefCell<
        Option<crate::cli::commands::run::InstallLifecycleContext>,
    > = const { std::cell::RefCell::new(None) };
}

pub(crate) fn set_install_lifecycle_context(
    ctx: crate::cli::commands::run::InstallLifecycleContext,
) {
    INSTALL_LIFECYCLE_CONTEXT.with(|slot| {
        *slot.borrow_mut() = Some(ctx);
    });
}

pub(crate) fn clear_install_lifecycle_context() {
    INSTALL_LIFECYCLE_CONTEXT.with(|slot| {
        *slot.borrow_mut() = None;
    });
}

/// RAII guard that sets the thread-local install lifecycle context on construction
/// and clears it on `Drop`. Use this instead of calling `set_install_lifecycle_context`
/// directly to guarantee cleanup on all return paths (including early returns and panics).
///
/// # Example
///
/// ```rust
/// let _guard = ScopedInstallLifecycleGuard::set(lifecycle_ctx);
/// execute_run_command(...)?;
/// // Context is cleared here automatically.
/// ```
pub(crate) struct ScopedInstallLifecycleGuard;

impl ScopedInstallLifecycleGuard {
    pub(crate) fn set(ctx: crate::cli::commands::run::InstallLifecycleContext) -> Self {
        set_install_lifecycle_context(ctx);
        ScopedInstallLifecycleGuard
    }
}

impl Drop for ScopedInstallLifecycleGuard {
    fn drop(&mut self) {
        clear_install_lifecycle_context();
    }
}

fn with_install_lifecycle_context<F, R>(f: F) -> R
where
    F: FnOnce(Option<&crate::cli::commands::run::InstallLifecycleContext>) -> R,
{
    INSTALL_LIFECYCLE_CONTEXT.with(|slot| {
        let guard = slot.borrow();
        f(guard.as_ref())
    })
}

/// Return the install profile key of the active install lifecycle context, if
/// any. Captured at launch time (while still on the synchronous run thread) so
/// it can be threaded explicitly into [`RuntimeLaunchContext`]; the thread-local
/// itself does not reliably survive the async executor boundary.
pub(crate) fn current_install_profile_key() -> Option<String> {
    with_install_lifecycle_context(|ctx| ctx.map(|c| c.install_profile_key.clone()))
}

/// Stamp install lifecycle IDs onto `record` if a lifecycle context was set
/// (i.e., we are running via `ato launch`).
///
/// `CapsuleInstanceKey` is derived here from the session's own `execution_id`
/// (the receipt identity assigned by the run pipeline) so CIK, session record,
/// and receipt all share the same execution identity.
fn apply_install_lifecycle(record: &mut StoredSessionInfo) {
    with_install_lifecycle_context(|ctx| {
        if let Some(ctx) = ctx {
            record.installed_app_id = Some(ctx.installed_app_id.clone());
            record.install_profile_id = Some(ctx.install_profile_id.clone());
            record.install_profile_key = Some(ctx.install_profile_key.clone());
            record.install_revision_id = Some(ctx.install_revision_id.clone());

            // Derive CIK from the session's real execution_id so the key
            // reflects the actual receipt/execution closure, not a random id.
            if let Some(exec_id_str) = &record.execution_id {
                use capsule_core::foundation::install_lifecycle::{
                    ExecutionId, InstallProfileKey, InstallRevisionId, derive_capsule_instance_key,
                };
                let ipk = InstallProfileKey::new(ctx.install_profile_key.clone());
                let rev_id = InstallRevisionId::new(ctx.install_revision_id.clone());
                let exec = ExecutionId::new(exec_id_str.clone());
                let cik = derive_capsule_instance_key(&ipk, &rev_id, &exec);
                record.capsule_instance_key = Some(cik.as_str().to_string());
            }
            // If execution_id is not yet set at write time, capsule_instance_key
            // stays None. It can be back-filled when the receipt is finalized.
        }
    });
}

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
    /// Set when network removal was attempted but failed. Includes the
    /// network name and the last error seen so callers can retry cleanup
    /// manually (`podman network rm <name>`).
    #[serde(skip_serializing_if = "Option::is_none")]
    network_cleanup_warning: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    declared_execution_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved_execution_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    observed_execution_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    graph_completeness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reproducibility_class: Option<String>,
    /// Runtime observation v1 (#490) carrier: the observed launch envelope +
    /// diagnostic facts, captured by `start_runtime_session` after the workload
    /// reached readiness. Internal only — `#[serde(skip)]` keeps it out of the
    /// desktop session envelope (the receipt is the source of truth). The
    /// session runner consumes it to stamp the execution receipt.
    #[serde(skip)]
    observed_runtime: Option<capsule_core::execution_identity::ObservedRuntimeEvidence>,
}

impl SessionInfo {
    /// PID of the spawned process. Used by the App Session Materialization
    /// layer to enrich the freshly-written record with its process_start_time.
    pub(crate) fn pid(&self) -> i32 {
        self.pid
    }

    pub(crate) fn attach_execution_receipt_metadata(
        &mut self,
        metadata: &ExecutionReceiptSessionMetadata,
    ) {
        self.execution_id = Some(metadata.execution_id.clone());
        self.execution_receipt_schema_version = Some(metadata.schema_version);
        self.declared_execution_id = metadata.declared_execution_id.clone();
        self.resolved_execution_id = metadata.resolved_execution_id.clone();
        self.observed_execution_id = metadata.observed_execution_id.clone();
        self.graph_completeness = metadata.graph_completeness.clone();
        self.reproducibility_class = metadata.reproducibility_class.clone();
    }

    /// Attach runtime observation v1 evidence (#490) captured post-spawn.
    pub(crate) fn set_observed_runtime(
        &mut self,
        evidence: capsule_core::execution_identity::ObservedRuntimeEvidence,
    ) {
        self.observed_runtime = Some(evidence);
    }

    /// Take the observed evidence carrier (consumed by the session runner to
    /// stamp the execution receipt).
    pub(crate) fn take_observed_runtime(
        &mut self,
    ) -> Option<capsule_core::execution_identity::ObservedRuntimeEvidence> {
        self.observed_runtime.take()
    }

    /// Surface the observed execution id onto the session after the receipt has
    /// been stamped, so the desktop envelope matches the receipt.
    pub(crate) fn set_observed_execution_id(&mut self, id: Option<String>) {
        self.observed_execution_id = id;
    }

    pub(crate) fn to_materialized_launch_record(
        &self,
        resolution: &super::resolve::HandleResolution,
        app_root: &Path,
        launch_key: &str,
        launch_digest: &str,
        run_config_hash: &str,
    ) -> MaterializedLaunchRecord {
        let created_at_unix_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0);
        MaterializedLaunchRecord {
            schema_version: ato_session_core::MATERIALIZED_LAUNCH_RECORD_SCHEMA_VERSION,
            launch_key: launch_key.to_string(),
            last_session_id: Some(self.session_id.clone()),
            handle: self.handle.clone(),
            normalized_handle: resolution.normalized_handle.clone(),
            canonical_handle: resolution.canonical_handle.clone(),
            trust_state: resolution.trust_state.clone(),
            source: resolution.source.clone(),
            restricted: resolution.restricted,
            snapshot: resolution.snapshot.clone(),
            target_label: self.target_label.clone(),
            manifest_path: self.manifest_path.clone(),
            app_root: app_root.display().to_string(),
            platform: ato_session_core::current_platform_tag(),
            launch_digest: launch_digest.to_string(),
            run_config_hash: run_config_hash.to_string(),
            created_at_unix_ms,
            execution_id: self.execution_id.clone(),
            execution_receipt_schema_version: self.execution_receipt_schema_version,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ExecutionReceiptSessionMetadata {
    pub(crate) execution_id: String,
    pub(crate) schema_version: u32,
    pub(crate) declared_execution_id: Option<String>,
    pub(crate) resolved_execution_id: Option<String>,
    pub(crate) observed_execution_id: Option<String>,
    pub(crate) graph_completeness: Option<String>,
    pub(crate) reproducibility_class: Option<String>,
}

fn fetch_community_toml_to_cache(
    community_toml_id: &str,
    expected_handle: &str,
) -> Result<PathBuf> {
    validate_community_toml_id_for_session_start(community_toml_id)?;
    let content =
        crate::community::fetch_and_validate_community_toml(community_toml_id, expected_handle)
            .with_context(|| {
                format!(
                    "failed to fetch/validate community capsule.toml '{}' for '{}'",
                    community_toml_id, expected_handle
                )
            })?;
    let cache_dir = capsule_core::common::paths::ato_cache_dir().join("community-tomls");
    fs::create_dir_all(&cache_dir)
        .with_context(|| format!("failed to create {}", cache_dir.display()))?;
    let path = cache_dir.join(format!("{community_toml_id}.toml"));
    fs::write(&path, content).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn validate_community_toml_id_for_session_start(id: &str) -> Result<()> {
    let rest = id
        .strip_prefix("ctoml_")
        .ok_or_else(|| anyhow::anyhow!("--community-toml-id must start with 'ctoml_'"))?;
    if rest.is_empty()
        || !rest
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        anyhow::bail!("--community-toml-id contains invalid characters; expected ctoml_<ascii-id>");
    }
    Ok(())
}

fn auto_attach_state_args_for_community_toml_manifest(
    manifest_path: &Path,
    community_toml_id: &str,
) -> Result<Vec<String>> {
    use capsule_core::types::StateDurability;

    let raw = fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest = capsule_core::types::CapsuleManifest::from_toml(&raw)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    let state_root = capsule_core::common::paths::ato_state_dir()
        .join("community-tomls")
        .join(community_toml_id);

    Ok(manifest
        .state
        .iter()
        .filter(|(_, requirement)| requirement.durability == StateDurability::Persistent)
        .map(|(state_name, _)| {
            format!(
                "{}:{}",
                state_name,
                state_root.join(state_name).to_string_lossy()
            )
        })
        .collect())
}

// On-disk session record schema lives in `ato-session-core` (see top-of-
// file `pub(crate) use`). Keep this comment as a back-pointer because
// `git blame` for this file should still surface the design rationale:
// schema is forward-compatible, schema_version < 2 records are
// reuse-ineligible. Refactor of v0.4 (PR 4A.0 — RFC §3.2) moved the
// types out so `ato-desktop` can read records without depending on
// `ato-cli`.

pub fn start_session(
    handle: &str,
    target_label: Option<&str>,
    community_toml_id: Option<&str>,
    attach_state: &[String],
    from_materialized_record: Option<&str>,
    local_manifest_path: Option<std::path::PathBuf>,
    run_config_hash: Option<&str>,
    json: bool,
) -> Result<()> {
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

    // Envelope mode means a parent (ato-desktop) is capturing our stdout via a
    // pipe and waiting for EOF. Detach our stdio from every child the launch is
    // about to spawn so a long-lived one (the OCI `logs --follow` streamer, the
    // parent-death watcher) cannot pin the pipe open past our own exit and hang
    // the parent's `Command::output()` (#377). Windows-only; see the helper.
    #[cfg(windows)]
    if json {
        clear_std_handle_inheritance();
    }

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

    let mut runner = if let Some(manifest_path) = local_manifest_path {
        // Installed-app relaunch (`ato launch <ipk>`): the caller already
        // extracted the pinned-revision `capsule.toml`. Resolve the plan
        // directly from it (no network) so the frozen revision runs even after
        // a rollback, and reuse the detached session-start machinery (port
        // reservation, ready probe, ProcessManager registration, stamped
        // session record) so the Desktop can discover the session by its
        // install_profile_key (#565).
        if community_toml_id.is_some() || from_materialized_record.is_some() {
            anyhow::bail!(
                "internal: installed-app relaunch cannot combine a pinned manifest with --community-toml-id / --from-materialized-record"
            );
        }
        super::session_runner::SessionStartPhaseRunner::from_toml_path(
            handle,
            manifest_path,
            target_label,
            attach_state.to_vec(),
            run_config_hash.map(str::to_string),
            json,
        )
    } else if let Some(cid) = community_toml_id {
        if from_materialized_record.is_some() {
            anyhow::bail!("--community-toml-id cannot be used with --from-materialized-record");
        }
        let manifest_path = fetch_community_toml_to_cache(cid, handle)?;
        let effective_attach_state = if attach_state.is_empty() {
            auto_attach_state_args_for_community_toml_manifest(&manifest_path, cid)?
        } else {
            attach_state.to_vec()
        };
        super::session_runner::SessionStartPhaseRunner::from_toml_path(
            handle,
            manifest_path,
            target_label,
            effective_attach_state,
            run_config_hash.map(str::to_string),
            json,
        )
    } else if let Some(record_path) = from_materialized_record {
        let path = PathBuf::from(record_path);
        let record = ato_session_core::read_materialized_launch_record(&path)?;
        if record.handle != handle && record.normalized_handle != handle {
            anyhow::bail!(
                "materialized launch record {} belongs to '{}' not '{}'",
                path.display(),
                record.handle,
                handle
            );
        }
        super::session_runner::SessionStartPhaseRunner::from_materialized_record(
            record,
            run_config_hash.map(str::to_string),
            attach_state.to_vec(),
            json,
        )
    } else {
        super::session_runner::SessionStartPhaseRunner::new(
            handle,
            target_label,
            attach_state.to_vec(),
            run_config_hash.map(str::to_string),
            json,
        )
    };
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
        crate::application::receipt_boundary::emit_receipt_on_result(ctx, |sink| async {
            // PR-3b: hand the boundary's graph-id sink to the runner so
            // `emit_execution_receipt` can publish declared/resolved ids
            // immediately after building the LaunchGraphBundle. If the
            // rest of the runner fails (spawn, healthcheck, etc.), the
            // partial-receipt boundary still observes the same ids the
            // success receipt would have carried.
            runner.receipt_graph_id_sink = Some(sink);
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
        launch_key: None,
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
        execution_id: None,
        execution_receipt_schema_version: None,
        declared_execution_id: None,
        resolved_execution_id: None,
        observed_execution_id: None,
        graph_completeness: None,
        reproducibility_class: None,
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
        installed_app_id: None,
        install_profile_id: None,
        install_profile_key: None,
        install_revision_id: None,
        capsule_instance_key: None,
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
        mut prepared,
        dep_contracts,
    } = prepare_session_execution(plan, raw_manifest)?;
    timer.finish_ok();

    #[cfg(unix)]
    {
        match crate::common::netd::ensure_egress_proxy() {
            Ok(egress_port) => {
                let proxy = crate::common::proxy::proxy_env_for_http_connect(egress_port, &[]);
                prepared
                    .launch_ctx
                    .extend_injected_env(crate::common::proxy::proxy_env_to_pairs(&proxy));
                prepared.launch_ctx.set_egress_proxy_port(egress_port);
                tracing::debug!(
                    egress_port,
                    "ato-netd egress proxy injected into session launch context"
                );
            }
            Err(crate::common::netd::EgressProxyError::NotSupported) => {}
            Err(err) => {
                return Err(anyhow::Error::from(err)
                    .context("failed to start ato-netd egress proxy for session"));
            }
        }
    }

    let session_web_port = if matches!(display_strategy, CapsuleDisplayStrategy::WebUrl) {
        if plan.execution_port().is_some() || runtime_overrides::override_port(None).is_some() {
            // Explicit port declared or ATO_UI_OVERRIDE_PORT set: use normal resolution
            // (which also handles remapping an occupied declared port to a free one).
            Some(resolve_session_web_port(
                resolution,
                manifest_path,
                plan,
                launch,
                &mut notes,
            )?)
        } else {
            // run command uses $PORT but no port declared: auto-assign a free port.
            let identity =
                session_port_identity(resolution, manifest_path, plan.selected_target_label());
            let port = PortManager::new()
                .context("failed to initialise PortManager for auto port assignment")?
                .resolve_port(&identity)
                .context(
                    "failed to auto-assign web port (run command uses $PORT but no port declared in capsule.toml)",
                )?;
            notes.push(format!(
                "Auto-assigned port {port} for web server (no port declared in capsule.toml)."
            ));
            Some(SessionWebPort {
                port,
                remapped_from: None,
            })
        }
    } else {
        None
    };
    let session_port_override = session_web_port
        .as_ref()
        .map(|web_port| runtime_overrides::scoped_override_port(web_port.port));

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
    let mut runtime_process = spawn_runtime_process(
        plan,
        &prepared,
        &display_strategy,
        &temp_log_path,
        session_web_port.as_ref().map(|web_port| web_port.port),
    )
    .with_context(|| {
        format!(
            "failed to start capsule session for {}",
            manifest_path.display()
        )
    })?;
    drop(session_port_override);
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

    let mut runtime = runtime_descriptor(plan);
    if let Some(web_port) = session_web_port.as_ref() {
        runtime.port = Some(web_port.port);
    }
    let local_url = if matches!(display_strategy, CapsuleDisplayStrategy::WebUrl) {
        let port = session_web_port
            .as_ref()
            .map(|web_port| web_port.port)
            .ok_or_else(|| {
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
                // Kill the entire process group so grandchildren (e.g. uvicorn spawned
                // by `sh -lc`) are reaped and don't orphan on the declared port.
                #[cfg(unix)]
                if let Some(pid) = runtime_process.child.id().checked_sub(0) {
                    let _ = unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
                }
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
        requested_port: session_web_port.as_ref().map(|web_port| web_port.port),
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
    let legacy_dependency_contracts = dependency_contracts_for_session_record(
        runtime_process.child.id() as i32,
        dep_contracts.as_ref(),
    );
    // PR-5a: emit provider→provider `depends_on` edges from the
    // live `RunningGraph.deps()[i].needs` so PR-5b's
    // `teardown_from_graph` has the reverse-topological ordering it
    // needs. When `dep_contracts` is `None`, the needs map is empty
    // and the populator behaves like the 1-arg form.
    let provider_needs = dep_contracts_provider_needs(dep_contracts.as_ref());
    let graph =
        crate::application::session_graph_populate::populate_graph_from_dependency_contracts_with_needs(
            legacy_dependency_contracts.as_ref(),
            provider_needs.as_ref(),
        );
    let dependency_contracts =
        crate::application::session_graph_populate::dependency_contracts_from_graph(
            graph.as_ref(),
            runtime_process.child.id() as i32,
        )
        .or(legacy_dependency_contracts);
    let session = StoredSessionInfo {
        session_id,
        launch_key: None,
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
        execution_id: None,
        execution_receipt_schema_version: None,
        declared_execution_id: None,
        resolved_execution_id: None,
        observed_execution_id: None,
        graph_completeness: None,
        reproducibility_class: None,
        // Single-target session (no `[services]`); orchestration_services
        // is populated only by start_orchestration_session_in_process.
        orchestration_services: None,
        // App Session Materialization: see note on the guest variant above.
        schema_version: None,
        launch_digest: None,
        process_start_time_unix_ms: None,
        installed_app_id: None,
        install_profile_id: None,
        install_profile_key: None,
        install_revision_id: None,
        capsule_instance_key: None,
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

    // Runtime observation v1 (#490): this point is reached only after the
    // workload spawned and passed its readiness gate (spawn/readiness failures
    // bail earlier), so capture the observed launch envelope from the realized
    // launch context. The session runner stamps it onto the execution receipt
    // (it holds the receipt's execution_id).
    let mut info = session_info_from_stored(session);
    info.set_observed_runtime(
        crate::application::runtime_observation::build_observed_runtime_evidence(
            plan,
            launch,
            &prepared.launch_ctx,
            session_web_port.as_ref().map(|web_port| web_port.port),
        ),
    );
    Ok(info)
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
        // Capture the install identity synchronously, on this (guard-bearing)
        // thread, and thread it explicitly via the prepared context — the async
        // launch-context resolution must not read the thread-local itself (#508).
        install_profile_key: current_install_profile_key(),
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

    #[cfg(unix)]
    {
        match crate::common::netd::ensure_egress_proxy() {
            Ok(egress_port) => {
                let proxy = crate::common::proxy::proxy_env_for_http_connect(egress_port, &[]);
                launch_ctx.extend_injected_env(crate::common::proxy::proxy_env_to_pairs(&proxy));
                launch_ctx.set_egress_proxy_port(egress_port);
                tracing::debug!(
                    egress_port,
                    "ato-netd egress proxy injected into orchestration session launch context"
                );
            }
            Err(crate::common::netd::EgressProxyError::NotSupported) => {}
            Err(err) => {
                return Err(anyhow::Error::from(err)
                    .context("failed to start ato-netd egress proxy for orchestration session"));
            }
        }
    }

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
        // Desktop sessions consume the leaf via their own WebView only, so
        // they want podman to pick a free host port instead of grabbing the
        // recipe's declared port. Without this, two recipes that both declare
        // `[services.main]` with the same port (e.g. Open WebUI + Excalidraw
        // both at 8080) collide on host:8080 and the second session's WebView
        // ends up rendering whichever container still owns the port (#289).
        publish_policy: crate::executors::orchestrator::PublishPolicy::EphemeralMainService,
    };

    let timer = PhaseStageTimer::start(HourglassPhase::Execute, "orchestration_start_until_ready");
    let oci_provider = runtime_handle
        .block_on(select_ready_runtime_oci_provider())
        .context("OCI provider not ready before session start")?;
    let detached = runtime_handle
        .block_on(
            crate::executors::orchestrator::execute_until_ready_and_detach(
                plan,
                &prepared,
                reporter.clone(),
                &launch_ctx,
                &options,
                None,
                oci_provider,
            ),
        )
        .context("orchestration services failed to start in-process")?;
    timer.finish_ok();

    // Step 3: leaf service URL — ServicePhaseCoordinator already ran the
    // per-service readiness probes, so the leaf is reachable. We bind the
    // WebView to the *actual* published host port from the detached snapshot
    // rather than the declared `leaf_port`. For OCI leaves these can differ
    // (and for two recipes that both declare 8080, e.g. Open WebUI and
    // Excalidraw, the declared port is not even unique). Local leaves don't
    // populate `host_ports`, so we fall back to the declared port — that
    // path's container_port and host_port are the same by construction.
    let leaf_snapshot = detached
        .services
        .iter()
        .find(|s| s.name == leaf_name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "orchestration leaf service '{}' missing from detached snapshot",
                leaf_name
            )
        })?;
    let leaf_host_port = leaf_snapshot
        .host_ports
        .get(&leaf_port)
        .copied()
        .unwrap_or(leaf_port);
    let local_url = format!("http://127.0.0.1:{leaf_host_port}/");

    notes.push(format!(
        "Orchestration mode: launched in-process; WebView bound to leaf service '{}' (target='{}', container_port={}, host_port={}).",
        leaf_name, leaf_target_label, leaf_port, leaf_host_port
    ));

    let runtime_descriptor = CapsuleRuntimeDescriptor {
        target_label: leaf_target_label.clone(),
        runtime: Some(leaf_runtime),
        driver: Some(leaf_driver.clone()),
        language: None,
        port: Some(leaf_host_port),
    };

    // Surface the leaf process to ProcessManager so `stop_session` can find
    // a recorded PID. OCI leaves do not surface a Unix PID; in that case we
    // fall back to the wrapper's own PID for session_id derivation, which
    // matches the legacy supervisor's behavior of using the spawned `ato run`
    // PID. PR-D wires the full materialized graph (including OCI container
    // ids) through SessionRecord.dependency_contracts.
    let leaf_local_pid = leaf_snapshot.local_pid.map(|pid| pid as i32).unwrap_or(0);

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
    let legacy_dependency_contracts =
        dependency_contracts_for_session_record(leaf_local_pid, dep_contracts.as_ref());
    // PR-5a: live provider needs + manifest service depends_on map
    // for teardown-order edges. See session_graph_populate.rs
    // EDGE_KIND_DEPENDS_ON / EDGE_KIND_USES docstrings.
    let provider_needs = dep_contracts_provider_needs(dep_contracts.as_ref());
    let manifest_service_deps = manifest_service_depends_on_map(&plan.manifest);
    let graph =
        crate::application::session_graph_populate::populate_graph_from_dependency_contracts_with_needs(
            legacy_dependency_contracts.as_ref(),
            provider_needs.as_ref(),
        );
    let orchestration_services = orchestration_services_for_session_record(
        std::process::id() as i32,
        &detached.services,
        detached.network_name.clone(),
        detached.ephemeral_volumes.clone(),
    );
    let graph =
        crate::application::session_graph_populate::append_orchestration_services_to_graph_with_deps(
            graph,
            orchestration_services.as_ref(),
            manifest_service_deps.as_ref(),
        );
    let dependency_contracts =
        crate::application::session_graph_populate::dependency_contracts_from_graph(
            graph.as_ref(),
            leaf_local_pid,
        )
        .or(legacy_dependency_contracts);
    let session = StoredSessionInfo {
        session_id: session_id.clone(),
        launch_key: None,
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
        execution_id: None,
        execution_receipt_schema_version: None,
        declared_execution_id: None,
        resolved_execution_id: None,
        observed_execution_id: None,
        graph_completeness: None,
        reproducibility_class: None,
        // [services] graph subset (#73 PR-D). Persisted so `stop_session`
        // (and the parent-death watcher from PR-B) can tear services down
        // after the wrapper process exits — the OS keeps the underlying
        // OCI containers / spawned children alive as orphans, but only
        // this record holds the container_ids / pids needed to stop them.
        orchestration_services,
        schema_version: None,
        launch_digest: None,
        process_start_time_unix_ms: None,
        installed_app_id: None,
        install_profile_id: None,
        install_profile_key: None,
        install_revision_id: None,
        capsule_instance_key: None,
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
        launch_key: None,
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
        execution_id: None,
        execution_receipt_schema_version: None,
        declared_execution_id: None,
        resolved_execution_id: None,
        observed_execution_id: None,
        graph_completeness: None,
        reproducibility_class: None,
        // Legacy supervisor path: the nested `ato run` child owns the
        // service lifecycle, so this wrapper has no DetachedServiceSnapshot
        // to persist. Reachable only via ATO_LEGACY_SUPERVISOR=1.
        orchestration_services: None,
        schema_version: None,
        launch_digest: None,
        process_start_time_unix_ms: None,
        installed_app_id: None,
        install_profile_id: None,
        install_profile_key: None,
        install_revision_id: None,
        capsule_instance_key: None,
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
/// runtime is `web` (front-end candidates), then prefer one with
/// `network.publish = true` (the public-facing service, auto-set for `main`
/// when it has a port), then fall back to the alphabetically last name.
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
            if let Some(published_leaf) = leaves.iter().find(|service| service.network.publish) {
                return Ok(*published_leaf);
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
///
/// `network_name` is the Docker/Podman network created by the OCI
/// orchestrator for this session, stored so `stop_session` can remove
/// it after all containers stop (closes #273).
fn orchestration_services_for_session_record(
    wrapper_pid: i32,
    snapshots: &[crate::executors::orchestrator::DetachedServiceSnapshot],
    network_name: Option<String>,
    ephemeral_volumes: Vec<String>,
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
        network_name,
        ephemeral_volumes,
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

/// PR-5a: extract per-provider `needs` (alias → needed-aliases) so
/// the session_graph populator can emit provider→provider
/// `depends_on` edges.
///
/// Today `RunningDep` does not carry `needs` (that field exists on
/// `TeardownTarget` but not on the live runtime dep). Until a
/// future commit threads needs through to `RunningDep`,
/// provider→provider `depends_on` edges remain empty and
/// PR-5b's `teardown_from_graph` will see no inter-provider
/// ordering. Provider teardown still works via the legacy
/// `teardown_reverse_topological` fallback because
/// `graph_complete_for_teardown` returns false when a multi-
/// provider session has no `depends_on` edges between them. Single-
/// provider sessions are unaffected.
///
/// This stub returns `None` unconditionally so the populator skips
/// emitting any provider depends_on edges. PR-6 demotion (or a
/// future PR-5c) will lift this once `RunningDep.needs` exists.
fn dep_contracts_provider_needs(
    _dep_contracts: Option<&DependencyContractGuard>,
) -> Option<std::collections::BTreeMap<String, Vec<String>>> {
    None
}

/// PR-5a: extract per-service `depends_on` (service-name →
/// dep-targets) from the raw manifest's `[services.<name>]` table.
/// Targets may be either sibling service names or provider aliases;
/// the populator classifies them at edge-emit time. Returns `None`
/// when the manifest has no `[services]` table or no depends_on
/// declarations.
fn manifest_service_depends_on_map(
    manifest_raw: &toml::Value,
) -> Option<std::collections::BTreeMap<String, Vec<String>>> {
    let services = manifest_raw.get("services")?.as_table()?;
    let mut out: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for (service_name, value) in services {
        let depends_on = value
            .get("depends_on")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|entry| entry.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        // Also fold in [services.<name>.dependencies.<alias>] keys
        // (service-to-provider).
        let mut targets = depends_on;
        if let Some(deps_table) = value.get("dependencies").and_then(|v| v.as_table()) {
            for alias in deps_table.keys() {
                if !targets.iter().any(|t| t == alias) {
                    targets.push(alias.clone());
                }
            }
        }
        if !targets.is_empty() {
            out.insert(service_name.clone(), targets);
        }
    }
    if out.is_empty() { None } else { Some(out) }
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
        execution_id: session.execution_id,
        execution_receipt_schema_version: session.execution_receipt_schema_version,
        declared_execution_id: session.declared_execution_id,
        resolved_execution_id: session.resolved_execution_id,
        observed_execution_id: session.observed_execution_id,
        graph_completeness: session.graph_completeness,
        reproducibility_class: session.reproducibility_class,
        // Observation evidence is attached post-spawn by start_runtime_session,
        // not derived from the stored record.
        observed_runtime: None,
    }
}

pub(super) fn resolve_session_launch_plan(
    handle: &str,
    target_label: Option<&str>,
    community_toml_path: Option<&Path>,
    attach_state: &[String],
) -> Result<(
    PathBuf,
    capsule_core::router::ManifestData,
    capsule_core::launch_spec::LaunchSpec,
    Vec<String>,
)> {
    if community_toml_path.is_none()
        && !super::resolve::input_is_existing_local_path(handle)
        && let Some(resolved) = super::sample_recipes::resolve_sample_recipe_for_input(handle)?
    {
        let manifest_path = resolved.manifest_path;
        let mut notes = vec![format!(
            "Resolved via bundled sample recipe '{}'.",
            resolved.slug
        )];
        let (plan, _guest, plan_notes) = resolve_local_plan_for_session_start(
            &manifest_path,
            target_label,
            Some(resolved.slug.as_str()),
            attach_state,
        )?;
        notes.extend(plan_notes);
        let launch = derive_launch_spec(&plan).with_context(|| {
            format!(
                "failed to derive launch spec for sample recipe '{}' at {}",
                resolved.slug,
                manifest_path.display()
            )
        })?;
        return Ok((manifest_path, plan, launch, notes));
    }

    if let Ok(CanonicalHandle::GithubRepo {
        ref owner,
        ref repo,
        ..
    }) = normalize_capsule_handle(handle)
        && community_toml_path.is_none()
        && let Some(resolved) =
            super::sample_recipes::resolve_sample_recipe_for_github(owner, repo)?
    {
        let manifest_path = resolved.manifest_path;
        let mut notes = vec![format!(
            "Resolved via bundled sample recipe '{}' for github.com/{}/{}.",
            resolved.slug, owner, repo
        )];
        let (plan, _guest, plan_notes) = resolve_local_plan_for_session_start(
            &manifest_path,
            target_label,
            Some(resolved.slug.as_str()),
            attach_state,
        )?;
        notes.extend(plan_notes);
        let launch = derive_launch_spec(&plan).with_context(|| {
            format!(
                "failed to derive launch spec for sample recipe '{}' at {}",
                resolved.slug,
                manifest_path.display()
            )
        })?;
        return Ok((manifest_path, plan, launch, notes));
    }

    if let Some(manifest_path) = community_toml_path {
        let manifest_path = manifest_path.to_path_buf();
        let (plan, _guest, notes) =
            resolve_local_plan_for_session_start(&manifest_path, target_label, None, attach_state)?;
        let launch = derive_launch_spec(&plan).with_context(|| {
            format!(
                "failed to derive launch spec for {}",
                manifest_path.display()
            )
        })?;
        return Ok((manifest_path, plan, launch, notes));
    }

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
                community_toml_path.map(|path| path.to_string_lossy().to_string()),
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

    let (plan, _guest, notes) =
        resolve_local_plan_for_session_start(&manifest_path, target_label, None, attach_state)?;
    let launch = derive_launch_spec(&plan).with_context(|| {
        format!(
            "failed to derive launch spec for {}",
            manifest_path.display()
        )
    })?;
    Ok((manifest_path, plan, launch, notes))
}

pub(super) fn resolve_local_plan_for_session_start(
    manifest_path: &Path,
    target_label: Option<&str>,
    sample_recipe_slug: Option<&str>,
    attach_state: &[String],
) -> Result<(
    capsule_core::router::ManifestData,
    Option<GuestContract>,
    Vec<String>,
)> {
    let raw = fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest = capsule_core::types::CapsuleManifest::from_toml(&raw)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    let state_source_overrides = if !attach_state.is_empty() {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        resolve_attach_state_source_overrides(&manifest, attach_state, &cwd)?
    } else if let Some(slug) = sample_recipe_slug {
        let bindings = auto_state_bindings_for_sample_recipe_manifest(manifest_path, slug)?;
        if !bindings.is_empty() {
            bindings
        } else {
            require_explicit_persistent_state_bindings(&manifest)?;
            std::collections::HashMap::new()
        }
    } else {
        require_explicit_persistent_state_bindings(&manifest)?;
        std::collections::HashMap::new()
    };

    let (plan, guest, mut notes) = resolve_local_plan_with_state_overrides(
        manifest_path,
        target_label,
        state_source_overrides,
    )?;
    if let Some(slug) = sample_recipe_slug
        && !plan.state_source_overrides.is_empty()
        && attach_state.is_empty()
    {
        notes.push(format!(
            "Auto-bound persistent sample recipe state under ~/.ato/state/sample-recipes/{slug}."
        ));
    }
    Ok((plan, guest, notes))
}

fn auto_state_bindings_for_sample_recipe_manifest(
    manifest_path: &Path,
    slug: &str,
) -> Result<std::collections::HashMap<String, String>> {
    use capsule_core::types::StateDurability;

    let raw = fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest = capsule_core::types::CapsuleManifest::from_toml(&raw)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    let state_root = capsule_core::common::paths::ato_state_dir()
        .join("sample-recipes")
        .join(slug);
    let mut bindings = std::collections::HashMap::new();
    for (state_name, _requirement) in manifest
        .state
        .iter()
        .filter(|(_, requirement)| requirement.durability == StateDurability::Persistent)
    {
        let path = state_root.join(state_name);
        fs::create_dir_all(&path).with_context(|| {
            format!(
                "failed to create sample recipe state directory {}",
                path.display()
            )
        })?;
        bindings.insert(
            state_name.clone(),
            path.canonicalize()
                .unwrap_or(path)
                .to_string_lossy()
                .to_string(),
        );
    }
    Ok(bindings)
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
        // Synchronous capture on the guard-bearing thread; threaded explicitly
        // so the async launch-context resolution never reads the thread-local (#508).
        install_profile_key: current_install_profile_key(),
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
    selected_web_port: Option<u16>,
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
                child: crate::executors::node_compat::spawn_with_selected_port(
                    plan,
                    None,
                    &prepared.execution_plan,
                    &prepared.launch_ctx,
                    false,
                    selected_web_port,
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

    // A run command that uses $PORT / ${PORT} implies this is a web server even
    // when no explicit `port` is declared in capsule.toml.  We will auto-assign
    // a free port for it below.
    if run_command_uses_port_var(plan) {
        return CapsuleDisplayStrategy::WebUrl;
    }

    // A run command invoking a known WSGI/ASGI server (uvicorn, gunicorn, flask,
    // etc.) — either as `uvicorn ...` or `python -m uvicorn ...` — is a web app
    // even when neither `port` nor `$PORT` appears in capsule.toml.  Ato will
    // auto-assign a port and inject `--port <N>` at spawn time.
    if run_command_is_known_web_server(plan) {
        return CapsuleDisplayStrategy::WebUrl;
    }

    CapsuleDisplayStrategy::TerminalStream
}

/// Returns `true` when the target's run command contains `$PORT` or `${PORT}`,
/// indicating the server binds to the system-injected port variable even when
/// `port` is not declared explicitly in `capsule.toml`.
fn run_command_uses_port_var(plan: &capsule_core::router::ManifestData) -> bool {
    plan.execution_run_command()
        .map(|cmd| cmd.contains("$PORT") || cmd.contains("${PORT}"))
        .unwrap_or(false)
}

/// Returns `true` when the run command invokes a known Python WSGI/ASGI web
/// server — either directly (`uvicorn app:app`) or via the module flag
/// (`python -m uvicorn app:app`).  These servers need an explicit `--port`
/// argument; ato injects one at spawn time when none is declared.
fn run_command_is_known_web_server(plan: &capsule_core::router::ManifestData) -> bool {
    const KNOWN_SERVERS: &[&str] = &[
        "uvicorn",
        "gunicorn",
        "flask",
        "streamlit",
        "hypercorn",
        "daphne",
    ];
    let Some(cmd) = plan.execution_run_command() else {
        return false;
    };
    let lower = cmd.trim().to_ascii_lowercase();
    // Direct invocation: starts with the server name
    if KNOWN_SERVERS.iter().any(|s| lower.starts_with(s)) {
        return true;
    }
    // Module invocation: `python -m uvicorn ...` or `python3 -m gunicorn ...`
    if KNOWN_SERVERS
        .iter()
        .any(|s| lower.contains(&format!("-m {s}")))
    {
        return true;
    }
    false
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
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(_) => {
            return None;
        }
    };
    serde_json::from_slice::<StoredSessionInfo>(&bytes).ok()
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
    let graph_snapshot;
    let snapshot = if let Some(snapshot) = record.orchestration_services.as_ref() {
        snapshot
    } else {
        graph_snapshot = orchestration_services_from_graph(record);
        let Some(snapshot) = graph_snapshot.as_ref() else {
            return Ok(false);
        };
        snapshot
    };
    if snapshot.services.is_empty() {
        return Ok(false);
    }

    // PR-5b-fix: per-service teardown is delegated to the
    // `orchestration_teardown` primitive so behavior is identical
    // whether the legacy iteration here or the graph-driven
    // `teardown_from_graph` driver invokes it. `force=true` collapses
    // to `Duration::ZERO`, matching the legacy "immediate SIGKILL"
    // semantics.
    let grace = if force {
        Duration::from_secs(0)
    } else {
        Duration::from_secs(10)
    };

    let mut any_stopped = false;
    // Reverse-topological: services were started by ServicePhaseCoordinator
    // in topological order, so reverse iteration is the correct teardown
    // order (consumers before providers). The per-service primitive
    // implements pgroup kill, descendant walk, lsof fallback, and OCI
    // stop/remove (PR-5b-fix).
    for service in snapshot.services.iter().rev() {
        match crate::application::orchestration_teardown::stop_orchestration_service_record(
            service, grace,
        ) {
            Ok(true) => any_stopped = true,
            Ok(false) => {}
            Err(err) => {
                eprintln!(
                    "ATO-WARN failed to stop orchestration service '{}': {}",
                    service.name, err
                );
            }
        }
    }
    Ok(any_stopped)
}

fn orchestration_services_from_graph(
    record: &StoredSessionInfo,
) -> Option<StoredOrchestrationServices> {
    let graph = record.graph.as_ref()?;
    let mut services = graph
        .nodes
        .iter()
        .filter(|node| node.kind == NODE_KIND_SERVICE)
        .map(|node| {
            let host_ports = node
                .metadata
                .get("host_ports")
                .map(|encoded| parse_graph_host_ports(encoded))
                .unwrap_or_default();
            let published_port = node
                .metadata
                .get("published_port")
                .and_then(|value| value.parse::<u16>().ok())
                .or(node.port);
            let order = node
                .metadata
                .get("order")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(usize::MAX);
            (
                order,
                StoredOrchestrationService {
                    name: node.identifier.clone(),
                    target_label: node
                        .metadata
                        .get("target_label")
                        .cloned()
                        .unwrap_or_else(|| node.identifier.clone()),
                    local_pid: node.pid,
                    container_id: node.container_id.clone(),
                    host_ports,
                    published_port,
                },
            )
        })
        .collect::<Vec<_>>();
    if services.is_empty() {
        return None;
    }
    services.sort_by_key(|(order, service)| (*order, service.name.clone()));
    Some(StoredOrchestrationServices {
        wrapper_pid: record.pid,
        services: services.into_iter().map(|(_, service)| service).collect(),
        network_name: None,
        ephemeral_volumes: Vec::new(),
    })
}

fn parse_graph_host_ports(encoded: &str) -> BTreeMap<u16, u16> {
    encoded
        .split(',')
        .filter_map(|pair| {
            let (host, container) = pair.split_once(':')?;
            Some((host.parse::<u16>().ok()?, container.parse::<u16>().ok()?))
        })
        .collect()
}

// PR-5b-fix: `collect_descendant_pids`, `kill_listeners_on_published_port`,
// and `listener_pids_on_port` moved to
// `crate::application::orchestration_teardown` so the graph-driven
// teardown driver can reuse them. The legacy
// `stop_recorded_orchestration_services` (above) delegates per-service
// to that primitive.

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
        graph_aliases_sorted, contract_aliases_sorted,
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

    // PR-5b: prefer graph-driven teardown when the persisted graph is
    // complete (every provider has pid+state_dir; service node count
    // matches; etc — see `graph_complete_for_teardown`). Incomplete
    // graphs (older session records, partial teardown info) fall
    // through to the legacy two-path teardown below.
    let used_graph_teardown = if let Some(record) = session_record.as_ref() {
        if crate::application::session_graph_populate::graph_complete_for_teardown(record) {
            if let Some(graph) = record.graph.as_ref() {
                match crate::application::dependency_runtime::teardown::teardown_from_graph(
                    graph,
                    Duration::from_secs(0),
                ) {
                    Ok(()) => {
                        stopped = true;
                        let _ = process_manager.delete_pid(session_id);
                        true
                    }
                    Err(err) => {
                        if stop_error.is_none() {
                            stop_error = Some(anyhow::Error::new(err));
                        }
                        false
                    }
                }
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
    };

    if !used_graph_teardown {
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
    }
    if let Some(err) = stop_error
        && !stopped
    {
        return Err(err);
    }

    // Cleanup of OCI residue (containers → ephemeral volumes → network).
    //
    // Container teardown happened above. These steps are intentionally **not**
    // gated on `stopped`: a session whose containers already exited reports
    // `stopped == false`, but its `ato-*` network and ephemeral volumes can
    // still be orphaned. Both removals are idempotent (not-found counts as
    // success) and guarded by the `ato-` / `ato-state-` name prefixes, so
    // running them unconditionally is safe and closes the leak where stop found
    // nothing live to kill (#450). Persistent volumes are not listed on the
    // record, so durable state survives stop (#444).
    let orchestration_services = session_record
        .as_ref()
        .and_then(|r| r.orchestration_services.as_ref());

    // Ephemeral engine-managed state volumes first — they are mounted by the
    // (now-removed) containers and are independent of the network. Failures are
    // logged, never fatal: a leaked ephemeral volume is less harmful than a
    // session that refuses to stop.
    if let Some(volumes) = orchestration_services.map(|s| s.ephemeral_volumes.as_slice()) {
        use crate::application::orchestration_teardown::{
            VolumeRemovalOutcome, remove_volume_if_present,
        };
        for volume in volumes {
            if let VolumeRemovalOutcome::Failed(err) = remove_volume_if_present(volume) {
                eprintln!(
                    "ATO-WARN ephemeral volume cleanup failed for {volume}: {err}; \
                     run `podman volume rm {volume}` to clean up manually"
                );
            }
        }
    }

    // Then the session-scoped network, once nothing is attached (#273, #450).
    let network_cleanup_warning: Option<String> = if let Some(network_name) =
        orchestration_services.and_then(|s| s.network_name.as_deref())
    {
        use crate::application::orchestration_teardown::{
            NetworkRemovalOutcome, remove_network_if_present,
        };
        match remove_network_if_present(network_name) {
            NetworkRemovalOutcome::Removed | NetworkRemovalOutcome::AlreadyGone => None,
            NetworkRemovalOutcome::SkippedNotAtoManaged => None,
            NetworkRemovalOutcome::Failed(err) => Some(format!(
                "network cleanup failed for {network_name}: {err}; \
                     run `podman network rm {network_name}` to clean up manually"
            )),
        }
    } else {
        None
    };

    if stopped && session_path.exists() {
        fs::remove_file(&session_path)
            .with_context(|| format!("failed to remove session file {}", session_path.display()))?;
        crate::common::netd::try_shutdown_if_last_session();
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
                network_cleanup_warning: network_cleanup_warning.clone(),
            })?
        );
        return Ok(());
    }

    if stopped {
        println!("Stopped session: {session_id}");
    } else {
        println!("Session was not active: {session_id}");
    }
    if let Some(warn) = &network_cleanup_warning {
        eprintln!("ATO-WARN {warn}");
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
    // Forward ATO_HOME so the watcher subprocess resolves session files
    // from the same root as the caller.
    if let Ok(ato_home) = std::env::var("ATO_HOME") {
        command.env("ATO_HOME", ato_home);
    }
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

/// Strip `HANDLE_FLAG_INHERIT` from this process's standard output and error
/// handles so that subsequently spawned children do not inherit them.
///
/// On Windows, `std`/`tokio` spawn children with `bInheritHandles = TRUE`, and
/// the stdio handles handed to *this* process are inheritable. When
/// `ato app session start` is itself launched with piped stdout/stderr — which
/// is exactly how ato-desktop launches it, via `Command::output()` — any
/// long-lived child that inherits copies of those pipe write-ends holds them
/// open for its own lifetime. The desktop's `output()` then never observes EOF
/// and blocks forever, even though session start has already exited with the
/// envelope written (#377). The worst offender is the `<engine> logs --follow`
/// streamer the OCI orchestrator spawns to mirror container output: it runs
/// until the container stops, long outliving session start. The parent-death
/// watcher is a second such child.
///
/// Clearing the inherit flag once, before orchestration spawns anything, stops
/// every child from capturing our stdio. The session-start envelope is still
/// printed afterwards through this process's own handles, which is unaffected;
/// service output is mirrored to our stderr at the Rust layer, not by child
/// inheritance, so nothing downstream needs to inherit these handles. POSIX is
/// immune because `std` marks the `output()` pipe fds `CLOEXEC`.
#[cfg(windows)]
fn clear_std_handle_inheritance() {
    use windows_sys::Win32::Foundation::{
        GetLastError, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
    };
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};

    for std_id in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: both are plain Win32 calls operating only on this process's
        // own standard handles; we skip null/invalid handles.
        unsafe {
            let handle = GetStdHandle(std_id);
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                // No usable handle for this stream (e.g. detached). Nothing to
                // detach from children — not an error.
                continue;
            }
            if SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) == 0 {
                // The clear failed on a real handle, so a long-lived child may
                // still inherit and pin this stream's pipe, re-introducing the
                // desktop `output()` hang (#377). Surface it instead of silently
                // proceeding — the launch may appear to stall downstream.
                tracing::warn!(
                    std_id,
                    last_error = GetLastError(),
                    "failed to clear HANDLE_FLAG_INHERIT on a standard handle; a \
                     long-lived child could pin the parent's stdout/stderr pipe (#377)"
                );
            }
        }
    }
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
    // If this process was started via `ato launch`, stamp the install lifecycle
    // IDs onto every session record so dashboards and replay tools can correlate
    // sessions with installed app instances.
    let has_ctx = with_install_lifecycle_context(|c| c.is_some());
    if has_ctx {
        let mut patched = session.clone();
        apply_install_lifecycle(&mut patched);
        return write_session_record_atomic(root, &patched);
    }
    write_session_record_atomic(root, session)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SessionWebPort {
    port: u16,
    remapped_from: Option<u16>,
}

fn resolve_session_web_port(
    resolution: &super::resolve::HandleResolution,
    manifest_path: &Path,
    plan: &capsule_core::router::ManifestData,
    launch: &capsule_core::launch_spec::LaunchSpec,
    notes: &mut Vec<String>,
) -> Result<SessionWebPort> {
    let requested_port = runtime_overrides::override_port(launch.port).ok_or_else(|| {
        anyhow::anyhow!(
            "runtime=web target '{}' requires targets.<label>.port",
            plan.selected_target_label()
        )
    })?;
    let identity = session_port_identity(resolution, manifest_path, plan.selected_target_label());
    choose_session_web_port(requested_port, &identity, notes, |identity| {
        PortManager::new()?.resolve_port(identity)
    })
}

fn choose_session_web_port<F>(
    requested_port: u16,
    identity: &str,
    notes: &mut Vec<String>,
    allocate_fallback: F,
) -> Result<SessionWebPort>
where
    F: FnOnce(&str) -> Result<u16>,
{
    if local_port_is_available(requested_port) {
        return Ok(SessionWebPort {
            port: requested_port,
            remapped_from: None,
        });
    }

    let fallback_port = allocate_fallback(identity).with_context(|| {
        format!(
            "declared web port {requested_port} is already in use and no alternate session port could be allocated"
        )
    })?;
    if fallback_port == requested_port || !local_port_is_available(fallback_port) {
        anyhow::bail!(
            "declared web port {requested_port} is already in use and alternate session port {fallback_port} is unavailable"
        );
    }

    eprintln!(
        "ATO-WARN declared web port {requested_port} is already in use; remapping this session to {fallback_port}"
    );
    notes.push(format!(
        "Declared web port {requested_port} was already in use; remapped this session to {fallback_port}."
    ));

    Ok(SessionWebPort {
        port: fallback_port,
        remapped_from: Some(requested_port),
    })
}

fn session_port_identity(
    resolution: &super::resolve::HandleResolution,
    manifest_path: &Path,
    target_label: &str,
) -> String {
    let base = resolution
        .canonical_handle
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            (!resolution.normalized_handle.trim().is_empty())
                .then_some(resolution.normalized_handle.as_str())
        })
        .map(str::to_string)
        .unwrap_or_else(|| manifest_path.to_string_lossy().to_string());

    if target_label.is_empty() || target_label == "default" {
        format!("session:{base}")
    } else {
        format!("session:{base}:{target_label}")
    }
}

fn local_port_is_available(port: u16) -> bool {
    #[cfg(unix)]
    if listener_pids_on_port(port)
        .map(|pids| !pids.is_empty())
        .unwrap_or(false)
    {
        return false;
    }

    TcpListener::bind(("127.0.0.1", port))
        .map(|listener| {
            drop(listener);
            true
        })
        .unwrap_or(false)
}

fn reserve_port(default_port: Option<u16>) -> Result<u16> {
    if let Some(port) = default_port
        && local_port_is_available(port)
    {
        return Ok(port);
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
    let mut last_rejected_listener: Option<String> = None;
    loop {
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("process exited before readiness with status {status}");
        }

        if http_get_ok(port, path) {
            match http_listener_belongs_to_child_tree(port, child.id()) {
                Ok(true) => return Ok(()),
                Ok(false) => {
                    last_rejected_listener = Some(format!(
                        "http://127.0.0.1:{port}{path} responded, but the listener is not owned by the session process tree rooted at pid {}",
                        child.id()
                    ));
                }
                Err(err) => {
                    last_rejected_listener = Some(format!(
                        "http://127.0.0.1:{port}{path} responded, but listener ownership could not be verified: {err}"
                    ));
                }
            }
        }

        if std::time::Instant::now() >= deadline {
            if let Some(reason) = last_rejected_listener {
                anyhow::bail!("readiness timed out for http://127.0.0.1:{port}{path}; {reason}");
            }
            anyhow::bail!("readiness timed out for http://127.0.0.1:{port}{path}");
        }

        std::thread::sleep(SESSION_READY_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn http_listener_belongs_to_child_tree(port: u16, root_pid: u32) -> Result<bool> {
    let listener_pids = listener_pids_on_port(port)?;
    if listener_pids.is_empty() {
        return Ok(false);
    }

    let mut allowed_pids = collect_descendant_pids(root_pid, "readiness");
    allowed_pids.push(root_pid);

    Ok(listener_pids.iter().all(|pid| allowed_pids.contains(pid)))
}

#[cfg(not(unix))]
fn http_listener_belongs_to_child_tree(_port: u16, _root_pid: u32) -> Result<bool> {
    Ok(true)
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

    /// Env guard for tests that mutate `HOME`/`ATO_HOME`/`ATO_DESKTOP_SESSION_ROOT`.
    /// Always take `crate::tests::env_lock()` BEFORE constructing this guard so that
    /// concurrent tests cannot observe the intermediate state.
    struct TestEnvGuard {
        ato_home: Option<String>,
        home: Option<String>,
        session_root: Option<String>,
    }

    impl TestEnvGuard {
        fn capture_and_set(
            ato_home_path: &std::path::Path,
            session_root: &std::path::Path,
        ) -> Self {
            let guard = Self {
                ato_home: std::env::var("ATO_HOME").ok(),
                home: std::env::var("HOME").ok(),
                session_root: std::env::var("ATO_DESKTOP_SESSION_ROOT").ok(),
            };
            unsafe {
                std::env::set_var("ATO_HOME", ato_home_path);
            }
            unsafe {
                std::env::set_var("HOME", ato_home_path);
            }
            unsafe {
                std::env::set_var("ATO_DESKTOP_SESSION_ROOT", session_root);
            }
            guard
        }
    }

    impl Drop for TestEnvGuard {
        fn drop(&mut self) {
            match &self.ato_home {
                Some(v) => unsafe { std::env::set_var("ATO_HOME", v) },
                None => unsafe { std::env::remove_var("ATO_HOME") },
            }
            match &self.home {
                Some(v) => unsafe { std::env::set_var("HOME", v) },
                None => unsafe { std::env::remove_var("HOME") },
            }
            match &self.session_root {
                Some(v) => unsafe { std::env::set_var("ATO_DESKTOP_SESSION_ROOT", v) },
                None => unsafe { std::env::remove_var("ATO_DESKTOP_SESSION_ROOT") },
            }
        }
    }

    #[test]
    fn reserve_port_returns_requested_port_when_available() {
        let port = reserve_port(Some(43291)).expect("reserve port");
        assert_eq!(port, 43291);
    }

    #[test]
    fn choose_session_web_port_remaps_occupied_declared_port() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind occupied port");
        let occupied_port = listener.local_addr().expect("listener addr").port();
        let mut notes = Vec::new();

        let selected = choose_session_web_port(occupied_port, "test/session", &mut notes, |_| {
            reserve_port(None)
        })
        .expect("choose alternate port");

        assert_ne!(selected.port, occupied_port);
        assert_eq!(selected.remapped_from, Some(occupied_port));
        assert!(
            notes
                .iter()
                .any(|note| note.contains(&format!("remapped this session to {}", selected.port)))
        );
    }

    #[cfg(unix)]
    #[test]
    fn http_listener_ownership_rejects_unrelated_process() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind occupied port");
        let port = listener.local_addr().expect("listener addr").port();
        let mut child = Command::new("sleep").arg("5").spawn().expect("spawn sleep");

        let belongs = http_listener_belongs_to_child_tree(port, child.id())
            .expect("check listener ownership");

        let _ = child.kill();
        let _ = child.wait();
        assert!(!belongs);
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
                declared_execution_id: None,
                resolved_execution_id: None,
                observed_execution_id: None,
                graph_completeness: None,
                reproducibility_class: None,
                observed_runtime: None,
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
            launch_key: None,
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
            execution_id: None,
            execution_receipt_schema_version: None,
            declared_execution_id: None,
            resolved_execution_id: None,
            observed_execution_id: None,
            graph_completeness: None,
            reproducibility_class: None,
            orchestration_services: None,
            schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
            launch_digest: Some("d".repeat(64)),
            process_start_time_unix_ms: None,
            installed_app_id: None,
            install_profile_id: None,
            install_profile_key: None,
            install_revision_id: None,
            capsule_instance_key: None,
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
            launch_key: None,
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
                        pid: None,
                        state_dir: None,
                        port: None,
                        container_id: None,
                        capability: None,
                        metadata: std::collections::BTreeMap::new(),
                    },
                    ato_session_core::StoredGraphNode {
                        kind: NODE_KIND_PROVIDER.to_string(),
                        identifier: "db".to_string(),
                        pid: None,
                        state_dir: None,
                        port: None,
                        container_id: None,
                        capability: None,
                        metadata: std::collections::BTreeMap::new(),
                    },
                ],
                edges: vec![
                    ato_session_core::StoredGraphEdge {
                        source: "cache".to_string(),
                        target: "output://cache".to_string(),
                        kind: EDGE_KIND_PROVIDES.to_string(),
                        metadata: std::collections::BTreeMap::new(),
                    },
                    ato_session_core::StoredGraphEdge {
                        source: "db".to_string(),
                        target: "output://db".to_string(),
                        kind: EDGE_KIND_PROVIDES.to_string(),
                        metadata: std::collections::BTreeMap::new(),
                    },
                ],
            }),
            execution_id: None,
            execution_receipt_schema_version: None,
            declared_execution_id: None,
            resolved_execution_id: None,
            observed_execution_id: None,
            graph_completeness: None,
            reproducibility_class: None,
            orchestration_services: None,
            schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
            launch_digest: Some("digest".repeat(8)),
            process_start_time_unix_ms: None,
            installed_app_id: None,
            install_profile_id: None,
            install_profile_key: None,
            install_revision_id: None,
            capsule_instance_key: None,
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
            launch_key: None,
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
            execution_id: None,
            execution_receipt_schema_version: None,
            declared_execution_id: None,
            resolved_execution_id: None,
            observed_execution_id: None,
            graph_completeness: None,
            reproducibility_class: None,
            orchestration_services: None,
            schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
            launch_digest: Some("digest".repeat(8)),
            process_start_time_unix_ms: None,
            installed_app_id: None,
            install_profile_id: None,
            install_profile_key: None,
            install_revision_id: None,
            capsule_instance_key: None,
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
            launch_key: None,
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
                    pid: None,
                    state_dir: None,
                    port: None,
                    container_id: None,
                    capability: None,
                    metadata: std::collections::BTreeMap::new(),
                }],
                edges: vec![ato_session_core::StoredGraphEdge {
                    source: "db".to_string(),
                    target: "output://db".to_string(),
                    kind: EDGE_KIND_PROVIDES.to_string(),
                    metadata: std::collections::BTreeMap::new(),
                }],
            }),
            execution_id: None,
            execution_receipt_schema_version: None,
            declared_execution_id: None,
            resolved_execution_id: None,
            observed_execution_id: None,
            graph_completeness: None,
            reproducibility_class: None,
            orchestration_services: None,
            schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
            launch_digest: Some("digest".repeat(8)),
            process_start_time_unix_ms: None,
            installed_app_id: None,
            install_profile_id: None,
            install_profile_key: None,
            install_revision_id: None,
            capsule_instance_key: None,
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
            launch_key: None,
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
            execution_id: None,
            execution_receipt_schema_version: None,
            declared_execution_id: None,
            resolved_execution_id: None,
            observed_execution_id: None,
            graph_completeness: None,
            reproducibility_class: None,
            orchestration_services: None,
            schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
            launch_digest: Some("digest".repeat(8)),
            process_start_time_unix_ms: None,
            installed_app_id: None,
            install_profile_id: None,
            install_profile_key: None,
            install_revision_id: None,
            capsule_instance_key: None,
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

    #[test]
    #[serial]
    fn sample_recipe_session_start_auto_binds_explicit_state() {
        let _env_lock = crate::tests::env_lock().lock().expect("env lock");
        let temp = tempfile::tempdir().expect("tempdir");
        let session_root = temp.path().join("sessions");
        fs::create_dir_all(&session_root).expect("create session root");
        let _guard = TestEnvGuard::capture_and_set(temp.path(), &session_root);

        let resolved = crate::app_control::sample_recipes::resolve_sample_recipe_for_input("memos")
            .expect("resolve sample recipe")
            .expect("memos recipe");
        let (plan, _guest, notes) = resolve_local_plan_for_session_start(
            &resolved.manifest_path,
            None,
            Some(resolved.slug.as_str()),
            &[],
        )
        .expect("resolve session plan");

        let state_path = plan
            .state_source_overrides
            .get("data")
            .expect("data state override");
        assert!(
            state_path.ends_with("/state/sample-recipes/memos/data"),
            "unexpected state path: {state_path}"
        );
        assert!(std::path::Path::new(state_path).exists());
        assert!(notes.iter().any(|note| note.contains("Auto-bound")));

        let services = plan
            .resolve_services()
            .expect("explicit state should be bound");
        let main = services.service("main").expect("main service");
        assert!(
            main.runtime
                .runtime()
                .mounts
                .iter()
                .any(|mount| mount.source == *state_path && mount.target == "/var/opt/memos")
        );
    }

    #[test]
    #[serial]
    fn session_start_applies_attach_state_to_explicit_persistent_state() {
        let _env_lock = crate::tests::env_lock().lock().expect("env lock");
        let temp = tempfile::tempdir().expect("tempdir");
        let session_root = temp.path().join("sessions");
        fs::create_dir_all(&session_root).expect("create session root");
        let _guard = TestEnvGuard::capture_and_set(temp.path(), &session_root);

        let resolved = crate::app_control::sample_recipes::resolve_sample_recipe_for_input("memos")
            .expect("resolve sample recipe")
            .expect("memos recipe");
        let state_dir = temp.path().join("memos-data");
        let attach_state = vec![format!("data:{}", state_dir.display())];
        let (plan, _guest, _notes) = resolve_local_plan_for_session_start(
            &resolved.manifest_path,
            None,
            None,
            &attach_state,
        )
        .expect("resolve session plan with attached state");

        let state_path = plan
            .state_source_overrides
            .get("data")
            .expect("data state override");
        assert_eq!(
            state_path,
            &state_dir
                .canonicalize()
                .expect("state dir created")
                .display()
                .to_string()
        );
        let services = plan.resolve_services().expect("resolve services");
        let main = services.service("main").expect("main service");
        assert!(
            main.runtime
                .runtime()
                .mounts
                .iter()
                .any(|mount| mount.source == *state_path && mount.target == "/var/opt/memos")
        );
    }

    #[test]
    #[serial]
    fn community_toml_launch_auto_binds_explicit_persistent_state() {
        let _env_lock = crate::tests::env_lock().lock().expect("env lock");
        let temp = tempfile::tempdir().expect("tempdir");
        let session_root = temp.path().join("sessions");
        fs::create_dir_all(&session_root).expect("create session root");
        let _guard = TestEnvGuard::capture_and_set(temp.path(), &session_root);

        let resolved =
            crate::app_control::sample_recipes::resolve_sample_recipe_for_input("blinko")
                .expect("resolve sample recipe")
                .expect("blinko recipe");
        let attach_state = auto_attach_state_args_for_community_toml_manifest(
            &resolved.manifest_path,
            "ctoml_test",
        )
        .expect("auto community state bindings");

        assert!(
            attach_state
                .iter()
                .any(|binding| binding.starts_with("db-data:"))
        );
        assert!(
            attach_state
                .iter()
                .any(|binding| binding.starts_with("app-data:"))
        );

        let (plan, _guest, _notes) = resolve_local_plan_for_session_start(
            &resolved.manifest_path,
            None,
            None,
            &attach_state,
        )
        .expect("resolve session plan with community auto-bound state");

        let state_path = PathBuf::from(
            plan.state_source_overrides
                .get("db-data")
                .expect("db-data state override"),
        );
        assert!(state_path.ends_with("state/community-tomls/ctoml_test/db-data"));
        assert!(state_path.is_dir());
    }

    #[test]
    fn community_toml_launch_skips_auto_bindings_without_persistent_state() {
        let resolved = crate::app_control::sample_recipes::resolve_sample_recipe_for_input("pgweb")
            .expect("resolve sample recipe")
            .expect("pgweb recipe");
        let attach_state = auto_attach_state_args_for_community_toml_manifest(
            &resolved.manifest_path,
            "ctoml_test",
        )
        .expect("auto community state bindings");
        assert!(attach_state.is_empty());
    }

    #[test]
    #[serial]
    fn session_start_reports_early_error_for_missing_attach_state() {
        let _env_lock = crate::tests::env_lock().lock().expect("env lock");
        let temp = tempfile::tempdir().expect("tempdir");
        let session_root = temp.path().join("sessions");
        fs::create_dir_all(&session_root).expect("create session root");
        let _guard = TestEnvGuard::capture_and_set(temp.path(), &session_root);

        let resolved = crate::app_control::sample_recipes::resolve_sample_recipe_for_input("memos")
            .expect("resolve sample recipe")
            .expect("memos recipe");
        let err = resolve_local_plan_for_session_start(&resolved.manifest_path, None, None, &[])
            .expect_err("missing attach-state should fail");
        assert!(err.to_string().contains("state 'data' requires"));
        assert!(
            err.to_string()
                .contains("--attach-state data:/path/to/data")
        );
    }

    #[test]
    fn session_start_allows_no_persistent_state_without_attach_state() {
        let resolved = crate::app_control::sample_recipes::resolve_sample_recipe_for_input("pgweb")
            .expect("resolve sample recipe")
            .expect("pgweb recipe");
        let (plan, _guest, _notes) =
            resolve_local_plan_for_session_start(&resolved.manifest_path, None, None, &[])
                .expect("pgweb should not require attach-state");
        assert!(plan.state_source_overrides.is_empty());
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn stop_session_uses_record_dependency_contracts_when_sidecar_is_missing() {
        let _env_lock = crate::tests::env_lock().lock().expect("env lock");
        let temp = tempfile::tempdir().expect("tempdir");
        let session_root = temp.path().join("sessions");
        fs::create_dir_all(&session_root).expect("create session root");
        let _guard = TestEnvGuard::capture_and_set(temp.path(), &session_root);

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
                launch_key: None,
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
                execution_id: None,
                execution_receipt_schema_version: None,
                declared_execution_id: None,
                resolved_execution_id: None,
                observed_execution_id: None,
                graph_completeness: None,
                reproducibility_class: None,
                orchestration_services: None,
                schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
                launch_digest: Some("digest".repeat(8)),
                process_start_time_unix_ms: None,
                installed_app_id: None,
                install_profile_id: None,
                install_profile_key: None,
                install_revision_id: None,
                capsule_instance_key: None,
            },
        )
        .expect("write session record");

        stop_session(&session_id, true).expect("stop session");

        // Allow signal delivery and process cleanup before polling.
        std::thread::sleep(std::time::Duration::from_millis(150));

        assert!(consumer.try_wait().expect("consumer wait").is_some());
        assert!(provider.try_wait().expect("provider wait").is_some());
        assert!(!session_root.join(format!("{}.json", session_id)).exists());
        assert!(
            ProcessManager::new()
                .expect("process manager after stop")
                .read_dependency_session_snapshot(&session_id)
                .expect("read dependency session after stop")
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    #[serial]
    fn stop_session_uses_record_dependency_contracts_when_pid_file_is_missing() {
        let _env_lock = crate::tests::env_lock().lock().expect("env lock");
        let temp = tempfile::tempdir().expect("tempdir");
        let session_root = temp.path().join("sessions");
        fs::create_dir_all(&session_root).expect("create session root");
        let _guard = TestEnvGuard::capture_and_set(temp.path(), &session_root);

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
                launch_key: None,
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
                execution_id: None,
                execution_receipt_schema_version: None,
                declared_execution_id: None,
                resolved_execution_id: None,
                observed_execution_id: None,
                graph_completeness: None,
                reproducibility_class: None,
                orchestration_services: None,
                schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
                launch_digest: Some("digest".repeat(8)),
                process_start_time_unix_ms: None,
                installed_app_id: None,
                install_profile_id: None,
                install_profile_key: None,
                install_revision_id: None,
                capsule_instance_key: None,
            },
        )
        .expect("write session record");

        stop_session(&session_id, true).expect("stop session");

        // Allow signal delivery and process cleanup before polling.
        std::thread::sleep(std::time::Duration::from_millis(150));

        assert!(provider.try_wait().expect("provider wait").is_some());
        assert!(!session_root.join(format!("{}.json", session_id)).exists());
        assert!(
            ProcessManager::new()
                .expect("process manager after stop")
                .read_dependency_session_snapshot(&session_id)
                .expect("read dependency session after stop")
                .is_none()
        );
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
            launch_key: None,
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
            execution_id: None,
            execution_receipt_schema_version: None,
            declared_execution_id: None,
            resolved_execution_id: None,
            observed_execution_id: None,
            graph_completeness: None,
            reproducibility_class: None,
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
                network_name: None,
                ephemeral_volumes: Vec::new(),
            }),
            schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
            launch_digest: Some("digest".repeat(8)),
            process_start_time_unix_ms: None,
            installed_app_id: None,
            install_profile_id: None,
            install_profile_key: None,
            install_revision_id: None,
            capsule_instance_key: None,
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
    #[cfg(unix)]
    #[test]
    #[serial]
    fn stop_session_kills_orchestration_services_when_recorded_pid_is_dead() {
        use std::collections::BTreeMap;

        let _env_lock = crate::tests::env_lock().lock().expect("env lock");
        let temp = tempfile::tempdir().expect("tempdir");
        let session_root = temp.path().join("sessions");
        fs::create_dir_all(&session_root).expect("create session root");
        let _guard = TestEnvGuard::capture_and_set(temp.path(), &session_root);

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
                launch_key: None,
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
                execution_id: None,
                execution_receipt_schema_version: None,
                declared_execution_id: None,
                resolved_execution_id: None,
                observed_execution_id: None,
                graph_completeness: None,
                reproducibility_class: None,
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
                    network_name: None,
                    ephemeral_volumes: Vec::new(),
                }),
                schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
                launch_digest: Some("digest".repeat(8)),
                process_start_time_unix_ms: None,
                installed_app_id: None,
                install_profile_id: None,
                install_profile_key: None,
                install_revision_id: None,
                capsule_instance_key: None,
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
            launch_key: None,
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
            execution_id: None,
            execution_receipt_schema_version: None,
            declared_execution_id: None,
            resolved_execution_id: None,
            observed_execution_id: None,
            graph_completeness: None,
            reproducibility_class: None,
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
                network_name: None,
                ephemeral_volumes: Vec::new(),
            }),
            schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
            launch_digest: Some("digest".repeat(8)),
            process_start_time_unix_ms: None,
            installed_app_id: None,
            install_profile_id: None,
            install_profile_key: None,
            install_revision_id: None,
            capsule_instance_key: None,
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

    /// PR-5b-fix parity test: the graph-driven teardown path
    /// (`teardown_from_graph` → `stop_orchestration_service_record`)
    /// must do the same `published_port` lsof listener cleanup that
    /// the legacy `stop_recorded_orchestration_services` does — see
    /// the sibling test above. Same setup (dead recorded pid + live
    /// orphan listener); the only difference is which teardown entry
    /// point we call. If they diverge, the graph path is unsafe.
    #[cfg(unix)]
    #[test]
    #[ignore = "depends on lsof + python3 cold-start; flaky under loaded `cargo test`"]
    fn teardown_from_graph_kills_orphan_listener_via_published_port_parity_with_legacy() {
        use std::collections::BTreeMap;
        use std::net::TcpListener;

        let listener =
            TcpListener::bind("127.0.0.1:0").expect("bind workload listener for graph parity test");
        let port = listener.local_addr().expect("listener addr").port();
        drop(listener);

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

        let dead_recorded_pid: i32 = 999_999_999;

        // Build a single-service graph carrying the dead recorded pid
        // + the live `published_port`. No depends_on/uses edges
        // needed (single service → no ordering ambiguity).
        let mut metadata = BTreeMap::new();
        metadata.insert("target_label".to_string(), "web".to_string());
        metadata.insert("published_port".to_string(), port.to_string());

        let service_node = ato_session_core::StoredGraphNode {
            kind: NODE_KIND_SERVICE.to_string(),
            identifier: "web".to_string(),
            pid: Some(dead_recorded_pid),
            state_dir: None,
            port: Some(port),
            container_id: None,
            capability: None,
            metadata,
        };
        let graph = ato_session_core::StoredExecutionGraph {
            schema_version: ato_session_core::StoredExecutionGraph::SCHEMA_VERSION,
            nodes: vec![service_node],
            edges: vec![],
        };

        crate::application::dependency_runtime::teardown::teardown_from_graph(
            &graph,
            Duration::from_secs(0),
        )
        .expect("graph teardown");

        // Same exit poll as the legacy parity test.
        let mut workload = workload;
        for _ in 0..40 {
            if workload.try_wait().expect("workload wait").is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = workload.kill();
        panic!("orphan workload (pid {workload_pid}) was not killed within 1s by graph teardown");
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
            launch_key: None,
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
            execution_id: None,
            execution_receipt_schema_version: None,
            declared_execution_id: None,
            resolved_execution_id: None,
            observed_execution_id: None,
            graph_completeness: None,
            reproducibility_class: None,
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
                network_name: None,
                ephemeral_volumes: Vec::new(),
            }),
            schema_version: Some(ato_session_core::SCHEMA_VERSION_V2),
            launch_digest: Some("digest".repeat(8)),
            process_start_time_unix_ms: None,
            installed_app_id: None,
            install_profile_id: None,
            install_profile_key: None,
            install_revision_id: None,
            capsule_instance_key: None,
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
    // ── CIK backfill tests ──────────────────────────────────────────────────

    fn minimal_session_record_with_exec(exec_id: Option<&str>) -> StoredSessionInfo {
        // Build the minimal required JSON and deserialize — avoids constructing
        // StoredSessionInfo via struct literal when the schema has many required fields.
        let exec_field = match exec_id {
            Some(id) => format!(r#","execution_id":"{id}""#),
            None => String::new(),
        };
        let json = format!(
            r#"{{
            "session_id":"test_session",
            "handle":"test/handle",
            "normalized_handle":"test/handle",
            "trust_state":"trusted",
            "restricted":false,
            "runtime":{{"target_label":"main","runtime":null,"driver":null,"language":null,"port":null}},
            "display_strategy":"guest_webview",
            "pid":0,
            "log_path":"",
            "manifest_path":"",
            "target_label":"",
            "notes":[],
            "guest":null,"web":null,"terminal":null,"service":null
            {exec_field}
        }}"#
        );
        serde_json::from_str(&json).expect("failed to build minimal session record")
    }

    /// `apply_install_lifecycle` should stamp all 5 lifecycle fields and derive
    /// `capsule_instance_key` from the session's `execution_id` when the
    /// thread-local context is active.
    #[test]
    fn apply_install_lifecycle_stamps_all_fields_and_derives_cik() {
        use capsule_core::foundation::install_lifecycle::{
            ExecutionId, InstallProfileKey, InstallRevisionId, derive_capsule_instance_key,
        };

        let ipk_str = "ipk_aabbccdd1122334455667788aabbccdd";
        let rev_str = "rev_aabbccdd1122334455667788aabbccdd";
        let exec_id_str = "exec_deadbeef1234567890abcdef0123456789ab";

        let ctx = crate::cli::commands::run::InstallLifecycleContext {
            installed_app_id: "app_aabbccdd1122334455667788aabbccdd".to_string(),
            install_profile_id: "default".to_string(),
            install_profile_key: ipk_str.to_string(),
            install_revision_id: rev_str.to_string(),
        };

        // Build a session record that already has execution_id set (simulating
        // the case where the receipt assigned it before write_session_record).
        let mut record = minimal_session_record_with_exec(Some(exec_id_str));

        // Apply under scoped guard.
        let _guard = ScopedInstallLifecycleGuard::set(ctx.clone());
        apply_install_lifecycle(&mut record);

        // All 4 identity fields must be stamped.
        assert_eq!(
            record.installed_app_id.as_deref(),
            Some(ctx.installed_app_id.as_str())
        );
        assert_eq!(
            record.install_profile_id.as_deref(),
            Some(ctx.install_profile_id.as_str())
        );
        assert_eq!(
            record.install_profile_key.as_deref(),
            Some(ctx.install_profile_key.as_str())
        );
        assert_eq!(
            record.install_revision_id.as_deref(),
            Some(ctx.install_revision_id.as_str())
        );

        // CIK must be derived from (profile_key, revision_id, execution_id).
        let expected_cik = derive_capsule_instance_key(
            &InstallProfileKey::new(ipk_str.to_string()),
            &InstallRevisionId::new(rev_str.to_string()),
            &ExecutionId::new(exec_id_str.to_string()),
        );
        assert_eq!(
            record.capsule_instance_key.as_deref(),
            Some(expected_cik.as_str()),
            "capsule_instance_key must be derived from the session execution_id"
        );
    }

    /// When `execution_id` is absent at write time, `capsule_instance_key` stays
    /// `None` (it will be back-filled when the receipt assigns the execution_id).
    #[test]
    fn apply_install_lifecycle_leaves_cik_none_when_no_execution_id() {
        let ctx = crate::cli::commands::run::InstallLifecycleContext {
            installed_app_id: "app_001".to_string(),
            install_profile_id: "default".to_string(),
            install_profile_key: "ipk_001".to_string(),
            install_revision_id: "rev_001".to_string(),
        };

        let mut record = minimal_session_record_with_exec(None);

        let _guard = ScopedInstallLifecycleGuard::set(ctx);
        apply_install_lifecycle(&mut record);

        assert!(
            record.capsule_instance_key.is_none(),
            "CIK should be None when execution_id has not been assigned yet"
        );
    }

    /// The install lifecycle context is thread-local: a context set on the
    /// calling thread (via `ato launch`) must NOT be visible on any other
    /// thread. This guards against the cross-launch / cross-thread record
    /// cross-stamping that a process-global slot would allow.
    #[test]
    fn install_lifecycle_context_is_not_visible_to_other_threads() {
        let ctx = crate::cli::commands::run::InstallLifecycleContext {
            installed_app_id: "app_worker".to_string(),
            install_profile_id: "default".to_string(),
            install_profile_key: "ipk_worker".to_string(),
            install_revision_id: "rev_worker".to_string(),
        };

        let _guard = ScopedInstallLifecycleGuard::set(ctx);
        let other_thread_saw_context =
            std::thread::spawn(|| with_install_lifecycle_context(|ctx| ctx.is_some()))
                .join()
                .expect("worker thread joins");

        assert!(
            !other_thread_saw_context,
            "thread-local lifecycle context must not leak to other threads; \
             cross-thread stamping must use the typed materializer request"
        );
    }

    /// Setting the lifecycle context must NOT mutate any `ATO_INSTALL_LIFECYCLE_*`
    /// process environment variable. Process env is inherited by guest / child
    /// processes and is externally settable, so control-plane metadata must never
    /// travel through it.
    ///
    /// `#[serial]` because this test observes process env, which is global; it
    /// must not overlap with any other test that mutates the same keys (see
    /// `dependency_materializer::tests::stamp_install_lifecycle_ignores_process_env_when_request_has_no_context`).
    #[test]
    #[serial_test::serial(ato_install_lifecycle_env)]
    fn set_install_lifecycle_context_does_not_touch_process_env() {
        const ENV_KEYS: &[&str] = &[
            "ATO_INSTALL_LIFECYCLE_APP_ID",
            "ATO_INSTALL_LIFECYCLE_PROFILE_ID",
            "ATO_INSTALL_LIFECYCLE_PROFILE_KEY",
            "ATO_INSTALL_LIFECYCLE_REVISION_ID",
        ];

        // Snapshot whatever the env happens to be before the call so the
        // assertion checks "unchanged", not "absent" — robust regardless of
        // the ambient environment.
        let before: Vec<Option<std::ffi::OsString>> =
            ENV_KEYS.iter().map(|key| std::env::var_os(key)).collect();

        let ctx = crate::cli::commands::run::InstallLifecycleContext {
            installed_app_id: "app_env".to_string(),
            install_profile_id: "default".to_string(),
            install_profile_key: "ipk_env".to_string(),
            install_revision_id: "rev_env".to_string(),
        };

        {
            let _guard = ScopedInstallLifecycleGuard::set(ctx);
            for (key, prior) in ENV_KEYS.iter().zip(&before) {
                assert_eq!(
                    std::env::var_os(key).as_ref(),
                    prior.as_ref(),
                    "lifecycle context must not mutate process env var {key}"
                );
            }
        }
        // Dropping the guard must likewise leave process env untouched.
        for (key, prior) in ENV_KEYS.iter().zip(&before) {
            assert_eq!(
                std::env::var_os(key).as_ref(),
                prior.as_ref(),
                "clearing lifecycle context must not mutate process env var {key}"
            );
        }
    }

    /// Verify `ScopedInstallLifecycleGuard` clears the context after it is dropped.
    #[test]
    fn scoped_guard_clears_context_on_drop() {
        let ctx = crate::cli::commands::run::InstallLifecycleContext {
            installed_app_id: "app_002".to_string(),
            install_profile_id: "default".to_string(),
            install_profile_key: "ipk_002".to_string(),
            install_revision_id: "rev_002".to_string(),
        };

        {
            let _guard = ScopedInstallLifecycleGuard::set(ctx);
            // Context is active inside this block.
            let has_ctx = with_install_lifecycle_context(|c| c.is_some());
            assert!(has_ctx, "context should be active after set");
        }
        // After drop, context must be cleared.
        let has_ctx = with_install_lifecycle_context(|c| c.is_some());
        assert!(
            !has_ctx,
            "ScopedInstallLifecycleGuard must clear context on drop"
        );
    }

    // ── pick_orchestration_leaf_service ───────────────────────────────────────

    fn make_leaf_service(
        name: &str,
        publish: bool,
        port: Option<u16>,
        depends_on: Vec<String>,
    ) -> capsule_core::foundation::types::ResolvedService {
        use capsule_core::foundation::types::{
            ResolvedService, ResolvedServiceNetwork, ResolvedServiceRuntime, ResolvedTargetRuntime,
        };
        ResolvedService {
            name: name.to_string(),
            depends_on,
            connections: vec![],
            readiness_probe: None,
            network: ResolvedServiceNetwork {
                aliases: vec![name.to_string()],
                publish,
                allow_from: vec![],
                egress_proxy: true,
            },
            run_once: false,
            runtime: ResolvedServiceRuntime::Oci(ResolvedTargetRuntime {
                target: name.to_string(),
                runtime: "oci".to_string(),
                driver: None,
                runtime_version: None,
                image: Some(format!("{}:latest", name)),
                entrypoint: String::new(),
                run_command: None,
                cmd: vec![],
                env: Default::default(),
                working_dir: None,
                source_layout: None,
                port,
                required_env: vec![],
                mounts: vec![],
                user: None,
            }),
        }
    }

    /// Dify-shaped plan: `main` (published, port 3000) and `worker`
    /// (unpublished, port 5001) are both leaves (neither is depended on).
    /// Should pick `main`, not `worker` (alphabetically last).
    #[test]
    fn pick_leaf_prefers_published_over_alphabetically_last_for_dify_shape() {
        use capsule_core::foundation::types::OrchestrationPlan;

        let db = make_leaf_service("db", false, Some(5432), vec![]);
        let redis = make_leaf_service("redis", false, Some(6379), vec![]);
        let weaviate = make_leaf_service("weaviate", false, Some(8080), vec![]);
        let api = make_leaf_service(
            "api",
            false,
            Some(5001),
            vec![
                "db".to_string(),
                "redis".to_string(),
                "weaviate".to_string(),
            ],
        );
        let worker = make_leaf_service(
            "worker",
            false,
            Some(5001),
            vec!["db".to_string(), "redis".to_string()],
        );
        // main: published (auto-set by resolve_services for `main` with a port)
        let main = make_leaf_service("main", true, Some(3000), vec!["api".to_string()]);

        let plan = OrchestrationPlan {
            startup_order: vec![
                "db".to_string(),
                "redis".to_string(),
                "weaviate".to_string(),
                "api".to_string(),
                "worker".to_string(),
                "main".to_string(),
            ],
            services: vec![db, redis, weaviate, api, worker, main],
        };

        let leaf = pick_orchestration_leaf_service(&plan).expect("leaf service");
        assert_eq!(
            leaf.name, "main",
            "published leaf `main` (port 3000) should be preferred over unpublished `worker` (port 5001)"
        );
    }

    /// node/web runtime still wins over `network.publish`.
    #[test]
    fn pick_leaf_node_driver_beats_published_oci() {
        use capsule_core::foundation::types::{
            OrchestrationPlan, ResolvedService, ResolvedServiceNetwork, ResolvedServiceRuntime,
            ResolvedTargetRuntime,
        };

        let api = make_leaf_service("api", false, Some(5001), vec![]);
        // published OCI leaf
        let main_oci = make_leaf_service("main", true, Some(3000), vec![]);
        // node-driver leaf (unpublished)
        let frontend = ResolvedService {
            name: "frontend".to_string(),
            depends_on: vec![],
            connections: vec![],
            readiness_probe: None,
            network: ResolvedServiceNetwork {
                aliases: vec!["frontend".to_string()],
                publish: false,
                allow_from: vec![],
                egress_proxy: true,
            },
            run_once: false,
            runtime: ResolvedServiceRuntime::Managed(ResolvedTargetRuntime {
                target: "frontend".to_string(),
                runtime: "web".to_string(),
                driver: Some("node".to_string()),
                runtime_version: None,
                image: None,
                entrypoint: "src/index.js".to_string(),
                run_command: None,
                cmd: vec![],
                env: Default::default(),
                working_dir: None,
                source_layout: None,
                port: Some(8080),
                required_env: vec![],
                mounts: vec![],
                user: None,
            }),
        };

        let plan = OrchestrationPlan {
            startup_order: vec![
                "api".to_string(),
                "main".to_string(),
                "frontend".to_string(),
            ],
            services: vec![api, main_oci, frontend],
        };

        let leaf = pick_orchestration_leaf_service(&plan).expect("leaf service");
        assert_eq!(
            leaf.name, "frontend",
            "node-driver leaf should be preferred over published OCI leaf"
        );
    }

    /// Single leaf — trivial case is unchanged.
    #[test]
    fn pick_leaf_single_leaf_is_returned_directly() {
        use capsule_core::foundation::types::OrchestrationPlan;

        let db = make_leaf_service("db", false, Some(5432), vec![]);
        let main = make_leaf_service("main", true, Some(3000), vec!["db".to_string()]);

        let plan = OrchestrationPlan {
            startup_order: vec!["db".to_string(), "main".to_string()],
            services: vec![db, main],
        };

        let leaf = pick_orchestration_leaf_service(&plan).expect("leaf service");
        assert_eq!(leaf.name, "main");
    }
}
