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
//! - the relay is data-only: an envelope must present the relay data
//!   capability, never the Coordinator admin bearer, and it can reach the
//!   data routes alone;
//! - a replayed `request_id` is answered from its recorded receipt instead of
//!   being applied a second time;
//! - fencing: an envelope for another `origin_epoch` is refused, so a replaced
//!   authority cannot be spoken to.

use std::sync::Mutex;

use hyper::Method;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::http::{ApiState, handle_relay_data_request};
use crate::{Coordinator, RelayReceipt};

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
    /// Requested payload mode. This build executes "none" only; "e2ee" and
    /// "recoverable" are refused rather than silently downgraded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption: Option<String>,
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
///
/// The origin holds no admin bearer. An envelope is admitted only when it
/// presents the relay-only data capability, and it then reaches the data
/// routes alone.
pub struct Origin {
    pub epoch: i64,
    state: ApiState,
    /// SHA-256 of the relay data capability; the secret itself is not kept.
    data_capability: [u8; 32],
}

impl Origin {
    pub fn new(coordinator: Coordinator, epoch: i64, data_token: String) -> Self {
        let (shutdown, _receiver) = watch::channel(false);
        Self {
            epoch,
            data_capability: digest(&data_token),
            state: ApiState {
                coordinator: Mutex::new(coordinator),
                // The relay never accepts the admin bearer, so this state
                // carries no usable admin token.
                token: String::new(),
                shutdown,
                apps: None,
                workspace: Vec::new(),
            },
        }
    }

    /// Whether the envelope presented the relay data capability. A missing or
    /// different bearer - including the admin API bearer - is not enough.
    fn presents_data_capability(&self, authorization: Option<&str>) -> bool {
        let Some(presented) = authorization.and_then(|value| value.strip_prefix("Bearer ")) else {
            return false;
        };
        constant_time_eq(&digest(presented), &self.data_capability)
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
        if request.request_id.is_empty() || request.request_id.len() > 128 {
            return refuse(400, "invalid_request_id");
        }
        // The capability is verified before anything is dispatched. The admin
        // bearer is not a substitute and is never compared here.
        if !self.presents_data_capability(request.authorization.as_deref()) {
            return refuse(401, "relay_data_capability_invalid");
        }
        if request.origin_epoch != self.epoch {
            return refuse(409, "origin_epoch_mismatch");
        }
        match request.encryption.as_deref().unwrap_or("none") {
            "none" => {}
            "e2ee" | "recoverable" => return refuse(422, "encryption_mode_unsupported"),
            _ => return refuse(400, "invalid_encryption_mode"),
        }
        let method = match request.method.as_str() {
            "GET" => Method::GET,
            "POST" => Method::POST,
            "DELETE" => Method::DELETE,
            _ => return refuse(400, "invalid_method"),
        };
        let request_digest = request_digest(&request);
        match self.receipt(&request.request_id) {
            Ok(Some(receipt)) => {
                if receipt.request_digest != request_digest {
                    return refuse(409, "relay_request_id_conflict");
                }
                return RelayResponse {
                    schema: RELAY_ENVELOPE_SCHEMA.to_owned(),
                    request_id: request.request_id,
                    status: receipt.status,
                    body: receipt.body,
                };
            }
            Ok(None) => {}
            Err(()) => return refuse(503, "relay_receipt_unavailable"),
        }
        let response =
            handle_relay_data_request(&self.state, &method, &request.path, request.body.as_bytes())
                .await;
        let status = response.status().as_u16();
        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .map(|collected| String::from_utf8_lossy(&collected.to_bytes()).into_owned())
            .unwrap_or_default();
        if status < 500
            && self
                .record(&request.request_id, &request_digest, status, &body)
                .is_err()
        {
            // The decision executed but could not be recorded. Claiming
            // success would let a redelivery apply it a second time.
            return refuse(503, "relay_receipt_unavailable");
        }
        RelayResponse {
            schema: RELAY_ENVELOPE_SCHEMA.to_owned(),
            request_id: request.request_id,
            status,
            body,
        }
    }

    fn receipt(&self, request_id: &str) -> Result<Option<RelayReceipt>, ()> {
        let coordinator = self.state.coordinator.lock().map_err(|_| ())?;
        coordinator.relay_receipt(request_id).map_err(|_| ())
    }

    fn record(
        &self,
        request_id: &str,
        request_digest: &str,
        status: u16,
        body: &str,
    ) -> Result<(), ()> {
        let mut coordinator = self.state.coordinator.lock().map_err(|_| ())?;
        coordinator
            .record_relay_receipt(request_id, request_digest, status, body)
            .map_err(|_| ())
    }
}

