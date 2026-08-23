//! Inbound HTTP requests are replayable operations. Responses are runtime output.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use ato_adapter_api::{
    AdapterAttachContext, AdapterCapabilities, AdapterContext, AdapterError, AdapterFactory,
    AdapterInstance, AdapterObservation, AttachedAdapter, ObservationEffect, ObservationSink,
    Stylus, SupportedOperation,
};
use ato_objects::{RecordCandidate, RecordEnvelope, read_exact_object};
use serde::{Deserialize, Serialize};

pub const HTTP_ADAPTER_ID: &str = "ato.http@1";
pub const HTTP_PROTOCOL_ID: &str = "ato.http@1";
pub const HTTP_REQUEST_OPERATION: &str = "request";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HttpEvent {
    Request {
        method: String,
        path: String,
        headers: BTreeMap<String, String>,
        body: Vec<u8>,
    },
    Response {
        status: u16,
        headers: BTreeMap<String, String>,
        body: Vec<u8>,
    },
}

pub fn encode_event(event: &HttpEvent) -> Result<Vec<u8>, serde_json::Error> {
    serde_jcs::to_vec(event)
}

pub fn decode_event(bytes: &[u8]) -> Result<HttpEvent, serde_json::Error> {
    let event = serde_json::from_slice(bytes)?;
    if serde_jcs::to_vec(&event)? != bytes {
        return Err(serde_json::Error::io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "HTTP event is not canonical JCS",
        )));
    }
    Ok(event)
}

#[derive(Default)]
pub struct HttpAdapter;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpAdapterConfig {
    pub listen: SocketAddr,
    pub upstream: SocketAddr,
    pub port_id: String,
    #[serde(default)]
    pub ready_path: Option<String>,
}

impl AdapterFactory for HttpAdapter {
    fn id(&self) -> &str {
        HTTP_ADAPTER_ID
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            observe: true,
            apply: true,
            verify: true,
            quiesce: true,
        }
    }

    fn supported_operations(&self) -> Vec<SupportedOperation> {
        vec![
            SupportedOperation::new(HTTP_PROTOCOL_ID, HTTP_REQUEST_OPERATION, 1, BTreeSet::new())
                .expect("valid static HTTP operation"),
        ]
    }

    fn preflight(
        &self,
        instance: &AdapterInstance,
        _context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        parse_config(instance).map(|_| ())
    }

    fn attach(
        &self,
        instance: &AdapterInstance,
        context: &AdapterAttachContext<'_>,
    ) -> Result<Box<dyn AttachedAdapter>, AdapterError> {
        let config = parse_config(instance)?;
        if let Some(path) = &config.ready_path {
            wait_until_ready(config.upstream, path)?;
        }
        let listener = TcpListener::bind(config.listen)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let failure = Arc::new(Mutex::new(None));
        let observed_responses = Arc::new(Mutex::new(VecDeque::new()));
        Ok(Box::new(HttpSession {
            instance_id: instance.instance_id.clone(),
            config,
            listener: Some(listener),
            stylus: Arc::clone(&context.stylus),
            observations: Arc::clone(&context.observations),
            stop,
            failure,
            observed_responses,
            join: None,
        }))
    }
}

