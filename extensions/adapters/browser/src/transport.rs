use std::collections::{BTreeMap, BTreeSet, VecDeque};
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
use crate::{
    BROWSER_ADAPTER_ID, BROWSER_PROTOCOL_ID, BrowserChannelScope, BrowserInputMode, BrowserStylus,
};

const POLL_INTERVAL: Duration = Duration::from_millis(2);
const ACK_HISTORY_LIMIT: usize = 1_024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserRuntimeBootstrap {
    pub protocol: String,
    pub control_url: String,
    pub channel_credential: String,
    pub browser_session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_scope: Option<BrowserChannelScope>,
    pub expected_origin: String,
    pub allowed_non_text_codes: BTreeSet<String>,
    pub input_mode: BrowserInputMode,
}

pub(crate) enum TransportCommand {
    Apply {
        request_id: String,
        correlation_id: String,
        realization_generation: Option<String>,
        ordered: bool,
        deadline: Instant,
        event: BrowserEvent,
        result: mpsc::Sender<Result<Option<u64>, AdapterError>>,
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
        last_sequence: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        channel_scope: Option<&'a BrowserChannelScope>,
    },
    Apply {
        request_id: &'a str,
        sequence: u64,
        operation_id: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        realization_generation: Option<&'a str>,
        event: &'a BrowserEvent,
    },
    Quiesce {
        request_id: &'a str,
        sequence: u64,
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
        #[serde(default)]
        channel_scope: Option<BrowserChannelScope>,
    },
    Event {
        event: BrowserEvent,
    },
    Ack {
        request_id: String,
        sequence: u64,
    },
    Quiesced {
        request_id: String,
        sequence: u64,
    },
    Error {
        #[serde(default)]
        request_id: Option<String>,
        #[serde(default)]
        sequence: Option<u64>,
        reason: String,
    },
}

struct PendingRequest {
    kind: PendingKind,
    sequence: u64,
    ordered: bool,
    deadline: Option<Instant>,
    result: PendingResult,
}

