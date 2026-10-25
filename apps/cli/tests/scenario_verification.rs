//! Scenario verifier v1 — end-to-end proof on a Linux Runtime.
//!
//! A `.capsule` whose Contract pins an `ato.scenario/1` object is packed, run,
//! and verified through the real CLI:
//!
//! ```text
//! source ──ato pack──▶ bundle (K: root HTTP + scenario ref) ──ato run──▶ receipt
//!                                                             │
//!                                  scenario executes stateful steps against
//!                                  the candidate (login/cookie/create/update/
//!                                  unauthorized/delete/read-after-delete)
//! ```
//!
//! and the negative matrix:
//!
//! - a candidate that does not meet a step's expectation is refused
//!   (`scenario_step_failed`), with the receipt kept as evidence;
//! - a candidate restored from a state slot is refused before the scenario runs
//!   (`scenario_isolation_unavailable`);
//! - a candidate D from a different source under the frozen K is accepted by
//!   `ato reform`, the same K is preserved, and the scenario executes.
//!
//! Everything needs a Linux Runtime (bubblewrap + the provisioned Python
//! 3.12), so the crate is Linux-only and compiles to an empty test binary
//! elsewhere. Evidence lands in the uploaded directory named below.

#![cfg(target_os = "linux")]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use assert_cmd::Command;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const EVIDENCE_SCHEMA: &str = "ato.scenario-verifier-evidence/1";
const SCENARIO_CONTRACT_VERIFIER: &str = "ato.contract.scenario@1";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scenario-v1")
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

/// Where the evidence index and copied artifacts live. `CARGO_TARGET_DIR` is
/// honoured so a shared target keeps the evidence beside the build; CI (no
/// override) lands it at `<repo>/target/scenario-verifier-v1-evidence`.
fn evidence_root() -> PathBuf {
    let base = match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => repo_root().join("target"),
    };
    base.join("scenario-verifier-v1-evidence")
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

fn scenario_observation(receipt: &Value) -> &Value {
    receipt["observations"]
        .as_array()
        .expect("observations")
        .iter()
        .find(|observation| observation["id"] == "crud")
        .expect("the receipt carries the scenario observation")
}

