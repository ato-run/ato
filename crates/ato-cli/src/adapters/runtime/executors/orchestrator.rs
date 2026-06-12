use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::Child;
use std::sync::{
    Arc, Mutex as StdMutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{Receiver, TryRecvError},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::sync::Mutex;

use capsule_core::CapsuleReporter;
use capsule_core::common::readiness::http_status_indicates_ready;
use capsule_core::execution_plan::guard::ExecutorKind;
use capsule_core::lifecycle::LifecycleEvent;
use capsule_core::router::ManifestData;
use capsule_core::runtime::oci::{
    BollardOciRuntimeClient, OciContainerRequest, OciLogChunk, OciMountSourceKind,
    OciNetworkRequest, OciPortSpec, OciRuntimeClient, resolve_oci_mount,
};
use capsule_core::types::{
    OrchestrationPlan, ReadinessProbe, ResolvedService, ResolvedServiceRuntime,
};

use super::launch_context::RuntimeLaunchContext;
use super::oci_multi_service::{
    OCI_EXIT_LOG_TAIL_LINES, OciExitedBeforeReadyError, collect_log_tail_from_rx,
};
use super::source::ExecuteMode;
use super::target_runner::{self, TargetLaunchOptions};
use crate::application::pipeline::cleanup::{CleanupScope, PipelineAttemptContext};
use crate::application::pipeline::phases::run::PreparedRunContext;
use crate::application::services::{
    ServiceGraphPlan, ServicePhaseCoordinator, ServicePhaseRuntime,
};
use crate::reporters::CliReporter;
use crate::runtime::overrides as runtime_overrides;

const READINESS_INTERVAL: Duration = Duration::from_millis(250);
const SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(200);
const OCI_STOP_TIMEOUT_SECS: i64 = 5;
const RUN_ONCE_TIMEOUT_SECS_DEFAULT: u64 = 300;

fn run_once_timeout() -> Duration {
    Duration::from_secs(
        std::env::var("ATO_OCI_RUN_ONCE_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(RUN_ONCE_TIMEOUT_SECS_DEFAULT),
    )
}

/// Process-wide flag set by entry points that reserve stdout
/// exclusively for a structured envelope (today: `ato app session
/// start --json`, which the desktop spawns and parses as a
/// `SessionStartEnvelope`). When set, the orchestration stream
/// pumpers route service stdout to the parent's **stderr** with the
/// usual `[<service>] ` prefix instead of stdout, so the envelope
/// JSON is the only thing on the parent's stdout.
///
/// `ato run` foreground use is unaffected — it never sets this flag,
/// so service output continues to render on the user's terminal as
/// before. The flag is process-local and never read by external code.
static REDIRECT_SERVICE_STDOUT_TO_STDERR: AtomicBool = AtomicBool::new(false);

/// Engage the stdout redirection described on
/// [`REDIRECT_SERVICE_STDOUT_TO_STDERR`]. Idempotent; safe to call
/// from any thread before orchestration setup begins. Has no effect
/// once the pumper threads have already been spawned for a given
/// session.
pub fn redirect_service_stdout_to_stderr_for_envelope_mode(active: bool) {
    REDIRECT_SERVICE_STDOUT_TO_STDERR.store(active, Ordering::Relaxed);
}

/// Public read of the envelope-mode flag. Used by sibling code paths
/// (e.g. the orchestration reporter constructor) that have to take
/// the same routing decision as `spawn_prefixed_stream` /
/// `print_prefixed_chunk` to keep stdout pure for the envelope JSON.
pub fn redirect_service_stdout_to_stderr_for_envelope_mode_active() -> bool {
    service_stdout_should_route_to_stderr()
}

fn service_stdout_should_route_to_stderr() -> bool {
    REDIRECT_SERVICE_STDOUT_TO_STDERR.load(Ordering::Relaxed)
}

/// Caller-driven policy for how the `services.main` leaf publishes its port
/// to the host. Policy is selected by the *caller context*, not by the
/// recipe or app — recipes still declare a port and `network.publish`, but
/// the caller decides whether the historical "main → fixed host port"
/// special case applies for this run.
///
/// Historically the orchestrator special-cases `service.name == "main"` to
/// `PublishMode::Fixed` so `ato run` exposes the recipe's declared port to
/// the host for CLI users and external tools. That model breaks any caller
/// that owns the only consumer and runs multiple sessions concurrently:
/// two recipes both declaring `[services.main]` with `port = 8080` (e.g.
/// Open WebUI and Excalidraw) compete for host:8080, and the second
/// session's caller ends up pointed at the first session's still-running
/// container.
///
/// `EphemeralMainService` opts the leaf out of the fixed-port special case
/// so the OCI runtime assigns a free host port per session. Callers that
/// pick this policy are responsible for reading the resolved host port
/// back from the runtime (e.g. `DetachedServiceSnapshot.host_ports`) when
/// they construct any user-facing URL.
///
/// For *non-main* services, `network.publish = true` is an explicit
/// recipe-level request for a stable host port and wins over this policy
/// (e.g. a sidecar API a recipe wants externally addressable). For the
/// `services.main` leaf, this policy decides exclusively — the router
/// auto-sets `network.publish = true` for every main with a port, so it
/// cannot be used as a per-recipe escape hatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PublishPolicy {
    /// `ato run` / CLI default. `services.main` publishes to the declared
    /// host port so external consumers (shells, browsers, scripts) can
    /// reach the recipe on a stable address.
    #[default]
    ExternalDefault,
    /// `services.main` publishes to an ephemeral host port chosen by the
    /// OCI runtime. Eliminates the cross-session host-port collision
    /// (#289) for callers that own the only consumer and may run multiple
    /// sessions concurrently. Currently used by the Desktop session path;
    /// the name is caller-agnostic so any future caller with the same
    /// shape (single consumer, concurrent sessions) can opt in.
    EphemeralMainService,
}

#[derive(Debug, Clone)]
pub struct OrchestratorOptions {
    pub enforcement: String,
    pub sandbox_mode: bool,
    pub dangerously_skip_permissions: bool,
    pub assume_yes: bool,
    pub nacelle: Option<PathBuf>,
    pub publish_policy: PublishPolicy,
}

impl OrchestratorOptions {
    fn target_launch_options(&self) -> TargetLaunchOptions {
        TargetLaunchOptions {
            enforcement: self.enforcement.clone(),
            sandbox_mode: self.sandbox_mode,
            dangerously_skip_permissions: self.dangerously_skip_permissions,
            assume_yes: self.assume_yes,
            preview_mode: false,
            defer_consent: false,
        }
    }
}

pub async fn execute(
    plan: &ManifestData,
    prepared: &PreparedRunContext,
    reporter: Arc<CliReporter>,
    launch_ctx: &RuntimeLaunchContext,
    options: OrchestratorOptions,
    attempt: Option<&mut PipelineAttemptContext>,
) -> Result<i32> {
    let client = BollardOciRuntimeClient::connect_default()
        .context("Failed to connect to OCI engine via Docker-compatible API")?;
    execute_with_client(
        plan, prepared, reporter, launch_ctx, &options, attempt, client,
    )
    .await
}

/// Public summary of a running orchestration service, sufficient for the
/// session layer to record provider lifecycle without holding the internal
/// `RunningService` directly. PR-D consumes this when populating
/// `SessionRecord.dependency_contracts` for orchestration capsules.
#[derive(Debug, Clone)]
#[allow(dead_code)] // PR-C: shape is fixed; PR-D consumes the fields.
pub struct DetachedServiceSnapshot {
    pub name: String,
    pub target_label: String,
    pub local_pid: Option<u32>,
    pub container_id: Option<String>,
    pub host_ports: HashMap<u16, u16>,
    pub published_port: Option<u16>,
}

/// Handle returned by `execute_until_ready_and_detach`. The session layer
/// reads `services` to build its `SessionRecord` view of the materialized
/// graph subset, then `mem::forget`s the handle so the underlying
/// `RunningService` values (and their `Child`/log threads/event channels)
/// outlive the call. PR-D replaces the `mem::forget` with explicit
/// ownership transfer to a session-scoped owner registered into
/// `ProcessManager`.
#[allow(dead_code)] // PR-C: shape is fixed; PR-D consumes the fields.
pub struct DetachedOrchestrationServices {
    pub services: Vec<DetachedServiceSnapshot>,
    pub network_name: Option<String>,
    /// Ephemeral engine-managed volumes created this session (Windows + Podman).
    /// Persisted on the session record so `stop_session` can delete them;
    /// persistent volumes are intentionally excluded so they survive stop. #444
    pub ephemeral_volumes: Vec<String>,
    /// Held privately so the caller cannot accidentally drop one provider
    /// while keeping the others. Both `mem::forget(handle)` (the v0.5.0
    /// PR-C pattern) and the future PR-D `BackgroundSessionOwner` consume
    /// the whole map together.
    inner: HashMap<String, RunningService>,
}

pub async fn execute_with_client<C>(
    plan: &ManifestData,
    prepared: &PreparedRunContext,
    reporter: Arc<CliReporter>,
    launch_ctx: &RuntimeLaunchContext,
    options: &OrchestratorOptions,
    attempt: Option<&mut PipelineAttemptContext>,
    client: C,
) -> Result<i32>
where
    C: OciRuntimeClient + Clone + Send + Sync + 'static,
{
    let (mut running, orchestration, network_name, client) = start_until_ready_with_client(
        plan,
        prepared,
        reporter.clone(),
        launch_ctx,
        options,
        attempt,
        client,
    )
    .await?;

    let exit_code = monitor_until_exit(
        &orchestration,
        &mut running,
        client.as_ref(),
        network_name.as_deref(),
    )
    .await?;
    Ok(exit_code)
}

/// Detach variant of `execute_with_client`: starts the orchestration graph,
/// awaits readiness, and returns a `DetachedOrchestrationServices` handle
/// instead of blocking in `monitor_until_exit`.
///
/// The caller (currently `start_orchestration_session_in_process`) keeps the
/// handle alive through the session lifetime — typically by `mem::forget`
/// after persisting the snapshot. PR-D replaces the `mem::forget` with a
/// session-scoped `BackgroundSessionOwner` that owns the handle and is
/// dropped from `stop_session`.
pub async fn execute_until_ready_and_detach<C>(
    plan: &ManifestData,
    prepared: &PreparedRunContext,
    reporter: Arc<CliReporter>,
    launch_ctx: &RuntimeLaunchContext,
    options: &OrchestratorOptions,
    attempt: Option<&mut PipelineAttemptContext>,
    client: C,
) -> Result<DetachedOrchestrationServices>
where
    C: OciRuntimeClient + Clone + Send + Sync + 'static,
{
    let (running, _orchestration, network_name, _client) = start_until_ready_with_client(
        plan, prepared, reporter, launch_ctx, options, attempt, client,
    )
    .await?;

    let services: Vec<DetachedServiceSnapshot> = running
        .iter()
        .map(|(name, rs)| build_detached_snapshot(name, rs))
        .collect();

    // Aggregate ephemeral engine volumes across all services so the session
    // record can drive their removal on stop. See #444.
    let ephemeral_volumes: Vec<String> = running
        .values()
        .filter_map(|rs| match &rs.handle {
            RunningHandle::Oci(oci) => Some(oci.ephemeral_volumes.iter().cloned()),
            RunningHandle::Local(_) => None,
        })
        .flatten()
        .collect();

    Ok(DetachedOrchestrationServices {
        services,
        network_name,
        ephemeral_volumes,
        inner: running,
    })
}

/// Shared start-up path used by both the foreground and detach modes.
///
/// Returns `(running_services, orchestration_plan, network_name, client_arc)`
/// so the foreground caller can immediately enter `monitor_until_exit` and
/// the detach caller can build a public snapshot. The mode is observable
/// only at the public call sites (`execute_with_client`, `execute_until_ready_and_detach`),
/// which differ in what they do *after* this returns; this shared startup
/// path itself behaves identically in both modes, so the mode is not
/// threaded as a parameter.
async fn start_until_ready_with_client<C>(
    plan: &ManifestData,
    prepared: &PreparedRunContext,
    reporter: Arc<CliReporter>,
    launch_ctx: &RuntimeLaunchContext,
    options: &OrchestratorOptions,
    attempt: Option<&mut PipelineAttemptContext>,
    client: C,
) -> Result<(
    HashMap<String, RunningService>,
    OrchestrationPlan,
    Option<String>,
    Arc<C>,
)>
where
    C: OciRuntimeClient + Clone + Send + Sync + 'static,
{
    let orchestration = plan.resolve_services()?;
    // Build the layered start order from the resolved orchestration so that
    // target-level `depends_on` (merged into ResolvedService.depends_on by the
    // router) and any cross-service edges materialized as `connections` are
    // both reflected. `from_services` would only see the raw [services.*]
    // table, which is empty for recipes like AFFiNE that declare depends_on
    // on targets — leaving sibling leaves (e.g. redis vs db for migration)
    // in a single layer with alphabetic start order that races the
    // connection-resolution check. See AODD PR #262.
    let graph = ServiceGraphPlan::from_orchestration(&orchestration)?;
    let session_id = session_id(plan);
    let client = Arc::new(client);
    // Detect the engine once so the mount strategy (#444) is consistent across
    // all services in this session.
    let is_podman = client.is_podman().await;
    let network_name = if orchestration
        .services
        .iter()
        .any(|service| service.runtime.is_oci())
    {
        Some(network_name(plan))
    } else {
        None
    };

    if let Some(network_name) = network_name.as_ref() {
        client
            .create_network(&OciNetworkRequest {
                name: network_name.clone(),
                labels: session_labels(plan, &session_id),
            })
            .await
            .with_context(|| format!("failed to create OCI network '{network_name}'"))?;
    }

    let runtime = OrchestratorStartupRuntime::new(
        plan.clone(),
        prepared.clone(),
        orchestration.clone(),
        reporter.clone(),
        launch_ctx.clone(),
        options.clone(),
        Arc::clone(&client),
        session_id,
        network_name.clone(),
        is_podman,
        attempt.map(|attempt| attempt.cleanup_scope()),
    );

    if let Err(err) = ServicePhaseCoordinator::new(&graph)
        .run(runtime.clone())
        .await
    {
        let mut running = runtime.into_running().await;
        shutdown_all(
            &orchestration,
            &mut running,
            client.as_ref(),
            network_name.as_deref(),
        )
        .await;
        return Err(err);
    }

    runtime.commit_startup_cleanup();
    let running = runtime.into_running().await;

    notify_main_endpoint(&orchestration, &running, &reporter).await?;

    Ok((running, orchestration, network_name, client))
}

fn build_detached_snapshot(name: &str, service: &RunningService) -> DetachedServiceSnapshot {
    let (container_id, host_ports) = match &service.handle {
        RunningHandle::Local(_) => (None, HashMap::new()),
        RunningHandle::Oci(oci) => (Some(oci.container_id.clone()), oci.host_ports.clone()),
    };
    let target_label = match &service.service.runtime {
        ResolvedServiceRuntime::Oci(rt) => rt.target.clone(),
        ResolvedServiceRuntime::Managed(rt) => rt.target.clone(),
    };
    let published_port = match &service.service.runtime {
        ResolvedServiceRuntime::Oci(rt) => rt.port,
        ResolvedServiceRuntime::Managed(rt) => rt.port,
    };
    DetachedServiceSnapshot {
        name: name.to_string(),
        target_label,
        local_pid: service.local_pid(),
        container_id,
        host_ports,
        published_port,
    }
}

struct RunningService {
    service: ResolvedService,
    env: HashMap<String, String>,
    handle: RunningHandle,
}

impl RunningService {
    fn local_pid(&self) -> Option<u32> {
        match &self.handle {
            RunningHandle::Local(local) => Some(local.child.id()),
            RunningHandle::Oci(_) => None,
        }
    }
}

enum RunningHandle {
    Local(RunningLocalService),
    Oci(RunningOciService),
}

struct RunningLocalService {
    child: Child,
    stdout_thread: Option<JoinHandle<std::io::Result<()>>>,
    stderr_thread: Option<JoinHandle<std::io::Result<()>>>,
    cleanup_paths: Vec<PathBuf>,
    exit_task: Option<tokio::task::JoinHandle<Result<i32>>>,
    event_rx: Option<Receiver<LifecycleEvent>>,
    readiness_state: LocalReadinessState,
}

struct RunningOciService {
    container_id: String,
    log_task: Option<tokio::task::JoinHandle<()>>,
    host_ports: HashMap<u16, u16>,
    /// Ephemeral engine-managed volumes created for this service (Windows +
    /// Podman). Removed when the service is stopped; persistent volumes are not
    /// tracked here so they survive stop. See #444.
    ephemeral_volumes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalReadinessState {
    Pending,
    Ready,
    Exited(i32),
}

#[derive(Default)]
struct OrchestratorStartupState {
    running: HashMap<String, RunningService>,
    ready: HashSet<String>,
}

#[derive(Clone)]
struct OrchestratorStartupRuntime<C>
where
    C: OciRuntimeClient + Clone + Send + Sync + 'static,
{
    plan: ManifestData,
    prepared: PreparedRunContext,
    orchestration: OrchestrationPlan,
    reporter: Arc<CliReporter>,
    launch_ctx: RuntimeLaunchContext,
    options: OrchestratorOptions,
    client: Arc<C>,
    session_id: String,
    network_name: Option<String>,
    /// Whether the engine is Podman — selects the Windows engine-managed-volume
    /// mount strategy. Detected once at startup. See #444.
    is_podman: bool,
    state: Arc<Mutex<OrchestratorStartupState>>,
    startup_cleanup: Arc<StdMutex<Option<CleanupScope>>>,
}

impl<C> OrchestratorStartupRuntime<C>
where
    C: OciRuntimeClient + Clone + Send + Sync + 'static,
{
    #[allow(clippy::too_many_arguments)]
    fn new(
        plan: ManifestData,
        prepared: PreparedRunContext,
        orchestration: OrchestrationPlan,
        reporter: Arc<CliReporter>,
        launch_ctx: RuntimeLaunchContext,
        options: OrchestratorOptions,
        client: Arc<C>,
        session_id: String,
        network_name: Option<String>,
        is_podman: bool,
        startup_cleanup: Option<CleanupScope>,
    ) -> Self {
        Self {
            plan,
            prepared,
            orchestration,
            reporter,
            launch_ctx,
            options,
            client,
            session_id,
            network_name,
            is_podman,
            state: Arc::new(Mutex::new(OrchestratorStartupState::default())),
            startup_cleanup: Arc::new(StdMutex::new(startup_cleanup)),
        }
    }

    fn commit_startup_cleanup(&self) {
        let scope = self
            .startup_cleanup
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        if let Some(scope) = scope {
            scope.commit_all();
        }
    }

    async fn into_running(self) -> HashMap<String, RunningService> {
        let mut state = self.state.lock().await;
        std::mem::take(&mut state.running)
    }
}

#[async_trait]
impl<C> ServicePhaseRuntime for OrchestratorStartupRuntime<C>
where
    C: OciRuntimeClient + Clone + Send + Sync + 'static,
{
    async fn start_service(&self, service_name: &str) -> Result<()> {
        let service = self
            .orchestration
            .service(service_name)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "service '{}' is missing from orchestration plan",
                    service_name
                )
            })?;

        let env = {
            let state = self.state.lock().await;
            build_service_env(&self.plan, &service, &state.running, &self.launch_ctx)?
        };
        preflight_required_envs(&service, &env)?;

        let running_service = launch_service(
            &self.plan,
            &self.prepared,
            &self.orchestration,
            &service,
            env,
            &self.reporter,
            &self.launch_ctx,
            self.client.as_ref(),
            &self.session_id,
            self.network_name.as_deref(),
            &self.options,
            self.is_podman,
        )
        .await?;

        if let Some(pid) = running_service.local_pid()
            && let Some(scope) = self
                .startup_cleanup
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .as_mut()
        {
            scope.register_kill_child_process(pid, service.name.clone());
        }

        self.state
            .lock()
            .await
            .running
            .insert(service.name.clone(), running_service);
        Ok(())
    }

    async fn await_readiness(&self, service_name: String) -> Result<()> {
        wait_until_ready_in_state(&service_name, &self.state, self.client.as_ref()).await
    }
}

