//! Acceptance tests for the ingress reverse proxy (slice B, #297).
//!
//! Each test:
//! 1. Spins up an in-process upstream HTTP server.
//! 2. Launches `ato-netd` as a subprocess with its own `ATO_HOME` tempdir.
//! 3. Registers the ingress route via `netd::net::control::Client`.
//! 4. Sends HTTP / WebSocket traffic to `127.0.0.1:<stable_port>`.
//! 5. Asserts the expected behaviour.
//! 6. Shuts down `ato-netd` cleanly via `Client::shutdown`.
//!
//! Tests are serialised with `#[serial_test::serial]` because each fresh
//! daemon starts its port allocator at port 40000. Running tests concurrently
//! causes multiple daemons to compete for the same port.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

use bytes::Bytes;
use http::Request;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use netd::net::control::{Client, IngressInfo};
use serial_test::serial;
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::sleep,
};

// ── Helpers ───────────────────────────────────────────────────────────────

fn ato_netd_binary() -> PathBuf {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_ato-netd") {
        return PathBuf::from(path);
    }
    let manifest_dir = std::env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir).join("../../target/debug/ato-netd")
}

fn spawn_daemon(ato_home: &TempDir) -> (Child, PathBuf) {
    let ato_home_path = ato_home.path().to_path_buf();
    let run_dir = ato_home_path.join("run");
    std::fs::create_dir_all(&run_dir).expect("create run dir");
    let socket_path = run_dir.join("netd.sock");

    let child = Command::new(ato_netd_binary())
        .env("ATO_HOME", &ato_home_path)
        .env("RUST_LOG", "netd=warn")
        .arg("--socket")
        .arg(&socket_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn ato-netd");

    (child, socket_path)
}

async fn wait_for_daemon(socket_path: &Path, timeout_ms: u64) -> Client {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        match Client::connect(socket_path).await {
            Ok(client) => return client,
            Err(_) => {
                if tokio::time::Instant::now() >= deadline {
                    panic!("ato-netd did not start within {timeout_ms}ms");
                }
                sleep(Duration::from_millis(25)).await;
            }
        }
    }
}

async fn shutdown_daemon(client: Client, mut child: Child) {
    client.shutdown().await.ok();
    for _ in 0..40 {
        match child.try_wait() {
            Ok(Some(_)) => return,
            _ => sleep(Duration::from_millis(25)).await,
        }
    }
    child.kill().ok();
    child.wait().ok();
}

async fn free_listener() -> (TcpListener, SocketAddr) {
    let l = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = l.local_addr().unwrap();
    (l, addr)
}

struct SimpleResp {
    status: u16,
    body: Vec<u8>,
}

/// Minimal HTTP/1.1 client (no external HTTP client dep required).
async fn http_get(url: &str) -> SimpleResp {
    let url: hyper::Uri = url.parse().expect("valid url");
    let host = url.host().unwrap_or("127.0.0.1");
    let port = url.port_u16().unwrap_or(80);

    let stream = tokio::net::TcpStream::connect(format!("{host}:{port}"))
        .await
        .expect("tcp connect");
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::Builder::new()
        .handshake::<_, Empty<Bytes>>(io)
        .await
        .expect("http handshake");
    tokio::spawn(conn);

    let path = url.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
    let req = Request::builder()
        .uri(path)
        .header("host", format!("{host}:{port}"))
        .body(Empty::<Bytes>::new())
        .unwrap();

    let resp = sender.send_request(req).await.expect("send request");
    let status = resp.status().as_u16();
    // Tolerate truncated bodies (upstream-gone-mid-stream scenario).
    let body = resp.collect().await.unwrap_or_default().to_bytes().to_vec();

    SimpleResp { status, body }
}

// ── Tests ─────────────────────────────────────────────────────────────────

