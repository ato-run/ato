use std::collections::{BTreeSet, VecDeque};
use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ato_adapter_api::{AdapterError, AdapterObservation, ObservationEffect, ObservationSink};
use ato_computation::{PortId, ProtocolId};
use ato_objects::Direction;
use serde::{Deserialize, Serialize};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::{Message, WebSocket, accept_hdr};

use crate::coalescer::ContinuousCoalescer;
use crate::protocol::{BrowserEvent, encode_event_with_policy, validate_event};
use crate::{BROWSER_ADAPTER_ID, BROWSER_PROTOCOL_ID, BrowserInputMode, BrowserStylus};

const POLL_INTERVAL: Duration = Duration::from_millis(2);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserRuntimeBootstrap {
    pub protocol: String,
    pub control_url: String,
    pub channel_credential: String,
    pub browser_session: String,
    pub expected_origin: String,
    pub allowed_non_text_codes: BTreeSet<String>,
    pub input_mode: BrowserInputMode,
}

pub(crate) enum TransportCommand {
    Apply {
        request_id: String,
        event: BrowserEvent,
        result: mpsc::Sender<Result<(), AdapterError>>,
    },
    Quiesce {
        request_id: String,
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
    },
    Apply {
        request_id: &'a str,
        event: &'a BrowserEvent,
    },
    Quiesce {
        request_id: &'a str,
    },
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
    Error {
        #[serde(default)]
        request_id: Option<String>,
        reason: String,
    },
}

struct PendingRequest {
    request_id: String,
    kind: PendingKind,
    result: mpsc::Sender<Result<(), AdapterError>>,
}

#[derive(Clone, Copy)]
enum PendingKind {
    Apply,
    Quiesce,
}

pub(crate) struct TransportHandle {
    pub discovery_path: PathBuf,
    pub readiness_path: PathBuf,
    pub commands: mpsc::Sender<TransportCommand>,
    pub failure: Arc<Mutex<Option<String>>>,
    pub join: Option<JoinHandle<()>>,
}

pub(crate) struct TransportConfig {
    pub expected_origin: String,
    pub port_id: PortId,
    pub allowed_non_text_codes: BTreeSet<String>,
    pub channel_credential: String,
    pub browser_session: String,
    pub input_mode: BrowserInputMode,
}

pub(crate) fn start_transport(
    workspace: &Path,
    instance_id: &str,
    config: TransportConfig,
    stylus: Arc<BrowserStylus>,
    observations: Arc<dyn ObservationSink>,
) -> Result<TransportHandle, AdapterError> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let bootstrap = BrowserRuntimeBootstrap {
        protocol: BROWSER_PROTOCOL_ID.to_owned(),
        control_url: format!("ws://{address}"),
        channel_credential: config.channel_credential.clone(),
        browser_session: config.browser_session.clone(),
        expected_origin: config.expected_origin.clone(),
        allowed_non_text_codes: config.allowed_non_text_codes.clone(),
        input_mode: config.input_mode,
    };
    let discovery_path = discovery_path(workspace, instance_id)?;
    write_runtime_discovery(&discovery_path, &bootstrap)?;
    let readiness_path = discovery_path.with_extension("ready");
    let (commands, receiver) = mpsc::channel();
    let failure = Arc::new(Mutex::new(None));
    let thread_failure = Arc::clone(&failure);
    let thread_readiness_path = readiness_path.clone();
    let join = thread::spawn(move || {
        if let Err(error) = run_transport(
            listener,
            config,
            stylus,
            observations,
            receiver,
            thread_readiness_path,
        ) && let Ok(mut slot) = thread_failure.lock()
        {
            *slot = Some(error.to_string());
        }
    });
    Ok(TransportHandle {
        discovery_path,
        readiness_path,
        commands,
        failure,
        join: Some(join),
    })
}

