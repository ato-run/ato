//! Outbound relay protocol (G4): the origin side.
//!
//! The home Coordinator never accepts an inbound connection. It polls a relay
//! it dialed out to, receives opaque envelopes, and decides every request
//! itself. This module defines that envelope and the origin's checks; the
//! HTTPS long-poll transport and the hosted relay endpoint are follow-up
//! slices, so nothing here claims a production remote path yet.
//!
//! Invariants this slice enforces:
//!
//! - the relay is opaque: it carries bytes and must not persist or log the
//!   authorization value or the body;
//! - the origin is the only authority: every envelope is re-authorized here,
//!   not by the relay;
//! - fencing: an envelope for another `origin_epoch` is refused, so a replaced
//!   authority cannot be spoken to.

use std::sync::Mutex;

use hyper::Method;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::Coordinator;
use crate::http::{ApiState, handle_request};

pub const RELAY_ENVELOPE_SCHEMA: &str = "ato.relay-envelope/1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayRequest {
    pub schema: String,
    pub request_id: String,
    /// The authority generation this request was addressed to.
    pub origin_epoch: i64,
    pub method: String,
    pub path: String,
    /// Opaque to the relay; interpreted only by the origin.
    pub body: String,
    /// Opaque to the relay; interpreted only by the origin.
    pub authorization: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayResponse {
    pub schema: String,
    pub request_id: String,
    pub status: u16,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayError {
    /// The relay could not be reached; the caller must fail closed.
    Unavailable,
    /// The envelope is malformed or for another authority.
    Refused(&'static str),
}

impl std::fmt::Display for RelayError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelayError::Unavailable => write!(formatter, "relay_unavailable"),
            RelayError::Refused(code) => write!(formatter, "{code}"),
        }
    }
}

/// The origin side: validates the envelope and dispatches it through the same
/// HTTP surface a local client uses.
pub struct Origin {
    pub epoch: i64,
    state: ApiState,
}

impl Origin {
    pub fn new(coordinator: Coordinator, epoch: i64, token: String) -> Self {
        let (shutdown, _receiver) = watch::channel(false);
        Self {
            epoch,
            state: ApiState {
                coordinator: Mutex::new(coordinator),
                token,
                shutdown,
            },
        }
    }

    pub async fn handle(&self, request: RelayRequest) -> RelayResponse {
        let refuse = |status: u16, code: &str| RelayResponse {
            schema: RELAY_ENVELOPE_SCHEMA.to_owned(),
            request_id: request.request_id.clone(),
            status,
            body: serde_json::json!({ "error": code }).to_string(),
        };
        if request.schema != RELAY_ENVELOPE_SCHEMA {
            return refuse(400, "invalid_envelope");
        }
        if request.origin_epoch != self.epoch {
            return refuse(409, "origin_epoch_mismatch");
        }
        let method = match request.method.as_str() {
            "GET" => Method::GET,
            "POST" => Method::POST,
            "DELETE" => Method::DELETE,
            _ => return refuse(400, "invalid_method"),
        };
        let response = handle_request(
            &self.state,
            &method,
            &request.path,
            request.authorization.as_deref(),
            request.body.as_bytes(),
        )
        .await;
        let status = response.status().as_u16();
        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .map(|collected| String::from_utf8_lossy(&collected.to_bytes()).into_owned())
            .unwrap_or_default();
        RelayResponse {
            schema: RELAY_ENVELOPE_SCHEMA.to_owned(),
            request_id: request.request_id,
            status,
            body,
        }
    }
}

/// Minimal newline-delimited transport for a locally reachable relay.
///
/// The wire format is one JSON object per line: `poll` asks for the next
/// envelope (`null` when idle) and `reply <json>` returns the origin's
/// decision. Production uses an HTTPS long-poll; TLS is not implemented here,
/// so this transport is for a local relay only (development and tests).
pub struct TcpRelayTransport {
    stream: std::net::TcpStream,
    reader: std::io::BufReader<std::net::TcpStream>,
}

impl TcpRelayTransport {
    pub fn connect(addr: &str) -> Result<Self, RelayError> {
        let stream = std::net::TcpStream::connect(addr).map_err(|_| RelayError::Unavailable)?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .ok();
        stream
            .set_write_timeout(Some(std::time::Duration::from_secs(5)))
            .ok();
        let reader =
            std::io::BufReader::new(stream.try_clone().map_err(|_| RelayError::Unavailable)?);
        Ok(Self { stream, reader })
    }

    fn send(&mut self, line: &str) -> Result<(), RelayError> {
        use std::io::Write;
        writeln!(self.stream, "{line}").map_err(|_| RelayError::Unavailable)
    }

    fn receive(&mut self) -> Result<String, RelayError> {
        use std::io::BufRead;
        let mut line = String::new();
        self.reader
            .read_line(&mut line)
            .map_err(|_| RelayError::Unavailable)?;
        Ok(line.trim_end().to_owned())
    }
}