/// Test 1: byte-for-byte static GET response.
#[serial]
#[tokio::test]
async fn static_get_byte_for_byte() {
    let (upstream_listener, upstream_addr) = free_listener().await;

    tokio::spawn(async move {
        while let Ok((stream, _)) = upstream_listener.accept().await {
            let io = TokioIo::new(stream);
            tokio::spawn(hyper::server::conn::http1::Builder::new().serve_connection(
                io,
                service_fn(|_req: Request<Incoming>| async {
                    Ok::<_, hyper::Error>(
                        http::Response::builder()
                            .status(200)
                            .header("content-type", "text/plain")
                            .body(Full::new(Bytes::from("hello proxy")))
                            .unwrap(),
                    )
                }),
            ));
        }
    });

    let ato_home = TempDir::new().unwrap();
    let (child, socket_path) = spawn_daemon(&ato_home);
    let mut client = wait_for_daemon(&socket_path, 3000).await;

    let IngressInfo { port } = client
        .register_ingress("test-static", &format!("http://{upstream_addr}"))
        .await
        .expect("register_ingress");

    sleep(Duration::from_millis(150)).await;

    let resp = http_get(&format!("http://127.0.0.1:{port}/")).await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"hello proxy");

    shutdown_daemon(client, child).await;
}

/// Test 2: SSE passthrough — 5 events 200ms apart arrive in order.
///
/// Uses a raw TCP upstream to avoid hyper v1 streaming body complexity.
#[serial]
#[tokio::test]
async fn sse_passthrough_events_arrive_in_order() {
    let (upstream_listener, upstream_addr) = free_listener().await;

    tokio::spawn(async move {
        if let Ok((mut stream, _)) = upstream_listener.accept().await {
            // Drain the request.
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;

            // Write SSE headers.
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\n\
                      content-type: text/event-stream\r\n\
                      cache-control: no-cache\r\n\
                      connection: keep-alive\r\n\
                      \r\n",
                )
                .await
                .ok();

            // Write 5 events with 200ms gaps.
            for i in 1..=5u8 {
                sleep(Duration::from_millis(200)).await;
                let event = format!("data: event{i}\n\n");
                if stream.write_all(event.as_bytes()).await.is_err() {
                    break;
                }
            }
        }
    });

    let ato_home = TempDir::new().unwrap();
    let (child, socket_path) = spawn_daemon(&ato_home);
    let mut client = wait_for_daemon(&socket_path, 3000).await;

    let IngressInfo { port } = client
        .register_ingress("test-sse", &format!("http://{upstream_addr}"))
        .await
        .expect("register_ingress");

    sleep(Duration::from_millis(150)).await;

    let mut tcp = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .unwrap();
    tcp.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n")
        .await
        .unwrap();

    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if tokio::time::Instant::now() > deadline {
            break;
        }
        match tokio::time::timeout(Duration::from_millis(500), tcp.read(&mut chunk)).await {
            Ok(Ok(0)) | Err(_) => break,
            Ok(Ok(n)) => {
                buf.extend_from_slice(&chunk[..n]);
                let events = buf
                    .windows(11)
                    .filter(|w| w.starts_with(b"data: event"))
                    .count();
                if events >= 5 {
                    break;
                }
            }
            Ok(Err(_)) => break,
        }
    }

    let events = buf
        .windows(11)
        .filter(|w| w.starts_with(b"data: event"))
        .count();
    assert_eq!(events, 5, "expected 5 SSE events, got {events}");

    // Confirm ordering: event1 before event2 … event4 before event5.
    for i in 1..=4usize {
        let pos_a = buf
            .windows(12)
            .position(|w| w == format!("data: event{i}").as_bytes())
            .unwrap_or(usize::MAX);
        let pos_b = buf
            .windows(12)
            .position(|w| w == format!("data: event{}", i + 1).as_bytes())
            .unwrap_or(usize::MAX);
        assert!(pos_a < pos_b, "event{i} must arrive before event{}", i + 1);
    }

    shutdown_daemon(client, child).await;
}

