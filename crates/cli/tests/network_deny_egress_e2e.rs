//! ato#786: `[network] enabled = false` must actually deny outbound network.
//!
//! The escape this guards: a `source` capsule that declared network disabled
//! still had full egress, because the `enabled` key was dropped on the way to
//! the launcher and the executor re-defaulted the posture to "enabled", which
//! the Linux launcher lowers to `bwrap --unshare-all --share-net`.
//!
//! The assertion is the one the issue asks for: an outbound `connect()` from
//! inside the sandbox fails with `ENETUNREACH`.
//!
//! ## Where this runs
//!
//! The enforcement mechanism is Linux-specific (a bubblewrap network
//! namespace), so the body only executes on Linux with `bwrap`, `nacelle` and
//! `python3` present. Everywhere else — and on a Linux host missing one of
//! those — the test skips with a printed reason instead of being compiled out,
//! so it always type-checks. `ATO_STRICT_CI=1` turns every skip into a
//! failure, which is how CI demands the real thing.

mod fail_closed_support;

use std::fs;
use std::path::Path;

use fail_closed_support::ato_cmd;
use tempfile::TempDir;

fn strict_ci() -> bool {
    std::env::var("ATO_STRICT_CI")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Skip unless this host can actually enforce a network namespace, or fail when
/// strict CI demands enforcement.
fn skip_unless_enforceable(reason_missing: &mut String) -> bool {
    if !cfg!(target_os = "linux") {
        *reason_missing = format!("network-namespace deny is Linux-only (host: {})", HOST_OS);
        return true;
    }
    for tool in ["bwrap", "python3"] {
        if which::which(tool).is_err() {
            *reason_missing = format!("`{tool}` not found on PATH");
            return true;
        }
    }
    if resolve_nacelle().is_none() {
        *reason_missing = "nacelle binary not built (set NACELLE_PATH)".to_string();
        return true;
    }
    false
}

const HOST_OS: &str = std::env::consts::OS;

fn resolve_nacelle() -> Option<std::path::PathBuf> {
    if let Ok(path) = std::env::var("NACELLE_PATH") {
        let nacelle = std::path::PathBuf::from(path);
        if nacelle.exists() {
            return Some(nacelle);
        }
    }
    let candidate = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../nacelle/target/debug/nacelle");
    candidate.exists().then_some(candidate)
}

/// The issue's canary, reporting an errno rather than just a boolean so a
/// failure names *why* the connection failed.
const PAYLOAD_PY: &str = r#"import socket

result = []
try:
    conn = socket.create_connection(("1.1.1.1", 443), timeout=5)
    conn.close()
    result.append("NET=REACHABLE")
except OSError as error:
    result.append(f"NET=BLOCKED({error.errno})")
print("DIAG:", " | ".join(result), flush=True)
"#;

fn write_capsule(root: &Path, network_section: &str) {
    fs::create_dir_all(root).expect("create capsule dir");
    fs::write(
        root.join("capsule.toml"),
        format!(
            r#"schema_version = "0.3"
name = "net-canary"
version = "0.1.0"
type = "job"
runtime = "source"
runtime_version = "3.11"
run = "payload.py"

{network_section}
"#
        ),
    )
    .expect("write capsule.toml");
    fs::write(root.join("payload.py"), PAYLOAD_PY).expect("write payload.py");

    // A project-local venv makes the executor run `python3 payload.py`
    // directly instead of `uv run ...`, which cannot work inside the sandbox
    // (no `uv` is bind-mounted). Keeps the fixture dependency-free.
    let venv = std::process::Command::new("python3")
        .arg("-m")
        .arg("venv")
        .arg(root.join(".venv"))
        .output()
        .expect("create venv");
    assert!(
        venv.status.success(),
        "python3 -m venv failed: {}",
        String::from_utf8_lossy(&venv.stderr)
    );
}

fn run_sandboxed(root: &Path, home: &Path) -> std::process::Output {
    let mut cmd = ato_cmd();
    cmd.arg("run").arg("--yes").arg("--sandbox");
    if let Some(nacelle) = resolve_nacelle() {
        cmd.arg("--nacelle").arg(nacelle);
    }
    cmd.arg(root)
        .env("HOME", home)
        .output()
        .expect("run net-canary capsule under the sandbox")
}

fn diag_line(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .find(|line| line.contains("DIAG:"))
        .unwrap_or_else(|| {
            panic!(
                "canary produced no DIAG line; stdout={stdout}; stderr={}",
                String::from_utf8_lossy(&output.stderr)
            )
        })
        .to_string()
}

/// ENETUNREACH is the expected errno inside an empty network namespace.
/// ENETDOWN / EHOSTUNREACH are accepted because which one the kernel reports
/// depends on whether bwrap brought loopback up — all three mean "no route off
/// this namespace", and none of them can happen with `--share-net`.
const BLOCKED_ERRNOS: [&str; 3] = ["NET=BLOCKED(101)", "NET=BLOCKED(100)", "NET=BLOCKED(113)"];

#[test]
fn declared_network_deny_blocks_outbound_connect_with_enetunreach() {
    let mut reason = String::new();
    if skip_unless_enforceable(&mut reason) {
        assert!(
            !strict_ci(),
            "ATO_STRICT_CI demands real network-deny enforcement, but {reason}"
        );
        eprintln!("skipping declared_network_deny_blocks_outbound_connect: {reason}");
        return;
    }

    let workspace = TempDir::new().expect("workspace");
    let home = TempDir::new().expect("home");
    let root = workspace.path().join("net-canary");
    write_capsule(&root, "[network]\nenabled = false");

    let output = run_sandboxed(&root, home.path());
    let diag = diag_line(&output);

    assert!(
        !diag.contains("NET=REACHABLE"),
        "SANDBOX ESCAPE: [network] enabled = false still reached 1.1.1.1:443 — {diag}"
    );
    assert!(
        BLOCKED_ERRNOS.iter().any(|errno| diag.contains(errno)),
        "expected the connection to fail with ENETUNREACH (errno 101); got {diag}"
    );
}

/// The other half of the claim: the deny above is caused by the declaration,
/// not by the test host simply having no network. Without the declaration the
/// same capsule on the same host reaches out.
#[test]
fn undeclared_network_posture_still_reaches_the_network() {
    let mut reason = String::new();
    if skip_unless_enforceable(&mut reason) {
        assert!(
            !strict_ci(),
            "ATO_STRICT_CI demands real network-deny enforcement, but {reason}"
        );
        eprintln!("skipping undeclared_network_posture_still_reaches_the_network: {reason}");
        return;
    }

    let workspace = TempDir::new().expect("workspace");
    let home = TempDir::new().expect("home");
    let root = workspace.path().join("net-canary-open");
    write_capsule(&root, "");

    let output = run_sandboxed(&root, home.path());
    let diag = diag_line(&output);

    if !diag.contains("NET=REACHABLE") {
        // No egress from the test host at all: the deny assertion above proves
        // nothing on its own, so say so rather than reporting a green control.
        eprintln!(
            "control case could not reach the network either ({diag}); \
             the deny assertion is not differential on this host"
        );
        assert!(
            !strict_ci(),
            "ATO_STRICT_CI requires the control case to have egress; got {diag}"
        );
    }
}
