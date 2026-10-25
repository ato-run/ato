//! Stateful transformation E2E — different code, different internal database
//! schema, the SAME frozen Contract K.
//!
//! ```text
//! source A (JSON file store) ──ato pack──▶ bundle A
//!                                          │  K: HTTP root + ato.scenario/1 (crud)
//!                                          ├──ato run──▶ receipt A (fully satisfied)
//!                                          │
//! source B (SQLite, different schema) ─ato reform──▶ candidate D_B
//!                                          └──ato run──▶ receipt B (fully satisfied)
//! ```
//!
//! The scenario is the same content-addressed object for both: it is carried
//! from the frozen bundle by `ato reform`, never re-declared by source B. The
//! two sources differ in code organisation and in their internal storage
//! (JSON object with `id/name/quantity` vs SQLite `users`/`items` tables with
//! `sku/qty/created_at`), which the scenario cannot see: it only observes HTTP.
//!
//! The negative matrix:
//!
//! - `source-b-broken` ignores authorization, so the scenario's cleared-cookie
//!   read returns 200 instead of 401: `ato reform` exits nonzero, the report is
//!   rejected, and the kept receipt is `fully_satisfied: false`;
//! - `source-b-redefines-k` declares a different Contract: `ato reform` refuses
//!   with `candidate_contract_mismatch` before writing a candidate bundle.
//!
//! Everything needs a Linux Runtime (bubblewrap + the provisioned Python 3.12),
//! so the crate is Linux-only and compiles to an empty test binary elsewhere.
//! Evidence lands under `$CARGO_TARGET_DIR/stateful-transformation-evidence`
//! (or `<repo>/target/...` without an override), which is what CI uploads.

#![cfg(target_os = "linux")]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_cmd::Command;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const EVIDENCE_SCHEMA: &str = "ato.stateful-transformation-evidence/1";
const SCENARIO_CONTRACT_VERIFIER: &str = "ato.contract.scenario@1";
const WORKSPACE_PROTOCOL: &str = "ato.workspace@1";
const SCENARIO_ID: &str = "crud";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stateful-transformation")
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

/// Evidence lives beside the shared build target so CI can upload it:
/// `$CARGO_TARGET_DIR/stateful-transformation-evidence`.
fn evidence_root() -> PathBuf {
    let base = match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => repo_root().join("target"),
    };
    base.join("stateful-transformation-evidence")
}

fn ato(home: &Path) -> Command {
    let mut command =
        Command::cargo_bin("ato").expect("the ato binary is built for integration tests");
    command
        .env("ATO_HOME", home)
        .timeout(Duration::from_secs(240));
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

fn flush_evidence(directory: &Path, evidence: &Value) {
    fs::create_dir_all(directory).expect("create evidence directory");
    let bytes = serde_json::to_vec_pretty(evidence).expect("serialize evidence");
    fs::write(directory.join("evidence.json"), bytes).expect("write evidence.json");
}

fn copy_artifact(source: &Path, directory: &Path, name: &str) -> Option<String> {
    if !source.is_file() {
        return None;
    }
    fs::create_dir_all(directory).expect("create evidence directory");
    let destination = directory.join(name);
    fs::copy(source, &destination).expect("copy artifact");
    Some(destination.display().to_string())
}

fn pack(home: &Path, source: &Path, output: &Path) {
    let packed = ato(home)
        .arg("pack")
        .arg(source)
        .arg("-o")
        .arg(output)
        .output()
        .expect("run ato pack");
    assert_success(&packed, "ato pack");
}

fn run(home: &Path, bundle: &Path, receipt: &Path) -> std::process::Output {
    ato(home)
        .arg("run")
        .arg(bundle)
        .arg("--no-open")
        .arg("--verification-receipt")
        .arg(receipt)
        .output()
        .expect("run ato run")
}

fn reform(home: &Path, frozen: &Path, source: &Path, out: &Path) -> std::process::Output {
    ato(home)
        .arg("reform")
        .arg(frozen)
        .arg(source)
        .arg("--out")
        .arg(out)
        .output()
        .expect("run ato reform")
}

fn observation<'a>(receipt: &'a Value, id: &str) -> &'a Value {
    receipt["observations"]
        .as_array()
        .expect("observations")
        .iter()
        .find(|observation| observation["id"] == id)
        .unwrap_or_else(|| panic!("receipt has no observation {id}: {receipt}"))
}