/// Test 3: WebSocket echo — 10 frames round-trip through the proxy.
#[serial]
#[tokio::test]
async fn websocket_echo_10_frames() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    let (upstream_listener, upstream_addr) = free_listener().await;

    tokio::spawn(async move {
        if let Ok((stream, _)) = upstream_listener.accept().await {
            let ws = tokio_tungstenite::accept_async(stream)
                .await
                .expect("upstream ws accept");
            let (mut tx, mut rx) = ws.split();
            while let Some(Ok(msg)) = rx.next().await {
                if msg.is_text() || msg.is_binary() {
                    tx.send(msg).await.ok();
                }
            }
        }
    });

    let ato_home = TempDir::new().unwrap();
    let (child, socket_path) = spawn_daemon(&ato_home);
    let mut client = wait_for_daemon(&socket_path, 3000).await;

    let IngressInfo { port } = client
        .register_ingress("test-ws", &format!("http://{upstream_addr}"))
        .await
        .expect("register_ingress");

    sleep(Duration::from_millis(150)).await;

    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/"))
        .await
        .expect("ws connect through proxy");
    let (mut tx, mut rx) = ws.split();

    for i in 0..10u8 {
        tx.send(Message::text(format!("frame{i}"))).await.unwrap();
    }

    let mut echoed = 0u8;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if tokio::time::Instant::now() > deadline {
            break;
        }
        match tokio::time::timeout(Duration::from_millis(500), rx.next()).await {
            Ok(Some(Ok(Message::Text(_)))) => {
                echoed += 1;
                if echoed >= 10 {
                    break;
                }
            }
            _ => break,
        }
    }

    assert_eq!(echoed, 10, "expected 10 echo frames, got {echoed}");

    shutdown_daemon(client, child).await;
}

/// Test 4: long-poll — upstream holds response for 20 s, proxy must not
/// drop or time out the connection prematurely.
#[serial]
#[tokio::test]
async fn long_poll_20s() {
    let (upstream_listener, upstream_addr) = free_listener().await;

    tokio::spawn(async move {
        if let Ok((stream, _)) = upstream_listener.accept().await {
            let io = TokioIo::new(stream);
            hyper::server::conn::http1::Builder::new()
                .serve_connection(
                    io,
                    service_fn(|_req: Request<Incoming>| async {
                        sleep(Duration::from_secs(20)).await;
                        Ok::<_, hyper::Error>(http::Response::new(Full::new(Bytes::from(
                            "long-poll done",
                        ))))
                    }),
                )
                .await
                .ok();
        }
    });

    let ato_home = TempDir::new().unwrap();
    let (child, socket_path) = spawn_daemon(&ato_home);
    let mut client = wait_for_daemon(&socket_path, 3000).await;

    let IngressInfo { port } = client
        .register_ingress("test-longpoll", &format!("http://{upstream_addr}"))
        .await
        .expect("register_ingress");

    sleep(Duration::from_millis(150)).await;

    let result = tokio::time::timeout(
        Duration::from_secs(25),
        http_get(&format!("http://127.0.0.1:{port}/")),
    )
    .await
    .expect("long-poll must complete within 25 s");

    assert_eq!(result.status, 200);
    assert_eq!(result.body, b"long-poll done");

    shutdown_daemon(client, child).await;
}

/// Test 5: upstream closes the connection mid-stream. The proxy must
/// forward what it received and close cleanly without hanging.
#[serial]
#[tokio::test]
async fn upstream_gone_mid_stream() {
    let (upstream_listener, upstream_addr) = free_listener().await;

    tokio::spawn(async move {
        if let Ok((mut stream, _)) = upstream_listener.accept().await {
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;

            // Write a partial response then drop → upstream gone mid-stream.
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\n\
                      content-length: 10\r\n\
                      \r\n\
                      part1",
                )
                .await
                .ok();
            sleep(Duration::from_millis(200)).await;
            // Dropping `stream` closes the TCP connection.
        }
    });

    let ato_home = TempDir::new().unwrap();
    let (child, socket_path) = spawn_daemon(&ato_home);
    let mut client = wait_for_daemon(&socket_path, 3000).await;

    let IngressInfo { port } = client
        .register_ingress("test-midstream", &format!("http://{upstream_addr}"))
        .await
        .expect("register_ingress");

    sleep(Duration::from_millis(150)).await;

    // Any response within 5 s is acceptable. What matters: no hang,
    // no zombie proxy task, daemon still healthy.
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        http_get(&format!("http://127.0.0.1:{port}/")),
    )
    .await
    .expect("proxy must not hang after upstream drops mid-stream");

    // Daemon is still alive and responsive.
    let status = client.status().await.expect("daemon still healthy");
    assert!(status.pid > 0, "daemon pid must be non-zero");
    let _ = result;

    shutdown_daemon(client, child).await;
}