fn wait_until_ready(upstream: SocketAddr, path: &str) -> Result<(), AdapterError> {
    if !path.starts_with('/') || path.contains(['\r', '\n']) {
        return Err(AdapterError::InvalidConfig(
            "HTTP ready_path must be an absolute path without control characters".to_owned(),
        ));
    }
    let mut last_error = None;
    for _ in 0..400 {
        match TcpStream::connect(upstream).and_then(|mut stream| {
            stream.write_all(
                format!("GET {path} HTTP/1.1\r\nHost: readiness\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )?;
            let response = read_http_message(&mut stream)?;
            match parse_response(&response)? {
                HttpEvent::Response { status, .. } if (200..300).contains(&status) => Ok(()),
                _ => Err(std::io::Error::other("HTTP readiness contract failed")),
            }
        }) {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                thread::sleep(Duration::from_millis(25));
            }
        }
    }
    Err(AdapterError::Operation(format!(
        "HTTP readiness contract timed out: {}",
        last_error.map_or_else(|| "unknown error".to_owned(), |error| error.to_string())
    )))
}

struct HttpSession {
    instance_id: String,
    config: HttpAdapterConfig,
    listener: Option<TcpListener>,
    stylus: Arc<dyn Stylus>,
    observations: Arc<dyn ObservationSink>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
    observed_responses: Arc<Mutex<VecDeque<HttpEvent>>>,
    join: Option<JoinHandle<()>>,
}

impl AttachedAdapter for HttpSession {
    fn instance_id(&self) -> &str {
        &self.instance_id
    }

    fn adapter_id(&self) -> &str {
        HTTP_ADAPTER_ID
    }

    fn capabilities(&self) -> AdapterCapabilities {
        AdapterFactory::capabilities(&HttpAdapter)
    }

    fn accepts(&self, record: &RecordEnvelope) -> bool {
        record.adapter_id == HTTP_ADAPTER_ID && record.port_id.as_str() == self.config.port_id
    }

    fn activate(&mut self) -> Result<(), AdapterError> {
        let Some(listener) = self.listener.take() else {
            return Ok(());
        };
        self.join = Some(spawn_proxy(
            listener,
            self.config.clone(),
            Arc::clone(&self.stylus),
            Arc::clone(&self.observations),
            Arc::clone(&self.stop),
            Arc::clone(&self.failure),
        ));
        Ok(())
    }

    fn apply(
        &mut self,
        record: &RecordEnvelope,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        match read_event(record, context)? {
            request @ HttpEvent::Request { .. } => {
                let mut stream = connect_with_retry(self.config.upstream)?;
                stream.write_all(&encode_request(&request)?)?;
                let response = parse_response(&read_http_message(&mut stream)?)?;
                self.observed_responses
                    .lock()
                    .map_err(|_| AdapterError::Operation("HTTP response queue poisoned".into()))?
                    .push_back(response);
                Ok(())
            }
            expected @ HttpEvent::Response { .. } => {
                let actual = self
                    .observed_responses
                    .lock()
                    .map_err(|_| AdapterError::Operation("HTTP response queue poisoned".into()))?
                    .pop_front()
                    .ok_or_else(|| {
                        AdapterError::Operation("HTTP replay produced no response".into())
                    })?;
                if actual == expected {
                    Ok(())
                } else {
                    Err(AdapterError::Operation(format!(
                        "HTTP replay response mismatch: expected {expected:?}, got {actual:?}"
                    )))
                }
            }
        }
    }

    fn verify(
        &mut self,
        record: &RecordEnvelope,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        read_event(record, context).map(|_| ())
    }

    fn quiesce(&mut self, _context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        let _ = TcpStream::connect(self.config.listen);
        if let Some(join) = self.join.take() {
            join.join()
                .map_err(|_| AdapterError::Operation("HTTP adapter thread panicked".into()))?;
        }
        if let Some(error) = self
            .failure
            .lock()
            .map_err(|_| AdapterError::Operation("HTTP failure state poisoned".into()))?
            .take()
        {
            return Err(AdapterError::Operation(error));
        }
        Ok(())
    }

    fn detach(&mut self, context: &AdapterContext<'_>) -> Result<(), AdapterError> {
        self.quiesce(context)
    }
}

fn parse_config(instance: &AdapterInstance) -> Result<HttpAdapterConfig, AdapterError> {
    if instance.adapter_id != HTTP_ADAPTER_ID {
        return Err(AdapterError::InvalidConfig(format!(
            "HTTP factory cannot attach `{}`",
            instance.adapter_id
        )));
    }
    let config: HttpAdapterConfig = serde_json::from_value(instance.config.clone())?;
    ato_computation::PortId::parse(&config.port_id)
        .map_err(|error| AdapterError::InvalidConfig(error.to_string()))?;
    Ok(config)
}

fn read_event(
    record: &RecordEnvelope,
    context: &AdapterContext<'_>,
) -> Result<HttpEvent, AdapterError> {
    let metadata = context.objects.metadata(&record.payload_ref)?;
    let bytes = read_exact_object(
        context.objects,
        &record.payload_ref,
        metadata.size,
        16 << 20,
    )?;
    decode_event(&bytes).map_err(|error| AdapterError::Operation(error.to_string()))
}

fn spawn_proxy(
    listener: TcpListener,
    config: HttpAdapterConfig,
    stylus: Arc<dyn Stylus>,
    observations: Arc<dyn ObservationSink>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let local_seq = AtomicU64::new(0);
        loop {
            match listener.accept() {
                Ok((mut client, _)) => {
                    let _ = proxy_exchange(
                        &mut client,
                        config.upstream,
                        &ato_computation::PortId::parse(&config.port_id)
                            .expect("preflight validated HTTP port id"),
                        &mut |observation: AdapterObservation| {
                            let candidate = RecordCandidate {
                                protocol_id: observation.protocol_id.clone(),
                                operation_id: ato_computation::OperationId::parse(
                                    HTTP_REQUEST_OPERATION,
                                )
                                .expect("valid static HTTP operation"),
                                port_id: observation.port_id.clone(),
                                payload: observation.payload.clone(),
                                payload_version: 1,
                                required_features: BTreeSet::new(),
                                recorded_by: Some(HTTP_ADAPTER_ID.to_owned()),
                                stream: "http".to_owned(),
                                local_seq: local_seq.fetch_add(1, Ordering::Relaxed) + 1,
                                caused_by: Vec::new(),
                                observed_at: observed_now(),
                            };
                            if let Err(error) = stylus
                                .record(candidate)
                                .and_then(|_| observations.emit(observation))
                                && let Ok(mut slot) = failure.lock()
                            {
                                *slot = Some(error.to_string());
                            }
                        },
                    );
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && stop.load(std::sync::atomic::Ordering::Acquire) =>
                {
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => {
                    if let Ok(mut slot) = failure.lock() {
                        *slot = Some(error.to_string());
                    }
                    break;
                }
            }
        }
    })
}

fn observed_now() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or_else(|_| "0".to_owned(), |value| value.as_secs().to_string())
}

