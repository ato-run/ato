//! Connected control-plane consumer for hosted physical Realizations.
//!
//! This is deliberately an application component, not a revival of the old
//! `ato runner serve` command. It consumes the current lease protocol, uses the
//! shared runtime graph validator, asks `RealizationPlanner` to select a path,
//! and reports ready only after Contract acceptance and Surface publication.

#![forbid(unsafe_code)]

mod activity_controller;
pub mod runtime_launch;
mod slot_state;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail, ensure};
#[cfg(test)]
use ato_adapter_api::Stylus;
use ato_adapter_api::{
    ActuatorProviderRegistry, AdapterAttachContext, AdapterContext, AdapterInstance,
    AdapterRegistry, AttachedAdapter, IgnoreObservations, LiveOperation, LiveOperationDispatcher,
    WorkspaceCapturePolicy,
};
use ato_adapter_browser::{
    BROWSER_PROTOCOL_ID, BrowserAdapter, BrowserAdapterConfig, BrowserChannelScope,
    BrowserInputMode, BrowserSurfaceTracker, RawWebMcpSnapshotV1,
    register_record_schemas as register_browser_record_schemas,
};
use ato_adapter_workspace::restore_workspace;
use ato_browser_host::{BrowserHost, BrowserHostConfig};
use ato_browser_semantics::{
    AcceptedBrowserOperation, BROWSER_COMPUTATION_SEMANTICS_ID, BrowserComputationSemantics,
    BrowserHeadPersistence, BrowserOperationActuator, BrowserOperationIngress,
    BrowserOperationRetryStage, BrowserProtocolSemantics, BrowserRecordSubmission,
};
use ato_compose::{COMPOSE_SEMANTICS_ID, ComposeSemantics, decode_composite_residual};
use ato_computation::{ComputationRef, ContentRef, PortId, ProtocolId, SemanticsId};
use ato_contracts::{HttpEndpointVerifier, WorkspaceContentVerifier};
use ato_kernel::{EvolutionError, Kernel, RunEvolutionAuthority};
use ato_materializer_api::{
    AcceptedRealization, ContractContext, ContractVerifierRegistry, MaterializerContext,
    MaterializerError, MaterializerRegistry, OperationReplayRuntime, Realization,
    RealizationDriver, ReplayRuntime, accept_candidate,
};
use ato_materializer_replay::{
    REPLAY_MATERIALIZER_ID, REPLAY_MATERIALIZER_V2_ID, ReplayMaterializer, ReplayMaterializerV2,
};
use ato_materializer_vm_snapshot::{
    ActiveFirecrackerRealization, FirecrackerActiveVmCaptureSource, FirecrackerBackend,
    FirecrackerBackendConfig, FirecrackerRecordCaptureBarrier, FirecrackerRecordCaptureLease,
    FirecrackerSurfaceRelayConfig, VM_SNAPSHOT_MATERIALIZER_ID, VmSnapshotError,
    VmSnapshotMaterializer,
};
use ato_objects::{
    ObjectResolver, ObjectStore, RecordCandidate, RecordEnvelope, RecordEnvelopeV2,
    read_exact_object, resolve_computation,
};
use ato_planner::{
    MaterializationCandidate, Placement, PlannerPolicy, RealizationPlanner, TargetEnvironment,
    TrustBoundary,
};
use ato_runtime_object_graph::{
    GraphDownloadExpectation, ObjectGraphIndexV1, RuntimeGraphSource, ValidatedRuntimeGraph,
    download_and_validate_graph,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use clap::Parser;
use hmac::{Hmac, Mac};
use reqwest::blocking::{Client, RequestBuilder, Response};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tungstenite::handshake::server::{
    ErrorResponse, Request as WebSocketRequest, Response as WebSocketResponse,
};
use tungstenite::{Message, accept_hdr};

use activity_controller::{
    ActivityControllerEvent, ActivityControllerPageConfig, ActivityControllerServer,
};

const PORTABLE_CAPSULE_LEASE_KIND: &str = "portable_capsule_v2";
const ACTIVITY_BROWSER_EXECUTOR_LEASE_KIND: &str = "activity_browser_executor_v0";
/// Capabilities every Runner has, regardless of host.
///
/// The VM-snapshot materializer and Firecracker backend are deliberately NOT
/// here: advertising them unconditionally is a lie the control plane acts on,
/// routing a VM-snapshot Capsule to a Runner that cannot restore one.
const BASE_RUNNER_CAPABILITIES: &[&str] = &[
    "execution_abi=process",
    runtime_launch::lease::RUNTIME_LAUNCH_LEASE_KIND,
    "isolation=untrusted-v1",
];

/// Capabilities that exist only when this host can actually run Firecracker.
const FIRECRACKER_RUNNER_CAPABILITIES: &[&str] = &[
    "materializer=ato.materialize.vm.snapshot@1",
    "backend=firecracker",
    "activity-coop-trace-v0",
];

fn runner_capabilities(
    firecracker_configured: bool,
    oci_available: bool,
    persistent_volumes: bool,
    network_controls: bool,
    fixed_tcp: bool,
) -> Vec<&'static str> {
    let mut capabilities = BASE_RUNNER_CAPABILITIES.to_vec();
    if firecracker_configured {
        capabilities.extend_from_slice(FIRECRACKER_RUNNER_CAPABILITIES);
    }
    if oci_available {
        capabilities.push("execution_abi=oci");
        // A group is a capability of the OCI evaluator, not a separate ABI:
        // it is offered exactly where single-container OCI is, by a build that
        // understands ato.runtime-launch-spec.v2.
        capabilities.push(ato_ipc::oci_service_group::OCI_SERVICE_GROUP_RUNTIME_FEATURE);
        // Volumes are their own feature: a group Runner is not a volume
        // Runner until it understands v3 AND has a writable volume store.
        if persistent_volumes {
            capabilities.push(runtime_launch::volume::RUNNER_PERSISTENT_VOLUME_FEATURE);
        }
        if network_controls {
            capabilities.push("network=ato.tcp-egress@1");
        }
        if network_controls && fixed_tcp {
            capabilities.push("network=ato.fixed-tcp@1");
        }
    }
    capabilities
}
const ACTIVE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const ACTIVITY_FRAME_REFRESH_INTERVAL: Duration = Duration::from_millis(250);
const ACTIVITY_FRAME_RETRY_INTERVAL: Duration = Duration::from_secs(5);
#[cfg(unix)]
const GUEST_CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(250);
#[cfg(unix)]
const GUEST_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const TERMINAL_REPORT_RETRY_DELAYS: [Duration; 3] = [
    Duration::ZERO,
    Duration::from_millis(250),
    Duration::from_secs(1),
];
const RUN_CONTROL_PATH: &str = "/.well-known/ato/control";
const BROWSER_PRESENTATION_PATH: &str = "/.well-known/ato/browser/frame.jpg";
const RUN_CONTROL_MAX_FRAME_BYTES: usize = 32 * 1024;
const RUN_CONTROL_REQUEST_HEADER_MAX_BYTES: usize = 16 * 1024;
const AUTHORING_SEMANTICS_ID: &str = "ato.authoring@1";
const SOURCE_READY_TIMEOUT: Duration = Duration::from_secs(30);
type HmacSha256 = Hmac<Sha256>;

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

struct AttachedBrowserActuator(Arc<dyn LiveOperationDispatcher>);

impl BrowserOperationActuator for AttachedBrowserActuator {
    fn apply(
        &self,
        correlation_id: &str,
        realization_generation: Option<&str>,
        operation: &LiveOperation,
    ) -> std::result::Result<u64, String> {
        self.0
            .apply_operation(correlation_id, realization_generation, operation)
            .map(|settlement| settlement.order)
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
    record_refs: Arc<Mutex<BTreeMap<String, String>>>,
}

impl BrowserRecordSubmission for RunnerBrowserRecordSubmission {
    fn submit(&self, operation: &AcceptedBrowserOperation) -> std::result::Result<(), String> {
        // `operation` contains the exact event/transition/run_seq/operation_id
        // accepted by the authority. Record metadata stays outside semantic
        // identity; the portable Record itself remains only the Browser action.
        let record_ref = self
            .stylus
            .record_with_receipt(RecordCandidate {
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
            .map_err(|error| error.to_string())?
            .to_string();
        self.record_refs
            .lock()
            .map_err(|_| "Browser Record reference cache poisoned".to_owned())?
            .insert(operation.operation_id.clone(), record_ref);
        Ok(())
    }

    fn record_ref(&self, operation_id: &str) -> Option<String> {
        self.record_refs
            .lock()
            .ok()
            .and_then(|refs| refs.get(operation_id).cloned())
    }
}

type HostedBrowserIngress = BrowserOperationIngress<
    AttachedBrowserActuator,
    RunnerBrowserHeadPersistence,
    RunnerBrowserRecordSubmission,
>;

/// Object-safe control boundary used by the generic WebSocket server. The
/// concrete BrowserOperationIngress remains the sole authority owner; this
/// trait prevents the listener from depending on its Adapter/Persistence types.
trait BrowserControlIngress: Send + Sync {
    fn accept_control_operation(
        &self,
        operation_id: String,
        event: ato_adapter_browser::BrowserEvent,
    ) -> std::result::Result<ato_kernel::AcceptedOperation, ato_kernel::EvolutionError>;

    fn accept_control_operation_in_context(
        &self,
        operation_id: String,
        event: ato_adapter_browser::BrowserEvent,
        _realization_generation: Option<String>,
    ) -> std::result::Result<ato_kernel::AcceptedOperation, ato_kernel::EvolutionError> {
        self.accept_control_operation(operation_id, event)
    }

    fn retry_control_persistence(
        &self,
    ) -> std::result::Result<
        Option<(String, ato_kernel::AcceptedOperation)>,
        ato_kernel::EvolutionError,
    > {
        Ok(None)
    }

    fn control_operation_retry_stage(&self, _operation_id: &str) -> BrowserOperationRetryStage {
        BrowserOperationRetryStage::BeforeApply
    }

    fn control_operation_record_ref(&self, _operation_id: &str) -> Option<String> {
        None
    }
}

impl<A, P, R> BrowserControlIngress for BrowserOperationIngress<A, P, R>
where
    A: BrowserOperationActuator + Send,
    P: BrowserHeadPersistence + Send + Sync,
    R: BrowserRecordSubmission + Send + Sync,
{
    fn accept_control_operation(
        &self,
        operation_id: String,
        event: ato_adapter_browser::BrowserEvent,
    ) -> std::result::Result<ato_kernel::AcceptedOperation, ato_kernel::EvolutionError> {
        self.accept_with_operation_id(operation_id, event)
    }

    fn accept_control_operation_in_context(
        &self,
        operation_id: String,
        event: ato_adapter_browser::BrowserEvent,
        realization_generation: Option<String>,
    ) -> std::result::Result<ato_kernel::AcceptedOperation, ato_kernel::EvolutionError> {
        self.accept_with_operation_context(operation_id, event, realization_generation)
    }

    fn retry_control_persistence(
        &self,
    ) -> std::result::Result<
        Option<(String, ato_kernel::AcceptedOperation)>,
        ato_kernel::EvolutionError,
    > {
        self.retry_pending_persistence()
            .map(|retry| retry.operation_id.zip(retry.accepted))
    }

    fn control_operation_retry_stage(&self, operation_id: &str) -> BrowserOperationRetryStage {
        self.operation_retry_stage(operation_id)
    }

    fn control_operation_record_ref(&self, operation_id: &str) -> Option<String> {
        self.record_ref(operation_id)
    }
}

#[derive(Debug, Clone)]
struct BrowserControlCapability {
    protocol: String,
    port: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunControlClaims {
    v: u8,
    session_id: String,
    run_id: String,
    lease_id: String,
    runner_id: String,
    protocol: String,
    port: String,
    expires_at: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunControlRequest {
    operation_id: String,
    client_seq: u64,
    protocol: String,
    operation: String,
    port: String,
    /// Canonical `ato.browser@1` JSON. Keeping the operation payload as its
    /// protocol encoding avoids a second, control-product schema.
    payload: String,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum RunControlResponse {
    Applied {
        operation_id: String,
        client_seq: u64,
        run_seq: u64,
        head_after: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        record_error: Option<String>,
    },
    Rejected {
        operation_id: String,
        client_seq: u64,
        reason: String,
    },
}

/// Loopback-only generic Run-control listener. The public surface mux routes
/// only `RUN_CONTROL_PATH` here; it has no Activity/Product vocabulary and it
/// never persists raw input events.
struct RunControlServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl RunControlServer {
    fn start<I>(
        ingress: Arc<I>,
        run_id: String,
        lease_id: String,
        runner_id: String,
        capability: BrowserControlCapability,
        verification_key: String,
    ) -> Result<Self>
    where
        I: BrowserControlIngress + 'static,
    {
        ensure!(
            verification_key.len() >= 32,
            "Run control verification key is too short"
        );
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let ingress: Arc<dyn BrowserControlIngress> = ingress;
        let thread_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let ingress = Arc::clone(&ingress);
                        let run_id = run_id.clone();
                        let lease_id = lease_id.clone();
                        let runner_id = runner_id.clone();
                        let capability = capability.clone();
                        let verification_key = verification_key.clone();
                        thread::spawn(move || {
                            let _ = serve_run_control_connection(
                                stream,
                                ingress,
                                &run_id,
                                &lease_id,
                                &runner_id,
                                &capability,
                                &verification_key,
                            );
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
            address,
            stop,
            worker: Some(worker),
        })
    }

    fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for RunControlServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

// tungstenite fixes the HTTP rejection response type in its handshake callback.
#[allow(clippy::result_large_err)]
fn serve_run_control_connection(
    stream: TcpStream,
    ingress: Arc<dyn BrowserControlIngress>,
    run_id: &str,
    lease_id: &str,
    runner_id: &str,
    capability: &BrowserControlCapability,
    verification_key: &str,
) -> Result<()> {
    stream.set_nonblocking(false)?;
    // A connected client must lose authority promptly when its short-lived
    // capability expires. A bounded read timeout lets an idle connection be
    // closed without waiting for the next client frame.
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_secs(15)))?;
    let expected_run = run_id.to_owned();
    let expected_lease = lease_id.to_owned();
    let expected_runner = runner_id.to_owned();
    let expected_capability = capability.clone();
    let verification_key = verification_key.to_owned();
    let credential_expires_at = Arc::new(AtomicI64::new(0));
    let handshake_expires_at = Arc::clone(&credential_expires_at);
    let mut socket = accept_hdr(
        stream,
        move |request: &WebSocketRequest, mut response: WebSocketResponse| {
            let requested_protocol = request
                .headers()
                .get("sec-websocket-protocol")
                .and_then(|value| value.to_str().ok());
            let credential = request
                .headers()
                .get("sec-websocket-protocol")
                .and_then(|value| value.to_str().ok())
                .and_then(run_control_credential_from_protocols);
            let expiry = credential.and_then(|credential| {
                verify_run_control_credential(
                    &verification_key,
                    credential,
                    &expected_run,
                    &expected_lease,
                    &expected_runner,
                    &expected_capability,
                )
                .ok()
            });
            if request.uri().path() != RUN_CONTROL_PATH || expiry.is_none() {
                return Err(run_control_forbidden());
            }
            handshake_expires_at.store(expiry.expect("expiry was checked"), Ordering::Release);
            if let Some(protocol) = requested_protocol.and_then(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .find(|value| value.starts_with("ato-control."))
            }) {
                response.headers_mut().insert(
                    "sec-websocket-protocol",
                    protocol
                        .parse()
                        .expect("validated control subprotocol is a header value"),
                );
            }
            Ok(response)
        },
    )
    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    loop {
        if credential_expires_at.load(Ordering::Acquire)
            <= OffsetDateTime::now_utc().unix_timestamp()
        {
            let _ = socket.send(Message::Close(None));
            break;
        }
        let message = match socket.read() {
            Ok(message) => message,
            Err(
                tungstenite::Error::ConnectionClosed
                | tungstenite::Error::AlreadyClosed
                | tungstenite::Error::Protocol(
                    tungstenite::error::ProtocolError::ResetWithoutClosingHandshake,
                ),
            ) => break,
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(error) => return Err(anyhow::anyhow!(error.to_string())),
        };
        match message {
            Message::Text(text) => {
                if text.len() > RUN_CONTROL_MAX_FRAME_BYTES {
                    socket.send(Message::Close(None))?;
                    break;
                }
                let request = serde_json::from_str::<RunControlRequest>(&text);
                let response = match request {
                    Ok(request) => {
                        handle_run_control_request(ingress.as_ref(), capability, &request)
                    }
                    Err(_) => RunControlResponse::Rejected {
                        operation_id: String::new(),
                        client_seq: 0,
                        reason: "invalid_operation".to_owned(),
                    },
                };
                socket.send(Message::Text(serde_json::to_string(&response)?.into()))?;
            }
            Message::Ping(payload) => socket.send(Message::Pong(payload))?,
            Message::Close(_) => break,
            Message::Binary(_) => socket.send(Message::Close(None))?,
            Message::Pong(_) | Message::Frame(_) => {}
        }
    }
    Ok(())
}

fn handle_run_control_request(
    ingress: &dyn BrowserControlIngress,
    capability: &BrowserControlCapability,
    request: &RunControlRequest,
) -> RunControlResponse {
    if request.protocol != capability.protocol || request.port != capability.port {
        return RunControlResponse::Rejected {
            operation_id: request.operation_id.clone(),
            client_seq: request.client_seq,
            reason: "protocol_or_port_forbidden".to_owned(),
        };
    }
    if request.payload.len() > RUN_CONTROL_MAX_FRAME_BYTES {
        return RunControlResponse::Rejected {
            operation_id: request.operation_id.clone(),
            client_seq: request.client_seq,
            reason: "payload_too_large".to_owned(),
        };
    }
    let event = match ato_adapter_browser::decode_event(request.payload.as_bytes()) {
        Ok(event) => event,
        Err(_) => {
            return RunControlResponse::Rejected {
                operation_id: request.operation_id.clone(),
                client_seq: request.client_seq,
                reason: "invalid_payload".to_owned(),
            };
        }
    };
    if request.operation != ato_adapter_browser::operation_for_event(&event) {
        return RunControlResponse::Rejected {
            operation_id: request.operation_id.clone(),
            client_seq: request.client_seq,
            reason: "operation_mismatch".to_owned(),
        };
    }
    match ingress.accept_control_operation(request.operation_id.clone(), event) {
        Ok(accepted) => RunControlResponse::Applied {
            operation_id: request.operation_id.clone(),
            client_seq: request.client_seq,
            run_seq: accepted.run_seq,
            head_after: accepted.transition.to.to_string(),
            record_error: accepted.record_error,
        },
        Err(error) => {
            let reason = evolution_error_code(&error);
            eprintln!("run control operation rejected: {reason}");
            RunControlResponse::Rejected {
                operation_id: request.operation_id.clone(),
                client_seq: request.client_seq,
                reason: reason.to_owned(),
            }
        }
    }
}

fn evolution_error_code(error: &EvolutionError) -> &'static str {
    match error {
        EvolutionError::Kernel(_) => "kernel_rejected",
        EvolutionError::Frozen => "run_frozen",
        EvolutionError::PersistencePending(_) => "head_persistence_pending",
        EvolutionError::Apply(_) => "adapter_apply_rejected",
        EvolutionError::Persist(_) => "head_persistence_failed",
    }
}

fn run_control_credential_from_protocols(protocols: &str) -> Option<&str> {
    protocols
        .split(',')
        .map(str::trim)
        .find_map(|value| value.strip_prefix("ato-control."))
}

fn verify_run_control_credential(
    verification_key: &str,
    credential: &str,
    run_id: &str,
    lease_id: &str,
    runner_id: &str,
    capability: &BrowserControlCapability,
) -> Result<i64> {
    let (encoded, signature) = credential
        .split_once('.')
        .context("Run control credential format is invalid")?;
    ensure!(
        !signature.contains('.'),
        "Run control credential format is invalid"
    );
    let signature =
        hex::decode(signature).context("Run control credential signature is invalid")?;
    let mut mac = HmacSha256::new_from_slice(verification_key.as_bytes())
        .map_err(|_| anyhow::anyhow!("Run control verification key is invalid"))?;
    mac.update(encoded.as_bytes());
    mac.verify_slice(&signature)
        .map_err(|_| anyhow::anyhow!("Run control credential signature is invalid"))?;
    let payload = URL_SAFE_NO_PAD
        .decode(encoded)
        .context("Run control credential payload is invalid")?;
    let claims: RunControlClaims =
        serde_json::from_slice(&payload).context("Run control credential claims are invalid")?;
    ensure!(claims.v == 1, "Run control credential version is invalid");
    ensure!(
        claims.expires_at > OffsetDateTime::now_utc().unix_timestamp(),
        "Run control credential expired"
    );
    ensure!(
        claims.session_id.starts_with("rcs_"),
        "Run control session id is invalid"
    );
    ensure!(
        claims.run_id == run_id && claims.lease_id == lease_id && claims.runner_id == runner_id,
        "Run control credential scope mismatch"
    );
    ensure!(
        claims.protocol == capability.protocol && claims.port == capability.port,
        "Run control credential capability mismatch"
    );
    Ok(claims.expires_at)
}

fn run_control_forbidden() -> ErrorResponse {
    tungstenite::http::Response::builder()
        .status(tungstenite::http::StatusCode::FORBIDDEN)
        .body(Some("Run control authorization failed".to_owned()))
        .expect("static Run control rejection is valid")
}

struct HostedBrowserRuntime {
    ingress: Option<Arc<HostedBrowserIngress>>,
    control: Option<RunControlServer>,
    control_capability: BrowserControlCapability,
    adapter: Option<Arc<Mutex<Box<dyn AttachedAdapter>>>>,
    host: Option<BrowserHost>,
    pipeline: Option<ato_record_writer::RecordPipeline>,
    objects: Arc<dyn ObjectStore>,
    workspace: PathBuf,
}

impl HostedBrowserRuntime {
    fn control_capability(&self) -> BrowserControlCapability {
        self.control_capability.clone()
    }

    fn control_address(&self) -> SocketAddr {
        self.control
            .as_ref()
            .expect("Browser runtime always owns its control listener")
            .address()
    }

    fn activity_ingress(&self) -> Arc<dyn BrowserControlIngress> {
        Arc::clone(
            self.ingress
                .as_ref()
                .expect("Browser runtime always owns its ingress while active"),
        ) as Arc<dyn BrowserControlIngress>
    }

    fn open_auxiliary_target(&mut self, target_url: &str) -> Result<()> {
        self.host
            .as_mut()
            .context("Browser Host is unavailable")?
            .open_auxiliary_target(target_url)
    }

    fn capture_presentation_frame(&mut self) -> Result<Vec<u8>> {
        self.host
            .as_mut()
            .context("Browser Host is unavailable")?
            .capture_jpeg()
    }

    fn webmcp_snapshot(&mut self) -> Result<RawWebMcpSnapshotV1> {
        self.host
            .as_mut()
            .context("Browser Host is unavailable")?
            .webmcp_snapshot()
    }

    fn abort_webmcp_operation(&mut self, operation_id: &str) -> Result<bool> {
        self.host
            .as_mut()
            .context("Browser Host is unavailable")?
            .abort_webmcp_operation(operation_id)
    }

    /// Future capture coordination freezes this logical gate before it asks
    /// the Browser Host to quiesce. No physical Browser state is captured in
    /// P0-B.
    fn freeze(&self) -> Result<ato_kernel::RunHeadSnapshot> {
        self.ingress
            .as_ref()
            .context("Browser ingress is already stopped")?
            .freeze()
            .map_err(|error| anyhow::anyhow!(error.to_string()))
    }

    fn stop(mut self) -> Result<()> {
        self.freeze()?;
        self.cleanup()?;
        Ok(())
    }

    fn cleanup(&mut self) -> Result<()> {
        self.control.take();
        // BrowserOperationIngress owns a Record stylus sender. It must be
        // released before RecordPipeline::shutdown waits for all senders;
        // otherwise clean stop deadlocks after Chrome has already exited.
        self.ingress.take();
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

fn refresh_activity_presentation_frame<C, P>(
    next_frame_at: &mut Instant,
    capture: C,
    publish: P,
) -> Result<()>
where
    C: FnOnce() -> Result<Vec<u8>>,
    P: FnOnce(Vec<u8>) -> Result<()>,
{
    if Instant::now() < *next_frame_at {
        return Ok(());
    }
    // Presentation is a compatibility projection, not the Run's physical
    // liveness boundary. Keep the last good frame across a transient CDP
    // capture failure and back off before trying the untrusted page again.
    let retry_after = match capture() {
        Ok(frame) => {
            publish(frame)?;
            ACTIVITY_FRAME_REFRESH_INTERVAL
        }
        Err(_) => ACTIVITY_FRAME_RETRY_INTERVAL,
    };
    *next_frame_at = Instant::now() + retry_after;
    Ok(())
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
    #[arg(
        long,
        env = "ATO_RUNNER_TOKEN",
        default_value = "",
        hide_env_values = true
    )]
    pub runner_token: String,
    /// Existing canonical runner credential JSON. Explicit CLI/env identity
    /// values, when provided, must exactly match this file.
    #[arg(long, env = "ATO_RUNNER_CREDENTIALS_FILE")]
    pub runner_credentials_file: Option<PathBuf>,
    #[arg(long, env = "ATO_RUNNER_PUBLIC_BASE_URL")]
    pub public_base_url: Option<String>,
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
    /// Firecracker TAP host CIDR.
    ///
    /// Firecracker-ONLY physical configuration, and therefore optional: the
    /// source/replay realization path never reads it. Requiring it at startup
    /// made every Runner a Firecracker Runner, which kept the worker off hosts
    /// that can serve the source/replay path perfectly well (macOS Desktop).
    #[arg(long, env = "ATO_FC_TAP_HOST_CIDR")]
    pub tap_host_cidr: Option<String>,
    #[arg(long, env = "ATO_RUNNER_SLOT_ID", default_value = "0")]
    pub slot_id: String,
    /// Host-wide capacity advertised to the control plane. Multiple worker
    /// processes may share one Runner credential when each owns a distinct
    /// physical slot (ports, TAP network, and slot id).
    #[arg(long, env = "ATO_RUNNER_MAX_SLOTS", default_value_t = 1)]
    pub max_slots: u32,
    /// Required only by roots that explicitly compose a Browser Computation.
    #[arg(long, env = "ATO_BROWSER_CHROME")]
    pub browser_chrome: Option<PathBuf>,
    /// Runner-scoped HMAC verification key for short-lived API-issued
    /// Run-control capabilities. The control-plane root is never installed on
    /// a Runner. Required only for Browser-aware Hosted Runs.
    #[arg(long, env = "ATO_RUN_CONTROL_VERIFICATION_KEY", hide_env_values = true)]
    pub run_control_verification_key: Option<String>,
    /// Host-wide root for Runner-local persistent volumes, shared by every
    /// slot worker of this Runner. Unset: volume-backed Runs are neither
    /// advertised nor accepted. Never under a lease or work directory.
    #[arg(long, env = "ATO_RUNNER_STATE_VOLUME_ROOT")]
    pub state_volume_root: Option<PathBuf>,
    /// Enable the isolated raw-TCP broker. Disabled by default; capability is
    /// advertised only when Docker admission also passes.
    #[arg(long, env = "ATO_RUNTIME_NETWORK_CONTROLS", default_value_t = false)]
    pub network_controls: bool,
    /// Comma-separated exact sockets this slot may own as fixed listeners.
    #[arg(long, env = "ATO_RUNTIME_FIXED_TCP_ALLOWLIST", default_value = "")]
    pub fixed_tcp_allowlist: String,
    #[arg(long)]
    pub once: bool,
}

/// How a runtime launch ended, and whether its workloads are confirmed
/// stopped (the journal entry may then go).
struct RuntimeLaunchOutcome {
    result: Result<()>,
    stop_confirmed: bool,
}

struct RuntimeServeControls<'a> {
    hard_deadline: Option<Instant>,
    execution_authorization: Option<&'a mut runtime_launch::lease::ExecutionAuthorizationMonitor>,
    network_authorization: Option<&'a runtime_launch::lease::NetworkAuthorization>,
}

pub struct ConnectedWorker {
    config: WorkerConfig,
    api: HttpRunnerApi,
    volume_store: Option<runtime_launch::volume::VolumeStore>,
    fixed_tcp: runtime_launch::network_broker::FixedTcpRegistry,
}

impl ConnectedWorker {
    pub fn new(mut config: WorkerConfig) -> Result<Self> {
        resolve_runner_credentials(&mut config)?;
        validate_config(&config)?;
        fs::create_dir_all(&config.work_root)?;
        let mut api =
            HttpRunnerApi::new(&config.api_base, &config.runner_id, &config.runner_token)?;
        if config.network_controls {
            runtime_launch::network_broker::prepare_egress_firewall().context(
                "TCP egress controls failed host firewall admission; capability not advertised",
            )?;
        }
        // A store that cannot be opened is not advertised; the worker still
        // serves everything else.
        let volume_store = config.state_volume_root.as_deref().and_then(|root| {
            runtime_launch::volume::VolumeStore::open(root, &config.runner_id)
                .map_err(|error| {
                    eprintln!("[runtime-launch] persistent volumes disabled: {error:#}")
                })
                .ok()
        });
        api.persistent_volumes = volume_store.is_some();
        let fixed_tcp =
            runtime_launch::network_broker::FixedTcpRegistry::new(if config.network_controls {
                &config.fixed_tcp_allowlist
            } else {
                ""
            })?;
        Ok(Self {
            config,
            api,
            volume_store,
            fixed_tcp,
        })
    }

