//! Linux direct-path acceptance for the OCI Service Group sample.
//!
//! macOS reaches the sample through the Run's relay; a native Linux Docker
//! daemon reaches the containers directly. This test runs the exact same
//! bundle bytes and pins the identity the macOS acceptance recorded, so the
//! two hosts are shown to agree on the Capsule while differing only in the
//! materialization.
//!
//! On a host without Docker it prints why it cannot run. With
//! `ATO_REQUIRE_DOCKER_E2E=1` (the Linux CI job) a missing daemon is a
//! failure: a skipped acceptance is not a passing one.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use assert_cmd::Command as AtoCommand;
use serde_json::Value;

/// The identity the same sample produced on macOS + Docker Desktop (relay
/// path), recorded in `docs/ops/1690-oci-service-group-connector.md`.
const MACOS_BUNDLE_SHA256: &str =
    "sha256:2c88d86cc1c10039a9454b57ad0962986fabb0aa951970af9567c6b4dcb0a902";
const MACOS_CONTRACT_REF: &str =
    "sha256:68228e9bf2bcadc375cc70747bb23e672290a1a16774d2ed6df387488db5e529";
const MACOS_DERIVATION_REF: &str =
    "sha256:a6ff45c1fb10cb349003b0703a0b4fdf7633879a7af361b9e04997514108148c";

fn docker_available() -> bool {
    Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn require_docker() -> bool {
    std::env::var("ATO_REQUIRE_DOCKER_E2E").as_deref() == Ok("1")
}

fn field(stdout: &str, name: &str) -> String {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix(name))
        .unwrap_or_else(|| panic!("`{name}` is missing from:\n{stdout}"))
        .trim()
        .to_owned()
}

#[test]
fn the_sample_runs_directly_on_a_linux_docker_host() {
    if !cfg!(target_os = "linux") {
        println!(
            "skipped: the direct path needs a native Linux Docker daemon, and this host is not \
             Linux; the macOS relay path is covered by the acceptance receipt"
        );
        return;
    }
    if !docker_available() {
        if require_docker() {
            panic!("ATO_REQUIRE_DOCKER_E2E=1 but no Docker daemon is reachable on this runner");
        }
        println!(
            "skipped: no Docker daemon is reachable; set ATO_REQUIRE_DOCKER_E2E=1 to make that a \
             failure"
        );
        return;
    }

    let root = tempfile::tempdir().expect("temporary root");
    let home = root.path().join("home");
    fs::create_dir_all(&home).expect("ato home");
    let capsule = root.path().join("oci.capsule");
    let receipt_path = root.path().join("receipt.json");
    let sample =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/oci-service-group-proof");
    let ato = || {
        let mut command = AtoCommand::cargo_bin("ato").expect("the ato binary");
        command.timeout(Duration::from_secs(600));
        command.env("ATO_HOME", &home);
        command
    };

    let pack = ato()
        .arg("pack")
        .arg(&sample)
        .arg("-o")
        .arg(&capsule)
        .output()
        .expect("pack runs");
    assert!(
        pack.status.success(),
        "pack failed: {}",
        String::from_utf8_lossy(&pack.stderr)
    );
    let packed = String::from_utf8_lossy(&pack.stdout).into_owned();
    let bundle_sha256 = field(&packed, "bundle_sha256=");
    let contract_ref = field(&packed, "contract_ref=");
    let derivation_ref = field(&packed, "derivation_ref=");
    // The same Capsule, not merely a similar one: identical bytes, identical
    // Contract and identical Derivation on both hosts.
    assert_eq!(bundle_sha256, MACOS_BUNDLE_SHA256, "the bundle bytes moved");
    assert_eq!(contract_ref, MACOS_CONTRACT_REF, "K moved");
    assert_eq!(derivation_ref, MACOS_DERIVATION_REF, "the selected D moved");

    let run = ato()
        .arg("run")
        .arg(&capsule)
        .args(["--no-open", "--verification-receipt"])
        .arg(&receipt_path)
        .output()
        .expect("run runs");
    assert!(
        run.status.success(),
        "run failed: {}\n{}",
        String::from_utf8_lossy(&run.stderr),
        String::from_utf8_lossy(&run.stdout)
    );
    let receipt: Value =
        serde_json::from_slice(&fs::read(&receipt_path).expect("receipt")).expect("receipt json");

    assert_eq!(receipt["fully_satisfied"], true);
    assert_eq!(receipt["contract_ref"], MACOS_CONTRACT_REF);
    assert_eq!(receipt["derivation_ref"], MACOS_DERIVATION_REF);
    // Direct, not relay: the daemon's container network is reachable here, so
    // no connector evidence is recorded and the receipt is not /4.
    assert!(
        receipt["execution"]["connector"].is_null(),
        "a direct Run must not record a connector: {}",
        receipt["execution"]
    );
    assert_ne!(receipt["schema"], "ato.contract-verification-receipt/4");

    // The same acceptance authority the hosted validator uses.
    let bytes = fs::read(&capsule).expect("capsule");
    let bundle = ato_objects::decode_portable_application_bundle(&bytes).expect("bundle");
    let route_ref = derivation_ref.clone();
    let route = ato_portable_application::validate_bundle_for_derivation(&bundle, &route_ref)
        .expect("the selected route");
    let attempt_id = receipt["execution"]["attempt_id"]
        .as_str()
        .expect("the receipt names its attempt")
        .to_owned();
    let assignment = ato_formation::receipt::ReceiptAssignment {
        contract: &route.contract,
        contract_ref: &contract_ref,
        derivation_ref: &route_ref,
        attempt_id: &attempt_id,
        request_id: None,
    };
    ato_formation::receipt::accept_receipt_json(&assignment, &receipt)
        .expect("the receipt is accepted");

    println!(
        "direct E2E ok: bundle={bundle_sha256} contract={contract_ref} derivation={derivation_ref} \
         schema={}",
        receipt["schema"]
    );
}
