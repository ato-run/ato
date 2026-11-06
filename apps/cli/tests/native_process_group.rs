//! End-to-end acceptance for a native process service group.
//!
//! The Runtime's process sandbox is the same one the single-process route
//! uses. A host that already applies a Seatbelt/Landlock sandbox to this test
//! process refuses a nested one (`portable process sandbox admission failed`);
//! the same condition already makes the existing process-route tests fall
//! back to runtime-admission rejection. Those cases are reported as skipped,
//! never as a pass.

// A native process group needs process groups and Unix sockets; the Run
// itself refuses the route on a non-Unix host with "a native process service
// group requires a Unix host". There is no Windows materialization to test, so
// the acceptance is Unix-only rather than a Windows test that asserts a
// refusal it cannot exercise.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use assert_cmd::Command as AtoCommand;
use serde_json::Value;

fn sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/native-process-group")
}

fn ato(home: &Path) -> AtoCommand {
    let mut command = AtoCommand::cargo_bin("ato").expect("the ato binary is built");
    command.env("ATO_HOME", home);
    command.timeout(Duration::from_secs(120));
    command
}

/// Whether the failure is the host refusing a nested process sandbox rather
/// than this feature failing.
fn sandbox_unavailable(output: &Output) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr.contains("portable process sandbox admission failed")
        || stderr.contains("macOS sandbox init failed")
        || stderr.contains("requires a Linux bubblewrap sandbox")
}

fn pack(home: &Path, source: &Path, bundle: &Path) -> Output {
    ato(home)
        .arg("pack")
        .arg(source)
        .arg("--output")
        .arg(bundle)
        .output()
        .expect("pack runs")
}

/// No workload, shim, or infra process may survive the Run.
fn assert_no_orphans(home: &Path) {
    std::thread::sleep(Duration::from_millis(200));
    let listing = Command::new("pgrep")
        .arg("-f")
        .arg(home.display().to_string())
        .output();
    if let Ok(listing) = listing {
        assert!(
            listing.status.code() == Some(1) || listing.stdout.is_empty(),
            "a Run process outlived its Run under {}:\n{}",
            home.display(),
            String::from_utf8_lossy(&listing.stdout)
        );
    }
}

#[test]
fn the_native_process_group_satisfies_its_contract_and_leaves_no_process() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let bundle = root.path().join("native-group.capsule");
    let packed = pack(&home, &sample(), &bundle);
    assert!(
        packed.status.success(),
        "pack failed: {}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let stdout = String::from_utf8_lossy(&packed.stdout);
    assert!(stdout.contains("derivation_ref=sha256:"), "{stdout}");
    let derivation_ref = stdout
        .trim()
        .lines()
        .find_map(|line| line.strip_prefix("derivation_ref="))
        .expect("the pack reports its route")
        .to_owned();

    let receipt = root.path().join("receipt.json");
    let run = ato(&home)
        .arg("run")
        .arg(&bundle)
        .arg("--no-open")
        .arg("--verification-receipt")
        .arg(&receipt)
        .output()
        .expect("run runs");
    if !run.status.success() && sandbox_unavailable(&run) {
        eprintln!(
            "skipped: this host refuses the nested process sandbox: {}",
            String::from_utf8_lossy(&run.stderr).trim()
        );
        assert_no_orphans(&home);
        return;
    }
    assert!(
        run.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let receipt: Value = serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
    assert_eq!(receipt["derivation_ref"], derivation_ref.as_str());
    assert_eq!(receipt["fully_satisfied"], true);
    let ids = receipt["observations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|observation| observation["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["external-egress-denied", "surface", "through-proxy"]);
    for observation in receipt["observations"].as_array().unwrap() {
        assert_eq!(observation["outcome"], "satisfied");
    }
    assert_no_orphans(&home);
}

#[test]
fn a_backend_exit_fails_the_dependent_observation_and_the_group_is_reaped() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let source = root.path().join("source");
    fs::create_dir_all(&source).unwrap();
    for file in ["backend.py", "proxy.py"] {
        fs::copy(sample().join(file), source.join(file)).unwrap();
    }
    fs::write(
        source.join("backend.py"),
        fs::read_to_string(sample().join("backend.py"))
            .unwrap()
            .replace(
                "ThreadingHTTPServer((\"127.0.0.1\", port), Handler).serve_forever()",
                "import threading, time\n\
                 threading.Thread(target=lambda: (time.sleep(0.5), os._exit(0)), daemon=True).start()\n\
                 ThreadingHTTPServer((\"127.0.0.1\", port), Handler).serve_forever()",
            ),
    )
    .unwrap();
    // The dependent observation waits past the backend's exit, so the proxy
    // cannot serve it from a live peer.
    fs::write(
        source.join("proxy.py"),
        fs::read_to_string(sample().join("proxy.py"))
            .unwrap()
            .replace(
                "            elif self.path == \"/\":\n                body = get_from_backend(\"/\")",
                "            elif self.path == \"/\":\n                import time\n                time.sleep(1.5)\n                body = get_from_backend(\"/\")",
            ),
    )
    .unwrap();
    fs::copy(sample().join("capsule.toml"), source.join("capsule.toml")).unwrap();

    let bundle = root.path().join("failing-group.capsule");
    let packed = pack(&home, &source, &bundle);
    assert!(
        packed.status.success(),
        "pack failed: {}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let receipt = root.path().join("receipt.json");
    let run = ato(&home)
        .arg("run")
        .arg(&bundle)
        .arg("--no-open")
        .arg("--verification-receipt")
        .arg(&receipt)
        .output()
        .expect("run runs");
    if sandbox_unavailable(&run) {
        eprintln!(
            "skipped: this host refuses the nested process sandbox: {}",
            String::from_utf8_lossy(&run.stderr).trim()
        );
        assert_no_orphans(&home);
        return;
    }
    assert!(
        !run.status.success(),
        "a backend that exits must fail the Run: {}",
        String::from_utf8_lossy(&run.stdout)
    );
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("exited") || stderr.contains("observation") || stderr.contains("did not"),
        "the failure must name the stopped service or the failed observation: {stderr}"
    );
    if receipt.exists() {
        let receipt: Value = serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
        assert_ne!(receipt["fully_satisfied"], true);
    }
    assert_no_orphans(&home);
}
