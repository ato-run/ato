//! HTTP request and response are deliberately separate adapter records.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

use ato_adapter_api::{Adapter, AdapterCapabilities, AdapterContext, AdapterError};
use ato_objects::{RecordEnvelope, read_exact_object};
use serde::{Deserialize, Serialize};

pub const HTTP_ADAPTER_ID: &str = "ato.http@1";
pub const HTTP_PROTOCOL_ID: &str = "ato.http@1";

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

impl Adapter for HttpAdapter {
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

    fn apply(
        &self,
        record: &RecordEnvelope,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        self.verify(record, context)
    }

    fn verify(
        &self,
        record: &RecordEnvelope,
        context: &AdapterContext<'_>,
    ) -> Result<(), AdapterError> {
        let metadata = context.objects.metadata(&record.payload_ref)?;
        let bytes = read_exact_object(
            context.objects,
            &record.payload_ref,
            metadata.size,
            16 << 20,
        )?;
        decode_event(&bytes)
            .map(|_| ())
            .map_err(|error| AdapterError::Operation(error.to_string()))
    }
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
    let request_bytes = read_http_message(client)?;
    let request = parse_request(&request_bytes)?;
    observe(ato_adapter_api::AdapterObservation {
        protocol_id: ato_computation::ProtocolId::parse(HTTP_PROTOCOL_ID)
            .expect("valid static HTTP protocol id"),
        port_id: port_id.clone(),
        direction: ato_objects::Direction::Inbound,
        payload: encode_event(&request).map_err(std::io::Error::other)?,
        caused_by: Vec::new(),
    });

    let mut upstream_stream = connect_with_retry(upstream)?;
    upstream_stream.write_all(&request_bytes)?;
    let response_bytes = read_http_message(&mut upstream_stream)?;
    let response = parse_response(&response_bytes)?;
    client.write_all(&response_bytes)?;
    observe(ato_adapter_api::AdapterObservation {
        protocol_id: ato_computation::ProtocolId::parse(HTTP_PROTOCOL_ID)
            .expect("valid static HTTP protocol id"),
        port_id: port_id.clone(),
        direction: ato_objects::Direction::Outbound,
        payload: encode_event(&response).map_err(std::io::Error::other)?,
        caused_by: Vec::new(),
    });
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

    #[test]
    fn request_and_response_are_distinct_protocol_events() {
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