fn encode_request(event: &HttpEvent) -> Result<Vec<u8>, AdapterError> {
    let HttpEvent::Request {
        method,
        path,
        headers,
        body,
    } = event
    else {
        return Err(AdapterError::Operation(
            "cannot encode response as HTTP request".into(),
        ));
    };
    let mut bytes = format!("{method} {path} HTTP/1.1\r\n").into_bytes();
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("content-length") {
            continue;
        }
        bytes.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    bytes.extend_from_slice(format!("content-length: {}\r\n\r\n", body.len()).as_bytes());
    bytes.extend_from_slice(body);
    Ok(bytes)
}

/// Runs a real HTTP/1 proxy until its owning process is stopped. Every inbound
/// request and outbound response is emitted independently.
pub fn serve_proxy(
    listen: SocketAddr,
    upstream: SocketAddr,
    port_id: ato_computation::PortId,
    mut observe: impl FnMut(ato_adapter_api::AdapterObservation) + Send + 'static,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(listen)?;
    for client in listener.incoming() {
        let mut client = client?;
        // A client disconnect or a temporarily unavailable upstream is local to
        // that exchange; it must not tear down the capsule's adapter endpoint.
        let _ = proxy_exchange(&mut client, upstream, &port_id, &mut observe);
    }
    Ok(())
}

fn proxy_exchange(
    client: &mut TcpStream,
    upstream: SocketAddr,
    port_id: &ato_computation::PortId,
    observe: &mut impl FnMut(ato_adapter_api::AdapterObservation),
) -> std::io::Result<()> {
    // The supervising listener is nonblocking so it can observe shutdown.
    // Accepted sockets inherit that mode on some platforms (including macOS),
    // where `write_all` can otherwise stop at the socket-buffer boundary with
    // `WouldBlock` and leave the browser with a truncated response body.
    client.set_nonblocking(false)?;
    let request_bytes = read_http_message(client)?;
    let request = parse_request(&request_bytes)?;
    observe(ato_adapter_api::AdapterObservation {
        adapter_id: HTTP_ADAPTER_ID.to_owned(),
        protocol_id: ato_computation::ProtocolId::parse(HTTP_PROTOCOL_ID)
            .expect("valid static HTTP protocol id"),
        port_id: port_id.clone(),
        direction: ato_objects::Direction::Inbound,
        payload: encode_event(&request).map_err(std::io::Error::other)?,
        caused_by: Vec::new(),
        effect: ObservationEffect::Evolution,
    });

    let mut upstream_stream = connect_with_retry(upstream)?;
    upstream_stream.write_all(&request_bytes)?;
    let response_bytes = read_http_message(&mut upstream_stream)?;
    client.write_all(&response_bytes)?;
    Ok(())
}

fn connect_with_retry(upstream: SocketAddr) -> std::io::Result<TcpStream> {
    let mut last_error = None;
    for _ in 0..100 {
        match TcpStream::connect(upstream) {
            Ok(stream) => return Ok(stream),
            Err(error) => {
                last_error = Some(error);
                thread::sleep(Duration::from_millis(25));
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "upstream unavailable",
        )
    }))
}

fn read_http_message(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut byte = [0_u8; 1];
    while !bytes.ends_with(b"\r\n\r\n") {
        if bytes.len() >= 64 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "HTTP headers exceed 64 KiB",
            ));
        }
        stream.read_exact(&mut byte)?;
        bytes.push(byte[0]);
    }
    let header = String::from_utf8_lossy(&bytes);
    let content_length = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let header_length = bytes.len();
    bytes.resize(header_length + content_length, 0);
    stream.read_exact(&mut bytes[header_length..])?;
    Ok(bytes)
}