fn workspace_closure(derivation: &ato_formation::authoring::BoundDerivation) -> String {
    derivation
        .inputs
        .iter()
        .find(|input| input.protocol == WORKSPACE_PROTOCOL)
        .expect("workspace input")
        .content_ref
        .clone()
}

fn root_contract_bytes(bundle: &ato_objects::PortableApplicationBundle) -> Vec<u8> {
    let reference = ato_computation::ContentRef::parse(bundle.index.root_contract_ref.clone())
        .expect("root contract ref");
    bundle.payload_bytes(&reference).expect("contract payload")
}

fn scenario_sidecar_beside_receipt(receipt: &Path) -> PathBuf {
    let parent = receipt.parent().expect("receipt has a directory");
    let mut name = receipt
        .file_name()
        .expect("receipt file name")
        .to_os_string();
    name.push(".scenarios");
    parent.join(name).join(format!("{SCENARIO_ID}.json"))
}

fn scenario_transcript_in(directory: &Path) -> PathBuf {
    directory
        .join("scenario-transcripts")
        .join(format!("{SCENARIO_ID}.json"))
}

/// The machine-readable statement of "the internal storage really differs".
fn storage_marks(source: &Path) -> Value {
    let text = fs::read_to_string(source.join("app.py")).expect("read fixture app.py");
    if text.contains("sqlite3") {
        for marker in [
            "CREATE TABLE IF NOT EXISTS users",
            "CREATE TABLE IF NOT EXISTS items",
            "sku TEXT PRIMARY KEY",
            "qty INTEGER",
            "created_at TEXT",
        ] {
            assert!(text.contains(marker), "SQLite fixture lost marker {marker}");
        }
        assert!(!text.contains("STORE = {"));
        json!({
            "kind": "in_memory_sqlite",
            "path": ":memory:",
            "tables": {
                "users": ["id", "username", "created_at"],
                "items": ["sku", "name", "qty", "created_at"],
            },
        })
    } else {
        for marker in ["STORE = {", "\"next_id\"", "\"quantity\""] {
            assert!(text.contains(marker), "JSON fixture lost marker {marker}");
        }
        assert!(!text.contains("sqlite3"));
        json!({
            "kind": "in_memory_json",
            "path": "<process memory>",
            "items": {"key": "item-<n>", "fields": ["id", "name", "quantity"]},
            "users": {"fields": ["username"]},
        })
    }
}