#[allow(clippy::too_many_arguments)]
async fn launch_service<C: OciRuntimeClient>(
    plan: &ManifestData,
    prepared: &PreparedRunContext,
    orchestration: &OrchestrationPlan,
    service: &ResolvedService,
    env: HashMap<String, String>,
    reporter: &std::sync::Arc<CliReporter>,
    launch_ctx: &RuntimeLaunchContext,
    client: &C,
    session_id: &str,
    network_name: Option<&str>,
    options: &OrchestratorOptions,
    is_podman: bool,
) -> Result<RunningService> {
    let handle = match &service.runtime {
        ResolvedServiceRuntime::Oci(runtime) => {
            let image = runtime
                .image
                .clone()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("service '{}' is missing OCI image", service.name)
                })?;

            client
                .pull_image(&image)
                .await
                .with_context(|| format!("failed to pull image for service '{}'", service.name))?;

            let publish_mode =
                determine_publish_mode(orchestration, service, options.publish_policy);
            let host_port = if matches!(publish_mode, PublishMode::Fixed) {
                runtime.port
            } else {
                None
            };
            let ports = runtime
                .port
                .filter(|_| !matches!(publish_mode, PublishMode::None))
                .map(|port| {
                    vec![OciPortSpec {
                        container_port: port,
                        host_port,
                        protocol: "tcp".to_string(),
                        host_ip: Some("127.0.0.1".to_string()),
                    }]
                })
                .unwrap_or_default();

            // Source strategy (#444): on Windows + Podman, Ato-managed writable
            // state becomes an engine-managed named volume instead of a host bind
            // mount; everything else stays a bind mount. Ownership is carried
            // through so the provider can apply engine-delegated ownership init.
            // This is the same `resolve_oci_mount` strategy used by the
            // multi-service executor, so the Desktop session path (which routes
            // here) behaves identically to `ato run`.
            let mounts: Vec<_> = runtime
                .mounts
                .iter()
                .map(|mount| resolve_oci_mount(mount, is_podman, cfg!(target_os = "windows")))
                .collect();

            // Ephemeral engine volumes created for this service must be removed on
            // stop; persistent volumes survive so durable state is preserved.
            let ephemeral_volumes: Vec<String> = mounts
                .iter()
                .filter(|m| {
                    matches!(
                        m.source_kind,
                        OciMountSourceKind::EngineVolume {
                            remove_on_stop: true
                        }
                    )
                })
                .map(|m| m.source.clone())
                .collect();

            let container_id = client
                .create_container(&OciContainerRequest {
                    name: container_name(plan, &service.name, session_id),
                    image,
                    cmd: runtime.cmd.clone(),
                    env: env.clone(),
                    working_dir: runtime.working_dir.clone(),
                    labels: container_labels(plan, &service.name, session_id, &runtime.target),
                    mounts,
                    ports,
                    network: network_name.map(str::to_string),
                    aliases: service.network.aliases.clone(),
                    platform: None,
                    extra_hosts: vec![],
                    user: runtime.user.clone(),
                })
                .await
                .with_context(|| {
                    format!("failed to create container for service '{}'", service.name)
                })?;
            client
                .start_container(&container_id)
                .await
                .with_context(|| {
                    format!("failed to start container for service '{}'", service.name)
                })?;

            let inspect = client
                .inspect_container(&container_id)
                .await
                .unwrap_or_default();
            let mut logs = client.logs(&container_id, true).await?;
            let service_name = service.name.clone();
            let log_task = tokio::spawn(async move {
                while let Some(chunk) = logs.recv().await {
                    match chunk {
                        Ok(chunk) => {
                            let _ = print_prefixed_chunk(&service_name, &chunk);
                        }
                        Err(err) => {
                            let _ = writeln!(
                                std::io::stderr(),
                                "[{}] log error: {}",
                                service_name,
                                err
                            );
                            break;
                        }
                    }
                }
            });

            RunningHandle::Oci(RunningOciService {
                container_id,
                log_task: Some(log_task),
                host_ports: inspect.host_ports,
                ephemeral_volumes,
            })
        }
        ResolvedServiceRuntime::Managed(runtime) => {
            let service_plan = ManifestData {
                selected_target: runtime.target.clone(),
                ..plan.clone()
            };
            let service_launch_ctx = launch_ctx.clone().with_injected_env(env.clone());
            let service_prepared = prepared.with_bridge_manifest(
                service_plan.manifest.clone(),
                if options.target_launch_options().preview_mode {
                    capsule_core::types::ValidationMode::Preview
                } else {
                    capsule_core::types::ValidationMode::Strict
                },
                service_plan.manifest.get("engine").is_some(),
            );
            let prepared = target_runner::prepare_target_execution(
                &service_plan,
                &service_prepared,
                service_launch_ctx,
                &options.target_launch_options(),
            )?;
            let managed_plan = &prepared.runtime_decision.plan;

            let (mut child, cleanup_paths, exit_task, event_rx) = match prepared
                .guard_result
                .executor_kind
            {
                ExecutorKind::Native => {
                    let process = if options.dangerously_skip_permissions {
                        crate::executors::source::execute_host(
                            managed_plan,
                            service_prepared.authoritative_lock.as_ref(),
                            reporter.clone(),
                            ExecuteMode::Piped,
                            &prepared.launch_ctx,
                        )?
                    } else {
                        let nacelle = crate::commands::run::preflight_native_sandbox(
                            options.nacelle.clone(),
                            managed_plan,
                            &service_prepared,
                            prepared.launch_ctx.effective_cwd().map(PathBuf::as_path),
                            reporter,
                        )?;
                        crate::executors::source::execute(
                            managed_plan,
                            service_prepared.authoritative_lock.as_ref(),
                            service_prepared.effective_state.as_ref(),
                            Some(nacelle),
                            reporter.clone(),
                            &options.enforcement,
                            ExecuteMode::Piped,
                            &prepared.launch_ctx,
                        )?
                    };
                    let pid = process.child.id();
                    let exit_task = if options.dangerously_skip_permissions {
                        None
                    } else {
                        Some(tokio::spawn(crate::executors::source::wait_for_pid_exit(
                            pid,
                        )))
                    };
                    (
                        process.child,
                        process.cleanup_paths,
                        exit_task,
                        process.event_rx,
                    )
                }
                ExecutorKind::Deno => (
                    crate::executors::deno::spawn(
                        managed_plan,
                        service_prepared.authoritative_lock.as_ref(),
                        &prepared.execution_plan,
                        &prepared.launch_ctx,
                        options.dangerously_skip_permissions,
                    )?,
                    Vec::new(),
                    None,
                    None,
                ),
                ExecutorKind::NodeCompat => (
                    crate::executors::node_compat::spawn(
                        managed_plan,
                        service_prepared.authoritative_lock.as_ref(),
                        &prepared.execution_plan,
                        &prepared.launch_ctx,
                        options.dangerously_skip_permissions,
                    )?,
                    Vec::new(),
                    None,
                    None,
                ),
                ExecutorKind::WebStatic => {
                    anyhow::bail!(
                        "service '{}' uses runtime=web driver=static, which is unsupported in orchestration mode",
                        service.name
                    );
                }
                ExecutorKind::Wasm => {
                    anyhow::bail!(
                        "service '{}' uses runtime=wasm, which is unsupported in orchestration mode",
                        service.name
                    );
                }
            };
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            RunningHandle::Local(RunningLocalService {
                child,
                stdout_thread: Some(spawn_prefixed_stream(stdout, &service.name, false)),
                stderr_thread: Some(spawn_prefixed_stream(stderr, &service.name, true)),
                cleanup_paths,
                exit_task,
                event_rx,
                readiness_state: LocalReadinessState::Pending,
            })
        }
    };

    Ok(RunningService {
        service: service.clone(),
        env,
        handle,
    })
}

