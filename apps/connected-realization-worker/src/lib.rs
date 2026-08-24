//! Connected control-plane consumer for hosted physical Realizations.
//!
//! This is deliberately an application component, not a revival of the old
//! `ato runner serve` command. It consumes the current lease protocol, uses the
//! shared runtime graph validator, asks `RealizationPlanner` to select a path,
//! and reports ready only after Contract acceptance and Surface publication.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail, ensure};
use ato_adapter_api::{
    ActuatorProviderRegistry, AdapterAttachContext, AdapterContext, AdapterInstance,
    AdapterRegistry, AttachedAdapter, IgnoreObservations, LiveOperation, Stylus,
    WorkspaceCapturePolicy,
};
use ato_adapter_browser::{
    BROWSER_PROTOCOL_ID, BrowserAdapter, BrowserAdapterConfig, BrowserInputMode,
    register_record_schemas as register_browser_record_schemas,
};
use ato_browser_host::{BrowserHost, BrowserHostConfig};
use ato_browser_semantics::{
    AcceptedBrowserOperation, BROWSER_COMPUTATION_SEMANTICS_ID, BrowserComputationSemantics,
    BrowserHeadPersistence, BrowserOperationActuator, BrowserOperationIngress,
    BrowserProtocolSemantics, BrowserRecordSubmission,
};
use ato_compose::{COMPOSE_SEMANTICS_ID, ComposeSemantics, decode_composite_residual};
use ato_computation::{ComputationRef, ContentRef, PortId, ProtocolId, SemanticsId};
use ato_contracts::{HttpEndpointVerifier, WorkspaceContentVerifier};
use ato_kernel::{Kernel, RunEvolutionAuthority};
use ato_materializer_api::{
    AcceptedRealization, ContractContext, ContractVerifierRegistry, MaterializerContext,
    MaterializerError, MaterializerRegistry, Realization, accept_candidate,
};
use ato_materializer_vm_snapshot::{
    ActiveFirecrackerRealization, FirecrackerActiveVmCaptureSource, FirecrackerBackend,
    FirecrackerBackendConfig, FirecrackerRecordCaptureBarrier, FirecrackerRecordCaptureLease,
    FirecrackerSurfaceRelayConfig, VM_SNAPSHOT_MATERIALIZER_ID, VmSnapshotError,
    VmSnapshotMaterializer,
};
use ato_objects::{ObjectResolver, ObjectStore, RecordCandidate, resolve_computation};
use ato_planner::{
    MaterializationCandidate, Placement, PlannerPolicy, RealizationPlanner, TargetEnvironment,
    TrustBoundary,
};
use ato_runtime_object_graph::{
    GraphDownloadExpectation, ObjectGraphIndexV1, RuntimeGraphSource, ValidatedRuntimeGraph,
    download_and_validate_graph,
};
use clap::Parser;
use reqwest::blocking::{Client, RequestBuilder, Response};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const PORTABLE_CAPSULE_LEASE_KIND: &str = "portable_capsule_v2";
const RUNNER_CAPABILITIES: &[&str] = &[
    "execution_abi=process",
    "isolation=untrusted-v1",
    "materializer=ato.materialize.vm.snapshot@1",
    "backend=firecracker",
];
const ACTIVE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const GUEST_CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const GUEST_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
struct HostedBrowserBinding {
    port: PortId,
}

/// Finds the single explicitly composed Browser continuation. This never
/// injects a Port into an existing source/2048 Computation: no Browser leaf
/// means no hosted Chrome lifecycle.
fn hosted_browser_binding(
    root: &ComputationRef,
    objects: &dyn ObjectResolver,
) -> Result<Option<HostedBrowserBinding>> {
    let mut leaves = Vec::new();
    collect_browser_leaves(root, objects, &mut leaves)?;
    if leaves.is_empty() {
        return Ok(None);
    }
    ensure!(
        leaves.len() == 1,
        "multiple Browser Computations are not supported by one Hosted Run"
    );
    let root = resolve_computation(objects, root)?;
    let ports = root
        .object()
        .boundary
        .iter()
        .filter(|(_, definition)| {
            definition.protocol.as_str() == BROWSER_PROTOCOL_ID
                && definition.role.as_str() == "controller"
        })
        .map(|(port, _)| port.clone())
        .collect::<Vec<_>>();
    let [port] = ports.as_slice() else {
        bail!("Browser Computation must be explicitly exported as one controller Port");
    };
    Ok(Some(HostedBrowserBinding { port: port.clone() }))
}

fn collect_browser_leaves(
    reference: &ComputationRef,
    objects: &dyn ObjectResolver,
    leaves: &mut Vec<()>,
) -> Result<()> {
    let resolved = resolve_computation(objects, reference)?;
    if resolved.object().semantics == SemanticsId::parse(BROWSER_COMPUTATION_SEMANTICS_ID)? {
        leaves.push(());
        return Ok(());
    }
    if resolved.object().semantics == SemanticsId::parse(COMPOSE_SEMANTICS_ID)? {
        let metadata = objects.metadata(&resolved.object().residual)?;
        let bytes = ato_objects::read_exact_object(
            objects,
            &resolved.object().residual,
            metadata.size,
            ato_compose::MAX_COMPOSITE_RESIDUAL_BYTES,
        )?;
        for child in decode_composite_residual(&bytes)?.nodes.values() {
            collect_browser_leaves(child, objects, leaves)?;
        }
    }
    Ok(())
}

struct AttachedBrowserActuator(Arc<Mutex<Box<dyn AttachedAdapter>>>);

impl BrowserOperationActuator for AttachedBrowserActuator {
    fn apply(&mut self, operation: &LiveOperation) -> std::result::Result<(), String> {
        self.0
            .lock()
            .map_err(|_| "Browser Adapter session mutex poisoned".to_owned())?
            .apply_operation(operation)
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone)]
struct RunnerBrowserHeadPersistence {
    api: HttpRunnerApi,
    lease_id: String,
}

impl BrowserHeadPersistence for RunnerBrowserHeadPersistence {
    fn persist(&self, operation: &AcceptedBrowserOperation) -> std::result::Result<(), String> {
        let pending = ato_kernel::PendingHeadPersistence {
            transition: operation.transition.clone(),
            run_seq: operation.run_seq,
        };
        self.api
            .persist_computation_head(&self.lease_id, &operation.operation_id, &pending)
            .map_err(|error| error.to_string())
    }
}

#[derive(Clone)]
struct RunnerBrowserRecordSubmission {
    stylus: Arc<ato_record_writer::AsyncRecordStylus>,
    port: PortId,
    stream: String,
    next_local_seq: Arc<AtomicU64>,
}

impl BrowserRecordSubmission for RunnerBrowserRecordSubmission {
    fn submit(&self, operation: &AcceptedBrowserOperation) -> std::result::Result<(), String> {
        // `operation` contains the exact event/transition/run_seq/operation_id
        // accepted by the authority. Record metadata stays outside semantic
        // identity; the portable Record itself remains only the Browser action.
        self.stylus
            .record(RecordCandidate {
                protocol_id: ProtocolId::parse(BROWSER_PROTOCOL_ID)
                    .expect("static Browser Protocol ID"),
                operation_id: ato_computation::OperationId::parse(
                    ato_adapter_browser::operation_for_event(&operation.event),
                )
                .expect("static Browser operation ID"),
                port_id: self.port.clone(),
                payload: ato_adapter_browser::encode_event(&operation.event)
                    .map_err(|error| error.to_string())?,
                payload_version: 1,
                required_features: BTreeSet::new(),
                recorded_by: Some("ato.browser@1".to_owned()),
                stream: self.stream.clone(),
                local_seq: self.next_local_seq.fetch_add(1, Ordering::Relaxed) + 1,
                caused_by: Vec::new(),
                observed_at: OffsetDateTime::now_utc().unix_timestamp().to_string(),
            })
            .map_err(|error| error.to_string())
    }
}

