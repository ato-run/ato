//! End-to-end test for ato-run/ato#723 (Bun as a `runtime_tools` entry).
//!
//! Verifies the canonical App Builder shape from the issue — a `source/node`
//! target that supplies **Bun** via `runtime_tools` and drives both lifecycle
//! phases with it — works end-to-end:
//!
//! 1. Bun is provisioned via `runtime_tools` (managed toolchain cache, not the
//!    host's Bun).
//! 2. The declared `build` command (`bun install`) runs during the build phase
//!    with Bun on PATH.
//! 3. The declared `run` command (`bun run server.ts`) starts a `Bun.serve`
//!    HTTP server — which only succeeds if `bun` is genuinely on the run PATH.
//! 4. `GET /` returns HTTP 200 with the fixture marker.
//! 5. `GET /api/health` returns HTTP 200 from the Bun backend.
//!
//! The fixture uses `--dangerously-skip-permissions` so the test works without a
//! native sandbox and without pre-seeding execution-plan consent.  The Bun
//! server auto-shuts down after 30 s so `ato run` exits on its own.
//!
//! Prerequisites:
//! - Network access for first-run Node 20 + Bun 1.1.38 toolchain downloads.
//!
//! Skip behaviour:
//! - `ATO_STRICT_CI=1`: **never skip** — any prerequisite failure is a test failure.
//! - Default: skip gracefully when a toolchain download fails (network
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
        .join("source-node-bun-runtime-tools")
}

fn test_root() -> PathBuf {
    let root = std::env::current_dir()
        .expect("cwd")
        .join(".ato")
        .join("test-scratch")
        .join("source-node-bun-runtime-tools-e2e")
        .join(format!("{:016x}", rand::random::<u64>()));
    fs::create_dir_all(&root).expect("create test root");
    root
}