fn build_service_env(
    plan: &ManifestData,
    service: &ResolvedService,
    running: &HashMap<String, RunningService>,
    launch_ctx: &RuntimeLaunchContext,
) -> Result<HashMap<String, String>> {
    // Manifest env first, then launch_ctx env on top: launch_ctx contains
    // dependency-contract template resolutions
    // (`{{deps.<alias>.runtime_exports.X}}` → real DATABASE_URL etc.) that
    // run.rs computes via inject_dependency_contract_env. The earlier order
    // (launch_ctx first, manifest second) re-overrode the resolved values
    // with their literal-template manifest source, which made consumers in
    // orchestration mode boot with `DATABASE_URL = "{{deps.db.runtime...}}"`
    // and crash at startup. Inverting puts launch_ctx — the canonical
    // resolved view — last so it wins.
    let mut env = runtime_overrides::merged_env(service.runtime.runtime().env.clone());
    env.extend(launch_ctx.merged_env());

    if let Some(port) = service.runtime.runtime().port {
        let port = if service.name == "main" {
            runtime_overrides::override_port(Some(port)).unwrap_or(port)
        } else {
            port
        };
        env.insert("PORT".to_string(), port.to_string());
        if service
            .runtime
            .runtime()
            .runtime
            .eq_ignore_ascii_case("web")
        {
            env.entry("HOST".to_string())
                .or_insert_with(|| "127.0.0.1".to_string());
            env.entry("ATO_WEB_HOST".to_string())
                .or_insert_with(|| "127.0.0.1".to_string());
        }
    }

    for connection in &service.connections {
        let dependency = running.get(&connection.dependency).ok_or_else(|| {
            anyhow::anyhow!(
                "dependency '{}' for service '{}' has not been started",
                connection.dependency,
                service.name
            )
        })?;

        let dependency_port = connection.container_port.ok_or_else(|| {
            anyhow::anyhow!(
                "dependency '{}' for service '{}' does not declare a port",
                connection.dependency,
                service.name
            )
        })?;

        let (host, port) = if service.runtime.is_oci() {
            if !dependency.service.runtime.is_oci() {
                anyhow::bail!(
                    "OCI service '{}' cannot depend on non-OCI service '{}'",
                    service.name,
                    connection.dependency
                );
            }
            (
                dependency.service.primary_alias().to_string(),
                dependency_port,
            )
        } else if dependency.service.runtime.is_oci() {
            (
                "127.0.0.1".to_string(),
                resolve_host_port(dependency, dependency_port)?,
            )
        } else {
            ("127.0.0.1".to_string(), dependency_port)
        };

        env.insert(connection.host_env.clone(), host);
        env.insert(connection.port_env.clone(), port.to_string());
    }

    if service.name == "main"
        && let Some(scoped_id) = runtime_overrides::scoped_id_override()
    {
        env.insert("ATO_SCOPED_ID".to_string(), scoped_id);
    }

    if let Some(path) = plan.manifest_path.to_str() {
        env.entry("ATO_MANIFEST_PATH".to_string())
            .or_insert_with(|| path.to_string());
    }

    Ok(env)
}