enum PendingResult {
    Apply(mpsc::Sender<Result<Option<u64>, AdapterError>>),
    Quiesce(mpsc::Sender<Result<(), AdapterError>>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PendingKind {
    Apply,
    Quiesce,
}

#[derive(Default)]
struct AcknowledgementHistory {
    terminal: BTreeMap<String, (PendingKind, u64)>,
    insertion_order: VecDeque<String>,
}

impl AcknowledgementHistory {
    fn contains(&self, request_id: &str, expected_kind: PendingKind, sequence: u64) -> bool {
        self.terminal.get(request_id) == Some(&(expected_kind, sequence))
    }

    fn remember(&mut self, request_id: String, kind: PendingKind, sequence: u64) {
        if self
            .terminal
            .insert(request_id.clone(), (kind, sequence))
            .is_none()
        {
            self.insertion_order.push_back(request_id);
        }
        while self.insertion_order.len() > ACK_HISTORY_LIMIT {
            if let Some(expired) = self.insertion_order.pop_front() {
                self.terminal.remove(&expired);
            }
        }
    }
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
    pub channel_scope: Option<BrowserChannelScope>,
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
        channel_scope: config.channel_scope.clone(),
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
    let mut pending = BTreeMap::<String, PendingRequest>::new();
    let mut acknowledgement_history = AcknowledgementHistory::default();
    let mut next_live_settlement_order = 0_u64;
    let mut next_command_sequence = 0_u64;
    let stopped = AtomicBool::new(false);

    macro_rules! transport_try {
        ($result:expr) => {
            match $result {
                Ok(value) => value,
                Err(error) => return terminate_after_error(&mut pending, error),
            }
        };
    }

    while !stopped.load(Ordering::Acquire) {
        if socket.is_none() {
            match listener.accept() {
                Ok((stream, _)) => match accept_bridge(stream, &config.expected_origin) {
                    Ok(bridge) => socket = Some(bridge),
                    Err(_) => continue,
                },
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) => return terminate_after_error(&mut pending, error.into()),
            }
        }

        while let Ok(command) = commands.try_recv() {
            queued_commands.push_back(command);
        }
        transport_try!(expire_pending_deadlines(
            &mut pending,
            &mut acknowledgement_history,
            Instant::now(),
        ));

        while let Some(command) = queued_commands.front() {
            if matches!(command, TransportCommand::Shutdown) {
                queued_commands.pop_front();
                stopped.store(true, Ordering::Release);
                break;
            }
            if matches!(command, TransportCommand::Apply { deadline, .. } if Instant::now() >= *deadline)
            {
                let TransportCommand::Apply { result, .. } = queued_commands
                    .pop_front()
                    .expect("expired front command was present")
                else {
                    unreachable!("expiry is checked only for Apply commands");
                };
                let _ = result.send(Err(AdapterError::Operation(
                    "Browser apply acknowledgement timed out before dispatch".to_owned(),
                )));
                continue;
            }
            if socket.as_ref().is_none_or(|_| !authenticated)
                || matches!(command, TransportCommand::Quiesce { .. }) && !pending.is_empty()
            {
                break;
            }
            let command = queued_commands
                .pop_front()
                .expect("front command was present");
            match command {
                TransportCommand::Apply {
                    request_id,
                    correlation_id,
                    realization_generation,
                    ordered,
                    deadline,
                    event,
                    result,
                } => {
                    transport_try!(validate_channel_not_expired(&config));
                    next_command_sequence =
                        transport_try!(next_command_sequence.checked_add(1).ok_or_else(|| {
                            AdapterError::Operation("Browser command sequence exhausted".to_owned())
                        }));
                    pending.insert(
                        request_id.clone(),
                        PendingRequest {
                            kind: PendingKind::Apply,
                            sequence: next_command_sequence,
                            ordered,
                            deadline: Some(deadline),
                            result: PendingResult::Apply(result),
                        },
                    );
                    let bridge = socket
                        .as_mut()
                        .filter(|_| authenticated)
                        .expect("authenticated bridge was checked");
                    transport_try!(send_message(
                        bridge,
                        &AdapterMessage::Apply {
                            request_id: &request_id,
                            sequence: next_command_sequence,
                            operation_id: &correlation_id,
                            realization_generation: realization_generation.as_deref(),
                            event: &event,
                        },
                    ));
                }
                TransportCommand::Quiesce { request_id, result } => {
                    if let Some(bridge) = socket.as_mut().filter(|_| authenticated) {
                        transport_try!(validate_channel_not_expired(&config));
                        next_command_sequence =
                            transport_try!(next_command_sequence.checked_add(1).ok_or_else(|| {
                                AdapterError::Operation(
                                    "Browser command sequence exhausted".to_owned(),
                                )
                            }));
                        pending.insert(
                            request_id.clone(),
                            PendingRequest {
                                kind: PendingKind::Quiesce,
                                sequence: next_command_sequence,
                                ordered: false,
                                deadline: None,
                                result: PendingResult::Quiesce(result),
                            },
                        );
                        transport_try!(send_message(
                            bridge,
                            &AdapterMessage::Quiesce {
                                request_id: &request_id,
                                sequence: next_command_sequence,
                            },
                        ));
                    } else {
                        transport_try!(flush_events(
                            &mut coalescer,
                            &config,
                            Arc::clone(&stylus),
                            Arc::clone(&observations),
                        ));
                        let _ = result.send(Ok(()));
                    }
                }
                TransportCommand::Shutdown => stopped.store(true, Ordering::Release),
            }
        }

        transport_try!(expire_pending_deadlines(
            &mut pending,
            &mut acknowledgement_history,
            Instant::now(),
        ));
        if let Some(bridge) = socket.as_mut() {
            match bridge.read() {
                Ok(Message::Text(text)) => {
                    let message: BridgeMessage =
                        transport_try!(serde_json::from_str(&text).map_err(|error| {
                            AdapterError::Operation(format!(
                                "invalid Browser Bridge message: {error}"
                            ))
                        }));
                    match message {
                        BridgeMessage::Hello {
                            protocol,
                            channel_credential,
                            browser_session,
                            top_level_origin,
                            channel_scope,
                        } if !authenticated => {
                            transport_try!(validate_hello(
                                &config,
                                &protocol,
                                &channel_credential,
                                &browser_session,
                                &top_level_origin,
                                channel_scope.as_ref(),
                            ));
                            authenticated = true;
                            transport_try!(send_message(
                                bridge,
                                &AdapterMessage::HelloAck {
                                    protocol: BROWSER_PROTOCOL_ID,
                                    browser_session: &config.browser_session,
                                    last_sequence: next_command_sequence,
                                    channel_scope: config.channel_scope.as_ref(),
                                },
                            ));
                            transport_try!(write_owner_only(&readiness_path, b"ready"));
                        }
                        BridgeMessage::Event { event }
                            if authenticated
                                && accepting_input
                                && config.input_mode.observes_trusted_events() =>
                        {
                            if validate_event(&event, &config.allowed_non_text_codes).is_ok() {
                                for ready in coalescer.ingest(event) {
                                    transport_try!(emit_event(
                                        &config,
                                        Arc::clone(&stylus),
                                        Arc::clone(&observations),
                                        &ready,
                                    ));
                                }
                            }
                        }
                        BridgeMessage::Ack {
                            request_id,
                            sequence,
                        } if authenticated => {
                            transport_try!(validate_channel_not_expired(&config));
                            transport_try!(complete_pending(
                                &mut pending,
                                &request_id,
                                PendingKind::Apply,
                                Ok(()),
                                sequence,
                                &mut next_live_settlement_order,
                                &mut acknowledgement_history,
                            ));
                        }
                        BridgeMessage::Quiesced {
                            request_id,
                            sequence,
                        } if authenticated => {
                            transport_try!(validate_channel_not_expired(&config));
                            accepting_input = false;
                            transport_try!(flush_events(
                                &mut coalescer,
                                &config,
                                Arc::clone(&stylus),
                                Arc::clone(&observations),
                            ));
                            transport_try!(complete_pending(
                                &mut pending,
                                &request_id,
                                PendingKind::Quiesce,
                                Ok(()),
                                sequence,
                                &mut next_live_settlement_order,
                                &mut acknowledgement_history,
                            ));
                        }
                        BridgeMessage::Error {
                            request_id,
                            sequence,
                            reason,
                        } if authenticated => {
                            transport_try!(validate_channel_not_expired(&config));
                            if let Some(request_id) = request_id {
                                let sequence = sequence.ok_or_else(|| {
                                    AdapterError::Operation(
                                        "Browser Bridge request error omitted sequence".to_owned(),
                                    )
                                });
                                let sequence = transport_try!(sequence);
                                let pending_kind = pending
                                    .get(&request_id)
                                    .map_or(PendingKind::Apply, |value| value.kind);
                                transport_try!(complete_pending(
                                    &mut pending,
                                    &request_id,
                                    pending_kind,
                                    Err(AdapterError::Operation(format!(
                                        "Browser Bridge rejected request: {reason}"
                                    ))),
                                    sequence,
                                    &mut next_live_settlement_order,
                                    &mut acknowledgement_history,
                                ));
                            } else {
                                return terminate_after_error(
                                    &mut pending,
                                    AdapterError::Operation(format!(
                                        "Browser Bridge failed: {reason}"
                                    )),
                                );
                            }
                        }
                        _ => {
                            return terminate_after_error(
                                &mut pending,
                                AdapterError::Operation(
                                    "Browser Bridge message violates handshake or lifecycle"
                                        .to_owned(),
                                ),
                            );
                        }
                    }
                }
                Ok(Message::Close(_)) => {
                    if fail_pending_after_disconnect(&mut pending) {
                        return Err(AdapterError::Operation(
                            "physical_outcome_indeterminate".to_owned(),
                        ));
                    }
                    socket = None;
                    authenticated = false;
                }
                Ok(Message::Ping(payload)) => transport_try!(
                    bridge
                        .send(Message::Pong(payload))
                        .map_err(|error| AdapterError::Operation(error.to_string()))
                ),
                Ok(_) => {}
                Err(tungstenite::Error::Io(error)) if error.kind() == ErrorKind::WouldBlock => {}
                Err(tungstenite::Error::ConnectionClosed) => {
                    if fail_pending_after_disconnect(&mut pending) {
                        return Err(AdapterError::Operation(
                            "physical_outcome_indeterminate".to_owned(),
                        ));
                    }
                    socket = None;
                    authenticated = false;
                }
                Err(_) if !accepting_input && pending.is_empty() => {
                    socket = None;
                    authenticated = false;
                }
                Err(error) => {
                    let reason = if has_ordered_apply(&pending) {
                        "physical_outcome_indeterminate".to_owned()
                    } else {
                        format!("Browser Bridge failed: {error}")
                    };
                    fail_pending(&mut pending, &reason);
                    return Err(AdapterError::Operation(reason));
                }
            }
        }
        thread::sleep(POLL_INTERVAL);
    }
    fail_pending(&mut pending, "Browser Adapter detached");
    for command in queued_commands {
        match command {
            TransportCommand::Apply { result, .. } => {
                let _ = result.send(Err(AdapterError::Operation(
                    "Browser Adapter detached".to_owned(),
                )));
            }
            TransportCommand::Quiesce { result, .. } => {
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
    channel_scope: Option<&BrowserChannelScope>,
) -> Result<(), AdapterError> {
    validate_channel_not_expired(config)?;
    if protocol != BROWSER_PROTOCOL_ID
        || channel_credential != config.channel_credential
        || browser_session != config.browser_session
        || top_level_origin != config.expected_origin
        || channel_scope != config.channel_scope.as_ref()
    {
        return Err(AdapterError::Operation(
            "Browser Bridge handshake identity mismatch".to_owned(),
        ));
    }
    Ok(())
}

fn validate_channel_not_expired(config: &TransportConfig) -> Result<(), AdapterError> {
    if config.channel_scope.as_ref().is_some_and(|scope| {
        scope.expires_at_unix_seconds
            <= std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |value| value.as_secs().try_into().unwrap_or(i64::MAX))
    }) {
        return Err(AdapterError::Operation(
            "Browser Bridge channel scope expired".to_owned(),
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
    pending: &mut BTreeMap<String, PendingRequest>,
    request_id: &str,
    expected_kind: PendingKind,
    result: Result<(), AdapterError>,
    sequence: u64,
    next_live_settlement_order: &mut u64,
    acknowledgement_history: &mut AcknowledgementHistory,
) -> Result<(), AdapterError> {
    let Some(request) = pending.get(request_id) else {
        if acknowledgement_history.contains(request_id, expected_kind, sequence) {
            return Ok(());
        }
        return Err(AdapterError::Operation(
            "Browser Bridge acknowledged no pending request".to_owned(),
        ));
    };
    if std::mem::discriminant(&request.kind) != std::mem::discriminant(&expected_kind) {
        return Err(AdapterError::Operation(
            "Browser Bridge acknowledgement mismatch".to_owned(),
        ));
    }
    if request.sequence != sequence {
        return Err(AdapterError::Operation(
            "Browser Bridge acknowledgement sequence mismatch".to_owned(),
        ));
    }
    if request
        .deadline
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        return Err(AdapterError::Operation(if request.ordered {
            "physical_outcome_indeterminate".to_owned()
        } else {
            "Browser apply acknowledgement timed out".to_owned()
        }));
    }
    let request = pending
        .remove(request_id)
        .expect("validated pending request remains present");
    acknowledgement_history.remember(request_id.to_owned(), request.kind, request.sequence);
    match request.result {
        PendingResult::Apply(sender) if request.ordered && result.is_ok() => {
            let next = next_live_settlement_order.checked_add(1).ok_or_else(|| {
                AdapterError::Operation("Browser settlement order exhausted".to_owned())
            })?;
            sender.send(Ok(Some(next))).map_err(|_| {
                AdapterError::Operation("physical_outcome_indeterminate".to_owned())
            })?;
            *next_live_settlement_order = next;
        }
        PendingResult::Apply(sender) => {
            let _ = sender.send(result.map(|()| None));
        }
        PendingResult::Quiesce(sender) => {
            let _ = sender.send(result);
        }
    }
    Ok(())
}

fn fail_pending(pending: &mut BTreeMap<String, PendingRequest>, reason: &str) {
    for (_, request) in std::mem::take(pending) {
        match request.result {
            PendingResult::Apply(sender) => {
                let _ = sender.send(Err(AdapterError::Operation(reason.to_owned())));
            }
            PendingResult::Quiesce(sender) => {
                let _ = sender.send(Err(AdapterError::Operation(reason.to_owned())));
            }
        }
    }
}

fn terminate_after_error(
    pending: &mut BTreeMap<String, PendingRequest>,
    error: AdapterError,
) -> Result<(), AdapterError> {
    if has_ordered_apply(pending) {
        let reason = "physical_outcome_indeterminate";
        fail_pending(pending, reason);
        Err(AdapterError::Operation(reason.to_owned()))
    } else {
        fail_pending(pending, &error.to_string());
        Err(error)
    }
}

fn expire_pending_deadlines(
    pending: &mut BTreeMap<String, PendingRequest>,
    acknowledgement_history: &mut AcknowledgementHistory,
    now: Instant,
) -> Result<(), AdapterError> {
    if pending.values().any(|request| {
        request.ordered
            && matches!(request.kind, PendingKind::Apply)
            && request.deadline.is_some_and(|deadline| now >= deadline)
    }) {
        return Err(AdapterError::Operation(
            "physical_outcome_indeterminate".to_owned(),
        ));
    }
    let expired = pending
        .iter()
        .filter(|(_, request)| {
            !request.ordered
                && matches!(request.kind, PendingKind::Apply)
                && request.deadline.is_some_and(|deadline| now >= deadline)
        })
        .map(|(request_id, _)| request_id.clone())
        .collect::<Vec<_>>();
    for request_id in expired {
        let request = pending
            .remove(&request_id)
            .expect("expired pending request remains present");
        acknowledgement_history.remember(request_id, request.kind, request.sequence);
        if let PendingResult::Apply(sender) = request.result {
            let _ = sender.send(Err(AdapterError::Operation(
                "Browser apply acknowledgement timed out".to_owned(),
            )));
        }
    }
    Ok(())
}

fn has_ordered_apply(pending: &BTreeMap<String, PendingRequest>) -> bool {
    pending
        .values()
        .any(|request| request.ordered && matches!(request.kind, PendingKind::Apply))
}

fn fail_pending_after_disconnect(pending: &mut BTreeMap<String, PendingRequest>) -> bool {
    let indeterminate = has_ordered_apply(pending);
    fail_pending(
        pending,
        if indeterminate {
            "physical_outcome_indeterminate"
        } else {
            "Browser Bridge disconnected"
        },
    );
    indeterminate
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

pub(crate) fn wait_for_apply_result(
    receiver: mpsc::Receiver<Result<Option<u64>, AdapterError>>,
) -> Result<Option<u64>, AdapterError> {
    receiver.recv().map_err(|_| {
        AdapterError::Operation("Browser apply acknowledgement disconnected".to_owned())
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
            channel_scope: None,
            input_mode: BrowserInputMode::ObserveAndApply,
        }
    }

    fn scoped_config() -> TransportConfig {
        TransportConfig {
            channel_scope: Some(BrowserChannelScope {
                activity_id: "activity-1".to_owned(),
                run_id: "run-1".to_owned(),
                epoch: "lease-1".to_owned(),
                expires_at_unix_seconds: i64::MAX,
            }),
            ..config()
        }
    }

    #[test]
    fn apply_wire_separates_ack_request_from_controller_correlation() {
        let event = BrowserEvent::Operation {
            operation_name: "slow_increment".to_owned(),
            arguments: serde_json::json!({"delay_ms":10}),
            surface_generation: 4,
        };
        let value = serde_json::to_value(AdapterMessage::Apply {
            request_id: "transport-17",
            sequence: 17,
            operation_id: "aop_controller_9",
            realization_generation: Some("document_4"),
            event: &event,
        })
        .expect("apply message should serialize");
        assert_eq!(value["request_id"], "transport-17");
        assert_eq!(value["operation_id"], "aop_controller_9");
        assert_eq!(value["realization_generation"], "document_4");
        assert!(
            serde_json::to_string(&event)
                .expect("event should serialize")
                .contains("slow_increment")
        );
        assert!(
            !serde_json::to_string(&event)
                .expect("event should serialize")
                .contains("aop_controller_9"),
            "operational correlation must not enter Browser semantic payload"
        );
    }

    #[test]
    fn ack_demultiplexer_assigns_settlement_order_before_waiters_wake() {
        let (first_tx, first_rx) = mpsc::channel();
        let (second_tx, second_rx) = mpsc::channel();
        let mut pending = BTreeMap::from([
            (
                "request-first".to_owned(),
                PendingRequest {
                    kind: PendingKind::Apply,
                    sequence: 1,
                    ordered: true,
                    deadline: Some(Instant::now() + Duration::from_secs(1)),
                    result: PendingResult::Apply(first_tx),
                },
            ),
            (
                "request-second".to_owned(),
                PendingRequest {
                    kind: PendingKind::Apply,
                    sequence: 2,
                    ordered: true,
                    deadline: Some(Instant::now() + Duration::from_secs(1)),
                    result: PendingResult::Apply(second_tx),
                },
            ),
        ]);
        let mut next = 0;
        let mut history = AcknowledgementHistory::default();
        complete_pending(
            &mut pending,
            "request-second",
            PendingKind::Apply,
            Ok(()),
            2,
            &mut next,
            &mut history,
        )
        .expect("second request ACK");
        complete_pending(
            &mut pending,
            "request-first",
            PendingKind::Apply,
            Ok(()),
            1,
            &mut next,
            &mut history,
        )
        .expect("first request ACK");

        assert_eq!(second_rx.recv().expect("second waiter").unwrap(), Some(1));
        assert_eq!(first_rx.recv().expect("first waiter").unwrap(), Some(2));
        assert!(pending.is_empty());
    }

    #[test]
    fn duplicate_ack_is_tolerated_without_publishing_a_second_ticket() {
        let (sender, receiver) = mpsc::channel();
        let mut pending = BTreeMap::from([(
            "request-once".to_owned(),
            PendingRequest {
                kind: PendingKind::Apply,
                sequence: 1,
                ordered: true,
                deadline: Some(Instant::now() + Duration::from_secs(1)),
                result: PendingResult::Apply(sender),
            },
        )]);
        let mut next = 0;
        let mut history = AcknowledgementHistory::default();

        for _ in 0..2 {
            complete_pending(
                &mut pending,
                "request-once",
                PendingKind::Apply,
                Ok(()),
                1,
                &mut next,
                &mut history,
            )
            .expect("a repeated ACK for the same terminal request is idempotent");
        }

        assert_eq!(receiver.recv().expect("first settlement").unwrap(), Some(1));
        assert_eq!(next, 1, "duplicate ACK must not advance Runner order");
        assert!(
            receiver.try_recv().is_err(),
            "no second result is published"
        );
    }

    #[test]
    fn late_ack_for_timed_out_unordered_apply_is_tolerated_without_reopening_it() {
        let (sender, receiver) = mpsc::channel();
        let now = Instant::now();
        let mut pending = BTreeMap::from([(
            "request-expired".to_owned(),
            PendingRequest {
                kind: PendingKind::Apply,
                sequence: 1,
                ordered: false,
                deadline: Some(now),
                result: PendingResult::Apply(sender),
            },
        )]);
        let mut next = 0;
        let mut history = AcknowledgementHistory::default();

        expire_pending_deadlines(&mut pending, &mut history, now)
            .expect("unordered timeout is a local rejection");
        complete_pending(
            &mut pending,
            "request-expired",
            PendingKind::Apply,
            Ok(()),
            1,
            &mut next,
            &mut history,
        )
        .expect("late ACK for a known terminal request is ignored");

        let error = receiver
            .recv()
            .expect("timeout result")
            .expect_err("late ACK cannot change a timeout into success");
        assert!(error.to_string().contains("timed out"));
        assert_eq!(next, 0);
    }

    #[test]
    fn acknowledgement_for_never_issued_request_fails_closed() {
        let mut pending = BTreeMap::new();
        let mut next = 0;
        let mut history = AcknowledgementHistory::default();

        let error = complete_pending(
            &mut pending,
            "request-forged",
            PendingKind::Apply,
            Ok(()),
            1,
            &mut next,
            &mut history,
        )
        .expect_err("unknown ACK is not a duplicate");

        assert!(error.to_string().contains("no pending request"));
        assert_eq!(next, 0);
    }

    #[test]
    fn future_or_replayed_ack_sequence_fails_closed() {
        for invalid_sequence in [1, 3] {
            let (sender, _receiver) = mpsc::channel();
            let mut pending = BTreeMap::from([(
                "request-sequenced".to_owned(),
                PendingRequest {
                    kind: PendingKind::Apply,
                    sequence: 2,
                    ordered: true,
                    deadline: Some(Instant::now() + Duration::from_secs(1)),
                    result: PendingResult::Apply(sender),
                },
            )]);
            let mut next = 0;
            let mut history = AcknowledgementHistory::default();

            let error = complete_pending(
                &mut pending,
                "request-sequenced",
                PendingKind::Apply,
                Ok(()),
                invalid_sequence,
                &mut next,
                &mut history,
            )
            .expect_err("ACK sequence must match the issued command exactly");

            assert!(error.to_string().contains("sequence mismatch"));
            assert_eq!(next, 0);
            assert!(pending.contains_key("request-sequenced"));
        }
    }

    #[test]
    fn expired_ordered_ack_fences_every_waiter_before_any_ticket_is_published() {
        let (first_tx, first_rx) = mpsc::channel();
        let (second_tx, second_rx) = mpsc::channel();
        let now = Instant::now();
        let mut pending = BTreeMap::from([
            (
                "request-expired".to_owned(),
                PendingRequest {
                    kind: PendingKind::Apply,
                    sequence: 1,
                    ordered: true,
                    deadline: Some(now),
                    result: PendingResult::Apply(first_tx),
                },
            ),
            (
                "request-later".to_owned(),
                PendingRequest {
                    kind: PendingKind::Apply,
                    sequence: 2,
                    ordered: true,
                    deadline: Some(now + Duration::from_secs(1)),
                    result: PendingResult::Apply(second_tx),
                },
            ),
        ]);
        let mut next = 0;
        let mut history = AcknowledgementHistory::default();

        let timeout = complete_pending(
            &mut pending,
            "request-expired",
            PendingKind::Apply,
            Ok(()),
            1,
            &mut next,
            &mut history,
        )
        .expect_err("the transport deadline wins over a boundary ACK");
        terminate_after_error(&mut pending, timeout)
            .expect_err("one expired ordered operation fences the incarnation");

        for receiver in [first_rx, second_rx] {
            let error = receiver
                .recv()
                .expect("every ordered waiter receives the transport fence")
                .expect_err("neither the expired nor later ACK can publish a ticket");
            assert!(error.to_string().contains("physical_outcome_indeterminate"));
        }
        assert_eq!(next, 0);
    }

    #[test]
    fn ordered_disconnect_is_indeterminate_and_never_assigns_a_ticket() {
        let (sender, receiver) = mpsc::channel();
        let mut pending = BTreeMap::from([(
            "request-ordered".to_owned(),
            PendingRequest {
                kind: PendingKind::Apply,
                sequence: 1,
                ordered: true,
                deadline: Some(Instant::now() + Duration::from_secs(1)),
                result: PendingResult::Apply(sender),
            },
        )]);
        assert!(fail_pending_after_disconnect(&mut pending));
        assert!(pending.is_empty());
        let error = receiver
            .recv()
            .expect("ordered waiter must receive terminal transport evidence")
            .expect_err("lost ACK is indeterminate, never a rejection");
        assert!(error.to_string().contains("physical_outcome_indeterminate"));
    }

    #[test]
    fn ordered_apply_send_failure_is_indeterminate_after_pre_send_registration() {
        let (sender, receiver) = mpsc::channel();
        let mut pending = BTreeMap::from([(
            "request-ordered".to_owned(),
            PendingRequest {
                kind: PendingKind::Apply,
                sequence: 1,
                ordered: true,
                deadline: Some(Instant::now() + Duration::from_secs(1)),
                result: PendingResult::Apply(sender),
            },
        )]);

        let terminal = terminate_after_error(
            &mut pending,
            AdapterError::Operation("socket write failed".to_owned()),
        )
        .expect_err("a write failure after an Apply may have reached the page");
        assert!(
            terminal
                .to_string()
                .contains("physical_outcome_indeterminate")
        );
        assert!(pending.is_empty());
        let waiter = receiver
            .recv()
            .expect("ordered waiter receives explicit terminal transport evidence")
            .expect_err("write failure is not a proven physical rejection");
        assert!(
            waiter
                .to_string()
                .contains("physical_outcome_indeterminate")
        );
    }

    #[test]
    fn acknowledgement_kind_mismatch_preserves_ordered_pending_for_transport_fence() {
        let (sender, receiver) = mpsc::channel();
        let mut pending = BTreeMap::from([(
            "request-ordered".to_owned(),
            PendingRequest {
                kind: PendingKind::Apply,
                sequence: 1,
                ordered: true,
                deadline: Some(Instant::now() + Duration::from_secs(1)),
                result: PendingResult::Apply(sender),
            },
        )]);
        let mut next = 0;
        let mut history = AcknowledgementHistory::default();

        let protocol_error = complete_pending(
            &mut pending,
            "request-ordered",
            PendingKind::Quiesce,
            Ok(()),
            1,
            &mut next,
            &mut history,
        )
        .expect_err("mismatched acknowledgement is a lifecycle violation");
        terminate_after_error(&mut pending, protocol_error)
            .expect_err("ordered lifecycle violation terminates the transport");
        let error = receiver
            .recv()
            .expect("pending ordered waiter must be fenced")
            .expect_err("no ticket may be assigned after a protocol violation");
        assert!(error.to_string().contains("physical_outcome_indeterminate"));
        assert_eq!(next, 0);
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
                "http://127.0.0.1:3000",
                None,
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
            assert!(validate_hello(&config, values.0, values.1, values.2, values.3, None).is_err());
        }
    }

    #[test]
    fn handshake_rejects_cross_activity_cross_run_epoch_and_expired_scope() {
        let scoped = scoped_config();
        let expected = scoped.channel_scope.as_ref().expect("test scope");
        assert!(
            validate_hello(
                &scoped,
                BROWSER_PROTOCOL_ID,
                "credential",
                "session",
                "http://127.0.0.1:3000",
                Some(expected),
            )
            .is_ok()
        );

        for scope in [
            BrowserChannelScope {
                activity_id: "activity-other".to_owned(),
                ..expected.clone()
            },
            BrowserChannelScope {
                run_id: "run-other".to_owned(),
                ..expected.clone()
            },
            BrowserChannelScope {
                epoch: "lease-other".to_owned(),
                ..expected.clone()
            },
        ] {
            assert!(
                validate_hello(
                    &scoped,
                    BROWSER_PROTOCOL_ID,
                    "credential",
                    "session",
                    "http://127.0.0.1:3000",
                    Some(&scope),
                )
                .is_err()
            );
        }

        let expired = TransportConfig {
            channel_scope: Some(BrowserChannelScope {
                expires_at_unix_seconds: 0,
                ..expected.clone()
            }),
            ..config()
        };
        let error = validate_hello(
            &expired,
            BROWSER_PROTOCOL_ID,
            "credential",
            "session",
            "http://127.0.0.1:3000",
            expired.channel_scope.as_ref(),
        )
        .expect_err("expired scope must not authenticate");
        assert!(error.to_string().contains("expired"));
    }
}
