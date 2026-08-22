use std::collections::{BTreeSet, VecDeque};
use std::io::ErrorKind;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ato_adapter_api::{
    AdapterError, AdapterObservation, ObservationEffect, ObservationSink, PresentationHint,
};
use ato_computation::{PortId, ProtocolId};
use ato_objects::Direction;
use serde::{Deserialize, Serialize};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::{Message, WebSocket, accept_hdr};

use crate::coalescer::ContinuousCoalescer;
use crate::protocol::{BrowserEvent, encode_event_with_policy, validate_event};
use crate::{BROWSER_ADAPTER_ID, BROWSER_PROTOCOL_ID};

const POLL_INTERVAL: Duration = Duration::from_millis(2);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserRuntimeBootstrap {
    pub protocol: String,
    pub control_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_socket: Option<String>,
    pub channel_credential: String,
    pub browser_session: String,
    pub expected_origin: String,
    pub allowed_non_text_codes: BTreeSet<String>,
}

pub(crate) enum TransportCommand {
    Apply {
        request_id: String,
        event: BrowserEvent,
        deadline: Instant,
        ack_timeout: Duration,
        result: mpsc::Sender<Result<(), AdapterError>>,
    },
    Quiesce {
        request_id: String,
        deadline: Instant,
        ack_timeout: Duration,
        result: mpsc::Sender<Result<(), AdapterError>>,
    },
    Pause {
        request_id: String,
        deadline: Instant,
        ack_timeout: Duration,
        result: mpsc::Sender<Result<(), AdapterError>>,
    },
    Resume {
        request_id: String,
        deadline: Instant,
        ack_timeout: Duration,
        result: mpsc::Sender<Result<(), AdapterError>>,
    },
    Activate {
        request_id: String,
        deadline: Instant,
        ack_timeout: Duration,
        result: mpsc::Sender<Result<(), AdapterError>>,
    },
    Shutdown,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AdapterMessage<'a> {
    HelloAck {
        protocol: &'a str,
        browser_session: &'a str,
        lifecycle: BrowserLifecycle,
    },
    Apply {
        request_id: &'a str,
        event: &'a BrowserEvent,
    },
    Quiesce {
        request_id: &'a str,
    },
    BlockInput,
    Activate {
        request_id: &'a str,
    },
    Stopped,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum BridgeMessage {
    Hello {
        protocol: String,
        channel_credential: String,
        browser_session: String,
        top_level_origin: String,
    },
    Event {
        event: BrowserEvent,
    },
    Ack {
        request_id: String,
    },
    Quiesced {
        request_id: String,
    },
    Activated {
        request_id: String,
    },
    Error {
        #[serde(default)]
        request_id: Option<String>,
        reason: String,
    },
}

struct PendingRequest {
    request_id: String,
    kind: PendingKind,
    deadline: Instant,
    result: mpsc::Sender<Result<(), AdapterError>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingKind {
    Apply,
    Quiesce,
    Activate,
    Pause,
    Resume,
}

impl PendingKind {
    fn name(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::Quiesce => "quiesce",
            Self::Activate => "activate",
            Self::Pause => "capture pause",
            Self::Resume => "capture resume",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum BrowserLifecycle {
    Restoring,
    Active,
    Quiescing,
    Stopped,
}

pub(crate) struct TransportHandle {
    pub discovery_path: PathBuf,
    pub commands: mpsc::Sender<TransportCommand>,
    pub failure: Arc<Mutex<Option<String>>>,
    pub join: Option<JoinHandle<()>>,
    #[cfg(unix)]
    control_relay: Option<ControlRelay>,
}

impl TransportHandle {
    pub(crate) fn shutdown(&mut self) -> Result<(), AdapterError> {
        let _ = self.commands.send(TransportCommand::Shutdown);
        let join_result = self.join.take().map(JoinHandle::join);
        remove_discovery(&self.discovery_path)?;
        #[cfg(unix)]
        self.control_relay.take();
        if join_result.is_some_and(|result| result.is_err()) {
            return Err(AdapterError::Operation(
                "Browser transport thread panicked".to_owned(),
            ));
        }
        if let Some(error) = self
            .failure
            .lock()
            .map_err(|_| {
                AdapterError::Operation("Browser transport failure state poisoned".to_owned())
            })?
            .as_ref()
        {
            return Err(AdapterError::Operation(error.clone()));
        }
        Ok(())
    }
}

#[cfg(unix)]
struct ControlRelay {
    file_name: String,
    path: PathBuf,
    stop: Arc<std::sync::atomic::AtomicBool>,
    join: Option<JoinHandle<()>>,
}

#[cfg(unix)]
impl ControlRelay {
    fn start(
        runtime_dir: &Path,
        instance_id: &str,
        target: SocketAddr,
    ) -> Result<Self, AdapterError> {
        let file_name = format!("browser-{instance_id}.sock");
        let path = runtime_dir.join(&file_name);
        if path.exists() {
            return Err(AdapterError::Operation(
                "Browser control relay socket already exists".to_owned(),
            ));
        }
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let join = thread::spawn(move || {
            while !thread_stop.load(std::sync::atomic::Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        thread::spawn(move || relay_unix_to_tcp(stream, target));
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(POLL_INTERVAL);
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            file_name,
            path,
            stop,
            join: Some(join),
        })
    }
}

#[cfg(unix)]
impl Drop for ControlRelay {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
fn relay_unix_to_tcp(mut client: UnixStream, target: SocketAddr) {
    let Ok(mut upstream) = TcpStream::connect(target) else {
        return;
    };
    let Ok(mut client_read) = client.try_clone() else {
        return;
    };
    let Ok(mut upstream_write) = upstream.try_clone() else {
        return;
    };
    let forward = thread::spawn(move || {
        let _ = std::io::copy(&mut client_read, &mut upstream_write);
        let _ = upstream_write.shutdown(Shutdown::Write);
    });
    let _ = std::io::copy(&mut upstream, &mut client);
    let _ = client.shutdown(Shutdown::Write);
    let _ = forward.join();
}

impl Drop for TransportHandle {
    fn drop(&mut self) {
        let _ = self.commands.send(TransportCommand::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = remove_discovery(&self.discovery_path);
    }
}

pub(crate) struct TransportConfig {
    pub expected_origin: String,
    pub port_id: PortId,
    pub allowed_non_text_codes: BTreeSet<String>,
    pub channel_credential: String,
    pub browser_session: String,
}

pub(crate) fn start_transport(
    runtime_dir: &Path,
    instance_id: &str,
    config: TransportConfig,
    observations: Arc<dyn ObservationSink>,
) -> Result<TransportHandle, AdapterError> {
    std::fs::create_dir_all(runtime_dir)?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    #[cfg(unix)]
    let control_relay = if std::env::var("ATO_BROWSER_CONTROL_RELAY").as_deref() == Ok("unix") {
        Some(ControlRelay::start(runtime_dir, instance_id, address)?)
    } else {
        None
    };
    let bootstrap = BrowserRuntimeBootstrap {
        protocol: BROWSER_PROTOCOL_ID.to_owned(),
        control_url: format!("ws://{address}"),
        #[cfg(unix)]
        control_socket: control_relay.as_ref().map(|relay| relay.file_name.clone()),
        #[cfg(not(unix))]
        control_socket: None,
        channel_credential: config.channel_credential.clone(),
        browser_session: config.browser_session.clone(),
        expected_origin: config.expected_origin.clone(),
        allowed_non_text_codes: config.allowed_non_text_codes.clone(),
    };
    let discovery_path = discovery_path(runtime_dir, instance_id)?;
    write_runtime_discovery(&discovery_path, &bootstrap)?;
    let (commands, receiver) = mpsc::channel();
    let failure = Arc::new(Mutex::new(None));
    let thread_failure = Arc::clone(&failure);
    let join = thread::spawn(move || {
        if let Err(error) = run_transport(listener, config, observations, receiver) {
            eprintln!("Browser Adapter transport failed: {error}");
            if let Ok(mut slot) = thread_failure.lock() {
                *slot = Some(error.to_string());
            }
        }
    });
    Ok(TransportHandle {
        discovery_path,
        commands,
        failure,
        join: Some(join),
        #[cfg(unix)]
        control_relay,
    })
}

fn run_transport(
    listener: TcpListener,
    config: TransportConfig,
    observations: Arc<dyn ObservationSink>,
    commands: mpsc::Receiver<TransportCommand>,
) -> Result<(), AdapterError> {
    let mut listener = Some(listener);
    let mut socket = None;
    let mut authenticated = false;
    let mut lifecycle = BrowserLifecycle::Restoring;
    let mut coalescer = ContinuousCoalescer::default();
    let mut queued_commands = VecDeque::new();
    let mut pending: Option<PendingRequest> = None;

    loop {
        let mut shutdown = false;
        while let Ok(command) = commands.try_recv() {
            match command {
                TransportCommand::Shutdown => {
                    shutdown = true;
                    break;
                }
                command @ TransportCommand::Quiesce { .. } => {
                    lifecycle = BrowserLifecycle::Quiescing;
                    listener.take();
                    if let Some(bridge) = socket.as_mut().filter(|_| authenticated) {
                        send_message(bridge, &AdapterMessage::BlockInput)?;
                    }
                    queued_commands.push_front(command);
                }
                command @ TransportCommand::Pause { .. } => {
                    lifecycle = BrowserLifecycle::Quiescing;
                    if let Some(bridge) = socket.as_mut().filter(|_| authenticated) {
                        send_message(bridge, &AdapterMessage::BlockInput)?;
                    }
                    queued_commands.push_front(command);
                }
                command => queued_commands.push_back(command),
            }
        }
        if shutdown {
            lifecycle = BrowserLifecycle::Stopped;
            if let Some(bridge) = socket.as_mut().filter(|_| authenticated) {
                let _ = send_message(bridge, &AdapterMessage::Stopped);
                let _ = bridge.close(None);
            }
            fail_pending(&mut pending, "Browser Adapter detached");
            fail_queued(&mut queued_commands, "Browser Adapter detached");
            break;
        }

        if pending
            .as_ref()
            .is_some_and(|request| Instant::now() >= request.deadline)
        {
            let operation = pending
                .as_ref()
                .map_or("request", |request| request.kind.name());
            fail_pending(
                &mut pending,
                &format!("Browser {operation} acknowledgement timed out"),
            );
            socket.take();
            authenticated = false;
        }
        expire_queued(&mut queued_commands);

        if socket.is_none()
            && !matches!(
                lifecycle,
                BrowserLifecycle::Quiescing | BrowserLifecycle::Stopped
            )
            && let Some(listener) = listener.as_ref()
        {
            match listener.accept() {
                Ok((stream, _)) => match accept_bridge(stream, &config.expected_origin) {
                    Ok(bridge) => socket = Some(bridge),
                    Err(error) => {
                        eprintln!("Browser Adapter rejected Bridge connection: {error}");
                        continue;
                    }
                },
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
        }

        if pending.is_none()
            && let Some(command) = queued_commands.pop_front()
        {
            match command {
                TransportCommand::Apply {
                    request_id,
                    event,
                    deadline,
                    ack_timeout,
                    result,
                } => {
                    if let Some(bridge) = socket.as_mut().filter(|_| authenticated) {
                        send_message(
                            bridge,
                            &AdapterMessage::Apply {
                                request_id: &request_id,
                                event: &event,
                            },
                        )?;
                        pending = Some(PendingRequest {
                            request_id,
                            kind: PendingKind::Apply,
                            deadline: Instant::now() + ack_timeout,
                            result,
                        });
                    } else {
                        // Keep processing later lifecycle commands while a
                        // replay waits for its Bridge to connect.
                        queued_commands.push_back(TransportCommand::Apply {
                            request_id,
                            event,
                            deadline,
                            ack_timeout,
                            result,
                        });
                    }
                }
                TransportCommand::Quiesce {
                    request_id,
                    deadline: _,
                    ack_timeout,
                    result,
                } => {
                    if let Some(bridge) = socket.as_mut().filter(|_| authenticated) {
                        send_message(
                            bridge,
                            &AdapterMessage::Quiesce {
                                request_id: &request_id,
                            },
                        )?;
                        pending = Some(PendingRequest {
                            request_id,
                            kind: PendingKind::Quiesce,
                            deadline: Instant::now() + ack_timeout,
                            result,
                        });
                    } else {
                        flush_events(&mut coalescer, &config, Arc::clone(&observations))?;
                        let _ = result.send(Ok(()));
                    }
                }
                TransportCommand::Pause {
                    request_id,
                    deadline: _,
                    ack_timeout,
                    result,
                } => {
                    if let Some(bridge) = socket.as_mut().filter(|_| authenticated) {
                        send_message(
                            bridge,
                            &AdapterMessage::Quiesce {
                                request_id: &request_id,
                            },
                        )?;
                        pending = Some(PendingRequest {
                            request_id,
                            kind: PendingKind::Pause,
                            deadline: Instant::now() + ack_timeout,
                            result,
                        });
                    } else {
                        flush_events(&mut coalescer, &config, Arc::clone(&observations))?;
                        let _ = result.send(Ok(()));
                    }
                }
                TransportCommand::Activate {
                    request_id,
                    deadline: _,
                    ack_timeout,
                    result,
                } => {
                    if lifecycle == BrowserLifecycle::Quiescing {
                        let _ = result.send(Err(AdapterError::Operation(
                            "Browser Adapter is quiescing".to_owned(),
                        )));
                    } else if lifecycle == BrowserLifecycle::Active {
                        let _ = result.send(Ok(()));
                    } else if let Some(bridge) = socket.as_mut().filter(|_| authenticated) {
                        send_message(
                            bridge,
                            &AdapterMessage::Activate {
                                request_id: &request_id,
                            },
                        )?;
                        pending = Some(PendingRequest {
                            request_id,
                            kind: PendingKind::Activate,
                            deadline: Instant::now() + ack_timeout,
                            result,
                        });
                    } else {
                        lifecycle = BrowserLifecycle::Active;
                        let _ = result.send(Ok(()));
                    }
                }
                TransportCommand::Resume {
                    request_id,
                    deadline: _,
                    ack_timeout,
                    result,
                } => {
                    if lifecycle != BrowserLifecycle::Quiescing {
                        let _ = result.send(Ok(()));
                    } else if let Some(bridge) = socket.as_mut().filter(|_| authenticated) {
                        send_message(
                            bridge,
                            &AdapterMessage::Activate {
                                request_id: &request_id,
                            },
                        )?;
                        pending = Some(PendingRequest {
                            request_id,
                            kind: PendingKind::Resume,
                            deadline: Instant::now() + ack_timeout,
                            result,
                        });
                    } else {
                        lifecycle = BrowserLifecycle::Active;
                        let _ = result.send(Ok(()));
                    }
                }
                TransportCommand::Shutdown => unreachable!("shutdown is handled with priority"),
            }
        }

        if let Some(bridge) = socket.as_mut() {
            match bridge.read() {
                Ok(Message::Text(text)) => {
                    let message: BridgeMessage = serde_json::from_str(&text).map_err(|error| {
                        AdapterError::Operation(format!("invalid Browser Bridge message: {error}"))
                    })?;
                    match message {
                        BridgeMessage::Hello {
                            protocol,
                            channel_credential,
                            browser_session,
                            top_level_origin,
                        } if !authenticated => {
                            validate_hello(
                                &config,
                                &protocol,
                                &channel_credential,
                                &browser_session,
                                &top_level_origin,
                            )?;
                            authenticated = true;
                            send_message(
                                bridge,
                                &AdapterMessage::HelloAck {
                                    protocol: BROWSER_PROTOCOL_ID,
                                    browser_session: &config.browser_session,
                                    lifecycle,
                                },
                            )?;
                        }
                        BridgeMessage::Event { event }
                            if authenticated
                                && matches!(
                                    lifecycle,
                                    BrowserLifecycle::Active | BrowserLifecycle::Quiescing
                                ) =>
                        {
                            if validate_event(&event, &config.allowed_non_text_codes).is_ok() {
                                for ready in coalescer.ingest(event) {
                                    emit_event(&config, Arc::clone(&observations), &ready)?;
                                }
                            }
                        }
                        BridgeMessage::Ack { request_id } if authenticated => {
                            complete_pending(
                                &mut pending,
                                &request_id,
                                PendingKind::Apply,
                                Ok(()),
                            )?;
                        }
                        BridgeMessage::Quiesced { request_id } if authenticated => {
                            flush_events(&mut coalescer, &config, Arc::clone(&observations))?;
                            let expected_kind =
                                pending.as_ref().map_or(PendingKind::Quiesce, |request| {
                                    match request.kind {
                                        PendingKind::Pause => PendingKind::Pause,
                                        _ => PendingKind::Quiesce,
                                    }
                                });
                            complete_pending(&mut pending, &request_id, expected_kind, Ok(()))?;
                        }
                        BridgeMessage::Activated { request_id } if authenticated => {
                            let expected_kind =
                                pending.as_ref().map_or(PendingKind::Activate, |request| {
                                    match request.kind {
                                        PendingKind::Resume => PendingKind::Resume,
                                        _ => PendingKind::Activate,
                                    }
                                });
                            complete_pending(&mut pending, &request_id, expected_kind, Ok(()))?;
                            lifecycle = BrowserLifecycle::Active;
                        }
                        BridgeMessage::Error { request_id, reason } if authenticated => {
                            if let Some(request_id) = request_id {
                                let pending_kind = pending
                                    .as_ref()
                                    .map_or(PendingKind::Apply, |value| value.kind);
                                complete_pending(
                                    &mut pending,
                                    &request_id,
                                    pending_kind,
                                    Err(AdapterError::Operation(format!(
                                        "Browser Bridge rejected request: {reason}"
                                    ))),
                                )?;
                            } else {
                                return Err(AdapterError::Operation(format!(
                                    "Browser Bridge failed: {reason}"
                                )));
                            }
                        }
                        _ => {
                            return Err(AdapterError::Operation(
                                "Browser Bridge message violates handshake or lifecycle".to_owned(),
                            ));
                        }
                    }
                }
                Ok(Message::Close(_)) => {
                    socket = None;
                    authenticated = false;
                    fail_pending(&mut pending, "Browser Bridge disconnected");
                }
                Ok(Message::Ping(payload)) => bridge
                    .send(Message::Pong(payload))
                    .map_err(|error| AdapterError::Operation(error.to_string()))?,
                Ok(_) => {}
                Err(tungstenite::Error::Io(error)) if error.kind() == ErrorKind::WouldBlock => {}
                Err(tungstenite::Error::ConnectionClosed) => {
                    socket = None;
                    authenticated = false;
                    fail_pending(&mut pending, "Browser Bridge disconnected");
                }
                Err(_) if lifecycle == BrowserLifecycle::Quiescing && pending.is_none() => {
                    socket = None;
                    authenticated = false;
                }
                Err(error) => return Err(AdapterError::Operation(error.to_string())),
            }
        }
        thread::sleep(POLL_INTERVAL);
    }
    debug_assert_eq!(lifecycle, BrowserLifecycle::Stopped);
    Ok(())
}

// tungstenite's callback contract fixes the large HTTP error-response type.
#[allow(clippy::result_large_err)]
fn accept_bridge(
    stream: TcpStream,
    expected_origin: &str,
) -> Result<WebSocket<TcpStream>, AdapterError> {
    // Accepted streams inherit nonblocking mode on some platforms. The HTTP
    // upgrade itself must be allowed to finish atomically; a bounded read
    // timeout keeps shutdown from being held indefinitely by a partial client.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let expected_origin = expected_origin.to_owned();
    let mut socket = accept_hdr(stream, move |request: &Request, response: Response| {
        let origin = request
            .headers()
            .get("origin")
            .and_then(|value| value.to_str().ok());
        if origin == Some(expected_origin.as_str()) {
            Ok(response)
        } else {
            Err(forbidden("Browser Bridge origin mismatch"))
        }
    })
    .map_err(|error| AdapterError::Operation(error.to_string()))?;
    socket.get_mut().set_nonblocking(true)?;
    Ok(socket)
}

fn forbidden(message: &str) -> ErrorResponse {
    tungstenite::http::Response::builder()
        .status(tungstenite::http::StatusCode::FORBIDDEN)
        .body(Some(message.to_owned()))
        .expect("static rejection response is valid")
}

fn validate_hello(
    config: &TransportConfig,
    protocol: &str,
    channel_credential: &str,
    browser_session: &str,
    top_level_origin: &str,
) -> Result<(), AdapterError> {
    if protocol != BROWSER_PROTOCOL_ID
        || channel_credential != config.channel_credential
        || browser_session != config.browser_session
        || top_level_origin != config.expected_origin
    {
        return Err(AdapterError::Operation(
            "Browser Bridge handshake identity mismatch".to_owned(),
        ));
    }
    Ok(())
}

fn send_message(
    socket: &mut WebSocket<TcpStream>,
    message: &AdapterMessage<'_>,
) -> Result<(), AdapterError> {
    socket
        .send(Message::Text(serde_json::to_string(message)?.into()))
        .map_err(|error| AdapterError::Operation(error.to_string()))
}

fn emit_event(
    config: &TransportConfig,
    observations: Arc<dyn ObservationSink>,
    event: &BrowserEvent,
) -> Result<(), AdapterError> {
    observations.emit(AdapterObservation {
        adapter_id: BROWSER_ADAPTER_ID.to_owned(),
        protocol_id: ProtocolId::parse(BROWSER_PROTOCOL_ID)
            .expect("static Browser Protocol ID is valid"),
        port_id: config.port_id.clone(),
        direction: Direction::Inbound,
        payload: encode_event_with_policy(event, &config.allowed_non_text_codes)
            .map_err(|error| AdapterError::Operation(error.to_string()))?,
        caused_by: Vec::new(),
        effect: ObservationEffect::Evolution,
        presentation_hint: if event.is_archive_keyframe_candidate() {
            PresentationHint::Keyframe
        } else {
            PresentationHint::None
        },
    })
}

fn flush_events(
    coalescer: &mut ContinuousCoalescer,
    config: &TransportConfig,
    observations: Arc<dyn ObservationSink>,
) -> Result<(), AdapterError> {
    for event in coalescer.flush() {
        emit_event(config, Arc::clone(&observations), &event)?;
    }
    Ok(())
}

fn complete_pending(
    pending: &mut Option<PendingRequest>,
    request_id: &str,
    expected_kind: PendingKind,
    result: Result<(), AdapterError>,
) -> Result<(), AdapterError> {
    let Some(request) = pending.take() else {
        return Err(AdapterError::Operation(
            "Browser Bridge acknowledged no pending request".to_owned(),
        ));
    };
    if request.request_id != request_id
        || std::mem::discriminant(&request.kind) != std::mem::discriminant(&expected_kind)
    {
        return Err(AdapterError::Operation(
            "Browser Bridge acknowledgement mismatch".to_owned(),
        ));
    }
    let _ = request.result.send(result);
    Ok(())
}

fn fail_pending(pending: &mut Option<PendingRequest>, reason: &str) {
    if let Some(request) = pending.take() {
        let _ = request
            .result
            .send(Err(AdapterError::Operation(reason.to_owned())));
    }
}

fn fail_queued(commands: &mut VecDeque<TransportCommand>, reason: &str) {
    while let Some(command) = commands.pop_front() {
        fail_command(command, reason);
    }
}

fn expire_queued(commands: &mut VecDeque<TransportCommand>) {
    let now = Instant::now();
    let mut live = VecDeque::with_capacity(commands.len());
    while let Some(command) = commands.pop_front() {
        let (deadline, operation) = match &command {
            TransportCommand::Apply { deadline, .. } => (*deadline, "apply"),
            TransportCommand::Quiesce { deadline, .. } => (*deadline, "quiesce"),
            TransportCommand::Activate { deadline, .. } => (*deadline, "activate"),
            TransportCommand::Pause { deadline, .. } => (*deadline, "capture pause"),
            TransportCommand::Resume { deadline, .. } => (*deadline, "capture resume"),
            TransportCommand::Shutdown => {
                live.push_back(command);
                continue;
            }
        };
        if now >= deadline {
            fail_command(
                command,
                &format!("Browser {operation} acknowledgement timed out"),
            );
        } else {
            live.push_back(command);
        }
    }
    *commands = live;
}

fn fail_command(command: TransportCommand, reason: &str) {
    match command {
        TransportCommand::Apply { result, .. }
        | TransportCommand::Quiesce { result, .. }
        | TransportCommand::Activate { result, .. }
        | TransportCommand::Pause { result, .. }
        | TransportCommand::Resume { result, .. } => {
            let _ = result.send(Err(AdapterError::Operation(reason.to_owned())));
        }
        TransportCommand::Shutdown => {}
    }
}

fn remove_discovery(path: &Path) -> Result<(), AdapterError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn discovery_path(runtime_dir: &Path, instance_id: &str) -> Result<PathBuf, AdapterError> {
    if instance_id.is_empty()
        || !instance_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(AdapterError::InvalidConfig(
            "Browser Adapter instance_id is not safe for runtime discovery".to_owned(),
        ));
    }
    Ok(runtime_dir.join(format!("browser-{instance_id}.json")))
}

fn write_runtime_discovery(
    path: &Path,
    bootstrap: &BrowserRuntimeBootstrap,
) -> Result<(), AdapterError> {
    let parent = path.parent().ok_or_else(|| {
        AdapterError::InvalidConfig("Browser runtime discovery has no parent".to_owned())
    })?;
    std::fs::create_dir_all(parent)?;
    let bytes = serde_jcs::to_vec(bootstrap)?;
    write_owner_only(path, &bytes)
}

#[cfg(unix)]
fn write_owner_only(path: &Path, bytes: &[u8]) -> Result<(), AdapterError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_owner_only(path: &Path, bytes: &[u8]) -> Result<(), AdapterError> {
    std::fs::write(path, bytes)?;
    Ok(())
}

pub(crate) fn wait_for_result(
    receiver: mpsc::Receiver<Result<(), AdapterError>>,
    timeout: Duration,
    operation: &str,
) -> Result<(), AdapterError> {
    let deadline = Instant::now() + timeout;
    receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|error| {
            AdapterError::Operation(format!(
                "Browser {operation} acknowledgement failed: {error}"
            ))
        })?
}

#[cfg(test)]
mod tests {
    use ato_adapter_api::IgnoreObservations;
    use tungstenite::client::IntoClientRequest;
    use tungstenite::{Message, WebSocket, connect};

    use super::*;

    fn config() -> TransportConfig {
        TransportConfig {
            expected_origin: "http://127.0.0.1:3000".to_owned(),
            port_id: PortId::parse("app.browser").expect("test port is valid"),
            allowed_non_text_codes: BTreeSet::new(),
            channel_credential: "credential".to_owned(),
            browser_session: "session".to_owned(),
        }
    }

    fn bootstrap(directory: &Path) -> BrowserRuntimeBootstrap {
        serde_json::from_slice(
            &std::fs::read(discovery_path(directory, "browser.test").expect("path should resolve"))
                .expect("runtime discovery should exist"),
        )
        .expect("runtime discovery should decode")
    }

    fn connect_bridge(
        bootstrap: &BrowserRuntimeBootstrap,
    ) -> WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>> {
        let mut request = bootstrap
            .control_url
            .as_str()
            .into_client_request()
            .expect("control URL should be valid");
        request.headers_mut().insert(
            "origin",
            bootstrap
                .expected_origin
                .parse()
                .expect("test origin should be a header"),
        );
        let (mut socket, _) = connect(request).expect("test Bridge should connect");
        socket
            .send(Message::Text(
                serde_json::json!({
                    "type": "hello",
                    "protocol": bootstrap.protocol,
                    "channel_credential": bootstrap.channel_credential,
                    "browser_session": bootstrap.browser_session,
                    "top_level_origin": bootstrap.expected_origin,
                })
                .to_string()
                .into(),
            ))
            .expect("hello should send");
        let hello = socket.read().expect("hello ack should arrive");
        assert!(
            hello
                .to_text()
                .expect("hello ack should be text")
                .contains("hello_ack")
        );
        socket
    }

    fn click() -> BrowserEvent {
        BrowserEvent::Click {
            x_normalized: 0.5,
            y_normalized: 0.5,
            button: 0,
        }
    }

    #[test]
    fn handshake_rejects_wrong_protocol_channel_session_and_origin() {
        let config = config();
        assert!(
            validate_hello(
                &config,
                BROWSER_PROTOCOL_ID,
                "credential",
                "session",
                "http://127.0.0.1:3000"
            )
            .is_ok()
        );
        for values in [
            ("wrong", "credential", "session", "http://127.0.0.1:3000"),
            (
                BROWSER_PROTOCOL_ID,
                "wrong",
                "session",
                "http://127.0.0.1:3000",
            ),
            (
                BROWSER_PROTOCOL_ID,
                "credential",
                "wrong",
                "http://127.0.0.1:3000",
            ),
            (
                BROWSER_PROTOCOL_ID,
                "credential",
                "session",
                "https://example.test",
            ),
        ] {
            assert!(validate_hello(&config, values.0, values.1, values.2, values.3).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn workspace_scoped_control_relay_cleans_up_its_unix_socket() {
        use std::os::unix::fs::FileTypeExt;

        let directory = tempfile::tempdir().expect("temporary workspace should open");
        let target = TcpListener::bind("127.0.0.1:0").expect("target should bind");
        let relay = ControlRelay::start(
            directory.path(),
            "browser.test",
            target.local_addr().expect("target address"),
        )
        .expect("relay should start");
        let file_name = relay.file_name.clone();
        assert!(!file_name.contains('/'));
        let socket = directory.path().join(&file_name);
        assert!(
            std::fs::symlink_metadata(&socket)
                .expect("control socket should exist")
                .file_type()
                .is_socket()
        );
        drop(relay);
        assert!(!socket.exists());
    }

    #[test]
    fn shutdown_is_not_starved_by_an_apply_waiting_for_a_bridge() {
        let directory = tempfile::tempdir().expect("temporary workspace should open");
        let config = config();
        let mut handle = start_transport(
            directory.path(),
            "browser.test",
            config,
            Arc::new(IgnoreObservations),
        )
        .expect("transport should start");
        let (result, receiver) = mpsc::channel();
        handle
            .commands
            .send(TransportCommand::Apply {
                request_id: "1".to_owned(),
                event: click(),
                deadline: Instant::now() + Duration::from_secs(1),
                ack_timeout: Duration::from_millis(50),
                result,
            })
            .expect("apply should queue");
        handle
            .commands
            .send(TransportCommand::Shutdown)
            .expect("shutdown should queue");
        handle
            .join
            .take()
            .expect("transport thread should exist")
            .join()
            .expect("transport should stop without a Bridge");
        assert!(
            receiver
                .recv()
                .expect("apply result should return")
                .is_err()
        );
    }

    #[test]
    fn apply_timeout_is_owned_by_transport_and_cleanup_still_completes() {
        let directory = tempfile::tempdir().expect("temporary workspace should open");
        let mut handle = start_transport(
            directory.path(),
            "browser.test",
            config(),
            Arc::new(IgnoreObservations),
        )
        .expect("transport should start");
        let mut socket = connect_bridge(&bootstrap(directory.path()));
        let (result, receiver) = mpsc::channel();
        handle
            .commands
            .send(TransportCommand::Apply {
                request_id: "apply-timeout".to_owned(),
                event: click(),
                deadline: Instant::now() + Duration::from_millis(50),
                ack_timeout: Duration::from_millis(50),
                result,
            })
            .expect("apply should queue");
        let apply = socket.read().expect("apply should arrive");
        assert!(
            apply
                .to_text()
                .expect("apply should be text")
                .contains("apply")
        );
        let error = receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("transport should resolve timeout")
            .expect_err("missing ACK must fail");
        assert!(error.to_string().contains("timed out"));
        handle
            .shutdown()
            .expect("shutdown should complete after timeout");
        assert!(handle.join.is_none());
        assert!(
            !discovery_path(directory.path(), "browser.test")
                .expect("path should resolve")
                .exists()
        );
    }

    #[test]
    fn pending_apply_cannot_delay_shutdown() {
        let directory = tempfile::tempdir().expect("temporary workspace should open");
        let mut handle = start_transport(
            directory.path(),
            "browser.test",
            config(),
            Arc::new(IgnoreObservations),
        )
        .expect("transport should start");
        let mut socket = connect_bridge(&bootstrap(directory.path()));
        let (result, receiver) = mpsc::channel();
        handle
            .commands
            .send(TransportCommand::Apply {
                request_id: "pending".to_owned(),
                event: click(),
                deadline: Instant::now() + Duration::from_secs(5),
                ack_timeout: Duration::from_secs(5),
                result,
            })
            .expect("apply should queue");
        socket.read().expect("apply should arrive");
        let started = Instant::now();
        handle
            .shutdown()
            .expect("shutdown should preempt pending apply");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(receiver.recv().expect("apply should resolve").is_err());
    }

    #[test]
    fn capture_pause_blocks_input_and_can_resume_the_same_bridge() {
        let directory = tempfile::tempdir().expect("temporary workspace should open");
        let mut handle = start_transport(
            directory.path(),
            "browser.test",
            config(),
            Arc::new(IgnoreObservations),
        )
        .expect("transport should start");
        let mut socket = connect_bridge(&bootstrap(directory.path()));

        let (pause_result, pause_receiver) = mpsc::channel();
        handle
            .commands
            .send(TransportCommand::Pause {
                request_id: "pause".to_owned(),
                deadline: Instant::now() + Duration::from_secs(1),
                ack_timeout: Duration::from_secs(1),
                result: pause_result,
            })
            .expect("pause should queue");
        let block = socket.read().expect("input block should arrive");
        assert!(
            block
                .to_text()
                .expect("block should be text")
                .contains("block_input")
        );
        let pause = socket.read().expect("pause quiesce should arrive");
        assert!(
            pause
                .to_text()
                .expect("pause should be text")
                .contains("quiesce")
        );
        socket
            .send(Message::Text(
                serde_json::json!({"type": "quiesced", "request_id": "pause"})
                    .to_string()
                    .into(),
            ))
            .expect("pause acknowledgement should send");
        pause_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("pause should resolve")
            .expect("pause should succeed");

        let (resume_result, resume_receiver) = mpsc::channel();
        handle
            .commands
            .send(TransportCommand::Resume {
                request_id: "resume".to_owned(),
                deadline: Instant::now() + Duration::from_secs(1),
                ack_timeout: Duration::from_secs(1),
                result: resume_result,
            })
            .expect("resume should queue");
        let resume = socket.read().expect("activate should arrive");
        assert!(
            resume
                .to_text()
                .expect("resume should be text")
                .contains("activate")
        );
        socket
            .send(Message::Text(
                serde_json::json!({"type": "activated", "request_id": "resume"})
                    .to_string()
                    .into(),
            ))
            .expect("resume acknowledgement should send");
        resume_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("resume should resolve")
            .expect("resume should succeed");

        handle
            .shutdown()
            .expect("resumed transport should still shut down cleanly");
    }

    #[test]
    fn quiesce_timeout_does_not_prevent_shutdown_cleanup() {
        let directory = tempfile::tempdir().expect("temporary workspace should open");
        let mut handle = start_transport(
            directory.path(),
            "browser.test",
            config(),
            Arc::new(IgnoreObservations),
        )
        .expect("transport should start");
        let mut socket = connect_bridge(&bootstrap(directory.path()));
        let (result, receiver) = mpsc::channel();
        handle
            .commands
            .send(TransportCommand::Quiesce {
                request_id: "quiesce-timeout".to_owned(),
                deadline: Instant::now() + Duration::from_millis(50),
                ack_timeout: Duration::from_millis(50),
                result,
            })
            .expect("quiesce should queue");
        loop {
            let message = socket.read().expect("lifecycle message should arrive");
            if message
                .to_text()
                .expect("lifecycle message should be text")
                .contains("quiesce")
            {
                break;
            }
        }
        assert!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("quiesce should resolve")
                .is_err()
        );
        handle
            .shutdown()
            .expect("shutdown should complete after quiesce timeout");
        assert!(
            !discovery_path(directory.path(), "browser.test")
                .expect("path should resolve")
                .exists()
        );
    }

    #[test]
    fn quiesce_without_bridge_closes_listener_before_returning() {
        let directory = tempfile::tempdir().expect("temporary workspace should open");
        let mut handle = start_transport(
            directory.path(),
            "browser.test",
            config(),
            Arc::new(IgnoreObservations),
        )
        .expect("transport should start");
        let bootstrap = bootstrap(directory.path());
        let (result, receiver) = mpsc::channel();
        handle
            .commands
            .send(TransportCommand::Quiesce {
                request_id: "quiesce".to_owned(),
                deadline: Instant::now() + Duration::from_secs(1),
                ack_timeout: Duration::from_millis(50),
                result,
            })
            .expect("quiesce should queue");
        receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("quiesce should resolve")
            .expect("quiesce without a Bridge should flush cleanly");
        assert!(connect_bridge_result(&bootstrap).is_err());
        handle.shutdown().expect("shutdown should complete");
    }

    fn connect_bridge_result(
        bootstrap: &BrowserRuntimeBootstrap,
    ) -> tungstenite::Result<(
        WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>,
        tungstenite::handshake::client::Response,
    )> {
        let mut request = bootstrap.control_url.as_str().into_client_request()?;
        request.headers_mut().insert(
            "origin",
            bootstrap
                .expected_origin
                .parse()
                .expect("origin should parse"),
        );
        connect(request)
    }
}
