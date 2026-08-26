//! Connected control-plane consumer for hosted physical Realizations.
//!
//! This is deliberately an application component, not a revival of the old
//! `ato runner serve` command. It consumes the current lease protocol, uses the
//! shared runtime graph validator, asks `RealizationPlanner` to select a path,
//! and reports ready only after Contract acceptance and Surface publication.

#![forbid(unsafe_code)]

mod activity_controller;

use std::collections::BTreeSet;
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
use ato_adapter_api::{
    ActuatorProviderRegistry, AdapterAttachContext, AdapterContext, AdapterInstance,
    AdapterRegistry, AttachedAdapter, IgnoreObservations, LiveOperation, Stylus,
    WorkspaceCapturePolicy,
};
use ato_adapter_browser::{
    BROWSER_PROTOCOL_ID, BrowserAdapter, BrowserAdapterConfig, BrowserInputMode,
    BrowserSurfaceTracker, RawWebMcpSnapshotV1,
    register_record_schemas as register_browser_record_schemas,
};
use ato_adapter_workspace::restore_workspace;
use ato_browser_host::{BrowserHost, BrowserHostConfig};
use ato_browser_semantics::{
    AcceptedBrowserOperation, BROWSER_COMPUTATION_SEMANTICS_ID, BrowserComputationSemantics,
    BrowserHeadPersistence, BrowserOperationActuator, BrowserOperationIngress,
    BrowserProtocolSemantics, BrowserRecordSubmission,
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
const RUNNER_CAPABILITIES: &[&str] = &[
    "execution_abi=process",
    "isolation=untrusted-v1",
    "materializer=ato.materialize.vm.snapshot@1",
    "backend=firecracker",
];
const ACTIVE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const GUEST_CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(250);
const GUEST_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
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

/// Object-safe control boundary used by the generic WebSocket server. The
/// concrete BrowserOperationIngress remains the sole authority owner; this
/// trait prevents the listener from depending on its Adapter/Persistence types.
trait BrowserControlIngress: Send + Sync {
    fn accept_control_operation(
        &self,
        operation_id: String,
        event: ato_adapter_browser::BrowserEvent,
    ) -> std::result::Result<ato_kernel::AcceptedOperation, ato_kernel::EvolutionError>;
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

    fn abort_active_webmcp_operation(&mut self) -> Result<bool> {
        self.host
            .as_mut()
            .context("Browser Host is unavailable")?
            .abort_active_webmcp_operation()
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
        let result = match &lease.command {
            LeaseCommand::Portable(command) => {
                self.execute_portable_lease(lease, command, &lease_root)
            }
            LeaseCommand::Activity(command) => {
                self.execute_activity_lease(lease, command, &lease_root)
            }
        };
        let cleanup = fs::remove_dir_all(&lease_root)
            .with_context(|| format!("failed to clean lease directory {}", lease_root.display()));
        match (result, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(error),
        }
    }

    fn execute_portable_lease(
        &self,
        lease: &ClaimedLease,
        command: &PortableLeaseCommand,
        lease_root: &Path,
    ) -> Result<()> {
        let source = self.api.graph_source(
            &lease.id,
            &command.bundle_id,
            &command.expected_root_computation_ref,
        )?;
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
            tap_host_cidr: &self.config.tap_host_cidr,
        };
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
        let proxy = TcpProxy::start_with_control(
            self.config.surface_listen,
            ProxyTarget::Tcp(self.config.hidden_surface_listen),
            browser.as_ref().map(|runtime| runtime.control_address()),
            presentation_frame.clone(),
        )?;
        let execution_id = format!("vm:{}:{}", lease.run_id, lease.id);
        ensure!(
            evolution.current_head().head.as_str() == command.expected_root_computation_ref,
            "hosted evolution authority root changed before the first operation"
        );
        self.api.report_ready(
            &lease.id,
            &execution_id,
            &self.config.public_base_url,
            ready_local_port(&self.config),
            browser.as_ref().map(|runtime| runtime.control_capability()),
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
        let session = self
            .api
            .activity_executor_session(&command.activity_id, &command.activity_run_id)?;
        validate_activity_executor_session(&session, lease, command)?;
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
            tap_host_cidr: &self.config.tap_host_cidr,
        };
        let running = restore_portable_path(&graph, lease_root, &lease.id, &physical)?;
        self.api.report_status(&lease.id, "running")?;
        let mut browser = start_hosted_browser_runtime(
            &graph,
            lease,
            lease_root,
            &lease_root.join("workspace"),
            evolution,
            self.api.clone(),
            self.config.browser_chrome.as_deref(),
            self.config.run_control_verification_key.as_deref(),
            &self.config.runner_id,
            &format!("http://{}/", self.config.hidden_surface_listen),
        )?
        .context("Activity source does not contain an explicit Browser Computation")?;
        let controller = ActivityControllerServer::start(
            ActivityControllerPageConfig {
                run_id: session.run_id.clone(),
                room_url: session.room_url,
                executor_credential: session.executor_credential,
                ice_servers: serde_json::to_value(session.rtc.ice_servers)?,
            },
            browser.activity_ingress(),
        )?;
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
        let mut last_heartbeat = Instant::now();
        let mut last_surface = Instant::now() - Duration::from_secs(1);
        let result = (|| -> Result<()> {
            loop {
                controller.publish_frame(browser.capture_presentation_frame()?)?;
                if last_surface.elapsed() >= Duration::from_millis(250) {
                    if let Ok(snapshot) = browser.webmcp_snapshot() {
                        let registry_generation = snapshot.registry_generation;
                        let observed_at = OffsetDateTime::now_utc().format(&Rfc3339)?;
                        let projection = surface.update(snapshot, observed_at)?.clone();
                        controller.publish_surface(projection, registry_generation)?;
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
                        break Err(anyhow::anyhow!("Activity controller failed"));
                    }
                    Ok(ActivityControllerEvent::AbortRequested { result }) => {
                        let signaled = browser.abort_active_webmcp_operation().unwrap_or(false);
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
        if let Err(error) = self.api.report_stopped(&lease.id, &execution_id) {
            shutdown_errors.push(format!("Activity stopped report failed: {error:#}"));
        }
        if shutdown_errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(shutdown_errors.join("; "))
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

fn activity_surface_id(run_id: &str, lease_id: &str) -> String {
    let scope = format!("ato.activity.surface.v0\0{run_id}\0{lease_id}");
    let digest = <Sha256 as sha2::Digest>::digest(scope.as_bytes());
    format!("surface_{}", URL_SAFE_NO_PAD.encode(digest))
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
    ensure!(
        !config.tap_host_cidr.trim().is_empty() && config.tap_host_cidr.contains('/'),
        "TAP host CIDR is invalid"
    );
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
    tap_host_cidr: &'a str,
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
            port: binding.port.clone(),
            stream: format!("browser-{}", lease.id),
            next_local_seq: Arc::new(AtomicU64::new(0)),
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
    command: LeaseCommand,
    expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
enum LeaseCommand {
    #[serde(rename = "portable_capsule_v2")]
    Portable(PortableLeaseCommand),
    #[serde(rename = "activity_browser_executor_v0")]
    Activity(ActivityLeaseCommand),
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
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivityLeaseCommand {
    activity_id: String,
    activity_run_id: String,
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
struct ReadyReport<'a> {
    execution_id: &'a str,
    ready_url: &'a str,
    local_port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    control: Option<BrowserControlReport<'a>>,
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
            "supported_lease_kinds": supported_lease_kinds(config),
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
        control: Option<BrowserControlCapability>,
    ) -> Result<()> {
        self.authorized(
            self.client
                .post(format!("{}/v1/runner-leases/{lease_id}/ready", self.base)),
        )
        .json(&ReadyReport {
            execution_id,
            ready_url,
            local_port,
            control: control.as_ref().map(|capability| BrowserControlReport {
                protocol: &capability.protocol,
                port: &capability.port,
            }),
        })
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

fn supported_lease_kinds(config: &WorkerConfig) -> Vec<&'static str> {
    let mut kinds = vec![PORTABLE_CAPSULE_LEASE_KIND];
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

impl TcpProxy {
    fn start(listen: SocketAddr, target: ProxyTarget) -> Result<Self> {
        Self::start_with_control(listen, target, None, None)
    }

    /// The Browser control listener is intentionally not a second public
    /// socket. A bounded request prelude routes its one exact WebSocket path;
    /// every other request keeps the existing VM Surface proxy unchanged.
    fn start_with_control(
        listen: SocketAddr,
        target: ProxyTarget,
        control_target: Option<SocketAddr>,
        presentation_frame: Option<Arc<RwLock<Vec<u8>>>>,
    ) -> Result<Self> {
        let listener = TcpListener::bind(listen)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((client, _)) => {
                        let target = target.clone();
                        let presentation_frame = presentation_frame.clone();
                        thread::spawn(move || {
                            if let Some(control_target) = control_target {
                                proxy_surface_or_browser(
                                    client,
                                    &target,
                                    control_target,
                                    presentation_frame,
                                );
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

fn proxy_surface_or_browser(
    mut client: TcpStream,
    surface_target: &ProxyTarget,
    control_target: SocketAddr,
    presentation_frame: Option<Arc<RwLock<Vec<u8>>>>,
) {
    let Ok(prelude) = read_http_request_prelude(&mut client) else {
        return;
    };
    let path = control_request_path(&prelude);
    if path == Some(BROWSER_PRESENTATION_PATH) {
        serve_browser_presentation(&mut client, presentation_frame.as_ref());
        return;
    }
    let target = if path == Some(RUN_CONTROL_PATH) {
        ProxyTarget::Tcp(control_target)
    } else {
        surface_target.clone()
    };
    proxy_connection_with_prelude(&mut client, &target, &prelude);
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

fn control_request_path(prelude: &[u8]) -> Option<&str> {
    let headers = std::str::from_utf8(prelude).ok()?;
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
    use tungstenite::client::IntoClientRequest;

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
    struct TestBrowserActuator;

    impl BrowserOperationActuator for TestBrowserActuator {
        fn apply(&mut self, _operation: &LiveOperation) -> std::result::Result<(), String> {
            Ok(())
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
            TestBrowserActuator,
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
            public_base_url: "https://runner.example".to_owned(),
            work_root: PathBuf::from(".tmp/worker-test"),
            surface_listen: "0.0.0.0:8420".parse().unwrap(),
            hidden_surface_listen: "127.0.0.1:18420".parse().unwrap(),
            surface_target: "127.0.0.1:8080".parse().unwrap(),
            tap_host_cidr: "172.16.0.1/24".to_owned(),
            slot_id: "0".to_owned(),
            max_slots: 1,
            browser_chrome: None,
            run_control_verification_key: None,
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
            public_base_url: "https://runner.example".to_owned(),
            work_root: PathBuf::from(".tmp/worker-test"),
            surface_listen: "127.0.0.1:8420".parse().unwrap(),
            hidden_surface_listen: "127.0.0.1:18420".parse().unwrap(),
            surface_target: "172.30.0.2:38865".parse().unwrap(),
            tap_host_cidr: "172.30.0.1/24".to_owned(),
            slot_id: "0".to_owned(),
            max_slots: 0,
            browser_chrome: None,
            run_control_verification_key: None,
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
            public_base_url: "https://runner.example".to_owned(),
            work_root: directory.path().join("work"),
            surface_listen: "127.0.0.1:8420".parse().unwrap(),
            hidden_surface_listen: "127.0.0.1:18420".parse().unwrap(),
            surface_target: "172.30.0.2:38865".parse().unwrap(),
            tap_host_cidr: "172.30.0.1/24".to_owned(),
            slot_id: "0".to_owned(),
            max_slots: 1,
            browser_chrome: None,
            run_control_verification_key: None,
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
            public_base_url: "https://runner.example".to_owned(),
            work_root: PathBuf::from(".tmp/worker"),
            surface_listen: "127.0.0.1:8420".parse().unwrap(),
            hidden_surface_listen: "127.0.0.1:18420".parse().unwrap(),
            surface_target: "172.30.0.2:38865".parse().unwrap(),
            tap_host_cidr: "172.30.0.1/24".to_owned(),
            slot_id: "0".to_owned(),
            max_slots: 1,
            browser_chrome: None,
            run_control_verification_key: None,
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

    #[test]
    fn activity_executor_is_advertised_only_when_browser_runtime_is_available() {
        let chrome = tempfile::NamedTempFile::new().unwrap();
        let mut config = WorkerConfig {
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
            max_slots: 1,
            browser_chrome: None,
            run_control_verification_key: None,
            once: true,
        };
        assert_eq!(
            supported_lease_kinds(&config),
            [PORTABLE_CAPSULE_LEASE_KIND]
        );
        config.browser_chrome = Some(chrome.path().to_owned());
        config.run_control_verification_key = Some("v".repeat(32));
        assert_eq!(
            supported_lease_kinds(&config),
            [
                PORTABLE_CAPSULE_LEASE_KIND,
                ACTIVITY_BROWSER_EXECUTOR_LEASE_KIND,
            ]
        );
    }
}
