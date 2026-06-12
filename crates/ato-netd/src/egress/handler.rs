//! Per-connection HTTP CONNECT handler.
//!
//! ## Decision sequence (mandatory order)
//!
//! ```text
//! hostname policy precheck
//!   → if denied: 403 + receipt{stage="hostname"}, resolver NOT called
//! DNS resolve
//!   → if error: 502 + receipt{stage="dns"}
//! CIDR / IP policy check (all resolved addrs; deny-any semantics)
//!   → if any addr denied: 403 + receipt{stage="cidr"}
//! TCP connect to first allowed addr
//!   → if connect fails: 502 + receipt{stage="connect"}
//! Write "200 Connection established"
//! Emit Allow receipt{stage="connect"} — BEFORE relay
//! Bidirectional TCP relay
//! ```
//!
//! ## Over-read protection
//!
//! [`tokio::io::BufReader`] may consume tunnel bytes (e.g. a TLS
//! `ClientHello`) while reading CONNECT headers.  After the header
//! terminator (`\r\n\r\n`) is found the leftover bytes in the read buffer
//! are extracted and forwarded to the upstream before the relay begins.

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use ato_net::{
    receipt::{EgressDecision, NetworkEgressDecision},
    resolver::{ResolveOptions, Resolver, ResolverError},
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    sync::mpsc,
};
use tracing::debug;

use super::policy::{EgressPolicy, PolicyDecision, normalize_hostname};

/// Maximum bytes consumed while reading CONNECT request headers.
/// Prevents memory exhaustion from a client sending infinite headers.
const MAX_HEADER_BYTES: usize = 16_384;

/// Entry point spawned per connection by [`super::EgressManager`].
///
/// Errors are logged at `debug` level and discarded; they are never
/// propagated because the caller (`tokio::spawn`) cannot act on them.
pub async fn handle_connect(
    stream: TcpStream,
    policy: Arc<EgressPolicy>,
    resolver: Arc<dyn Resolver + Send + Sync>,
    receipt_tx: mpsc::Sender<NetworkEgressDecision>,
) {
    if let Err(e) = handle_connect_inner(stream, policy, resolver, receipt_tx).await {
        debug!("egress CONNECT: connection closed with error: {e:#}");
    }
}

