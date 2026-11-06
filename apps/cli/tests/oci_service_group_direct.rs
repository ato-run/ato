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

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
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

/// `docker <arguments>` output, one trimmed non-empty line per entry.
fn docker_lines(arguments: &[&str]) -> Vec<String> {
    let output = Command::new("docker")
        .args(arguments)
        .output()
        .expect("docker runs");
    assert!(
        output.status.success(),
        "docker {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn docker_text(arguments: &[&str]) -> String {
    let output = Command::new("docker")
        .args(arguments)
        .output()
        .expect("docker runs");
    assert!(
        output.status.success(),
        "docker {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// `id name` pairs, as `docker ps`/`docker network ls` print them.
fn docker_pairs(arguments: &[&str]) -> Vec<(String, String)> {
    docker_lines(arguments)
        .into_iter()
        .map(|line| {
            let (id, name) = line
                .split_once(' ')
                .unwrap_or_else(|| panic!("docker printed no name: {line}"));
            (id.to_owned(), name.to_owned())
        })
        .collect()
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

    // The Run owns containers and a network while it is up. Watch them while it
    // runs: a direct Run must not publish a container Port, and nothing may
    // survive the stop. Both are checked against this Run's own names, so a
    // parallel test's containers cannot be mistaken for ours.
    let derivation_hex = derivation_ref
        .strip_prefix("sha256:")
        .expect("the derivation ref is a sha256 ref");
    let owned_prefix = format!("ato-sha256{}", &derivation_hex[..20]);
    let containers_before = docker_pairs(&["ps", "-a", "--format", "{{.ID}} {{.Names}}"]);

    let mut child = Command::new(assert_cmd::cargo::cargo_bin("ato"))
        .env("ATO_HOME", &home)
        .arg("run")
        .arg(&capsule)
        .args(["--no-open", "--verification-receipt"])
        .arg(&receipt_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run starts");
    let mut published = Vec::new();
    let mut inspected = BTreeSet::new();
    let status = loop {
        if let Some(status) = child.try_wait().expect("run is waited for") {
            break status;
        }
        for (id, name) in docker_pairs(&["ps", "--format", "{{.ID}} {{.Names}}"]) {
            let ours = name.starts_with(&owned_prefix)
                || (name.starts_with("ato-relay-")
                    && !containers_before.iter().any(|(before, _)| before == &id));
            if !ours || !inspected.insert(id.clone()) {
                continue;
            }
            let ports = docker_text(&["port", &id]);
            let bindings = docker_text(&[
                "inspect",
                "--format",
                "{{json .HostConfig.PortBindings}}",
                &id,
            ]);
            if !ports.trim().is_empty() || bindings.trim() != "{}" {
                published.push(format!(
                    "{name}: docker port={:?} PortBindings={}",
                    ports.trim(),
                    bindings.trim()
                ));
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let output = child.wait_with_output().expect("run output");
    assert!(
        status.success(),
        "run failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        published.is_empty(),
        "a direct Run must not publish a container Port: {published:#?}"
    );
    let containers_after = docker_pairs(&["ps", "-a", "--format", "{{.ID}} {{.Names}}"]);
    let networks_after = docker_pairs(&["network", "ls", "--format", "{{.ID}} {{.Name}}"]);
    let leftover_containers: Vec<&str> = containers_after
        .iter()
        .filter(|(id, name)| {
            name.starts_with(&owned_prefix)
                || (name.starts_with("ato-relay-")
                    && !containers_before.iter().any(|(before, _)| before == id))
        })
        .map(|(_, name)| name.as_str())
        .collect();
    assert!(
        leftover_containers.is_empty(),
        "containers left after the Run: {leftover_containers:?}"
    );
    let leftover_networks: Vec<&str> = networks_after
        .iter()
        .filter(|(_, name)| name.starts_with(&owned_prefix))
        .map(|(_, name)| name.as_str())
        .collect();
    assert!(
        leftover_networks.is_empty(),
        "networks left after the Run: {leftover_networks:?}"
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