impl RelayTransport for TcpRelayTransport {
    fn poll(&mut self) -> Result<Option<RelayRequest>, RelayError> {
        self.send("poll")?;
        let line = self.receive()?;
        if line == "null" {
            return Ok(None);
        }
        serde_json::from_str(&line)
            .map(Some)
            .map_err(|_| RelayError::Refused("invalid_envelope"))
    }

    fn reply(&mut self, response: RelayResponse) -> Result<(), RelayError> {
        let payload = serde_json::to_string(&response)
            .map_err(|_| RelayError::Refused("invalid_envelope"))?;
        self.send(&format!("reply {payload}"))?;
        if self.receive()? != "ok" {
            return Err(RelayError::Unavailable);
        }
        Ok(())
    }
}

/// The relay connection. The origin only ever calls these two methods; the
/// transport is responsible for keeping the payload opaque (no logging, no
/// persistence).
pub trait RelayTransport {
    /// Fetch the next request, or `None` when the relay has nothing pending.
    fn poll(&mut self) -> Result<Option<RelayRequest>, RelayError>;
    /// Send the origin's decision back through the relay.
    fn reply(&mut self, response: RelayResponse) -> Result<(), RelayError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayStep {
    /// Nothing was pending.
    Idle,
    /// A request was answered; `status` is the origin's decision.
    Answered { request_id: String, status: u16 },
    /// The relay could not be reached. Nothing was executed.
    Unavailable,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RelayLoopReport {
    pub answered: u64,
    pub consecutive_failures: u32,
}

/// One step of the outbound loop. A caller runs this repeatedly; the loop
/// never executes a request the origin has not authorized, and never pretends
/// an offline relay succeeded.
#[derive(Debug, Default)]
pub struct RelayLoop {
    report: RelayLoopReport,
}

impl RelayLoop {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn report(&self) -> &RelayLoopReport {
        &self.report
    }

    pub async fn step(&mut self, origin: &Origin, transport: &mut dyn RelayTransport) -> RelayStep {
        let request = match transport.poll() {
            Ok(Some(request)) => request,
            Ok(None) => {
                self.report.consecutive_failures = 0;
                return RelayStep::Idle;
            }
            Err(_) => {
                self.report.consecutive_failures += 1;
                return RelayStep::Unavailable;
            }
        };
        let request_id = request.request_id.clone();
        let response = origin.handle(request).await;
        let status = response.status;
        match transport.reply(response) {
            Ok(()) => {
                self.report.answered += 1;
                self.report.consecutive_failures = 0;
                RelayStep::Answered { request_id, status }
            }
            Err(_) => {
                self.report.consecutive_failures += 1;
                RelayStep::Unavailable
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> Origin {
        Origin::new(
            Coordinator::open_in_memory().unwrap(),
            7,
            "test-token".into(),
        )
    }

    fn request(method: &str, path: &str, body: &str) -> RelayRequest {
        RelayRequest {
            schema: RELAY_ENVELOPE_SCHEMA.to_owned(),
            request_id: "req-1".into(),
            origin_epoch: 7,
            method: method.into(),
            path: path.into(),
            body: body.into(),
            authorization: Some("Bearer test-token".into()),
        }
    }

    #[derive(Default)]
    struct MemoryRelay {
        queue: Vec<RelayRequest>,
        replies: Vec<RelayResponse>,
        fail_poll: bool,
        fail_reply: bool,
    }

    impl RelayTransport for MemoryRelay {
        fn poll(&mut self) -> Result<Option<RelayRequest>, RelayError> {
            if self.fail_poll {
                return Err(RelayError::Unavailable);
            }
            Ok(if self.queue.is_empty() {
                None
            } else {
                Some(self.queue.remove(0))
            })
        }

        fn reply(&mut self, response: RelayResponse) -> Result<(), RelayError> {
            if self.fail_reply {
                return Err(RelayError::Unavailable);
            }
            self.replies.push(response);
            Ok(())
        }
    }

    #[tokio::test]
    async fn the_loop_answers_with_the_origin_decision() {
        let origin = origin();
        let mut stale = request("GET", "/health", "");
        stale.request_id = "req-2".into();
        stale.origin_epoch = 6;
        let mut relay = MemoryRelay {
            queue: vec![request("GET", "/health", ""), stale],
            ..MemoryRelay::default()
        };
        let mut loop_ = RelayLoop::new();
        assert_eq!(
            loop_.step(&origin, &mut relay).await,
            RelayStep::Answered {
                request_id: "req-1".into(),
                status: 200
            }
        );
        assert_eq!(
            loop_.step(&origin, &mut relay).await,
            RelayStep::Answered {
                request_id: "req-2".into(),
                status: 409
            }
        );
        assert_eq!(relay.replies.len(), 2);
        assert_eq!(loop_.report().answered, 2);
        assert_eq!(loop_.report().consecutive_failures, 0);
    }

    #[tokio::test]
    async fn an_unreachable_relay_fails_closed_and_recovers() {
        let origin = origin();
        let mut offline = MemoryRelay {
            fail_poll: true,
            ..MemoryRelay::default()
        };
        let mut loop_ = RelayLoop::new();
        assert_eq!(
            loop_.step(&origin, &mut offline).await,
            RelayStep::Unavailable
        );
        assert_eq!(loop_.report().consecutive_failures, 1);
        assert_eq!(loop_.report().answered, 0);
        assert!(offline.replies.is_empty());

        let mut relay = MemoryRelay {
            queue: vec![request("GET", "/v1/data-resources", "")],
            ..MemoryRelay::default()
        };
        assert_eq!(
            loop_.step(&origin, &mut relay).await,
            RelayStep::Answered {
                request_id: "req-1".into(),
                status: 200
            }
        );
        assert_eq!(loop_.report().consecutive_failures, 0);
        assert_eq!(loop_.report().answered, 1);
    }

    #[test]
    fn answers_over_a_local_relay_socket() {
        use std::io::{BufRead, BufReader, Write};
        use std::sync::{Arc, Mutex};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let recorded: Arc<Mutex<Vec<RelayResponse>>> = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&recorded);
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            let mut writer = socket.try_clone().unwrap();
            let mut reader = BufReader::new(socket);
            let mut served: u32 = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break;
                }
                let line = line.trim_end();
                if line == "poll" {
                    if served == 0 {
                        served += 1;
                        let envelope = serde_json::json!({
                            "schema": RELAY_ENVELOPE_SCHEMA,
                            "request_id": "socket-1",
                            "origin_epoch": 7,
                            "method": "POST",
                            "path": "/v1/data-resources",
                            "body": r#"{"protocol":"todo.board@1","document":{}}"#,
                            "authorization": "Bearer test-token"
                        });
                        writeln!(writer, "{envelope}").unwrap();
                    } else {
                        writeln!(writer, "null").unwrap();
                    }
                } else if let Some(payload) = line.strip_prefix("reply ") {
                    let response: RelayResponse = serde_json::from_str(payload).unwrap();
                    record.lock().unwrap().push(response);
                    writeln!(writer, "ok").unwrap();
                }
            }
        });

        let origin = origin();
        let mut transport = TcpRelayTransport::connect(&addr).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut loop_ = RelayLoop::new();
        let answered = runtime.block_on(loop_.step(&origin, &mut transport));
        assert_eq!(
            answered,
            RelayStep::Answered {
                request_id: "socket-1".into(),
                status: 201
            }
        );
        assert_eq!(
            runtime.block_on(loop_.step(&origin, &mut transport)),
            RelayStep::Idle
        );
        drop(transport);
        server.join().unwrap();
        let replies = recorded.lock().unwrap();
        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].status, 201);
        assert!(
            replies[0].body.contains("resource_id"),
            "{}",
            replies[0].body
        );
    }

    #[tokio::test]
    async fn refuses_an_envelope_for_another_authority() {
        let mut request = request("GET", "/v1/data-resources", "");
        request.origin_epoch = 6;
        let response = origin().handle(request).await;
        assert_eq!(response.status, 409);
        assert!(response.body.contains("origin_epoch_mismatch"));
    }

    #[tokio::test]
    async fn refuses_a_malformed_envelope() {
        let mut envelope = request("GET", "/health", "");
        envelope.schema = "ato.relay-envelope/2".into();
        assert_eq!(origin().handle(envelope).await.status, 400);
        let mut envelope = request("PUT", "/health", "");
        envelope.method = "PUT".into();
        assert_eq!(origin().handle(envelope).await.status, 400);
    }

    #[tokio::test]
    async fn the_relay_cannot_bypass_origin_authorization() {
        let mut envelope = request("GET", "/v1/data-resources", "");
        envelope.authorization = None;
        assert_eq!(origin().handle(envelope).await.status, 401);
        let mut envelope = request("GET", "/v1/data-resources", "");
        envelope.authorization = Some("Bearer wrong".into());
        assert_eq!(origin().handle(envelope).await.status, 401);
    }

    #[tokio::test]
    async fn dispatches_an_authorized_request_through_the_local_surface() {
        let origin = origin();
        let response = origin.handle(request("GET", "/health", "")).await;
        assert_eq!(response.status, 200);
        assert!(
            response.body.contains("\"status\":\"ok\""),
            "{}",
            response.body
        );

        let created = origin
            .handle(request(
                "POST",
                "/v1/data-resources",
                r#"{"protocol":"todo.board@1","document":{"tasks":{}}}"#,
            ))
            .await;
        assert_eq!(created.status, 201);
        assert!(created.body.contains("resource_id"), "{}", created.body);
    }
}
