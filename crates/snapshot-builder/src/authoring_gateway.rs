//! One registered builder slot serving both Authoring Preview and Terminal.
//!
//! Preview traffic normalizes the public ingress `Host` into the held guest's
//! authority, then relays bytes. Normal HTTP is one request per upstream
//! connection; a WebSocket becomes opaque after its normalized handshake.
//! Only the session-bound Terminal path is terminated here as a WebSocket, and
//! only after the API-injected builder bearer is verified.

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tungstenite::Message;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::http::StatusCode;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PREVIEW_REQUEST_HEAD_BYTES: usize = 64 * 1024;

pub struct AuthoringGateway {
    listen: SocketAddr,
    stopping: Arc<AtomicBool>,
    accept_thread: Option<std::thread::JoinHandle<()>>,
}

impl AuthoringGateway {
    pub fn start(
        listen: SocketAddr,
        preview_upstream: &str,
        builder_session_id: &str,
        expected_builder_token: &str,
        terminal_lines: Vec<String>,
    ) -> io::Result<Self> {
        let upstream: SocketAddr = preview_upstream.parse().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Authoring Preview upstream is not an ip:port address",
            )
        })?;
        TcpStream::connect_timeout(&upstream, CONNECT_TIMEOUT)?;
        let listener = TcpListener::bind(listen)?;
        let bound = listener.local_addr()?;
        let stopping = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stopping);
        let terminal_path = format!("/authoring/{builder_session_id}/terminal");
        let expected_authorization = format!("Bearer {expected_builder_token}");
        let accept_thread = std::thread::Builder::new()
            .name("ato-authoring-gateway".to_string())
            .spawn(move || {
                accept_loop(
                    listener,
                    upstream,
                    terminal_path,
                    expected_authorization,
                    terminal_lines,
                    stop_flag,
                )
            })?;
        Ok(Self {
            listen: bound,
            stopping,
            accept_thread: Some(accept_thread),
        })
    }

    #[allow(dead_code)]
    pub fn listen_addr(&self) -> SocketAddr {
        self.listen
    }

    fn shutdown(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.listen, Duration::from_millis(500));
        if let Some(thread) = self.accept_thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for AuthoringGateway {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn accept_loop(
    listener: TcpListener,
    preview_upstream: SocketAddr,
    terminal_path: String,
    expected_authorization: String,
    terminal_lines: Vec<String>,
    stopping: Arc<AtomicBool>,
) {
    for incoming in listener.incoming() {
        if stopping.load(Ordering::SeqCst) {
            return;
        }
        let Ok(client) = incoming else { continue };
        let path = terminal_path.clone();
        let authorization = expected_authorization.clone();
        let lines = terminal_lines.clone();
        let _ = std::thread::Builder::new()
            .name("ato-authoring-gateway-conn".to_string())
            .spawn(move || {
                if is_terminal_request(&client, &path) {
                    serve_terminal(client, &path, &authorization, &lines);
                } else if let Err(error) = relay_preview(client, preview_upstream) {
                    eprintln!("[builder] Authoring Preview connection ended: {error}");
                }
            });
    }
}

fn is_terminal_request(stream: &TcpStream, path: &str) -> bool {
    let _ = stream.set_read_timeout(Some(CONNECT_TIMEOUT));
    let mut prefix = [0_u8; 8192];
    let Ok(length) = stream.peek(&mut prefix) else {
        return false;
    };
    let request = String::from_utf8_lossy(&prefix[..length]);
    request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        == Some(path)
}

#[allow(clippy::result_large_err)]
fn serve_terminal(
    stream: TcpStream,
    expected_path: &str,
    expected_authorization: &str,
    lines: &[String],
) {
    let accepted = tungstenite::accept_hdr(stream, |request: &Request, response: Response| {
        authorize_terminal_handshake(request, response, expected_path, expected_authorization)
    });
    let Ok(mut socket) = accepted else {
        return;
    };
    for line in lines {
        if socket
            .send(Message::Text(format!("{line}\r\n").into()))
            .is_err()
        {
            return;
        }
    }
    let notice =
        "Suggested setup is reproducible and read-only. Select manual setup to run commands.";
    if socket
        .send(Message::Text(format!("{notice}\r\n").into()))
        .is_err()
    {
        return;
    }
    while let Ok(message) = socket.read() {
        match message {
            Message::Ping(bytes) => {
                if socket.send(Message::Pong(bytes)).is_err() {
                    break;
                }
            }
            Message::Text(_) | Message::Binary(_) => {
                if socket
                    .send(Message::Text(format!("{notice}\r\n").into()))
                    .is_err()
                {
                    break;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
}

#[allow(clippy::result_large_err)]
fn authorize_terminal_handshake(
    request: &Request,
    response: Response,
    expected_path: &str,
    expected_authorization: &str,
) -> Result<Response, ErrorResponse> {
    let path_matches = request.uri().path() == expected_path;
    let authorization_matches = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        == Some(expected_authorization);
    if path_matches && authorization_matches {
        return Ok(response);
    }
    let mut denied = ErrorResponse::new(Some(
        "Authoring Terminal authentication failed.".to_string(),
    ));
    *denied.status_mut() = StatusCode::UNAUTHORIZED;
    Err(denied)
}

fn relay_preview(mut client: TcpStream, upstream: SocketAddr) -> io::Result<()> {
    let mut server = TcpStream::connect_timeout(&upstream, CONNECT_TIMEOUT)?;
    let request = read_preview_request_head(&mut client)?;
    let normalized = normalize_preview_request(request, upstream)?;
    server.write_all(&normalized)?;
    client.set_read_timeout(None)?;
    let mut client_read = client.try_clone()?;
    let mut server_write = server.try_clone()?;
    let upstream_half = std::thread::spawn(move || {
        let copied = io::copy(&mut client_read, &mut server_write);
        let _ = server_write.shutdown(Shutdown::Write);
        copied
    });
    let downstream = io::copy(&mut server, &mut client);
    let _ = client.shutdown(Shutdown::Both);
    let _ = server.shutdown(Shutdown::Both);
    let _ = upstream_half.join();
    downstream.map(|_| ())
}

fn read_preview_request_head(client: &mut TcpStream) -> io::Result<Vec<u8>> {
    client.set_read_timeout(Some(CONNECT_TIMEOUT))?;
    let mut request = Vec::with_capacity(4096);
    let mut chunk = [0_u8; 4096];
    loop {
        let read = client.read(&mut chunk)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Authoring Preview request ended before its HTTP headers",
            ));
        }
        request.extend_from_slice(&chunk[..read]);
        if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            if end + 4 > MAX_PREVIEW_REQUEST_HEAD_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Authoring Preview request headers exceed 64 KiB",
                ));
            }
            return Ok(request);
        }
        if request.len() >= MAX_PREVIEW_REQUEST_HEAD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Authoring Preview request headers exceed 64 KiB",
            ));
        }
    }
}