fn preflight_required_envs(service: &ResolvedService, env: &HashMap<String, String>) -> Result<()> {
    let override_env = runtime_overrides::override_env();
    let missing: Vec<String> = service
        .runtime
        .runtime()
        .required_env
        .iter()
        .filter(|key| {
            env.get(*key)
                .map(|value| value.trim().is_empty())
                .unwrap_or_else(|| {
                    if override_env
                        .get(key.as_str())
                        .map(|value| !value.trim().is_empty())
                        .unwrap_or(false)
                    {
                        return false;
                    }
                    std::env::var(key.as_str())
                        .map(|value| value.trim().is_empty())
                        .unwrap_or(true)
                })
        })
        .cloned()
        .collect();

    if missing.is_empty() {
        return Ok(());
    }

    anyhow::bail!(
        "missing required environment variables for service '{}': {}",
        service.name,
        missing.join(", ")
    );
}

async fn wait_until_ready_in_state<C: OciRuntimeClient>(
    service_name: &str,
    state: &Arc<Mutex<OrchestratorStartupState>>,
    client: &C,
) -> Result<()> {
    {
        let state = state.lock().await;
        if state.ready.contains(service_name) {
            return Ok(());
        }
    }

    let mut service = {
        let mut state = state.lock().await;
        state.running.remove(service_name).ok_or_else(|| {
            anyhow::anyhow!(
                "service '{}' was not started before readiness check",
                service_name
            )
        })?
    };

    let mut completed_run_once = false;
    let result = async {
        if service.service.run_once {
            wait_run_once_service(service_name, &mut service, client).await?;
            stop_service(&mut service, client)
                .await
                .with_context(|| format!("failed to clean up run_once service '{service_name}'"))?;
            drain_service(&mut service);
            completed_run_once = true;
            return Ok(());
        }

        let Some(probe) = service.service.readiness_probe.clone() else {
            return Ok(());
        };

        let initial_delay = readiness_initial_delay(&probe);
        if !initial_delay.is_zero() {
            tokio::time::sleep(initial_delay).await;
        }

        let timeout = readiness_timeout(&probe);
        let interval = readiness_interval(&probe);
        let deadline = Instant::now() + timeout;
        loop {
            if let RunningHandle::Local(local) = &mut service.handle {
                match poll_local_readiness_events(local)? {
                    LocalReadinessState::Ready => return Ok(()),
                    LocalReadinessState::Exited(exit_code) => {
                        // Local (source/native/managed) services keep the generic
                        // orchestration error — the typed
                        // `oci_container_exited_before_ready` diagnostic is for OCI
                        // containers only (#445 review).
                        anyhow::bail!(
                            "service '{}' exited before readiness event was observed (exit code: {})",
                            service_name,
                            exit_code
                        );
                    }
                    LocalReadinessState::Pending => {}
                }
            }

            if let Some(exit_code) = try_wait(&mut service, client).await? {
                // Only OCI containers get the typed exited-before-ready diagnostic
                // (it carries a container log tail and OCI-specific hint); local
                // services keep the generic orchestration error (#445 review).
                // The container id is cloned out of the borrow before awaiting the
                // log fetch so the future stays `Send` (RunningService is `!Sync`).
                let oci_container_id = match &service.handle {
                    RunningHandle::Oci(oci) => Some(oci.container_id.clone()),
                    RunningHandle::Local(_) => None,
                };
                match oci_container_id {
                    Some(container_id) => {
                        let log_tail = oci_log_tail(client, &container_id).await;
                        return Err(OciExitedBeforeReadyError {
                            service_name: service_name.to_string(),
                            exit_code: Some(exit_code as i64),
                            log_tail,
                        }
                        .into());
                    }
                    None => {
                        anyhow::bail!(
                            "service '{}' exited before readiness check passed (exit code: {})",
                            service_name,
                            exit_code
                        );
                    }
                }
            }

            if !uses_event_driven_readiness(&service) {
                if let Some(cmd) = probe.exec.as_ref().filter(|cmd| !cmd.is_empty()) {
                    let container_id = exec_readiness_container_id(&service)?;
                    if exec_readiness_probe_ok(client, &container_id, cmd).await? {
                        return Ok(());
                    }
                } else if let Some(port) = resolve_probe_port(&service, &probe)?
                    && readiness_probe_ok(&probe, port)?
                {
                    return Ok(());
                }
            }

            if Instant::now() >= deadline {
                anyhow::bail!(
                    "service '{}' readiness check timed out after {}s",
                    service_name,
                    timeout.as_secs()
                );
            }

            tokio::time::sleep(interval).await;
        }
    }
    .await;

    let mut state = state.lock().await;
    if !completed_run_once {
        state.running.insert(service_name.to_string(), service);
    }
    if result.is_ok() {
        state.ready.insert(service_name.to_string());
    }
    result
}

async fn wait_run_once_service<C: OciRuntimeClient>(
    service_name: &str,
    service: &mut RunningService,
    client: &C,
) -> Result<()> {
    let timeout = run_once_timeout();
    let deadline = Instant::now() + timeout;

    loop {
        if let Some(exit_code) = try_wait(service, client).await? {
            if exit_code == 0 {
                return Ok(());
            }
            anyhow::bail!(
                "oci_run_once_failed: init container '{}' exited with non-zero status {}",
                service_name,
                exit_code
            );
        }

        if Instant::now() >= deadline {
            anyhow::bail!(
                "oci_run_once_timeout: init container '{}' did not complete within {}s",
                service_name,
                timeout.as_secs()
            );
        }

        tokio::time::sleep(READINESS_INTERVAL).await;
    }
}

async fn monitor_until_exit<C: OciRuntimeClient>(
    orchestration: &OrchestrationPlan,
    running: &mut HashMap<String, RunningService>,
    client: &C,
    network_name: Option<&str>,
) -> Result<i32> {
    let shutdown_signal = wait_for_shutdown_signal();
    tokio::pin!(shutdown_signal);

    loop {
        tokio::select! {
            signal_code = &mut shutdown_signal => {
                shutdown_all(orchestration, running, client, network_name).await;
                return signal_code;
            }
            _ = tokio::time::sleep(SHUTDOWN_POLL_INTERVAL) => {
                let mut exited = None;
                for service_name in &orchestration.startup_order {
                    let Some(service) = running.get_mut(service_name) else {
                        continue;
                    };
                    if let Some(exit_code) = try_wait(service, client).await? {
                        exited = Some((service_name.clone(), exit_code));
                        break;
                    }
                }

                if let Some((_exited_name, exit_code)) = exited {
                    shutdown_all(orchestration, running, client, network_name).await;
                    return Ok(exit_code);
                }
            }
        }
    }
}