fn failure_of(observation: &Value) -> &str {
    observation["failure"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a failure, got {observation}"))
}

/// The runtime scratch survives a failed Run (kept under `ATO_HOME/cache`);
/// its receipt is the attempt's own evidence, never a published receipt.
fn latest_runtime_receipt_for_derivation(
    home: &Path,
    derivation_ref: &str,
) -> (Value, PathBuf) {
    let cache = home.join("cache");
    // Two negative Runs in this test can share a Contract and differ only by
    // Derivation; choosing by mtime would race them. The receipt names the D
    // it covers, so select on that identity.
    let mut matches: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    if let Ok(entries) = fs::read_dir(&cache) {
        for entry in entries.flatten() {
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with("ato-portable-run-")
            {
                continue;
            }
            let receipt = entry.path().join("verification-receipt.json");
            if !receipt.is_file() {
                continue;
            }
            let value = read_json(&receipt);
            if value["derivation_ref"] != derivation_ref {
                continue;
            }
            let Ok(modified) = receipt.metadata().and_then(|metadata| metadata.modified()) else {
                continue;
            };
            matches.push((modified, receipt));
        }
    }
    matches.sort_by_key(|(modified, _)| *modified);
    let (_, path) = matches
        .pop()
        .expect("a failed Run leaves its attempt receipt under the runtime scratch");
    let value = read_json(&path);
    (value, path)
}

fn derivation_of(bundle: &ato_objects::PortableApplicationBundle) -> String {
    assert_eq!(bundle.index.derivations.len(), 1, "one route");
    bundle.index.derivations[0].clone()
}

fn transcript_beside(receipt: &Path) -> PathBuf {
    receipt
        .parent()
        .expect("receipt has a directory")
        .join("scenario-transcripts/crud.json")
}

#[test]
fn a_scenario_contract_is_executed_verified_and_reformable() {
    let evidence_dir = evidence_root();
    fs::create_dir_all(&evidence_dir).expect("create evidence directory");
    let work_dir = tempfile::tempdir().expect("work dir");
    let work = work_dir.path();
    let home = work.join("home");
    fs::create_dir_all(&home).expect("home");

    let mut evidence = json!({
        "schema": EVIDENCE_SCHEMA,
        "fixtures": {
            "good": fixture("good").display().to_string(),
            "bad": fixture("bad").display().to_string(),
            "isolation": fixture("isolation").display().to_string(),
            "candidate": fixture("candidate").display().to_string(),
        },
        "status": "running",
        "non_claims": [
            "fully_satisfied means the defined Contract K was satisfied, not that the application is complete or secure",
            "the verifier does not prove the candidate cannot reach the network; it proves the verifier itself only addressed candidate endpoints",
            "formation and hosted validation do not execute scenarios in v1 and fail closed when K requires one",
        ],
    });
    flush_evidence(&evidence_dir, &evidence);

    // ── Pack: the scenario object is content-addressed into K ───────────────
    let good_bundle_path = work.join("good.capsule");
    pack(&home, &fixture("good"), &good_bundle_path);
    let bundle_bytes = fs::read(&good_bundle_path).expect("good bundle");
    let bundle = decode_portable(&bundle_bytes);
    assert_eq!(bundle.index.derivations.len(), 1, "one route");
    let derivation_ref = bundle.index.derivations[0].clone();
    let (_, validated) =
        ato_portable_application::validate_bytes_for_derivation(&bundle_bytes, &derivation_ref)
            .expect("validate the scenario bundle");
    assert_eq!(validated.scenarios.len(), 1, "one scenario plan");
    let scenario = &validated.scenarios[0];
    assert_eq!(scenario.requirement_id, "crud");
    assert_eq!(scenario.port, "app.http");
    let scenario_ref = scenario.scenario_ref.to_string();
    let requirement = validated
        .contract
        .requirements
        .iter()
        .find(|requirement| requirement.id == "crud")
        .expect("scenario requirement in K");
    assert_eq!(requirement.verifier, SCENARIO_CONTRACT_VERIFIER);
    assert_eq!(requirement.digest.as_deref(), Some(scenario_ref.as_str()));

    // ── Run: the scenario executes against the fresh candidate ──────────────
    let receipt_path = work.join("good-receipt.json");
    let run_output = run(&home, &good_bundle_path, &receipt_path);
    assert_success(&run_output, "ato run scenario bundle");
    let receipt = read_json(&receipt_path);
    assert_eq!(receipt["fully_satisfied"], true, "receipt: {receipt}");
    assert_eq!(receipt["contract_ref"], validated.contract_ref.as_str());
    assert_eq!(receipt["derivation_ref"], derivation_ref.as_str());
    assert_eq!(receipt["target"]["kind"], "cli-local");
    let observation = scenario_observation(&receipt);
    assert_eq!(observation["outcome"], "satisfied");
    let scenario_evidence = &observation["evidence"];
    assert_eq!(scenario_evidence["path"], "scenario:crud");
    assert_eq!(scenario_evidence["digest"], scenario_ref.as_str());
    let transcript_digest = scenario_evidence["body_sha256"]
        .as_str()
        .expect("transcript digest");
    assert!(
        transcript_digest.starts_with("sha256:") && transcript_digest.len() == 71,
        "{transcript_digest}"
    );
    assert!(scenario_evidence.get("method").is_none());
    assert!(scenario_evidence.get("status").is_none());
    assert!(scenario_evidence.get("input").is_none());

    // The transcript sidecar lands next to the receipt, and its canonical
    // transcript hashes to exactly the digest the receipt carries.
    let sidecar = work.join("good-receipt.json.scenarios/crud.json");
    assert!(sidecar.is_file(), "missing scenario sidecar {sidecar:?}");
    let raw = read_json(&sidecar);
    assert_eq!(raw["schema"], "ato.scenario-transcript-raw/1");
    assert_eq!(raw["scenario_ref"], scenario_ref.as_str());
    assert_eq!(raw["transcript_digest"], transcript_digest);
    assert_eq!(raw["result"], "satisfied");
    let canonical = raw["canonical_transcript"].clone();
    assert_eq!(
        sha256(&serde_jcs::to_vec(&canonical).expect("canonical transcript")),
        transcript_digest,
        "the recorded transcript must hash to the receipt's digest"
    );
    let steps = canonical["steps"].as_array().expect("transcript steps");
    assert_eq!(steps.len(), 6, "the whole scenario ran: {canonical}");
    assert_eq!(
        canonical["steps"][3]["outcome"], "satisfied",
        "the cleared-cookie negative case passed"
    );

    evidence["run"] = json!({
        "bundle_sha256": sha256(&bundle_bytes),
        "contract_ref": validated.contract_ref.to_string(),
        "derivation_ref": derivation_ref,
        "receipt_sha256": sha256(&receipt_path_bytes(&receipt_path)),
        "receipt_path": copy_artifact(&receipt_path, &evidence_dir, "good--receipt.json"),
        "transcript_path": copy_artifact(&sidecar, &evidence_dir, "good--crud-transcript.json"),
        "scenario": {
            "id": "crud",
            "ref": scenario_ref,
            "verifier_version": 1,
            "transcript_digest": transcript_digest,
        },
        "fully_satisfied": true,
    });
    flush_evidence(&evidence_dir, &evidence);

    // ── Negative: a step's expectation is not met ───────────────────────────
    let bad_bundle_path = work.join("bad.capsule");
    pack(&home, &fixture("bad"), &bad_bundle_path);
    let bad_receipt_path = work.join("bad-receipt.json");
    let bad_run = run(&home, &bad_bundle_path, &bad_receipt_path);
    assert!(
        !bad_run.status.success(),
        "a candidate that misses a step expectation must not exit 0"
    );
    let bad_stderr = String::from_utf8_lossy(&bad_run.stderr).to_string();
    assert!(
        bad_stderr.contains("did not fully satisfy observation crud"),
        "{bad_stderr}"
    );
    // A failed Run publishes no receipt: the attempt record and the runtime
    // scratch are its evidence.
    assert!(
        !bad_receipt_path.exists(),
        "a failed ato run must not publish a verification receipt"
    );
    let bad_derivation = derivation_of(&decode_portable(
        &fs::read(&bad_bundle_path).expect("bad bundle"),
    ));
    let (bad_receipt, bad_attempt_receipt) =
        latest_runtime_receipt_for_derivation(&home, &bad_derivation);
    assert_eq!(bad_receipt["fully_satisfied"], false);
    let bad_observation = scenario_observation(&bad_receipt);
    assert_eq!(bad_observation["outcome"], "failed");
    let bad_failure = failure_of(bad_observation).to_owned();
    assert!(
        bad_failure.starts_with("scenario_step_failed:"),
        "{bad_failure}"
    );
    assert!(bad_failure.contains("unauthorized-read"), "{bad_failure}");
    evidence["negative_broken_behavior"] = json!({
        "fully_satisfied": false,
        "exit_ok": bad_run.status.success(),
        "failure": bad_failure,
        "published_receipt": false,
        "attempt_receipt_path": copy_artifact(
            &bad_attempt_receipt,
            &evidence_dir,
            "bad--attempt-receipt.json",
        ),
        "transcript_path": copy_artifact(
            &transcript_beside(&bad_attempt_receipt),
            &evidence_dir,
            "bad--crud-transcript.json",
        ),
    });
    flush_evidence(&evidence_dir, &evidence);

    // ── Negative: a stateful candidate is refused before the scenario runs ──
    let isolation_bundle_path = work.join("isolation.capsule");
    pack(&home, &fixture("isolation"), &isolation_bundle_path);
    let isolation_receipt_path = work.join("isolation-receipt.json");
    let isolation_run = run(&home, &isolation_bundle_path, &isolation_receipt_path);
    assert!(
        !isolation_run.status.success(),
        "a stateful candidate must not run a scenario in v1"
    );
    assert!(
        !isolation_receipt_path.exists(),
        "a failed ato run must not publish a verification receipt"
    );
    let isolation_derivation = derivation_of(&decode_portable(
        &fs::read(&isolation_bundle_path).expect("isolation bundle"),
    ));
    let (isolation_receipt, isolation_attempt_receipt) =
        latest_runtime_receipt_for_derivation(&home, &isolation_derivation);
    assert_eq!(isolation_receipt["fully_satisfied"], false);
    let isolation_failure = failure_of(scenario_observation(&isolation_receipt)).to_owned();
    assert!(
        isolation_failure.starts_with("scenario_isolation_unavailable:"),
        "{isolation_failure}"
    );
    evidence["negative_isolation"] = json!({
        "fully_satisfied": false,
        "exit_ok": isolation_run.status.success(),
        "failure": isolation_failure,
        "published_receipt": false,
        "attempt_receipt_path": copy_artifact(
            &isolation_attempt_receipt,
            &evidence_dir,
            "isolation--attempt-receipt.json",
        ),
    });
    flush_evidence(&evidence_dir, &evidence);

    // ── Frozen K: another source satisfies the same scenario Contract ───────
    let out = work.join("out");
    let reformed = ato(&home)
        .arg("reform")
        .arg(&good_bundle_path)
        .arg(fixture("candidate"))
        .arg("--out")
        .arg(&out)
        .output()
        .expect("run ato reform");
    assert_success(&reformed, "ato reform under the frozen scenario K");
    let report = read_json(&out.join("report.json"));
    assert_eq!(report["status"], "accepted", "report: {report}");
    assert_eq!(report["k_preserved"], true);
    assert_eq!(report["fully_satisfied"], true);
    assert_eq!(
        report["frozen_contract_ref"],
        validated.contract_ref.as_str()
    );
    let candidate_receipt = read_json(&out.join("receipt.json"));
    assert_eq!(
        candidate_receipt["contract_ref"],
        validated.contract_ref.as_str()
    );
    assert_eq!(candidate_receipt["fully_satisfied"], true);
    assert_ne!(
        candidate_receipt["derivation_ref"],
        derivation_ref.as_str(),
        "a different source is a different Derivation"
    );
    assert_eq!(
        scenario_observation(&candidate_receipt)["outcome"],
        "satisfied"
    );
    let reform_sidecar = out.join("scenario-transcripts/crud.json");
    assert!(
        reform_sidecar.is_file(),
        "ato reform must keep the scenario transcript: {reform_sidecar:?}"
    );
    evidence["reform"] = json!({
        "accepted": true,
        "k_preserved": true,
        "frozen_contract_ref": validated.contract_ref.to_string(),
        "new_derivation_ref": report["new_derivation_ref"].clone(),
        "receipt_path": copy_artifact(&out.join("receipt.json"), &evidence_dir, "reform--receipt.json"),
        "transcript_path": copy_artifact(&reform_sidecar, &evidence_dir, "reform--crud-transcript.json"),
        "fully_satisfied": true,
    });
    evidence["status"] = json!("complete");
    flush_evidence(&evidence_dir, &evidence);
}

fn receipt_path_bytes(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}