fn run_transport(
    listener: TcpListener,
    config: TransportConfig,
    stylus: Arc<BrowserStylus>,
    observations: Arc<dyn ObservationSink>,
    commands: mpsc::Receiver<TransportCommand>,
    readiness_path: PathBuf,
) -> Result<(), AdapterError> {
    let mut socket = None;
    let mut authenticated = false;
    let mut accepting_input = true;
    let mut coalescer = ContinuousCoalescer::default();
    let mut queued_commands = VecDeque::new();
    let mut pending: Option<PendingRequest> = None;
    let stopped = AtomicBool::new(false);

    while !stopped.load(Ordering::Acquire) {
        if socket.is_none() {
            match listener.accept() {
                Ok((stream, _)) => match accept_bridge(stream, &config.expected_origin) {
                    Ok(bridge) => socket = Some(bridge),
                    Err(_) => continue,
                },
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
        }

        while let Ok(command) = commands.try_recv() {
            queued_commands.push_back(command);
        }

        if pending.is_none()
            && let Some(command) = queued_commands.pop_front()
        {
            match command {
                TransportCommand::Apply {
                    request_id,
                    event,
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
                            result,
                        });
                    } else {
                        queued_commands.push_front(TransportCommand::Apply {
                            request_id,
                            event,
                            result,
                        });
                    }
                }
                TransportCommand::Quiesce { request_id, result } => {
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
                            result,
                        });
                    } else {
                        flush_events(
                            &mut coalescer,
                            &config,
                            Arc::clone(&stylus),
                            Arc::clone(&observations),
                        )?;
                        let _ = result.send(Ok(()));
                    }
                }
                TransportCommand::Shutdown => stopped.store(true, Ordering::Release),
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
                                },
                            )?;
                            write_owner_only(&readiness_path, b"ready")?;
                        }
                        BridgeMessage::Event { event }
                            if authenticated
                                && accepting_input
                                && config.input_mode.observes_trusted_events() =>
                        {
                            if validate_event(&event, &config.allowed_non_text_codes).is_ok() {
                                for ready in coalescer.ingest(event) {
                                    emit_event(
                                        &config,
                                        Arc::clone(&stylus),
                                        Arc::clone(&observations),
                                        &ready,
                                    )?;
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
                            accepting_input = false;
                            flush_events(
                                &mut coalescer,
                                &config,
                                Arc::clone(&stylus),
                                Arc::clone(&observations),
                            )?;
                            complete_pending(
                                &mut pending,
                                &request_id,
                                PendingKind::Quiesce,
                                Ok(()),
                            )?;
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
                Err(_) if !accepting_input && pending.is_none() => {
                    socket = None;
                    authenticated = false;
                }
                Err(error) => return Err(AdapterError::Operation(error.to_string())),
            }
        }
        thread::sleep(POLL_INTERVAL);
    }
    fail_pending(&mut pending, "Browser Adapter detached");
    for command in queued_commands {
        match command {
            TransportCommand::Apply { result, .. } | TransportCommand::Quiesce { result, .. } => {
                let _ = result.send(Err(AdapterError::Operation(
                    "Browser Adapter detached".to_owned(),
                )));
            }
            TransportCommand::Shutdown => {}
        }
    }
    Ok(())
}

// tungstenite's callback contract fixes the large HTTP error-response type.
#[allow(clippy::result_large_err)]
fn accept_bridge(
    stream: TcpStream,
    expected_origin: &str,
) -> Result<WebSocket<TcpStream>, AdapterError> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
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
    stylus: Arc<BrowserStylus>,
    observations: Arc<dyn ObservationSink>,
    event: &BrowserEvent,
) -> Result<(), AdapterError> {
    stylus.record(event)?;
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
    })
}

fn flush_events(
    coalescer: &mut ContinuousCoalescer,
    config: &TransportConfig,
    stylus: Arc<BrowserStylus>,
    observations: Arc<dyn ObservationSink>,
) -> Result<(), AdapterError> {
    for event in coalescer.flush() {
        emit_event(
            config,
            Arc::clone(&stylus),
            Arc::clone(&observations),
            &event,
        )?;
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

fn discovery_path(workspace: &Path, instance_id: &str) -> Result<PathBuf, AdapterError> {
    if instance_id.is_empty()
        || !instance_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(AdapterError::InvalidConfig(
            "Browser Adapter instance_id is not safe for runtime discovery".to_owned(),
        ));
    }
    Ok(workspace
        .join(".capsule/runs")
        .join(format!("browser-{instance_id}.json")))
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
    use super::*;

    fn config() -> TransportConfig {
        TransportConfig {
            expected_origin: "http://127.0.0.1:3000".to_owned(),
            port_id: PortId::parse("app.browser").expect("test port is valid"),
            allowed_non_text_codes: BTreeSet::new(),
            channel_credential: "credential".to_owned(),
            browser_session: "session".to_owned(),
            input_mode: BrowserInputMode::ObserveAndApply,
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
}
