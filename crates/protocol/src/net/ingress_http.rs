//! Deterministic HTTP request normalization at a capsule ingress boundary.
//!
//! Public ingress hostnames describe Ato topology, not the capsule's local
//! server. Forwarding that public `Host` unchanged makes development servers
//! such as Vite reject an otherwise healthy capsule. Both authoring preview
//! and normal runner ingress use this helper so the boundary policy cannot
//! drift between execution lanes.

use std::io::Write as _;

use thiserror::Error;

/// Maximum HTTP request-head size accepted by an Ato capsule ingress.
pub const MAX_REQUEST_HEAD_BYTES: usize = 64 * 1024;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum NormalizeRequestError {
    #[error("request has no complete HTTP header block")]
    IncompleteHead,
    #[error("request has no request line")]
    MissingRequestLine,
    #[error("request contains a malformed header")]
    MalformedHeader,
}

/// Replace the public `Host` with `upstream_authority`.
///
/// Normal HTTP is forced to `Connection: close`, ensuring that every request
/// on a browser-reused connection crosses this normalization. WebSocket
/// upgrades retain their connection headers and become an opaque byte stream
/// after the normalized handshake. Bytes already read after the header block
/// (for example a request body prefix) are preserved verbatim.
pub fn normalize_request_head(
    request: Vec<u8>,
    upstream_authority: &str,
) -> Result<Vec<u8>, NormalizeRequestError> {
    let head_end = request
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .map(|position| position + 4)
        .ok_or(NormalizeRequestError::IncompleteHead)?;
    let mut lines = request[..head_end - 4]
        .split(|byte| *byte == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line));
    let request_line = lines
        .next()
        .filter(|line| !line.is_empty())
        .ok_or(NormalizeRequestError::MissingRequestLine)?;
    let headers: Vec<&[u8]> = lines.collect();
    let has_upgrade_header = headers.iter().any(|line| {
        header_parts(line).is_some_and(|(name, _)| name.eq_ignore_ascii_case(b"upgrade"))
    });
    let connection_requests_upgrade = headers.iter().any(|line| {
        header_parts(line).is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case(b"connection")
                && value
                    .split(|byte| *byte == b',')
                    .any(|token| trim_ascii(token).eq_ignore_ascii_case(b"upgrade"))
        })
    });
    let is_upgrade = has_upgrade_header && connection_requests_upgrade;

    let mut normalized = Vec::with_capacity(request.len() + upstream_authority.len() + 32);
    normalized.extend_from_slice(request_line);
    normalized.extend_from_slice(b"\r\n");
    let mut wrote_host = false;
    let mut wrote_connection = false;
    for line in headers {
        let Some((name, _)) = header_parts(line) else {
            return Err(NormalizeRequestError::MalformedHeader);
        };
        if name.eq_ignore_ascii_case(b"host") {
            if !wrote_host {
                write!(&mut normalized, "Host: {upstream_authority}\r\n")
                    .expect("writing to Vec cannot fail");
                wrote_host = true;
            }
        } else if name.eq_ignore_ascii_case(b"connection") && !is_upgrade {
            if !wrote_connection {
                normalized.extend_from_slice(b"Connection: close\r\n");
                wrote_connection = true;
            }
        } else {
            normalized.extend_from_slice(line);
            normalized.extend_from_slice(b"\r\n");
        }
    }
    if !wrote_host {
        write!(&mut normalized, "Host: {upstream_authority}\r\n")
            .expect("writing to Vec cannot fail");
    }
    if !is_upgrade && !wrote_connection {
        normalized.extend_from_slice(b"Connection: close\r\n");
    }
    normalized.extend_from_slice(b"\r\n");
    normalized.extend_from_slice(&request[head_end..]);
    Ok(normalized)
}

fn header_parts(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let separator = line.iter().position(|byte| *byte == b':')?;
    let name = trim_ascii(&line[..separator]);
    (!name.is_empty()).then(|| (name, trim_ascii(&line[separator + 1..])))
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_http_uses_upstream_authority_and_closes() {
        let request = b"POST /api HTTP/1.1\r\nHost: public.example\r\nConnection: keep-alive\r\nContent-Length: 4\r\n\r\nbody".to_vec();
        let normalized = normalize_request_head(request, "172.16.0.2:8080").unwrap();
        let normalized = String::from_utf8(normalized).unwrap();

        assert!(normalized.starts_with("POST /api HTTP/1.1\r\n"));
        assert!(normalized.contains("Host: 172.16.0.2:8080\r\n"));
        assert!(normalized.contains("Connection: close\r\n"));
        assert!(!normalized.contains("public.example"));
        assert!(normalized.ends_with("\r\n\r\nbody"));
    }

    #[test]
    fn websocket_keeps_upgrade_headers() {
        let request = b"GET /socket HTTP/1.1\r\nHost: public.example\r\nConnection: keep-alive, Upgrade\r\nUpgrade: websocket\r\n\r\n".to_vec();
        let normalized = normalize_request_head(request, "127.0.0.1:8000").unwrap();
        let normalized = String::from_utf8(normalized).unwrap();

        assert!(normalized.contains("Host: 127.0.0.1:8000\r\n"));
        assert!(normalized.contains("Connection: keep-alive, Upgrade\r\n"));
        assert!(normalized.contains("Upgrade: websocket\r\n"));
        assert!(!normalized.contains("Connection: close"));
    }
}