#[cfg(unix)]
async fn wait_for_shutdown_signal() -> Result<i32> {
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("Failed to install SIGTERM handler for orchestrator")?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => Ok(130),
        _ = sigterm.recv() => Ok(143),
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown_signal() -> Result<i32> {
    tokio::signal::ctrl_c()
        .await
        .context("Failed to install Ctrl+C handler for orchestrator")?;
    Ok(130)
}

fn uses_event_driven_readiness(service: &RunningService) -> bool {
    matches!(
        &service.handle,
        RunningHandle::Local(local) if local.event_rx.is_some()
    )
}

fn readiness_initial_delay(probe: &ReadinessProbe) -> Duration {
    Duration::from_secs(probe.initial_delay_seconds as u64)
}

fn readiness_timeout(probe: &ReadinessProbe) -> Duration {
    Duration::from_secs(probe.timeout_seconds.max(1) as u64)
}

fn readiness_interval(probe: &ReadinessProbe) -> Duration {
    if probe.interval_seconds == 0 {
        return READINESS_INTERVAL;
    }
    Duration::from_secs(probe.interval_seconds as u64)
}

fn exec_readiness_container_id(service: &RunningService) -> Result<String> {
    match &service.handle {
        RunningHandle::Oci(oci) => Ok(oci.container_id.clone()),
        RunningHandle::Local(_) => anyhow::bail!(
            "service '{}' uses readiness_probe.exec, which is only supported for OCI services",
            service.service.name
        ),
    }
}

async fn exec_readiness_probe_ok<C: OciRuntimeClient>(
    client: &C,
    container_id: &str,
    cmd: &[String],
) -> Result<bool> {
    Ok(client.exec_container(container_id, cmd).await? == 0)
}

fn poll_local_readiness_events(local: &mut RunningLocalService) -> Result<LocalReadinessState> {
    if matches!(
        local.readiness_state,
        LocalReadinessState::Ready | LocalReadinessState::Exited(_)
    ) {
        return Ok(local.readiness_state);
    }

    let Some(event_rx) = local.event_rx.as_ref() else {
        return Ok(LocalReadinessState::Pending);
    };

    match event_rx.try_recv() {
        Ok(LifecycleEvent::Ready { .. }) => {
            local.readiness_state = LocalReadinessState::Ready;
            Ok(local.readiness_state)
        }
        Ok(LifecycleEvent::Started { .. }) => {
            // Launched without a readiness signal — NOT ready. Leave readiness
            // state unchanged (Pending) so we never treat "started" as ready.
            Ok(local.readiness_state)
        }
        Ok(LifecycleEvent::Exited { exit_code, .. }) => {
            local.readiness_state = LocalReadinessState::Exited(exit_code.unwrap_or(1));
            Ok(local.readiness_state)
        }
        Err(TryRecvError::Empty) => Ok(local.readiness_state),
        Err(TryRecvError::Disconnected) => {
            local.event_rx = None;
            Ok(local.readiness_state)
        }
    }
}

async fn shutdown_all<C: OciRuntimeClient>(
    orchestration: &OrchestrationPlan,
    running: &mut HashMap<String, RunningService>,
    client: &C,
    network_name: Option<&str>,
) {
    for service_name in orchestration.startup_order.iter().rev() {
        let Some(mut service) = running.remove(service_name) else {
            continue;
        };
        let _ = stop_service(&mut service, client).await;
        drain_service(&mut service);
    }

    if let Some(network_name) = network_name {
        let _ = client.remove_network(network_name).await;
    }
}

async fn stop_service<C: OciRuntimeClient>(service: &mut RunningService, client: &C) -> Result<()> {
    match &mut service.handle {
        RunningHandle::Local(local) => {
            let _ = send_sigterm(&mut local.child);
            let deadline = Instant::now() + Duration::from_secs(OCI_STOP_TIMEOUT_SECS as u64);
            while Instant::now() < deadline {
                if let Some(task) = local.exit_task.as_ref() {
                    if task.is_finished() {
                        if let Some(task) = local.exit_task.take() {
                            let _ = task.await;
                        }
                        return Ok(());
                    }
                } else if local.child.try_wait()?.is_some() {
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(100));
            }
            if local.exit_task.is_some() || local.child.try_wait()?.is_none() {
                let _ = local.child.kill();
                let _ = local.child.wait();
            }
            if let Some(task) = local.exit_task.take() {
                task.abort();
                let _ = task.await;
            }
        }
        RunningHandle::Oci(oci) => {
            let _ = client
                .stop_container(&oci.container_id, OCI_STOP_TIMEOUT_SECS)
                .await;
            let _ = client.remove_container(&oci.container_id, true).await;
            // Remove ephemeral engine-managed volumes after the container is
            // gone; persistent volumes are not tracked here so they survive. #444
            for volume in &oci.ephemeral_volumes {
                let _ = client.remove_volume(volume).await;
            }
        }
    }
    Ok(())
}

fn drain_service(service: &mut RunningService) {
    match &mut service.handle {
        RunningHandle::Local(local) => {
            if let Some(handle) = local.stdout_thread.take() {
                let _ = handle.join();
            }
            if let Some(handle) = local.stderr_thread.take() {
                let _ = handle.join();
            }
            for path in local.cleanup_paths.drain(..) {
                if path.exists() {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        RunningHandle::Oci(oci) => {
            if let Some(task) = oci.log_task.take() {
                task.abort();
            }
        }
    }
}

async fn try_wait<C: OciRuntimeClient>(
    service: &mut RunningService,
    client: &C,
) -> Result<Option<i32>> {
    match &mut service.handle {
        RunningHandle::Local(local) => {
            if let Some(task) = local.exit_task.as_ref() {
                if !task.is_finished() {
                    return Ok(None);
                }
                let task = local
                    .exit_task
                    .take()
                    .expect("finished exit task must still be present");
                return Ok(Some(
                    task.await.context("native service exit watcher failed")??,
                ));
            }

            Ok(local
                .child
                .try_wait()?
                .map(|status| status.code().unwrap_or(1)))
        }
        RunningHandle::Oci(oci) => {
            let inspect = client.inspect_container(&oci.container_id).await?;
            oci.host_ports = inspect.host_ports.clone();
            if inspect.running {
                Ok(None)
            } else {
                Ok(Some(inspect.exit_code.unwrap_or(1) as i32))
            }
        }
    }
}

/// Best-effort tail of an OCI service's container logs for an
/// `oci_container_exited_before_ready` diagnostic, so the postgres / init output
/// is attached to the diagnostic. Local services have already streamed their
/// stdout/stderr to the console, so this is only called for OCI containers.
///
/// Takes an owned `container_id` rather than `&RunningService` so the future
/// stays `Send` (a `RunningService` is `!Sync` due to its lifecycle channel).
async fn oci_log_tail<C: OciRuntimeClient>(client: &C, container_id: &str) -> Vec<String> {
    match client.logs(container_id, false).await {
        Ok(rx) => collect_log_tail_from_rx(rx, OCI_EXIT_LOG_TAIL_LINES).await,
        Err(_) => Vec::new(),
    }
}

fn resolve_probe_port(service: &RunningService, probe: &ReadinessProbe) -> Result<Option<u16>> {
    // Exec probes do not use a port.
    if probe.exec.is_some() {
        return Ok(None);
    }
    let key = probe
        .port
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "services.{}.readiness_probe.port must be a non-empty env placeholder",
                service.service.name
            )
        })?;

    let container_port = match service.env.get(key) {
        Some(value) => value.parse::<u16>().map_err(|_| {
            anyhow::anyhow!(
                "services.{}.readiness_probe.port '{}' resolved to non-numeric value '{}'",
                service.service.name,
                key,
                value
            )
        })?,
        None => key.parse::<u16>().map_err(|_| {
            anyhow::anyhow!(
                "services.{}.readiness_probe.port '{}' is neither defined in service env nor a numeric port literal",
                service.service.name,
                key
            )
        })?,
    };

    let host_port = match &service.handle {
        RunningHandle::Local(_) => container_port,
        RunningHandle::Oci(oci) => oci
            .host_ports
            .get(&container_port)
            .copied()
            .unwrap_or(container_port),
    };
    Ok(Some(host_port))
}

fn resolve_host_port(service: &RunningService, container_port: u16) -> Result<u16> {
    match &service.handle {
        RunningHandle::Local(_) => Ok(container_port),
        RunningHandle::Oci(oci) => oci.host_ports.get(&container_port).copied().ok_or_else(|| {
            anyhow::anyhow!(
                "service '{}' has no published host port for {}",
                service.service.name,
                container_port
            )
        }),
    }
}

fn determine_publish_mode(
    orchestration: &OrchestrationPlan,
    service: &ResolvedService,
    publish_policy: PublishPolicy,
) -> PublishMode {
    // `services.main` is the leaf consumers reach. The caller's session
    // policy decides exclusively here — the router auto-sets
    // `network.publish = true` for every main service with a port
    // (see `routing/router/services.rs::resolve_services`), so checking
    // `network.publish` first would short-circuit the policy and force
    // Fixed for every recipe. CLI / external callers (`ExternalDefault`)
    // still get the historical fixed-host-port behavior; Desktop sessions
    // (`EphemeralMainService`) get a podman-assigned ephemeral host port
    // so two recipes both declaring `[services.main]` with the same port
    // (e.g. Open WebUI vs Excalidraw at 8080) don't collide on host:8080
    // (#289).
    if service.name == "main" {
        return match publish_policy {
            PublishPolicy::ExternalDefault => PublishMode::Fixed,
            PublishPolicy::EphemeralMainService => PublishMode::Ephemeral,
        };
    }

    // For non-main services, `network.publish = true` is an explicit
    // recipe-level request for a host-stable port (e.g. a sidecar API the
    // recipe wants exposed). Honor it regardless of the session policy.
    if service.network.publish {
        return PublishMode::Fixed;
    }

    if service.readiness_probe.is_some() {
        return PublishMode::Ephemeral;
    }

    if orchestration.services.iter().any(|candidate| {
        candidate
            .depends_on
            .iter()
            .any(|dependency| dependency == &service.name)
            && !candidate.runtime.is_oci()
    }) {
        return PublishMode::Ephemeral;
    }

    PublishMode::None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishMode {
    None,
    Fixed,
    Ephemeral,
}

async fn notify_main_endpoint(
    orchestration: &OrchestrationPlan,
    running: &HashMap<String, RunningService>,
    reporter: &std::sync::Arc<CliReporter>,
) -> Result<()> {
    let Some(main) = orchestration.service("main") else {
        return Ok(());
    };
    let Some(port) = main.runtime.runtime().port else {
        return Ok(());
    };
    let Some(running_main) = running.get("main") else {
        return Ok(());
    };

    let host_port = if main.runtime.is_oci() {
        resolve_host_port(running_main, port)?
    } else {
        port
    };

    reporter
        .notify(format!(
            "🌐 Orchestrated service 'main' is available at http://127.0.0.1:{}/",
            host_port
        ))
        .await?;
    Ok(())
}

fn readiness_probe_ok(probe: &ReadinessProbe, port: u16) -> Result<bool> {
    if let Some(path) = probe
        .http_get
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        return Ok(http_probe(path, port));
    }
    if let Some(target) = probe
        .tcp_connect
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        return Ok(tcp_probe(target, port));
    }
    anyhow::bail!("readiness_probe must define http_get, tcp_connect, or exec");
}

fn http_probe(path: &str, port: u16) -> bool {
    if path.starts_with("http://") || path.starts_with("https://") {
        return false;
    }

    let normalized_path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{}", path)
    };
    let address = format!("127.0.0.1:{}", port);
    let Ok(mut stream) = connect_with_timeout(&address) else {
        return false;
    };
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        normalized_path
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }

    let mut response = [0u8; 128];
    let Ok(read) = stream.read(&mut response) else {
        return false;
    };
    if read == 0 {
        return false;
    }
    let head = String::from_utf8_lossy(&response[..read]);
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok());
    status.map(http_status_indicates_ready).unwrap_or(false)
}

fn tcp_probe(target: &str, port: u16) -> bool {
    let address = if target.contains(':') {
        target.to_string()
    } else {
        format!("{}:{}", target, port)
    };
    connect_with_timeout(&address).is_ok()
}

fn connect_with_timeout(address: &str) -> std::io::Result<TcpStream> {
    let mut addrs = address.to_socket_addrs()?;
    let Some(addr) = addrs.next() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            "no address resolved",
        ));
    };
    TcpStream::connect_timeout(&addr, Duration::from_secs(1))
}