type HostedBrowserIngress = BrowserOperationIngress<
    AttachedBrowserActuator,
    RunnerBrowserHeadPersistence,
    RunnerBrowserRecordSubmission,
>;

struct HostedBrowserRuntime {
    ingress: Arc<HostedBrowserIngress>,
    adapter: Option<Arc<Mutex<Box<dyn AttachedAdapter>>>>,
    host: Option<BrowserHost>,
    pipeline: Option<ato_record_writer::RecordPipeline>,
    objects: Arc<dyn ObjectStore>,
    workspace: PathBuf,
}

impl HostedBrowserRuntime {
    /// Future capture coordination freezes this logical gate before it asks
    /// the Browser Host to quiesce. No physical Browser state is captured in
    /// P0-B.
    fn freeze(&self) -> Result<ato_kernel::RunHeadSnapshot> {
        self.ingress
            .freeze()
            .map_err(|error| anyhow::anyhow!(error.to_string()))
    }

    fn stop(mut self) -> Result<()> {
        self.freeze()?;
        self.cleanup()?;
        Ok(())
    }

    fn cleanup(&mut self) -> Result<()> {
        if let Some(adapter) = self.adapter.take() {
            let mut adapter = adapter
                .lock()
                .map_err(|_| anyhow::anyhow!("Browser Adapter session mutex poisoned"))?;
            adapter.quiesce(&AdapterContext {
                workspace: &self.workspace,
                objects: self.objects.as_ref(),
            })?;
            adapter.detach(&AdapterContext {
                workspace: &self.workspace,
                objects: self.objects.as_ref(),
            })?;
        }
        if let Some(host) = self.host.take() {
            host.stop()?;
        }
        if let Some(pipeline) = self.pipeline.take() {
            pipeline.shutdown()?;
        }
        Ok(())
    }
}

impl Drop for HostedBrowserRuntime {
    fn drop(&mut self) {
        // Error paths may bypass the normal stop request. Do not leave a
        // private profile, bridge transport, or writer behind merely because
        // the control-plane request which followed readiness failed.
        let _ = self.cleanup();
    }
}

struct WorkerRecordCaptureBarrier {
    inner: ato_record_writer::CaptureBarrier,
}

struct WorkerRecordCaptureLease {
    frontier: ContentRef,
    _paused: ato_record_writer::PausedCapture,
}

impl FirecrackerRecordCaptureLease for WorkerRecordCaptureLease {
    fn frontier_ref(&self) -> &ContentRef {
        &self.frontier
    }
}

impl FirecrackerRecordCaptureBarrier for WorkerRecordCaptureBarrier {
    fn pause_and_seal(
        &self,
    ) -> std::result::Result<Box<dyn FirecrackerRecordCaptureLease>, VmSnapshotError> {
        let paused = self
            .inner
            .pause_and_seal()
            .map_err(|error| VmSnapshotError::Backend(error.to_string()))?;
        Ok(Box::new(WorkerRecordCaptureLease {
            frontier: paused.frontier.frontier_digest.clone(),
            _paused: paused,
        }))
    }
}

/// Capture-capable hosted assembly. Ordinary restore workers use
/// `FirecrackerBackend::new`; only a runtime that actually owns an active VM
/// and its Record Writer receives this backend.
pub fn capture_capable_firecracker_backend(
    config: FirecrackerBackendConfig,
    active: Box<dyn ActiveFirecrackerRealization>,
    barrier: ato_record_writer::CaptureBarrier,
    capture_root: PathBuf,
) -> FirecrackerBackend {
    let barrier = Arc::new(WorkerRecordCaptureBarrier { inner: barrier });
    let source = Arc::new(FirecrackerActiveVmCaptureSource::new(
        active,
        barrier,
        capture_root,
    ));
    FirecrackerBackend::with_capture_source(config, source)
}

#[derive(Debug, Clone, Parser)]
pub struct WorkerConfig {
    #[arg(long, env = "ATO_API_URL")]
    pub api_base: String,
    #[arg(long, env = "ATO_RUNNER_ID", default_value = "")]
    pub runner_id: String,
    #[arg(long, env = "ATO_RUNNER_TOKEN", default_value = "")]
    pub runner_token: String,
    /// Existing canonical runner credential JSON. Explicit CLI/env identity
    /// values, when provided, must exactly match this file.
    #[arg(long, env = "ATO_RUNNER_CREDENTIALS_FILE")]
    pub runner_credentials_file: Option<PathBuf>,
    #[arg(long, env = "ATO_RUNNER_PUBLIC_BASE_URL")]
    pub public_base_url: String,
    #[arg(long, env = "ATO_RUNTIME_WORK_ROOT")]
    pub work_root: PathBuf,
    /// Loopback port consumed by the existing per-slot ingress.
    #[arg(
        long,
        env = "ATO_RUNTIME_SURFACE_LISTEN",
        default_value = "127.0.0.1:8420"
    )]
    pub surface_listen: SocketAddr,
    /// Candidate-internal loopback relay used by Contract verifiers.
    #[arg(
        long,
        env = "ATO_RUNTIME_HIDDEN_SURFACE_LISTEN",
        default_value = "127.0.0.1:18420"
    )]
    pub hidden_surface_listen: SocketAddr,
    /// Host-reachable guest endpoint behind the candidate-internal relay. This
    /// is physical runtime configuration and never participates in identity.
    #[arg(long, env = "ATO_RUNTIME_SURFACE_TARGET")]
    pub surface_target: SocketAddr,
    #[arg(long, env = "ATO_FC_TAP_HOST_CIDR")]
    pub tap_host_cidr: String,
    #[arg(long, env = "ATO_RUNNER_SLOT_ID", default_value = "0")]
    pub slot_id: String,
    /// Required only by roots that explicitly compose a Browser Computation.
    #[arg(long, env = "ATO_BROWSER_CHROME")]
    pub browser_chrome: Option<PathBuf>,
    #[arg(long)]
    pub once: bool,
}

pub struct ConnectedWorker {
    config: WorkerConfig,
    api: HttpRunnerApi,
}

impl ConnectedWorker {
    pub fn new(mut config: WorkerConfig) -> Result<Self> {
        resolve_runner_credentials(&mut config)?;
        validate_config(&config)?;
        fs::create_dir_all(&config.work_root)?;
        let api = HttpRunnerApi::new(&config.api_base, &config.runner_id, &config.runner_token)?;
        Ok(Self { config, api })
    }

    pub fn run(&self) -> Result<()> {
        self.api.heartbeat(&self.config, 0)?;
        loop {
            let claim = self.api.claim_next()?;
            let Some(lease) = claim.lease else {
                if self.config.once {
                    return Ok(());
                }
                thread::sleep(Duration::from_secs(claim.next_poll_seconds.clamp(1, 30)));
                self.api.heartbeat(&self.config, 0)?;
                continue;
            };
            self.api.heartbeat(&self.config, 1)?;
            if let Err(error) = self.execute_lease(&lease) {
                let message = format!("connected Realization failed: {error:#}");
                let _ = self.api.report_failed(&lease.id, &message);
            }
            self.api.heartbeat(&self.config, 0)?;
            if self.config.once {
                return Ok(());
            }
        }
    }

