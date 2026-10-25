//! Contract-preserving Derivation transformation v0 — end-to-end proof.
//!
//! This suite executes two independently written Python sources under the SAME
//! frozen Contract K and records the machine-readable evidence the PR cites:
//!
//! ```text
//! source A ──ato pack──▶ bundle A ──ato run──▶ receipt A (fully satisfied)
//!                                     │
//! source B ──ato reform bundle A──────▶┘ candidate D_B, same K, receipt B
//! ```
//!
//! and the negative matrix:
//!
//! - a candidate that returns different bytes under the same K is never
//!   accepted (the existing verifier fails the observation);
//! - a candidate source that derives a different K is refused before it is
//!   built or run (`candidate_contract_mismatch`);
//! - a frozen K that captures a workspace closure refuses a different source
//!   (`frozen_contract_is_source_bound`).
//!
//! Everything here needs a Linux Runtime (bubblewrap + the provisioned Python
//! 3.12 toolchain), so the whole crate is Linux-only. On other hosts it
//! compiles to an empty test binary and filters zero tests.
//!
//! The suite never publishes, activates or switches anything: `ato reform`
//! writes a local candidate directory and the old bundle and receipt are
//! asserted byte-identical afterwards.

#![cfg(target_os = "linux")]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_cmd::Command;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const EVIDENCE_SCHEMA: &str = "ato.derivation-transformation-evidence/1";
const REPORT_SCHEMA: &str = "ato.transformation-candidate/1";
const WORKSPACE_PROTOCOL: &str = "ato.workspace@1";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/transformation-v0")
}

fn fixture(name: &str) -> PathBuf {
    fixtures().join(name)
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

/// Where the evidence index and copied artifacts live.
///
/// `CARGO_TARGET_DIR` is honoured so a shared target directory keeps the
/// evidence beside the build; CI (no override) lands it at
/// `<repo>/target/transformation-v0-evidence`, which is the uploaded path.
fn evidence_root() -> PathBuf {
    let base = match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => repo_root().join("target"),
    };
    base.join("transformation-v0-evidence")
}

fn ato(home: &Path) -> Command {
    let mut command =
        Command::cargo_bin("ato").expect("the ato binary is built for integration tests");
    command
        .env("ATO_HOME", home)
        .timeout(Duration::from_secs(180));
    command
}

fn sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

fn read_json(path: &Path) -> Value {
    let bytes = fs::read(path).unwrap_or_else(|error| {
        panic!("read {}: {error}", path.display());
    });
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!("parse {}: {error}", path.display());
    })
}

fn assert_success(output: &std::process::Output, what: &str) {
    assert!(
        output.status.success(),
        "{what} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn decode_portable(bytes: &[u8]) -> ato_objects::PortableApplicationBundle {
    match ato_objects::decode_capsule_bundle_document(bytes).expect("decode Capsule bundle") {
        ato_objects::CapsuleBundleDocument::PortableApplicationV3(bundle)
        | ato_objects::CapsuleBundleDocument::PortableApplicationV4(bundle) => bundle,
        _ => panic!("expected a portable-application bundle"),
    }
}

/// Write the evidence index after every phase, so a later assertion failure
/// still leaves the phases that already ran on disk for the CI artifact.
fn flush_evidence(directory: &Path, evidence: &Value) {
    fs::create_dir_all(directory).expect("create evidence directory");
    let bytes = serde_json::to_vec_pretty(evidence).expect("serialize evidence");
    fs::write(directory.join("evidence.json"), bytes).expect("write evidence.json");
}

/// Copy one artifact into the evidence directory, keeping the phase in the
/// file name so same-named report/receipt files do not collide.
fn copy_artifact(source: &Path, directory: &Path) -> String {
    fs::create_dir_all(directory).expect("create evidence directory");
    let phase = source
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "artifact".to_owned());
    let name = source
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "artifact".to_owned());
    let destination = directory.join(format!("{phase}--{name}"));
    fs::copy(source, &destination).unwrap_or_else(|error| {
        panic!(
            "copy {} -> {}: {error}",
            source.display(),
            destination.display()
        );
    });
    destination.display().to_string()
}

/// The frozen bundle's K/D and the source closure its single Derivation pins.
struct Baseline {
    bundle_bytes: Vec<u8>,
    bundle_sha256: String,
    contract_ref: String,
    derivation_ref: String,
    source_closure_ref: String,
    receipt_bytes: Vec<u8>,
    receipt_sha256: String,
}

