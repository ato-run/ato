//! Minimal HTTP forward proxy that gates every outbound connection
//! against an [`EgressPolicy`]. Phase 3 of the URL permission model.
//!
//! Why HTTP (not SOCKS5)?
//!   - Python's stdlib `urllib` + most CLIs (curl, wget, npm, pip) speak
//!     HTTP-proxy out of the box via `HTTPS_PROXY`/`HTTP_PROXY` env vars.
//!   - SOCKS5 requires extra client-side libraries (PySocks, etc.) and
//!     therefore cannot gate the common tooling reachable from the REPL.
//!
//! Protocol scope:
//!   - `CONNECT host:port HTTP/1.1`  → tunnel after 200 reply (HTTPS).
//!   - Absolute-form requests like `GET http://host/path HTTP/1.1` →
//!     checked against policy, then forwarded verbatim (plain HTTP).
//!   - Everything else → 400.
//!
//! Listens on `127.0.0.1:<ephemeral>` only; inbound from non-loopback
//! is dropped defensively.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::egress_policy::{Decision, EgressPolicy};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const DIAL_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_HEADER_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone)]
pub struct DenyEvent {
    pub host: String,
    pub port: u16,
}

pub type DenySink = Sender<DenyEvent>;

pub struct EgressProxyHandle {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EgressProxyHandle {
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http://127.0.0.1:PORT` — set into `HTTP_PROXY` / `HTTPS_PROXY` /
    /// `ALL_PROXY` on child processes. HTTPS traffic tunnels via CONNECT.
    pub fn http_url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

impl Drop for EgressProxyHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(100));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub struct EgressProxy;

impl EgressProxy {
    pub fn spawn(
        policy: Arc<Mutex<EgressPolicy>>,
        deny_sink: Option<DenySink>,
    ) -> std::io::Result<EgressProxyHandle> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
        let addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stop_clone = stop.clone();

        let thread = std::thread::Builder::new()
            .name("egress-proxy".into())
            .spawn(move || {
                for conn in listener.incoming() {
                    if stop_clone.load(Ordering::SeqCst) {
                        break;
                    }
                    let stream = match conn {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!(error=%e, "egress-proxy: accept failed");
                            continue;
                        }
                    };
                    let _ = stream.set_nonblocking(false);
                    let peer = match stream.peer_addr() {
                        Ok(p) => p,
                        Err(_) => {
                            let _ = stream.shutdown(Shutdown::Both);
                            continue;
                        }
                    };
                    if !is_loopback(peer.ip()) {
                        let _ = stream.shutdown(Shutdown::Both);
                        continue;
                    }
                    let policy = policy.clone();
                    let deny_sink = deny_sink.clone();
                    std::thread::Builder::new()
                        .name("egress-proxy-conn".into())
                        .spawn(move || {
                            if let Err(e) = handle_client(stream, &policy, deny_sink.as_ref()) {
                                tracing::debug!(error=%e, "egress-proxy: connection error");
                            }
                        })
                        .ok();
                }
            })?;

        tracing::info!(addr=%addr, "egress-proxy: listening (http)");
        Ok(EgressProxyHandle {
            addr,
            stop,
            thread: Some(thread),
        })
    }
}

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

fn handle_client(
    client: TcpStream,
    policy: &Arc<Mutex<EgressPolicy>>,
    deny_sink: Option<&DenySink>,
) -> std::io::Result<()> {
    client.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    client.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;

    let mut reader = BufReader::new(client.try_clone()?);

    // Read request line.
    let mut request_line = String::new();
    let n = reader.read_line(&mut request_line)?;
    if n == 0 {
        return Ok(());
    }
    let trimmed = request_line.trim_end_matches(['\r', '\n']).to_string();
    let mut parts = trimmed.splitn(3, ' ');
    let method = parts.next().unwrap_or("").to_ascii_uppercase();
    let target = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("HTTP/1.1");

    // Collect header bytes until blank line. Also capture raw bytes so we
    // can forward them verbatim for absolute-URI requests.
    let mut raw_headers = Vec::with_capacity(512);
    raw_headers.extend_from_slice(request_line.as_bytes());
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        raw_headers.extend_from_slice(line.as_bytes());
        if raw_headers.len() > MAX_HEADER_BYTES {
            let mut cw = client;
            let _ = cw.write_all(b"HTTP/1.1 431 Request Header Fields Too Large\r\nConnection: close\r\n\r\n");
            return Ok(());
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
    }

    if method == "CONNECT" {
        handle_connect(client, target, policy, deny_sink)?;
    } else {
        handle_absolute(client, reader, &method, target, version, raw_headers, policy, deny_sink)?;
    }
    Ok(())
}