    fn execute_lease(&self, lease: &ClaimedLease) -> Result<()> {
        validate_lease(lease, SystemTime::now())?;
        self.api.report_status(&lease.id, "preparing")?;

        let lease_root = self.config.work_root.join("leases").join(&lease.id);
        fs::create_dir_all(&lease_root)?;
        let result = (|| {
            let source = self.api.graph_source(lease)?;
            let index: ObjectGraphIndexV1 = serde_json::from_slice(source.index_bytes())
                .context("runtime graph index is not valid JSON")?;
            let expectation = GraphDownloadExpectation {
                index_digest: source.index_digest().to_owned(),
                root_computation_ref: lease.command.expected_root_computation_ref.clone(),
                object_count: index.objects.len(),
                logical_bytes: index.logical_bytes()?,
            };
            let graph = download_and_validate_graph(&source, &expectation, &lease_root)?;
            // The Worker owns the live logical Run head from the same immutable
            // root that its assigned lease authorizes. No operation is wired to
            // this authority yet: Browser composition and its registered
            // Semantics arrive in P0-B. Keeping it alive here establishes the
            // hosted lifecycle without inventing an authoring/hash fallback.
            let evolution = Arc::new(initialize_hosted_run_evolution_authority(
                &graph,
                &lease.command.expected_root_computation_ref,
            )?);
            let firecracker_work_root = self.config.work_root.join("fc");
            let physical = RestorePhysicalConfig {
                firecracker_work_root: &firecracker_work_root,
                slot_id: &self.config.slot_id,
                hidden_surface_listen: self.config.hidden_surface_listen,
                guest_surface_target: self.config.surface_target,
                tap_host_cidr: &self.config.tap_host_cidr,
            };
            let running = restore_vm_path(&graph, &lease_root, &lease.id, &physical)?;
            self.api.report_status(&lease.id, "running")?;

            // An explicit Browser Computation receives one private Chrome
            // realization. Ordinary VM-only roots take the unchanged path.
            let mut browser = start_hosted_browser_runtime(
                &graph,
                lease,
                &lease_root,
                &lease_root.join("workspace"),
                Arc::clone(&evolution),
                self.api.clone(),
                self.config.browser_chrome.as_deref(),
                &format!("http://{}/", self.config.hidden_surface_listen),
            )?;

            // The externally reachable listener does not exist until the VM is
            // active, every Contract passed, and the Realization published.
            let proxy = TcpProxy::start(
                self.config.surface_listen,
                ProxyTarget::Tcp(self.config.hidden_surface_listen),
            )?;
            let execution_id = format!("vm:{}:{}", lease.run_id, lease.id);
            ensure!(
                evolution.current_head().head.as_str()
                    == lease.command.expected_root_computation_ref,
                "hosted evolution authority root changed before the first operation"
            );
            self.api.report_ready(
                &lease.id,
                &execution_id,
                &self.config.public_base_url,
                ready_local_port(&self.config),
            )?;
            let mut last_heartbeat = Instant::now();
            loop {
                let control = self.api.control(&lease.id)?;
                if control.stop_requested {
                    if let Some(browser) = browser.take() {
                        browser.stop()?;
                    }
                    drop(proxy);
                    running.quiesce()?;
                    self.api.report_stopped(&lease.id, &execution_id)?;
                    return Ok(());
                }
                if last_heartbeat.elapsed() >= ACTIVE_HEARTBEAT_INTERVAL {
                    self.api.heartbeat(&self.config, 1)?;
                    last_heartbeat = Instant::now();
                }
                thread::sleep(Duration::from_secs(1));
            }
        })();
        let cleanup = fs::remove_dir_all(&lease_root)
            .with_context(|| format!("failed to clean lease directory {}", lease_root.display()));
        match (result, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(error),
        }
    }
}

#[derive(Deserialize)]
struct StoredRunnerCredentials {
    api_base: String,
    runner_id: String,
    runner_token: String,
}

fn resolve_runner_credentials(config: &mut WorkerConfig) -> Result<()> {
    let Some(path) = &config.runner_credentials_file else {
        return Ok(());
    };
    let credentials: StoredRunnerCredentials = serde_json::from_slice(
        &fs::read(path)
            .with_context(|| format!("failed to read runner credentials {}", path.display()))?,
    )
    .context("runner credentials are not valid JSON")?;
    ensure!(
        credentials.api_base.trim_end_matches('/') == config.api_base.trim_end_matches('/'),
        "runner credential API does not match configured API"
    );
    if config.runner_id.is_empty() {
        config.runner_id = credentials.runner_id;
    } else {
        ensure!(
            config.runner_id == credentials.runner_id,
            "runner credential identity does not match configured runner"
        );
    }
    if config.runner_token.is_empty() {
        config.runner_token = credentials.runner_token;
    } else {
        ensure!(
            config.runner_token == credentials.runner_token,
            "runner credential token does not match configured token"
        );
    }
    Ok(())
}

fn ready_local_port(config: &WorkerConfig) -> u16 {
    config.surface_listen.port()
}

fn validate_config(config: &WorkerConfig) -> Result<()> {
    ensure!(!config.runner_id.trim().is_empty(), "runner id is empty");
    ensure!(
        !config.runner_token.trim().is_empty(),
        "runner token is empty"
    );
    ensure!(
        config.surface_listen.ip().is_loopback(),
        "Surface listener must be loopback-only behind existing ingress"
    );
    ensure!(
        config.hidden_surface_listen.ip().is_loopback(),
        "hidden Surface listener must be loopback-only"
    );
    ensure!(
        config.hidden_surface_listen != config.surface_listen,
        "hidden and published Surface listeners must be distinct"
    );
    ensure!(
        !config.tap_host_cidr.trim().is_empty() && config.tap_host_cidr.contains('/'),
        "TAP host CIDR is invalid"
    );
    Ok(())
}

fn validate_lease(lease: &ClaimedLease, now: SystemTime) -> Result<()> {
    ensure!(valid_control_id(&lease.id), "invalid lease id");
    ensure!(valid_control_id(&lease.run_id), "invalid run id");
    ensure!(
        lease.command.kind == PORTABLE_CAPSULE_LEASE_KIND,
        "unsupported lease kind `{}`",
        lease.command.kind
    );
    ensure!(
        lease.command.bundle_id.starts_with("bnd_"),
        "invalid bundle id"
    );
    ensure!(
        lease.command.transport_digest.starts_with("sha256:"),
        "invalid transport digest"
    );
    ensure!(
        lease.command.run_id == lease.run_id,
        "lease command run mismatch"
    );
    ensure!(
        lease.command.session_id == format!("run:{}", lease.run_id),
        "lease command session mismatch"
    );
    ensure!(
        !lease.command.exported_port_id.is_empty(),
        "lease exported Port is empty"
    );
    ensure!(
        lease.command.surface_contract_version == "1",
        "unsupported Surface contract version"
    );
    ensure!(
        lease.command.session_surface.is_object()
            && !lease.command.accepted_session_surfaces.is_empty(),
        "lease Surface negotiation is incomplete"
    );
    ComputationRef::parse(&lease.command.expected_root_computation_ref)
        .context("lease root ComputationRef is invalid")?;
    if let Some(expires_at) = &lease.expires_at {
        let expiry =
            OffsetDateTime::parse(expires_at, &Rfc3339).context("lease expiry is not RFC3339")?;
        let now = OffsetDateTime::from(now);
        ensure!(expiry > now, "lease expired before execution");
    }
    Ok(())
}

fn valid_control_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':'))
}

struct FrontierVerifier;

impl ato_materializer_vm_snapshot::SealedRecordFrontierVerifier for FrontierVerifier {
    fn verify(
        &self,
        reference: &ContentRef,
        objects: &dyn ato_objects::ObjectResolver,
    ) -> Result<(), VmSnapshotError> {
        ato_record_writer::verify_frontier_object(reference, objects)
            .map(|_| ())
            .map_err(|error| VmSnapshotError::InvalidDescriptor(error.to_string()))
    }
}