fn spawn_prefixed_stream(
    stream: Option<impl Read + Send + 'static>,
    service_name: &str,
    stderr: bool,
) -> JoinHandle<std::io::Result<()>> {
    // Honor the process-wide envelope-mode redirect: when the parent
    // CLI is emitting a structured envelope on stdout (e.g.
    // `ato app session start --json` spawned by the desktop), service
    // stdout must NOT contaminate the JSON stream. Service stderr
    // already goes to the parent's stderr regardless. See
    // [`redirect_service_stdout_to_stderr_for_envelope_mode`] for
    // the why.
    let stderr = stderr || service_stdout_should_route_to_stderr();
    let name = service_name.to_string();
    thread::spawn(move || -> std::io::Result<()> {
        let Some(stream) = stream else {
            return Ok(());
        };
        let mut reader = BufReader::new(stream);
        let mut buf = Vec::new();
        let prefix = format!("[{}] ", name);
        loop {
            buf.clear();
            let read = reader.read_until(b'\n', &mut buf)?;
            if read == 0 {
                break;
            }
            if stderr {
                let mut writer = std::io::stderr();
                writer.write_all(prefix.as_bytes())?;
                writer.write_all(&buf)?;
                writer.flush()?;
            } else {
                let mut writer = std::io::stdout();
                writer.write_all(prefix.as_bytes())?;
                writer.write_all(&buf)?;
                writer.flush()?;
            }
        }
        Ok(())
    })
}

fn print_prefixed_chunk(service_name: &str, chunk: &OciLogChunk) -> Result<()> {
    let prefix = format!("[{}] ", service_name);
    // Same envelope-mode redirect rule as `spawn_prefixed_stream` —
    // see its body for the rationale. OCI services land here through
    // a different code path but the stdout-contamination risk is the
    // same.
    let route_to_stderr = chunk.stderr || service_stdout_should_route_to_stderr();
    if route_to_stderr {
        let mut writer = std::io::stderr();
        writer.write_all(prefix.as_bytes())?;
        writer.write_all(&chunk.message)?;
        writer.flush()?;
    } else {
        let mut writer = std::io::stdout();
        writer.write_all(prefix.as_bytes())?;
        writer.write_all(&chunk.message)?;
        writer.flush()?;
    }
    Ok(())
}

fn session_id(plan: &ManifestData) -> String {
    format!(
        "{}-{}-{}",
        sanitize_name(
            &plan
                .manifest_name()
                .unwrap_or_else(|| "capsule".to_string())
        ),
        short_hash(plan.manifest_name().as_deref().unwrap_or("capsule")),
        std::process::id()
    )
}

fn network_name(plan: &ManifestData) -> String {
    let manifest_name = plan
        .manifest_name()
        .unwrap_or_else(|| "capsule".to_string());
    format!(
        "ato-{}-{}-{}",
        sanitize_name(&manifest_name),
        short_hash(&manifest_name),
        std::process::id()
    )
}

fn session_labels(plan: &ManifestData, session_id: &str) -> HashMap<String, String> {
    HashMap::from([
        ("io.ato.session".to_string(), session_id.to_string()),
        (
            "io.ato.manifest".to_string(),
            plan.manifest_name()
                .unwrap_or_else(|| "capsule".to_string()),
        ),
    ])
}

fn container_labels(
    plan: &ManifestData,
    service_name: &str,
    session_id: &str,
    target_label: &str,
) -> HashMap<String, String> {
    let mut labels = session_labels(plan, session_id);
    labels.insert("io.ato.service".to_string(), service_name.to_string());
    labels.insert("io.ato.target".to_string(), target_label.to_string());
    labels
}

fn container_name(plan: &ManifestData, service_name: &str, session_id: &str) -> String {
    let manifest_name = plan
        .manifest_name()
        .unwrap_or_else(|| "capsule".to_string());
    format!(
        "ato-{}-{}-{}",
        sanitize_name(&manifest_name),
        short_hash(session_id),
        sanitize_name(service_name)
    )
}

fn short_hash(value: &str) -> String {
    blake3::hash(value.as_bytes())
        .to_hex()
        .to_string()
        .chars()
        .take(8)
        .collect()
}

fn sanitize_name(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

#[cfg(unix)]
fn send_sigterm(child: &mut Child) -> Result<()> {
    let ret = unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    if ret == 0 {
        return Ok(());
    }

    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(err.into())
    }
}

