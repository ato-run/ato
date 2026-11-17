//! The durable local Instance library through the CLI surface a person uses:
//! list, open, and remove with or without saved data.
//!
//! These tests do not start a Run. A Run needs a host-observed worker process
//! identity, which is covered by the macOS manual gate rather than here.

use std::path::{Path, PathBuf};
use std::process::Output;
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

fn import(home: &Path) -> String {
    let output = ato(home)
        .args(["app", "import"])
        .arg(fixture())
        .output()
        .expect("the ato binary runs");
    assert!(
        output.status.success(),
        "import failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: Value = serde_json::from_slice(&output.stdout).expect("import prints JSON");
    document["instance_id"]
        .as_str()
        .expect("import names the Instance")
        .to_owned()
}

fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "expected JSON on stdout ({error}); stdout: {}, stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn an_empty_library_lists_no_instances() {
    let home = tempfile::tempdir().unwrap();
    let output = ato(home.path())
        .args(["app", "list", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let document = stdout_json(&output);
    assert_eq!(document["schema"], "ato.local-instance-list/1");
    assert_eq!(document["instances"].as_array().unwrap().len(), 0);
}

#[test]
fn an_imported_instance_is_listed_and_removal_retains_its_data() {
    let home = tempfile::tempdir().unwrap();
    let instance_id = import(home.path());

    let document = stdout_json(
        &ato(home.path())
            .args(["app", "list", "--json"])
            .output()
            .unwrap(),
    );
    let instances = document["instances"].as_array().unwrap();
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0]["instance"]["instance_id"], instance_id);
    assert_eq!(instances[0]["state"], "stopped");
    assert!(instances[0]["active_run"].is_null());

    let retained = home.path().join("removed").join(&instance_id);
    let removal = stdout_json(
        &ato(home.path())
            .args(["app", "remove", &instance_id, "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(removal["schema"], "ato.local-instance-removal/1");
    assert_eq!(removal["data_retained"], true);
    assert!(retained.join("instance.json").is_file());
    assert!(retained.join("snapshots").is_dir());

    let document = stdout_json(
        &ato(home.path())
            .args(["app", "list", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(document["instances"].as_array().unwrap().len(), 0);

    // Purging the retained data is a separate, explicit operation.
    let purged = stdout_json(
        &ato(home.path())
            .args(["app", "remove", &instance_id, "--purge-data", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(purged["data_retained"], false);
    assert!(!retained.exists());
}

#[test]
fn purge_data_deletes_the_registration_and_its_data() {
    let home = tempfile::tempdir().unwrap();
    let instance_id = import(home.path());
    let instance_root = home.path().join("instances").join(&instance_id);
    assert!(instance_root.is_dir());

    let removal = stdout_json(
        &ato(home.path())
            .args(["app", "remove", &instance_id, "--purge-data", "--json"])
            .output()
            .unwrap(),
    );
    assert_eq!(removal["data_retained"], false);
    assert!(!instance_root.exists());
    assert!(!home.path().join("removed").join(&instance_id).exists());
}

#[test]
fn opening_a_stopped_instance_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let instance_id = import(home.path());
    let output = ato(home.path())
        .args(["app", "open", &instance_id])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("is not running"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn removing_an_unknown_instance_fails_without_touching_the_library() {
    let home = tempfile::tempdir().unwrap();
    let output = ato(home.path())
        .args(["app", "remove", "linst_deadbeef", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("does not exist"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn doctor_reports_a_machine_readable_contract() {
    let home = tempfile::tempdir().unwrap();
    let output = ato(home.path())
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let document = stdout_json(&output);
    assert_eq!(document["schema"], "ato.doctor/1");
    assert!(document["version"].is_string());
    assert!(document["commit"].is_string());
    let checks = document["checks"].as_array().unwrap();
    let ids: Vec<&str> = checks
        .iter()
        .filter_map(|check| check["id"].as_str())
        .collect();
    for expected in ["target", "ato-home", "process-identity", "docker"] {
        assert!(ids.contains(&expected), "doctor is missing `{expected}`");
    }
    for check in checks {
        let status = check["status"].as_str().unwrap();
        assert!(
            ["pass", "warn", "fail"].contains(&status),
            "unexpected status `{status}`"
        );
    }
    // The exit code follows the checks, never the other way round.
    assert_eq!(output.status.success(), document["ok"].as_bool().unwrap());
    if std::env::consts::OS == "macos" && std::env::consts::ARCH == "aarch64" {
        assert_eq!(document["supported"], true);
    }
}