struct RestorePhysicalConfig<'a> {
    firecracker_work_root: &'a Path,
    slot_id: &'a str,
    hidden_surface_listen: SocketAddr,
    guest_surface_target: SocketAddr,
    tap_host_cidr: &'a str,
}

fn initialize_hosted_run_evolution_authority(
    graph: &ValidatedRuntimeGraph,
    expected_root: &str,
) -> Result<RunEvolutionAuthority> {
    let expected = ComputationRef::parse(expected_root)?;
    let validated = ComputationRef::parse(&graph.report().root_computation_ref)?;
    ensure!(
        expected == validated,
        "validated graph root does not match the assigned lease root"
    );
    Ok(RunEvolutionAuthority::new(
        hosted_evolution_kernel(Arc::new(graph.objects().clone()))?,
        expected,
    ))
}

/// Shared hosted registration point. Browser interaction is available only to
/// an explicit Browser Computation/Port; registering it never mutates legacy
/// source Computations or grants a Browser capability by itself.
fn hosted_evolution_kernel(objects: Arc<dyn ato_objects::ObjectStore>) -> Result<Kernel> {
    let mut kernel = Kernel::new(objects);
    kernel.register(Arc::new(ComposeSemantics::default()))?;
    kernel.register(Arc::new(BrowserComputationSemantics::default()))?;
    kernel.register_protocol(Arc::new(BrowserProtocolSemantics::default()))?;
    Ok(kernel)
}

fn hosted_record_schema_registry() -> Result<ato_record_writer::RecordSchemaRegistry> {
    let mut schemas = ato_record_writer::RecordSchemaRegistry::default();
    register_browser_record_schemas(&mut schemas)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    Ok(schemas)
}

fn start_hosted_browser_runtime(
    graph: &ValidatedRuntimeGraph,
    lease: &ClaimedLease,
    lease_root: &Path,
    workspace: &Path,
    evolution: Arc<RunEvolutionAuthority>,
    api: HttpRunnerApi,
    chrome: Option<&Path>,
    browser_target_url: &str,
) -> Result<Option<HostedBrowserRuntime>> {
    let root = ComputationRef::parse(&graph.report().root_computation_ref)?;
    let Some(binding) = hosted_browser_binding(&root, graph.objects())? else {
        return Ok(None);
    };
    let chrome = chrome.context(
        "Browser-aware Hosted Run requires ATO_BROWSER_CHROME to name an absolute Chrome executable",
    )?;
    let browser_target_url = url::Url::parse(browser_target_url)
        .context("Hosted Browser document endpoint is invalid")?;
    ensure!(
        matches!(browser_target_url.scheme(), "http" | "https")
            && browser_target_url.username().is_empty()
            && browser_target_url.password().is_none(),
        "Hosted Browser document endpoint must be credential-free HTTP(S)"
    );
    let browser_origin = browser_target_url.origin().ascii_serialization();
    ensure!(
        chrome.is_absolute() && chrome.is_file(),
        "Browser-aware Hosted Run requires ATO_BROWSER_CHROME to name an absolute Chrome executable"
    );
    let records_root = lease_root.join("records");
    let pipeline = ato_record_writer::RecordPipeline::start(
        ato_record_writer::RecordWriterConfig::at(&records_root, &lease.run_id),
        Arc::new(graph.objects().clone()),
        hosted_record_schema_registry()?,
    )?;
    let mut registry = AdapterRegistry::default();
    registry.register(Arc::new(BrowserAdapter))?;
    let instance = AdapterInstance {
        instance_id: "hosted.browser".to_owned(),
        adapter_id: ato_adapter_browser::BROWSER_ADAPTER_ID.to_owned(),
        config: serde_json::to_value(BrowserAdapterConfig {
            port_id: binding.port.to_string(),
            expected_origin: browser_origin.clone(),
            allowed_non_text_codes: BTreeSet::new(),
            input_mode: BrowserInputMode::ApplyOnly,
        })?,
    };
    let mut attached = registry.attach_all(
        &[instance],
        &AdapterAttachContext {
            runtime: AdapterContext {
                workspace,
                objects: graph.objects(),
            },
            stylus: pipeline.stylus.clone(),
            observations: Arc::new(IgnoreObservations),
        },
    )?;
    let adapter = Arc::new(Mutex::new(
        attached.pop().context("Browser Adapter did not attach")?,
    ));
    ensure!(
        attached.is_empty(),
        "unexpected additional Browser Adapter session"
    );
    let host_root = lease_root.join("browser");
    fs::create_dir_all(&host_root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&host_root, fs::Permissions::from_mode(0o700))?;
    }
    let bootstrap_path = ato_adapter_browser::runtime_discovery_path(workspace, "hosted.browser");
    let host = match BrowserHost::start(BrowserHostConfig {
        runtime_dir: host_root,
        bootstrap_path,
        target_url: browser_target_url.to_string(),
        chrome: chrome.to_owned(),
        headless: true,
    }) {
        Ok(host) => host,
        Err(error) => {
            if let Ok(mut adapter) = adapter.lock() {
                let context = AdapterContext {
                    workspace,
                    objects: graph.objects(),
                };
                let _ = adapter.detach(&context);
            }
            drop(adapter);
            let _ = pipeline.shutdown();
            return Err(error);
        }
    };
    let ingress = Arc::new(BrowserOperationIngress::new(
        evolution,
        binding.port.clone(),
        AttachedBrowserActuator(Arc::clone(&adapter)),
        RunnerBrowserHeadPersistence {
            api,
            lease_id: lease.id.clone(),
        },
        RunnerBrowserRecordSubmission {
            stylus: pipeline.stylus.clone(),
            port: binding.port,
            stream: format!("browser-{}", lease.id),
            next_local_seq: Arc::new(AtomicU64::new(0)),
        },
    ));
    Ok(Some(HostedBrowserRuntime {
        ingress,
        adapter: Some(adapter),
        host: Some(host),
        pipeline: Some(pipeline),
        objects: Arc::new(graph.objects().clone()),
        workspace: workspace.to_owned(),
    }))
}

