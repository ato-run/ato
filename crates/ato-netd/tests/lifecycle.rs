//! Lifecycle smoke tests for slice **A** (#296).
//!
//! Per the acceptance criteria: exercise start, double-start (typed
//! error), clean stop, `--status` against a running daemon, and
//! `--status` without a daemon — all through `ato_netd::net::control::Client`
//! (no reaching into `ato-netd` internals). This pins the API boundary
//! from day one so follow-up slices can add verbs without re-shaping
//! the consumer surface.
//!
//! Each test gets its own `ATO_HOME` tempdir so they cannot collide on
//! the canonical control socket path.
//!
//! Unix-only in slice A. The whole file is gated behind `#[cfg(unix)]`
//! because the `ato_netd::net::control::Client` it drives is Unix-only —
//! when slice A's TCP fallback (tracked under #294) lands, these tests
//! can be split into transport-specific variants.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use ato_netd::net::control::{Client, Error as ControlError};
use tempfile::TempDir;
use tokio::process::{Child, Command};

/// Path to the compiled `ato-netd` binary cargo built for this test
/// binary. Cargo sets `CARGO_BIN_EXE_<bin>` for integration tests in
/// the same package, so we don't have to guess the target directory.
fn netd_binary() -> &'static str {
    env!("CARGO_BIN_EXE_ato-netd")
}

/// Per-test isolation: a fresh `ATO_HOME` tempdir + the canonical
/// socket path inside it (`<tempdir>/run/netd.sock`).
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

    /// Spawn `ato-netd` as a child daemon under this env. The returned
    /// `Child` must be killed by the test to clean up; we wrap it in
    /// `DaemonGuard` below.
    async fn spawn_daemon(&self) -> DaemonGuard {
        let mut cmd = Command::new(netd_binary());
        cmd.env("ATO_HOME", &self.home_path);
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);
        let child = cmd.spawn().expect("spawn ato-netd");
        let guard = DaemonGuard { child: Some(child) };
        // Poll the control socket until the daemon accepts. Bounded
        // wait so a hung daemon fails the test instead of the suite.
        wait_for_socket(&self.socket(), Duration::from_secs(5))
            .await
            .expect("daemon to bring up the control socket");
        guard
    }

    /// Spawn `ato-netd --status` as a one-shot client. Returns
    /// `(exit_status, stdout, stderr)`.
    async fn run_status_client(&self) -> (Option<i32>, String, String) {
        let output = Command::new(netd_binary())
            .arg("--status")
            .env("ATO_HOME", &self.home_path)
            .output()
            .await
            .expect("spawn --status client");
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

struct DaemonGuard {
    child: Option<Child>,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Best-effort; the daemon may already be down from
            // a successful `shutdown`. `kill_on_drop` already covers
            // the panic path.
            let _ = child.start_kill();
        }
    }
}

async fn wait_for_socket(path: &Path, timeout: Duration) -> Result<(), &'static str> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        match Client::connect(path).await {
            Ok(_) => return Ok(()),
            Err(ControlError::NotRunning { .. }) => {
                tokio::time::sleep(Duration::from_millis(25)).await;
                continue;
            }
            Err(_) => {
                // Transient I/O during socket bringup — keep polling.
                tokio::time::sleep(Duration::from_millis(25)).await;
                continue;
            }
        }
    }
    Err("control socket did not become reachable")
}

#[tokio::test]
async fn status_without_daemon_returns_not_running() {
    let env = TestEnv::new();
    // No daemon spawned. Client should report NotRunning at the typed
    // level, so consumers can branch without parsing kernel error
    // codes.
    let result = Client::connect(&env.socket()).await;
    assert!(
        matches!(result, Err(ControlError::NotRunning { .. })),
        "expected NotRunning, got: {result:?}"
    );

    // The `--status` CLI surface should mirror that with the
    // documented `{"status":"not_running"}` envelope + exit 3.
    let (exit, stdout, _stderr) = env.run_status_client().await;
    assert_eq!(exit, Some(3), "status without daemon should exit 3");
    assert!(
        stdout.contains("not_running"),
        "expected not_running envelope, got: {stdout}"
    );
}

#[tokio::test]
async fn status_against_running_daemon_returns_report() {
    let env = TestEnv::new();
    let _daemon = env.spawn_daemon().await;

    let mut client = Client::connect(&env.socket())
        .await
        .expect("connect to running daemon");
    let report = client.status().await.expect("status request");
    assert!(!report.version.is_empty());
    assert!(report.pid > 0);
    // Slice A doesn't register any listeners. The field exists so
    // slice B can populate it without a wire-format break.
    assert!(report.listeners.is_empty());
}

#[tokio::test]
async fn double_start_surfaces_typed_already_running_error() {
    let env = TestEnv::new();
    let _daemon = env.spawn_daemon().await;

    // Second daemon spawn should observe AlreadyRunning at the
    // socket-bind probe and exit 4. We want a typed signal that a
    // wrapper (or a future `ensure_running` helper) can branch on.
    let output = Command::new(netd_binary())
        .env("ATO_HOME", &env.home_path)
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .output()
        .await
        .expect("spawn second daemon");
    assert_eq!(
        output.status.code(),
        Some(4),
        "second daemon should exit 4 (AlreadyRunning); stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already running"),
        "stderr should mention AlreadyRunning; got: {stderr}"
    );
}

#[tokio::test]
async fn shutdown_via_client_stops_daemon_cleanly() {
    let env = TestEnv::new();
    let mut daemon = env.spawn_daemon().await;

    // Confirm the daemon is reachable before we ask it to stop.
    {
        let mut client = Client::connect(&env.socket())
            .await
            .expect("connect before shutdown");
        client.status().await.expect("pre-shutdown status");
    }

    // Send shutdown via the typed Client API. The shutdown verb
    // is acked by the daemon BEFORE it tears down the listener, so
    // `client.shutdown()` itself should succeed.
    let client = Client::connect(&env.socket())
        .await
        .expect("connect for shutdown");
    client.shutdown().await.expect("shutdown ack");

    // Wait for the child process to exit. Bounded wait so a stuck
    // daemon fails the test rather than the suite.
    let mut child = daemon.child.take().expect("daemon child handle");
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .expect("daemon should exit within 5s of shutdown ack")
        .expect("wait on daemon process");
    assert!(
        status.success(),
        "daemon should exit cleanly after shutdown; got: {status:?}"
    );

    // After shutdown, --status should report not_running again. The
    // socket file may persist briefly (Drop is best-effort), so we
    // tolerate both "file absent" and "file present but refusing":
    // the typed Client::connect collapses both into NotRunning.
    let result = Client::connect(&env.socket()).await;
    assert!(
        matches!(result, Err(ControlError::NotRunning { .. })),
        "post-shutdown connect should be NotRunning, got: {result:?}"
    );
}

#[tokio::test]
async fn stale_socket_file_is_unlinked_and_replaced_on_start() {
    let env = TestEnv::new();
    // Simulate a SIGKILL'd previous daemon: create the parent dir and
    // a leftover socket-shaped file at the canonical path. The next
    // start should probe, decide it's stale (no live daemon answering),
    // unlink, and bind successfully.
    tokio::fs::create_dir_all(env.socket().parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(env.socket(), b"stale").await.unwrap();

    let _daemon = env.spawn_daemon().await;
    let mut client = Client::connect(&env.socket())
        .await
        .expect("connect after stale-socket cleanup");
    let report = client.status().await.expect("status after recovery");
    assert!(report.pid > 0);
}
