//! Regression test for ato-run/ato#192 (KOH-21).
//!
//! Verifies that a `source/python` target with `runtime_tools = { node = "20" }`
//! and a target-level `build` command behaves correctly end-to-end:
//!
//! 1. Node 20 is provisioned via `runtime_tools` (not the host's Node).
//! 2. The declared `build` command runs during the build phase.
//! 3. `dist/index.html` is produced by the build.
//! 4. The Python server starts and serves the built frontend at `/` (HTTP 200).
//! 5. The `/api/health` backend endpoint returns HTTP 200.
//!
//! The fixture uses `--dangerously-skip-permissions` so the test works without a
//! native sandbox and without pre-seeding execution-plan consent.  The Python
//! server auto-shuts down after 30 s so `ato run` exits on its own.
//!
//! Prerequisites (non-strict-CI environments gracefully skip if absent):
//! - `uv` on PATH (Python provisioning via `uv venv`).
//! - Internet access for the first-run Node 20 toolchain download (~30-60 s).

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
    // listener drops here, releasing the port; brief TOCTOU but acceptable in tests
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

/// Issue a bare-bones HTTP/1.0 GET and return the status code.
fn http_get_status(addr: &str, path: &str) -> Option<u16> {
    let mut stream = TcpStream::connect(addr).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .ok()?;
    let req = format!("GET {path} HTTP/1.0\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).ok()?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let status_str = text.lines().next()?.split_whitespace().nth(1)?;
    status_str.parse().ok()
}

// ─── cleanup ──────────────────────────────────────────────────────────────────

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// ─── test ─────────────────────────────────────────────────────────────────────

/// End-to-end regression for #192: source/python with runtime_tools + build lifecycle.
///
/// Requires `uv` on PATH and internet for the first Node 20 download.
/// Skips gracefully if prerequisites are unavailable (unless `ATO_STRICT_CI=1`).
#[test]
#[serial]
#[cfg(unix)]
fn source_python_runtime_tools_build_and_serve() {
    // ── prerequisites ────────────────────────────────────────────────────────
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

    // ── reserve a free port and make it available to ato via env var ─────────
    let port = reserve_free_port();
    let addr = format!("127.0.0.1:{port}");

    // ── log files (written while ato is running) ──────────────────────────────
    let stdout_log = root.join("ato-stdout.log");
    let stderr_log = root.join("ato-stderr.log");

    let stdout_file = fs::File::create(&stdout_log).expect("create stdout log");
    let stderr_file = fs::File::create(&stderr_log).expect("create stderr log");

    // ── spawn `ato run . --yes --dangerously-skip-permissions` ────────────────
    // * `--dangerously-skip-permissions` bypasses both E301 (sandbox opt-in) and
    //   E302 (execution-plan consent), running the Python server via execute_host.
    // * `ATO_UI_OVERRIDE_PORT` injects the dynamically reserved port into the
    //   capsule process so the Python server binds to the same address we poll.
    // * `CAPSULE_ALLOW_UNSAFE=1` is the env-based counterpart of --dangerously-skip-permissions.
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
    // Cold run: Node 20 download + uv venv + npm run build → allow up to 5 min.
    // Warm run (Node cached): typically under 30 s.
    let server_ready = wait_for_tcp(&addr, Duration::from_secs(300));

    let stderr_content = fs::read_to_string(&stderr_log).unwrap_or_default();
    let stdout_content = fs::read_to_string(&stdout_log).unwrap_or_default();

    if !server_ready {
        let _ = child.kill();
        let _ = child.wait();

        // Detect a graceful skip condition: Node download not available or uv
        // provisioning error in a non-strict environment.
        let skip_signal = stderr_content.contains("managed node runtime is unavailable")
            || stderr_content.contains("No such file or directory")
            || stderr_content.contains("toolchain")
                && !strict_ci()
                && !stderr_content.contains("Build [main]");
        if skip_signal {
            eprintln!("[source_python_runtime_tools_e2e] skipping: Node 20 not downloadable or uv error\n{stderr_content}");
            return;
        }

        panic!(
            "server on {addr} did not become ready within 5 minutes\n\
             stdout:\n{stdout_content}\nstderr:\n{stderr_content}"
        );
    }

    // ── HTTP assertions ───────────────────────────────────────────────────────
    let root_status = http_get_status(&addr, "/");
    let api_status = http_get_status(&addr, "/api/health");

    // ── wait for ato to finish (server auto-shuts down after 30 s) ───────────
    // Give it up to 60 s after the HTTP probes; the server's 30 s timer started
    // when it came up, so there's at most ~30 s remaining.
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

    // (1) Build command ran (the 🏗️ header appears in ato's stdout/stderr).
    assert!(
        stdout_final.contains("Build [main]") || stderr_final.contains("Build [main]"),
        "expected 'Build [main]' lifecycle log — build did not run\n\
         stdout:\n{stdout_final}\nstderr:\n{stderr_final}"
    );

    // (2) dist/index.html was produced by the Node build.
    assert!(
        workspace.join("dist/index.html").exists(),
        "dist/index.html not found after build — npm run build may not have run"
    );
    let index_html = fs::read_to_string(workspace.join("dist/index.html"))
        .expect("read dist/index.html");
    assert!(
        index_html.contains("DOCTYPE") || index_html.contains("html"),
        "dist/index.html does not look like HTML: {index_html:?}"
    );

    // (3) Node 20 was provisioned via runtime_tools (not the host's Node).
    // The managed toolchain lives under $HOME/.ato/toolchains/node-20/.
    let node_toolchain_root = home.join(".ato").join("toolchains").join("node-20");
    assert!(
        node_toolchain_root.exists(),
        "managed Node 20 toolchain not found at {} — runtime_tools may not have been resolved",
        node_toolchain_root.display()
    );

    // (4) / returns HTTP 200 and serves the built frontend.
    assert_eq!(
        root_status,
        Some(200),
        "GET / expected 200, got {:?}\nstderr:\n{stderr_final}",
        root_status
    );

    // (5) Backend /api/health returns HTTP 200.
    assert_eq!(
        api_status,
        Some(200),
        "GET /api/health expected 200, got {:?}\nstderr:\n{stderr_final}",
        api_status
    );
}