fn restore_vm_path(
    graph: &ValidatedRuntimeGraph,
    lease_root: &Path,
    lease_id: &str,
    physical: &RestorePhysicalConfig<'_>,
) -> Result<AcceptedRealization> {
    let root = ComputationRef::parse(&graph.report().root_computation_ref)?;
    let workspace = lease_root.join("workspace");
    fs::create_dir_all(&workspace)?;
    let workspace_policy = WorkspaceCapturePolicy::secure_default();
    let backend_config = FirecrackerBackendConfig {
        // Firecracker's API/vsock Unix socket paths are bounded by SUN_LEN.
        // Object downloads remain lease-scoped, while backend-owned physical
        // sessions live under this short, process-wide restore root.
        work_root: physical.firecracker_work_root.to_owned(),
        slot_id: format!("{}-{}", physical.slot_id, safe_component(lease_id)),
        surface_relay: Some(FirecrackerSurfaceRelayConfig {
            binary: std::env::current_exe()?,
            guest_target: physical.guest_surface_target,
            uds_path: lease_root.join("surface-relay.sock"),
        }),
        tap_host_cidr: Some(physical.tap_host_cidr.to_owned()),
        ..FirecrackerBackendConfig::default()
    };
    let backend = Arc::new(FirecrackerBackend::new(backend_config));
    let capabilities = backend.probe();
    ensure!(
        capabilities.backends.contains("firecracker"),
        "runner Firecracker capability probe failed"
    );

    let mut materializers = MaterializerRegistry::default();
    materializers.register(Arc::new(VmSnapshotMaterializer::new(
        backend,
        Arc::new(FrontierVerifier),
    )))?;
    let actuator_providers = ActuatorProviderRegistry::default();
    let mut contract_verifiers = ContractVerifierRegistry::default();
    contract_verifiers.register(Arc::new(HttpEndpointVerifier))?;
    contract_verifiers.register(Arc::new(WorkspaceContentVerifier))?;
    let context = MaterializerContext {
        objects: graph.objects(),
        adapters: &AdapterRegistry::default(),
        records: &[],
        records_v2: &[],
        replay_anchor: None,
        record_frontier_ref: None,
        workspace: &workspace,
        workspace_policy: &workspace_policy,
        realization: None,
        contracts: &[],
        runner_capabilities: Some(&capabilities),
    };
    let environment = TargetEnvironment {
        id: format!("hosted:{lease_id}"),
        placement: Placement::Hosted,
        trust_boundary: TrustBoundary::TenantIsolated,
    };
    let candidates = graph
        .index()
        .materializations
        .iter()
        .filter(|candidate| candidate.id == VM_SNAPSHOT_MATERIALIZER_ID)
        .map(|candidate| {
            Ok(MaterializationCandidate {
                materializer_id: candidate.id.clone(),
                descriptor_ref: ContentRef::parse(&candidate.descriptor_ref)?,
                environment: environment.clone(),
                context: MaterializerContext {
                    objects: context.objects,
                    adapters: context.adapters,
                    records: context.records,
                    records_v2: context.records_v2,
                    replay_anchor: context.replay_anchor,
                    record_frontier_ref: None,
                    workspace: context.workspace,
                    workspace_policy: context.workspace_policy,
                    realization: None,
                    contracts: context.contracts,
                    runner_capabilities: context.runner_capabilities,
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        !candidates.is_empty(),
        "graph has no VM Materialization candidate"
    );
    let plan = RealizationPlanner {
        target: &root,
        materializers: &materializers,
        actuator_providers: &actuator_providers,
        contract_verifiers: &contract_verifiers,
        port_bindings: &[],
        policy: &PlannerPolicy::default(),
    }
    .plan(candidates)?;
    let selected = plan
        .candidates
        .first()
        .context("Planner returned no path")?;
    ensure!(
        selected.materializer_id == VM_SNAPSHOT_MATERIALIZER_ID,
        "Planner did not select the VM snapshot path"
    );
    let materializer = materializers.get(&selected.materializer_id)?;
    let contracts = materializer.contracts(&selected.descriptor_ref, &context)?;
    let realization = materializer.restore(&selected.descriptor_ref, &context)?;
    ensure!(realization.target() == &root, "restored VM target mismatch");
    let realization: Box<dyn Realization> = Box::new(SurfaceGatewayRealization {
        inner: realization,
        hidden_listen: physical.hidden_surface_listen,
        guest_target: ProxyTarget::Unix(lease_root.join("surface-relay.sock")),
        hidden_proxy: None,
    });
    let contract_context = ContractContext {
        objects: graph.objects(),
        workspace: &workspace,
    };
    accept_candidate(
        realization,
        &contracts,
        &contract_verifiers,
        &contract_context,
    )
    .map_err(Into::into)
}

struct SurfaceGatewayRealization {
    inner: Box<dyn Realization>,
    hidden_listen: SocketAddr,
    guest_target: ProxyTarget,
    hidden_proxy: Option<TcpProxy>,
}

impl Realization for SurfaceGatewayRealization {
    fn target(&self) -> &ComputationRef {
        self.inner.target()
    }

    fn activate(&mut self) -> Result<(), MaterializerError> {
        self.inner.activate()?;
        self.hidden_proxy = Some(
            TcpProxy::start(self.hidden_listen, self.guest_target.clone())
                .map_err(|error| MaterializerError::Operation(error.to_string()))?,
        );
        Ok(())
    }

    fn publish(&mut self) -> Result<(), MaterializerError> {
        self.inner.publish()
    }

    fn wait(&mut self) -> Result<(), MaterializerError> {
        self.inner.wait()
    }

    fn quiesce(&mut self) -> Result<(), MaterializerError> {
        self.hidden_proxy.take();
        self.inner.quiesce()
    }
}

fn safe_component(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .take(48)
        .collect()
}

#[derive(Debug, Deserialize)]
struct ClaimResponse {
    lease: Option<ClaimedLease>,
    #[serde(default = "default_poll_seconds")]
    next_poll_seconds: u64,
}

fn default_poll_seconds() -> u64 {
    2
}

#[derive(Debug, Deserialize)]
struct ClaimedLease {
    id: String,
    run_id: String,
    command: PortableLeaseCommand,
    expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PortableLeaseCommand {
    kind: String,
    bundle_id: String,
    transport_digest: String,
    expected_root_computation_ref: String,
    run_id: String,
    session_id: String,
    exported_port_id: String,
    surface_contract_version: String,
    session_surface: serde_json::Value,
    accepted_session_surfaces: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ControlResponse {
    stop_requested: bool,
}

#[derive(Serialize)]
struct StatusReport<'a> {
    status: &'a str,
}

#[derive(Serialize)]
struct ComputationHeadReport<'a> {
    operation_id: &'a str,
    run_seq: u64,
    head_before: &'a str,
    head_after: &'a str,
}

#[derive(Clone)]
pub struct HttpRunnerApi {
    client: Client,
    base: String,
    runner_id: String,
    token: String,
}

impl HttpRunnerApi {
    pub fn new(base: &str, runner_id: &str, token: &str) -> Result<Self> {
        Ok(Self {
            client: Client::builder().timeout(Duration::from_secs(60)).build()?,
            base: base.trim_end_matches('/').to_owned(),
            runner_id: runner_id.to_owned(),
            token: token.to_owned(),
        })
    }

    fn authorized(&self, request: RequestBuilder) -> RequestBuilder {
        request.bearer_auth(&self.token)
    }

    fn heartbeat(&self, config: &WorkerConfig, active_slots: u32) -> Result<()> {
        self.authorized(self.client.post(format!(
            "{}/v1/runners/{}/heartbeat",
            self.base, self.runner_id
        )))
        .json(&serde_json::json!({
            "capabilities": RUNNER_CAPABILITIES,
            "supported_lease_kinds": [PORTABLE_CAPSULE_LEASE_KIND],
            "supported_session_surfaces": [{
                "kind": "web",
                "profiles": ["ato.web-surface.v1"],
                "transports": ["https"]
            }],
            "public_base_url": config.public_base_url,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "max_slots": 1,
            "active_slots": active_slots,
            "agent_version": env!("CARGO_PKG_VERSION"),
        }))
        .send()?
        .error_for_status()?;
        Ok(())
    }

    fn claim_next(&self) -> Result<ClaimResponse> {
        Ok(self
            .authorized(self.client.get(format!(
                "{}/v1/runners/{}/leases/next?wait_ms=20000",
                self.base, self.runner_id
            )))
            .send()?
            .error_for_status()?
            .json()?)
    }

    fn report_status(&self, lease_id: &str, status: &str) -> Result<()> {
        self.authorized(
            self.client
                .post(format!("{}/v1/runner-leases/{lease_id}/status", self.base)),
        )
        .json(&StatusReport { status })
        .send()?
        .error_for_status()?;
        Ok(())
    }

    fn report_failed(&self, lease_id: &str, message: &str) -> Result<()> {
        self.authorized(
            self.client
                .post(format!("{}/v1/runner-leases/{lease_id}/status", self.base)),
        )
        .json(&serde_json::json!({
            "status": "failed",
            "error": { "code": "realization_failed", "message": truncate(message, 2000) }
        }))
        .send()?
        .error_for_status()?;
        Ok(())
    }

    fn report_ready(
        &self,
        lease_id: &str,
        execution_id: &str,
        ready_url: &str,
        local_port: u16,
    ) -> Result<()> {
        self.authorized(
            self.client
                .post(format!("{}/v1/runner-leases/{lease_id}/ready", self.base)),
        )
        .json(&serde_json::json!({
            "execution_id": execution_id,
            "ready_url": ready_url,
            "local_port": local_port,
        }))
        .send()?
        .error_for_status()?;
        Ok(())
    }

    pub fn persist_computation_head(
        &self,
        lease_id: &str,
        operation_id: &str,
        pending: &ato_kernel::PendingHeadPersistence,
    ) -> Result<()> {
        self.authorized(self.client.post(format!(
            "{}/v1/runner-leases/{lease_id}/computation-head",
            self.base
        )))
        .json(&ComputationHeadReport {
            operation_id,
            run_seq: pending.run_seq,
            head_before: pending.transition.from.as_str(),
            head_after: pending.transition.to.as_str(),
        })
        .send()?
        .error_for_status()?;
        Ok(())
    }

    fn control(&self, lease_id: &str) -> Result<ControlResponse> {
        Ok(self
            .authorized(
                self.client
                    .get(format!("{}/v1/runner-leases/{lease_id}/control", self.base)),
            )
            .send()?
            .error_for_status()?
            .json()?)
    }

    fn report_stopped(&self, lease_id: &str, execution_id: &str) -> Result<()> {
        self.authorized(
            self.client
                .post(format!("{}/v1/runner-leases/{lease_id}/stopped", self.base)),
        )
        .json(&serde_json::json!({ "execution_id": execution_id }))
        .send()?
        .error_for_status()?;
        Ok(())
    }

    fn graph_source(&self, lease: &ClaimedLease) -> Result<LeaseGraphSource> {
        LeaseGraphSource::load(
            self.client.clone(),
            self.base.clone(),
            self.token.clone(),
            &lease.id,
            &lease.command.bundle_id,
            &lease.command.expected_root_computation_ref,
        )
    }
}

fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

struct LeaseGraphSource {
    client: Client,
    base: String,
    token: String,
    lease_id: String,
    graph_id: String,
    index_digest: String,
    index: Vec<u8>,
}

impl LeaseGraphSource {
    fn load(
        client: Client,
        base: String,
        token: String,
        lease_id: &str,
        expected_bundle: &str,
        expected_root: &str,
    ) -> Result<Self> {
        let response = client
            .get(format!("{base}/v1/runner-leases/{lease_id}/object-graph"))
            .bearer_auth(&token)
            .send()?
            .error_for_status()?;
        let headers = GraphIdentityHeaders::from_response(&response)?;
        headers.verify(expected_bundle, expected_root)?;
        let index = response.bytes()?.to_vec();
        Ok(Self {
            client,
            base,
            token,
            lease_id: lease_id.to_owned(),
            graph_id: headers.graph_id,
            index_digest: headers.index_digest,
            index,
        })
    }

    fn index_bytes(&self) -> &[u8] {
        &self.index
    }

    fn index_digest(&self) -> &str {
        &self.index_digest
    }
}

impl RuntimeGraphSource for LeaseGraphSource {
    fn load_index(&self) -> Result<Vec<u8>> {
        Ok(self.index.clone())
    }

    fn load_object(&self, reference: &ContentRef, expected_size: u64) -> Result<Vec<u8>> {
        let response = self
            .client
            .get(format!(
                "{}/v1/runner-leases/{}/object-graph/objects/{}",
                self.base, self.lease_id, reference
            ))
            .bearer_auth(&self.token)
            .send()?
            .error_for_status()?;
        ensure_header(&response, "x-ato-object-graph-id", &self.graph_id)?;
        ensure_header(&response, "x-ato-content-ref", reference.as_str())?;
        let bytes = response.bytes()?.to_vec();
        ensure!(
            bytes.len() as u64 == expected_size,
            "downloaded object size mismatch"
        );
        Ok(bytes)
    }
}

struct GraphIdentityHeaders {
    bundle_id: String,
    root: String,
    graph_id: String,
    index_digest: String,
}

impl GraphIdentityHeaders {
    fn from_response(response: &Response) -> Result<Self> {
        Ok(Self {
            bundle_id: header(response, "x-ato-bundle-id")?,
            root: header(response, "x-ato-root-computation-ref")?,
            graph_id: header(response, "x-ato-object-graph-id")?,
            index_digest: header(response, "x-ato-bundle-index-digest")?,
        })
    }

    fn verify(&self, expected_bundle: &str, expected_root: &str) -> Result<()> {
        ensure!(
            self.bundle_id == expected_bundle,
            "graph belongs to the wrong Bundle"
        );
        ensure!(
            self.root == expected_root,
            "graph root does not match the lease"
        );
        ensure!(!self.graph_id.is_empty(), "graph id is empty");
        ContentRef::parse(&self.index_digest).context("graph index digest is invalid")?;
        Ok(())
    }
}

fn header(response: &Response, name: &str) -> Result<String> {
    response
        .headers()
        .get(name)
        .context("runtime graph response is missing an authorization binding header")?
        .to_str()
        .context("runtime graph authorization header is not ASCII")
        .map(ToOwned::to_owned)
}

fn ensure_header(response: &Response, name: &str, expected: &str) -> Result<()> {
    ensure!(
        header(response, name)? == expected,
        "runtime object binding header mismatch"
    );
    Ok(())
}

struct TcpProxy {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Clone)]
enum ProxyTarget {
    Tcp(SocketAddr),
    Unix(PathBuf),
}

impl TcpProxy {
    fn start(listen: SocketAddr, target: ProxyTarget) -> Result<Self> {
        let listener = TcpListener::bind(listen)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((client, _)) => {
                        let target = target.clone();
                        thread::spawn(move || proxy_connection(client, &target));
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
}

impl Drop for TcpProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn proxy_connection(mut client: TcpStream, target: &ProxyTarget) {
    match target {
        ProxyTarget::Tcp(target) => {
            let Ok(upstream) = TcpStream::connect(target) else {
                return;
            };
            proxy_tcp_pair(&mut client, upstream);
        }
        ProxyTarget::Unix(path) => {
            #[cfg(unix)]
            {
                let Ok(upstream) = UnixStream::connect(path) else {
                    return;
                };
                proxy_tcp_unix_pair(&mut client, upstream);
            }
            #[cfg(not(unix))]
            let _ = path;
        }
    }
}

fn proxy_tcp_pair(client: &mut TcpStream, mut upstream: TcpStream) {
    let Ok(mut client_read) = client.try_clone() else {
        return;
    };
    let Ok(mut upstream_write) = upstream.try_clone() else {
        return;
    };
    let forward = thread::spawn(move || {
        let _ = io::copy(&mut client_read, &mut upstream_write);
        let _ = upstream_write.shutdown(std::net::Shutdown::Write);
    });
    let _ = io::copy(&mut upstream, client);
    let _ = client.shutdown(std::net::Shutdown::Write);
    let _ = forward.join();
}

#[cfg(unix)]
fn proxy_tcp_unix_pair(client: &mut TcpStream, mut upstream: UnixStream) {
    let Ok(mut client_read) = client.try_clone() else {
        return;
    };
    let Ok(mut upstream_write) = upstream.try_clone() else {
        return;
    };
    let forward = thread::spawn(move || {
        let _ = io::copy(&mut client_read, &mut upstream_write);
        let _ = upstream_write.shutdown(std::net::Shutdown::Write);
    });
    let _ = io::copy(&mut upstream, client);
    let _ = client.shutdown(std::net::Shutdown::Write);
    let _ = forward.join();
}

/// Internal helper process launched by `FirecrackerBackend` inside the
/// per-realization network namespace. Its UDS is unique in the host filesystem,
/// while its TCP target may be identical for concurrent snapshot restores.
#[cfg(unix)]
pub fn run_netns_surface_relay(args: &[String]) -> Result<()> {
    let mut listen_unix = None;
    let mut target = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--listen-unix" => {
                index += 1;
                listen_unix = args.get(index).map(PathBuf::from);
            }
            "--target" => {
                index += 1;
                target = args
                    .get(index)
                    .map(|value| value.parse::<SocketAddr>())
                    .transpose()?;
            }
            other => bail!("unknown netns Surface relay argument `{other}`"),
        }
        index += 1;
    }
    let listen_unix = listen_unix.context("--listen-unix is required")?;
    let target = target.context("--target is required")?;
    ensure!(!listen_unix.exists(), "relay UDS already exists");
    let listener = UnixListener::bind(&listen_unix)?;
    for stream in listener.incoming() {
        let Ok(mut client) = stream else {
            continue;
        };
        thread::spawn(move || {
            let Ok(upstream) = connect_tcp_until(target, GUEST_CONNECT_TIMEOUT) else {
                return;
            };
            proxy_unix_tcp_pair(&mut client, upstream);
        });
    }
    Ok(())
}

fn connect_tcp_until(target: SocketAddr, timeout: Duration) -> io::Result<TcpStream> {
    let started = Instant::now();
    loop {
        match TcpStream::connect(target) {
            Ok(stream) => return Ok(stream),
            Err(error) if started.elapsed() < timeout => {
                thread::sleep(GUEST_CONNECT_RETRY_INTERVAL);
                let _ = error;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(not(unix))]
pub fn run_netns_surface_relay(_args: &[String]) -> Result<()> {
    bail!("network namespace Surface relay requires Unix")
}

#[cfg(unix)]
fn proxy_unix_tcp_pair(client: &mut UnixStream, mut upstream: TcpStream) {
    let Ok(mut client_read) = client.try_clone() else {
        return;
    };
    let Ok(mut upstream_write) = upstream.try_clone() else {
        return;
    };
    let forward = thread::spawn(move || {
        let _ = io::copy(&mut client_read, &mut upstream_write);
        let _ = upstream_write.shutdown(std::net::Shutdown::Write);
    });
    let _ = io::copy(&mut upstream, client);
    let _ = client.shutdown(std::net::Shutdown::Write);
    let _ = forward.join();
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use super::*;

    #[derive(Default)]
    struct TestStylus;

    impl Stylus for TestStylus {
        fn record(&self, _candidate: RecordCandidate) -> Result<(), ato_adapter_api::AdapterError> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct TestPersistence;

    impl BrowserHeadPersistence for TestPersistence {
        fn persist(
            &self,
            _operation: &AcceptedBrowserOperation,
        ) -> std::result::Result<(), String> {
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct TestRecords(Arc<Mutex<Vec<AcceptedBrowserOperation>>>);

    impl BrowserRecordSubmission for TestRecords {
        fn submit(&self, operation: &AcceptedBrowserOperation) -> std::result::Result<(), String> {
            self.0.lock().unwrap().push(operation.clone());
            Ok(())
        }
    }

    fn browser_authority() -> Arc<RunEvolutionAuthority> {
        let objects = Arc::new(ato_objects::MemoryObjectStore::default());
        let mut kernel = Kernel::new(objects.clone());
        kernel
            .register(Arc::new(BrowserComputationSemantics::default()))
            .unwrap();
        kernel
            .register_protocol(Arc::new(BrowserProtocolSemantics::default()))
            .unwrap();
        let residual = objects
            .put(
                &ato_browser_semantics::encode_residual(
                    &ato_browser_semantics::BrowserResidualV1 {
                        version: 1,
                        interaction_frontier: None,
                    },
                )
                .unwrap(),
            )
            .unwrap();
        let root = kernel
            .seal(&ato_computation::ComputationObject {
                semantics: SemanticsId::parse(BROWSER_COMPUTATION_SEMANTICS_ID).unwrap(),
                boundary: ato_computation::Boundary::from([(
                    PortId::parse("browser").unwrap(),
                    ato_computation::PortDef {
                        protocol: ProtocolId::parse(BROWSER_PROTOCOL_ID).unwrap(),
                        role: ato_computation::RoleId::parse("controller").unwrap(),
                    },
                )]),
                residual,
            })
            .unwrap();
        Arc::new(RunEvolutionAuthority::new(kernel, root))
    }

    fn e2e_chrome() -> PathBuf {
        std::env::var_os("ATO_BROWSER_E2E_CHROME")
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .or_else(|| {
                [
                    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                    "/usr/bin/google-chrome",
                    "/usr/bin/chromium",
                    "/usr/bin/chromium-browser",
                ]
                .into_iter()
                .map(PathBuf::from)
                .find(|path| path.is_file())
            })
            .expect("P0-B Browser E2E requires Chrome; set ATO_BROWSER_E2E_CHROME")
    }

    fn counter_server() -> (SocketAddr, Arc<AtomicBool>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            const PAGE: &str = "<!doctype html><button id='button' style='width:100vw;height:100vh'>counter</button><output id='counter'>0</output><script>let n=0;const inc=()=>document.querySelector('#counter').textContent=String(++n);document.addEventListener('keydown',e=>{if(e.code==='ArrowRight')inc()});document.addEventListener('click',inc)</script>";
            while !stopped.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = [0_u8; 1024];
                        let _ = stream.read(&mut request);
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            PAGE.len(),
                            PAGE
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => return,
                }
            }
        });
        (address, stop, thread)
    }

    #[test]
    fn browser_e2e_routes_keyboard_and_click_through_authority_then_ack_then_record() {
        let workspace = tempfile::tempdir().unwrap();
        let (address, stop_server, server) = counter_server();
        let origin = format!("http://{address}");
        let objects = Arc::new(ato_objects::MemoryObjectStore::default());
        let mut registry = AdapterRegistry::default();
        registry.register(Arc::new(BrowserAdapter)).unwrap();
        let mut sessions = registry
            .attach_all(
                &[AdapterInstance {
                    instance_id: "hosted.browser".to_owned(),
                    adapter_id: ato_adapter_browser::BROWSER_ADAPTER_ID.to_owned(),
                    config: serde_json::to_value(BrowserAdapterConfig {
                        port_id: "browser".to_owned(),
                        expected_origin: origin.clone(),
                        allowed_non_text_codes: BTreeSet::new(),
                        input_mode: BrowserInputMode::ApplyOnly,
                    })
                    .unwrap(),
                }],
                &AdapterAttachContext {
                    runtime: AdapterContext {
                        workspace: workspace.path(),
                        objects: objects.as_ref(),
                    },
                    stylus: Arc::new(TestStylus),
                    observations: Arc::new(IgnoreObservations),
                },
            )
            .unwrap();
        let adapter = Arc::new(Mutex::new(sessions.pop().unwrap()));
        let host_root = workspace.path().join("browser-host");
        fs::create_dir(&host_root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&host_root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut host = BrowserHost::start(BrowserHostConfig {
            runtime_dir: host_root,
            bootstrap_path: ato_adapter_browser::runtime_discovery_path(
                workspace.path(),
                "hosted.browser",
            ),
            target_url: format!("{origin}/"),
            chrome: e2e_chrome(),
            headless: true,
        })
        .unwrap();
        let records = TestRecords::default();
        let submitted = Arc::clone(&records.0);
        let ingress = BrowserOperationIngress::new(
            browser_authority(),
            PortId::parse("browser").unwrap(),
            AttachedBrowserActuator(Arc::clone(&adapter)),
            TestPersistence,
            records,
        );
        let first = ingress
            .accept(ato_adapter_browser::BrowserEvent::Keyboard {
                kind: ato_adapter_browser::KeyboardKind::KeyDown,
                code: "ArrowRight".to_owned(),
                modifiers: ato_adapter_browser::Modifiers::default(),
            })
            .unwrap();
        assert_eq!(first.run_seq, 1);
        assert_eq!(
            host.evaluate("document.querySelector('#counter').textContent")
                .unwrap()
                .pointer("/result/value")
                .and_then(serde_json::Value::as_str),
            Some("1")
        );
        let second = ingress
            .accept(ato_adapter_browser::BrowserEvent::Click {
                x_normalized: 0.5,
                y_normalized: 0.5,
                button: 0,
            })
            .unwrap();
        assert_eq!(second.run_seq, 2);
        assert_eq!(submitted.lock().unwrap().len(), 2);
        assert_eq!(
            host.evaluate("document.querySelector('#counter').textContent")
                .unwrap()
                .pointer("/result/value")
                .and_then(serde_json::Value::as_str),
            Some("2")
        );
        ingress.freeze().unwrap();
        let context = AdapterContext {
            workspace: workspace.path(),
            objects: objects.as_ref(),
        };
        adapter.lock().unwrap().quiesce(&context).unwrap();
        adapter.lock().unwrap().detach(&context).unwrap();
        host.stop().unwrap();
        stop_server.store(true, Ordering::Release);
        server.join().unwrap();
    }

    fn lease(expires_at: Option<String>) -> ClaimedLease {
        ClaimedLease {
            id: "lease_1".to_owned(),
            run_id: "run_1".to_owned(),
            command: PortableLeaseCommand {
                kind: PORTABLE_CAPSULE_LEASE_KIND.to_owned(),
                bundle_id: "bnd_1".to_owned(),
                transport_digest: "sha256:ignored-by-graph-runtime".to_owned(),
                expected_root_computation_ref: format!("blake3:{}", "11".repeat(32)),
                run_id: "run_1".to_owned(),
                session_id: "run:run_1".to_owned(),
                exported_port_id: "web".to_owned(),
                surface_contract_version: "1".to_owned(),
                session_surface: serde_json::json!({"kind":"web"}),
                accepted_session_surfaces: Vec::new(),
            },
            expires_at,
        }
    }

    #[test]
    fn expired_lease_is_rejected_before_graph_access() {
        let expired = "2020-01-01T00:00:00Z".to_owned();
        assert!(validate_lease(&lease(Some(expired)), SystemTime::now()).is_err());
    }

    #[test]
    fn graph_from_wrong_bundle_is_rejected() {
        let headers = GraphIdentityHeaders {
            bundle_id: "bnd_other".to_owned(),
            root: format!("blake3:{}", "11".repeat(32)),
            graph_id: "graph_1".to_owned(),
            index_digest: format!("blake3:{}", "22".repeat(32)),
        };
        assert!(headers.verify("bnd_1", &headers.root).is_err());
    }

    #[test]
    fn graph_with_wrong_root_is_rejected() {
        let headers = GraphIdentityHeaders {
            bundle_id: "bnd_1".to_owned(),
            root: format!("blake3:{}", "11".repeat(32)),
            graph_id: "graph_1".to_owned(),
            index_digest: format!("blake3:{}", "22".repeat(32)),
        };
        assert!(
            headers
                .verify("bnd_1", &format!("blake3:{}", "33".repeat(32)))
                .is_err()
        );
    }

    #[test]
    fn surface_listener_must_be_hidden_loopback() {
        let config = WorkerConfig {
            api_base: "https://staging.api.ato.run".to_owned(),
            runner_id: "runner".to_owned(),
            runner_token: "token".to_owned(),
            runner_credentials_file: None,
            public_base_url: "https://runner.example".to_owned(),
            work_root: PathBuf::from(".tmp/worker-test"),
            surface_listen: "0.0.0.0:8420".parse().unwrap(),
            hidden_surface_listen: "127.0.0.1:18420".parse().unwrap(),
            surface_target: "127.0.0.1:8080".parse().unwrap(),
            tap_host_cidr: "172.16.0.1/24".to_owned(),
            slot_id: "0".to_owned(),
            browser_chrome: None,
            once: true,
        };
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn loads_existing_runner_credentials_without_copying_the_token_to_args() {
        let directory = tempfile::tempdir().unwrap();
        let credentials = directory.path().join("credentials.json");
        fs::write(
            &credentials,
            br#"{"api_base":"https://staging.api.ato.run","runner_id":"runner-file","runner_token":"token-file"}"#,
        )
        .unwrap();
        let mut config = WorkerConfig {
            api_base: "https://staging.api.ato.run".to_owned(),
            runner_id: String::new(),
            runner_token: String::new(),
            runner_credentials_file: Some(credentials),
            public_base_url: "https://runner.example".to_owned(),
            work_root: directory.path().join("work"),
            surface_listen: "127.0.0.1:8420".parse().unwrap(),
            hidden_surface_listen: "127.0.0.1:18420".parse().unwrap(),
            surface_target: "172.30.0.2:38865".parse().unwrap(),
            tap_host_cidr: "172.30.0.1/24".to_owned(),
            slot_id: "0".to_owned(),
            browser_chrome: None,
            once: true,
        };
        resolve_runner_credentials(&mut config).unwrap();
        assert_eq!(config.runner_id, "runner-file");
        assert_eq!(config.runner_token, "token-file");
    }

    #[test]
    fn surface_is_unreachable_until_publication_proxy_starts() {
        let target_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = target_listener.local_addr().unwrap();
        let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
        let published = reservation.local_addr().unwrap();
        drop(reservation);
        assert!(TcpStream::connect(published).is_err());

        let upstream = thread::spawn(move || {
            let (mut stream, _) = target_listener.accept().unwrap();
            let mut byte = [0_u8; 1];
            stream.read_exact(&mut byte).unwrap();
            stream.write_all(&byte).unwrap();
        });
        let proxy = TcpProxy::start(published, ProxyTarget::Tcp(target)).unwrap();
        let mut stream = TcpStream::connect(published).unwrap();
        stream.write_all(b"x").unwrap();
        let mut echoed = [0_u8; 1];
        stream.read_exact(&mut echoed).unwrap();
        assert_eq!(echoed, *b"x");
        drop(stream);
        drop(proxy);
        upstream.join().unwrap();
        assert!(TcpStream::connect(published).is_err());
    }

    #[test]
    fn ready_receipt_reports_the_published_proxy_port() {
        let config = WorkerConfig {
            api_base: "https://staging.api.ato.run".to_owned(),
            runner_id: "runner_1".to_owned(),
            runner_token: "token".to_owned(),
            runner_credentials_file: None,
            public_base_url: "https://runner.example".to_owned(),
            work_root: PathBuf::from(".tmp/worker"),
            surface_listen: "127.0.0.1:8420".parse().unwrap(),
            hidden_surface_listen: "127.0.0.1:18420".parse().unwrap(),
            surface_target: "172.30.0.2:38865".parse().unwrap(),
            tap_host_cidr: "172.30.0.1/24".to_owned(),
            slot_id: "0".to_owned(),
            browser_chrome: None,
            once: true,
        };
        assert_eq!(ready_local_port(&config), 8420);
        assert_ne!(
            ready_local_port(&config),
            config.hidden_surface_listen.port()
        );
    }

    #[test]
    fn heartbeat_advertises_dispatch_and_vm_requirements() {
        assert!(RUNNER_CAPABILITIES.contains(&"execution_abi=process"));
        assert!(RUNNER_CAPABILITIES.contains(&"isolation=untrusted-v1"));
        assert!(RUNNER_CAPABILITIES.contains(&"materializer=ato.materialize.vm.snapshot@1"));
        assert!(RUNNER_CAPABILITIES.contains(&"backend=firecracker"));
    }
}