/// SHA-256 of a capability. Hashing both sides first means the comparison
/// always runs over 32 bytes and the secret length never changes the work.
fn digest(value: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(value.as_bytes());
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// Compare two capability digests without an early return on the first
/// differing byte.
fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    let mut difference = 0u8;
    for (left, right) in left.iter().zip(right.iter()) {
        difference |= left ^ right;
    }
    difference == 0
}

/// The identity of a request for replay detection. The authorization value is
/// deliberately excluded: a retried envelope with a rotated capability is the
/// same request, and the capability itself is never stored.
fn request_digest(request: &RelayRequest) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"ato.relay-request/1");
    hasher.update([0]);
    hasher.update(request.origin_epoch.to_le_bytes());
    hasher.update([0]);
    hasher.update(request.method.as_bytes());
    hasher.update([0]);
    hasher.update(request.path.as_bytes());
    hasher.update([0]);
    hasher.update(request.body.as_bytes());
    hex::encode(hasher.finalize())
}

/// Minimal newline-delimited transport for a locally reachable relay.
///
/// The wire format is one JSON object per line: `poll` asks for the next
/// envelope (`null` when idle) and `reply <json>` returns the origin's
/// decision. Production uses an HTTPS long-poll; TLS is not implemented here,
/// so this transport is for a local relay only (development and tests).
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

