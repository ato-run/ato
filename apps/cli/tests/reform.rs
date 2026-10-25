//! `ato reform`: a candidate Derivation is built against the frozen Contract
//! of an existing `.capsule`. These tests are runtime-light: every case here
//! ends before execution (source-bound K, K mismatch, existing `--out`), so
//! they hold on hosts without bubblewrap.
//!
//! The real "accepted" path — candidate runs, existing verifier observes K and
//! the receipt is fully satisfied — needs a Runtime and is PR3's E2E.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_cmd::Command;
use ato_portable_application::build_authored_bundle_v2;

fn ato() -> Command {
    let mut command =
        Command::cargo_bin("ato").expect("the ato binary is built for integration tests");
    command.timeout(Duration::from_secs(30));
    command
}

/// A behavioral `ato.capsule/2` process source: one HTTP observation, no
/// workspace identity in K, so the Contract can admit a different source.
fn written_source(root: &Path, status: u16, app: &str) {
    let manifest = format!(
        r#"
schema = "ato.capsule/2"

[application]
title = "Reform fixture"
surface_path = "/"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[contract]
mode = "all"

[[contract.observation]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
method = "GET"
path = "/"
status = {status}

[[derivation]]
id = "app"
use = "ato.process@1"
argv = ["python3", "app.py"]
cwd = "."
guest_port = 8000
runtimes = {{ python = "3.12" }}

[effects]
default = "pure"
"#
    );
    fs::write(root.join("capsule.toml"), manifest).expect("capsule.toml");
    fs::write(root.join("app.py"), app).expect("app.py");
}

fn source(status: u16, app: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    written_source(dir.path(), status, app);
    dir
}

fn frozen_bundle(root: &Path, destination: &Path) -> Vec<u8> {
    let (bytes, _) = build_authored_bundle_v2(root).expect("frozen bundle");
    fs::write(destination, &bytes).expect("frozen capsule");
    bytes
}

fn read_report(out: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(out.join("report.json")).expect("report.json"))
        .expect("report json")
}

#[test]
fn an_existing_out_directory_is_refused_and_nothing_is_written() {
    let frozen_source = source(200, "print('old')\n");
    let directory = tempfile::tempdir().unwrap();
    let frozen_path = directory.path().join("frozen.capsule");
    let frozen = frozen_bundle(frozen_source.path(), &frozen_path);

    let out = directory.path().join("out");
    fs::create_dir(&out).unwrap();
    fs::write(out.join("keep.txt"), b"do not touch").unwrap();

    let new_source = source(200, "print('new')\n");
    ato()
        .args(["reform"])
        .arg(&frozen_path)
        .arg(new_source.path())
        .arg("--out")
        .arg(&out)
        .assert()
        .failure()
        .stderr(predicates::str::contains("already exists"));

    assert_eq!(fs::read(&frozen_path).unwrap(), frozen);
    assert_eq!(fs::read(out.join("keep.txt")).unwrap(), b"do not touch");
    assert!(!out.join("report.json").exists());
    assert!(!out.join("candidate.capsule").exists());
}

#[test]
fn a_candidate_that_changes_k_is_rejected_without_execution_or_artifact() {
    let frozen_source = source(200, "print('old')\n");
    let directory = tempfile::tempdir().unwrap();
    let frozen_path = directory.path().join("frozen.capsule");
    let frozen = frozen_bundle(frozen_source.path(), &frozen_path);
    let new_source = source(201, "print('new')\n");

    let out = directory.path().join("out");
    ato()
        .args(["reform"])
        .arg(&frozen_path)
        .arg(new_source.path())
        .arg("--out")
        .arg(&out)
        .assert()
        .failure()
        .stderr(predicates::str::contains("candidate_contract_mismatch"));

    let report = read_report(&out);
    assert_eq!(report["schema"], "ato.transformation-candidate/1");
    assert_eq!(report["status"], "rejected");
    assert_eq!(report["k_preserved"], false);
    assert_eq!(report["fully_satisfied"], false);
    assert!(
        report["error"]
            .as_str()
            .is_some_and(|error| error.contains("candidate_contract_mismatch")),
        "{report}"
    );
    // Both identities are recorded, and they differ.
    assert!(report["frozen_contract_ref"].is_string(), "{report}");
    assert!(
        report["frozen_contract_ref"] != report["error"].as_str().unwrap_or_default(),
        "{report}"
    );
    // No candidate transport and no execution evidence.
    assert!(!out.join("candidate.capsule").exists());
    assert!(!out.join("receipt.json").exists());
    // The frozen bundle is byte-identical after the refusal.
    assert_eq!(fs::read(&frozen_path).unwrap(), frozen);
}

#[test]
fn a_source_bound_frozen_contract_is_rejected_before_building() {
    // The sample's K captures a workspace closure (ato.contract.workspace@1),
    // so it only admits its original source. The refusal must happen before
    // the new source is built, and before execution.
    let frozen_path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static.capsule");
    let frozen = fs::read(&frozen_path).expect("source-bound sample");
    let new_source = source(200, "print('new')\n");

    let directory = tempfile::tempdir().unwrap();
    let out = directory.path().join("out");
    ato()
        .args(["reform"])
        .arg(&frozen_path)
        .arg(new_source.path())
        .arg("--out")
        .arg(&out)
        .assert()
        .failure()
        .stderr(predicates::str::contains("frozen_contract_is_source_bound"));

    let report = read_report(&out);
    assert_eq!(report["status"], "rejected");
    assert_eq!(report["k_preserved"], false);
    assert!(report["frozen_contract_ref"].is_string(), "{report}");
    assert!(
        !report["old_derivation_refs"]
            .as_array()
            .expect("old derivations")
            .is_empty(),
        "{report}"
    );
    assert!(
        !report["old_source_closure_refs"]
            .as_array()
            .expect("old source closures")
            .is_empty(),
        "the old source closure is reported"
    );
    assert!(!out.join("candidate.capsule").exists());
    assert!(!out.join("receipt.json").exists());
    assert_eq!(fs::read(&frozen_path).unwrap(), frozen);
}