/// Test 6: two independent routes proxy to different upstreams.
#[serial]
#[tokio::test]
async fn concurrent_routes() {
    let (listener_a, addr_a) = free_listener().await;
    let (listener_b, addr_b) = free_listener().await;

    for (listener, label) in [(listener_a, "route-A"), (listener_b, "route-B")] {
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let io = TokioIo::new(stream);
                let body = Full::new(Bytes::from(label));
                tokio::spawn(hyper::server::conn::http1::Builder::new().serve_connection(
                    io,
                    service_fn(move |_req: Request<Incoming>| {
                        let body = body.clone();
                        async move { Ok::<_, hyper::Error>(http::Response::new(body)) }
                    }),
                ));
            }
        });
    }

    let ato_home = TempDir::new().unwrap();
    let (child, socket_path) = spawn_daemon(&ato_home);
    let mut client = wait_for_daemon(&socket_path, 3000).await;

    let IngressInfo { port: port_a } = client
        .register_ingress("concurrent-a", &format!("http://{addr_a}"))
        .await
        .unwrap();
    let IngressInfo { port: port_b } = client
        .register_ingress("concurrent-b", &format!("http://{addr_b}"))
        .await
        .unwrap();

    sleep(Duration::from_millis(150)).await;

    let resp_a = http_get(&format!("http://127.0.0.1:{port_a}/")).await;
    let resp_b = http_get(&format!("http://127.0.0.1:{port_b}/")).await;

    assert_eq!(resp_a.body, b"route-A");
    assert_eq!(resp_b.body, b"route-B");

    shutdown_daemon(client, child).await;
}

/// Test 7: restarting the daemon restores the same stable port for the
/// same key (port allocator JSON persistence).
#[serial]
#[tokio::test]
async fn restart_returns_same_port() {
    let (upstream_listener, upstream_addr) = free_listener().await;
    tokio::spawn(async move {
        while let Ok((stream, _)) = upstream_listener.accept().await {
            let io = TokioIo::new(stream);
            tokio::spawn(hyper::server::conn::http1::Builder::new().serve_connection(
                io,
                service_fn(|_req: Request<Incoming>| async {
                    Ok::<_, hyper::Error>(http::Response::new(Full::new(Bytes::from("ok"))))
                }),
            ));
        }
    });

    let ato_home = TempDir::new().unwrap();

    // First daemon.
    let (child1, socket_path) = spawn_daemon(&ato_home);
    let mut client1 = wait_for_daemon(&socket_path, 3000).await;
    let IngressInfo { port: port1 } = client1
        .register_ingress("persistent-key", &format!("http://{upstream_addr}"))
        .await
        .unwrap();
    shutdown_daemon(client1, child1).await;

    // Allow the socket file to be cleaned up before the next daemon starts.
    sleep(Duration::from_millis(200)).await;

    // Second daemon with same ATO_HOME.
    let (child2, socket_path2) = spawn_daemon(&ato_home);
    let mut client2 = wait_for_daemon(&socket_path2, 3000).await;
    let IngressInfo { port: port2 } = client2
        .register_ingress("persistent-key", &format!("http://{upstream_addr}"))
        .await
        .unwrap();
    shutdown_daemon(client2, child2).await;

    assert_eq!(
        port1, port2,
        "stable port must persist across daemon restart"
    );
}