/// Delay before retrying after this many consecutive relay failures: 1s, 2s,
/// 4s, ... capped at 30s. A caller that never reaches the relay therefore backs
/// off instead of hammering it.
pub fn backoff_delay(consecutive_failures: u32) -> std::time::Duration {
    let exponent = consecutive_failures.saturating_sub(1).min(5);
    let seconds = (1u64 << exponent).min(30);
    std::time::Duration::from_secs(seconds)
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

/// HTTP transport for a hosted relay (`ato-api`'s /v1/relay endpoints).
///
/// Plain HTTP only: it is for a relay reachable over a local/trusted network and
/// for tests. Production HTTPS and E2EE negotiation are follow-up slices.
pub struct HttpRelayTransport {
    base: String,
    origin_id: String,
    token: String,
}

impl HttpRelayTransport {
    pub fn new(base: &str, origin_id: &str, token: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_owned(),
            origin_id: origin_id.to_owned(),
            token: token.to_owned(),
        }
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<(u16, String), RelayError> {
        use std::io::{BufRead, BufReader, Read, Write};
        // A hosted relay in production is https. The raw TcpStream path below
        // stays for the local/http relay (and its framing tests); https goes
        // through the same HTTP client the CLI already uses.
        if self.base.starts_with("https://") {
            return self.request_https(method, path, body);
        }
        let address = self
            .base
            .strip_prefix("http://")
            .ok_or(RelayError::Unavailable)?;
        let (host, port) = match address.split_once(':') {
            Some((host, port)) => (host.to_owned(), port.parse::<u16>().unwrap_or(80)),
            None => (address.to_owned(), 80),
        };
        let mut stream = std::net::TcpStream::connect((host.as_str(), port))
            .map_err(|_| RelayError::Unavailable)?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .ok();
        let payload = body.unwrap_or("");
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            self.token,
            payload.len()
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|_| RelayError::Unavailable)?;
        // Read the response ourselves instead of `read_to_string`: a keep-alive
        // connection (or a 204 with no body) never reaches EOF, so waiting for
        // one silently turns every poll into a read timeout and the relay loop
        // stalls. Parse the status line and the framing headers, then read
        // exactly the body the server framed.
        let mut reader = BufReader::new(stream.try_clone().map_err(|_| RelayError::Unavailable)?);
        let mut status_line = String::new();
        reader
            .read_line(&mut status_line)
            .map_err(|_| RelayError::Unavailable)?;
        let status = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or(RelayError::Unavailable)?;
        let mut content_length: Option<usize> = None;
        let mut chunked = false;
        loop {
            let mut line = String::new();
            let read = reader
                .read_line(&mut line)
                .map_err(|_| RelayError::Unavailable)?;
            if read == 0 {
                break;
            }
            let header = line.trim_end_matches(['\r', '\n']);
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':') {
                let name = name.trim().to_ascii_lowercase();
                let value = value.trim();
                if name == "content-length" {
                    content_length = value.parse::<usize>().ok();
                } else if name == "transfer-encoding"
                    && value.to_ascii_lowercase().contains("chunked")
                {
                    chunked = true;
                }
            }
        }
        if status == 204 || status == 304 {
            return Ok((status, String::new()));
        }
        let mut body_bytes: Vec<u8> = Vec::new();
        if chunked {
            loop {
                let mut size_line = String::new();
                reader
                    .read_line(&mut size_line)
                    .map_err(|_| RelayError::Unavailable)?;
                let size_text = size_line.trim();
                let size =
                    usize::from_str_radix(size_text.split(';').next().unwrap_or("0").trim(), 16)
                        .map_err(|_| RelayError::Unavailable)?;
                if size == 0 {
                    loop {
                        let mut trailer = String::new();
                        let read = reader
                            .read_line(&mut trailer)
                            .map_err(|_| RelayError::Unavailable)?;
                        if read == 0 || trailer.trim().is_empty() {
                            break;
                        }
                    }
                    break;
                }
                let mut chunk = vec![0u8; size];
                reader
                    .read_exact(&mut chunk)
                    .map_err(|_| RelayError::Unavailable)?;
                body_bytes.extend_from_slice(&chunk);
                let mut crlf = [0u8; 2];
                reader
                    .read_exact(&mut crlf)
                    .map_err(|_| RelayError::Unavailable)?;
            }
        } else if let Some(length) = content_length {
            let mut buf = vec![0u8; length];
            reader
                .read_exact(&mut buf)
                .map_err(|_| RelayError::Unavailable)?;
            body_bytes = buf;
        } else {
            reader
                .read_to_end(&mut body_bytes)
                .map_err(|_| RelayError::Unavailable)?;
        }
        Ok((status, String::from_utf8_lossy(&body_bytes).into_owned()))
    }

    /// https relay calls, through the rustls client the CLI already depends on.
    ///
    /// The blocking client owns a private runtime, so it must be built and
    /// dropped outside the relay command's tokio context (dropping one inside
    /// an async context panics). The request therefore runs on a scoped helper
    /// thread; the caller only waits for its result. Same contract as the http
    /// path: origin token in Authorization, opaque envelope as the body, and
    /// every transport problem is Unavailable so the loop backs off.
    fn request_https(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<(u16, String), RelayError> {
        use std::time::Duration;
        let url = format!("{}{}", self.base, path);
        let token = self.token.clone();
        let method = method.to_owned();
        let payload = body.map(|value| value.to_owned());
        std::thread::scope(|scope| {
            scope
                .spawn(move || -> Result<(u16, String), RelayError> {
                    let parsed = reqwest::Method::from_bytes(method.as_bytes())
                        .map_err(|_| RelayError::Unavailable)?;
                    let client = reqwest::blocking::Client::builder()
                        .timeout(Duration::from_secs(10))
                        .build()
                        .map_err(|_| RelayError::Unavailable)?;
                    let mut request = client
                        .request(parsed, &url)
                        .header("authorization", format!("Bearer {token}"))
                        .header("content-type", "application/json");
                    if let Some(body) = payload {
                        request = request.body(body);
                    }
                    let response = request.send().map_err(|_| RelayError::Unavailable)?;
                    let status = response.status().as_u16();
                    let text = response.text().map_err(|_| RelayError::Unavailable)?;
                    Ok((status, text))
                })
                .join()
                .map_err(|_| RelayError::Unavailable)?
        })
    }
}

impl RelayTransport for HttpRelayTransport {
    fn poll(&mut self) -> Result<Option<RelayRequest>, RelayError> {
        let path = format!("/v1/relay/origins/{}/next", self.origin_id);
        let (status, body) = self.request("GET", &path, None)?;
        if status == 204 {
            return Ok(None);
        }
        if status != 200 {
            return Err(RelayError::Unavailable);
        }
        let value: serde_json::Value =
            serde_json::from_str(&body).map_err(|_| RelayError::Refused("invalid_envelope"))?;
        let envelope = value
            .get("envelope")
            .and_then(serde_json::Value::as_str)
            .ok_or(RelayError::Refused("invalid_envelope"))?;
        serde_json::from_str(envelope)
            .map(Some)
            .map_err(|_| RelayError::Refused("invalid_envelope"))
    }