fn pack_and_run(home: &Path, work: &Path) -> Baseline {
    let bundle_path = work.join("bundle-a.capsule");
    let receipt_path = work.join("receipt-a.json");

    let pack = ato(home)
        .arg("pack")
        .arg(fixture("source-a"))
        .arg("-o")
        .arg(&bundle_path)
        .output()
        .expect("run ato pack");
    assert_success(&pack, "ato pack source-a");

    let bundle_bytes = fs::read(&bundle_path).expect("bundle-a");
    let bundle = decode_portable(&bundle_bytes);
    assert_eq!(bundle.index.derivations.len(), 1, "fixture A has one route");
    let derivation_ref = bundle.index.derivations[0].clone();
    let (_, validated) =
        ato_portable_application::validate_bytes_for_derivation(&bundle_bytes, &derivation_ref)
            .expect("validate bundle A");
    let source_closure_ref = validated
        .derivation
        .inputs
        .iter()
        .find(|input| input.protocol == WORKSPACE_PROTOCOL)
        .expect("workspace input A")
        .content_ref
        .clone();

    let run = ato(home)
        .arg("run")
        .arg(&bundle_path)
        .arg("--no-open")
        .arg("--verification-receipt")
        .arg(&receipt_path)
        .output()
        .expect("run bundle A");
    assert!(
        run.status.success(),
        "baseline run must succeed on Linux (bwrap + python 3.12)\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let receipt_bytes = fs::read(&receipt_path).expect("receipt A");
    let receipt: Value = serde_json::from_slice(&receipt_bytes).expect("receipt A json");
    assert_eq!(
        receipt["fully_satisfied"], true,
        "baseline receipt: {receipt}"
    );
    assert_eq!(receipt["contract_ref"], validated.contract_ref.as_str());
    assert_eq!(receipt["derivation_ref"], derivation_ref.as_str());
    assert_eq!(receipt["target"]["kind"], "cli-local");

    Baseline {
        bundle_sha256: sha256(&bundle_bytes),
        bundle_bytes,
        contract_ref: validated.contract_ref.to_string(),
        derivation_ref,
        source_closure_ref,
        receipt_sha256: sha256(&receipt_bytes),
        receipt_bytes,
    }
}

fn reform(home: &Path, bundle: &Path, source: &Path, out: &Path) -> std::process::Output {
    ato(home)
        .arg("reform")
        .arg(bundle)
        .arg(source)
        .arg("--out")
        .arg(out)
        .output()
        .expect("run ato reform")
}

#[test]
fn two_source_derivations_satisfy_one_frozen_contract_and_the_matrix_is_evidence() {
    let evidence_dir = evidence_root();
    fs::create_dir_all(&evidence_dir).expect("create evidence directory");

    let work_dir = tempfile::tempdir().expect("work dir");
    let work = work_dir.path();
    let home = work.join("home");
    fs::create_dir_all(&home).expect("home");

    let mut evidence = json!({
        "schema": EVIDENCE_SCHEMA,
        "fixtures": {
            "source_a": fixture("source-a").display().to_string(),
            "source_b": fixture("source-b").display().to_string(),
            "source_b_broken": fixture("source-b-broken").display().to_string(),
            "source_c_contract_change": fixture("source-c-contract-change").display().to_string(),
        },
        "status": "running",
    });
    flush_evidence(&evidence_dir, &evidence);

    // ── Baseline: pack + run source A, keep bundle/receipt bytes as evidence ──
    let baseline = pack_and_run(&home, work);
    let bundle_a_path = work.join("bundle-a.capsule");
    let receipt_a_path = work.join("receipt-a.json");
    fs::write(&bundle_a_path, &baseline.bundle_bytes).expect("bundle A");
    fs::write(&receipt_a_path, &baseline.receipt_bytes).expect("receipt A");

    evidence["baseline"] = json!({
        "bundle_a_sha256": baseline.bundle_sha256.clone(),
        "contract_ref": baseline.contract_ref.clone(),
        "derivation_ref": baseline.derivation_ref.clone(),
        "source_closure_ref": baseline.source_closure_ref.clone(),
        "receipt_a_sha256": baseline.receipt_sha256.clone(),
        "fully_satisfied": true,
    });
    flush_evidence(&evidence_dir, &evidence);

    // ── Transformation: candidate D_B from an independent source B ──────────
    let out = work.join("out");
    let transformed = reform(&home, &bundle_a_path, &fixture("source-b"), &out);
    assert!(
        transformed.status.success(),
        "reform must accept source B under the frozen K\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&transformed.stdout),
        String::from_utf8_lossy(&transformed.stderr)
    );
    let report = read_json(&out.join("report.json"));
    let receipt_b = read_json(&out.join("receipt.json"));
    assert_eq!(report["schema"], REPORT_SCHEMA);
    assert_eq!(report["status"], "accepted");
    assert_eq!(report["k_preserved"], true);
    assert_eq!(report["fully_satisfied"], true);
    assert_eq!(
        report["frozen_contract_ref"],
        baseline.contract_ref.as_str()
    );
    assert_eq!(receipt_b["contract_ref"], baseline.contract_ref.as_str());
    assert_eq!(receipt_b["fully_satisfied"], true);
    assert_ne!(
        receipt_b["derivation_ref"],
        baseline.derivation_ref.as_str(),
        "a different source is a different Derivation"
    );
    assert_eq!(report["new_derivation_ref"], receipt_b["derivation_ref"]);
    let new_closure = report["new_source_closure_ref"]
        .as_str()
        .expect("new closure")
        .to_owned();
    assert_ne!(
        new_closure, baseline.source_closure_ref,
        "a different source is a different closure"
    );

    let candidate_bytes = fs::read(out.join("candidate.capsule")).expect("candidate bundle");
    let candidate_sha = sha256(&candidate_bytes);
    assert_eq!(report["new_bundle_sha256"], candidate_sha);
    let candidate_bundle = decode_portable(&candidate_bytes);
    assert_eq!(
        candidate_bundle.index.root_contract_ref, baseline.contract_ref,
        "the candidate bundle is rooted at the SAME frozen K"
    );

    // The frozen bundle and its receipt are byte-identical after a transform.
    assert_eq!(fs::read(&bundle_a_path).unwrap(), baseline.bundle_bytes);
    assert_eq!(fs::read(&receipt_a_path).unwrap(), baseline.receipt_bytes);
    assert_eq!(sha256(&baseline.bundle_bytes), baseline.bundle_sha256);
    assert_eq!(sha256(&baseline.receipt_bytes), baseline.receipt_sha256);

    let transformation_report = copy_artifact(&out.join("report.json"), &evidence_dir);
    let transformation_receipt = copy_artifact(&out.join("receipt.json"), &evidence_dir);
    evidence["baseline"]["bundle_a_sha256_after_reform"] = json!(baseline.bundle_sha256.clone());
    evidence["baseline"]["receipt_a_sha256_after_reform"] = json!(baseline.receipt_sha256.clone());
    evidence["baseline"]["old_bundle_unchanged"] = json!(true);
    evidence["baseline"]["old_receipt_unchanged"] = json!(true);
    evidence["transformation"] = json!({
        "report_path": transformation_report,
        "receipt_path": transformation_receipt,
        "candidate_bundle_sha256": candidate_sha,
        "candidate_root_contract_ref": candidate_bundle.index.root_contract_ref,
        "new_derivation_ref": receipt_b["derivation_ref"],
        "new_source_closure_ref": new_closure,
        "k_equal": receipt_b["contract_ref"] == baseline.contract_ref.as_str(),
        "derivation_changed": receipt_b["derivation_ref"] != baseline.derivation_ref.as_str(),
        "source_changed": new_closure != baseline.source_closure_ref,
        "fully_satisfied": receipt_b["fully_satisfied"],
        "status": report["status"],
        "observations": receipt_b["observations"],
    });
    flush_evidence(&evidence_dir, &evidence);

    // ── Negative 1: same K, broken behavior ─────────────────────────────────
    let out_broken = work.join("out-broken");
    let broken = reform(
        &home,
        &bundle_a_path,
        &fixture("source-b-broken"),
        &out_broken,
    );
    let broken_stderr = String::from_utf8_lossy(&broken.stderr).to_string();
    assert!(
        !broken.status.success(),
        "a behavior-breaking candidate must not be accepted:\n{broken_stderr}"
    );
    let broken_report = read_json(&out_broken.join("report.json"));
    assert_eq!(broken_report["status"], "rejected");
    assert_eq!(
        broken_report["k_preserved"], true,
        "K was preserved; the run failed"
    );
    assert_eq!(broken_report["fully_satisfied"], false);
    assert!(
        broken_report["error"]
            .as_str()
            .is_some_and(|error| error.contains("did not fully satisfy")),
        "unexpected behavior-break error: {broken_report}"
    );
    let broken_receipt = read_json(&out_broken.join("receipt.json"));
    assert_eq!(broken_receipt["fully_satisfied"], false);
    assert_eq!(
        broken_receipt["contract_ref"],
        baseline.contract_ref.as_str()
    );
    assert!(
        broken_receipt["observations"]
            .as_array()
            .expect("observations")
            .iter()
            .any(|observation| observation["outcome"] == "failed"),
        "the failed observation is recorded: {broken_receipt}"
    );
    assert_eq!(fs::read(&bundle_a_path).unwrap(), baseline.bundle_bytes);
    let broken_report_path = copy_artifact(&out_broken.join("report.json"), &evidence_dir);
    let broken_receipt_path = copy_artifact(&out_broken.join("receipt.json"), &evidence_dir);
    evidence["negatives"] = json!({
        "behavior_break_rejected": true,
        "behavior_break_report_path": broken_report_path,
        "behavior_break_receipt_path": broken_receipt_path,
        "behavior_break_fully_satisfied": false,
        "behavior_break_contract_ref": broken_receipt["contract_ref"],
    });
    flush_evidence(&evidence_dir, &evidence);

    // ── Negative 2: a source that derives a different K ─────────────────────
    let out_contract = work.join("out-contract");
    let contract_change = reform(
        &home,
        &bundle_a_path,
        &fixture("source-c-contract-change"),
        &out_contract,
    );
    let contract_change_stderr = String::from_utf8_lossy(&contract_change.stderr).to_string();
    assert!(
        !contract_change.status.success(),
        "a different K must be refused:\n{contract_change_stderr}"
    );
    assert!(
        contract_change_stderr.contains("candidate_contract_mismatch"),
        "unexpected refusal: {contract_change_stderr}"
    );
    let contract_report = read_json(&out_contract.join("report.json"));
    assert_eq!(contract_report["status"], "rejected");
    assert_eq!(contract_report["k_preserved"], false);
    assert_eq!(contract_report["fully_satisfied"], false);
    assert!(
        !out_contract.join("candidate.capsule").exists(),
        "no candidate transport is written when K is not preserved"
    );
    assert!(
        !out_contract.join("receipt.json").exists(),
        "nothing is executed when K is not preserved"
    );
    assert_eq!(fs::read(&bundle_a_path).unwrap(), baseline.bundle_bytes);
    let contract_report_path = copy_artifact(&out_contract.join("report.json"), &evidence_dir);
    evidence["negatives"]["contract_change_refused"] = json!(true);
    evidence["negatives"]["contract_change_report_path"] = json!(contract_report_path);
    evidence["negatives"]["contract_change_error"] = contract_report["error"].clone();
    evidence["negatives"]["contract_change_no_candidate_bundle"] = json!(true);
    evidence["negatives"]["contract_change_no_receipt"] = json!(true);
    flush_evidence(&evidence_dir, &evidence);

    // ── Negative 3: a frozen K that captures its source closure ─────────────
    let source_bound_bundle = repo_root().join("samples/interop-static.capsule");
    let out_source_bound = work.join("out-source-bound");
    let source_bound = reform(
        &home,
        &source_bound_bundle,
        &fixture("source-b"),
        &out_source_bound,
    );
    let source_bound_stderr = String::from_utf8_lossy(&source_bound.stderr).to_string();
    assert!(
        !source_bound.status.success(),
        "a source-bound K must refuse a different source:\n{source_bound_stderr}"
    );
    assert!(
        source_bound_stderr.contains("frozen_contract_is_source_bound"),
        "unexpected refusal: {source_bound_stderr}"
    );
    let source_bound_report = read_json(&out_source_bound.join("report.json"));
    assert_eq!(source_bound_report["status"], "rejected");
    assert_eq!(source_bound_report["k_preserved"], false);
    assert!(
        !out_source_bound.join("candidate.capsule").exists(),
        "source-bound refusal happens before building"
    );
    assert!(!out_source_bound.join("receipt.json").exists());
    let source_bound_report_path =
        copy_artifact(&out_source_bound.join("report.json"), &evidence_dir);
    evidence["negatives"]["source_bound_refused"] = json!(true);
    evidence["negatives"]["source_bound_report_path"] = json!(source_bound_report_path);
    evidence["negatives"]["source_bound_error"] = source_bound_report["error"].clone();

    evidence["status"] = json!("passed");
    evidence["evidence_directory"] = json!(evidence_dir.display().to_string());
    flush_evidence(&evidence_dir, &evidence);

    println!(
        "transformation-v0 evidence written to {}",
        evidence_dir.join("evidence.json").display()
    );
}
