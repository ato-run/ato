//! End-to-end regression for the Desktop installed-relaunch **port path**
//! (`ato launch <ipk> --detached-session`, the exact command ato-desktop spawns).
//!
//! The hermetic Desktop MCP smoke (`docs/aodd/relaunch_smoke_mcp.py`) stops the
//! first runtime before relaunching, so its relaunch reuses the declared port and
//! never exercises the *remap*. This test pins that gap: with the declared port
//! already occupied, the detached relaunch must
//!
//!   1. choose a *different* free port (remap), and
//!   2. inject it as `$PORT` so the runtime binds the resolved port instead of
//!      its hard-coded fallback — i.e. it "injects PORT rather than relying on
//!      18890". (If injection were broken the runtime would try the occupied
//!      declared port, fail readiness, and `ato launch` would exit non-zero with
//!      no session record — so a healthy record on a remapped port *is* the
//!      proof.)
//!   3. `ato app session stop` then tears the detached runtime down, leaving no
//!      process listening on the resolved port (no orphan → no AddrInUse on a
//!      subsequent relaunch).
//!
//! Marked `#[ignore]` because it spawns a real `source`/`node` runtime via the
//! managed toolchain (the repo convention for toolchain-dependent e2e tests, cf.
//! the electron/wails cases in `ato_desktop_session_e2e.rs`). Run locally with:
//!
//! ```sh
//! cargo test -p ato-cli --test installed_relaunch_port_remap_e2e -- --ignored --nocapture
//! ```
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use serial_test::serial;

/// Unroutable Store/GitHub base: any accidental remote call fails fast instead of
/// reaching the real services. `--from-local` must not touch them.
const UNROUTABLE_API: &str = "http://127.0.0.1:1";
/// `basic-web`'s declared port (see the fixture's `capsule.toml`).
const DECLARED_PORT: u16 = 18890;
const MARKER: &str = "Ato local-install basic-web fixture";

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn basic_web_fixture() -> PathBuf {
    workspace_root()
        .join("tests")
        .join("fixtures")
        .join("local-install")
        .join("basic-web")
}

/// Best-effort SIGKILL of a detached runtime PID on drop, so a panicking
/// assertion never leaks the spawned process onto the test host.
struct RuntimeReaper(Option<i32>);

impl RuntimeReaper {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for RuntimeReaper {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let _ = StdCommand::new("kill")
                .arg("-9")
                .arg(pid.to_string())
                .status();
        }
    }
}

fn ato() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin!("ato"))
}

fn hermetic_env(cmd: &mut Command, ato_home: &Path, home: &Path) {
    cmd.env("ATO_HOME", ato_home)
        .env("HOME", home)
        .env("ATO_STORE_API_URL", UNROUTABLE_API)
        .env("ATO_GITHUB_API_BASE_URL", UNROUTABLE_API)
        .env("ATO_TELEMETRY", "0");
}

fn parse_install_ipk(stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(stdout);
    for (index, _) in text.match_indices('{').rev() {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text[index..]) {
            if let Some(ipk) = value
                .get("install_lifecycle")
                .and_then(|l| l.get("install_profile_key"))
                .and_then(|v| v.as_str())
            {
                return ipk.to_string();
            }
        }
    }
    panic!("install_profile_key not found in install stdout:\n{text}");
}

/// Read the detached session record stamped with `ipk` that actually serves on a
/// live upstream. Returns `(session_id, resolved_port, pid)`.
fn live_session_for_ipk(ato_home: &Path, ipk: &str) -> Option<(String, u16, i32)> {
    let dir = ato_home.join("apps").join("ato-desktop").join("sessions");
    let entries = std::fs::read_dir(&dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if value.get("install_profile_key").and_then(|v| v.as_str()) != Some(ipk) {
            continue;
        }
        let session_id = value
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let pid = value.get("pid").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
        let url = value
            .get("web")
            .and_then(|w| w.get("local_url").or_else(|| w.get("healthcheck_url")))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if let Some(port) = port_of(url) {
            return Some((session_id, port, pid));
        }
    }
    None
}