#[test]
fn a_stateful_contract_is_satisfied_by_different_code_and_a_different_database_schema() {
    let evidence_dir = evidence_root();
    fs::create_dir_all(&evidence_dir).expect("create evidence directory");
    let work_dir = tempfile::tempdir().expect("work dir");
    let work = work_dir.path();
    let home = work.join("home");
    fs::create_dir_all(&home).expect("home");

    let mut evidence = json!({
        "schema": EVIDENCE_SCHEMA,
        "status": "running",
        "fixtures": {
            "source_a": fixture("source-a").display().to_string(),
            "source_b": fixture("source-b").display().to_string(),
            "source_b_broken": fixture("source-b-broken").display().to_string(),
            "source_b_redefines_k": fixture("source-b-redefines-k").display().to_string(),
            "scenario": fixture("scenarios/main.json").display().to_string(),
        },
        "implementation_difference": {
            "source_a": storage_marks(&fixture("source-a")),
            "source_b": storage_marks(&fixture("source-b")),
            "source_a_sha256": sha256(&fs::read(fixture("source-a/app.py")).expect("a app")),
            "source_b_sha256": sha256(&fs::read(fixture("source-b/app.py")).expect("b app")),
            "scenario_json_sha256": sha256(
                &fs::read(fixture("scenarios/main.json")).expect("scenario"),
            ),
        },
        "non_claims": [
            "different code + different internal schema are proven only for this fixture pair; scenario completeness is not claimed",
            "the scenario proves request/response behavior and state continuity inside one fresh Run, not persistence across process restarts",
            "the verifier addresses only the candidate endpoint; it does not prove the application cannot reach the network",
            "hosted / ato-api execution of scenarios is out of v1 scope",
        ],
    });
    flush_evidence(&evidence_dir, &evidence);

    // ── pack: source A declares the full Contract, scenario pinned into K ───
    let bundle_a_path = work.join("bundle-a.capsule");
    pack(&home, &fixture("source-a"), &bundle_a_path);
    let bundle_a_bytes = fs::read(&bundle_a_path).expect("bundle A");
    let bundle_a_hash = sha256(&bundle_a_bytes);
    let bundle_a = decode_portable(&bundle_a_bytes);
    assert_eq!(bundle_a.index.derivations.len(), 1, "one route");
    let derivation_a_ref = bundle_a.index.derivations[0].clone();
    let (_, validated_a) =
        ato_portable_application::validate_bytes_for_derivation(&bundle_a_bytes, &derivation_a_ref)
            .expect("validate baseline bundle");
    assert_eq!(validated_a.scenarios.len(), 1, "one scenario plan");
    let scenario_a = &validated_a.scenarios[0];
    assert_eq!(scenario_a.requirement_id, SCENARIO_ID);
    assert_eq!(scenario_a.port, "app.http");
    let scenario_ref = scenario_a.scenario_ref.to_string();
    let requirement = validated_a
        .contract
        .requirements
        .iter()
        .find(|requirement| requirement.id == SCENARIO_ID)
        .expect("scenario requirement in K");
    assert_eq!(requirement.verifier, SCENARIO_CONTRACT_VERIFIER);
    assert_eq!(requirement.digest.as_deref(), Some(scenario_ref.as_str()));
    let contract_ref = validated_a.contract_ref.to_string();
    let closure_a = workspace_closure(&validated_a.derivation);
    assert!(
        validated_a.derivation.state.is_empty(),
        "the fixtures use no state slot"
    );
    assert_eq!(
        bundle_a.index.instance_snapshot_ref, None,
        "the fixtures restore no snapshot"
    );
    assert_eq!(
        fs::read(fixture("source-a/scenarios/main.json")).unwrap(),
        fs::read(fixture("source-b/scenarios/main.json")).unwrap(),
        "both sources carry the identical scenario JSON bytes"
    );

    // ── run A: root HTTP + stateful scenario are both satisfied ─────────────
    let receipt_a_path = work.join("receipt-a.json");
    let run_a = run(&home, &bundle_a_path, &receipt_a_path);
    assert_success(&run_a, "ato run baseline bundle A");
    let receipt_a_bytes = fs::read(&receipt_a_path).expect("receipt A");
    let receipt_a_hash = sha256(&receipt_a_bytes);
    let receipt_a = read_json(&receipt_a_path);
    assert_eq!(receipt_a["fully_satisfied"], true, "{receipt_a}");
    assert_eq!(receipt_a["contract_ref"], contract_ref.as_str());
    assert_eq!(receipt_a["derivation_ref"], derivation_a_ref.as_str());
    assert_eq!(observation(&receipt_a, "root")["outcome"], "satisfied");
    let scenario_a_observation = observation(&receipt_a, SCENARIO_ID);
    assert_eq!(scenario_a_observation["outcome"], "satisfied");
    assert_eq!(
        scenario_a_observation["evidence"]["path"],
        format!("scenario:{SCENARIO_ID}")
    );
    assert_eq!(
        scenario_a_observation["evidence"]["digest"],
        scenario_ref.as_str()
    );
    let transcript_a = scenario_a_observation["evidence"]["body_sha256"]
        .as_str()
        .expect("transcript digest A")
        .to_owned();
    assert!(
        transcript_a.starts_with("sha256:") && transcript_a.len() == 71,
        "{transcript_a}"
    );
    let sidecar_a = scenario_sidecar_beside_receipt(&receipt_a_path);
    assert!(
        sidecar_a.is_file(),
        "missing baseline sidecar {sidecar_a:?}"
    );
    let raw_a = read_json(&sidecar_a);
    assert_eq!(
        sha256(&serde_jcs::to_vec(&raw_a["canonical_transcript"]).expect("canonical")),
        transcript_a,
        "the baseline transcript hashes to the receipt digest"
    );

    evidence["baseline"] = json!({
        "bundle_sha256": bundle_a_hash.clone(),
        "contract_ref": contract_ref.clone(),
        "derivation_ref": derivation_a_ref.clone(),
        "source_closure_ref": closure_a.clone(),
        "receipt_sha256": receipt_a_hash.clone(),
        "receipt_path": copy_artifact(&receipt_a_path, &evidence_dir, "baseline--receipt.json"),
        "transcript_path": copy_artifact(&sidecar_a, &evidence_dir, "baseline--crud-transcript.json"),
        "scenario": {
            "id": SCENARIO_ID,
            "ref": scenario_ref.clone(),
            "verifier_version": 1,
            "transcript_digest": transcript_a.clone(),
        },
        "fully_satisfied": true,
    });
    flush_evidence(&evidence_dir, &evidence);

    // ── reform: source B (different code, SQLite schema) under the frozen K ─
    let out = work.join("out-b");
    let reformed = reform(&home, &bundle_a_path, &fixture("source-b"), &out);
    assert_success(&reformed, "ato reform bundle A -> source B");
    let report = read_json(&out.join("report.json"));
    assert_eq!(report["status"], "accepted", "{report}");
    assert_eq!(report["k_preserved"], true, "{report}");
    assert_eq!(report["fully_satisfied"], true, "{report}");
    assert_eq!(report["frozen_contract_ref"], contract_ref.as_str());
    let derivation_b_ref = report["new_derivation_ref"]
        .as_str()
        .expect("new derivation ref")
        .to_owned();
    let closure_b = report["new_source_closure_ref"]
        .as_str()
        .expect("new source closure")
        .to_owned();
    assert_ne!(derivation_b_ref, derivation_a_ref);
    assert_ne!(closure_b, closure_a);
    assert_eq!(
        report["old_source_closure_refs"][0], closure_a,
        "the baseline closure is recorded"
    );

    let candidate_bytes = fs::read(out.join("candidate.capsule")).expect("candidate bundle");
    let candidate_bundle = decode_portable(&candidate_bytes);
    assert_eq!(
        candidate_bundle.index.root_contract_ref, contract_ref,
        "the candidate is rooted at the frozen K"
    );
    assert_eq!(
        root_contract_bytes(&candidate_bundle),
        root_contract_bytes(&bundle_a),
        "the candidate carries the frozen Contract bytes"
    );
    let (_, validated_b) = ato_portable_application::validate_bytes_for_derivation(
        &candidate_bytes,
        &derivation_b_ref,
    )
    .expect("validate candidate bundle");
    assert_eq!(validated_b.contract_ref.to_string(), contract_ref);
    assert_eq!(validated_b.scenarios.len(), 1, "frozen scenario carried");
    assert_eq!(
        validated_b.scenarios[0].scenario_ref.to_string(),
        scenario_ref,
        "the candidate executes the SAME scenario object"
    );
    assert!(
        validated_b.derivation.state.is_empty(),
        "source B uses no state slot"
    );

    let candidate_receipt = read_json(&out.join("receipt.json"));
    assert_eq!(
        candidate_receipt["fully_satisfied"], true,
        "{candidate_receipt}"
    );
    assert_eq!(candidate_receipt["contract_ref"], contract_ref.as_str());
    assert_eq!(
        candidate_receipt["derivation_ref"],
        derivation_b_ref.as_str()
    );
    let scenario_b_observation = observation(&candidate_receipt, SCENARIO_ID);
    assert_eq!(scenario_b_observation["outcome"], "satisfied");
    assert_eq!(
        scenario_b_observation["evidence"]["digest"],
        scenario_ref.as_str()
    );
    let transcript_b = scenario_b_observation["evidence"]["body_sha256"]
        .as_str()
        .expect("transcript digest B")
        .to_owned();
    assert_eq!(
        transcript_b, transcript_a,
        "identical observable behavior yields the same canonical transcript digest"
    );
    let reform_transcript = scenario_transcript_in(&out);
    assert!(
        reform_transcript.is_file(),
        "ato reform must keep the scenario transcript: {reform_transcript:?}"
    );
    let raw_b = read_json(&reform_transcript);
    assert_eq!(
        sha256(&serde_jcs::to_vec(&raw_b["canonical_transcript"]).expect("canonical")),
        transcript_b
    );

    // Neither the old bundle nor its receipt was rewritten.
    assert_eq!(sha256(&fs::read(&bundle_a_path).unwrap()), bundle_a_hash);
    assert_eq!(sha256(&fs::read(&receipt_a_path).unwrap()), receipt_a_hash);

    evidence["reform"] = json!({
        "accepted": true,
        "k_preserved": true,
        "frozen_contract_ref": contract_ref,
        "baseline_derivation_ref": derivation_a_ref,
        "new_derivation_ref": derivation_b_ref,
        "baseline_source_closure_ref": closure_a,
        "new_source_closure_ref": closure_b,
        "candidate_bundle_sha256": sha256(&candidate_bytes),
        "candidate_root_contract_ref": candidate_bundle.index.root_contract_ref,
        "receipt_path": copy_artifact(&out.join("receipt.json"), &evidence_dir, "reform--receipt.json"),
        "transcript_path": copy_artifact(&reform_transcript, &evidence_dir, "reform--crud-transcript.json"),
        "report_path": copy_artifact(&out.join("report.json"), &evidence_dir, "reform--report.json"),
        "scenario_transcript_digest": transcript_b,
        "fully_satisfied": true,
    });
    flush_evidence(&evidence_dir, &evidence);

    // ── negative: the same K, but behavior that misses a step ───────────────
    let out_broken = work.join("out-broken");
    let broken = reform(
        &home,
        &bundle_a_path,
        &fixture("source-b-broken"),
        &out_broken,
    );
    assert!(
        !broken.status.success(),
        "a candidate that misses a scenario step must not exit 0"
    );
    let broken_report = read_json(&out_broken.join("report.json"));
    assert_eq!(broken_report["status"], "rejected", "{broken_report}");
    assert_eq!(broken_report["k_preserved"], true, "{broken_report}");
    assert_eq!(broken_report["fully_satisfied"], false, "{broken_report}");
    let broken_error = broken_report["error"].as_str().unwrap_or_default();
    assert!(
        broken_error.contains("scenario_step_failed"),
        "unexpected broken-candidate error: {broken_report}"
    );
    assert!(
        out_broken.join("candidate.capsule").is_file(),
        "K was preserved, so the candidate bundle exists"
    );
    let broken_receipt_path = out_broken.join("receipt.json");
    let broken_receipt = read_json(&broken_receipt_path);
    assert_eq!(broken_receipt["fully_satisfied"], false, "{broken_receipt}");
    assert_eq!(
        observation(&broken_receipt, SCENARIO_ID)["outcome"],
        "failed"
    );
    let broken_transcript = copy_artifact(
        &scenario_transcript_in(&out_broken),
        &evidence_dir,
        "broken--crud-transcript.json",
    );
    evidence["negative_broken_behavior"] = json!({
        "fully_satisfied": false,
        "exit_ok": broken.status.success(),
        "error": broken_error,
        "candidate_written": true,
        "receipt_path": copy_artifact(&broken_receipt_path, &evidence_dir, "broken--receipt.json"),
        "transcript_path": broken_transcript,
        "report_path": copy_artifact(&out_broken.join("report.json"), &evidence_dir, "broken--report.json"),
    });
    flush_evidence(&evidence_dir, &evidence);

    // ── negative: a candidate manifest that tries to redefine K ─────────────
    let out_redefine = work.join("out-redefine");
    let redefined = reform(
        &home,
        &bundle_a_path,
        &fixture("source-b-redefines-k"),
        &out_redefine,
    );
    assert!(
        !redefined.status.success(),
        "a candidate manifest that redefines K must not be accepted"
    );
    let redefine_stderr = String::from_utf8_lossy(&redefined.stderr).to_string();
    assert!(
        redefine_stderr.contains("candidate_contract_mismatch"),
        "{redefine_stderr}"
    );
    let redefine_report = read_json(&out_redefine.join("report.json"));
    assert_eq!(redefine_report["status"], "rejected");
    assert_eq!(redefine_report["k_preserved"], false);
    assert!(!out_redefine.join("candidate.capsule").exists());
    assert!(!out_redefine.join("receipt.json").exists());
    assert_eq!(sha256(&fs::read(&bundle_a_path).unwrap()), bundle_a_hash);
    evidence["negative_redefines_k"] = json!({
        "exit_ok": redefined.status.success(),
        "error": redefine_report["error"].clone(),
        "k_preserved": false,
        "candidate_written": false,
        "report_path": copy_artifact(&out_redefine.join("report.json"), &evidence_dir, "redefines-k--report.json"),
    });

    evidence["status"] = json!("complete");
    flush_evidence(&evidence_dir, &evidence);
    println!(
        "stateful transformation evidence: {}",
        evidence_dir.join("evidence.json").display()
    );
}