    fn reply(&mut self, response: RelayResponse) -> Result<(), RelayError> {
        let reply = serde_json::to_string(&response)
            .map_err(|_| RelayError::Refused("invalid_envelope"))?;
        let body = serde_json::json!({
            "request_id": response.request_id,
            "reply": reply,
        });
        let path = format!("/v1/relay/origins/{}/replies", self.origin_id);
        let (status, _) = self.request("POST", &path, Some(&body.to_string()))?;
        if status != 200 {
            return Err(RelayError::Unavailable);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin() -> Origin {
        Origin::new(
            Coordinator::open_in_memory().unwrap(),
            7,
            "data-token".into(),
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
            authorization: Some("Bearer data-token".into()),
            encryption: None,
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
        let mut stale = request("GET", "/v1/data-resources", "");
        stale.request_id = "req-2".into();
        stale.origin_epoch = 6;
        let mut relay = MemoryRelay {
            queue: vec![request("GET", "/v1/data-resources", ""), stale],
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
                            "authorization": "Bearer data-token"
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
        let origin = origin();
        let mut envelope = request("GET", "/v1/data-resources", "");
        envelope.schema = "ato.relay-envelope/2".into();
        assert_eq!(origin.handle(envelope).await.status, 400);
        let mut envelope = request("GET", "/v1/data-resources", "");
        envelope.request_id.clear();
        assert_eq!(origin.handle(envelope).await.status, 400);
        let mut envelope = request("PUT", "/v1/data-resources", "");
        envelope.method = "PUT".into();
        assert_eq!(origin.handle(envelope).await.status, 400);
    }

    #[tokio::test]
    async fn an_envelope_must_present_the_data_capability() {
        let origin = origin();
        let mut envelope = request("GET", "/v1/data-resources", "");
        envelope.authorization = None;
        let response = origin.handle(envelope).await;
        assert_eq!(response.status, 401);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&response.body).unwrap()["error"],
            "relay_data_capability_invalid"
        );

        for bearer in [
            "Bearer wrong",
            "Bearer test-token",
            "data-token",
            "bearer data-token",
        ] {
            let mut envelope = request("GET", "/v1/data-resources", "");
            envelope.authorization = Some(bearer.into());
            let response = origin.handle(envelope).await;
            assert_eq!(response.status, 401, "{bearer}");
            assert!(
                response.body.contains("relay_data_capability_invalid"),
                "{bearer}: {}",
                response.body
            );
        }
    }

    #[tokio::test]
    async fn dispatches_a_data_request_through_the_local_surface() {
        let origin = origin();
        let created = origin
            .handle(request(
                "POST",
                "/v1/data-resources",
                r#"{"protocol":"todo.board@1","document":{"tasks":{}}}"#,
            ))
            .await;
        assert_eq!(created.status, 201);
        assert!(created.body.contains("resource_id"), "{}", created.body);
        assert!(created.body.contains("\"revision\":1"), "{}", created.body);
        assert!(created.body.contains("content_digest"), "{}", created.body);

        let body: serde_json::Value = serde_json::from_str(&created.body).unwrap();
        let resource_id = body["resource_id"].as_str().unwrap();
        let mut read_request = request("GET", &format!("/v1/data-resources/{resource_id}"), "");
        read_request.request_id = "req-read".into();
        let read = origin.handle(read_request).await;
        assert_eq!(read.status, 200);
        assert!(read.body.contains(resource_id), "{}", read.body);
    }

    #[tokio::test]
    async fn the_data_capability_cannot_reach_administrative_routes() {
        let origin = origin();
        for (index, (method, path)) in [
            ("GET", "/health"),
            ("POST", "/control/shutdown"),
            ("GET", "/v1/data-resources/export"),
            ("GET", "/v1/data-resources/quota"),
            ("GET", "/v1/data-resources/abc/export"),
            ("POST", "/mcp"),
            ("GET", "/v1/bindings"),
            ("POST", "/v1/protocols/todo.board@1/operations"),
            ("GET", "/apps/whoever"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut envelope = request(method, path, "{}");
            envelope.request_id = format!("scope-{index}");
            let response = origin.handle(envelope).await;
            assert_eq!(response.status, 403, "{method} {path}");
            assert!(
                response.body.contains("relay_capability_scope_exceeded"),
                "{method} {path}: {}",
                response.body
            );
        }
    }

    #[tokio::test]
    async fn a_replayed_request_id_is_not_applied_twice() {
        let origin = origin();
        let create = || {
            request(
                "POST",
                "/v1/data-resources",
                r#"{"protocol":"todo.board@1","document":{"tasks":{"t1":{"id":"t1"}}}}"#,
            )
        };
        let first = origin.handle(create()).await;
        assert_eq!(first.status, 201);
        let second = origin.handle(create()).await;
        assert_eq!(second, first);
        {
            let coordinator = origin.state.coordinator.lock().unwrap();
            assert_eq!(coordinator.list_resources("local").unwrap().len(), 1);
        }

        let mut conflicting = create();
        conflicting.body = r#"{"protocol":"todo.board@1","document":{"tasks":{}}}"#.into();
        let response = origin.handle(conflicting).await;
        assert_eq!(response.status, 409);
        assert!(
            response.body.contains("relay_request_id_conflict"),
            "{}",
            response.body
        );
        let coordinator = origin.state.coordinator.lock().unwrap();
        assert_eq!(coordinator.list_resources("local").unwrap().len(), 1);
    }

    #[test]
    fn answers_through_an_http_relay() {
        use std::io::{BufRead, BufReader, Write};
        use std::sync::{Arc, Mutex};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let recorded: Arc<Mutex<Vec<RelayResponse>>> = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&recorded);
        let server = std::thread::spawn(move || {
            let mut served_poll = false;
            for _ in 0..3 {
                let Ok((socket, _)) = listener.accept() else {
                    break;
                };
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut writer = socket;
                let mut request_line = String::new();
                reader.read_line(&mut request_line).unwrap();
                let mut headers = Vec::new();
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line.trim().is_empty() {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_length = value.trim().parse().unwrap_or(0);
                    }
                    headers.push(line);
                }
                let mut body = vec![0u8; content_length];
                if content_length > 0 {
                    use std::io::Read;
                    reader.read_exact(&mut body).unwrap();
                }
                let body = String::from_utf8_lossy(&body).into_owned();
                if request_line.contains("/next") {
                    if served_poll {
                        write!(
                            writer,
                            "HTTP/1.1 204 No Content
Connection: close

"
                        )
                        .unwrap();
                    } else {
                        served_poll = true;
                        let envelope = serde_json::to_string(&request(
                            "POST",
                            "/v1/data-resources",
                            r#"{"protocol":"todo.board@1","document":{}}"#,
                        ))
                        .unwrap();
                        let response_body = serde_json::json!({
                            "request_id": "http-1",
                            "origin_epoch": 7,
                            "envelope": envelope,
                        })
                        .to_string();
                        write!(
                            writer,
                            "HTTP/1.1 200 OK
Content-Length: {}
Connection: close

{}",
                            response_body.len(),
                            response_body
                        )
                        .unwrap();
                    }
                } else if request_line.contains("/replies") {
                    let reply: serde_json::Value = serde_json::from_str(&body).unwrap();
                    let response: RelayResponse =
                        serde_json::from_str(reply["reply"].as_str().unwrap()).unwrap();
                    record.lock().unwrap().push(response);
                    let response_body = r#"{"status":"recorded"}"#;
                    write!(
                        writer,
                        "HTTP/1.1 200 OK
Content-Length: {}
Connection: close

{}",
                        response_body.len(),
                        response_body
                    )
                    .unwrap();
                }
                let _ = writer.flush();
            }
        });

        let origin = origin();
        let mut transport = HttpRelayTransport::new(&base, "relay-1", "origin-token");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut loop_ = RelayLoop::new();
        assert_eq!(
            runtime.block_on(loop_.step(&origin, &mut transport)),
            RelayStep::Answered {
                request_id: "req-1".into(),
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
    async fn refuses_an_encryption_mode_it_cannot_verify() {
        let origin = origin();
        let mut e2ee = request("GET", "/v1/data-resources", "");
        e2ee.encryption = Some("e2ee".into());
        let response = origin.handle(e2ee).await;
        assert_eq!(response.status, 422);
        assert!(response.body.contains("encryption_mode_unsupported"));

        let mut recoverable = request("GET", "/v1/data-resources", "");
        recoverable.encryption = Some("recoverable".into());
        assert_eq!(origin.handle(recoverable).await.status, 422);

        let mut unknown = request("GET", "/v1/data-resources", "");
        unknown.encryption = Some("rot13".into());
        let response = origin.handle(unknown).await;
        assert_eq!(response.status, 400);
        assert!(response.body.contains("invalid_encryption_mode"));

        let mut plain = request("GET", "/v1/data-resources", "");
        plain.encryption = Some("none".into());
        assert_eq!(origin.handle(plain).await.status, 200);
    }

    #[test]
    fn backs_off_after_repeated_failures() {
        use std::time::Duration;
        assert_eq!(backoff_delay(1), Duration::from_secs(1));
        assert_eq!(backoff_delay(2), Duration::from_secs(2));
        assert_eq!(backoff_delay(3), Duration::from_secs(4));
        assert_eq!(backoff_delay(4), Duration::from_secs(8));
        assert_eq!(backoff_delay(5), Duration::from_secs(16));
        assert_eq!(backoff_delay(6), Duration::from_secs(30));
        assert_eq!(backoff_delay(100), Duration::from_secs(30));
    }
}
