//! Capsule Protocol acceptance: Capture -> transfer -> replay -> continue.

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use capsule::protocol_bundle::{PortableCapsule, capture_workspace_state};

fn scratch_dir(prefix: &str) -> tempfile::TempDir {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".ato")
        .join("test-scratch");
    fs::create_dir_all(&root).expect("create hermetic test scratch root");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(root)
        .expect("create hermetic test directory")
}

fn run_capture(producer: &Path, bundle: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ato"))
        .args(["internal", "capsule-protocol", "capture", "--workspace"])
        .arg(producer.join("workspace"))
        .arg("--output")
        .arg(bundle)
        .args(["--", "rustc", "main.rs"])
        .env("ATO_HOME", producer.join("ato-home"))
        .env("HOME", producer.join("user-home"))
        .output()
        .expect("run producer capture")
}

#[test]
fn capsule_protocol_pty_transfers_replays_and_continues_without_producer() {
    let producer = scratch_dir("protocol-producer-");
    let transfer = scratch_dir("protocol-transfer-");
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/protocol-pty-error/main.rs");
    let producer_workspace = producer.path().join("workspace");
    fs::create_dir_all(&producer_workspace).expect("create producer workspace");
    fs::copy(&fixture, producer_workspace.join("main.rs")).expect("copy fixture");

    let bundle = transfer.path().join("error.capsule");
    let captured = run_capture(producer.path(), &bundle);
    assert!(
        captured.status.success(),
        "capture failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&captured.stdout),
        String::from_utf8_lossy(&captured.stderr)
    );
    assert!(bundle.is_file(), "producer must emit one portable bundle");
    assert!(
        String::from_utf8_lossy(&captured.stdout).contains("mismatched types"),
        "producer must observe the real compiler diagnostic"
    );

    let producer_path = producer.path().to_path_buf();
    drop(producer);
    assert!(
        !producer_path.exists(),
        "producer workspace and ATO_HOME must be unavailable before replay"
    );

    let consumer = scratch_dir("protocol-consumer-");
    let restored = consumer.path().join("restored-workspace");
    let mut child = Command::new(env!("CARGO_BIN_EXE_ato"))
        .args(["internal", "capsule-protocol", "replay"])
        .arg(&bundle)
        .arg("--into")
        .arg(&restored)
        .env("ATO_HOME", consumer.path().join("ato-home"))
        .env("HOME", consumer.path().join("user-home"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn consumer replay");
    child
        .stdin
        .as_mut()
        .expect("consumer stdin")
        .write_all(b"echo __CONTINUED__\npwd\nexit\n")
        .expect("send continuation commands");
    drop(child.stdin.take());
    let replayed = child.wait_with_output().expect("wait for consumer replay");
    let stdout = String::from_utf8_lossy(&replayed.stdout);
    let stderr = String::from_utf8_lossy(&replayed.stderr);
    assert!(
        replayed.status.success(),
        "replay failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("mismatched types"),
        "consumer must show actual replay egress, not inject recorded output: {stdout}"
    );
    assert!(
        stdout.contains("__CONTINUED__"),
        "the replayed PTY must accept new input: {stdout}"
    );
    assert!(
        stdout.contains(restored.to_string_lossy().as_ref()),
        "continuation must run inside restored State: {stdout}"
    );
    assert!(
        stderr.contains("Capsule Protocol replay complete: 2 records"),
        "replay completion must be explicit: {stderr}"
    );
    assert_eq!(
        fs::read_to_string(restored.join("main.rs")).expect("restored source"),
        fs::read_to_string(fixture).expect("fixture source"),
        "consumer State must come from the bundle"
    );
}

#[test]
fn capsule_protocol_replay_reports_divergence_from_actual_egress() {
    let producer = scratch_dir("protocol-diverge-producer-");
    let transfer = scratch_dir("protocol-diverge-transfer-");
    let workspace = producer.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(
        workspace.join("main.rs"),
        "fn main() { let _: u8 = \"recorded\"; }\n",
    )
    .unwrap();
    let bundle_path = transfer.path().join("diverge.capsule");
    let captured = run_capture(producer.path(), &bundle_path);
    assert!(captured.status.success());

    // Replace the State with a different, internally valid State object while
    // retaining the recorded ingress/egress. Bundle closure and digests remain
    // valid, so only actual execution can discover the divergence.
    let changed = scratch_dir("protocol-diverge-state-");
    fs::write(
        changed.path().join("main.rs"),
        "fn main() { let _: bool = 7; }\n",
    )
    .unwrap();
    let (state, object) = capture_workspace_state(changed.path()).unwrap();
    let mut bundle = PortableCapsule::read(&bundle_path).unwrap();
    bundle.objects.clear();
    bundle.objects.insert(state.state_ref.clone(), object);
    bundle.descriptor.base_state = state;
    bundle.write(&bundle_path).unwrap();

    let consumer = scratch_dir("protocol-diverge-consumer-");
    let replayed = Command::new(env!("CARGO_BIN_EXE_ato"))
        .args(["internal", "capsule-protocol", "replay"])
        .arg(&bundle_path)
        .arg("--into")
        .arg(consumer.path().join("restored"))
        .arg("--no-continue")
        .env("ATO_HOME", consumer.path().join("ato-home"))
        .env("HOME", consumer.path().join("user-home"))
        .output()
        .unwrap();
    assert!(!replayed.status.success());
    assert!(
        String::from_utf8_lossy(&replayed.stderr).contains("replay diverged at seq 2"),
        "stderr: {}",
        String::from_utf8_lossy(&replayed.stderr)
    );
}
