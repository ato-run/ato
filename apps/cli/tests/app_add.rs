//! `ato app add`: one entry point for a local `.capsule` file and a Hub
//! reference, both producing the same durable Instance.

use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_cmd::Command;
use serde_json::Value;

fn ato(home: &Path) -> Command {
    let mut command =
        Command::cargo_bin("ato").expect("the ato binary is built for integration tests");
    command.env("ATO_HOME", home);
    command.timeout(Duration::from_secs(60));
    command
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static.capsule")
}

#[test]
fn a_local_capsule_is_added_as_an_instance() {
    let home = tempfile::tempdir().unwrap();
    let output = ato(home.path())
        .args(["app", "add"])
        .arg(fixture())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "add failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let instance: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(instance["schema"], "ato.local-instance/1");
    let instance_id = instance["instance_id"].as_str().unwrap();

    // `add` registers the same object `inspect` reads back.
    let inspected = ato(home.path())
        .args(["app", "inspect", instance_id])
        .output()
        .unwrap();
    assert!(inspected.status.success());
    let inspected: Value = serde_json::from_slice(&inspected.stdout).unwrap();
    assert_eq!(inspected["instance"]["instance_id"], instance_id);
    assert_eq!(
        inspected["instance"]["contract_ref"],
        instance["contract_ref"]
    );
}

#[test]
fn a_path_that_is_not_a_capsule_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let output = ato(home.path())
        .args(["app", "add", "Cargo.toml"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("local .capsule file or a Hub reference"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn a_missing_path_is_refused_without_touching_the_library() {
    let home = tempfile::tempdir().unwrap();
    let output = ato(home.path())
        .args(["app", "add", "does-not-exist.capsule"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!home.path().join("instances").join("linst_").exists());
}