    /// Stop and report whatever this slot left running before it advertises
    /// runtime work. A failure leaves the slot unrecovered, not the worker
    /// dead: other lease kinds keep working and recovery is retried.
    fn recover_runtime_launch(&self) {
        if runtime_launch::recovery::slot_recovered()
            || !runtime_launch::lease::RUNTIME_LAUNCH_SUPPORTED()
        {
            return;
        }
        let journal = match runtime_launch::recovery::RunJournal::new(
            &self.config.work_root,
            &self.config.runner_id,
            &self.config.slot_id,
        ) {
            Ok(journal) => journal,
            Err(error) => {
                eprintln!("[runtime-launch-recovery] blocked: {error:#}");
                runtime_launch::recovery::mark_slot_recovered(false);
                return;
            }
        };
        let scanner = ato_adapter_oci::OwnedResourceScanner::new(
            &self.config.runner_id,
            &self.config.slot_id,
        )
        .ok();
        match runtime_launch::recovery::recover_slot(
            &journal,
            scanner.as_ref(),
            &self.api,
            ato_adapter_oci::StopBudget::DEFAULT,
        ) {
            Ok(result) => {
                for report in &result.reports {
                    eprintln!(
                        "[runtime-launch-recovery] {}",
                        serde_json::to_string(report).unwrap_or_default()
                    );
                }
                runtime_launch::recovery::mark_slot_recovered(result.clean);
            }
            Err(error) => {
                eprintln!("[runtime-launch-recovery] blocked: {error:#}");
                runtime_launch::recovery::mark_slot_recovered(false);
            }
        }
    }

