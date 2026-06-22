//! Regression test for ato-run/ato#192 (KOH-21).
//!
//! Verifies that a `source/python` target with `runtime_tools = { node = "20" }`
//! and a target-level `build` command behaves correctly end-to-end:
//!
//! 1. Node 20 is provisioned via `runtime_tools` (not the host's Node).
//! 2. The declared `build` command (`npm install && npm run build`) runs during
//!    the build phase.
//! 3. `dist/index.html` is produced by the Node build.
//! 4. The Python server starts and serves the built frontend at `/` (HTTP 200,
//!    body contains fixture marker).
//! 5. `/assets/bundle.js` is served (HTTP 200) — confirms static asset routing.
//! 6. The `/api/health` backend endpoint returns HTTP 200.
//!
//! The fixture uses `--dangerously-skip-permissions` so the test works without a
//! native sandbox and without pre-seeding execution-plan consent.  The Python
//! server auto-shuts down after 30 s so `ato run` exits on its own.
//!
//! Prerequisites:
//! - `uv` on PATH — required for Python provisioning (`uv venv`).
//! - Network access for first-run Node 20 toolchain download (~30-60 s).
//!
//! Skip behaviour:
//! - `ATO_STRICT_CI=1`: **never skip** — any prerequisite failure is a test failure.
//! - Default: skip gracefully when Node 20 cannot be downloaded (network
//!   unavailable signal in ato output).  Any other failure is still a test
//!   failure to avoid masking regressions.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serial_test::serial;

// ─── helpers ──────────────────────────────────────────────────────────────────

fn strict_ci() -> bool {
    std::env::var("ATO_STRICT_CI")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("source-python-runtime-tools")
}

fn test_root() -> PathBuf {
    let root = std::env::current_dir()
        .expect("cwd")
        .join(".ato")
        .join("test-scratch")
        .join("source-python-runtime-tools-e2e")
        .join(format!("{:016x}", rand::random::<u64>()));
    fs::create_dir_all(&root).expect("create test root");
    root
}

fn copy_fixture(dst: &Path) {
    copy_dir(&fixture_dir(), dst);
}

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create dst");
    for entry in walkdir::WalkDir::new(src).min_depth(1) {
        let entry = entry.expect("walkdir entry");
        let rel = entry.path().strip_prefix(src).expect("strip prefix");
        let dest = dst.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&dest).expect("create subdir");
        } else {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).expect("create parent");
            }
            fs::copy(entry.path(), &dest).expect("copy file");
        }
    }
}

fn reserve_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind :0");
    listener.local_addr().expect("local_addr").port()
    // listener drops here, releasing the port; brief TOCTOU is acceptable in tests
}

