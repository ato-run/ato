//! Integration tests for Slice E (#300) — HTTP CONNECT egress proxy.
//!
//! Each test spawns a real `ato-netd` subprocess, reads the
//! `egress_proxy_port` from the `Status` report, and drives the proxy
//! with raw TCP CONNECT requests.  No `ato-netd` crate internals are
//! imported; the only ato crate used is `netd::net::control::Client`.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use netd::net::control::{Client, Error as ControlError};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};

fn netd_binary() -> &'static str {
    env!("CARGO_BIN_EXE_ato-netd")
}

struct TestEnv {
    _home: TempDir,
    home_path: PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        let home = TempDir::new().expect("create ATO_HOME tempdir");
        let home_path = home.path().to_path_buf();
        Self {
            _home: home,
            home_path,
        }
    }

    fn socket(&self) -> PathBuf {
        self.home_path.join("run/netd.sock")
    }

    async fn spawn_daemon(&self) -> DaemonGuard {
        let mut cmd = Command::new(netd_binary());
        cmd.env("ATO_HOME", &self.home_path);
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);
        let child = cmd.spawn().expect("spawn ato-netd");
        let guard = DaemonGuard { child: Some(child) };
        wait_for_socket(&self.socket(), Duration::from_secs(5))
            .await
            .expect("daemon control socket");
        guard
    }
}

struct DaemonGuard {
    child: Option<Child>,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
    }
}

async fn wait_for_socket(path: &Path, timeout: Duration) -> Result<(), &'static str> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        match Client::connect(path).await {
            Ok(_) => return Ok(()),
            Err(ControlError::NotRunning { .. }) | Err(_) => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
    }
    Err("control socket did not become reachable")
}

/// Spin up a local TCP echo server that echoes every byte back.
/// Returns the listening port.
async fn spawn_echo_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let (mut r, mut w) = stream.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    port
}

/// Send an HTTP CONNECT request and return the status code.
async fn send_connect(proxy_port: u16, authority: &str) -> (u16, TcpStream) {
    let mut stream = TcpStream::connect(("127.0.0.1", proxy_port))
        .await
        .expect("connect to egress proxy");
    let req = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n");
    stream.write_all(req.as_bytes()).await.unwrap();

    let mut response = String::new();
    let mut buf = vec![0u8; 256];
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

// ── Tests ─────────────────────────────────────────────────────────────────────

/// The `status` report includes `egress_proxy_port` once the daemon is up.
#[tokio::test]
async fn status_includes_egress_proxy_port() {
    let env = TestEnv::new();
    let _daemon = env.spawn_daemon().await;

    let mut client = Client::connect(&env.socket()).await.unwrap();
    let report = client.status().await.unwrap();

    assert!(
        report.egress_proxy_port.is_some(),
        "egress_proxy_port must be present in status report, got: {:?}",
        report.egress_proxy_port
    );
    let port = report.egress_proxy_port.unwrap();
    assert!(port > 0, "egress_proxy_port must be non-zero");
}

/// Happy-path CONNECT: 200 response, then data flows through the proxy
/// to a local echo server.
#[tokio::test]
async fn connect_happy_path() {
    let env = TestEnv::new();
    let _daemon = env.spawn_daemon().await;

    let echo_port = spawn_echo_server().await;
    let authority = format!("127.0.0.1:{echo_port}");

    let mut client = Client::connect(&env.socket()).await.unwrap();
    let report = client.status().await.unwrap();
    let proxy_port = report.egress_proxy_port.expect("egress_proxy_port");

    let (status, mut stream) = send_connect(proxy_port, &authority).await;
    assert_eq!(
        status, 200,
        "CONNECT should return 200 Connection established"
    );

    // Send some data through the tunnel — the echo server sends it back.
    let payload = b"hello egress proxy";
    stream.write_all(payload).await.unwrap();

    let mut echo_buf = vec![0u8; payload.len()];
    stream.read_exact(&mut echo_buf).await.unwrap();
    assert_eq!(echo_buf, payload, "echoed data must match sent payload");
}

/// 50 concurrent CONNECT tunnels — no cross-talk, all succeed.
#[tokio::test]
async fn concurrent_50_connects() {
    let env = TestEnv::new();
    let _daemon = env.spawn_daemon().await;

    let echo_port = spawn_echo_server().await;
    let authority = format!("127.0.0.1:{echo_port}");

    let mut client = Client::connect(&env.socket()).await.unwrap();
    let report = client.status().await.unwrap();
    let proxy_port = report.egress_proxy_port.expect("egress_proxy_port");

    let authority = std::sync::Arc::new(authority);
    let mut handles = Vec::with_capacity(50);

    for i in 0u8..50 {
        let auth = authority.clone();
        handles.push(tokio::spawn(async move {
            let (status, mut stream) = send_connect(proxy_port, &auth).await;
            assert_eq!(status, 200, "connection {i}: expected 200");

            // Send a unique payload per connection to detect cross-talk.
            let payload = vec![i; 16];
            stream.write_all(&payload).await.unwrap();
            let mut echo_buf = vec![0u8; 16];
            stream.read_exact(&mut echo_buf).await.unwrap();
            assert_eq!(
                echo_buf, payload,
                "connection {i}: echo mismatch (cross-talk?)"
            );
        }));
    }

    for handle in handles {
        handle.await.expect("concurrent connect task panicked");
    }
}