async fn handle_connect_inner(
    stream: TcpStream,
    policy: Arc<EgressPolicy>,
    resolver: Arc<dyn Resolver + Send + Sync>,
    receipt_tx: mpsc::Sender<NetworkEgressDecision>,
) -> anyhow::Result<()> {
    // ── 1. Parse CONNECT headers ──────────────────────────────────────────────

    let mut buf_reader = BufReader::new(stream);

    // Byte budget shared by the request line and all header lines.  The
    // budget is enforced *while* reading (inside `read_header_line`), so a
    // newline-less line cannot grow a buffer without bound.
    let mut header_budget = MAX_HEADER_BYTES;

    // Request line: "CONNECT host:port HTTP/1.1\r\n"
    let request_line = read_header_line(&mut buf_reader, &mut header_budget).await?;

    let (host, port) = parse_connect_line(&request_line)?;

    // Discard remaining headers up to the blank separator line.
    loop {
        let line = read_header_line(&mut buf_reader, &mut header_budget).await?;
        if line.is_empty() {
            anyhow::bail!("premature EOF in CONNECT headers");
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
    }

    // Extract any tunnel bytes already buffered beyond the headers.
    // These must be forwarded to upstream before the relay loop starts.
    let leftovers: Vec<u8> = buf_reader.buffer().to_vec();
    let mut client = buf_reader.into_inner();

    let decided_at = unix_secs_now();

    // ── 2. Hostname policy precheck ───────────────────────────────────────────

    // Only check hostname policy for actual hostnames (not IP literals).
    let is_ip_literal = host.parse::<IpAddr>().is_ok();

    if !is_ip_literal && let PolicyDecision::DenyHost = policy.check_hostname(&host) {
        write_error_response(&mut client, 403, "hostname", &host, port).await?;
        let _ = receipt_tx.try_send(NetworkEgressDecision {
            target: host,
            port,
            protocol: "tcp".to_string(),
            decision: EgressDecision::DenyHost,
            resolved_addr: None,
            decided_at_unix: decided_at,
            stage: "hostname".to_string(),
        });
        return Ok(());
    }

    // ── 3. DNS resolve (skipped for IP literals) ──────────────────────────────

    let addrs: Vec<IpAddr> = if is_ip_literal {
        vec![host.parse::<IpAddr>().unwrap()]
    } else {
        let opts = ResolveOptions::default();
        match resolver.resolve(&host, &opts).await {
            Ok(record) => record
                .addrs_v4
                .iter()
                .copied()
                .map(IpAddr::V4)
                .chain(record.addrs_v6.iter().copied().map(IpAddr::V6))
                .collect(),
            Err(e) => {
                let body = serde_json::json!({
                    "error": format!("DNS resolution failed for {host}: {e}"),
                    "stage": "dns",
                    "kind": typed_resolver_error_kind(&e),
                })
                .to_string();
                write_raw_error(&mut client, 502, "Bad Gateway", &body).await?;
                let _ = receipt_tx.try_send(NetworkEgressDecision {
                    target: host,
                    port,
                    protocol: "tcp".to_string(),
                    decision: EgressDecision::ResolveFailure,
                    resolved_addr: None,
                    decided_at_unix: decided_at,
                    stage: "dns".to_string(),
                });
                return Ok(());
            }
        }
    };

    if addrs.is_empty() {
        write_error_response(&mut client, 502, "dns", &host, port).await?;
        return Ok(());
    }

    // ── 4. CIDR / IP policy check ─────────────────────────────────────────────

    // Deny-any: if ANY resolved address falls in a denied CIDR, block all.
    let mut first_denied: Option<IpAddr> = None;
    let mut first_allowed: Option<IpAddr> = None;

    for &addr in &addrs {
        match policy.check_addr(addr) {
            PolicyDecision::DenyCidr => {
                if first_denied.is_none() {
                    first_denied = Some(addr);
                }
            }
            PolicyDecision::Allow if first_allowed.is_none() => {
                first_allowed = Some(addr);
            }
            _ => {}
        }
    }

    if let Some(denied_ip) = first_denied {
        write_error_response(&mut client, 403, "cidr", &host, port).await?;
        let _ = receipt_tx.try_send(NetworkEgressDecision {
            target: host,
            port,
            protocol: "tcp".to_string(),
            decision: EgressDecision::DenyCidr,
            resolved_addr: Some(denied_ip),
            decided_at_unix: decided_at,
            stage: "cidr".to_string(),
        });
        return Ok(());
    }

    // All addresses passed CIDR; use the first allowed one.
    let connect_ip = first_allowed.unwrap_or(addrs[0]);

    // ── 5. TCP connect ────────────────────────────────────────────────────────

    let connect_addr = SocketAddr::new(connect_ip, port);
    let mut upstream = match TcpStream::connect(connect_addr).await {
        Ok(s) => s,
        Err(e) => {
            let body = serde_json::json!({
                "error": format!("TCP connect to {connect_addr} failed: {e}"),
                "stage": "connect",
            })
            .to_string();
            write_raw_error(&mut client, 502, "Bad Gateway", &body).await?;
            let _ = receipt_tx.try_send(NetworkEgressDecision {
                target: host,
                port,
                protocol: "tcp".to_string(),
                decision: EgressDecision::ConnectFailure,
                resolved_addr: Some(connect_ip),
                decided_at_unix: decided_at,
                stage: "connect".to_string(),
            });
            return Ok(());
        }
    };

    // ── 6. Tunnel established — write 200 ────────────────────────────────────

    client
        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
        .await?;
    client.flush().await?;

    // Emit Allow receipt BEFORE the relay so tests can observe it without
    // waiting for the connection to close.
    let _ = receipt_tx.try_send(NetworkEgressDecision {
        target: host,
        port,
        protocol: "tcp".to_string(),
        decision: EgressDecision::Allow,
        resolved_addr: Some(connect_ip),
        decided_at_unix: decided_at,
        stage: "connect".to_string(),
    });

    // ── 7. Forward pre-buffered bytes ─────────────────────────────────────────

    if !leftovers.is_empty() {
        upstream.write_all(&leftovers).await?;
    }

    // ── 8. Bidirectional relay ────────────────────────────────────────────────

    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Read one `\n`-terminated line, enforcing the remaining byte `budget`
/// *during* the read.
///
/// Unlike [`AsyncBufReadExt::read_line`] — whose `BufReader` capacity bounds
/// only the internal buffer, not the appended `String` — this caps total
/// consumed bytes at `budget`, so a malicious client streaming a newline-less
/// line cannot exhaust memory (issue #644).
///
/// Returns the line including its terminator; an empty string signals EOF.
/// Fails when the line would exceed `budget` or is not valid UTF-8.
async fn read_header_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    budget: &mut usize,
) -> anyhow::Result<String> {
    let mut line: Vec<u8> = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            break; // EOF
        }
        let (n, terminated) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (available.len(), false),
        };
        if n > *budget {
            anyhow::bail!("CONNECT headers exceed {MAX_HEADER_BYTES} bytes");
        }
        *budget -= n;
        line.extend_from_slice(&available[..n]);
        reader.consume(n);
        if terminated {
            break;
        }
    }
    Ok(String::from_utf8(line)?)
}