fn parse_host_port(target: &str, default_port: u16) -> Option<(String, u16)> {
    // For CONNECT: "host:port" (IPv6: "[::1]:443").
    if let Some(rest) = target.strip_prefix('[') {
        if let Some(close) = rest.find(']') {
            let host = &rest[..close];
            let after = &rest[close + 1..];
            let port = after
                .strip_prefix(':')
                .and_then(|p| p.parse::<u16>().ok())
                .unwrap_or(default_port);
            return Some((host.to_string(), port));
        }
    }
    if let Some((h, p)) = target.rsplit_once(':') {
        if let Ok(port) = p.parse::<u16>() {
            return Some((h.to_string(), port));
        }
    }
    Some((target.to_string(), default_port))
}

fn parse_absolute_url(url: &str) -> Option<(String, u16, String)> {
    // Returns (host, port, path-with-query). Supports http:// and https://.
    let (scheme, rest) = if let Some(r) = url.strip_prefix("http://") {
        ("http", r)
    } else if let Some(r) = url.strip_prefix("https://") {
        ("https", r)
    } else {
        return None;
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let default_port = if scheme == "https" { 443 } else { 80 };
    let (host, port) = parse_host_port(authority, default_port)?;
    Some((host, port, path.to_string()))
}

fn handle_connect(
    mut client: TcpStream,
    target: &str,
    policy: &Arc<Mutex<EgressPolicy>>,
    deny_sink: Option<&DenySink>,
) -> std::io::Result<()> {
    let (host, port) = match parse_host_port(target, 443) {
        Some(hp) => hp,
        None => {
            let _ = client.write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n");
            return Ok(());
        }
    };

    let decision = policy
        .lock()
        .map(|p| p.decide(&host, port))
        .unwrap_or(Decision::DenyAskUser);

    if !matches!(decision, Decision::Allow) {
        tracing::info!(host=%host, port=port, "egress-proxy: DENY (connect)");
        if let Some(sink) = deny_sink {
            let _ = sink.send(DenyEvent {
                host: host.clone(),
                port,
            });
        }
        let _ = client.write_all(
            b"HTTP/1.1 403 Forbidden\r\nX-Ato-Egress: denied\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        );
        return Ok(());
    }

    let upstream = match TcpStream::connect_timeout_any(&format!("{host}:{port}"), DIAL_TIMEOUT) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(host=%host, port=port, error=%e, "egress-proxy: dial failed");
            let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n");
            return Ok(());
        }
    };

    client.set_read_timeout(None)?;
    client.set_write_timeout(None)?;
    upstream.set_read_timeout(None)?;
    upstream.set_write_timeout(None)?;

    client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;

    tracing::debug!(host=%host, port=port, "egress-proxy: ALLOW CONNECT, relaying");
    relay(client, upstream);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn handle_absolute(
    mut client: TcpStream,
    mut reader: BufReader<TcpStream>,
    method: &str,
    target: &str,
    _version: &str,
    raw_headers: Vec<u8>,
    policy: &Arc<Mutex<EgressPolicy>>,
    deny_sink: Option<&DenySink>,
) -> std::io::Result<()> {
    // Only absolute-form URIs are valid for a forward proxy. Reject
    // origin-form ("GET /path") — that would mean the client thinks we
    // are the origin server.
    let (host, port, _path) = match parse_absolute_url(target) {
        Some(x) => x,
        None => {
            let _ = client.write_all(
                b"HTTP/1.1 400 Bad Request\r\nX-Ato-Egress: non-proxy-request\r\nConnection: close\r\n\r\n",
            );
            return Ok(());
        }
    };

    // Reject https:// absolute-URI requests (clients should use CONNECT
    // for HTTPS — handling them here would silently strip TLS).
    if target.starts_with("https://") {
        let _ = client.write_all(
            b"HTTP/1.1 400 Bad Request\r\nX-Ato-Egress: use-connect-for-https\r\nConnection: close\r\n\r\n",
        );
        return Ok(());
    }

    let decision = policy
        .lock()
        .map(|p| p.decide(&host, port))
        .unwrap_or(Decision::DenyAskUser);

    if !matches!(decision, Decision::Allow) {
        tracing::info!(host=%host, port=port, method=%method, "egress-proxy: DENY (http)");
        if let Some(sink) = deny_sink {
            let _ = sink.send(DenyEvent {
                host: host.clone(),
                port,
            });
        }
        let _ = client.write_all(
            b"HTTP/1.1 403 Forbidden\r\nX-Ato-Egress: denied\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
        );
        return Ok(());
    }

    let mut upstream = match TcpStream::connect_timeout_any(&format!("{host}:{port}"), DIAL_TIMEOUT)
    {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(host=%host, port=port, error=%e, "egress-proxy: dial failed");
            let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\n\r\n");
            return Ok(());
        }
    };

    // Forward request: we don't rewrite — just send the headers we read
    // (absolute-form) plus any buffered body. Modern servers accept the
    // absolute-form URI in request-line if they're expecting proxy
    // traffic, but most origin servers want origin-form. To be safe,
    // rebuild the request line with the path only.
    let new_request_line = format!("{method} {} HTTP/1.1\r\n", extract_path(target));
    upstream.write_all(new_request_line.as_bytes())?;

    // Skip the first line of raw_headers and forward the rest.
    if let Some(newline) = raw_headers.iter().position(|&b| b == b'\n') {
        upstream.write_all(&raw_headers[newline + 1..])?;
    }

    // Forward any bytes the reader has buffered (body, pipelined data).
    let mut buf = Vec::new();
    let avail = reader.buffer().len();
    if avail > 0 {
        buf.extend_from_slice(reader.buffer());
        upstream.write_all(&buf)?;
    }

    client.set_read_timeout(None)?;
    client.set_write_timeout(None)?;
    upstream.set_read_timeout(None)?;
    upstream.set_write_timeout(None)?;

    // Hand off the raw socket back so relay can take both halves.
    let client_stream = reader.into_inner();
    relay(client_stream, upstream);
    Ok(())
}