fn port_of(url: &str) -> Option<u16> {
    // http://127.0.0.1:<port>/...
    let after = url.split("://").nth(1)?;
    let hostport = after.split('/').next()?;
    hostport.rsplit(':').next()?.parse().ok()
}

/// Minimal HTTP/1.0 GET; returns the response body or "" on any failure.
fn http_get(port: u16, path: &str) -> String {
    let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) else {
        return String::new();
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let req = format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    if stream.write_all(req.as_bytes()).is_err() {
        return String::new();
    }
    let mut body = String::new();
    let _ = stream.read_to_string(&mut body);
    body
}

fn port_is_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

#[test]
#[serial]
#[ignore = "spawns a real source/node runtime via the managed toolchain; run with --ignored"]
fn installed_relaunch_remaps_and_injects_port_when_declared_port_busy() {
    let scratch = tempfile::tempdir().expect("temp scratch");
    let ato_home = scratch.path().join("ato-home");
    let home = scratch.path().join("home");
    let store = scratch.path().join("store");
    std::fs::create_dir_all(&ato_home).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    // 1. Install the fixture from local (no network).
    let mut install = ato();
    hermetic_env(&mut install, &ato_home, &home);
    install
        .args(["install", "--from-local"])
        .arg(basic_web_fixture())
        .arg("--output")
        .arg(&store)
        .args(["--no-project", "--json"]);
    let install_out = install.output().expect("run ato install --from-local");
    assert!(
        install_out.status.success(),
        "install failed\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&install_out.stdout),
        String::from_utf8_lossy(&install_out.stderr)
    );
    let ipk = parse_install_ipk(&install_out.stdout);

    // 2. Occupy the declared port so the relaunch is forced to remap. Held for
    //    the whole launch so `ato`'s availability probe sees it as busy.
    let occupier = TcpListener::bind(("127.0.0.1", DECLARED_PORT))
        .expect("bind declared port to force a remap");

    // 3. Detached relaunch — the exact command ato-desktop spawns.
    let mut launch = ato();
    hermetic_env(&mut launch, &ato_home, &home);
    launch.args(["launch", &ipk, "-y", "--detached-session"]);
    launch.timeout(Duration::from_secs(180));
    let launch_out = launch.output().expect("run ato launch --detached-session");
    assert!(
        launch_out.status.success(),
        "detached relaunch must succeed (a broken PORT injection would fail \
         readiness on the busy declared port and exit non-zero)\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&launch_out.stdout),
        String::from_utf8_lossy(&launch_out.stderr)
    );

    let (session_id, resolved_port, pid) = live_session_for_ipk(&ato_home, &ipk)
        .expect("detached relaunch must write a discoverable session record");
    let mut reaper = RuntimeReaper(Some(pid));

    // 4a. Remap: the resolved port must differ from the occupied declared port.
    assert_ne!(
        resolved_port, DECLARED_PORT,
        "relaunch must remap off the occupied declared port {DECLARED_PORT}, got {resolved_port}"
    );

    // 4b. PORT injection: the runtime must actually serve the marker on the
    //     *resolved* port (proving $PORT was injected, not the 18890 fallback).
    let mut served = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let body = http_get(resolved_port, "/health");
        if body.contains(MARKER) {
            served = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        served,
        "runtime must serve the marker on the resolved port {resolved_port} \
         (proves $PORT injection); declared {DECLARED_PORT} is still held by the test"
    );

    // 5. Stop the detached runtime; the resolved port must be released (no orphan).
    let mut stop = ato();
    hermetic_env(&mut stop, &ato_home, &home);
    stop.args(["app", "session", "stop", &session_id, "--json"]);
    let stop_out = stop.output().expect("run ato app session stop");
    assert!(
        stop_out.status.success(),
        "stop must succeed for a Desktop-spawned detached session\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&stop_out.stdout),
        String::from_utf8_lossy(&stop_out.stderr)
    );

    let freed_deadline = Instant::now() + Duration::from_secs(10);
    let mut freed = false;
    while Instant::now() < freed_deadline {
        if port_is_free(resolved_port) {
            freed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    reaper.disarm(); // stop succeeded; nothing left to reap
    drop(occupier);
    assert!(
        freed,
        "resolved port {resolved_port} must be free after stop (no orphan runtime)"
    );
}