/// Parse `CONNECT <authority> HTTP/1.1` and return `(host, port)`.
///
/// Handles three authority formats:
/// - `hostname:port` — regular hostname or IPv4 literal
/// - `[ipv6]:port`   — IPv6 literal
fn parse_connect_line(line: &str) -> anyhow::Result<(String, u16)> {
    let line = line.trim();
    let mut parts = line.splitn(3, ' ');
    match parts.next() {
        Some("CONNECT") => {}
        other => anyhow::bail!("expected CONNECT, got {:?}", other),
    }
    let authority = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("malformed CONNECT line: {line:?}"))?;
    parse_authority(authority)
}

fn parse_authority(authority: &str) -> anyhow::Result<(String, u16)> {
    if authority.starts_with('[') {
        // IPv6 literal: [::1]:443
        let bracket_end = authority
            .rfind(']')
            .ok_or_else(|| anyhow::anyhow!("malformed IPv6 authority: {authority:?}"))?;
        let host = authority[1..bracket_end].to_string();
        let port_str = authority[bracket_end + 1..]
            .strip_prefix(':')
            .ok_or_else(|| anyhow::anyhow!("missing port in IPv6 authority: {authority:?}"))?;
        let port = port_str
            .parse::<u16>()
            .map_err(|_| anyhow::anyhow!("invalid port {port_str:?} in {authority:?}"))?;
        Ok((host, port))
    } else {
        let colon = authority
            .rfind(':')
            .ok_or_else(|| anyhow::anyhow!("missing port in authority: {authority:?}"))?;
        let raw_host = &authority[..colon];
        let port = authority[colon + 1..]
            .parse::<u16>()
            .map_err(|_| anyhow::anyhow!("invalid port in authority: {authority:?}"))?;
        // Normalize hostname (lowercase, strip trailing dot).
        // If it's an IP literal, parse-then-format to canonicalize.
        let host = if raw_host.parse::<IpAddr>().is_ok() {
            raw_host.to_string()
        } else {
            normalize_hostname(raw_host)
        };
        Ok((host, port))
    }
}

async fn write_error_response(
    stream: &mut TcpStream,
    status: u16,
    stage: &str,
    target: &str,
    port: u16,
) -> anyhow::Result<()> {
    let status_text = if status == 403 {
        "Forbidden"
    } else {
        "Bad Gateway"
    };
    let body = serde_json::json!({
        "error": format!("connection to {target}:{port} denied"),
        "stage": stage,
    })
    .to_string();
    write_raw_error(stream, status, status_text, &body).await
}

async fn write_raw_error(
    stream: &mut TcpStream,
    status: u16,
    status_text: &str,
    body: &str,
) -> anyhow::Result<()> {
    let response = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

fn typed_resolver_error_kind(e: &ResolverError) -> &'static str {
    match e {
        ResolverError::NxDomain(_) => "nxdomain",
        ResolverError::Timeout(_) => "timeout",
        ResolverError::Servfail(_) => "servfail",
        ResolverError::TransportFailure(_) => "transport_failure",
        ResolverError::PolicyDenied(_) => "policy_denied",
        ResolverError::BackendUnavailable(_) => "backend_unavailable",
    }
}

