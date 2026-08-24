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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail, ensure};
use ato_adapter_api::{ActuatorProviderRegistry, AdapterRegistry, WorkspaceCapturePolicy};
use ato_computation::{ComputationRef, ContentRef};
use ato_contracts::{HttpEndpointVerifier, WorkspaceContentVerifier};
use ato_materializer_api::{
    AcceptedRealization, ContractContext, ContractVerifierRegistry, Materializer,
    MaterializerContext, MaterializerError, MaterializerRegistry, Realization, RunnerCapabilities,
    accept_candidate,
};
use ato_materializer_vm_snapshot::{
    ActiveFirecrackerRealization, FirecrackerActiveVmCaptureSource, FirecrackerBackend,
    FirecrackerBackendConfig, FirecrackerIngressGate, FirecrackerRecordCaptureBarrier,
    FirecrackerRecordCaptureLease, FirecrackerSurfaceRelayConfig, VM_SNAPSHOT_MATERIALIZER_ID,
    VmSnapshotError, VmSnapshotMaterializer, load_descriptor,
};
use ato_objects::{
    GraphMaterialization, GraphRestoreCapability, ObjectResolver, ObjectStore, read_exact_object,
};
use ato_planner::{
    MaterializationCandidate, Placement, PlannerPolicy, RealizationPlanner, TargetEnvironment,
    TrustBoundary,
};
use ato_record_writer::{
    AsyncRecordStylus, RecordPipeline, RecordSchemaRegistry, RecordWriterConfig,
};
use ato_runtime_object_graph::{
    GraphDownloadExpectation, ObjectGraphIndexV1, RuntimeGraphSource, ValidatedRuntimeGraph,
    VisibilityPolicy, build_runtime_object_graph_index, download_and_validate_graph,
    standard_reference_registry, vm_capture_refs,
};
use clap::Parser;
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

const PORTABLE_CAPSULE_LEASE_KIND: &str = "portable_capsule_v2";
const RUNNER_CAPABILITIES: &[&str] = &[
    "execution_abi=process",
    "isolation=untrusted-v1",
    "materializer=ato.materialize.vm.snapshot@1",
    "backend=firecracker",
    "capture=current-point-vm-v1",
];
const ACTIVE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const GUEST_CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const GUEST_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Default)]
struct IngressState {
    frozen: AtomicBool,
    active_connections: AtomicUsize,
}

#[derive(Clone)]
struct WorkerIngressGate {
    state: Arc<IngressState>,
}

impl FirecrackerIngressGate for WorkerIngressGate {
    fn freeze(&mut self) -> std::result::Result<(), VmSnapshotError> {
        self.state.frozen.store(true, Ordering::Release);
        Ok(())
    }