    pub fn run(&self) -> Result<()> {
        let _slot = slot_state::SlotGuard::acquire(&self.config.work_root)?;
        self.recover_runtime_launch();
        self.api.heartbeat(&self.config, 0)?;
        loop {
            self.recover_runtime_launch();
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
                let code = if message.contains("execution authorization") {
                    "execution_authorization_ended"
                } else {
                    "realization_failed"
                };
                if let Err(report_error) = self.api.report_failed(&lease.id, code, &message) {
                    // Never print credentials or the remote response body. The
                    // lease id and bounded HTTP error are enough to distinguish
                    // a physical failure from a terminal-evidence delivery gap.
                    eprintln!(
                        "connected Realization terminal report failed lease_id={} error={report_error:#}",
                        lease.id
                    );
                }
                // A failed state commit or physical teardown must not be followed
                // by a new claim. The retained lease directory fences restarts.
                return Err(error).context("worker slot requires recovery");
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
        fs::create_dir(&lease_root)?;
        let result = match &lease.command {
            LeaseCommand::Portable(command) => {
                self.execute_portable_lease(lease, command, &lease_root)
            }
            LeaseCommand::Activity(command) => {
                self.execute_activity_lease(lease, command, &lease_root)
            }
            LeaseCommand::RuntimeLaunch(command) => {
                self.execute_runtime_launch_lease(lease, command, &lease_root)
            }
            LeaseCommand::VolumeMaintenance(command) => {
                self.execute_volume_maintenance_lease(lease, command)
            }
        };
        settle_lease_directory(&lease_root, result)
    }

    /// Checkpoint, restore or delete a Runner-local volume. No workload runs:
    /// the control plane issued this lease only after the previous Run's stop
    /// was confirmed and this operation took the next writer generation.
    fn execute_volume_maintenance_lease(
        &self,
        lease: &ClaimedLease,
        command: &runtime_launch::volume_maintenance::VolumeMaintenanceLeaseCommand,
    ) -> Result<()> {
        ensure!(
            runtime_launch::recovery::slot_recovered(),
            "this Runner slot has not confirmed its previous Runs stopped; refusing volume work"
        );
        let store = self
            .volume_store
            .as_ref()
            .context("this Runner has no volume store; refusing a volume operation")?;
        // Journaled first: a Runner that dies mid-operation is recovered as a
        // confirmed stop (there is no workload), and the control plane marks
        // the operation interrupted. Restore and delete swap atomically, so
        // the volume is consistent either way.
        let owner = ato_adapter_oci::OciOwner {
            runner_id: self.config.runner_id.clone(),
            slot_id: self.config.slot_id.clone(),
            lease_id: lease.id.clone(),
            run_id: command.run_id.clone(),
            incarnation: runtime_launch::recovery::incarnation().to_owned(),
        };
        let journal = runtime_launch::recovery::RunJournal::new(
            &self.config.work_root,
            &self.config.runner_id,
            &self.config.slot_id,
        )?;
        journal.record(&runtime_launch::recovery::RunJournalEntry::new(&owner))?;

        let transport = runtime_launch::state_artifact::LeaseStateArtifactTransport::new(
            self.api.client.clone(),
            self.api.base.clone(),
            lease.id.clone(),
            self.api.token.clone(),
        );
        let performed = runtime_launch::volume_maintenance::perform(command, store, &transport);
        let (outcome, error) = match &performed {
            Ok(()) => (
                runtime_launch::state_artifact::VolumeOperationOutcome::Succeeded,
                None,
            ),
            Err(error) => (
                runtime_launch::state_artifact::VolumeOperationOutcome::Failed,
                Some(format!("{error:#}")),
            ),
        };
        eprintln!(
            "[volume-operation] {}",
            serde_json::json!({
                "operation_id": command.operation_id,
                "operation": format!("{:?}", command.operation).to_lowercase(),
                "volume_ref": command.volume_ref,
                "outcome": if performed.is_ok() { "succeeded" } else { "failed" },
                "error": error,
            })
        );
        runtime_launch::state_artifact::StateArtifactTransport::complete_volume_operation(
            &transport,
            &command.operation_id,
            outcome,
            error.as_deref(),
        )?;
        // Reported: the control plane has the outcome and released the writer.
        journal.remove(&lease.id)?;
        Ok(())
    }

    /// One Dynamic Compute Run: contained process, real state, real stop.
    ///
    /// The Run stays ACTIVE after readiness rather than committing there. A
    /// Run that ended at readiness would make every App a batch job — up,
    /// declared ready, torn down before anyone used it — and the state it
    /// committed would be the state of an App nobody touched.
    fn execute_runtime_launch_lease(
        &self,
        lease: &ClaimedLease,
        command: &runtime_launch::lease::RuntimeLaunchLeaseCommand,
        lease_root: &Path,
    ) -> Result<()> {
        // A slot that has not finished recovering may still have a previous
        // incarnation's workload running; it takes no runtime work.
        ensure!(
            runtime_launch::recovery::slot_recovered(),
            "this Runner slot has not confirmed its previous Runs stopped; refusing runtime launch"
        );
        // Proven before anything is materialized: if the spec that arrived is
        // not the one the control plane digested onto the Run, the Runner
        // would execute something the receipt does not describe.
        let spec = runtime_launch::lease::verified_spec(command)?;
        // Arm this before materialization/startup. A public preview's cap owns
        // the whole allocation, not only the time after readiness.
        let started = Instant::now();
        let hard_deadline = runtime_launch::lease::maximum_lifetime(command)
            .map(|duration| {
                started
                    .checked_add(duration)
                    .context("runtime launch deadline overflowed")
            })
            .transpose()?;
        let mut execution_authorization = command
            .execution_authorization
            .as_ref()
            .map(|authorization| {
                runtime_launch::lease::ExecutionAuthorizationMonitor::new(authorization, started)
            })
            .transpose()?;

        // Journaled before any writer, network or container exists, so a
        // Runner that dies from here on leaves something recovery can find.
        let owner = ato_adapter_oci::OciOwner {
            runner_id: self.config.runner_id.clone(),
            slot_id: self.config.slot_id.clone(),
            lease_id: lease.id.clone(),
            run_id: spec.context().run_id.clone(),
            incarnation: runtime_launch::recovery::incarnation().to_owned(),
        };
        let journal = runtime_launch::recovery::RunJournal::new(
            &self.config.work_root,
            &self.config.runner_id,
            &self.config.slot_id,
        )?;
        let mut entry = runtime_launch::recovery::RunJournalEntry::new(&owner);
        if let Some(network) = command.network_authorization.as_ref() {
            entry.network_generations.extend(
                network
                    .egress
                    .iter()
                    .map(|grant| (grant.grant_id.clone(), grant.generation)),
            );
            entry.network_generations.extend(
                network
                    .fixed_tcp
                    .iter()
                    .map(|allocation| (allocation.allocation_id.clone(), allocation.generation)),
            );
        }
        journal.record(&entry)?;

        if let Some(network) = command.network_authorization.as_ref() {
            ensure!(
                self.config.network_controls,
                "this Runner did not enable network controls"
            );
            self.fixed_tcp
                .register_pending(&lease.run_id, &network.fixed_tcp)?;
        }

        let outcome = self.run_runtime_launch(
            lease,
            &spec,
            hard_deadline,
            &mut execution_authorization,
            command.network_authorization.as_ref(),
            &owner,
            &journal,
            &mut entry,
            lease_root,
        );
        if let Some(network) = command.network_authorization.as_ref() {
            for allocation in &network.fixed_tcp {
                self.fixed_tcp.deactivate(&lease.run_id, allocation);
            }
        }
        if outcome.stop_confirmed {
            // Confirmed stopped, or never started: nothing left to recover.
            journal.remove(&lease.id)?;
        } else {
            // Kept for recovery; the slots are quarantined, not released.
            entry.phase = runtime_launch::recovery::RunPhase::StopUnconfirmed;
            journal.record(&entry)?;
        }
        outcome.result
    }

    /// One runtime launch, from resolution to a stop. Every path after the
    /// workload starts goes through `finish`, which stops it before anything
    /// is committed or released.
    #[allow(clippy::too_many_arguments)]
    fn run_runtime_launch(
        &self,
        lease: &ClaimedLease,
        spec: &ato_ipc::runtime_launch_v2::RuntimeLaunchSpec,
        hard_deadline: Option<Instant>,
        execution_authorization: &mut Option<runtime_launch::lease::ExecutionAuthorizationMonitor>,
        network_authorization: Option<&runtime_launch::lease::NetworkAuthorization>,
        owner: &ato_adapter_oci::OciOwner,
        journal: &runtime_launch::recovery::RunJournal,
        entry: &mut runtime_launch::recovery::RunJournalEntry,
        lease_root: &Path,
    ) -> RuntimeLaunchOutcome {
        let not_started = |result: Result<()>| RuntimeLaunchOutcome {
            result,
            stop_confirmed: true,
        };
        let workspace = runtime_launch::workspace::LeaseWorkspaceTransport::new(
            self.api.client.clone(),
            self.api.base.clone(),
            lease.id.clone(),
            self.api.token.clone(),
        );
        let state = runtime_launch::state_artifact::LeaseStateArtifactTransport::new(
            self.api.client.clone(),
            self.api.base.clone(),
            lease.id.clone(),
            self.api.token.clone(),
        );
        let secrets = match self
            .api
            .redeem_runtime_bindings(&lease.id, &spec.secret_grants())
        {
            Ok(secrets) => secrets,
            Err(error) => return not_started(Err(error)),
        };
        match self.refresh_execution_authorization(&lease.id, execution_authorization.as_mut()) {
            Ok(true) => {
                let execution_id = match spec.canonical_digest() {
                    Ok(digest) => digest,
                    Err(error) => return not_started(Err(error.into())),
                };
                return not_started(self.api.report_stopped(&lease.id, &execution_id));
            }
            Ok(false) => {}
            Err(error) => return not_started(Err(error)),
        }

        // P4-A: publish the process on this Runner's ingress slot.
        //
        // `surface_listen` is the loopback port the slot's reverse proxy
        // already forwards to, and `public_base_url` is the hostname it
        // forwards from. The workload does NOT bind it: it binds
        // `hidden_surface_listen`, and the worker's own gate owns the public
        // port — the slot host is internet-reachable, so a request has to
        // prove it came through the app proxy before it may reach the app.
        // Both are existing Runner configuration — the control plane never
        // picks a host port, because only the Runner knows what is free and
        // the stable URL must not depend on it.
        let endpoint_name = match runtime_launch::lease::published_endpoint_name(spec) {
            Ok(name) => name,
            Err(error) => return not_started(Err(error)),
        };
        let mut assigned_ports = std::collections::BTreeMap::new();
        assigned_ports.insert(
            endpoint_name.clone(),
            self.config.hidden_surface_listen.port(),
        );
        // `resolve_run` acquires the writers and gives them back itself if it
        // fails; no workload exists yet.
        let process_host = runtime_launch::process_executor::ProcessLaunchHost {
            shim: match std::env::current_exe() {
                Ok(path) => path,
                Err(error) => return not_started(Err(error.into())),
            },
            runtime_root: lease_root.join("process-runtime"),
            output: None,
        };
        let resolved = match runtime_launch::lease::resolve_run(
            spec,
            lease_root,
            &workspace,
            &state,
            secrets,
            &assigned_ports,
            self.volume_store.as_ref(),
        ) {
            Ok(resolved) => resolved,
            Err(error) => return not_started(Err(error)),
        };
        entry.writer_fences = resolved.prepared.writer_fences();
        entry.phase = runtime_launch::recovery::RunPhase::Launching;
        if let Err(error) = journal.record(entry) {
            runtime_launch::session::abort_run(&state, &resolved.prepared);
            return not_started(Err(error));
        }
        let probe =
            runtime_launch::process_executor::LoopbackReadinessProbe::new(self.api.client.clone());
        let active = match runtime_launch::lease::start(
            spec,
            resolved,
            &state,
            &probe,
            owner,
            network_authorization,
            &process_host,
        ) {
            Ok(active) => active,
            Err(mut failure) => {
                entry.process = failure.process.take();
                return RuntimeLaunchOutcome {
                    stop_confirmed: failure.stop.is_confirmed(),
                    result: Err(anyhow::Error::new(failure)),
                };
            }
        };
        entry.phase = runtime_launch::recovery::RunPhase::Active;
        if let runtime_launch::lease::ActiveWorkload::Process(process) = &active.launched {
            entry.process =
                runtime_launch::recovery::process_start_time(process.pid()).map(|start_time| {
                    runtime_launch::recovery::ProcessIdentity {
                        pid: process.pid(),
                        start_time,
                    }
                });
        }

        // Everything between readiness and the stop request. Any error here
        // still ends in `finish` below.
        let serving = journal.record(entry).and_then(|()| {
            self.serve_runtime_launch(
                lease,
                spec,
                &active,
                &endpoint_name,
                RuntimeServeControls {
                    hard_deadline,
                    execution_authorization: execution_authorization.as_mut(),
                    network_authorization,
                },
            )
        });

        entry.phase = runtime_launch::recovery::RunPhase::Stopping;
        let _ = journal.record(entry);
        // Whether the stop was requested, the lifetime ran out or serving
        // failed, the Run ends the same way: stop, prove the workloads are
        // gone, then pack, commit and release — or quarantine if the stop
        // cannot be confirmed.
        let (stopped, committed) = runtime_launch::lease::finish(
            spec,
            active,
            &state,
            &format!("commit_{}", lease.run_id),
        );
        eprintln!(
            "[runtime-launch] run={} stop={}",
            lease.run_id,
            serde_json::json!({
                "overall": &stopped.overall,
                "services": stopped
                    .services
                    .iter()
                    .map(|(name, outcome)| serde_json::json!({ "name": name, "outcome": outcome }))
                    .collect::<Vec<_>>(),
            })
        );
        let stop_confirmed = stopped.overall.is_confirmed();
        let result = serving.and_then(|execution_id| {
            let committed = committed?;
            for entry in &committed {
                eprintln!(
                    "[runtime-launch] run={} state={} fence={} {} -> {}",
                    lease.run_id,
                    entry.state_key,
                    entry.writer_fence,
                    entry.parent_revision_ref.as_deref().unwrap_or("<new>"),
                    entry.revision_ref.as_deref().unwrap_or("<unchanged>")
                );
            }
            self.api.report_stopped(&lease.id, &execution_id)
        });
        RuntimeLaunchOutcome {
            result,
            stop_confirmed,
        }
    }

    /// Report readiness and hold the Run ACTIVE until the control plane asks
    /// it to stop. Returns the execution id to report the stop against.
    fn serve_runtime_launch(
        &self,
        lease: &ClaimedLease,
        spec: &ato_ipc::runtime_launch_v2::RuntimeLaunchSpec,
        active: &runtime_launch::lease::ActiveRun,
        endpoint_name: &str,
        controls: RuntimeServeControls<'_>,
    ) -> Result<String> {
        let RuntimeServeControls {
            hard_deadline,
            mut execution_authorization,
            network_authorization,
        } = controls;
        ensure!(
            hard_deadline.is_none_or(|deadline| Instant::now() < deadline),
            "runtime launch maximum lifetime elapsed during startup"
        );
        let execution_id = spec
            .canonical_digest()
            .map_err(|error| anyhow::anyhow!("cannot digest the launch spec: {error}"))?;
        if self
            .refresh_execution_authorization(&lease.id, execution_authorization.as_deref_mut())?
        {
            return Ok(execution_id);
        }
        let port = active.endpoint_port(endpoint_name).with_context(|| {
            format!("the launch declared no `{endpoint_name}` endpoint to report")
        })?;
        let execution_evidence = active.execution_evidence();
        let oci_port_mapping = active.launched.surface_container().and_then(|container| {
            let (container_ip, mappings) = container.port_mapping();
            mappings.first().map(|mapping| OciPortMappingReport {
                container_ip: container_ip.to_string(),
                guest_port: mapping.guest_port,
                runner_forward_port: mapping.host_port,
            })
        });
        // The public slot port is this worker's gate, not the workload: a
        // request is forwarded to the hidden loopback port only after it
        // proves it came through the app proxy. When the claim carried no
        // assertion key the gate is absent and the proxy passes through —
        // that is exactly the state in which the control plane cannot sign
        // assertions, so nothing enforceable is lost.
        let _surface_gate = TcpProxy::start_with_mux(
            self.config.surface_listen,
            ProxyTarget::Tcp(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port)),
            Some(SurfaceMux {
                control_target: None,
                presentation_frame: None,
                gate: HttpProxyGate::for_lease(lease, &self.config.runner_id),
                guest_surface_gateway: false,
            }),
        )?;
        // The ready_url is the ingress slot's public hostname, and the process
        // is listening on the loopback port that slot forwards to. The API
        // still validates the hostname against this Runner's active ingress
        // slots, so a misconfigured `public_base_url` is refused rather than
        // believed.
        self.api.report_ready(
            &lease.id,
            ReadyReport {
                execution_id: &execution_id,
                ready_url: self.config.public_base_url.as_deref(),
                local_port: Some(port),
                execution: Some(&execution_evidence),
                port_mapping: oci_port_mapping.as_ref(),
                control: None,
            },
        )?;
        if let Some(network) = network_authorization {
            let runtime_launch::lease::ActiveWorkload::OciServiceGroup(group) = &active.launched
            else {
                bail!("network authorization was attached to a non-group workload")
            };
            for allocation in &network.fixed_tcp {
                let (_, container) = group
                    .services()
                    .find(|(name, _)| *name == allocation.service_id)
                    .with_context(|| {
                        format!(
                            "fixed TCP allocation names absent service `{}`",
                            allocation.service_id
                        )
                    })?;
                self.fixed_tcp.activate(
                    &lease.run_id,
                    allocation,
                    SocketAddr::new(container.container_address(), allocation.guest_port),
                )?;
            }
        }
        if let Some(container) = active.launched.surface_container() {
            let (container_ip, mappings) = container.port_mapping();
            for mapping in mappings {
                eprintln!(
                    "[runtime-launch-surface] {}",
                    serde_json::json!({
                        "run_id": lease.run_id,
                        "lease_id": lease.id,
                        "container_id": container.container_id(),
                        "container_ip": container_ip.to_string(),
                        "guest_port": mapping.guest_port,
                        "runner_forward_port": mapping.host_port,
                        "public_host": self.config.public_base_url,
                    })
                );
            }
        }
        eprintln!(
            "[runtime-launch] run={} ready {} endpoint=127.0.0.1:{port} public={}",
            lease.run_id,
            active.execution_subject(),
            self.config
                .public_base_url
                .as_deref()
                .unwrap_or("<private>")
        );

        // ACTIVE. The control plane decides when this ends.
        let last_heartbeat = std::cell::Cell::new(Instant::now());
        let mut stop = || -> Result<bool> {
            // A group is one Application: a service that exits while ACTIVE
            // fails the whole Run, which is then stopped by `finish`.
            if let runtime_launch::lease::ActiveWorkload::OciServiceGroup(group) = &active.launched
                && let Some((name, code)) = group.exited_service()?
            {
                bail!("OCI service `{name}` exited while the group was active with code {code}");
            }
            if last_heartbeat.get().elapsed() >= ACTIVE_HEARTBEAT_INTERVAL {
                self.api.heartbeat(&self.config, 1)?;
                last_heartbeat.set(Instant::now());
            }
            poll_runtime_launch_control(
                || Ok(self.api.control(&lease.id)?.stop_requested),
                || {
                    self.refresh_execution_authorization(
                        &lease.id,
                        execution_authorization.as_deref_mut(),
                    )
                },
            )
        };
        runtime_launch::lease::wait_for_stop(&mut stop, Duration::from_millis(500), hard_deadline)?;
        Ok(execution_id)
    }

    fn refresh_execution_authorization(
        &self,
        lease_id: &str,
        monitor: Option<&mut runtime_launch::lease::ExecutionAuthorizationMonitor>,
    ) -> Result<bool> {
        let Some(monitor) = monitor else {
            return Ok(false);
        };
        runtime_launch::lease::refresh_execution_authorization(
            monitor,
            lease_id,
            Instant::now,
            || self.api.renew_execution_authorization(lease_id),
            std::thread::sleep,
        )
    }