fn parse_request(bytes: &[u8]) -> std::io::Result<HttpEvent> {
    let (head, body) = split_message(bytes)?;
    let mut lines = head.lines();
    let start = lines.next().ok_or_else(invalid_http)?;
    let mut parts = start.split_whitespace();
    let method = parts.next().ok_or_else(invalid_http)?.to_owned();
    let path = parts.next().ok_or_else(invalid_http)?.to_owned();
    Ok(HttpEvent::Request {
        method,
        path,
        headers: parse_headers(lines)?,
        body: body.to_vec(),
    })
}

fn parse_response(bytes: &[u8]) -> std::io::Result<HttpEvent> {
    let (head, body) = split_message(bytes)?;
    let mut lines = head.lines();
    let start = lines.next().ok_or_else(invalid_http)?;
    let status = start
        .split_whitespace()
        .nth(1)
        .ok_or_else(invalid_http)?
        .parse::<u16>()
        .map_err(|_| invalid_http())?;
    Ok(HttpEvent::Response {
        status,
        headers: parse_headers(lines)?,
        body: body.to_vec(),
    })
}

fn split_message(bytes: &[u8]) -> std::io::Result<(&str, &[u8])> {
    let boundary = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(invalid_http)?;
    let head = std::str::from_utf8(&bytes[..boundary]).map_err(|_| invalid_http())?;
    Ok((head, &bytes[boundary + 4..]))
}

fn parse_headers<'a>(
    lines: impl Iterator<Item = &'a str>,
) -> std::io::Result<BTreeMap<String, String>> {
    lines
        .map(|line| {
            let (name, value) = line.split_once(':').ok_or_else(invalid_http)?;
            Ok((name.to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect()
}

fn invalid_http() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid HTTP/1 message")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct CapturingStylus {
        candidates: Mutex<Vec<RecordCandidate>>,
    }

    impl Stylus for CapturingStylus {
        fn record(&self, candidate: RecordCandidate) -> Result<(), AdapterError> {
            self.candidates.lock().unwrap().push(candidate);
            Ok(())
        }
    }

    #[test]
    fn proxy_forwards_large_response_from_nonblocking_listener_without_truncation() {
        let response_body = vec![b'x'; 2 * 1024 * 1024];
        let upstream_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let upstream = upstream_listener.local_addr().unwrap();
        let upstream_body = response_body.clone();
        let upstream_thread = thread::spawn(move || {
            let (mut stream, _) = upstream_listener.accept().unwrap();
            read_http_message(&mut stream).unwrap();
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                upstream_body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&upstream_body).unwrap();
        });

        let proxy_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy = proxy_listener.local_addr().unwrap();
        proxy_listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let failure = Arc::new(Mutex::new(None));
        let stylus = Arc::new(CapturingStylus::default());
        let proxy_thread = spawn_proxy(
            proxy_listener,
            HttpAdapterConfig {
                listen: proxy,
                upstream,
                port_id: "app.http".to_owned(),
                ready_path: None,
            },
            stylus.clone(),
            Arc::new(ato_adapter_api::IgnoreObservations),
            Arc::clone(&stop),
            Arc::clone(&failure),
        );

        let mut client = connect_with_retry(proxy).unwrap();
        client
            .write_all(b"GET /asset.js HTTP/1.1\r\nHost: test\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        let response = parse_response(&read_http_message(&mut client).unwrap()).unwrap();
        let HttpEvent::Response { status, body, .. } = response else {
            panic!("proxy returned a request event")
        };
        assert_eq!(status, 200);
        assert_eq!(body, response_body);

        upstream_thread.join().unwrap();
        stop.store(true, std::sync::atomic::Ordering::Release);
        TcpStream::connect(proxy).unwrap();
        proxy_thread.join().unwrap();
        assert!(failure.lock().unwrap().is_none());
        let candidates = stylus.candidates.lock().unwrap();
        assert_eq!(
            candidates.len(),
            1,
            "the response is runtime output, not a Record"
        );
        assert_eq!(candidates[0].operation_id.as_str(), HTTP_REQUEST_OPERATION);
        assert!(matches!(
            decode_event(&candidates[0].payload).unwrap(),
            HttpEvent::Request { .. }
        ));
    }

    #[test]
    fn legacy_reader_keeps_request_and_response_payloads_distinct() {
        let request = encode_event(&HttpEvent::Request {
            method: "POST".to_owned(),
            path: "/increment".to_owned(),
            headers: BTreeMap::new(),
            body: Vec::new(),
        })
        .unwrap();
        let response = encode_event(&HttpEvent::Response {
            status: 204,
            headers: BTreeMap::new(),
            body: Vec::new(),
        })
        .unwrap();
        assert_ne!(request, response);
        assert!(matches!(
            decode_event(&request).unwrap(),
            HttpEvent::Request { .. }
        ));
        assert!(matches!(
            decode_event(&response).unwrap(),
            HttpEvent::Response { .. }
        ));
    }
}