/// Test 8: re-registering the same key with a different upstream URL
/// keeps the same stable port but routes new connections to the new upstream.
#[serial]
#[tokio::test]
async fn reregister_same_key_updates_upstream() {
    let (listener_v1, addr_v1) = free_listener().await;
    let (listener_v2, addr_v2) = free_listener().await;

    for (listener, label) in [(listener_v1, "v1"), (listener_v2, "v2")] {
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let io = TokioIo::new(stream);
                let body = Full::new(Bytes::from(label));
                tokio::spawn(hyper::server::conn::http1::Builder::new().serve_connection(
                    io,
                    service_fn(move |_req: Request<Incoming>| {
                        let body = body.clone();
                        async move { Ok::<_, hyper::Error>(http::Response::new(body)) }
                    }),
                ));
            }
        });
    }

    let ato_home = TempDir::new().unwrap();
    let (child, socket_path) = spawn_daemon(&ato_home);
    let mut client = wait_for_daemon(&socket_path, 3000).await;

    let IngressInfo { port: port1 } = client
        .register_ingress("swap-key", &format!("http://{addr_v1}"))
        .await
        .unwrap();

    sleep(Duration::from_millis(150)).await;
    let r1 = http_get(&format!("http://127.0.0.1:{port1}/")).await;
    assert_eq!(r1.body, b"v1");

    // Swap upstream.
    let IngressInfo { port: port2 } = client
        .register_ingress("swap-key", &format!("http://{addr_v2}"))
        .await
        .unwrap();
    assert_eq!(port1, port2, "same key must return same port after swap");

    sleep(Duration::from_millis(150)).await;
    let r2 = http_get(&format!("http://127.0.0.1:{port2}/")).await;
    assert_eq!(
        r2.body, b"v2",
        "new upstream must be active after re-register"
    );

    shutdown_daemon(client, child).await;
}

/// Test 9: deregister stops the listener but keeps the port reserved in the
/// persistent allocator. Re-registering the same key within the same daemon
/// session must return the identical port, preserving WebView origin and
/// browser storage (IndexedDB, localStorage, Service Workers) across
/// session stop/restart cycles.
#[serial]
#[tokio::test]
async fn deregister_keeps_port_in_allocator() {
    let (upstream_listener, upstream_addr) = free_listener().await;
    tokio::spawn(async move {
        while let Ok((stream, _)) = upstream_listener.accept().await {
            let io = TokioIo::new(stream);
            tokio::spawn(hyper::server::conn::http1::Builder::new().serve_connection(
                io,
                service_fn(|_req: Request<Incoming>| async {
                    Ok::<_, hyper::Error>(http::Response::new(Full::new(Bytes::from("ok"))))
                }),
            ));
        }
    });

    let ato_home = TempDir::new().unwrap();
    let (child, socket_path) = spawn_daemon(&ato_home);
    let mut client = wait_for_daemon(&socket_path, 3000).await;

    // First registration — gets initial port.
    let IngressInfo { port: port1 } = client
        .register_ingress("session-key", &format!("http://{upstream_addr}"))
        .await
        .unwrap();

    // Deregister simulates session stop.
    client.deregister_ingress("session-key").await.unwrap();

    // Brief pause to let the listener fully drain.
    sleep(Duration::from_millis(200)).await;

    // Re-register simulates session restart — must return the same port.
    let IngressInfo { port: port2 } = client
        .register_ingress("session-key", &format!("http://{upstream_addr}"))
        .await
        .unwrap();

    assert_eq!(
        port1, port2,
        "deregister must not remove the port from the allocator; \
         re-registering the same key must yield the same stable port"
    );

    // Verify the route is actually serving traffic after re-registration.
    sleep(Duration::from_millis(150)).await;
    let resp = http_get(&format!("http://127.0.0.1:{port2}/")).await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"ok");

    shutdown_daemon(client, child).await;
}