/// Poll `addr` until a TCP connection succeeds or `timeout` elapses.
fn wait_for_tcp(addr: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect(addr).is_ok() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

struct HttpResponse {
    status: u16,
    body: String,
}

/// Issue a bare-bones HTTP/1.0 GET and return status + body.
fn http_get(addr: &str, path: &str) -> Option<HttpResponse> {
    let mut stream = TcpStream::connect(addr).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let req = format!("GET {path} HTTP/1.0\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).ok()?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).ok()?;
    let raw = String::from_utf8_lossy(&buf);

    // Split headers from body on the blank line.
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let status_str = head.lines().next()?.split_whitespace().nth(1)?;
    Some(HttpResponse {
        status: status_str.parse().ok()?,
        body: body.to_owned(),
    })
}

// ─── cleanup ──────────────────────────────────────────────────────────────────

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// ─── test ─────────────────────────────────────────────────────────────────────

/// End-to-end regression for #192: source/python + runtime_tools + build lifecycle.
///
/// Requires `uv` on PATH and network for the first Node 20 download.
///
/// Skip policy:
/// - Non-strict CI: skip if and only if Node 20 toolchain download fails
///   (specific network-unavailable signal in ato output).
/// - `ATO_STRICT_CI=1`: **never skip** — any failure is a real failure.
#[test]
#[serial]
#[cfg(unix)]
fn source_python_runtime_tools_build_and_serve() {
    // ── hard prerequisite: uv must be on PATH ────────────────────────────────
    if which::which("uv").is_err() {
        assert!(
            !strict_ci(),
            "strict CI requires `uv` on PATH for source-python-runtime-tools e2e"
        );
        return;
    }

    // ── workspace setup ──────────────────────────────────────────────────────
    let root = test_root();
    let _cleanup = Cleanup(root.clone());

    let home = root.join("home");
    let workspace = root.join("workspace");
    fs::create_dir_all(&home).expect("create home");
    copy_fixture(&workspace);

    // ── reserve a free port; inject it so the Python server binds there ───────
    let port = reserve_free_port();
    let addr = format!("127.0.0.1:{port}");

    // ── log files (populated while ato is running) ────────────────────────────
    let stdout_log = root.join("ato-stdout.log");
    let stderr_log = root.join("ato-stderr.log");
    let stdout_file = fs::File::create(&stdout_log).expect("create stdout log");
    let stderr_file = fs::File::create(&stderr_log).expect("create stderr log");

    // ── spawn `ato run . --yes --dangerously-skip-permissions` ────────────────
    // `--dangerously-skip-permissions` bypasses E301 (sandbox opt-in) and E302
    // (execution-plan consent), running the Python server via execute_host.
    // `ATO_UI_OVERRIDE_PORT` injects the dynamically reserved port so the
    // Python server binds exactly where we poll.
    // `CAPSULE_ALLOW_UNSAFE=1` is the env-var counterpart of the flag.
    let mut child = Command::new(env!("CARGO_BIN_EXE_ato"))
        .args(["run", ".", "--yes", "--dangerously-skip-permissions"])
        .current_dir(&workspace)
        .env("HOME", &home)
        .env("CAPSULE_ALLOW_UNSAFE", "1")
        .env("ATO_UI_OVERRIDE_PORT", port.to_string())
        .stdout(stdout_file)
        .stderr(stderr_file)
        .spawn()
        .expect("spawn ato run");

    // ── wait for the Python server to come up ─────────────────────────────────
    // Cold run: Node 20 download + uv venv + npm install + npm run build.
    // Allow up to 5 minutes; warm run (Node cached) is typically under 30 s.
    let server_ready = wait_for_tcp(&addr, Duration::from_secs(300));

    let stderr_content = fs::read_to_string(&stderr_log).unwrap_or_default();
    let stdout_content = fs::read_to_string(&stdout_log).unwrap_or_default();

    if !server_ready {
        let _ = child.kill();
        let _ = child.wait();

        // Narrow skip: only skip in non-strict CI when the ato output clearly
        // signals a Node toolchain download failure (network unavailable).
        // Any other failure (including runtime_tools not being applied at all)
        // must surface as a real test failure so the regression is not masked.
        let is_node_download_failure = stderr_content
            .contains("managed node runtime is unavailable")
            || stderr_content.contains("failed to download")
            || stderr_content.contains("toolchain download");
        if !strict_ci() && is_node_download_failure {
            eprintln!(
                "[source_python_runtime_tools_e2e] skipping: Node 20 toolchain \
                 download unavailable\n{stderr_content}"
            );
            return;
        }

        panic!(
            "server on {addr} did not become ready within 5 minutes\n\
             stdout:\n{stdout_content}\nstderr:\n{stderr_content}"
        );
    }

    // ── HTTP probes ───────────────────────────────────────────────────────────
    let root_resp = http_get(&addr, "/");
    let asset_resp = http_get(&addr, "/assets/bundle.js");
    let api_resp = http_get(&addr, "/api/health");

    // ── wait for ato to finish (server auto-shuts down after 30 s) ───────────
    let exit_deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => break,
            None => {
                if Instant::now() >= exit_deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }

    // ── final log snapshot ────────────────────────────────────────────────────
    let stderr_final = fs::read_to_string(&stderr_log).unwrap_or_default();
    let stdout_final = fs::read_to_string(&stdout_log).unwrap_or_default();

    // ── assertions ────────────────────────────────────────────────────────────

    // (1) Build lifecycle ran: the 🏗️ header appears in ato output.
    assert!(
        stdout_final.contains("Build [main]") || stderr_final.contains("Build [main]"),
        "expected 'Build [main]' in ato output — build lifecycle did not run\n\
         stdout:\n{stdout_final}\nstderr:\n{stderr_final}"
    );

    // (2) dist/index.html was produced by the Node build.
    assert!(
        workspace.join("dist/index.html").exists(),
        "dist/index.html not found — npm run build may not have run"
    );
    let index_html =
        fs::read_to_string(workspace.join("dist/index.html")).expect("read dist/index.html");
    assert!(
        index_html.contains("DOCTYPE") || index_html.contains("html"),
        "dist/index.html does not look like HTML: {index_html:?}"
    );

    // (3) Node 20 was provisioned via runtime_tools (managed toolchain dir created).
    let node_toolchain_root = home.join(".ato").join("toolchains").join("node-20");
    assert!(
        node_toolchain_root.exists(),
        "managed Node 20 toolchain not found at {} — runtime_tools was not applied",
        node_toolchain_root.display()
    );

    // (4) GET / → HTTP 200 + body contains fixture marker (confirms built frontend served).
    let root_resp = root_resp.expect("HTTP GET / should succeed (server was ready on TCP)");
    assert_eq!(
        root_resp.status, 200,
        "GET / expected HTTP 200, got {}\nstderr:\n{stderr_final}",
        root_resp.status
    );
    assert!(
        root_resp
            .body
            .contains("source-python-runtime-tools-fixture")
            || root_resp.body.contains("/assets/bundle.js"),
        "GET / body does not contain built-frontend marker\nbody:\n{}",
        root_resp.body
    );

    // (5) GET /assets/bundle.js → HTTP 200 (static asset served from dist/).
    let asset_resp = asset_resp.expect("HTTP GET /assets/bundle.js should succeed");
    assert_eq!(
        asset_resp.status, 200,
        "GET /assets/bundle.js expected HTTP 200, got {}",
        asset_resp.status
    );

    // (6) GET /api/health → HTTP 200 (backend endpoint works).
    let api_resp = api_resp.expect("HTTP GET /api/health should succeed");
    assert_eq!(
        api_resp.status, 200,
        "GET /api/health expected HTTP 200, got {}\nstderr:\n{stderr_final}",
        api_resp.status
    );
}
