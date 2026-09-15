use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;

fn ato() -> Command {
    Command::cargo_bin("ato").expect("the ato binary is built for integration tests")
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static.capsule")
}

#[test]
fn byte_identical_fixture_runs_and_emits_a_fully_satisfied_cli_receipt() {
    let output = tempfile::tempdir().unwrap();
    let receipt_path = output.path().join("cli-receipt.json");
    ato()
        .arg("run")
        .arg(fixture())
        .arg("--no-open")
        .arg("--verification-receipt")
        .arg(&receipt_path)
        .assert()
        .success();

    let receipt: Value = serde_json::from_slice(&fs::read(receipt_path).unwrap()).unwrap();
    assert_eq!(
        receipt["bundle_sha256"],
        "sha256:95b837f4e8ed3c3354a4560e56820030cdb2ba34ba2a7c65aa6d26e139bdd813"
    );
    assert_eq!(
        receipt["contract_ref"],
        "sha256:b565e9d771c53ba03548b1a151c0ebbc2b884c1d6ec532f72596ce33a4b741db"
    );
    assert_eq!(receipt["target"]["kind"], "cli-local");
    assert_eq!(receipt["fully_satisfied"], true);
    assert_eq!(receipt["observations"][0]["id"], "app-proof");
    assert_eq!(receipt["observations"][0]["outcome"], "satisfied");
    assert_eq!(
        receipt["observations"][0]["evidence"]["body_sha256"],
        "sha256:5ec8f5a458f7f500116b8394dd550718915f2d960283905210abe03132ee986a"
    );
    assert_eq!(receipt["observations"][1]["id"], "source-identity");
    assert_eq!(receipt["observations"][1]["outcome"], "satisfied");
}

#[test]
fn one_byte_bundle_tamper_fails_before_running() {
    let output = tempfile::tempdir().unwrap();
    let tampered = output.path().join("tampered.capsule");
    let mut value: Value = serde_json::from_slice(&fs::read(fixture()).unwrap()).unwrap();
    let encoded = value["payloads"][0]["bytes"].as_str().unwrap();
    let replacement = if encoded.starts_with('e') { "f" } else { "e" };
    value["payloads"][0]["bytes"] = Value::String(format!("{replacement}{}", &encoded[1..]));
    fs::write(&tampered, serde_jcs::to_vec(&value).unwrap()).unwrap();

    ato()
        .arg("run")
        .arg(tampered)
        .arg("--no-open")
        .assert()
        .failure()
        .stderr(predicates::str::contains("object digest mismatch"));
}

#[test]
fn changed_root_contract_ref_fails_before_running() {
    let output = tempfile::tempdir().unwrap();
    let tampered = output.path().join("root-tampered.capsule");
    let mut value: Value = serde_json::from_slice(&fs::read(fixture()).unwrap()).unwrap();
    value["index"]["root_contract_ref"] = Value::String(format!("sha256:{}", "0".repeat(64)));
    fs::write(&tampered, serde_jcs::to_vec(&value).unwrap()).unwrap();

    ato()
        .arg("run")
        .arg(tampered)
        .arg("--no-open")
        .assert()
        .failure()
        .stderr(predicates::str::contains("closure is incomplete"));
}