fn extract_path(absolute: &str) -> String {
    let rest = absolute
        .strip_prefix("http://")
        .or_else(|| absolute.strip_prefix("https://"))
        .unwrap_or(absolute);
    match rest.find('/') {
        Some(i) => rest[i..].to_string(),
        None => "/".to_string(),
    }
}

fn relay(client: TcpStream, upstream: TcpStream) {
    let c2u_client = match client.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    };
    let u2c_upstream = match upstream.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    };
    let t1 = std::thread::spawn(move || copy_direction(c2u_client, upstream));
    let t2 = std::thread::spawn(move || copy_direction(u2c_upstream, client));
    let _ = t1.join();
    let _ = t2.join();
}

fn copy_direction(mut src: TcpStream, mut dst: TcpStream) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        match src.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if dst.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
        }
    }
    let _ = dst.shutdown(Shutdown::Write);
    let _ = src.shutdown(Shutdown::Read);
}

trait ConnectAny {
    fn connect_timeout_any(addr: &str, timeout: Duration) -> std::io::Result<TcpStream>;
}
impl ConnectAny for TcpStream {
    fn connect_timeout_any(addr: &str, timeout: Duration) -> std::io::Result<TcpStream> {
        use std::net::ToSocketAddrs;
        let addrs: Vec<SocketAddr> = addr.to_socket_addrs()?.collect();
        if addrs.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                "no addresses resolved",
            ));
        }
        let per = timeout / (addrs.len() as u32).max(1);
        let mut last_err = None;
        for a in addrs {
            match TcpStream::connect_timeout(&a, per) {
                Ok(s) => return Ok(s),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err
            .unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "connect failed")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::egress_policy::{EgressPolicy, HostPattern};
    use std::io::{Read, Write};

    fn make_policy(allows: &[&str]) -> Arc<Mutex<EgressPolicy>> {
        let mut pol = EgressPolicy::localhost_only();
        for a in allows {
            pol.allow(HostPattern::parse(a).unwrap());
        }
        Arc::new(Mutex::new(pol))
    }

    fn send_connect(proxy: SocketAddr, target: &str) -> (TcpStream, String) {
        let mut s = TcpStream::connect(proxy).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
        let req = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n");
        s.write_all(req.as_bytes()).unwrap();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 512];
        // Read just the status line + headers (until blank line).
        loop {
            let n = s.read(&mut tmp).unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let header = String::from_utf8_lossy(&buf).to_string();
        (s, header)
    }

    #[test]
    fn denies_non_allowlisted_connect() {
        let policy = make_policy(&[]);
        let handle = EgressProxy::spawn(policy, None).unwrap();
        let (_s, hdr) = send_connect(handle.addr(), "example.com:443");
        assert!(hdr.contains("403"), "expected 403, got: {hdr}");
        assert!(hdr.contains("X-Ato-Egress: denied"));
    }

    #[test]
    fn allows_explicitly_granted_connect() {
        // Bind a tiny upstream on loopback (always allowed).
        let upstream = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let up_port = upstream.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = upstream.accept() {
                let mut buf = [0u8; 4];
                if s.read_exact(&mut buf).is_ok() {
                    let _ = s.write_all(&buf);
                }
            }
        });

        let policy = make_policy(&[]);
        let handle = EgressProxy::spawn(policy, None).unwrap();
        let target = format!("localhost:{up_port}");
        let (mut s, hdr) = send_connect(handle.addr(), &target);
        assert!(hdr.contains("200"), "expected 200, got: {hdr}");
        s.write_all(b"ping").unwrap();
        let mut reply = [0u8; 4];
        s.read_exact(&mut reply).unwrap();
        assert_eq!(&reply, b"ping");
    }

    #[test]
    fn deny_event_is_published() {
        use std::sync::mpsc::channel;
        let policy = make_policy(&[]);
        let (tx, rx) = channel::<DenyEvent>();
        let handle = EgressProxy::spawn(policy, Some(tx)).unwrap();
        let (_s, hdr) = send_connect(handle.addr(), "blocked.example.com:443");
        assert!(hdr.contains("403"));
        let ev = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(ev.host, "blocked.example.com");
        assert_eq!(ev.port, 443);
    }

    #[test]
    fn rejects_origin_form_request() {
        let policy = make_policy(&["example.com"]);
        let handle = EgressProxy::spawn(policy, None).unwrap();
        let mut s = TcpStream::connect(handle.addr()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        // Origin-form: client thinks we are the origin. Must be rejected.
        s.write_all(b"GET /foo HTTP/1.1\r\nHost: example.com\r\n\r\n")
            .unwrap();
        let mut buf = [0u8; 256];
        let n = s.read(&mut buf).unwrap();
        let hdr = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(hdr.contains("400"), "expected 400, got: {hdr}");
    }

    #[test]
    fn denies_absolute_http_not_allowlisted() {
        let policy = make_policy(&[]);
        let handle = EgressProxy::spawn(policy, None).unwrap();
        let mut s = TcpStream::connect(handle.addr()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        s.write_all(b"GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\n\r\n")
            .unwrap();
        let mut buf = [0u8; 256];
        let n = s.read(&mut buf).unwrap();
        let hdr = String::from_utf8_lossy(&buf[..n]).to_string();
        assert!(hdr.contains("403"), "expected 403, got: {hdr}");
    }

    #[test]
    fn parses_host_port_ipv6() {
        assert_eq!(
            parse_host_port("[::1]:443", 80).unwrap(),
            ("::1".to_string(), 443)
        );
        assert_eq!(
            parse_host_port("example.com:8080", 80).unwrap(),
            ("example.com".to_string(), 8080)
        );
    }

    #[test]
    fn parses_absolute_url() {
        let (h, p, path) = parse_absolute_url("http://example.com/foo?x=1").unwrap();
        assert_eq!(h, "example.com");
        assert_eq!(p, 80);
        assert_eq!(path, "/foo?x=1");
        let (h, p, _) = parse_absolute_url("http://example.com:8080/").unwrap();
        assert_eq!(h, "example.com");
        assert_eq!(p, 8080);
    }
}