    fn execute_portable_lease(
        &self,
        lease: &ClaimedLease,
        command: &PortableLeaseCommand,
        lease_root: &Path,
    ) -> Result<()> {
        let trace_started_at = Instant::now();
        let source = self.api.graph_source(
            &lease.id,
            &command.bundle_id,
            &command.expected_root_computation_ref,
        )?;
        emit_coop_trace(
            command.trace_id.as_deref(),
            "capsule_resolved",
            trace_started_at,
        );
        let index: ObjectGraphIndexV1 = serde_json::from_slice(source.index_bytes())
            .context("runtime graph index is not valid JSON")?;
        let expectation = GraphDownloadExpectation {
            index_digest: source.index_digest().to_owned(),
            root_computation_ref: command.expected_root_computation_ref.clone(),
            object_count: index.objects.len(),
            logical_bytes: index.logical_bytes()?,
        };
        let graph = download_and_validate_graph(&source, &expectation, lease_root)?;
        validate_exported_web_port(
            &graph,
            &command.expected_root_computation_ref,
            &command.exported_port_id,
        )?;
        let evolution = Arc::new(initialize_hosted_run_evolution_authority(
            &graph,
            &command.expected_root_computation_ref,
        )?);
        let firecracker_work_root = self.config.work_root.join("fc");
        let physical = RestorePhysicalConfig {
            firecracker_work_root: &firecracker_work_root,
            slot_id: &self.config.slot_id,
            hidden_surface_listen: self.config.hidden_surface_listen,
            guest_surface_target: self.config.surface_target,
            tap_host_cidr: self.config.tap_host_cidr.as_deref(),
        };
        emit_coop_trace(
            command.trace_id.as_deref(),
            "realization_selected",
            trace_started_at,
        );
        let running = restore_portable_path(&graph, lease_root, &lease.id, &physical)?;
        self.api.report_status(&lease.id, "running")?;

        // An explicit Browser Computation receives one private Chrome
        // realization. Ordinary VM-only roots take the unchanged path.
        let mut browser = start_hosted_browser_runtime(
            &graph,
            lease,
            lease_root,
            &lease_root.join("workspace"),
            Arc::clone(&evolution),
            self.api.clone(),
            self.config.browser_chrome.as_deref(),
            self.config.run_control_verification_key.as_deref(),
            None,
            &self.config.runner_id,
            &format!("http://{}/", self.config.hidden_surface_listen),
        )?;

        // The externally reachable listener does not exist until the VM is
        // active, every Contract passed, and the Realization published.
        let presentation_frame = browser
            .as_mut()
            .map(|runtime| {
                runtime
                    .capture_presentation_frame()
                    .map(|frame| Arc::new(RwLock::new(frame)))
            })
            .transpose()?;
        let proxy = TcpProxy::start_with_mux(
            self.config.surface_listen,
            ProxyTarget::Tcp(self.config.hidden_surface_listen),
            Some(SurfaceMux {
                control_target: browser.as_ref().map(|runtime| runtime.control_address()),
                presentation_frame: presentation_frame.clone(),
                gate: HttpProxyGate::for_lease(lease, &self.config.runner_id),
                guest_surface_gateway: true,
            }),
        )?;
        let execution_id = format!("vm:{}:{}", lease.run_id, lease.id);
        ensure!(
            evolution.current_head().head.as_str() == command.expected_root_computation_ref,
            "hosted evolution authority root changed before the first operation"
        );
        let control = browser.as_ref().map(|runtime| runtime.control_capability());
        self.api.report_ready(
            &lease.id,
            ReadyReport {
                execution_id: &execution_id,
                ready_url: self.config.public_base_url.as_deref(),
                local_port: Some(ready_local_port(&self.config)),
                execution: None,
                port_mapping: None,
                control: control.as_ref().map(|capability| BrowserControlReport {
                    protocol: &capability.protocol,
                    port: &capability.port,
                }),
            },
        )?;
        let mut last_control = Instant::now() - Duration::from_secs(1);
        let mut last_frame = Instant::now();
        let mut last_heartbeat = Instant::now();
        loop {
            if last_frame.elapsed() >= Duration::from_millis(250) {
                if let (Some(browser), Some(frame)) =
                    (browser.as_mut(), presentation_frame.as_ref())
                {
                    *frame.write().map_err(|_| {
                        anyhow::anyhow!("Browser presentation frame lock poisoned")
                    })? = browser.capture_presentation_frame()?;
                }
                last_frame = Instant::now();
            }
            if last_control.elapsed() >= Duration::from_secs(1) {
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
                last_control = Instant::now();
            }
            if last_heartbeat.elapsed() >= ACTIVE_HEARTBEAT_INTERVAL {
                self.api.heartbeat(&self.config, 1)?;
                last_heartbeat = Instant::now();
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn execute_activity_lease(
        &self,
        lease: &ClaimedLease,
        command: &ActivityLeaseCommand,
        lease_root: &Path,
    ) -> Result<()> {
        let trace_started_at = Instant::now();
        emit_coop_trace(
            command.trace_id.as_deref(),
            "runner_accepted",
            trace_started_at,
        );
        let session = self
            .api
            .activity_executor_session(&command.activity_id, &command.activity_run_id)?;
        validate_activity_executor_session(&session, lease, command)?;
        emit_coop_trace(
            command.trace_id.as_deref(),
            "run_contract_verified",
            trace_started_at,
        );
        emit_coop_trace(
            command.trace_id.as_deref(),
            "materialization_started",
            trace_started_at,
        );
        let source = self.api.graph_source(
            &lease.id,
            &session.source.bundle_id,
            &session.source.computation_ref,
        )?;
        let index: ObjectGraphIndexV1 = serde_json::from_slice(source.index_bytes())
            .context("Activity runtime graph index is not valid JSON")?;
        let expectation = GraphDownloadExpectation {
            index_digest: source.index_digest().to_owned(),
            root_computation_ref: session.source.computation_ref.clone(),
            object_count: index.objects.len(),
            logical_bytes: index.logical_bytes()?,
        };
        let graph = download_and_validate_graph(&source, &expectation, lease_root)?;
        validate_exported_web_port(
            &graph,
            &session.source.computation_ref,
            &session.source.exported_port_id,
        )?;
        let evolution = Arc::new(initialize_hosted_run_evolution_authority(
            &graph,
            &session.source.computation_ref,
        )?);
        let firecracker_work_root = self.config.work_root.join("fc");
        let physical = RestorePhysicalConfig {
            firecracker_work_root: &firecracker_work_root,
            slot_id: &self.config.slot_id,
            hidden_surface_listen: self.config.hidden_surface_listen,
            guest_surface_target: self.config.surface_target,
            tap_host_cidr: self.config.tap_host_cidr.as_deref(),
        };
        let running = restore_portable_path(&graph, lease_root, &lease.id, &physical)?;
        emit_coop_trace(
            command.trace_id.as_deref(),
            "materialization_completed",
            trace_started_at,
        );
        self.api.report_status(&lease.id, "running")?;
        emit_coop_trace(command.trace_id.as_deref(), "run_started", trace_started_at);
        let browser_channel_scope = BrowserChannelScope {
            activity_id: session.activity_id.clone(),
            run_id: session.run_id.clone(),
            epoch: lease.id.clone(),
            expires_at_unix_seconds: OffsetDateTime::parse(&session.expires_at, &Rfc3339)
                .context("Activity executor expiry is not RFC3339")?
                .unix_timestamp(),
        };
        emit_coop_trace(
            command.trace_id.as_deref(),
            "adapter_runtime_start",
            trace_started_at,
        );
        emit_coop_trace(
            command.trace_id.as_deref(),
            "adapter_attach_start",
            trace_started_at,
        );
        let mut browser = start_hosted_browser_runtime(
            &graph,
            lease,
            lease_root,
            &lease_root.join("workspace"),
            evolution,
            self.api.clone(),
            self.config.browser_chrome.as_deref(),
            self.config.run_control_verification_key.as_deref(),
            Some(browser_channel_scope),
            &self.config.runner_id,
            &format!("http://{}/", self.config.hidden_surface_listen),
        )?
        .context("Activity source does not contain an explicit Browser Computation")?;
        emit_coop_trace(
            command.trace_id.as_deref(),
            "surface_transport_start",
            trace_started_at,
        );
        let controller = ActivityControllerServer::start(
            ActivityControllerPageConfig {
                run_id: session.run_id.clone(),
                room_url: session.room_url,
                executor_credential: session.executor_credential,
                ice_servers: serde_json::to_value(session.rtc.ice_servers)?,
            },
            browser.activity_ingress(),
            &lease_root.join("activity-operation-receipts"),
        )?;
        emit_coop_trace(
            command.trace_id.as_deref(),
            "adapter_handshake_complete",
            trace_started_at,
        );
        emit_coop_trace(
            command.trace_id.as_deref(),
            "adapter_ready",
            trace_started_at,
        );
        emit_coop_trace(command.trace_id.as_deref(), "run_ready", trace_started_at);
        browser.open_auxiliary_target(controller.target_url())?;
        let execution_id = format!("activity:{}:{}", lease.run_id, lease.id);
        // A restarted Browser context must not reuse an older Room surface
        // identity with an epoch reset to one. The lease identifies this
        // physical realization without becoming Run/Computation identity.
        let mut surface = BrowserSurfaceTracker::new(
            activity_surface_id(&session.run_id, &lease.id),
            session.run_id.clone(),
        );
        let mut ready = false;
        let mut last_control = Instant::now() - Duration::from_secs(1);
        let mut next_frame_at = Instant::now();
        let mut last_heartbeat = Instant::now();
        let mut last_surface = Instant::now() - Duration::from_secs(1);
        let result = (|| -> Result<()> {
            loop {
                refresh_activity_presentation_frame(
                    &mut next_frame_at,
                    || browser.capture_presentation_frame(),
                    |frame| controller.publish_frame(frame),
                )?;
                if last_surface.elapsed() >= Duration::from_millis(250) {
                    if let Ok(snapshot) = browser.webmcp_snapshot() {
                        let registry_generation = snapshot.registry_generation;
                        let document_token = snapshot.document_token.clone();
                        let observed_at = OffsetDateTime::now_utc().format(&Rfc3339)?;
                        let projection = surface.update(snapshot, observed_at)?.clone();
                        controller.publish_surface(
                            projection,
                            registry_generation,
                            document_token,
                        )?;
                    }
                    last_surface = Instant::now();
                }
                match controller.recv_timeout(Duration::from_millis(100)) {
                    Ok(ActivityControllerEvent::Ready) if !ready => {
                        self.api.report_activity_ready(&lease.id, &execution_id)?;
                        ready = true;
                    }
                    Ok(ActivityControllerEvent::Ready) => {}
                    Ok(ActivityControllerEvent::Ended) => break Ok(()),
                    Ok(ActivityControllerEvent::Failed) => {
                        eprintln!(
                            "activity controller reported failure: run_id={} lease={}",
                            lease.run_id, lease.id
                        );
                        break Err(anyhow::anyhow!("Activity controller failed"));
                    }
                    Ok(ActivityControllerEvent::AbortRequested {
                        operation_id,
                        result,
                    }) => {
                        let signaled = browser
                            .abort_webmcp_operation(&operation_id)
                            .unwrap_or(false);
                        let _ = result.send(signaled);
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        break Err(anyhow::anyhow!("Activity controller stopped unexpectedly"));
                    }
                }
                if last_control.elapsed() >= Duration::from_secs(1) {
                    if self.api.control(&lease.id)?.stop_requested {
                        break Ok(());
                    }
                    last_control = Instant::now();
                }
                if last_heartbeat.elapsed() >= ACTIVE_HEARTBEAT_INTERVAL {
                    self.api.heartbeat(&self.config, 1)?;
                    last_heartbeat = Instant::now();
                }
            }
        })();
        let mut shutdown_errors = Vec::new();
        if let Err(error) = result {
            shutdown_errors.push(format!("Activity execution failed: {error:#}"));
        }
        if let Err(error) = controller.stop() {
            shutdown_errors.push(format!("Activity controller shutdown failed: {error:#}"));
        }
        if let Err(error) = browser.stop() {
            shutdown_errors.push(format!("Activity Browser shutdown failed: {error:#}"));
        }
        if let Err(error) = running.quiesce() {
            shutdown_errors.push(format!("Activity realization quiesce failed: {error:#}"));
        }
        // A failed physical/controller loop must remain a failed lease. Sending
        // `stopped` first makes the later failure report terminal-conflict and
        // erases the only durable diagnostic evidence.
        if shutdown_errors.is_empty()
            && let Err(error) = self.api.report_stopped(&lease.id, &execution_id)
        {
            shutdown_errors.push(format!("Activity stopped report failed: {error:#}"));
        }
        if shutdown_errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(shutdown_errors.join("; "))
        }
    }
}

/// Read the control-plane stop fence before renewing authority to continue.
///
/// Setting desired state to stopped revokes the renewable authorization and
/// requests teardown as one control-plane operation. Reading the stop request
/// first lets the Runner acknowledge that intentional teardown cleanly. When
/// no stop was requested, continuation still requires a successful renewal.
fn poll_runtime_launch_control(
    mut stop_requested: impl FnMut() -> Result<bool>,
    mut refresh_execution_authorization: impl FnMut() -> Result<bool>,
) -> Result<bool> {
    if stop_requested()? {
        return Ok(true);
    }
    refresh_execution_authorization()
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

fn settle_lease_directory(lease_root: &Path, outcome: Result<()>) -> Result<()> {
    // The engine may have stopped but failed to commit state or report cleanup.
    // Preserve all working data until both the outcome and cleanup are proven.
    outcome?;
    cleanup_lease_directory(lease_root)
}

fn cleanup_lease_directory(lease_root: &Path) -> Result<()> {
    fs::remove_dir_all(lease_root)
        .with_context(|| format!("failed to clean lease directory {}", lease_root.display()))
}

fn activity_surface_id(run_id: &str, lease_id: &str) -> String {
    let scope = format!("ato.activity.surface.v0\0{run_id}\0{lease_id}");
    let digest = <Sha256 as sha2::Digest>::digest(scope.as_bytes());
    format!("surface_{}", URL_SAFE_NO_PAD.encode(digest))
}

impl WorkerConfig {
    /// Whether this host is configured to restore VM snapshots.
    ///
    /// One predicate, used by BOTH the capability advertisement and the VM
    /// restore path, so what the Runner claims and what it can do cannot
    /// drift apart.
    pub fn firecracker_configured(&self) -> bool {
        self.tap_host_cidr
            .as_deref()
            .is_some_and(|cidr| !cidr.trim().is_empty())
    }
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
        (1..=64).contains(&config.max_slots),
        "Runner max slots must be in [1, 64]"
    );
    // Firecracker configuration is validated when it is PRESENT, and its
    // absence is not an error: a Runner without it simply does not advertise
    // the VM-snapshot capability, so no VM lease is ever routed to it.
    if let Some(cidr) = config.tap_host_cidr.as_deref() {
        ensure!(
            !cidr.trim().is_empty() && cidr.contains('/'),
            "TAP host CIDR is invalid"
        );
    }
    if let Some(base) = config.public_base_url.as_deref() {
        let url = url::Url::parse(base).context("invalid public base URL")?;
        ensure!(
            matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
            "public base URL must be HTTP(S)"
        );
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "public base URL must not contain credentials, query or fragment"
        );
    }
    Ok(())
}

fn validate_lease(lease: &ClaimedLease, now: SystemTime) -> Result<()> {
    ensure!(valid_control_id(&lease.id), "invalid lease id");
    ensure!(valid_control_id(&lease.run_id), "invalid run id");
    match &lease.command {
        LeaseCommand::Portable(command) => {
            ensure!(command.bundle_id.starts_with("bnd_"), "invalid bundle id");
            ensure!(
                command.transport_digest.starts_with("sha256:"),
                "invalid transport digest"
            );
            ensure!(command.run_id == lease.run_id, "lease command run mismatch");
            ensure!(
                command.session_id == format!("run:{}", lease.run_id),
                "lease command session mismatch"
            );
            ensure!(
                !command.exported_port_id.is_empty(),
                "lease exported Port is empty"
            );
            ensure!(
                command.surface_contract_version == "1",
                "unsupported Surface contract version"
            );
            ensure!(
                command.session_surface.is_object()
                    && !command.accepted_session_surfaces.is_empty(),
                "lease Surface negotiation is incomplete"
            );
            ComputationRef::parse(&command.expected_root_computation_ref)
                .context("lease root ComputationRef is invalid")?;
        }
        LeaseCommand::RuntimeLaunch(command) => {
            ensure!(
                command.run_id == lease.run_id,
                "runtime launch lease Run identity mismatch"
            );
            ensure!(
                valid_control_id(&command.compute_instance_id),
                "runtime launch lease ComputeInstance scope is invalid"
            );
            ensure!(
                valid_sha256_digest(&command.launch_spec_digest),
                "runtime launch lease spec digest is invalid"
            );
        }
        LeaseCommand::VolumeMaintenance(command) => {
            command.validate(&lease.run_id)?;
            ensure!(
                valid_control_id(&command.compute_instance_id),
                "volume operation ComputeInstance scope is invalid"
            );
        }
        LeaseCommand::Activity(command) => {
            ensure!(
                valid_control_id(&command.activity_id)
                    && valid_control_id(&command.activity_run_id),
                "Activity lease scope is invalid"
            );
            ensure!(
                command.activity_run_id == lease.run_id,
                "Activity lease Run identity mismatch"
            );
            ensure!(
                command.trace_id.as_deref().is_none_or(valid_coop_trace_id),
                "Activity lease Coop trace identity is invalid"
            );
        }
    }
    if let Some(expires_at) = &lease.expires_at {
        let expiry =
            OffsetDateTime::parse(expires_at, &Rfc3339).context("lease expiry is not RFC3339")?;
        let now = OffsetDateTime::from(now);
        ensure!(expiry > now, "lease expired before execution");
    }
    Ok(())
}

fn validate_activity_executor_session(
    session: &ActivityExecutorSession,
    lease: &ClaimedLease,
    command: &ActivityLeaseCommand,
) -> Result<()> {
    ensure!(
        session.activity_id == command.activity_id
            && session.run_id == command.activity_run_id
            && session.run_id == lease.run_id,
        "Activity executor session escaped its lease scope"
    );
    ensure!(
        session.source.kind == "capsuleContinuation"
            && session.source.bundle_id.starts_with("bnd_")
            && valid_sha256_digest(&session.source.transport_digest)
            && !session.source.exported_port_id.trim().is_empty(),
        "Activity executor source is invalid"
    );
    ComputationRef::parse(&session.source.computation_ref)
        .context("Activity source ComputationRef is invalid")?;
    let room = url::Url::parse(&session.room_url).context("Activity Room URL is invalid")?;
    ensure!(
        matches!(room.scheme(), "ws" | "wss")
            && room.username().is_empty()
            && room.password().is_none()
            && !session.executor_credential.trim().is_empty(),
        "Activity realtime boundary is invalid"
    );
    let expiry = OffsetDateTime::parse(&session.expires_at, &Rfc3339)
        .context("Activity executor expiry is not RFC3339")?;
    ensure!(
        expiry > OffsetDateTime::now_utc(),
        "Activity executor session expired before execution"
    );
    ensure!(
        session.rtc.ice_servers.iter().all(|server| {
            !server.urls.is_empty()
                && server
                    .urls
                    .iter()
                    .all(|url| url.starts_with("stun:") || url.starts_with("stuns:"))
        }),
        "Activity executor session contains a non-STUN ICE server"
    );
    Ok(())
}

fn valid_sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn validate_exported_web_port(
    graph: &ValidatedRuntimeGraph,
    expected_root: &str,
    exported_port_id: &str,
) -> Result<()> {
    let root_ref = ComputationRef::parse(expected_root)?;
    let root = resolve_computation(graph.objects(), &root_ref)?;
    let port = PortId::parse(exported_port_id).context("exported Port id is invalid")?;
    let definition = root
        .object()
        .boundary
        .get(&port)
        .context("exported Port is absent from the root Computation")?;
    ensure!(
        definition.protocol.as_str() == "ato.http@1" && definition.role.as_str() == "server",
        "exported Port is not the realized Web server boundary"
    );
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
    /// Firecracker-only; `None` on a host without it. The source/replay path
    /// never reads this.
    tap_host_cidr: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceAuthoringState {
    version: u32,
    config: SourceAuthoringConfig,
    workspace_snapshot: String,
    #[serde(default)]
    semantic_frontier: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceAuthoringConfig {
    schema: u32,
    process: Vec<SourceProcessConfig>,
    adapter: Vec<SourceAdapterConfig>,
    port: Vec<SourcePortConfig>,
    connection: Vec<serde_json::Value>,
    binding: Vec<serde_json::Value>,
    workspace: serde_json::Value,
    encap: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceProcessConfig {
    id: String,
    command: Vec<String>,
    cwd: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceAdapterConfig {
    #[serde(rename = "use")]
    use_adapter: String,
    target: Option<String>,
    port: Option<String>,
    listen: Option<String>,
    upstream: Option<String>,
    input: Option<String>,
    ready_path: Option<String>,
    config: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourcePortConfig {
    id: String,
    node: String,
    protocol: String,
    role: String,
    address: Option<String>,
    environment: Option<String>,
    internal: bool,
}

#[derive(Debug, Clone)]
struct SourceLaunchSpec {
    command: Vec<String>,
    cwd: PathBuf,
    listen_environment: String,
    application_address: SocketAddr,
    workspace: PathBuf,
    relay_script: PathBuf,
    surface_socket: PathBuf,
    hidden_surface_listen: SocketAddr,
}

const SOURCE_RELAY_SCRIPT: &str = r#"import os
import selectors
import signal
import socket
import subprocess
import sys
import threading
import time

surface = sys.argv[1]
host, port_text = sys.argv[2].rsplit(':', 1)
command = sys.argv[4:]
if not command:
    raise SystemExit('source command is empty')
child = subprocess.Popen(command)
stopping = False

def stop(_signal=None, _frame=None):
    global stopping
    stopping = True
    if child.poll() is None:
        child.terminate()

signal.signal(signal.SIGTERM, stop)
signal.signal(signal.SIGINT, stop)
deadline = time.monotonic() + 30
while time.monotonic() < deadline and child.poll() is None:
    try:
        probe = socket.create_connection((host, int(port_text)), timeout=0.2)
        probe.close()
        break
    except OSError:
        time.sleep(0.05)
else:
    stop()
    child.wait(timeout=5)
    raise SystemExit('source application did not become ready')

try:
    os.unlink(surface)
except FileNotFoundError:
    pass
listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
listener.bind(surface)
os.chmod(surface, 0o600)
listener.listen(16)
listener.settimeout(0.2)

def proxy(client):
    upstream = socket.create_connection((host, int(port_text)), timeout=5)
    client.setblocking(False)
    upstream.setblocking(False)
    selector = selectors.DefaultSelector()
    selector.register(client, selectors.EVENT_READ, upstream)
    selector.register(upstream, selectors.EVENT_READ, client)
    try:
        while True:
            events = selector.select(timeout=5)
            if not events and stopping:
                return
            for key, _ in events:
                data = key.fileobj.recv(65536)
                if not data:
                    return
                key.data.sendall(data)
    finally:
        selector.close()
        client.close()
        upstream.close()

try:
    while not stopping and child.poll() is None:
        try:
            client, _ = listener.accept()
        except TimeoutError:
            continue
        threading.Thread(target=proxy, args=(client,), daemon=True).start()
finally:
    listener.close()
    try:
        os.unlink(surface)
    except FileNotFoundError:
        pass
    stop()
    try:
        child.wait(timeout=5)
    except subprocess.TimeoutExpired:
        child.kill()
        child.wait()
raise SystemExit(child.returncode or 0)
"#;

fn source_launch_spec(
    graph: &ValidatedRuntimeGraph,
    lease_root: &Path,
    hidden_surface_listen: SocketAddr,
) -> Result<SourceLaunchSpec> {
    let root = ComputationRef::parse(&graph.report().root_computation_ref)?;
    let mut leaves = Vec::new();
    collect_authoring_leaves(&root, graph.objects(), &mut leaves)?;
    let [authoring] = leaves.as_slice() else {
        bail!("source Replay requires exactly one authored process computation");
    };
    let resolved = resolve_computation(graph.objects(), authoring)?;
    let metadata = graph.objects().metadata(&resolved.object().residual)?;
    let bytes = read_exact_object(
        graph.objects(),
        &resolved.object().residual,
        metadata.size,
        16 * 1024 * 1024,
    )?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    ensure!(
        serde_jcs::to_vec(&value)? == bytes,
        "source authoring state is non-canonical"
    );
    let state: SourceAuthoringState = serde_json::from_value(value)?;
    ensure!(
        state.version == 1,
        "source authoring state version is unsupported"
    );
    ensure!(
        state.config.schema == 1,
        "source authoring schema is unsupported"
    );
    ensure!(
        state.semantic_frontier.is_none(),
        "source Replay with an evolved semantic frontier requires a captured VM"
    );
    ensure!(
        state.config.connection.is_empty() && state.config.binding.is_empty(),
        "source Replay v0 does not admit connections or runtime Bindings"
    );
    ensure!(
        state.config.adapter.iter().all(|adapter| {
            let _ = (
                &adapter.target,
                &adapter.port,
                &adapter.listen,
                &adapter.upstream,
                &adapter.input,
                &adapter.ready_path,
                &adapter.config,
            );
            adapter.use_adapter == "ato.process@1"
        }),
        "source Replay v0 admits only the process Adapter inside the sandbox"
    );
    let [process] = state.config.process.as_slice() else {
        bail!("source Replay v0 requires exactly one process");
    };
    ensure!(
        !process.command.is_empty(),
        "source process command is empty"
    );
    ensure!(
        !process.cwd.is_absolute()
            && process.cwd.components().all(|component| matches!(
                component,
                std::path::Component::CurDir | std::path::Component::Normal(_)
            )),
        "source process cwd escapes the restored workspace"
    );
    let web_ports = state
        .config
        .port
        .iter()
        .filter(|port| !port.internal && port.protocol == "ato.http@1" && port.role == "server")
        .collect::<Vec<_>>();
    let [web_port] = web_ports.as_slice() else {
        bail!("source Replay v0 requires exactly one exported ato.http@1 server Port");
    };
    ensure!(
        web_port.node == process.id && !web_port.id.is_empty(),
        "source HTTP Port is not owned by the selected process"
    );
    let application_address: SocketAddr = web_port
        .address
        .as_deref()
        .context("source HTTP Port requires an address")?
        .parse()?;
    ensure!(
        application_address.ip().is_loopback(),
        "source HTTP Port must bind loopback"
    );
    let listen_environment = web_port
        .environment
        .clone()
        .context("source HTTP Port requires an environment projection")?;
    ensure!(
        !listen_environment.is_empty(),
        "source HTTP environment name is empty"
    );
    let _ = (&state.config.workspace, &state.config.encap);
    let workspace = lease_root.join("workspace");
    let snapshot = ContentRef::parse(state.workspace_snapshot)?;
    restore_workspace(&snapshot, &workspace, graph.objects())?;
    let relay_script = lease_root.join("source-relay.py");
    fs::write(&relay_script, SOURCE_RELAY_SCRIPT)?;
    let surface_socket = workspace.join("surface.sock");
    Ok(SourceLaunchSpec {
        command: process.command.clone(),
        cwd: process.cwd.clone(),
        listen_environment,
        application_address,
        workspace,
        relay_script,
        surface_socket,
        hidden_surface_listen,
    })
}

fn collect_authoring_leaves(
    reference: &ComputationRef,
    objects: &dyn ObjectResolver,
    leaves: &mut Vec<ComputationRef>,
) -> Result<()> {
    let resolved = resolve_computation(objects, reference)?;
    if resolved.object().semantics == SemanticsId::parse(AUTHORING_SEMANTICS_ID)? {
        leaves.push(reference.clone());
        return Ok(());
    }
    if resolved.object().semantics == SemanticsId::parse(BROWSER_COMPUTATION_SEMANTICS_ID)? {
        return Ok(());
    }
    ensure!(
        resolved.object().semantics == SemanticsId::parse(COMPOSE_SEMANTICS_ID)?,
        "source Replay graph contains an unsupported computation leaf"
    );
    let metadata = objects.metadata(&resolved.object().residual)?;
    let bytes = read_exact_object(
        objects,
        &resolved.object().residual,
        metadata.size,
        ato_compose::MAX_COMPOSITE_RESIDUAL_BYTES,
    )?;
    for child in decode_composite_residual(&bytes)?.nodes.values() {
        collect_authoring_leaves(child, objects, leaves)?;
    }
    Ok(())
}

struct SourceReplayDriver {
    expected_root: ComputationRef,
    spec: SourceLaunchSpec,
}

impl RealizationDriver for SourceReplayDriver {
    fn begin(&self, anchor: &ComputationRef) -> Result<Box<dyn ReplayRuntime>, MaterializerError> {
        if anchor != &self.expected_root {
            return Err(MaterializerError::Operation(
                "source Replay anchor does not match the graph root".to_owned(),
            ));
        }
        Ok(Box::new(SourceReplayRuntime {
            expected_root: self.expected_root.clone(),
            spec: self.spec.clone(),
        }))
    }

    fn preflight_operations(&self, records: &[RecordEnvelopeV2]) -> Result<(), MaterializerError> {
        if records.is_empty() {
            Ok(())
        } else {
            Err(MaterializerError::OperationReplayUnsupported)
        }
    }

    fn begin_operations(
        &self,
        anchor: &ComputationRef,
    ) -> Result<Box<dyn OperationReplayRuntime>, MaterializerError> {
        if anchor != &self.expected_root {
            return Err(MaterializerError::Operation(
                "source Replay anchor does not match the graph root".to_owned(),
            ));
        }
        Ok(Box::new(SourceOperationReplayRuntime {
            expected_root: self.expected_root.clone(),
            spec: self.spec.clone(),
        }))
    }
}

struct SourceReplayRuntime {
    expected_root: ComputationRef,
    spec: SourceLaunchSpec,
}

impl ReplayRuntime for SourceReplayRuntime {
    fn apply(&mut self, _record: &RecordEnvelope) -> Result<(), MaterializerError> {
        Err(MaterializerError::OperationReplayUnsupported)
    }

    fn finish(
        self: Box<Self>,
        target: &ComputationRef,
    ) -> Result<Box<dyn Realization>, MaterializerError> {
        source_realization(self.expected_root, target, self.spec)
    }
}

struct SourceOperationReplayRuntime {
    expected_root: ComputationRef,
    spec: SourceLaunchSpec,
}

impl OperationReplayRuntime for SourceOperationReplayRuntime {
    fn apply(&mut self, _record: &RecordEnvelopeV2) -> Result<(), MaterializerError> {
        Err(MaterializerError::OperationReplayUnsupported)
    }

    fn finish(
        self: Box<Self>,
        target: &ComputationRef,
    ) -> Result<Box<dyn Realization>, MaterializerError> {
        source_realization(self.expected_root, target, self.spec)
    }
}

fn source_realization(
    expected_root: ComputationRef,
    target: &ComputationRef,
    spec: SourceLaunchSpec,
) -> Result<Box<dyn Realization>, MaterializerError> {
    if target != &expected_root {
        return Err(MaterializerError::Operation(
            "source Replay target does not match the graph root".to_owned(),
        ));
    }
    Ok(Box::new(SourceProcessRealization {
        target: expected_root,
        spec,
        child: None,
        hidden_proxy: None,
    }))
}

struct SourceProcessRealization {
    target: ComputationRef,
    spec: SourceLaunchSpec,
    child: Option<Child>,
    hidden_proxy: Option<TcpProxy>,
}

impl SourceProcessRealization {
    fn spawn(&self) -> Result<Child> {
        let bwrap = Path::new("/usr/bin/bwrap");
        ensure!(bwrap.is_file(), "source Replay requires /usr/bin/bwrap");
        let workspace = self.spec.workspace.canonicalize()?;
        let relay = self.spec.relay_script.canonicalize()?;
        let cwd = Path::new("/workspace").join(&self.spec.cwd);
        let mut command = Command::new(bwrap);
        command.args([
            "--die-with-parent",
            "--new-session",
            "--unshare-all",
            "--clearenv",
            "--cap-drop",
            "ALL",
            "--tmpfs",
            "/",
        ]);
        for path in ["/usr", "/lib", "/lib64", "/bin", "/sbin"] {
            if Path::new(path).exists() {
                command.args(["--ro-bind", path, path]);
            }
        }
        for path in ["/etc/ld.so.cache", "/etc/ld.so.conf", "/etc/ld.so.conf.d"] {
            if Path::new(path).exists() {
                command.args(["--ro-bind", path, path]);
            }
        }
        command
            .args(["--dir", "/opt", "--dir", "/opt/ato"])
            .arg("--ro-bind")
            .arg(relay)
            .arg("/opt/ato/source-relay.py")
            .args(["--dir", "/workspace"])
            .arg("--bind")
            .arg(workspace)
            .arg("/workspace")
            .args([
                "--proc",
                "/proc",
                "--dev",
                "/dev",
                "--tmpfs",
                "/tmp",
                "--dir",
                "/home",
                "--dir",
                "/home/ato",
                "--chdir",
            ])
            .arg(cwd)
            .args([
                "--setenv",
                "HOME",
                "/home/ato",
                "--setenv",
                "TMPDIR",
                "/tmp",
            ])
            .args(["--setenv", "PATH", "/usr/bin:/bin"])
            .arg("--setenv")
            .arg(&self.spec.listen_environment)
            .arg(self.spec.application_address.to_string())
            .args([
                "/usr/bin/python3",
                "/opt/ato/source-relay.py",
                "/workspace/surface.sock",
            ])
            .arg(self.spec.application_address.to_string())
            .arg("--")
            .args(&self.spec.command)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        command.spawn().context("start source Replay sandbox")
    }
}

impl Realization for SourceProcessRealization {
    fn target(&self) -> &ComputationRef {
        &self.target
    }

    fn activate(&mut self) -> Result<(), MaterializerError> {
        if self.child.is_some() {
            return Ok(());
        }
        let mut child = self
            .spawn()
            .map_err(|error| MaterializerError::Operation(error.to_string()))?;
        let deadline = Instant::now() + SOURCE_READY_TIMEOUT;
        while Instant::now() < deadline {
            if self.spec.surface_socket.exists() {
                let proxy = TcpProxy::start(
                    self.spec.hidden_surface_listen,
                    ProxyTarget::Unix(self.spec.surface_socket.clone()),
                )
                .map_err(|error| MaterializerError::Operation(error.to_string()))?;
                self.child = Some(child);
                self.hidden_proxy = Some(proxy);
                return Ok(());
            }
            if let Some(status) = child
                .try_wait()
                .map_err(|error| MaterializerError::Operation(error.to_string()))?
            {
                return Err(MaterializerError::Operation(format!(
                    "source Replay sandbox exited before readiness: {status}"
                )));
            }
            thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        Err(MaterializerError::Operation(
            "source Replay sandbox did not publish its Surface".to_owned(),
        ))
    }

    fn publish(&mut self) -> Result<(), MaterializerError> {
        Ok(())
    }

    fn wait(&mut self) -> Result<(), MaterializerError> {
        let status = self
            .child
            .as_mut()
            .ok_or_else(|| MaterializerError::Operation("source Replay is inactive".to_owned()))?
            .wait()
            .map_err(|error| MaterializerError::Operation(error.to_string()))?;
        if status.success() {
            Ok(())
        } else {
            Err(MaterializerError::Operation(format!(
                "source Replay sandbox exited: {status}"
            )))
        }
    }

    fn quiesce(&mut self) -> Result<(), MaterializerError> {
        self.hidden_proxy.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            child
                .wait()
                .map_err(|error| MaterializerError::Operation(error.to_string()))?;
        }
        match fs::remove_file(&self.spec.surface_socket) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(MaterializerError::Operation(error.to_string())),
        }
        Ok(())
    }
}

fn restore_source_replay_path(
    graph: &ValidatedRuntimeGraph,
    lease_root: &Path,
    lease_id: &str,
    physical: &RestorePhysicalConfig<'_>,
) -> Result<AcceptedRealization> {
    let root = ComputationRef::parse(&graph.report().root_computation_ref)?;
    let spec = source_launch_spec(graph, lease_root, physical.hidden_surface_listen)?;
    let driver = SourceReplayDriver {
        expected_root: root.clone(),
        spec,
    };
    let workspace = lease_root.join("workspace");
    let workspace_policy = WorkspaceCapturePolicy::secure_default();
    let adapters = AdapterRegistry::default();
    let mut materializers = MaterializerRegistry::default();
    materializers.register(Arc::new(ReplayMaterializer))?;
    materializers.register(Arc::new(ReplayMaterializerV2))?;
    let actuator_providers = ActuatorProviderRegistry::default();
    let context = MaterializerContext {
        objects: graph.objects(),
        adapters: &adapters,
        records: &[],
        records_v2: &[],
        replay_anchor: None,
        record_frontier_ref: None,
        workspace: &workspace,
        workspace_policy: &workspace_policy,
        realization: Some(&driver),
        contracts: &[],
        runner_capabilities: None,
    };
    let environment = TargetEnvironment {
        id: format!("hosted-source:{lease_id}"),
        placement: Placement::Hosted,
        trust_boundary: TrustBoundary::TenantIsolated,
    };
    let candidates = graph
        .index()
        .materializations
        .iter()
        .filter(|candidate| {
            candidate.id == REPLAY_MATERIALIZER_ID || candidate.id == REPLAY_MATERIALIZER_V2_ID
        })
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
                    record_frontier_ref: context.record_frontier_ref,
                    workspace: context.workspace,
                    workspace_policy: context.workspace_policy,
                    realization: context.realization,
                    contracts: context.contracts,
                    runner_capabilities: context.runner_capabilities,
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        !candidates.is_empty(),
        "graph has neither a VM nor source Replay Materialization candidate"
    );
    let contract_verifiers = ContractVerifierRegistry::default();
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
        .context("Planner returned no source Replay path")?;
    let materializer = materializers.get(&selected.materializer_id)?;
    let contracts = materializer.contracts(&selected.descriptor_ref, &context)?;
    let realization = materializer.restore(&selected.descriptor_ref, &context)?;
    ensure!(
        realization.target() == &root,
        "source Replay target mismatch"
    );
    accept_candidate(
        realization,
        &contracts,
        &contract_verifiers,
        &ContractContext {
            objects: graph.objects(),
            workspace: &workspace,
        },
    )
    .map_err(Into::into)
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

#[allow(clippy::too_many_arguments)]
fn start_hosted_browser_runtime(
    graph: &ValidatedRuntimeGraph,
    lease: &ClaimedLease,
    lease_root: &Path,
    workspace: &Path,
    evolution: Arc<RunEvolutionAuthority>,
    api: HttpRunnerApi,
    chrome: Option<&Path>,
    run_control_verification_key: Option<&str>,
    channel_scope: Option<BrowserChannelScope>,
    runner_id: &str,
    browser_target_url: &str,
) -> Result<Option<HostedBrowserRuntime>> {
    let root = ComputationRef::parse(&graph.report().root_computation_ref)?;
    let Some(binding) = hosted_browser_binding(&root, graph.objects())? else {
        return Ok(None);
    };
    let chrome = chrome.context(
        "Browser-aware Hosted Run requires ATO_BROWSER_CHROME to name an absolute Chrome executable",
    )?;
    let run_control_verification_key = run_control_verification_key
        .context("Browser-aware Hosted Run requires ATO_RUN_CONTROL_VERIFICATION_KEY")?;
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
            channel_scope,
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
    let live_dispatcher = adapter
        .lock()
        .map_err(|_| anyhow::anyhow!("Browser Adapter session mutex poisoned"))?
        .live_operation_dispatcher()
        .context("Browser Adapter does not expose concurrent live dispatch")?;
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
        AttachedBrowserActuator(live_dispatcher),
        RunnerBrowserHeadPersistence {
            api,
            lease_id: lease.id.clone(),
        },
        RunnerBrowserRecordSubmission {
            stylus: pipeline.stylus.clone(),
            port: binding.port.clone(),
            stream: format!("browser-{}", lease.id),
            next_local_seq: Arc::new(AtomicU64::new(0)),
            record_refs: Arc::new(Mutex::new(BTreeMap::new())),
        },
    ));
    let control_capability = BrowserControlCapability {
        protocol: BROWSER_PROTOCOL_ID.to_owned(),
        port: binding.port.to_string(),
    };
    let control = match RunControlServer::start(
        Arc::clone(&ingress),
        lease.run_id.clone(),
        lease.id.clone(),
        runner_id.to_owned(),
        control_capability.clone(),
        run_control_verification_key.to_owned(),
    ) {
        Ok(control) => control,
        Err(error) => {
            if let Ok(mut adapter) = adapter.lock() {
                let context = AdapterContext {
                    workspace,
                    objects: graph.objects(),
                };
                let _ = adapter.detach(&context);
            }
            drop(adapter);
            let _ = host.stop();
            let _ = pipeline.shutdown();
            return Err(error);
        }
    };
    Ok(Some(HostedBrowserRuntime {
        ingress: Some(ingress),
        control: Some(control),
        control_capability,
        adapter: Some(adapter),
        host: Some(host),
        pipeline: Some(pipeline),
        objects: Arc::new(graph.objects().clone()),
        workspace: workspace.to_owned(),
    }))
}

fn restore_portable_path(
    graph: &ValidatedRuntimeGraph,
    lease_root: &Path,
    lease_id: &str,
    physical: &RestorePhysicalConfig<'_>,
) -> Result<AcceptedRealization> {
    if graph
        .index()
        .materializations
        .iter()
        .any(|candidate| candidate.id == VM_SNAPSHOT_MATERIALIZER_ID)
    {
        restore_vm_path(graph, lease_root, lease_id, physical)
    } else {
        restore_source_replay_path(graph, lease_root, lease_id, physical)
    }
}

fn restore_vm_path(
    graph: &ValidatedRuntimeGraph,
    lease_root: &Path,
    lease_id: &str,
    physical: &RestorePhysicalConfig<'_>,
) -> Result<AcceptedRealization> {
    // The Firecracker requirement belongs HERE, on the path that actually
    // needs it — not at startup, where it excluded hosts that only ever serve
    // the source/replay path. A Runner without it never advertises the
    // VM-snapshot capability, so reaching this point means the control plane
    // routed a VM lease to a Runner that said it could take one.
    let tap_host_cidr = physical.tap_host_cidr.ok_or_else(|| {
        anyhow::anyhow!(
            "this Runner has no Firecracker configuration (ATO_FC_TAP_HOST_CIDR)              and cannot restore a VM-snapshot Capsule"
        )
    })?;
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
        tap_host_cidr: Some(tap_host_cidr.to_owned()),
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
    command: LeaseCommand,
    expires_at: Option<String>,
    /// Runner-scoped HMAC key for the HTTP proxy-origin gate, delivered by the
    /// control plane inside the claim. Absent means the API does not mint
    /// assertions for this Runner — the gate stays off, which is the only
    /// ordering-safe read (an enforcing gate with a non-signing API would
    /// refuse every request).
    #[serde(default)]
    proxy_assertion_key: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
pub(crate) enum LeaseCommand {
    #[serde(rename = "portable_capsule_v2")]
    Portable(PortableLeaseCommand),
    #[serde(rename = "activity_browser_executor_v0")]
    Activity(ActivityLeaseCommand),
    #[serde(rename = "runtime_launch")]
    RuntimeLaunch(runtime_launch::lease::RuntimeLaunchLeaseCommand),
    #[serde(rename = "state_volume_maintenance")]
    VolumeMaintenance(runtime_launch::volume_maintenance::VolumeMaintenanceLeaseCommand),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PortableLeaseCommand {
    bundle_id: String,
    transport_digest: String,
    expected_root_computation_ref: String,
    run_id: String,
    session_id: String,
    exported_port_id: String,
    surface_contract_version: String,
    session_surface: serde_json::Value,
    accepted_session_surfaces: Vec<serde_json::Value>,
    #[serde(default)]
    trace_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivityLeaseCommand {
    activity_id: String,
    activity_run_id: String,
    #[serde(default)]
    trace_id: Option<String>,
}

fn valid_coop_trace_id(value: &str) -> bool {
    value.len() == 37
        && value.starts_with("coop_")
        && value[5..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn emit_coop_trace(trace_id: Option<&str>, stage: &str, started_at: Instant) {
    let Some(trace_id) = trace_id.filter(|value| valid_coop_trace_id(value)) else {
        return;
    };
    eprintln!(
        "{}",
        serde_json::json!({
            "event":"ato.coop.trace",
            "trace_id":trace_id,
            "component":"connected-realization-worker",
            "stage":stage,
            "elapsed_ms":(started_at.elapsed().as_secs_f64() * 10000.0).round() / 10.0,
        })
    );
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActivityExecutorSession {
    activity_id: String,
    run_id: String,
    source: ActivityCapsuleSource,
    #[serde(rename = "experienceUrl")]
    _experience_url: String,
    #[serde(rename = "experienceOrigin")]
    _experience_origin: String,
    #[serde(rename = "experienceManifestDigest")]
    _experience_manifest_digest: String,
    room_url: String,
    executor_credential: String,
    expires_at: String,
    rtc: ActivityRtcConfiguration,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActivityCapsuleSource {
    kind: String,
    bundle_id: String,
    transport_digest: String,
    computation_ref: String,
    exported_port_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ActivityRtcConfiguration {
    ice_servers: Vec<ActivityIceServer>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ActivityIceServer {
    urls: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ControlResponse {
    stop_requested: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionAuthorizationRenewalResponse {
    ok: bool,
    policy_generation: u64,
    authorization_generation: u64,
    server_time: String,
    expires_at: String,
    renew_after_secs: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionAuthorizationRefusalResponse {
    error: String,
    reason: String,
    stop_requested: bool,
    server_time: String,
    message: String,
}

use runtime_launch::lease::ExecutionAuthorizationRenewalOutcome;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeBindingsResponse {
    bindings: BTreeMap<String, String>,
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

#[derive(Serialize)]
struct BrowserControlReport<'a> {
    protocol: &'a str,
    port: &'a str,
}

#[derive(Serialize)]
struct OciPortMappingReport {
    container_ip: String,
    guest_port: u16,
    runner_forward_port: u16,
}

#[derive(Serialize)]
struct ReadyReport<'a> {
    execution_id: &'a str,
    /// Omitted when the realization has no externally reachable URL.
    ///
    /// The control plane stores a ready_url ONLY when the Runner proved it is
    /// non-loopback and under the Runner's public base, and treats an absent
    /// one as an honest "ready, no URL". A process realization has neither a
    /// stable host nor an ingress until P4, so reporting a loopback URL — or
    /// worse, synthesizing a public one that serves nothing — would be a lie
    /// the API is right to refuse.
    #[serde(skip_serializing_if = "Option::is_none")]
    ready_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    local_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution: Option<&'a runtime_launch::lease::RuntimeExecutionEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    port_mapping: Option<&'a OciPortMappingReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    control: Option<BrowserControlReport<'a>>,
}

#[derive(Clone)]
pub struct HttpRunnerApi {
    client: Client,
    base: String,
    runner_id: String,
    token: String,
    /// Whether this worker has a usable persistent volume store.
    persistent_volumes: bool,
}

impl HttpRunnerApi {
    pub fn new(base: &str, runner_id: &str, token: &str) -> Result<Self> {
        Ok(Self {
            client: Client::builder().timeout(Duration::from_secs(60)).build()?,
            base: base.trim_end_matches('/').to_owned(),
            runner_id: runner_id.to_owned(),
            token: token.to_owned(),
            persistent_volumes: false,
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
            "capabilities": runner_capabilities(
                config.firecracker_configured(),
                ato_adapter_oci::docker_runtime_available(),
                self.persistent_volumes,
                config.network_controls,
                !config.fixed_tcp_allowlist.trim().is_empty(),
            ),
            "supported_lease_kinds": supported_lease_kinds(config, self.persistent_volumes),
            "supported_session_surfaces": [{
                "kind": "web",
                "profiles": ["ato.web-surface.v1"],
                "transports": ["https"]
            }],
            "public_base_url": config.public_base_url,
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "max_slots": config.max_slots,
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

    fn redeem_runtime_bindings(
        &self,
        lease_id: &str,
        grants: &[ato_ipc::runtime_launch::SecretGrantV1],
    ) -> Result<Vec<runtime_launch::resolved::ResolvedSecret>> {
        if grants.is_empty() {
            return Ok(Vec::new());
        }
        let mut response = self
            .authorized(self.client.post(format!(
                "{}/v1/runner-leases/{lease_id}/runtime-bindings",
                self.base
            )))
            .send()?
            .error_for_status()?
            .json::<RuntimeBindingsResponse>()?;
        if response.bindings.len() != grants.len() {
            bail!("runtime Binding grant did not match the launch spec");
        }
        grants
            .iter()
            .map(|grant| {
                response
                    .bindings
                    .remove(&grant.name)
                    .map(|value| runtime_launch::resolved::ResolvedSecret::new(&grant.name, value))
                    .context("runtime Binding grant omitted a declared name")
            })
            .collect()
    }

    fn report_activity_ready(&self, lease_id: &str, execution_id: &str) -> Result<()> {
        self.authorized(
            self.client
                .post(format!("{}/v1/runner-leases/{lease_id}/status", self.base)),
        )
        .json(&serde_json::json!({
            "status": "ready",
            "execution_id": execution_id,
        }))
        .send()?
        .error_for_status()?;
        Ok(())
    }

    fn report_failed(&self, lease_id: &str, code: &str, message: &str) -> Result<()> {
        let message = truncate(message, 2000);
        let mut last_error = None;
        for delay in TERMINAL_REPORT_RETRY_DELAYS {
            if !delay.is_zero() {
                thread::sleep(delay);
            }
            match self
                .authorized(
                    self.client
                        .post(format!("{}/v1/runner-leases/{lease_id}/status", self.base)),
                )
                .json(&serde_json::json!({
                    "status": "failed",
                    "error": { "code": code, "message": message }
                }))
                .send()
                .and_then(reqwest::blocking::Response::error_for_status)
            {
                Ok(_) => return Ok(()),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error
            .context("terminal failure report retry set is empty")?
            .into())
    }

    fn report_ready(&self, lease_id: &str, report: ReadyReport<'_>) -> Result<()> {
        self.authorized(
            self.client
                .post(format!("{}/v1/runner-leases/{lease_id}/ready", self.base)),
        )
        .json(&report)
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

    fn renew_execution_authorization(
        &self,
        lease_id: &str,
    ) -> Result<ExecutionAuthorizationRenewalOutcome> {
        let response = self
            .authorized(self.client.post(format!(
                "{}/v1/runner-leases/{lease_id}/execution-authorization/renew",
                self.base
            )))
            .send()?;
        if response.status() == reqwest::StatusCode::CONFLICT {
            let refusal = response.json::<ExecutionAuthorizationRefusalResponse>()?;
            ensure!(
                refusal.error == "execution_authorization_refused",
                "unexpected execution authorization refusal: {}",
                refusal.error
            );
            ensure!(
                !refusal.server_time.is_empty() && !refusal.message.is_empty(),
                "execution authorization refusal omitted Coordinator evidence"
            );
            return Ok(ExecutionAuthorizationRenewalOutcome::Refused {
                reason: refusal.reason,
                stop_requested: refusal.stop_requested,
            });
        }
        let renewal = response
            .error_for_status()?
            .json::<ExecutionAuthorizationRenewalResponse>()?;
        ensure!(
            renewal.ok,
            "execution authorization renewal was not accepted"
        );
        Ok(ExecutionAuthorizationRenewalOutcome::Renewed(
            runtime_launch::lease::ExecutionAuthorizationRenewal {
                policy_generation: renewal.policy_generation,
                authorization_generation: renewal.authorization_generation,
                server_time: renewal.server_time,
                expires_at: renewal.expires_at,
                renew_after_secs: renewal.renew_after_secs,
            },
        ))
    }

    fn report_stopped(&self, lease_id: &str, execution_id: &str) -> Result<()> {
        let mut last_error = None;
        for delay in TERMINAL_REPORT_RETRY_DELAYS {
            if !delay.is_zero() {
                thread::sleep(delay);
            }
            match self
                .authorized(
                    self.client
                        .post(format!("{}/v1/runner-leases/{lease_id}/stopped", self.base)),
                )
                .json(&serde_json::json!({ "execution_id": execution_id }))
                .send()
                .and_then(reqwest::blocking::Response::error_for_status)
            {
                Ok(_) => return Ok(()),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error
            .context("terminal stopped report retry set is empty")?
            .into())
    }

    fn activity_executor_session(
        &self,
        activity_id: &str,
        activity_run_id: &str,
    ) -> Result<ActivityExecutorSession> {
        Ok(self
            .authorized(self.client.post(format!(
                "{}/v1/activities/{activity_id}/runs/{activity_run_id}/executor-session",
                self.base
            )))
            .send()?
            .error_for_status()?
            .json()?)
    }

    fn graph_source(
        &self,
        lease_id: &str,
        expected_bundle: &str,
        expected_root: &str,
    ) -> Result<LeaseGraphSource> {
        LeaseGraphSource::load(
            self.client.clone(),
            self.base.clone(),
            self.token.clone(),
            lease_id,
            expected_bundle,
            expected_root,
        )
    }
}

impl runtime_launch::recovery::RecoveryReporter for HttpRunnerApi {
    /// Tell the control plane what recovery found for one lease. A lease the
    /// control plane no longer knows has nothing to release: that is an
    /// acknowledgement, not a failure.
    fn report_recovery(
        &self,
        report: &runtime_launch::recovery::LeaseRecoveryReport,
    ) -> Result<()> {
        let response = self
            .authorized(self.client.post(format!(
                "{}/v1/runner-leases/{}/recovery",
                self.base, report.lease_id
            )))
            .json(report)
            .send()?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        response
            .error_for_status()
            .context("control plane refused the recovery report")?;
        Ok(())
    }
}

fn supported_lease_kinds(config: &WorkerConfig, persistent_volumes: bool) -> Vec<&'static str> {
    let mut kinds = vec![PORTABLE_CAPSULE_LEASE_KIND];
    // Only advertised where the workload can actually be contained. A Runner
    // that took `runtime_launch` leases it must then refuse would look
    // available to the scheduler and fail every Run it won.
    // A slot still holding an unconfirmed previous Run takes no new runtime
    // work: its old workload may still be writing.
    if runtime_launch::lease::RUNTIME_LAUNCH_SUPPORTED()
        && runtime_launch::recovery::slot_recovered()
    {
        kinds.push(runtime_launch::lease::RUNTIME_LAUNCH_LEASE_KIND);
        // Volume operations need the volume store, and the same recovered
        // slot: an unrecovered one may still have a workload writing.
        if persistent_volumes {
            kinds.push(runtime_launch::volume_maintenance::STATE_VOLUME_MAINTENANCE_LEASE_KIND);
        }
    }
    if config.browser_chrome.as_deref().is_some_and(Path::is_file)
        && config
            .run_control_verification_key
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    {
        kinds.push(ACTIVITY_BROWSER_EXECUTOR_LEASE_KIND);
    }
    kinds
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

/// `x-ato-run-assertion` — the control plane's per-request proof that a
/// request arriving at this slot's public ingress already passed the app
/// proxy's authentication and authorization. The slot port is reachable from
/// the internet through the Runner's ingress, so without this proof a direct
/// hit on the slot hostname bypasses the instance host's auth boundary
/// entirely. The gate strips the header before forwarding: the workload never
/// sees the credential it was admitted by.
const RUN_ASSERTION_HEADER: &str = "x-ato-run-assertion";

/// The session-surface WebSocket connect path. It bypasses the HTTP gate on
/// VM guests only: the in-guest surface gateway verifies its own
/// (keyring-signed) assertion, so gating it here would demand a credential the
/// exchange flow never issues for that path.
const SURFACE_CONNECT_PATH: &str = "/__ato/surface";

/// The lease scope an HTTP proxy assertion is bound to. The verification key
/// is the per-runner key derived from the control plane's Run-control root —
/// the same key the Run-control capability verifier uses — delivered inside
/// the lease claim so it never sits in config or command_json.
struct HttpProxyGate {
    verification_key: String,
    run_id: String,
    lease_id: String,
    runner_id: String,
}

#[derive(Deserialize)]
struct HttpProxyAssertionClaims {
    v: u32,
    kind: String,
    run_id: String,
    lease_id: String,
    runner_id: String,
    exp: i64,
    jti: String,
}

impl HttpProxyGate {
    /// The gate for one lease, or None when the claim carried no key — the
    /// control plane does not sign assertions for this Runner, so there is
    /// nothing to enforce with.
    fn for_lease(lease: &ClaimedLease, runner_id: &str) -> Option<Arc<Self>> {
        let key = lease.proxy_assertion_key.as_ref()?.trim();
        if key.len() < 32 {
            return None;
        }
        Some(Arc::new(Self {
            verification_key: key.to_owned(),
            run_id: lease.run_id.clone(),
            lease_id: lease.id.clone(),
            runner_id: runner_id.to_owned(),
        }))
    }

    /// Compact-HMAC envelope identical to Run-control credentials
    /// (`base64url(json).hex_mac`). Scope is exact: the assertion opens only
    /// this lease on this runner, and only until `exp`. No replay cache —
    /// a captured assertion replays against the same Run for at most its TTL.
    fn verify(&self, assertion: &str) -> bool {
        let Some((encoded, signature)) = assertion.split_once('.') else {
            return false;
        };
        if signature.contains('.') {
            return false;
        }
        let Ok(signature) = hex::decode(signature) else {
            return false;
        };
        let Ok(mut mac) = HmacSha256::new_from_slice(self.verification_key.as_bytes()) else {
            return false;
        };
        mac.update(encoded.as_bytes());
        if mac.verify_slice(&signature).is_err() {
            return false;
        }
        let Ok(payload) = URL_SAFE_NO_PAD.decode(encoded) else {
            return false;
        };
        let Ok(claims) = serde_json::from_slice::<HttpProxyAssertionClaims>(&payload) else {
            return false;
        };
        claims.v == 1
            && claims.kind == "http"
            && claims.run_id == self.run_id
            && claims.lease_id == self.lease_id
            && claims.runner_id == self.runner_id
            && claims.exp > OffsetDateTime::now_utc().unix_timestamp()
            && !claims.jti.is_empty()
    }
}

/// Split a bounded request prelude at the header terminator. The read that
/// found CRLFCRLF may already hold body or pipelined bytes; header logic
/// applies to the head only. A binary body is not valid UTF-8 and must not
/// fail header lookup, and no body byte may be rebuilt line-wise — the tail
/// is forwarded exactly as received.
fn split_http_prelude(prelude: &[u8]) -> Option<(&[u8], &[u8])> {
    prelude
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| (&prelude[..index], &prelude[index + 4..]))
}

/// First-value-wins header lookup over a request head (request line +
/// headers, terminator excluded).
fn http_prelude_header<'a>(head: &'a [u8], name: &str) -> Option<&'a str> {
    let text = std::str::from_utf8(head).ok()?;
    text.split("\r\n").skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

/// Remove every `name` header line from the request head and rebuild
/// head + terminator + tail. The workload never sees the assertion it was
/// admitted by, so a compromised app cannot replay it against anything else
/// that trusts it. The tail is appended untouched: it is data, not headers.
fn strip_http_prelude_header(head: &[u8], tail: &[u8], name: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(head.len() + tail.len() + 4);
    match std::str::from_utf8(head) {
        Ok(text) => {
            for (index, line) in text.split("\r\n").enumerate() {
                let is_header = index > 0
                    && line
                        .split_once(':')
                        .is_some_and(|(key, _)| key.trim().eq_ignore_ascii_case(name));
                if !is_header {
                    out.extend_from_slice(line.as_bytes());
                    out.extend_from_slice(b"\r\n");
                }
            }
        }
        // A non-UTF-8 head cannot be parsed line-wise; forward it unchanged.
        // With the gate on this is unreachable — the assertion lookup already
        // failed closed — so this only keeps the helper total.
        Err(_) => {
            out.extend_from_slice(head);
            out.extend_from_slice(b"\r\n");
        }
    }
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(tail);
    out
}

fn proxy_assertion_forbidden(client: &mut TcpStream) {
    let _ = client.write_all(
        b"HTTP/1.1 403 Forbidden\r\nCache-Control: no-store\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    );
}

/// How a public slot listener dispatches one request beyond "forward
/// everything". Any field set forces the bounded prelude read.
struct SurfaceMux {
    /// Run-control WebSocket target; the handshake carries its own capability
    /// credential, verified inside the control server.
    control_target: Option<SocketAddr>,
    presentation_frame: Option<Arc<RwLock<Vec<u8>>>>,
    /// The proxy-origin gate. None only when the control plane does not sign
    /// assertions for this Runner.
    gate: Option<Arc<HttpProxyGate>>,
    /// True when surface_target is a guest hosting the session-surface
    /// gateway, which authenticates `SURFACE_CONNECT_PATH` itself. False for
    /// runtime-launch workloads — there the path is just an app path and stays
    /// behind the gate.
    guest_surface_gateway: bool,
}

impl TcpProxy {
    fn start(listen: SocketAddr, target: ProxyTarget) -> Result<Self> {
        Self::start_with_mux(listen, target, None)
    }

    /// The Browser control listener and the proxy-origin gate are
    /// intentionally not second public sockets. A bounded request prelude
    /// routes reserved paths and enforces the gate; every other request keeps
    /// the existing VM Surface proxy unchanged.
    fn start_with_mux(
        listen: SocketAddr,
        target: ProxyTarget,
        mux: Option<SurfaceMux>,
    ) -> Result<Self> {
        let listener = TcpListener::bind(listen)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let mux = mux.map(Arc::new);
        let worker = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((client, _)) => {
                        let target = target.clone();
                        let mux = mux.clone();
                        thread::spawn(move || {
                            if let Some(mux) = mux.as_deref() {
                                proxy_dispatched(client, &target, mux);
                            } else {
                                proxy_connection(client, &target);
                            }
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

impl Drop for TcpProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn proxy_connection(mut client: TcpStream, target: &ProxyTarget) {
    proxy_connection_with_prelude(&mut client, target, &[]);
}

fn proxy_dispatched(mut client: TcpStream, surface_target: &ProxyTarget, mux: &SurfaceMux) {
    let Ok(prelude) = read_http_request_prelude(&mut client) else {
        return;
    };
    // The prelude read may hold bytes past the header terminator — request
    // body or pipelined traffic. All header parsing runs on the head alone;
    // the tail is data and is forwarded byte-for-byte.
    let Some((head, tail)) = split_http_prelude(&prelude) else {
        return;
    };
    let path = control_request_path(head);
    // Reserved paths keep their own verifier. Run control carries a
    // capability credential inside the WS handshake; the session-surface
    // connect is authenticated by the in-guest gateway. The gate must not
    // double- or un-authenticate either.
    if path == Some(RUN_CONTROL_PATH) {
        if let Some(control) = mux.control_target {
            proxy_connection_with_prelude(&mut client, &ProxyTarget::Tcp(control), &prelude);
        }
        return;
    }
    if mux.guest_surface_gateway && path == Some(SURFACE_CONNECT_PATH) {
        proxy_connection_with_prelude(&mut client, surface_target, &prelude);
        return;
    }
    if let Some(gate) = mux.gate.as_deref() {
        let asserted = http_prelude_header(head, RUN_ASSERTION_HEADER)
            .is_some_and(|assertion| gate.verify(assertion));
        if !asserted {
            proxy_assertion_forbidden(&mut client);
            return;
        }
    }
    if path == Some(BROWSER_PRESENTATION_PATH) {
        serve_browser_presentation(&mut client, mux.presentation_frame.as_ref());
        return;
    }
    let forwarded = if mux.gate.is_some() {
        strip_http_prelude_header(head, tail, RUN_ASSERTION_HEADER)
    } else {
        prelude
    };
    proxy_connection_with_prelude(&mut client, surface_target, &forwarded);
}

fn serve_browser_presentation(
    client: &mut TcpStream,
    presentation_frame: Option<&Arc<RwLock<Vec<u8>>>>,
) {
    let Some(frame) = presentation_frame.and_then(|frame| frame.read().ok()) else {
        let _ = client.write_all(
            b"HTTP/1.1 503 Service Unavailable\r\nCache-Control: no-store\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        return;
    };
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nCache-Control: no-store, max-age=0\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n\r\n",
        frame.len()
    );
    if client.write_all(headers.as_bytes()).is_ok() {
        let _ = client.write_all(&frame);
    }
}

fn read_http_request_prelude(client: &mut TcpStream) -> io::Result<Vec<u8>> {
    client.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut bytes = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while bytes.len() < RUN_CONTROL_REQUEST_HEADER_MAX_BYTES {
        let read = client.read(&mut chunk)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "HTTP request ended before headers",
            ));
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            client.set_read_timeout(None)?;
            return Ok(bytes);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "HTTP request headers exceed bound",
    ))
}

fn control_request_path(head: &[u8]) -> Option<&str> {
    let headers = std::str::from_utf8(head).ok()?;
    let request = headers.lines().next()?;
    let mut fields = request.split_whitespace();
    let method = fields.next()?;
    let target = fields.next()?;
    if method != "GET" || !target.starts_with('/') {
        return None;
    }
    Some(target.split('?').next().unwrap_or(target))
}

fn proxy_connection_with_prelude(client: &mut TcpStream, target: &ProxyTarget, prelude: &[u8]) {
    match target {
        ProxyTarget::Tcp(target) => {
            let Ok(mut upstream) = TcpStream::connect(target) else {
                return;
            };
            if upstream.write_all(prelude).is_ok() {
                proxy_tcp_pair(client, upstream);
            }
        }
        ProxyTarget::Unix(path) => {
            #[cfg(unix)]
            {
                let Ok(mut upstream) = UnixStream::connect(path) else {
                    return;
                };
                if upstream.write_all(prelude).is_ok() {
                    proxy_tcp_unix_pair(client, upstream);
                }
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

#[cfg(unix)]
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
    use std::cell::Cell;
    use std::io::{Read, Write};

    use super::*;
    use tungstenite::client::IntoClientRequest;

    #[test]
    fn explicit_stop_wins_a_race_with_revoked_continuation_authority() {
        let refreshed = Cell::new(false);

        let stopped = poll_runtime_launch_control(
            || Ok(true),
            || {
                refreshed.set(true);
                Ok(false)
            },
        )
        .expect("an explicit stop does not require authority to continue");

        assert!(stopped);
        assert!(!refreshed.get());
    }

    #[test]
    fn renewal_refusal_is_clean_only_when_the_coordinator_confirms_stop() {
        let stopped = poll_runtime_launch_control(|| Ok(false), || Ok(true))
            .expect("renewal response confirms the owner stop");
        assert!(stopped);

        let error = poll_runtime_launch_control(
            || Ok(false),
            || anyhow::bail!("execution authorization revoked without stop intent"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("without stop intent"));
    }

    #[test]
    fn continuing_run_checks_stop_before_renewing_authority() {
        let step = Cell::new(0_u8);

        let stopped = poll_runtime_launch_control(
            || {
                assert_eq!(step.replace(1), 0);
                Ok(false)
            },
            || {
                assert_eq!(step.replace(2), 1);
                Ok(false)
            },
        )
        .expect("continuation authority is current");

        assert!(!stopped);
        assert_eq!(step.get(), 2);
    }

    #[test]
    fn activity_frame_capture_failure_keeps_the_run_alive_and_backs_off() {
        let attempts = AtomicU64::new(0);
        let publications = AtomicU64::new(0);
        let mut next_frame_at = Instant::now() - Duration::from_secs(1);
        let before = Instant::now();

        refresh_activity_presentation_frame(
            &mut next_frame_at,
            || -> Result<Vec<u8>> {
                attempts.fetch_add(1, Ordering::SeqCst);
                anyhow::bail!("transient presentation capture failure")
            },
            |_| {
                publications.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .expect("presentation failure is not Activity failure");

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(publications.load(Ordering::SeqCst), 0);
        assert!(next_frame_at >= before + ACTIVITY_FRAME_RETRY_INTERVAL);

        refresh_activity_presentation_frame(
            &mut next_frame_at,
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                Ok(Vec::new())
            },
            |_| {
                publications.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .expect("backoff check");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(publications.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn failed_state_commit_preserves_the_working_copy_and_blocks_restart() {
        let temporary = tempfile::tempdir().unwrap();
        let lease = temporary.path().join("leases/lease-failed");
        fs::create_dir_all(&lease).unwrap();
        fs::write(lease.join("state"), b"unsaved work").unwrap();
        let outcome = settle_lease_directory(&lease, Err(anyhow::anyhow!("state commit failed")));
        assert!(outcome.is_err());
        assert_eq!(fs::read(lease.join("state")).unwrap(), b"unsaved work");
        assert!(slot_state::SlotGuard::acquire(temporary.path()).is_err());
    }

    #[test]
    fn presentation_refresh_does_not_change_computation_head_or_records() {
        let records = TestRecords::default();
        let submitted = Arc::clone(&records.0);
        let ingress = BrowserOperationIngress::new(
            browser_authority(),
            PortId::parse("browser").unwrap(),
            TestBrowserActuator::default(),
            TestPersistence,
            records,
        );
        ingress
            .accept_with_operation_id(
                "operation-before-presentation".to_owned(),
                ato_adapter_browser::BrowserEvent::Click {
                    x_normalized: 0.25,
                    y_normalized: 0.75,
                    button: 0,
                },
            )
            .expect("authoritative operation");
        let before = ingress.freeze().unwrap();
        ingress.unfreeze();
        let mut published = Vec::new();

        for frame in [vec![0xff, 0xd8, 0xff, 0xd9], vec![1, 2, 3, 4]] {
            let mut next_frame_at = Instant::now() - Duration::from_secs(1);
            refresh_activity_presentation_frame(
                &mut next_frame_at,
                || Ok(frame),
                |frame| {
                    published.push(frame);
                    Ok(())
                },
            )
            .unwrap();
        }

        let after = ingress.freeze().unwrap();
        assert_eq!(published.len(), 2);
        assert_eq!(after, before, "presentation must not evolve the Run head");
        assert_eq!(
            submitted.lock().unwrap().len(),
            1,
            "presentation must not submit an operation Record"
        );
    }

    #[test]
    fn normal_lease_cleanup_removes_activity_operation_journal() {
        let temporary = tempfile::tempdir().expect("lease tempdir");
        let lease_root = temporary.path().join("leases/lease-cleanup");
        let journal = lease_root.join("activity-operation-receipts");
        fs::create_dir_all(&journal).expect("journal directory");
        fs::write(journal.join("evidence.json"), b"settled").expect("journal evidence");

        cleanup_lease_directory(&lease_root).expect("lease cleanup");
        assert!(!lease_root.exists());
    }

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

    #[derive(Default)]
    struct TestBrowserActuator(AtomicU64);

    impl BrowserOperationActuator for TestBrowserActuator {
        fn apply(
            &self,
            _correlation_id: &str,
            _realization_generation: Option<&str>,
            _operation: &LiveOperation,
        ) -> std::result::Result<u64, String> {
            Ok(self.0.fetch_add(1, Ordering::SeqCst) + 1)
        }
    }

    fn assert_tcp_unreachable(address: SocketAddr) {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if TcpStream::connect_timeout(&address, Duration::from_millis(20)).is_err() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "TCP endpoint remained reachable after its listener closed"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn browser_presentation_response_is_no_store_jpeg() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let frame = Arc::new(RwLock::new(vec![0xff, 0xd8, 0xff, 0xd9]));
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            serve_browser_presentation(&mut stream, Some(&frame));
        });
        let mut client = TcpStream::connect(address).unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        server.join().unwrap();
        let headers_end = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        let headers = std::str::from_utf8(&response[..headers_end]).unwrap();
        assert!(headers.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(headers.contains("Content-Type: image/jpeg\r\n"));
        assert!(headers.contains("Cache-Control: no-store, max-age=0\r\n"));
        assert_eq!(&response[headers_end..], &[0xff, 0xd8, 0xff, 0xd9]);
    }

    fn control_credential(
        verification_key: &str,
        run_id: &str,
        lease_id: &str,
        runner_id: &str,
        capability: &BrowserControlCapability,
    ) -> String {
        let claims = serde_json::json!({
            "v": 1,
            "session_id": "rcs_test",
            "run_id": run_id,
            "lease_id": lease_id,
            "runner_id": runner_id,
            "protocol": capability.protocol,
            "port": capability.port,
            "expires_at": OffsetDateTime::now_utc().unix_timestamp() + 60,
        });
        let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let mut mac = HmacSha256::new_from_slice(verification_key.as_bytes()).unwrap();
        mac.update(encoded.as_bytes());
        format!("{encoded}.{}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn control_websocket_is_idempotent_and_routes_only_through_ingress() {
        let capability = BrowserControlCapability {
            protocol: BROWSER_PROTOCOL_ID.to_owned(),
            port: "browser".to_owned(),
        };
        let secret = "s".repeat(32);
        let records = TestRecords::default();
        let submitted = Arc::clone(&records.0);
        let ingress = Arc::new(BrowserOperationIngress::new(
            browser_authority(),
            PortId::parse("browser").unwrap(),
            TestBrowserActuator::default(),
            TestPersistence,
            records,
        ));
        let control = RunControlServer::start(
            Arc::clone(&ingress),
            "run_1".to_owned(),
            "lease_1".to_owned(),
            "runner_1".to_owned(),
            capability.clone(),
            secret.clone(),
        )
        .unwrap();
        let credential = control_credential(&secret, "run_1", "lease_1", "runner_1", &capability);
        let mut request = format!("ws://{}/{}", control.address(), &RUN_CONTROL_PATH[1..])
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "sec-websocket-protocol",
            format!("ato-control.{credential}").parse().unwrap(),
        );
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        let event = ato_adapter_browser::BrowserEvent::Keyboard {
            kind: ato_adapter_browser::KeyboardKind::KeyDown,
            code: "ArrowRight".to_owned(),
            modifiers: ato_adapter_browser::Modifiers::default(),
        };
        let request = serde_json::json!({
            "operation_id": "op_1",
            "client_seq": 1,
            "protocol": BROWSER_PROTOCOL_ID,
            "operation": "keyboard",
            "port": "browser",
            "payload": String::from_utf8(ato_adapter_browser::encode_event(&event).unwrap()).unwrap(),
        });
        socket
            .send(Message::Text(request.to_string().into()))
            .unwrap();
        let first: serde_json::Value =
            serde_json::from_str(socket.read().unwrap().into_text().unwrap().as_str()).unwrap();
        assert_eq!(first["status"], "applied");
        assert_eq!(first["run_seq"], 1);
        socket
            .send(Message::Text(request.to_string().into()))
            .unwrap();
        let retry: serde_json::Value =
            serde_json::from_str(socket.read().unwrap().into_text().unwrap().as_str()).unwrap();
        assert_eq!(retry["status"], "applied");
        assert_eq!(retry["run_seq"], 1);
        assert_eq!(ingress.freeze().unwrap().run_seq, 1);
        assert_eq!(submitted.lock().unwrap().len(), 1);
        drop(socket);
        drop(control);
    }

    #[test]
    fn control_credential_is_rejected_by_a_different_runner() {
        let capability = BrowserControlCapability {
            protocol: BROWSER_PROTOCOL_ID.to_owned(),
            port: "browser".to_owned(),
        };
        let verification_key = "v".repeat(32);
        let credential = control_credential(
            &verification_key,
            "run_1",
            "lease_1",
            "runner_a",
            &capability,
        );
        assert!(
            verify_run_control_credential(
                &verification_key,
                &credential,
                "run_1",
                "lease_1",
                "runner_b",
                &capability,
            )
            .is_err()
        );
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
                        checkpoint_state_ref: None,
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
            const PAGE: &str = r#"<!doctype html>
<button id='button' style='width:100vw;height:100vh'>counter</button>
<output id='counter'>0</output><script>
let n=0;
const counter=document.querySelector('#counter');
const inc=()=>counter.textContent=String(++n);
document.addEventListener('keydown',e=>{if(e.code==='ArrowRight')inc()});
document.addEventListener('click',inc);
globalThis.__ATO_WEBMCP_FIXTURE_OBSERVATION__=()=>({counter:n});
globalThis.__ATO_WEBMCP_FIXTURE_TOOLS__=[{
  name:'slow_increment',
  inputSchema:{type:'object',properties:{delay_ms:{type:'integer'}},additionalProperties:false},
  execute:async({delay_ms=800}={}, {signal}={})=>{
    await new Promise((resolve,reject)=>{
      const timer=setTimeout(resolve,delay_ms);
      signal?.addEventListener('abort',()=>{
        clearTimeout(timer);reject(new DOMException('aborted','AbortError'));
      },{once:true});
    });
    inc();
  }
},{
  name:'sync_block',
  inputSchema:{type:'object',properties:{duration_ms:{type:'integer'}},additionalProperties:false},
  execute:({duration_ms=11000}={})=>{
    const until=Date.now()+duration_ms;
    while(Date.now()<until){}
    inc();
  }
}];
</script>"#;
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
                        channel_scope: None,
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
        host.open_auxiliary_target(&format!("{origin}/"))
            .expect("credential-free Activity controller target");
        assert!(
            !host
                .capture_jpeg()
                .expect("application frame after auxiliary target")
                .is_empty()
        );
        thread::sleep(Duration::from_millis(250));
        assert!(
            !host
                .capture_jpeg()
                .expect("continued application frame after auxiliary target")
                .is_empty()
        );
        let records = TestRecords::default();
        let submitted = Arc::clone(&records.0);
        let dispatcher = adapter
            .lock()
            .unwrap()
            .live_operation_dispatcher()
            .expect("Browser test adapter dispatcher");
        let ingress = Arc::new(BrowserOperationIngress::new(
            browser_authority(),
            PortId::parse("browser").unwrap(),
            AttachedBrowserActuator(dispatcher),
            TestPersistence,
            records,
        ));
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
        let webmcp = host.webmcp_snapshot().expect("WebMCP fixture snapshot");
        assert!(
            webmcp
                .tools
                .iter()
                .any(|tool| tool.name.as_str() == Some("slow_increment"))
        );
        let generation = webmcp.registry_generation;
        let document_token = webmcp.document_token.clone();
        let realization_generation = format!("{document_token}.{generation}");
        let slow_ingress = Arc::clone(&ingress);
        let slow_realization_generation = realization_generation.clone();
        let slow = thread::spawn(move || {
            slow_ingress.accept_with_operation_context(
                "aop-agent-slow".to_owned(),
                ato_adapter_browser::BrowserEvent::Operation {
                    operation_name: "slow_increment".to_owned(),
                    // Model the five-second handler plus scheduling margin;
                    // the former deadline made this operation indeterminate.
                    arguments: serde_json::json!({"delay_ms":5200}),
                    surface_generation: generation,
                },
                Some(slow_realization_generation),
            )
        });
        thread::sleep(Duration::from_millis(75));
        let human_started = Instant::now();
        let human = ingress
            .accept_with_operation_context(
                "aop-human-click".to_owned(),
                ato_adapter_browser::BrowserEvent::Click {
                    x_normalized: 0.5,
                    y_normalized: 0.5,
                    button: 0,
                },
                Some(realization_generation.clone()),
            )
            .expect("Human operation must not wait for slow Agent WebMCP");
        assert_eq!(human.run_seq, 3);
        assert!(human_started.elapsed() < Duration::from_millis(500));
        let slow = slow
            .join()
            .expect("slow operation thread")
            .expect("slow operation receipt");
        assert_eq!(slow.run_seq, 4);
        assert_eq!(
            host.evaluate("document.querySelector('#counter').textContent")
                .unwrap()
                .pointer("/result/value")
                .and_then(serde_json::Value::as_str),
            Some("4")
        );

        let abort_ingress = Arc::clone(&ingress);
        let abort_realization_generation = realization_generation.clone();
        let aborted = thread::spawn(move || {
            abort_ingress.accept_with_operation_context(
                "aop-targeted-abort".to_owned(),
                ato_adapter_browser::BrowserEvent::Operation {
                    operation_name: "slow_increment".to_owned(),
                    arguments: serde_json::json!({"delay_ms":3000}),
                    surface_generation: generation,
                },
                Some(abort_realization_generation),
            )
        });
        let complete_ingress = Arc::clone(&ingress);
        let complete_realization_generation = realization_generation.clone();
        let completed = thread::spawn(move || {
            complete_ingress.accept_with_operation_context(
                "aop-not-aborted".to_owned(),
                ato_adapter_browser::BrowserEvent::Operation {
                    operation_name: "slow_increment".to_owned(),
                    arguments: serde_json::json!({"delay_ms":200}),
                    surface_generation: generation,
                },
                Some(complete_realization_generation),
            )
        });
        thread::sleep(Duration::from_millis(75));
        assert!(
            host.abort_webmcp_operation("aop-targeted-abort")
                .expect("targeted Browser abort")
        );
        assert!(matches!(
            aborted.join().expect("aborted operation thread"),
            Err(EvolutionError::Apply(_))
        ));
        let completed = completed
            .join()
            .expect("completed operation thread")
            .expect("other WebMCP operation must complete");
        assert_eq!(completed.run_seq, 5);
        assert_eq!(submitted.lock().unwrap().len(), 5);
        assert_eq!(
            host.evaluate("document.querySelector('#counter').textContent")
                .unwrap()
                .pointer("/result/value")
                .and_then(serde_json::Value::as_str),
            Some("5")
        );

        host.evaluate(
            r#"globalThis.__ATO_WEBMCP_FIXTURE_TOOLS__=[{
              name:'slow_increment',
              inputSchema:{type:'object',properties:{delay_ms:{type:'integer'},label:{type:'string'}},additionalProperties:false},
              execute:async()=>{document.querySelector('#counter').textContent='999'}
            }]; true"#,
        )
        .unwrap();
        let replaced_registry = host
            .webmcp_snapshot()
            .expect("replacement registry snapshot");
        assert_eq!(replaced_registry.document_token, document_token);
        assert!(replaced_registry.registry_generation > generation);
        let stale_registry_webmcp = ingress
            .accept_with_operation_context(
                "aop-old-registry-webmcp".to_owned(),
                ato_adapter_browser::BrowserEvent::Operation {
                    operation_name: "slow_increment".to_owned(),
                    arguments: serde_json::json!({"delay_ms":1}),
                    surface_generation: generation,
                },
                Some(realization_generation.clone()),
            )
            .expect_err("old WebMCP descriptor must not resolve after registry replacement");
        assert!(
            stale_registry_webmcp
                .to_string()
                .contains("stale_operation")
        );
        let stale_registry_click = ingress
            .accept_with_operation_context(
                "aop-old-registry-click".to_owned(),
                ato_adapter_browser::BrowserEvent::Click {
                    x_normalized: 0.5,
                    y_normalized: 0.5,
                    button: 0,
                },
                Some(realization_generation.clone()),
            )
            .expect_err("old fixed descriptor must not cross registry replacement");
        assert!(stale_registry_click.to_string().contains("stale_operation"));
        assert_eq!(
            host.evaluate("document.querySelector('#counter').textContent")
                .unwrap()
                .pointer("/result/value")
                .and_then(serde_json::Value::as_str),
            Some("5")
        );

        // A replacement document deliberately exposes the same operation
        // name and starts its registry generation at the same value. Before
        // the worker's next surface poll, the old descriptor/context must not
        // resolve against that new document -- for WebMCP or fixed Browser
        // input.
        host.evaluate("location.reload(); true").unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let replacement = loop {
            if let Ok(snapshot) = host.webmcp_snapshot()
                && snapshot.document_token != document_token
            {
                break snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "replacement document did not load"
            );
            thread::sleep(Duration::from_millis(25));
        };
        assert_eq!(replacement.registry_generation, generation);
        let stale_webmcp = ingress
            .accept_with_operation_context(
                "aop-old-document-webmcp".to_owned(),
                ato_adapter_browser::BrowserEvent::Operation {
                    operation_name: "slow_increment".to_owned(),
                    arguments: serde_json::json!({"delay_ms":1}),
                    surface_generation: generation,
                },
                Some(realization_generation.clone()),
            )
            .expect_err("old WebMCP descriptor must not resolve in replacement document");
        assert!(stale_webmcp.to_string().contains("stale_operation"));
        let stale_click = ingress
            .accept_with_operation_context(
                "aop-old-document-click".to_owned(),
                ato_adapter_browser::BrowserEvent::Click {
                    x_normalized: 0.5,
                    y_normalized: 0.5,
                    button: 0,
                },
                Some(realization_generation),
            )
            .expect_err("old fixed Browser descriptor must not target replacement document");
        assert!(stale_click.to_string().contains("stale_operation"));
        assert_eq!(
            host.evaluate("document.querySelector('#counter').textContent")
                .unwrap()
                .pointer("/result/value")
                .and_then(serde_json::Value::as_str),
            Some("0")
        );
        let replacement_generation = format!(
            "{}.{}",
            replacement.document_token, replacement.registry_generation
        );
        let hung_started = Instant::now();
        let hung = ingress
            .accept_with_operation_context(
                "aop-sync-hung-page".to_owned(),
                ato_adapter_browser::BrowserEvent::Operation {
                    operation_name: "sync_block".to_owned(),
                    arguments: serde_json::json!({"duration_ms":11000}),
                    surface_generation: replacement.registry_generation,
                },
                Some(replacement_generation),
            )
            .expect_err("synchronous hostile page handler must be fenced");
        assert!(hung.to_string().contains("physical_outcome_indeterminate"));
        let hung_elapsed = hung_started.elapsed();
        assert!(
            hung_elapsed >= Duration::from_secs(9) && hung_elapsed < Duration::from_secs(12),
            "native ACK deadline, not an unrelated early failure, must fence a blocked page"
        );
        // Allow the hostile task to unwind. Its late physical effect remains
        // deliberately uncommitted while this adapter incarnation stays
        // terminally fenced.
        thread::sleep(Duration::from_millis(1200));
        ingress.freeze().unwrap();
        let context = AdapterContext {
            workspace: workspace.path(),
            objects: objects.as_ref(),
        };
        let quiesce_started = Instant::now();
        assert!(adapter.lock().unwrap().quiesce(&context).is_err());
        assert!(quiesce_started.elapsed() < Duration::from_millis(500));
        assert!(adapter.lock().unwrap().detach(&context).is_err());
        host.stop().unwrap();
        stop_server.store(true, Ordering::Release);
        server.join().unwrap();
    }

    fn lease(expires_at: Option<String>) -> ClaimedLease {
        ClaimedLease {
            id: "lease_1".to_owned(),
            run_id: "run_1".to_owned(),
            proxy_assertion_key: None,
            command: LeaseCommand::Portable(PortableLeaseCommand {
                bundle_id: "bnd_1".to_owned(),
                transport_digest: format!("sha256:{}", "11".repeat(32)),
                expected_root_computation_ref: format!("blake3:{}", "11".repeat(32)),
                run_id: "run_1".to_owned(),
                session_id: "run:run_1".to_owned(),
                exported_port_id: "web".to_owned(),
                surface_contract_version: "1".to_owned(),
                session_surface: serde_json::json!({"kind":"web"}),
                accepted_session_surfaces: vec![serde_json::json!({"kind":"web"})],
                trace_id: None,
            }),
            expires_at,
        }
    }

    #[test]
    fn expired_lease_is_rejected_before_graph_access() {
        let expired = "2020-01-01T00:00:00Z".to_owned();
        assert!(validate_lease(&lease(Some(expired)), SystemTime::now()).is_err());
    }

    #[test]
    fn activity_surface_identity_is_stable_per_lease_and_changes_on_restart() {
        let first = activity_surface_id("run_shared", "lease_one");
        assert_eq!(first, activity_surface_id("run_shared", "lease_one"));
        assert_ne!(first, activity_surface_id("run_shared", "lease_two"));
        assert!(first.starts_with("surface_"));
        assert!(first.len() <= 128);
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
            public_base_url: Some("https://runner.example".to_owned()),
            work_root: PathBuf::from(".tmp/worker-test"),
            surface_listen: "0.0.0.0:8420".parse().unwrap(),
            hidden_surface_listen: "127.0.0.1:18420".parse().unwrap(),
            surface_target: "127.0.0.1:8080".parse().unwrap(),
            tap_host_cidr: Some("172.16.0.1/24".to_owned()),
            slot_id: "0".to_owned(),
            max_slots: 1,
            browser_chrome: None,
            run_control_verification_key: None,
            state_volume_root: None,
            network_controls: false,
            fixed_tcp_allowlist: String::new(),
            once: true,
        };
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn advertised_host_capacity_is_bounded() {
        let mut config = WorkerConfig {
            api_base: "https://staging.api.ato.run".to_owned(),
            runner_id: "runner".to_owned(),
            runner_token: "token".to_owned(),
            runner_credentials_file: None,
            public_base_url: Some("https://runner.example".to_owned()),
            work_root: PathBuf::from(".tmp/worker-test"),
            surface_listen: "127.0.0.1:8420".parse().unwrap(),
            hidden_surface_listen: "127.0.0.1:18420".parse().unwrap(),
            surface_target: "172.30.0.2:38865".parse().unwrap(),
            tap_host_cidr: Some("172.30.0.1/24".to_owned()),
            slot_id: "0".to_owned(),
            max_slots: 0,
            browser_chrome: None,
            run_control_verification_key: None,
            state_volume_root: None,
            network_controls: false,
            fixed_tcp_allowlist: String::new(),
            once: true,
        };
        assert!(validate_config(&config).is_err());
        config.max_slots = 64;
        assert!(validate_config(&config).is_ok());
        config.max_slots = 65;
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
            public_base_url: Some("https://runner.example".to_owned()),
            work_root: directory.path().join("work"),
            surface_listen: "127.0.0.1:8420".parse().unwrap(),
            hidden_surface_listen: "127.0.0.1:18420".parse().unwrap(),
            surface_target: "172.30.0.2:38865".parse().unwrap(),
            tap_host_cidr: Some("172.30.0.1/24".to_owned()),
            slot_id: "0".to_owned(),
            max_slots: 1,
            browser_chrome: None,
            run_control_verification_key: None,
            state_volume_root: None,
            network_controls: false,
            fixed_tcp_allowlist: String::new(),
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
        assert_tcp_unreachable(published);

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
        assert_tcp_unreachable(published);
    }

    #[test]
    fn ready_receipt_reports_the_published_proxy_port() {
        let config = WorkerConfig {
            api_base: "https://staging.api.ato.run".to_owned(),
            runner_id: "runner_1".to_owned(),
            runner_token: "token".to_owned(),
            runner_credentials_file: None,
            public_base_url: Some("https://runner.example".to_owned()),
            work_root: PathBuf::from(".tmp/worker"),
            surface_listen: "127.0.0.1:8420".parse().unwrap(),
            hidden_surface_listen: "127.0.0.1:18420".parse().unwrap(),
            surface_target: "172.30.0.2:38865".parse().unwrap(),
            tap_host_cidr: Some("172.30.0.1/24".to_owned()),
            slot_id: "0".to_owned(),
            max_slots: 1,
            browser_chrome: None,
            run_control_verification_key: None,
            state_volume_root: None,
            network_controls: false,
            fixed_tcp_allowlist: String::new(),
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
        let process_only = runner_capabilities(true, false, true, true, true);
        assert!(process_only.contains(&"execution_abi=process"));
        assert!(!process_only.contains(&"execution_abi=oci"));
        assert!(process_only.contains(&"isolation=untrusted-v1"));
        assert!(process_only.contains(&"materializer=ato.materialize.vm.snapshot@1"));
        assert!(process_only.contains(&"backend=firecracker"));
        assert!(
            runner_capabilities(false, true, false, false, false).contains(&"execution_abi=oci")
        );
        assert!(
            runner_capabilities(false, true, false, false, false)
                .contains(&"runtime_feature=oci_service_group_v1")
        );
        assert!(!process_only.contains(&"runtime_feature=oci_service_group_v1"));
        // Volumes are advertised only with OCI AND a usable store.
        let volume = "runtime_feature=runner_persistent_volume_v1";
        assert!(runner_capabilities(false, true, true, false, false).contains(&volume));
        assert!(!runner_capabilities(false, true, false, false, false).contains(&volume));
        assert!(!process_only.contains(&volume));
        let network = runner_capabilities(false, true, false, true, true);
        assert!(network.contains(&"network=ato.tcp-egress@1"));
        assert!(network.contains(&"network=ato.fixed-tcp@1"));
    }

    fn capability_test_config(tap_host_cidr: Option<&str>) -> WorkerConfig {
        WorkerConfig {
            api_base: "https://staging.api.ato.run".to_owned(),
            runner_id: "runner".to_owned(),
            runner_token: "token".to_owned(),
            runner_credentials_file: None,
            public_base_url: Some("https://runner.example".to_owned()),
            work_root: PathBuf::from(".tmp/worker-test"),
            surface_listen: "127.0.0.1:8420".parse().unwrap(),
            hidden_surface_listen: "127.0.0.1:18420".parse().unwrap(),
            surface_target: "127.0.0.1:8080".parse().unwrap(),
            tap_host_cidr: tap_host_cidr.map(str::to_owned),
            slot_id: "0".to_owned(),
            max_slots: 1,
            browser_chrome: None,
            run_control_verification_key: None,
            state_volume_root: None,
            network_controls: false,
            fixed_tcp_allowlist: String::new(),
            once: true,
        }
    }

    #[test]
    fn a_firecracker_host_advertises_the_vm_snapshot_capability() {
        let capabilities = runner_capabilities(
            capability_test_config(Some("172.16.0.1/24")).firecracker_configured(),
            false,
            false,
            false,
            false,
        );
        assert!(capabilities.contains(&"execution_abi=process"));
        assert!(capabilities.contains(&"isolation=untrusted-v1"));
        assert!(capabilities.contains(&"materializer=ato.materialize.vm.snapshot@1"));
        assert!(capabilities.contains(&"backend=firecracker"));
    }

    #[test]
    fn a_host_without_firecracker_advertises_only_what_it_can_do() {
        // Advertising VM-snapshot unconditionally is a lie the control plane
        // ACTS on: it would route a VM Capsule here and the lease would fail at
        // materialization instead of never being offered.
        let capabilities = runner_capabilities(
            capability_test_config(None).firecracker_configured(),
            false,
            false,
            false,
            false,
        );
        assert!(capabilities.contains(&"execution_abi=process"));
        assert!(capabilities.contains(&"isolation=untrusted-v1"));
        assert!(!capabilities.contains(&"materializer=ato.materialize.vm.snapshot@1"));
        assert!(!capabilities.contains(&"backend=firecracker"));
    }

    #[test]
    fn an_empty_tap_cidr_is_not_a_firecracker_host() {
        assert!(!capability_test_config(Some("   ")).firecracker_configured());
        assert!(capability_test_config(Some("172.16.0.1/24")).firecracker_configured());
    }

    #[test]
    fn startup_no_longer_requires_firecracker_configuration() {
        // The whole point: a host that can only serve the source/replay path
        // must be able to START. Requiring TAP here made every Runner a
        // Firecracker Runner.
        let config = capability_test_config(None);
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn a_present_but_malformed_tap_cidr_is_still_rejected() {
        // Optional does not mean unvalidated: a host that MEANT to configure
        // Firecracker and got it wrong must fail loudly rather than silently
        // drop the capability.
        assert!(validate_config(&capability_test_config(Some("not-a-cidr"))).is_err());
    }

    #[test]
    fn activity_executor_is_advertised_only_when_browser_runtime_is_available() {
        let chrome = tempfile::NamedTempFile::new().unwrap();
        let mut config = WorkerConfig {
            api_base: "https://staging.api.ato.run".to_owned(),
            runner_id: "runner_1".to_owned(),
            runner_token: "token".to_owned(),
            runner_credentials_file: None,
            public_base_url: Some("https://runner.example".to_owned()),
            work_root: PathBuf::from(".tmp/worker"),
            surface_listen: "127.0.0.1:8420".parse().unwrap(),
            hidden_surface_listen: "127.0.0.1:18420".parse().unwrap(),
            surface_target: "172.30.0.2:38865".parse().unwrap(),
            tap_host_cidr: Some("172.30.0.1/24".to_owned()),
            slot_id: "0".to_owned(),
            max_slots: 1,
            browser_chrome: None,
            run_control_verification_key: None,
            state_volume_root: None,
            network_controls: false,
            fixed_tcp_allowlist: String::new(),
            once: true,
        };
        runtime_launch::recovery::mark_slot_recovered(true);
        let mut expected = vec![PORTABLE_CAPSULE_LEASE_KIND];
        if runtime_launch::lease::RUNTIME_LAUNCH_SUPPORTED() {
            expected.push(runtime_launch::lease::RUNTIME_LAUNCH_LEASE_KIND);
        }
        assert_eq!(supported_lease_kinds(&config, false), expected);
        config.browser_chrome = Some(chrome.path().to_owned());
        config.run_control_verification_key = Some("v".repeat(32));
        expected.push(ACTIVITY_BROWSER_EXECUTOR_LEASE_KIND);
        assert_eq!(supported_lease_kinds(&config, false), expected);
    }

    fn http_assertion(
        key: &str,
        run_id: &str,
        lease_id: &str,
        runner_id: &str,
        exp: i64,
    ) -> String {
        let claims = serde_json::json!({
            "v": 1,
            "kind": "http",
            "run_id": run_id,
            "lease_id": lease_id,
            "runner_id": runner_id,
            "exp": exp,
            "jti": "jti_1",
        });
        let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let mut mac = HmacSha256::new_from_slice(key.as_bytes()).unwrap();
        mac.update(encoded.as_bytes());
        format!("{encoded}.{}", hex::encode(mac.finalize().into_bytes()))
    }

    fn test_gate() -> Arc<HttpProxyGate> {
        Arc::new(HttpProxyGate {
            verification_key: "k".repeat(32),
            run_id: "run_1".to_owned(),
            lease_id: "lease_1".to_owned(),
            runner_id: "runner_1".to_owned(),
        })
    }

    /// One-shot HTTP upstream: returns the received request prelude so a test
    /// can assert what the gate forwarded.
    fn prelude_echo_upstream() -> (SocketAddr, JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut prelude = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !prelude.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                prelude.extend_from_slice(&chunk[..read]);
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
            prelude
        });
        (address, worker)
    }

    fn gated_proxy(gate: Arc<HttpProxyGate>, target: SocketAddr) -> (SocketAddr, TcpProxy) {
        let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
        let public = reservation.local_addr().unwrap();
        drop(reservation);
        let proxy = TcpProxy::start_with_mux(
            public,
            ProxyTarget::Tcp(target),
            Some(SurfaceMux {
                control_target: None,
                presentation_frame: None,
                gate: Some(gate),
                guest_surface_gateway: false,
            }),
        )
        .unwrap();
        (public, proxy)
    }

    fn http_get(public: SocketAddr, path: &str, assertion: Option<&str>) -> Vec<u8> {
        let mut client = TcpStream::connect(public).unwrap();
        let mut request =
            format!("GET {path} HTTP/1.1\r\nHost: s0-runner.ato.run\r\nConnection: close\r\n")
                .into_bytes();
        if let Some(assertion) = assertion {
            request
                .extend_from_slice(format!("{RUN_ASSERTION_HEADER}: {assertion}\r\n").as_bytes());
        }
        request.extend_from_slice(b"\r\n");
        client.write_all(&request).unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        response
    }

    #[test]
    fn http_gate_admits_only_scoped_assertions() {
        let key = "k".repeat(32);
        let gate = test_gate();
        let (upstream_addr, upstream) = prelude_echo_upstream();
        let (public, proxy) = gated_proxy(Arc::clone(&gate), upstream_addr);

        // No assertion — the direct slot-host hit this gate exists to refuse.
        let response = http_get(public, "/i/", None);
        assert!(response.starts_with(b"HTTP/1.1 403"), "{response:?}");

        // Wrong scope: an assertion minted for a different Run opens nothing.
        let foreign = http_assertion(
            &key,
            "run_2",
            "lease_1",
            "runner_1",
            OffsetDateTime::now_utc().unix_timestamp() + 60,
        );
        let response = http_get(public, "/i/", Some(&foreign));
        assert!(response.starts_with(b"HTTP/1.1 403"), "{response:?}");

        // Expired.
        let expired = http_assertion(
            &key,
            "run_1",
            "lease_1",
            "runner_1",
            OffsetDateTime::now_utc().unix_timestamp() - 1,
        );
        let response = http_get(public, "/i/", Some(&expired));
        assert!(response.starts_with(b"HTTP/1.1 403"), "{response:?}");

        // Valid: forwarded, and the credential itself is stripped.
        let valid = http_assertion(
            &key,
            "run_1",
            "lease_1",
            "runner_1",
            OffsetDateTime::now_utc().unix_timestamp() + 60,
        );
        let response = http_get(public, "/i/", Some(&valid));
        assert!(response.starts_with(b"HTTP/1.1 200"), "{response:?}");
        let forwarded = upstream.join().unwrap();
        let forwarded = String::from_utf8(forwarded).unwrap();
        assert!(forwarded.starts_with("GET /i/ HTTP/1.1"), "{forwarded}");
        assert!(forwarded.contains("Host: s0-runner.ato.run"), "{forwarded}");
        assert!(!forwarded.contains(RUN_ASSERTION_HEADER), "{forwarded}");
        drop(proxy);
    }

    #[test]
    fn reserved_paths_keep_their_own_verifier() {
        let gate = test_gate();

        // A guest-hosted surface gateway authenticates /__ato/surface itself:
        // the HTTP gate must not demand the run assertion on that path.
        let (guest_addr, guest) = prelude_echo_upstream();
        let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
        let public = reservation.local_addr().unwrap();
        drop(reservation);
        let proxy = TcpProxy::start_with_mux(
            public,
            ProxyTarget::Tcp(guest_addr),
            Some(SurfaceMux {
                control_target: None,
                presentation_frame: None,
                gate: Some(gate),
                guest_surface_gateway: true,
            }),
        )
        .unwrap();
        let response = http_get(public, SURFACE_CONNECT_PATH, None);
        assert!(response.starts_with(b"HTTP/1.1 200"), "{response:?}");
        guest.join().unwrap();
        drop(proxy);

        // A runtime-launch workload owns no such gateway — the same path is
        // just an app path and stays behind the gate.
        let (app_addr, _app) = prelude_echo_upstream();
        let (public, proxy) = gated_proxy(test_gate(), app_addr);
        let response = http_get(public, SURFACE_CONNECT_PATH, None);
        assert!(response.starts_with(b"HTTP/1.1 403"), "{response:?}");
        drop(proxy);
    }

    /// One-shot upstream that captures EVERYTHING it receives until the
    /// client half-closes, then answers. Used to assert the body tail
    /// survives the gate byte-for-byte.
    fn request_capture_upstream() -> (SocketAddr, JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut received = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => received.extend_from_slice(&chunk[..read]),
                }
            }
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
            received
        });
        (address, worker)
    }

    #[test]
    fn http_gate_forwards_binary_body_byte_for_byte() {
        let key = "k".repeat(32);
        let gate = test_gate();
        let (upstream_addr, upstream) = request_capture_upstream();
        let (public, proxy) = gated_proxy(Arc::clone(&gate), upstream_addr);

        // A binary body: non-UTF-8 bytes, embedded CRLFs, and a forged
        // assertion line inside the body — none of it is header data, and a
        // single TCP read can deliver all of it with the headers.
        let mut body = vec![0xFF, 0xFE, 0x00, 0x80];
        body.extend_from_slice(b"\r\nx-ato-run-assertion: forged-in-body\r\n");
        body.extend_from_slice(&[0x00; 64]);
        body.extend_from_slice(&[0xAB; 64]);

        let assertion = http_assertion(
            &key,
            "run_1",
            "lease_1",
            "runner_1",
            OffsetDateTime::now_utc().unix_timestamp() + 60,
        );
        let mut request = format!(
            "POST /upload HTTP/1.1\r\nHost: s0-runner.ato.run\r\nContent-Length: {}\r\nConnection: close\r\n{RUN_ASSERTION_HEADER}: {assertion}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        request.extend_from_slice(&body);

        let mut client = TcpStream::connect(public).unwrap();
        // Headers AND the binary body in ONE write: the regression this pins
        // is the prelude read returning head+body and header logic then
        // failing (non-UTF-8 → 403) or rewriting (CRLF rebuild) the tail.
        client.write_all(&request).unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200"), "{response:?}");

        let received = upstream.join().unwrap();
        let mut expected = format!(
            "POST /upload HTTP/1.1\r\nHost: s0-runner.ato.run\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        expected.extend_from_slice(&body);
        assert_eq!(received, expected);
        drop(proxy);
    }
}