fn unix_secs_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use ato_net::{
        receipt::EgressDecision,
        resolver::{ResolvedRecord, ResolverError},
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Start a minimal TCP echo server and return its listening port.
    async fn start_echo_server() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    loop {
                        let n = match stream.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        if stream.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        port
    }

    /// Fake `Resolver` that returns a fixed set of addresses and counts calls.
    struct FakeResolver {
        addrs: Vec<IpAddr>,
        err: Option<ResolverError>,
        call_count: Arc<AtomicUsize>,
    }

    impl FakeResolver {
        fn returning(addrs: Vec<IpAddr>) -> (Self, Arc<AtomicUsize>) {
            let counter = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    addrs,
                    err: None,
                    call_count: counter.clone(),
                },
                counter,
            )
        }

        fn failing(err: ResolverError) -> (Self, Arc<AtomicUsize>) {
            let counter = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    addrs: vec![],
                    err: Some(err),
                    call_count: counter.clone(),
                },
                counter,
            )
        }
    }

    #[async_trait]
    impl Resolver for FakeResolver {
        fn backend_name(&self) -> &str {
            "fake"
        }

        async fn resolve(
            &self,
            _name: &str,
            _opts: &ResolveOptions,
        ) -> Result<ResolvedRecord, ResolverError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            if let Some(e) = &self.err {
                return Err(e.clone());
            }
            let addrs_v4 = self
                .addrs
                .iter()
                .filter_map(|a| {
                    if let IpAddr::V4(v) = a {
                        Some(*v)
                    } else {
                        None
                    }
                })
                .collect();
            let addrs_v6 = self
                .addrs
                .iter()
                .filter_map(|a| {
                    if let IpAddr::V6(v) = a {
                        Some(*v)
                    } else {
                        None
                    }
                })
                .collect();
            Ok(ResolvedRecord {
                name: "test".to_string(),
                cname_chain: vec![],
                addrs_v4,
                addrs_v6,
                ttl_seconds: None,
                backend: "fake".to_string(),
                fallback_reason: None,
            })
        }
    }

    /// Send a CONNECT request to `addr` and read the response line.
    async fn send_connect(addr: SocketAddr, authority: &str) -> (u16, TcpStream) {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let req = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n");
        stream.write_all(req.as_bytes()).await.unwrap();
        // Read until we see "\r\n\r\n" (end of response headers).
        let mut buf = vec![0u8; 256];
        let mut response = String::new();
        loop {
            let n = stream.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            response.push_str(&String::from_utf8_lossy(&buf[..n]));
            if response.contains("\r\n\r\n") {
                break;
            }
        }
        let status: u16 = response
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        (status, stream)
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// Test 1: permissive policy + allowed IP → 200 + Allow receipt.
    #[tokio::test]
    async fn connect_allow_returns_200_and_receipt() {
        let echo_port = start_echo_server().await;
        let upstream_addr: IpAddr = "127.0.0.1".parse().unwrap();

        let (resolver, _counter) = FakeResolver::returning(vec![upstream_addr]);
        let (receipt_tx, mut receipt_rx) = mpsc::channel::<NetworkEgressDecision>(32);

        let policy = Arc::new(EgressPolicy::permissive());
        let resolver_arc: Arc<dyn Resolver + Send + Sync> = Arc::new(resolver);

        // Spawn a tiny proxy that accepts one CONNECT connection.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connect(stream, policy, resolver_arc, receipt_tx).await;
        });

        let (status, mut tunnel) =
            send_connect(proxy_addr, &format!("allowed.test:{echo_port}")).await;
        assert_eq!(status, 200, "expected 200 Connection established");

        // Receipt must be available before relay (not after close).
        let receipt = tokio::time::timeout(std::time::Duration::from_secs(2), receipt_rx.recv())
            .await
            .expect("receipt timed out")
            .expect("channel closed");

        assert_eq!(receipt.decision, EgressDecision::Allow);
        assert_eq!(receipt.stage, "connect");
        assert_eq!(receipt.port, echo_port);

        // Verify data flows through the tunnel (echo).
        tunnel.write_all(b"hello").await.unwrap();
        let mut buf = [0u8; 5];
        tunnel.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");
    }

    /// Test 2: hostname deny short-circuits DNS — resolver call count = 0.
    #[tokio::test]
    async fn hostname_deny_short_circuits_dns() {
        let (resolver, call_count) = FakeResolver::returning(vec![]);
        let (receipt_tx, mut receipt_rx) = mpsc::channel::<NetworkEgressDecision>(32);

        let policy = Arc::new(EgressPolicy::permissive().with_hostname_deny("denied.test"));
        let resolver_arc: Arc<dyn Resolver + Send + Sync> = Arc::new(resolver);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connect(stream, policy, resolver_arc, receipt_tx).await;
        });

        let (status, _) = send_connect(proxy_addr, "denied.test:80").await;
        assert_eq!(status, 403, "expected 403 Forbidden");

        // DNS must NOT have been called.
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            0,
            "resolver must not be called when hostname is denied"
        );

        // Receipt must report hostname stage.
        let receipt = tokio::time::timeout(std::time::Duration::from_secs(2), receipt_rx.recv())
            .await
            .expect("receipt timed out")
            .expect("channel closed");

        assert_eq!(receipt.decision, EgressDecision::DenyHost);
        assert_eq!(receipt.stage, "hostname");
    }

    /// Test 3: DNS rebinding — CIDR deny after resolution → 403 + DenyCidr receipt.
    #[tokio::test]
    async fn dns_rebinding_cidr_deny() {
        let private_ip: IpAddr = "192.168.1.1".parse().unwrap();
        let (resolver, _) = FakeResolver::returning(vec![private_ip]);
        let (receipt_tx, mut receipt_rx) = mpsc::channel::<NetworkEgressDecision>(32);

        let policy =
            Arc::new(EgressPolicy::permissive().with_cidr_deny("192.168.0.0/16".parse().unwrap()));
        let resolver_arc: Arc<dyn Resolver + Send + Sync> = Arc::new(resolver);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connect(stream, policy, resolver_arc, receipt_tx).await;
        });

        let (status, _) = send_connect(proxy_addr, "rebind.test:443").await;
        assert_eq!(status, 403, "expected 403 Forbidden for CIDR deny");

        let receipt = tokio::time::timeout(std::time::Duration::from_secs(2), receipt_rx.recv())
            .await
            .expect("receipt timed out")
            .expect("channel closed");

        assert_eq!(receipt.decision, EgressDecision::DenyCidr);
        assert_eq!(receipt.stage, "cidr");
    }

    /// Test 4: NXDOMAIN → 502 Bad Gateway, receipt = ResolveFailure at stage "dns".
    #[tokio::test]
    async fn nxdomain_returns_502() {
        let (resolver, _) = FakeResolver::failing(ResolverError::NxDomain("nxdomain.test".into()));
        let (receipt_tx, mut receipt_rx) = mpsc::channel::<NetworkEgressDecision>(32);

        let policy = Arc::new(EgressPolicy::permissive());
        let resolver_arc: Arc<dyn Resolver + Send + Sync> = Arc::new(resolver);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connect(stream, policy, resolver_arc, receipt_tx).await;
        });

        let (status, _) = send_connect(proxy_addr, "nxdomain.test:443").await;
        assert_eq!(status, 502, "expected 502 for NXDOMAIN");

        let receipt = tokio::time::timeout(std::time::Duration::from_secs(2), receipt_rx.recv())
            .await
            .expect("receipt timed out")
            .expect("channel closed");

        assert_eq!(receipt.decision, EgressDecision::ResolveFailure);
        assert_eq!(receipt.stage, "dns");
    }

    /// Test 5: TCP connect fails (unused port) → 502.
    #[tokio::test]
    async fn tcp_connect_failure_returns_502() {
        // Bind a listener to grab an ephemeral port, then drop it so
        // the port is closed by the time the handler tries to connect.
        let dead_port = {
            let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap().port()
        };

        let target_ip: IpAddr = "127.0.0.1".parse().unwrap();
        let (resolver, _) = FakeResolver::returning(vec![target_ip]);
        let (receipt_tx, mut receipt_rx) = mpsc::channel::<NetworkEgressDecision>(32);

        let policy = Arc::new(EgressPolicy::permissive());
        let resolver_arc: Arc<dyn Resolver + Send + Sync> = Arc::new(resolver);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connect(stream, policy, resolver_arc, receipt_tx).await;
        });

        let authority = format!("dead.test:{dead_port}");
        let (status, _) = send_connect(proxy_addr, &authority).await;
        assert_eq!(status, 502, "expected 502 when upstream TCP connect fails");

        // Receipt stage should be "connect".
        let receipt = tokio::time::timeout(std::time::Duration::from_secs(2), receipt_rx.recv())
            .await
            .expect("receipt timed out")
            .expect("channel closed");

        assert_eq!(receipt.stage, "connect");
        assert_eq!(receipt.decision, EgressDecision::ConnectFailure);
    }

    /// Test: pre-buffered bytes are forwarded to upstream (over-read protection).
    #[tokio::test]
    async fn pipelined_bytes_forwarded_after_connect() {
        let echo_port = start_echo_server().await;
        let upstream_addr: IpAddr = "127.0.0.1".parse().unwrap();

        let (resolver, _) = FakeResolver::returning(vec![upstream_addr]);
        let (receipt_tx, _) = mpsc::channel::<NetworkEgressDecision>(32);

        let policy = Arc::new(EgressPolicy::permissive());
        let resolver_arc: Arc<dyn Resolver + Send + Sync> = Arc::new(resolver);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connect(stream, policy, resolver_arc, receipt_tx).await;
        });

        // Connect and send CONNECT + tunnel data in a single write to
        // force the BufReader to over-read past the header terminator.
        let mut stream = TcpStream::connect(proxy_addr).await.unwrap();
        let msg = format!(
            "CONNECT allowed.test:{echo_port} HTTP/1.1\r\nHost: allowed.test\r\n\r\nPIPELINED"
        );
        stream.write_all(msg.as_bytes()).await.unwrap();

        // Read the 200 response.
        let mut buf = vec![0u8; 256];
        let mut response = String::new();
        loop {
            let n = stream.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            response.push_str(&String::from_utf8_lossy(&buf[..n]));
            if response.contains("\r\n\r\n") {
                break;
            }
        }
        assert!(response.starts_with("HTTP/1.1 200"));

        // The pipelined "PIPELINED" bytes must be echoed back.
        let mut echo_buf = [0u8; 9];
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.read_exact(&mut echo_buf),
        )
        .await
        .expect("echo timed out")
        .expect("read error");
        assert_eq!(&echo_buf, b"PIPELINED");
    }

    /// Regression (issue #644): a newline-less CONNECT line longer than
    /// MAX_HEADER_BYTES must abort the connection during the read instead
    /// of buffering it without bound.
    #[tokio::test]
    async fn oversized_connect_line_is_rejected() {
        let (resolver, call_count) = FakeResolver::returning(vec![]);
        let (receipt_tx, _receipt_rx) = mpsc::channel::<NetworkEgressDecision>(32);

        let policy = Arc::new(EgressPolicy::permissive());
        let resolver_arc: Arc<dyn Resolver + Send + Sync> = Arc::new(resolver);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();

        let handler = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_connect(stream, policy, resolver_arc, receipt_tx).await;
        });

        // Stream more than MAX_HEADER_BYTES with no `\n`.
        let mut stream = TcpStream::connect(proxy_addr).await.unwrap();
        let chunk = vec![b'A'; 4096];
        let mut written = 0usize;
        while written <= MAX_HEADER_BYTES {
            if stream.write_all(&chunk).await.is_err() {
                break; // handler already closed the connection
            }
            written += chunk.len();
        }
        let _ = stream.flush().await;

        // The handler must bail out instead of waiting for a newline.
        tokio::time::timeout(std::time::Duration::from_secs(5), handler)
            .await
            .expect("handler must abort an oversized CONNECT line")
            .unwrap();

        // The resolver must never have been consulted.
        assert_eq!(call_count.load(Ordering::SeqCst), 0);
    }

    /// Regression (issue #644): read_header_line enforces its byte budget
    /// while reading, and returns intact lines within budget.
    #[tokio::test]
    async fn read_header_line_enforces_budget_during_read() {
        // A newline-less line longer than the budget fails.
        let data = vec![b'A'; MAX_HEADER_BYTES * 2];
        let mut reader = BufReader::new(&data[..]);
        let mut budget = MAX_HEADER_BYTES;
        let err = read_header_line(&mut reader, &mut budget)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("exceed"), "got: {err:#}");

        // A line within budget is returned with terminator, budget debited.
        let data: &[u8] = b"CONNECT example.com:443 HTTP/1.1\r\nHost: x\r\n";
        let mut reader = BufReader::new(data);
        let mut budget = MAX_HEADER_BYTES;
        let line = read_header_line(&mut reader, &mut budget).await.unwrap();
        assert_eq!(line, "CONNECT example.com:443 HTTP/1.1\r\n");
        assert_eq!(budget, MAX_HEADER_BYTES - line.len());

        // EOF yields an empty string.
        let data: &[u8] = b"";
        let mut reader = BufReader::new(data);
        let mut budget = MAX_HEADER_BYTES;
        let line = read_header_line(&mut reader, &mut budget).await.unwrap();
        assert!(line.is_empty());
    }

    /// Test: parse_authority handles IPv6 literals correctly.
    #[test]
    fn parse_authority_ipv6_literal() {
        let (host, port) = parse_authority("[::1]:443").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 443);
    }

    /// Test: parse_authority normalises hostname.
    #[test]
    fn parse_authority_normalises_hostname() {
        let (host, port) = parse_authority("EXAMPLE.COM.:8080").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 8080);
    }
}