fn copy_fixture(dst: &Path) {
    fs::create_dir_all(dst).expect("create dst");
    for entry in walkdir::WalkDir::new(fixture_dir()).min_depth(1) {
        let entry = entry.expect("walkdir entry");
        let rel = entry
            .path()
            .strip_prefix(fixture_dir())
            .expect("strip prefix");
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

/// Outcome of waiting for the capsule server to come up.
enum Readiness {
    /// The declared port accepted a TCP connection.
    Ready,
    /// `ato run` exited before the port was ready — the run phase failed. Carries
    /// nothing; the caller reads the captured logs to classify the failure.
    ChildExited,
    /// Neither readiness nor exit within the timeout.
    Timeout,
}

/// Wait until `addr` accepts a connection, the child process exits, or `timeout`
/// elapses — whichever comes first. Polling the child means a crashed run phase
/// is reported in seconds instead of stalling for the full readiness window.
fn wait_for_server_or_exit(
    addr: &str,
    child: &mut std::process::Child,
    timeout: Duration,
) -> Readiness {
    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect(addr).is_ok() {
            return Readiness::Ready;
        }
        if child.try_wait().expect("try_wait").is_some() {
            // Re-check the port once: the server could have bound and exited in
            // the same poll window (not expected for this fixture, but cheap).
            if TcpStream::connect(addr).is_ok() {
                return Readiness::Ready;
            }
            return Readiness::ChildExited;
        }
        if Instant::now() >= deadline {
            return Readiness::Timeout;
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

/// End-to-end for #723: source/node + `runtime_tools = { bun = "1.1.38" }`,
/// with Bun driving both build and run.
///
/// Skip policy:
/// - Non-strict CI: skip if and only if a toolchain download fails (network
///   unavailable signal in ato output).
/// - `ATO_STRICT_CI=1`: **never skip** — any failure is a real failure.
#[test]
#[serial]
#[cfg(unix)]
fn source_node_bun_runtime_tools_build_and_serve() {
    // ── workspace setup ──────────────────────────────────────────────────────
    let root = test_root();
    let _cleanup = Cleanup(root.clone());

    let home = root.join("home");
    let workspace = root.join("workspace");
    fs::create_dir_all(&home).expect("create home");
    copy_fixture(&workspace);

    // ── reserve a free port; inject it so the Bun server binds there ──────────
    let port = reserve_free_port();
    let addr = format!("127.0.0.1:{port}");

    // ── log files (populated while ato is running) ────────────────────────────
    let stdout_log = root.join("ato-stdout.log");
    let stderr_log = root.join("ato-stderr.log");
    let stdout_file = fs::File::create(&stdout_log).expect("create stdout log");
    let stderr_file = fs::File::create(&stderr_log).expect("create stderr log");

    // ── spawn `ato run . --yes --dangerously-skip-permissions` ────────────────
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

    // ── wait for the Bun server to come up (or `ato run` to exit) ─────────────
    // Cold run: Node 20 + Bun downloads + `bun install`. Allow up to 5 minutes;
    // a warm run (toolchains cached) is typically well under 30 s. If the run
    // phase fails, `ato run` exits early and we report in seconds, not minutes.
    let readiness = wait_for_server_or_exit(&addr, &mut child, Duration::from_secs(300));

    let stderr_content = fs::read_to_string(&stderr_log).unwrap_or_default();
    let stdout_content = fs::read_to_string(&stdout_log).unwrap_or_default();

    if !matches!(readiness, Readiness::Ready) {
        let _ = child.kill();
        let _ = child.wait();

        // Narrow skip: only skip in non-strict CI when ato output clearly
        // signals a toolchain download failure (network unavailable). Any other
        // failure (including runtime_tools not being applied at run time) must
        // surface as a real failure so the regression is not masked.
        let is_download_failure = stderr_content.contains("managed node runtime is unavailable")
            || stderr_content.contains("failed to download")
            || stderr_content.contains("toolchain download")
            || stderr_content.to_lowercase().contains("network");
        if !strict_ci() && is_download_failure {
            eprintln!(
                "[source_node_bun_runtime_tools_e2e] skipping: toolchain download \
                 unavailable\n{stderr_content}"
            );
            return;
        }

        let reason = match readiness {
            Readiness::ChildExited => {
                "`ato run` exited before the bun server became ready \
                 (run phase failed — e.g. managed bun not on the run PATH)"
            }
            Readiness::Timeout => "bun server did not become ready within 5 minutes",
            Readiness::Ready => unreachable!(),
        };
        panic!("{reason}\non {addr}\nstdout:\n{stdout_content}\nstderr:\n{stderr_content}");
    }

    // ── HTTP probes ───────────────────────────────────────────────────────────
    let root_resp = http_get(&addr, "/");
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

    let stderr_final = fs::read_to_string(&stderr_log).unwrap_or_default();
    let stdout_final = fs::read_to_string(&stdout_log).unwrap_or_default();

    // ── assertions ────────────────────────────────────────────────────────────

    // (1) Build lifecycle ran: the build header appears in ato output.
    assert!(
        stdout_final.contains("Build [main]") || stderr_final.contains("Build [main]"),
        "expected 'Build [main]' in ato output — build lifecycle did not run\n\
         stdout:\n{stdout_final}\nstderr:\n{stderr_final}"
    );

    // (2) Bun was provisioned via runtime_tools (managed tool cache created).
    let bun_toolchain_root = home
        .join(".ato")
        .join("toolchains")
        .join("tools")
        .join("bun")
        .join("1.1.38");
    assert!(
        bun_toolchain_root.exists(),
        "managed Bun 1.1.38 toolchain not found at {} — runtime_tools.bun was not applied\n\
         stderr:\n{stderr_final}",
        bun_toolchain_root.display()
    );

    // (3) GET / → HTTP 200 + fixture marker (the Bun server served it).
    let root_resp = root_resp.expect("HTTP GET / should succeed (server was ready on TCP)");
    assert_eq!(
        root_resp.status, 200,
        "GET / expected HTTP 200, got {}\nstderr:\n{stderr_final}",
        root_resp.status
    );
    assert!(
        root_resp
            .body
            .contains("source-node-bun-runtime-tools-fixture"),
        "GET / body does not contain the fixture marker\nbody:\n{}",
        root_resp.body
    );

    // (4) GET /api/health → HTTP 200 from the Bun backend.
    let api_resp = api_resp.expect("HTTP GET /api/health should succeed");
    assert_eq!(
        api_resp.status, 200,
        "GET /api/health expected HTTP 200, got {}\nstderr:\n{stderr_final}",
        api_resp.status
    );
    assert!(
        api_resp.body.contains("\"runtime\":\"bun\"") || api_resp.body.contains("ok"),
        "GET /api/health body unexpected: {}",
        api_resp.body
    );
}