/// Normalize the public builder origin into the guest-facing authority.
///
/// Source servers such as Vite reject an arbitrary public `Host` by default.
/// The Authoring Preview boundary owns that public origin, so forwarding it
/// unchanged leaks ingress topology into the capsule and makes an otherwise
/// healthy app return 403. Normal HTTP requests are also made one-per-upstream
/// connection: that keeps every request on a reused browser connection going
/// through this normalization. WebSocket upgrades retain their connection
/// headers and become an opaque byte stream after this first handshake.
fn normalize_preview_request(request: Vec<u8>, upstream: SocketAddr) -> io::Result<Vec<u8>> {
    let head_end = request
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .map(|position| position + 4)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Authoring Preview request has no complete HTTP header block",
            )
        })?;
    let mut lines = request[..head_end - 4]
        .split(|byte| *byte == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line));
    let request_line = lines
        .next()
        .filter(|line| !line.is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Authoring Preview request has no request line",
            )
        })?;
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

    let mut normalized = Vec::with_capacity(request.len() + 32);
    normalized.extend_from_slice(request_line);
    normalized.extend_from_slice(b"\r\n");
    let mut wrote_host = false;
    let mut wrote_connection = false;
    for line in headers {
        let Some((name, _)) = header_parts(line) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Authoring Preview request contains a malformed header",
            ));
        };
        if name.eq_ignore_ascii_case(b"host") {
            if !wrote_host {
                write!(&mut normalized, "Host: {upstream}\r\n")?;
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
        write!(&mut normalized, "Host: {upstream}\r\n")?;
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
    use std::io::{Read, Write};

    #[test]
    fn preview_requests_use_the_guest_authority_and_close_normal_http() {
        let upstream = TcpListener::bind("127.0.0.1:0").expect("upstream");
        let upstream_address = upstream.local_addr().expect("address");
        let echo = std::thread::spawn(move || {
            let (probe, _) = upstream.accept().expect("probe");
            drop(probe);
            let (mut stream, _) = upstream.accept().expect("accept");
            let mut request = [0_u8; 128];
            let read = stream.read(&mut request).expect("read");
            stream.write_all(&request[..read]).expect("echo");
        });
        let gateway = AuthoringGateway::start(
            "127.0.0.1:0".parse().unwrap(),
            &upstream_address.to_string(),
            "setup_1",
            "builder-secret",
            Vec::new(),
        )
        .expect("gateway");
        let mut client = TcpStream::connect(gateway.listen_addr()).expect("connect");
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: preview.example\r\nConnection: keep-alive\r\n\r\n")
            .expect("write");
        let mut echoed = [0_u8; 128];
        let read = client.read(&mut echoed).expect("read");
        let request = String::from_utf8_lossy(&echoed[..read]);
        assert!(request.starts_with("GET / HTTP/1.1"));
        assert!(request.contains(&format!("Host: {upstream_address}\r\n")));
        assert!(request.contains("Connection: close\r\n"));
        assert!(!request.contains("preview.example"));
        drop(gateway);
        echo.join().expect("echo thread");
    }

    #[test]
    fn preview_websocket_upgrade_keeps_upgrade_headers() {
        let upstream: SocketAddr = "127.0.0.1:8000".parse().expect("upstream");
        let request = b"GET /socket HTTP/1.1\r\nHost: preview.example\r\nConnection: keep-alive, Upgrade\r\nUpgrade: websocket\r\n\r\n".to_vec();

        let normalized = normalize_preview_request(request, upstream).expect("normalize");
        let normalized = String::from_utf8(normalized).expect("utf8");

        assert!(normalized.contains("Host: 127.0.0.1:8000\r\n"));
        assert!(normalized.contains("Connection: keep-alive, Upgrade\r\n"));
        assert!(normalized.contains("Upgrade: websocket\r\n"));
        assert!(!normalized.contains("Connection: close"));
    }
}
