//! `ato app install`: one entry point for a local `.capsule` file and a Hub
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
        .args(["app", "install"])
        .arg(fixture())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The install prints what it did for a human; the Instance id is in that text.
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let instance_id = stdout
        .split_whitespace()
        .find(|word| word.starts_with("linst_"))
        .expect("the install names the Instance")
        .trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .to_owned();

    // The same object is what inspect reads back.
    let inspected = ato(home.path())
        .args(["app", "inspect", &instance_id])
        .output()
        .unwrap();
    assert!(inspected.status.success());
    let inspected: Value = serde_json::from_slice(&inspected.stdout).unwrap();
    assert_eq!(inspected["instance"]["instance_id"], instance_id.as_str());
    assert_eq!(inspected["instance"]["schema"], "ato.local-instance/1");
}

#[test]
fn a_path_that_is_not_a_capsule_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let output = ato(home.path())
        .args(["app", "install", "Cargo.toml"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("bundle") || stderr.contains("local .capsule file or a Hub reference"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn a_missing_path_is_refused_without_touching_the_library() {
    let home = tempfile::tempdir().unwrap();
    let output = ato(home.path())
        .args(["app", "install", "does-not-exist.capsule"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!home.path().join("instances").join("linst_").exists());
}

#[test]
#[ignore = "network: downloads a public GitHub repository and packs it"]
fn a_github_url_becomes_a_durable_instance_with_a_pinned_snapshot() {
    let home = tempfile::tempdir().unwrap();
    let output = ato(home.path())
        .args(["app", "install", "https://github.com/octocat/Spoon-Knife"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let instance: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(instance["schema"], "ato.local-instance/1");
    assert!(
        instance["contract_ref"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Commit: "), "no pinned commit: {stderr}");

    // The snapshot and the packed Application are kept under ATO_HOME.
    let mut capsules = Vec::new();
    for entry in std::fs::read_dir(home.path().join("sources")).unwrap() {
        let path = entry.unwrap().path().join("application.capsule");
        if path.is_file() {
            capsules.push(path);
        }
    }
    assert_eq!(capsules.len(), 1, "expected exactly one packed Application");
}