#[cfg(not(unix))]
fn send_sigterm(child: &mut Child) -> Result<()> {
    child.kill().map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::execute_with_client;
    use super::*;
    use capsule_core::runtime::oci::OciContainerInspect;
    use capsule_core::types::ResolvedTargetRuntime;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct FakeClient {
        events: Arc<Mutex<Vec<String>>>,
        states: Arc<Mutex<HashMap<String, FakeState>>>,
    }

    #[derive(Clone, Default)]
    struct FakeState {
        service: String,
        running: bool,
        exit_code: i64,
        inspect_calls: usize,
        host_ports: HashMap<u16, u16>,
        mounts: Vec<(String, String, bool)>,
    }

    #[async_trait::async_trait]
    impl OciRuntimeClient for FakeClient {
        async fn pull_image(&self, image: &str) -> capsule_core::Result<()> {
            self.events.lock().unwrap().push(format!("pull:{image}"));
            Ok(())
        }

        async fn create_network(
            &self,
            request: &OciNetworkRequest,
        ) -> capsule_core::Result<String> {
            self.events
                .lock()
                .unwrap()
                .push(format!("network:create:{}", request.name));
            Ok(request.name.clone())
        }

        async fn remove_network(&self, network_name: &str) -> capsule_core::Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(format!("network:remove:{network_name}"));
            Ok(())
        }

        async fn create_container(
            &self,
            request: &OciContainerRequest,
        ) -> capsule_core::Result<String> {
            let service = request
                .labels
                .get("io.ato.service")
                .cloned()
                .unwrap_or_else(|| request.name.clone());
            self.events
                .lock()
                .unwrap()
                .push(format!("container:create:{service}"));
            self.states.lock().unwrap().insert(
                request.name.clone(),
                FakeState {
                    service: service.clone(),
                    running: false,
                    exit_code: if matches!(service.as_str(), "main" | "migration") {
                        0
                    } else {
                        1
                    },
                    inspect_calls: 0,
                    host_ports: request
                        .ports
                        .iter()
                        .map(|port| {
                            (
                                port.container_port,
                                port.host_port.unwrap_or(port.container_port),
                            )
                        })
                        .collect(),
                    mounts: request
                        .mounts
                        .iter()
                        .map(|mount| (mount.source.clone(), mount.target.clone(), mount.readonly))
                        .collect(),
                },
            );
            Ok(request.name.clone())
        }

        async fn start_container(&self, container_id: &str) -> capsule_core::Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(format!("container:start:{container_id}"));
            if let Some(state) = self.states.lock().unwrap().get_mut(container_id) {
                state.running = true;
            }
            Ok(())
        }

        async fn inspect_container(
            &self,
            container_id: &str,
        ) -> capsule_core::Result<OciContainerInspect> {
            let mut states = self.states.lock().unwrap();
            let state = states.get_mut(container_id).expect("state");
            state.inspect_calls += 1;
            if matches!(state.service.as_str(), "main" | "migration") && state.inspect_calls > 1 {
                state.running = false;
            }
            Ok(OciContainerInspect {
                running: state.running,
                exit_code: (!state.running).then_some(state.exit_code),
                host_ports: state.host_ports.clone(),
            })
        }

        async fn logs(
            &self,
            _container_id: &str,
            _follow: bool,
        ) -> capsule_core::Result<tokio::sync::mpsc::Receiver<capsule_core::Result<OciLogChunk>>>
        {
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok(rx)
        }

        async fn exec_container(
            &self,
            container_id: &str,
            cmd: &[String],
        ) -> capsule_core::Result<i64> {
            self.events
                .lock()
                .unwrap()
                .push(format!("container:exec:{container_id}:{}", cmd.join(" ")));
            Ok(0)
        }

        async fn wait_container(&self, _container_id: &str) -> capsule_core::Result<i64> {
            Ok(0)
        }

        async fn stop_container(
            &self,
            container_id: &str,
            _timeout_secs: i64,
        ) -> capsule_core::Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(format!("container:stop:{container_id}"));
            if let Some(state) = self.states.lock().unwrap().get_mut(container_id) {
                state.running = false;
            }
            Ok(())
        }

        async fn remove_container(
            &self,
            container_id: &str,
            _force: bool,
        ) -> capsule_core::Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(format!("container:remove:{container_id}"));
            Ok(())
        }

        async fn remove_volume(&self, volume_name: &str) -> capsule_core::Result<()> {
            self.events
                .lock()
                .unwrap()
                .push(format!("volume:remove:{volume_name}"));
            Ok(())
        }
    }

    /// Build an OCI `RunningService` with the given ephemeral engine volumes,
    /// reusing `oci_running_service` for the bulk of the shape (#444).
    fn oci_running_service_with_volumes(
        container_id: &str,
        ephemeral_volumes: Vec<String>,
    ) -> RunningService {
        let mut service = oci_running_service(HashMap::new(), HashMap::new());
        if let RunningHandle::Oci(oci) = &mut service.handle {
            oci.container_id = container_id.to_string();
            oci.ephemeral_volumes = ephemeral_volumes;
        }
        service
    }

    #[tokio::test]
    async fn stop_service_removes_ephemeral_engine_volumes() {
        // #444: an OCI service that created an ephemeral engine volume must have
        // it removed on stop, after the container is stopped + removed.
        let client = FakeClient::default();
        let mut service = oci_running_service_with_volumes(
            "cid-db",
            vec!["ato-state-deadbeef0000-pgdata".to_string()],
        );

        stop_service(&mut service, &client).await.expect("stop ok");

        let events = client.events.lock().unwrap();
        assert!(
            events
                .iter()
                .any(|e| e == "volume:remove:ato-state-deadbeef0000-pgdata"),
            "ephemeral volume must be removed on stop: {events:?}"
        );
        // Ordering: container removed before its volume.
        let rm_container = events.iter().position(|e| e == "container:remove:cid-db");
        let rm_volume = events
            .iter()
            .position(|e| e == "volume:remove:ato-state-deadbeef0000-pgdata");
        assert!(rm_container < rm_volume, "container removed before volume");
    }

    #[tokio::test]
    async fn stop_service_without_ephemeral_volumes_removes_none() {
        let client = FakeClient::default();
        let mut service = oci_running_service_with_volumes("cid-web", vec![]);

        stop_service(&mut service, &client).await.expect("stop ok");

        let events = client.events.lock().unwrap();
        assert!(
            !events.iter().any(|e| e.starts_with("volume:remove:")),
            "no volume removal when there are no ephemeral volumes: {events:?}"
        );
    }

    fn manifest_data(manifest_toml: &str) -> ManifestData {
        capsule_core::router::execution_descriptor_from_manifest_parts(
            toml::from_str(manifest_toml).expect("manifest toml"),
            PathBuf::from("/tmp/capsule.toml"),
            PathBuf::from("/tmp"),
            capsule_core::router::ExecutionProfile::Dev,
            Some("app"),
            HashMap::new(),
        )
        .expect("execution descriptor")
    }

    fn http_probe(port: &str) -> ReadinessProbe {
        ReadinessProbe {
            http_get: Some("/".to_string()),
            tcp_connect: None,
            exec: None,
            port: Some(port.to_string()),
            initial_delay_seconds: 0,
            timeout_seconds: 1,
            interval_seconds: 1,
        }
    }

    #[test]
    fn readiness_timing_uses_manifest_probe_fields() {
        let probe = ReadinessProbe {
            initial_delay_seconds: 3,
            timeout_seconds: 60,
            interval_seconds: 2,
            ..http_probe("1111")
        };

        assert_eq!(readiness_initial_delay(&probe), Duration::from_secs(3));
        assert_eq!(readiness_timeout(&probe), Duration::from_secs(60));
        assert_eq!(readiness_interval(&probe), Duration::from_secs(2));
    }

    fn oci_running_service(
        env: HashMap<String, String>,
        host_ports: HashMap<u16, u16>,
    ) -> RunningService {
        RunningService {
            service: ResolvedService {
                name: "main".to_string(),
                depends_on: Vec::new(),
                connections: Vec::new(),
                readiness_probe: None,
                network: Default::default(),
                run_once: false,
                runtime: ResolvedServiceRuntime::Oci(ResolvedTargetRuntime {
                    target: "app".to_string(),
                    runtime: "oci".to_string(),
                    driver: None,
                    runtime_version: None,
                    image: Some("example/app:latest".to_string()),
                    entrypoint: String::new(),
                    run_command: None,
                    cmd: Vec::new(),
                    env: HashMap::new(),
                    working_dir: None,
                    source_layout: None,
                    port: Some(1111),
                    required_env: Vec::new(),
                    mounts: Vec::new(),
                    user: None,
                }),
            },
            env,
            handle: RunningHandle::Oci(RunningOciService {
                container_id: "container-main".to_string(),
                log_task: None,
                host_ports,
                ephemeral_volumes: Vec::new(),
            }),
        }
    }

    #[test]
    fn resolve_probe_port_accepts_numeric_literal_and_maps_to_oci_host_port() {
        let service = oci_running_service(HashMap::new(), HashMap::from([(1111, 49111)]));
        let port =
            resolve_probe_port(&service, &http_probe("1111")).expect("literal port should resolve");
        assert_eq!(port, Some(49111));
    }

    #[test]
    fn resolve_probe_port_keeps_env_placeholder_precedence() {
        let service = oci_running_service(
            HashMap::from([("APP_PORT".to_string(), "1111".to_string())]),
            HashMap::from([(1111, 49111)]),
        );
        let port = resolve_probe_port(&service, &http_probe("APP_PORT"))
            .expect("env placeholder should resolve");
        assert_eq!(port, Some(49111));
    }

    /// Spawn a throwaway, immediately-exiting child so a `RunningLocalService`
    /// can be constructed in tests without driving a real readiness lifecycle.
    fn spawn_dummy_child() -> Child {
        if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/C", "exit"])
                .spawn()
                .expect("spawn cmd")
        } else {
            std::process::Command::new("sh")
                .args(["-c", "exit 0"])
                .spawn()
                .expect("spawn sh")
        }
    }

    #[tokio::test]
    async fn oci_service_exit_before_ready_returns_typed_error() {
        // An OCI container that exits before readiness must surface the typed
        // `OciExitedBeforeReadyError` so diagnostics can map it to E306 (#445).
        let client = FakeClient::default();
        client.states.lock().unwrap().insert(
            "container-main".to_string(),
            FakeState {
                service: "main".to_string(),
                running: false,
                exit_code: 1,
                ..FakeState::default()
            },
        );

        let mut service = oci_running_service(HashMap::new(), HashMap::new());
        service.service.readiness_probe = Some(http_probe("1111"));

        let state = Arc::new(tokio::sync::Mutex::new(OrchestratorStartupState::default()));
        state
            .lock()
            .await
            .running
            .insert("main".to_string(), service);

        let err = wait_until_ready_in_state("main", &state, &client)
            .await
            .expect_err("exited-before-ready must fail readiness");

        let typed = err
            .downcast_ref::<OciExitedBeforeReadyError>()
            .expect("OCI exit must produce the typed error");
        assert_eq!(typed.service_name, "main");
        assert_eq!(typed.exit_code, Some(1));
    }

    #[tokio::test]
    async fn local_service_exit_before_ready_is_not_oci_typed_error() {
        // A local (source/native/managed) service that exits before readiness
        // must keep the generic orchestration error — it must NOT be reclassified
        // as the OCI-specific `oci_container_exited_before_ready` (#445 review).
        let client = FakeClient::default();

        let mut service = oci_running_service(HashMap::new(), HashMap::new());
        service.service.readiness_probe = Some(http_probe("1111"));
        service.handle = RunningHandle::Local(RunningLocalService {
            child: spawn_dummy_child(),
            stdout_thread: None,
            stderr_thread: None,
            cleanup_paths: Vec::new(),
            exit_task: None,
            event_rx: None,
            readiness_state: LocalReadinessState::Exited(7),
        });

        let state = Arc::new(tokio::sync::Mutex::new(OrchestratorStartupState::default()));
        state
            .lock()
            .await
            .running
            .insert("main".to_string(), service);

        let err = wait_until_ready_in_state("main", &state, &client)
            .await
            .expect_err("local exit-before-ready must fail readiness");

        assert!(
            err.downcast_ref::<OciExitedBeforeReadyError>().is_none(),
            "local service exit must not produce the OCI typed error: {err}"
        );
        assert!(
            err.to_string().contains("exited before readiness"),
            "local service must keep the generic orchestration error: {err}"
        );
    }

    #[tokio::test]
    async fn orchestrator_cleans_up_oci_services_and_network() {
        let plan = manifest_data(
            r#"
schema_version = "0.3"
name = "demo-app"
version = "0.1.0"
type = "app"

default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"
port = 8080

[targets.db]
runtime = "oci"
image = "mysql:8"
port = 3306
[state.data]
kind = "filesystem"
durability = "ephemeral"
purpose = "primary-data"

[services.main]
target = "app"
depends_on = ["db"]

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"

[services.db]
target = "db"
"#,
        );
        let client = FakeClient::default();
        let reporter = Arc::new(CliReporter::new(false));
        let launch_ctx = RuntimeLaunchContext::empty();
        let options = OrchestratorOptions {
            enforcement: "strict".to_string(),
            sandbox_mode: true,
            dangerously_skip_permissions: false,
            assume_yes: true,
            nacelle: None,
            publish_policy: PublishPolicy::ExternalDefault,
        };

        let exit = execute_with_client(
            &plan,
            &PreparedRunContext {
                authoritative_lock: None,
                lock_path: None,
                workspace_root: PathBuf::from("/tmp"),
                effective_state: None,
                execution_override: None,
                bridge_manifest:
                    crate::application::pipeline::phases::run::DerivedBridgeManifest::new(
                        plan.manifest.clone(),
                    ),
                validation_mode: capsule_core::types::ValidationMode::Strict,
                engine_override_declared: false,
                compatibility_legacy_lock: None,
                install_profile_key: None,
            },
            reporter,
            &launch_ctx,
            &options,
            None,
            client.clone(),
        )
        .await
        .expect("orchestrator exit");
        assert_eq!(exit, 0);

        let events = client.events.lock().unwrap().clone();
        assert!(
            events
                .iter()
                .any(|event| event.starts_with("network:create:"))
        );
        assert!(
            events
                .iter()
                .any(|event| event.contains("container:create:db"))
        );
        assert!(
            events
                .iter()
                .any(|event| event.contains("container:create:main"))
        );
        assert!(
            events
                .iter()
                .any(|event| event.starts_with("network:remove:"))
        );
        let stop_db = events
            .iter()
            .position(|event| event.contains("container:stop:") && event.contains("db"))
            .expect("db stop");
        let remove_network = events
            .iter()
            .position(|event| event.starts_with("network:remove:"))
            .expect("network remove");
        assert!(stop_db < remove_network);

        let states = client.states.lock().unwrap();
        let app_state = states
            .values()
            .find(|state| state.service == "main")
            .expect("main state");
        assert_eq!(
            app_state.mounts,
            vec![(
                "/var/lib/ato/state/demo-app/data".to_string(),
                "/var/lib/app".to_string(),
                false,
            )]
        );
    }

    #[tokio::test]
    async fn orchestrator_cleans_up_partial_oci_start_on_readiness_error() {
        let plan = manifest_data(
            r#"
schema_version = "0.3"
name = "demo-app"
version = "0.1.0"
type = "app"

default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"
port = 8080

[targets.db]
runtime = "oci"
image = "postgres:16"
port = 5432

[services.main]
target = "app"
depends_on = ["db"]

[services.db]
target = "db"
readiness_probe = { http_get = "/", port = "MISSING_PORT", timeout_seconds = 1 }
"#,
        );
        let client = FakeClient::default();
        let reporter = Arc::new(CliReporter::new(false));
        let launch_ctx = RuntimeLaunchContext::empty();
        let options = OrchestratorOptions {
            enforcement: "strict".to_string(),
            sandbox_mode: true,
            dangerously_skip_permissions: false,
            assume_yes: true,
            nacelle: None,
            publish_policy: PublishPolicy::ExternalDefault,
        };

        let err = execute_with_client(
            &plan,
            &PreparedRunContext {
                authoritative_lock: None,
                lock_path: None,
                workspace_root: PathBuf::from("/tmp"),
                effective_state: None,
                execution_override: None,
                bridge_manifest:
                    crate::application::pipeline::phases::run::DerivedBridgeManifest::new(
                        plan.manifest.clone(),
                    ),
                validation_mode: capsule_core::types::ValidationMode::Strict,
                engine_override_declared: false,
                compatibility_legacy_lock: None,
                install_profile_key: None,
            },
            reporter,
            &launch_ctx,
            &options,
            None,
            client.clone(),
        )
        .await
        .expect_err("readiness error should fail startup");
        assert!(
            err.to_string().contains("MISSING_PORT"),
            "unexpected error: {err}"
        );

        let events = client.events.lock().unwrap().clone();
        let db_create = events
            .iter()
            .position(|event| event.contains("container:create:db"))
            .expect("db create");
        let db_stop = events
            .iter()
            .position(|event| event.contains("container:stop:") && event.contains("db"))
            .expect("db stop");
        let db_remove = events
            .iter()
            .position(|event| event.contains("container:remove:") && event.contains("db"))
            .expect("db remove");
        let network_remove = events
            .iter()
            .position(|event| event.starts_with("network:remove:"))
            .expect("network remove");
        assert!(db_create < db_stop);
        assert!(db_stop < db_remove);
        assert!(db_remove < network_remove);
    }

    #[tokio::test]
    async fn orchestrator_exec_readiness_probe_starts_dependent_service() {
        let plan = manifest_data(
            r#"
schema_version = "0.3"
name = "demo-app"
version = "0.1.0"
type = "app"

default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"
port = 8080

[targets.db]
runtime = "oci"
image = "postgres:16"
port = 5432

[services.main]
target = "app"
depends_on = ["db"]

[services.db]
target = "db"
readiness_probe = { exec = ["pg_isready", "-U", "postgres"], timeout_seconds = 60, interval_seconds = 1 }
"#,
        );
        let client = FakeClient::default();
        let reporter = Arc::new(CliReporter::new(false));
        let launch_ctx = RuntimeLaunchContext::empty();
        let options = OrchestratorOptions {
            enforcement: "strict".to_string(),
            sandbox_mode: true,
            dangerously_skip_permissions: false,
            assume_yes: true,
            nacelle: None,
            publish_policy: PublishPolicy::ExternalDefault,
        };

        let _handle = execute_until_ready_and_detach(
            &plan,
            &PreparedRunContext {
                authoritative_lock: None,
                lock_path: None,
                workspace_root: PathBuf::from("/tmp"),
                effective_state: None,
                execution_override: None,
                bridge_manifest:
                    crate::application::pipeline::phases::run::DerivedBridgeManifest::new(
                        plan.manifest.clone(),
                    ),
                validation_mode: capsule_core::types::ValidationMode::Strict,
                engine_override_declared: false,
                compatibility_legacy_lock: None,
                install_profile_key: None,
            },
            reporter,
            &launch_ctx,
            &options,
            None,
            client.clone(),
        )
        .await
        .expect("exec readiness should allow startup");

        let events = client.events.lock().unwrap().clone();
        let db_exec = events
            .iter()
            .position(|event| event.contains("container:exec:") && event.contains("pg_isready"))
            .expect("db exec readiness probe");
        let main_create = events
            .iter()
            .position(|event| event.contains("container:create:main"))
            .expect("main create after db readiness");
        assert!(db_exec < main_create);
    }

    #[tokio::test]
    async fn orchestrator_run_once_completion_starts_dependent_service() {
        let plan = manifest_data(
            r#"
schema_version = "0.3"
name = "demo-app"
version = "0.1.0"
type = "app"

default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"
port = 8080

[targets.db]
runtime = "oci"
image = "postgres:16"
port = 5432

[targets.migration]
runtime = "oci"
image = "ghcr.io/example/app:latest"
run_once = true
cmd = ["node", "./scripts/migrate.js"]

[services.main]
target = "app"
depends_on = ["migration"]

[services.db]
target = "db"

[services.migration]
target = "migration"
depends_on = ["db"]
"#,
        );
        let client = FakeClient::default();
        let reporter = Arc::new(CliReporter::new(false));
        let launch_ctx = RuntimeLaunchContext::empty();
        let options = OrchestratorOptions {
            enforcement: "strict".to_string(),
            sandbox_mode: true,
            dangerously_skip_permissions: false,
            assume_yes: true,
            nacelle: None,
            publish_policy: PublishPolicy::ExternalDefault,
        };

        let handle = execute_until_ready_and_detach(
            &plan,
            &PreparedRunContext {
                authoritative_lock: None,
                lock_path: None,
                workspace_root: PathBuf::from(".tmp/orchestrator-run-once-test"),
                effective_state: None,
                execution_override: None,
                bridge_manifest:
                    crate::application::pipeline::phases::run::DerivedBridgeManifest::new(
                        plan.manifest.clone(),
                    ),
                validation_mode: capsule_core::types::ValidationMode::Strict,
                engine_override_declared: false,
                compatibility_legacy_lock: None,
                install_profile_key: None,
            },
            reporter,
            &launch_ctx,
            &options,
            None,
            client.clone(),
        )
        .await
        .expect("run_once completion should allow startup");

        let events = client.events.lock().unwrap().clone();
        let migration_create = events
            .iter()
            .position(|event| event.contains("container:create:migration"))
            .expect("migration create");
        let migration_remove = events
            .iter()
            .position(|event| event.contains("container:remove:") && event.contains("migration"))
            .expect("migration remove");
        let main_create = events
            .iter()
            .position(|event| event.contains("container:create:main"))
            .expect("main create");

        assert!(migration_create < migration_remove);
        assert!(migration_remove < main_create);
        assert!(
            !handle
                .services
                .iter()
                .any(|service| service.name == "migration"),
            "completed run_once services must not be retained in detached session snapshot"
        );
        assert!(handle.services.iter().any(|service| service.name == "main"));
    }

    /// Regression for #106: `ato app session start --json` reserves
    /// stdout for the SessionStartEnvelope. The orchestrator stream
    /// pumper must therefore route service stdout to the parent's
    /// stderr while the envelope-mode flag is set, so the captured
    /// stdout doesn't contain `[main] ...` lines that break JSON
    /// parsing.
    ///
    /// Pre-fix, `service_stdout_should_route_to_stderr` did not exist;
    /// `spawn_prefixed_stream` always wrote to stdout when its `stderr`
    /// arg was false. Post-fix, the static flag OR'd into the local
    /// `stderr` variable forces stderr routing.
    ///
    /// We test the gate function directly because driving an actual
    /// pumper requires a `Read` stream + thread join and is covered
    /// end-to-end by the #92 verification harness on a clean
    /// `ATO_HOME`.
    #[test]
    fn determine_publish_mode_respects_ephemeral_main_service_policy() {
        use capsule_core::types::{
            OrchestrationPlan, ResolvedService, ResolvedServiceNetwork, ResolvedServiceRuntime,
            ResolvedTargetRuntime,
        };

        // Minimal `[services.main]` shaped like Open WebUI / Excalidraw:
        // OCI runtime, declared port 8080, no readiness_probe, no explicit
        // `network.publish`. The bug under #289 is that this shape always
        // bound host:8080 under PublishMode::Fixed regardless of caller.
        let service = ResolvedService {
            name: "main".to_string(),
            depends_on: Vec::new(),
            connections: Vec::new(),
            readiness_probe: None,
            network: ResolvedServiceNetwork::default(),
            run_once: false,
            runtime: ResolvedServiceRuntime::Oci(ResolvedTargetRuntime {
                target: "app".to_string(),
                runtime: "oci".to_string(),
                driver: None,
                runtime_version: None,
                image: Some("ghcr.io/example/app:latest".to_string()),
                entrypoint: String::new(),
                run_command: None,
                cmd: Vec::new(),
                env: Default::default(),
                working_dir: None,
                source_layout: None,
                port: Some(8080),
                required_env: Vec::new(),
                mounts: Vec::new(),
                user: None,
            }),
        };
        let plan = OrchestrationPlan {
            startup_order: vec!["main".to_string()],
            services: vec![service.clone()],
        };

        assert_eq!(
            super::determine_publish_mode(&plan, &service, PublishPolicy::ExternalDefault),
            super::PublishMode::Fixed,
            "ato run / CLI path must keep main on the declared host port"
        );
        assert_eq!(
            super::determine_publish_mode(&plan, &service, PublishPolicy::EphemeralMainService),
            super::PublishMode::Ephemeral,
            "EphemeralMainService must hand main an ephemeral host port so two recipes sharing port 8080 do not collide (#289)"
        );

        // For non-main services, `network.publish = true` is an explicit
        // recipe-level request for a stable host port and must win over the
        // policy. (For `main` services the policy decides exclusively — see
        // the comment in `determine_publish_mode`. The router auto-sets
        // `network.publish = true` for every main with a port, so honoring
        // it on main would defeat the policy.)
        let sidecar = ResolvedService {
            name: "api".to_string(),
            depends_on: Vec::new(),
            connections: Vec::new(),
            readiness_probe: None,
            network: ResolvedServiceNetwork {
                publish: true,
                ..Default::default()
            },
            run_once: false,
            runtime: service.runtime.clone(),
        };
        let plan_with_sidecar = OrchestrationPlan {
            startup_order: vec!["main".to_string(), "api".to_string()],
            services: vec![service.clone(), sidecar.clone()],
        };
        assert_eq!(
            super::determine_publish_mode(
                &plan_with_sidecar,
                &sidecar,
                PublishPolicy::EphemeralMainService
            ),
            super::PublishMode::Fixed,
            "non-main service with explicit network.publish=true must keep the recipe-level fixed host port even under EphemeralMainService"
        );
    }

    #[test]
    fn envelope_mode_flag_round_trips() {
        // The flag is process-wide; serialize the test against itself
        // by setting + clearing within this single test. Other tests
        // in this module never touch the flag.
        super::redirect_service_stdout_to_stderr_for_envelope_mode(false);
        assert!(!super::service_stdout_should_route_to_stderr());

        super::redirect_service_stdout_to_stderr_for_envelope_mode(true);
        assert!(super::service_stdout_should_route_to_stderr());

        // Restore so subsequent tests (and the binary's tests across
        // `cargo test` invocations on the same process) are not
        // pinned to envelope mode.
        super::redirect_service_stdout_to_stderr_for_envelope_mode(false);
        assert!(!super::service_stdout_should_route_to_stderr());
    }
}