    fn quiesce(&mut self) -> std::result::Result<(), VmSnapshotError> {
        let deadline = Instant::now() + Duration::from_secs(15);
        while self.state.active_connections.load(Ordering::Acquire) > 0 {
            if Instant::now() >= deadline {
                return Err(VmSnapshotError::Backend(
                    "interaction ingress did not quiesce before capture".to_owned(),
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    }

    fn unfreeze(&mut self) -> std::result::Result<(), VmSnapshotError> {
        self.state.frozen.store(false, Ordering::Release);
        Ok(())
    }
}

struct WorkerRecordCaptureBarrier {
    inner: ato_record_writer::CaptureBarrier,
    calls: Arc<AtomicUsize>,
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
        self.calls.fetch_add(1, Ordering::AcqRel);
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
    let barrier = Arc::new(WorkerRecordCaptureBarrier {
        inner: barrier,
        calls: Arc::new(AtomicUsize::new(0)),
    });
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
            let firecracker_work_root = self.config.work_root.join("fc");
            let physical = RestorePhysicalConfig {
                firecracker_work_root: &firecracker_work_root,
                slot_id: &self.config.slot_id,
                hidden_surface_listen: self.config.hidden_surface_listen,
                guest_surface_target: self.config.surface_target,
                tap_host_cidr: &self.config.tap_host_cidr,
            };
            let mut running = restore_vm_path(graph, &lease_root, &lease.id, &physical)?;
            self.api.report_status(&lease.id, "running")?;

            // The externally reachable listener does not exist until the VM is
            // active, every Contract passed, and the Realization published.
            let proxy = TcpProxy::start_with_ingress(
                self.config.surface_listen,
                ProxyTarget::Tcp(self.config.hidden_surface_listen),
                Some(Arc::clone(&running.ingress)),
            )?;
            let execution_id = format!("vm:{}:{}", lease.run_id, lease.id);
            self.api.report_ready(
                &lease.id,
                &execution_id,
                &self.config.public_base_url,
                ready_local_port(&self.config),
            )?;
            let mut last_heartbeat = Instant::now();
            let mut observed_captures = BTreeSet::new();
            loop {
                let control = self.api.control(&lease.id)?;
                if control.stop_requested {
                    drop(proxy);
                    running.quiesce()?;
                    self.api.report_stopped(&lease.id, &execution_id)?;
                    return Ok(());
                }
                if let Some(capture) = control.capture
                    && observed_captures.insert(capture.request_id.clone())
                {
                    let started = Instant::now();
                    let outcome = (|| {
                        let point = running.capture_current_point()?;
                        self.api.report_capture_uploading(
                            &lease.id,
                            &capture.request_id,
                            started.elapsed(),
                            point.capture_barrier_count,
                        )?;
                        self.api.upload_capture_graph(
                            &lease.id,
                            &capture,
                            &point,
                            running.graph.objects(),
                        )
                    })();
                    if let Err(error) = outcome {
                        let _ = self.api.report_capture_failed(
                            &lease.id,
                            &capture.request_id,
                            "current_point_capture_failed",
                        );
                        eprintln!(
                            "current-point capture {} failed: {:#}",
                            capture.request_id, error
                        );
                    }
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

struct RunningHostedRealization {
    accepted: Option<AcceptedRealization>,
    graph: ValidatedRuntimeGraph,
    target: ComputationRef,
    materializer: Arc<VmSnapshotMaterializer>,
    source_descriptor: ContentRef,
    capabilities: RunnerCapabilities,
    workspace: PathBuf,
    ingress: Arc<IngressState>,
    barrier_calls: Arc<AtomicUsize>,
    _record_stylus: Arc<AsyncRecordStylus>,
}

impl RunningHostedRealization {
    fn capture_current_point(&mut self) -> Result<CapturedPoint> {
        let adapters = AdapterRegistry::default();
        let workspace_policy = WorkspaceCapturePolicy::secure_default();
        let base_context = MaterializerContext {
            objects: self.graph.objects(),
            adapters: &adapters,
            records: &[],
            records_v2: &[],
            replay_anchor: None,
            record_frontier_ref: None,
            workspace: &self.workspace,
            workspace_policy: &workspace_policy,
            realization: None,
            contracts: &[],
            runner_capabilities: Some(&self.capabilities),
        };
        let contracts = self
            .materializer
            .contracts(&self.source_descriptor, &base_context)?;
        let capture_context = MaterializerContext {
            contracts: &contracts,
            ..base_context
        };
        let calls_before = self.barrier_calls.load(Ordering::Acquire);
        let descriptor = self.materializer.encode(&self.target, &capture_context)?;
        let calls_after = self.barrier_calls.load(Ordering::Acquire);
        ensure!(
            calls_after == calls_before + 1,
            "current-point capture did not cross exactly one Capture Barrier"
        );

        let mut materializations = self.graph.index().materializations.clone();
        materializations.retain(|item| item.id != VM_SNAPSHOT_MATERIALIZER_ID);
        materializations.push(GraphMaterialization {
            id: VM_SNAPSHOT_MATERIALIZER_ID.to_owned(),
            descriptor_ref: descriptor.to_string(),
            restore_capability: GraphRestoreCapability::Supported,
        });
        let references = standard_reference_registry()?;
        let index = build_runtime_object_graph_index(
            &self.target,
            &materializations,
            self.graph.objects(),
            &references,
            VisibilityPolicy::Private,
        )?;
        let (verified_descriptor, frontier) = vm_capture_refs(&index, self.graph.objects())?
            .context("captured graph omitted VM descriptor or RecordFrontier")?;
        ensure!(
            verified_descriptor == descriptor,
            "captured VM descriptor changed"
        );
        scan_vm_capture(&verified_descriptor, self.graph.objects())?;
        Ok(CapturedPoint {
            index,
            descriptor,
            frontier,
            capture_barrier_count: calls_after - calls_before,
        })
    }

    fn quiesce(mut self) -> Result<()> {
        self.accepted
            .take()
            .context("hosted Realization already stopped")?
            .quiesce()
            .map_err(Into::into)
    }
}

struct CapturedPoint {
    index: ObjectGraphIndexV1,
    descriptor: ContentRef,
    frontier: ContentRef,
    capture_barrier_count: usize,
}

fn scan_vm_capture(descriptor_ref: &ContentRef, objects: &dyn ObjectResolver) -> Result<()> {
    const SCAN_PATTERNS: &[&[u8]] = &[
        b"-----begin private key",
        b"authorization: bearer",
        b"cloudflare_api_token",
        b"cf_api_token",
        b"ato_runner_token",
        b"aws_access_key_id",
        b"aws_secret_access_key",
        b"x-api-key:",
        b"set-cookie:",
        b"session_token",
        b"sessionid=",
        b"user_notes",
        b"notes.db",
    ];
    let descriptor = load_descriptor(descriptor_ref, objects)?;
    let overlap = SCAN_PATTERNS
        .iter()
        .map(|pattern| pattern.len())
        .max()
        .unwrap_or(1)
        .saturating_sub(1);
    for artifact in descriptor.artifacts {
        let mut tail = Vec::new();
        for chunk in artifact.chunks {
            let reference = ContentRef::parse(&chunk.content_ref)?;
            let bytes = read_exact_object(objects, &reference, chunk.size, 64 * 1024 * 1024)?;
            let mut window = Vec::with_capacity(tail.len() + bytes.len());
            window.extend_from_slice(&tail);
            window.extend(bytes.iter().map(u8::to_ascii_lowercase));
            if SCAN_PATTERNS.iter().any(|pattern| {
                window
                    .windows(pattern.len())
                    .any(|candidate| candidate == *pattern)
            }) {
                bail!(
                    "new VM artifact failed bounded private-data scan ({:?})",
                    artifact.role
                );
            }
            let keep = overlap.min(window.len());
            tail.clear();
            tail.extend_from_slice(&window[window.len() - keep..]);
        }
    }
    Ok(())
}

fn restore_vm_path(
    graph: ValidatedRuntimeGraph,
    lease_root: &Path,
    lease_id: &str,
    physical: &RestorePhysicalConfig<'_>,
) -> Result<RunningHostedRealization> {
    let root = ComputationRef::parse(&graph.report().root_computation_ref)?;
    let workspace = lease_root.join("workspace");
    fs::create_dir_all(&workspace)?;
    let adapters = AdapterRegistry::default();
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
    let graph_objects = Arc::new(graph.objects().clone());
    let RecordPipeline {
        stylus,
        barrier,
        published: _,
    } = RecordPipeline::start(
        RecordWriterConfig::at(lease_root.join("records"), safe_component(lease_id)),
        Arc::clone(&graph_objects) as Arc<dyn ObjectStore>,
        RecordSchemaRegistry::default(),
    )?;
    let barrier_calls = Arc::new(AtomicUsize::new(0));
    let capture_barrier = Arc::new(WorkerRecordCaptureBarrier {
        inner: barrier,
        calls: Arc::clone(&barrier_calls),
    });
    let ingress = Arc::new(IngressState::default());
    let backend = Arc::new(FirecrackerBackend::with_restored_capture(
        backend_config,
        capture_barrier,
        lease_root.join("captures"),
        Box::new(WorkerIngressGate {
            state: Arc::clone(&ingress),
        }),
    ));
    let capabilities = backend.probe();
    ensure!(
        capabilities.backends.contains("firecracker"),
        "runner Firecracker capability probe failed"
    );

    let mut materializers = MaterializerRegistry::default();
    let vm_materializer = Arc::new(VmSnapshotMaterializer::new(
        backend,
        Arc::new(FrontierVerifier),
    ));
    materializers.register(vm_materializer.clone())?;
    let actuator_providers = ActuatorProviderRegistry::default();
    let mut contract_verifiers = ContractVerifierRegistry::default();
    contract_verifiers.register(Arc::new(HttpEndpointVerifier))?;
    contract_verifiers.register(Arc::new(WorkspaceContentVerifier))?;
    let context = MaterializerContext {
        objects: graph.objects(),
        adapters: &adapters,
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
    let source_descriptor = selected.descriptor_ref.clone();
    let accepted = accept_candidate(
        realization,
        &contracts,
        &contract_verifiers,
        &contract_context,
    )?;
    Ok(RunningHostedRealization {
        accepted: Some(accepted),
        graph,
        target: root,
        materializer: vm_materializer,
        source_descriptor,
        capabilities,
        workspace,
        ingress,
        barrier_calls,
        _record_stylus: stylus,
    })
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
    capture: Option<CaptureInstruction>,
}

#[derive(Debug, Clone, Deserialize)]
struct CaptureInstruction {
    request_id: String,
    prepare_url: String,
}

#[derive(Serialize)]
struct StatusReport<'a> {
    status: &'a str,
}

#[derive(Debug, Deserialize)]
struct CaptureGraphResponse {
    graph_id: String,
    root_computation_ref: String,
    bundle_index_digest: String,
    status: String,
    object_count: usize,
    logical_bytes: u64,
    bundle_id: Option<String>,
    rejection_code: Option<String>,
    #[serde(default)]
    uploads: Vec<CaptureUploadInstruction>,
}

#[derive(Debug, Deserialize)]
struct CaptureUploadInstruction {
    content_ref: String,
    size_bytes: u64,
    upload_url: String,
    upload_direct: bool,
    upload_headers: std::collections::BTreeMap<String, String>,
}

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

    fn report_capture_uploading(
        &self,
        lease_id: &str,
        capture_id: &str,
        capture_elapsed: Duration,
        barrier_count: usize,
    ) -> Result<()> {
        ensure!(
            barrier_count == 1,
            "capture crossed an invalid barrier count"
        );
        self.authorized(self.client.post(format!(
            "{}/v1/runner-leases/{lease_id}/captures/{capture_id}/status",
            self.base
        )))
        .json(&serde_json::json!({
            "status": "uploading",
            "capture_ms": capture_elapsed.as_millis().min(u128::from(u32::MAX)) as u32,
            "capture_barrier_count": 1,
        }))
        .send()?
        .error_for_status()?;
        Ok(())
    }

    fn report_capture_failed(
        &self,
        lease_id: &str,
        capture_id: &str,
        error_code: &str,
    ) -> Result<()> {
        self.authorized(self.client.post(format!(
            "{}/v1/runner-leases/{lease_id}/captures/{capture_id}/status",
            self.base
        )))
        .json(&serde_json::json!({
            "status": "failed",
            "error_code": error_code,
        }))
        .send()?
        .error_for_status()?;
        Ok(())
    }

    fn upload_capture_graph(
        &self,
        lease_id: &str,
        instruction: &CaptureInstruction,
        point: &CapturedPoint,
        objects: &dyn ObjectResolver,
    ) -> Result<()> {
        let expected_prepare = format!(
            "/v1/runner-leases/{lease_id}/captures/{}/object-graph/prepare",
            instruction.request_id
        );
        ensure!(
            instruction.prepare_url == expected_prepare,
            "capture prepare route is not bound to the claimed lease"
        );
        let graph_base = instruction
            .prepare_url
            .strip_suffix("/prepare")
            .context("capture prepare route is malformed")?;
        let index_digest = point.index.digest()?;
        let prepare = self
            .authorized(
                self.client
                    .post(format!("{}{}", self.base, instruction.prepare_url)),
            )
            .json(&serde_json::json!({
                "idempotency_key": instruction.request_id,
                "index_digest": index_digest,
                "index": point.index,
            }))
            .send()?
            .error_for_status()?
            .json::<CaptureGraphResponse>()?;
        ensure_capture_graph_response(&prepare, point, &index_digest)?;

        if prepare.status == "uploading" {
            let expected = point
                .index
                .objects
                .iter()
                .map(|object| (object.content_ref.as_str(), object.size_bytes))
                .collect::<std::collections::BTreeMap<_, _>>();
            let actual = prepare
                .uploads
                .iter()
                .map(|upload| (upload.content_ref.as_str(), upload.size_bytes))
                .collect::<std::collections::BTreeMap<_, _>>();
            ensure!(expected == actual, "capture upload closure mismatch");
            for upload in &prepare.uploads {
                let reference = ContentRef::parse(&upload.content_ref)?;
                let bytes =
                    read_exact_object(objects, &reference, upload.size_bytes, 64 * 1024 * 1024)?;
                let upload_url = if upload.upload_url.starts_with('/') {
                    format!("{}{}", self.base, upload.upload_url)
                } else {
                    upload.upload_url.clone()
                };
                let mut request = self.client.put(upload_url).body(bytes);
                for (name, value) in &upload.upload_headers {
                    let name = HeaderName::from_bytes(name.to_ascii_lowercase().as_bytes())?;
                    let value = HeaderValue::from_str(value)?;
                    request = request.header(name, value);
                }
                if !upload.upload_direct || upload.upload_url.starts_with('/') {
                    request = self.authorized(request);
                }
                request.send()?.error_for_status()?;
            }
        }

        let mut status = if prepare.status == "ready" {
            prepare
        } else {
            self.authorized(self.client.post(format!(
                "{}{}",
                self.base,
                format!("{graph_base}/finalize")
            )))
            .send()?
            .error_for_status()?
            .json::<CaptureGraphResponse>()?
        };
        for _ in 0..300 {
            ensure_capture_graph_response(&status, point, &index_digest)?;
            match status.status.as_str() {
                "ready" => {
                    ensure!(status.bundle_id.is_some(), "ready capture omitted Bundle");
                    eprintln!(
                        "current-point capture ready: request={} graph={} descriptor={} frontier={}",
                        instruction.request_id, status.graph_id, point.descriptor, point.frontier
                    );
                    return Ok(());
                }
                "rejected" | "failed" => bail!(
                    "capture graph validation rejected ({:?})",
                    status.rejection_code
                ),
                "validating" => thread::sleep(Duration::from_secs(2)),
                other => bail!("capture graph entered unexpected state `{other}`"),
            }
            status = self
                .authorized(self.client.get(format!("{}{}", self.base, graph_base)))
                .send()?
                .error_for_status()?
                .json::<CaptureGraphResponse>()?;
        }
        bail!("capture graph validation timed out")
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

fn ensure_capture_graph_response(
    response: &CaptureGraphResponse,
    point: &CapturedPoint,
    index_digest: &str,
) -> Result<()> {
    ensure!(
        response.root_computation_ref == point.index.root_computation_ref,
        "capture graph root mismatch"
    );
    ensure!(
        response.bundle_index_digest == index_digest,
        "capture graph index mismatch"
    );
    ensure!(
        response.object_count == point.index.objects.len(),
        "capture graph object count mismatch"
    );
    ensure!(
        response.logical_bytes == point.index.logical_bytes()?,
        "capture graph byte count mismatch"
    );
    ensure!(!response.graph_id.is_empty(), "capture graph id is empty");
    Ok(())
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
        Self::start_with_ingress(listen, target, None)
    }

    fn start_with_ingress(
        listen: SocketAddr,
        target: ProxyTarget,
        ingress: Option<Arc<IngressState>>,
    ) -> Result<Self> {
        let listener = TcpListener::bind(listen)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((client, _)) => {
                        let connection = match ingress.as_ref() {
                            Some(state) if state.frozen.load(Ordering::Acquire) => continue,
                            Some(state) => {
                                state.active_connections.fetch_add(1, Ordering::AcqRel);
                                if state.frozen.load(Ordering::Acquire) {
                                    state.active_connections.fetch_sub(1, Ordering::AcqRel);
                                    continue;
                                }
                                Some(ConnectionLease {
                                    state: Arc::clone(state),
                                })
                            }
                            None => None,
                        };
                        let target = target.clone();
                        thread::spawn(move || {
                            let _connection = connection;
                            proxy_connection(client, &target);
                        });
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

struct ConnectionLease {
    state: Arc<IngressState>,
}

impl Drop for ConnectionLease {
    fn drop(&mut self) {
        self.state.active_connections.fetch_sub(1, Ordering::AcqRel);
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
        assert!(RUNNER_CAPABILITIES.contains(&"capture=current-point-vm-v1"));
    }

    #[test]
    fn control_decodes_current_point_capture_instruction() {
        let control: ControlResponse = serde_json::from_value(serde_json::json!({
            "stop_requested": false,
            "capture": {
                "request_id": "cap_1",
                "prepare_url": "/v1/runner-leases/lease_1/captures/cap_1/object-graph/prepare"
            }
        }))
        .unwrap();
        let capture = control.capture.unwrap();
        assert_eq!(capture.request_id, "cap_1");
        assert!(capture.prepare_url.ends_with("/object-graph/prepare"));
    }

    #[test]
    fn ingress_gate_rejects_new_connections_while_capture_is_frozen() {
        let state = Arc::new(IngressState::default());
        let target_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let target = target_listener.local_addr().unwrap();
        let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
        let published = reservation.local_addr().unwrap();
        drop(reservation);
        let _proxy = TcpProxy::start_with_ingress(
            published,
            ProxyTarget::Tcp(target),
            Some(Arc::clone(&state)),
        )
        .unwrap();
        let mut gate = WorkerIngressGate {
            state: Arc::clone(&state),
        };
        gate.freeze().unwrap();
        let mut client = TcpStream::connect(published).unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();
        client.write_all(b"x").unwrap();
        let mut reply = [0_u8; 1];
        assert!(client.read_exact(&mut reply).is_err());
        assert!(target_listener.set_nonblocking(true).is_ok());
        assert!(target_listener.accept().is_err());
        gate.unfreeze().unwrap();
    }
}