/// Test 10: when the upstream is not listening (ConnectionRefused), the ingress
/// proxy returns 503 Service Unavailable with an HTML auto-refresh page instead
/// of a bare "HTTP proxy error" body.  This lets the browser keep retrying
/// automatically while a slow service (Docker container, JVM startup) boots.
#[serial]
#[tokio::test]
async fn upstream_not_ready_returns_service_starting_page() {
    // Bind a port and immediately drop the listener — nothing will accept on it.
    let (listener, deaf_addr) = free_listener().await;
    drop(listener);

    let ato_home = TempDir::new().unwrap();
    let (child, socket_path) = spawn_daemon(&ato_home);
    let mut client = wait_for_daemon(&socket_path, 3000).await;

    let IngressInfo { port } = client
        .register_ingress("not-ready", &format!("http://{deaf_addr}"))
        .await
        .unwrap();

    sleep(Duration::from_millis(150)).await;
    let resp = http_get(&format!("http://127.0.0.1:{port}/")).await;

    assert_eq!(
        resp.status, 503,
        "upstream not ready must yield 503, not 502"
    );
    let body = String::from_utf8_lossy(&resp.body);
    assert!(
        body.contains("Service is starting"),
        "body must contain user-friendly 'Service is starting' message; got: {body:.200}"
    );
    assert!(
        body.contains("http-equiv=\"refresh\""),
        "body must contain meta auto-refresh; got: {body:.200}"
    );

    shutdown_daemon(client, child).await;
}

/// Regression: deregistering a route while a guest still holds an idle
/// keep-alive connection open must return promptly.
///
/// `IngressManager::deregister` previously awaited every in-flight proxy task
/// unconditionally. A keep-alive connection (exactly what a guest WebView
/// leaves behind when its window closes) parks in hyper's idle state and never
/// completes on its own, so the DeregisterIngress reply blocked for the full
/// connection lifetime — surfacing as a ~57s freeze of the Desktop UI thread
/// that issued the synchronous deregister from `AppCapsuleShell::Drop`.
///
/// Teardown now drains with a bounded grace period and then aborts the
/// stragglers, so deregister must complete in well under that. Before the fix
/// this test hangs until the harness timeout.
#[serial]
#[tokio::test]
async fn deregister_is_prompt_with_idle_keepalive_connection() {
    // Upstream that answers one request and keeps the connection alive, so the
    // proxied client connection parks in hyper's keep-alive idle state.
    let (upstream_listener, upstream_addr) = free_listener().await;
    tokio::spawn(async move {
        while let Ok((stream, _)) = upstream_listener.accept().await {
            let io = TokioIo::new(stream);
            tokio::spawn(hyper::server::conn::http1::Builder::new().serve_connection(
                io,
                service_fn(|_req: Request<Incoming>| async {
                    Ok::<_, hyper::Error>(
                        http::Response::builder()
                            .status(200)
                            .header("content-type", "text/plain")
                            .body(Full::new(Bytes::from("ok")))
                            .unwrap(),
                    )
                }),
            ));
        }
    });

    let ato_home = TempDir::new().unwrap();
    let (child, socket_path) = spawn_daemon(&ato_home);
    let mut client = wait_for_daemon(&socket_path, 3000).await;

    let IngressInfo { port } = client
        .register_ingress("test-idle-keepalive", &format!("http://{upstream_addr}"))
        .await
        .expect("register_ingress");
    sleep(Duration::from_millis(150)).await;

    // Open a raw keep-alive connection through the ingress, complete one
    // request, then deliberately hold the socket open and idle.
    let mut conn = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .expect("connect to ingress");
    conn.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: keep-alive\r\n\r\n")
        .await
        .expect("write request");
    let mut buf = vec![0u8; 1024];
    let n = conn.read(&mut buf).await.expect("read response");
    assert!(n > 0, "expected a proxied response from the ingress");
    // `conn` is intentionally NOT dropped before deregister — it stays parked
    // in the proxy's keep-alive idle state, reproducing the closing WebView.

    let started = tokio::time::Instant::now();
    client
        .deregister_ingress("test-idle-keepalive")
        .await
        .expect("deregister_ingress");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(5),
        "deregister blocked on an idle keep-alive connection ({elapsed:?}); \
         the bounded-drain teardown regressed"
    );

    drop(conn);
    shutdown_daemon(client, child).await;
}
