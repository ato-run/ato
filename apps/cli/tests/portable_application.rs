use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use assert_cmd::Command;
use ato_objects::{
    CapsuleBundleDocument, PortableApplicationBundle, PortableDependencyProfile,
    decode_capsule_bundle_document,
};
use ato_portable_application::bundle_sha256;
use ato_portable_application::instance_snapshot::{
    DATA_JSON_PROTOCOL, INSTANCE_SNAPSHOT_SCHEMA, InstanceSnapshotAssetV1,
    InstanceSnapshotResourceV1, InstanceSnapshotV1, attach_instance_snapshot,
};
use ato_portable_application::portability_export::repack_portable_dependencies;
use serde_json::Value;

fn ato() -> Command {
    let mut command =
        Command::cargo_bin("ato").expect("the ato binary is built for integration tests");
    command.timeout(Duration::from_secs(30));
    command
}

fn ato_with_home(home: &Path) -> Command {
    let mut command = ato();
    command.env("ATO_HOME", home);
    command
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static.capsule")
}

fn multi_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-multi-derivation.capsule")
}

fn datasette_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/datasette-cpu.capsule")
}

fn authored_multi_process_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/portable-multi-process-authored")
}

fn succeeded_or_rejected_by_runtime_admission(output: &Output, receipt: &Path) -> bool {
    if output.status.success() {
        return true;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("selected derivation requires Python 3.12")
            || stderr.contains("portable process sandbox admission failed"),
        "portable process failed for an unexpected reason: {stderr}"
    );
    assert!(
        !receipt.exists(),
        "runtime admission failure must not emit a verification receipt"
    );
    false
}

fn snapshot_fixture(destination: &Path) -> Vec<u8> {
    let source = match decode_capsule_bundle_document(&fs::read(fixture()).unwrap()).unwrap() {
        CapsuleBundleDocument::PortableApplicationV3(bundle) => bundle,
        _ => panic!("static fixture must remain portable v3"),
    };
    let bytes = attach_test_snapshot(source);
    fs::write(destination, &bytes).unwrap();
    bytes
}

fn attach_test_snapshot(source: PortableApplicationBundle) -> Vec<u8> {
    let (_, source) =
        repack_portable_dependencies(&source, PortableDependencyProfile::Cached, &BTreeMap::new())
            .unwrap();
    let saved_data = br#"{"todos":["one","two"]}"#.to_vec();
    let asset = b"portable-photo-bytes".to_vec();
    let saved_data_ref = bundle_sha256(&saved_data);
    let asset_ref = bundle_sha256(&asset);
    let snapshot = InstanceSnapshotV1 {
        schema: INSTANCE_SNAPSHOT_SCHEMA.to_owned(),
        resources: vec![InstanceSnapshotResourceV1 {
            slot: "main".to_owned(),
            protocol: DATA_JSON_PROTOCOL.to_owned(),
            content_ref: saved_data_ref.clone(),
        }],
        assets: vec![InstanceSnapshotAssetV1 {
            alias: "asset-1".to_owned(),
            content_ref: asset_ref.clone(),
            filename: "photo.jpg".to_owned(),
            content_type: "image/jpeg".to_owned(),
            size: asset.len() as u64,
        }],
        asset_bindings: vec![],
    };
    let content = BTreeMap::from([(saved_data_ref, saved_data), (asset_ref, asset)]);
    attach_instance_snapshot(&source, snapshot, &content)
        .unwrap()
        .0
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

#[test]
fn explicitly_selected_process_route_satisfies_the_same_contract_at_runtime() {
    let output = tempfile::tempdir().unwrap();
    let receipt_path = output.path().join("process-receipt.json");
    let process_ref = "sha256:5d1ec4660745130f196102c1bd81828993ab75d69130f0fdf2e1e2598fd9f3cc";
    let run = ato()
        .arg("run")
        .arg(multi_fixture())
        .arg("--derivation")
        .arg(process_ref)
        .arg("--no-open")
        .arg("--verification-receipt")
        .arg(&receipt_path)
        .output()
        .unwrap();
    if !succeeded_or_rejected_by_runtime_admission(&run, &receipt_path) {
        return;
    }
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains(&format!("Route: {process_ref}")));
    assert!(stdout.contains("Runtime: local process"));

    let receipt: Value = serde_json::from_slice(&fs::read(receipt_path).unwrap()).unwrap();
    assert_eq!(
        receipt["bundle_sha256"],
        "sha256:f11e3514e4b6d6a11ef362044cd867d01b8ea3259e126da20a50c07a12cd9cdf"
    );
    assert_eq!(
        receipt["contract_ref"],
        "sha256:94d7bd2900fa01d8b000d1cf095b0f47bfcc279898c830f0542dd53ee99ffdaf"
    );
    assert_eq!(receipt["derivation_ref"], process_ref);
    assert_eq!(receipt["fully_satisfied"], true);
    assert!(
        receipt["observations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|observation| observation["outcome"] == "satisfied")
    );
    assert_eq!(
        receipt["observations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|observation| observation["id"] == "app-proof")
            .unwrap()["evidence"]["body_sha256"],
        "sha256:0facb416176b5bebf18f709cef9fbc062b1378fdde7675a8bca946e93c9f34d8"
    );
}

#[test]
fn a_multi_route_bundle_never_selects_a_derivation_implicitly() {
    ato()
        .arg("run")
        .arg(multi_fixture())
        .arg("--no-open")
        .assert()
        .failure()
        .stderr(predicates::str::contains("select one with --derivation"));

    ato()
        .arg("run")
        .arg(multi_fixture())
        .arg("--derivation")
        .arg(format!("sha256:{}", "0".repeat(64)))
        .arg("--no-open")
        .assert()
        .failure()
        .stderr(predicates::str::contains("is not declared"));
}

#[test]
fn pack_compiles_v2_authoring_and_both_explicit_routes_satisfy_one_contract() {
    let output = tempfile::tempdir().unwrap();
    let bundle_path = output.path().join("authored.capsule");
    ato()
        .arg("pack")
        .arg(authored_multi_process_fixture())
        .arg("--output")
        .arg(&bundle_path)
        .assert()
        .success()
        .stdout(predicates::str::contains("contract_ref=sha256:"))
        .stdout(predicates::str::contains("derivation_ref=sha256:"));

    let bundle = match decode_capsule_bundle_document(&fs::read(&bundle_path).unwrap()).unwrap() {
        CapsuleBundleDocument::PortableApplicationV3(bundle) => bundle,
        other => panic!("pack must emit a portable Application, got {other:?}"),
    };
    assert_eq!(bundle.index.derivations.len(), 2);

    let mut runtime_missing = false;
    for (index, derivation_ref) in bundle.index.derivations.iter().enumerate() {
        let receipt_path = output.path().join(format!("receipt-{index}.json"));
        let run = ato()
            .arg("run")
            .arg(&bundle_path)
            .arg("--derivation")
            .arg(derivation_ref)
            .arg("--no-open")
            .arg("--verification-receipt")
            .arg(&receipt_path)
            .output()
            .unwrap();
        if !succeeded_or_rejected_by_runtime_admission(&run, &receipt_path) {
            runtime_missing = true;
            continue;
        }
        assert!(
            !runtime_missing,
            "one route admitted the shared Python runtime after another rejected it"
        );
        let receipt: Value = serde_json::from_slice(&fs::read(receipt_path).unwrap()).unwrap();
        assert_eq!(receipt["contract_ref"], bundle.index.root_contract_ref);
        assert_eq!(receipt["derivation_ref"], *derivation_ref);
        assert_eq!(receipt["fully_satisfied"], true);
    }
}

#[test]
fn export_plan_preserves_identity_and_refuses_unproven_offline_claim() {
    let cached = ato()
        .arg("export-plan")
        .arg(datasette_fixture())
        .args(["--portability", "cached", "--json"])
        .output()
        .unwrap();
    assert!(cached.status.success());
    let cached: Value = serde_json::from_slice(&cached.stdout).unwrap();
    assert_eq!(cached["existing_bundle_satisfies_profile"], true);

    let offline = ato()
        .arg("export-plan")
        .arg(datasette_fixture())
        .args(["--portability", "offline", "--json"])
        .output()
        .unwrap();
    assert!(offline.status.success());
    let offline: Value = serde_json::from_slice(&offline.stdout).unwrap();
    assert_eq!(cached["contract_ref"], offline["contract_ref"]);
    assert_eq!(cached["derivation_refs"], offline["derivation_refs"]);
    assert_eq!(offline["existing_bundle_satisfies_profile"], false);
    assert_eq!(offline["requires_network_on_clean_host"], Value::Null);
    assert!(
        offline["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker.as_str().unwrap().contains("OCI images"))
    );
}

#[test]
fn v4_bundle_can_be_planned_and_reexported_without_changing_identity() {
    let output = tempfile::tempdir().unwrap();
    let first = output.path().join("datasette-cached-v4.capsule");
    let second = output.path().join("datasette-cached-v4-reexport.capsule");

    ato()
        .arg("export")
        .arg(datasette_fixture())
        .args(["--portability", "cached", "--output"])
        .arg(&first)
        .assert()
        .success();

    let plan = ato()
        .arg("export-plan")
        .arg(&first)
        .args(["--portability", "cached", "--json"])
        .output()
        .unwrap();
    assert!(plan.status.success());
    let plan: Value = serde_json::from_slice(&plan.stdout).unwrap();

    ato()
        .arg("export")
        .arg(&first)
        .args(["--portability", "cached", "--output"])
        .arg(&second)
        .assert()
        .success();

    let first: Value = serde_json::from_slice(&fs::read(first).unwrap()).unwrap();
    let second: Value = serde_json::from_slice(&fs::read(second).unwrap()).unwrap();
    assert_eq!(first["index"]["version"], 4);
    assert_eq!(second["index"]["version"], 4);
    assert_eq!(first["index"]["root_contract_ref"], plan["contract_ref"]);
    assert_eq!(
        first["index"]["root_contract_ref"],
        second["index"]["root_contract_ref"]
    );
    assert_eq!(
        first["index"]["derivations"],
        second["index"]["derivations"]
    );
    assert_eq!(second["portability"]["profile"], "cached");
}

#[test]
fn imported_local_instance_can_stop_and_restart_without_reimporting() {
    let root = tempfile::tempdir().unwrap();
    eprintln!("durable lifecycle: import");
    let imported = ato_with_home(root.path())
        .args(["app", "import"])
        .arg(fixture())
        .output()
        .unwrap();
    assert!(
        imported.status.success(),
        "import failed: {}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let instance: Value = serde_json::from_slice(&imported.stdout).unwrap();
    let instance_id = instance["instance_id"].as_str().unwrap();
    assert_eq!(
        instance["bundle_sha256"],
        "sha256:95b837f4e8ed3c3354a4560e56820030cdb2ba34ba2a7c65aa6d26e139bdd813"
    );
    assert_eq!(
        instance["contract_ref"],
        "sha256:b565e9d771c53ba03548b1a151c0ebbc2b884c1d6ec532f72596ce33a4b741db"
    );
    assert!(
        root.path()
            .join("instances")
            .join(instance_id)
            .join("instance.json")
            .is_file()
    );

    let first_receipt = root.path().join("first-receipt.json");
    eprintln!("durable lifecycle: first start");
    ato_with_home(root.path())
        .args(["app", "start", instance_id, "--no-open"])
        .arg("--verification-receipt")
        .arg(&first_receipt)
        .assert()
        .success();
    eprintln!("durable lifecycle: inspect first run");
    let first_status = ato_with_home(root.path())
        .args(["app", "inspect", instance_id])
        .output()
        .unwrap();
    assert!(first_status.status.success());
    let first_status: Value = serde_json::from_slice(&first_status.stdout).unwrap();
    assert_eq!(first_status["instance"]["instance_id"], instance_id);
    assert_eq!(first_status["active_run"]["status"], "active");
    let first_run = first_status["active_run"]["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let first_receipt: Value = serde_json::from_slice(&fs::read(first_receipt).unwrap()).unwrap();
    assert_eq!(first_receipt["fully_satisfied"], true);

    eprintln!("durable lifecycle: stop first run");
    ato_with_home(root.path())
        .args(["app", "stop", instance_id])
        .assert()
        .success();
    eprintln!("durable lifecycle: inspect stopped instance");
    let stopped = ato_with_home(root.path())
        .args(["app", "inspect", instance_id])
        .output()
        .unwrap();
    let stopped: Value = serde_json::from_slice(&stopped.stdout).unwrap();
    assert!(stopped["active_run"].is_null());

    let second_receipt = root.path().join("second-receipt.json");
    eprintln!("durable lifecycle: second start");
    ato_with_home(root.path())
        .args(["app", "start", instance_id, "--no-open"])
        .arg("--verification-receipt")
        .arg(&second_receipt)
        .assert()
        .success();
    eprintln!("durable lifecycle: inspect second run");
    let second_status = ato_with_home(root.path())
        .args(["app", "inspect", instance_id])
        .output()
        .unwrap();
    let second_status: Value = serde_json::from_slice(&second_status.stdout).unwrap();
    assert_eq!(second_status["instance"], instance);
    assert_eq!(second_status["active_run"]["status"], "active");
    assert_ne!(second_status["active_run"]["run_id"], first_run);
    let second_receipt: Value = serde_json::from_slice(&fs::read(second_receipt).unwrap()).unwrap();
    assert_eq!(
        second_receipt["contract_ref"],
        first_receipt["contract_ref"]
    );
    assert_eq!(
        second_receipt["derivation_ref"],
        first_receipt["derivation_ref"]
    );
    assert_eq!(second_receipt["fully_satisfied"], true);

    eprintln!("durable lifecycle: stop second run");
    ato_with_home(root.path())
        .args(["app", "stop", instance_id])
        .assert()
        .success();
}

#[test]
fn snapshot_bundle_imports_into_independent_asset_namespaces_and_reexports() {
    let root = tempfile::tempdir().unwrap();
    let capsule = root.path().join("saved.capsule");
    let original = snapshot_fixture(&capsule);

    let first = ato_with_home(root.path())
        .args(["app", "import"])
        .arg(&capsule)
        .output()
        .unwrap();
    assert!(first.status.success());
    let first: Value = serde_json::from_slice(&first.stdout).unwrap();
    let second = ato_with_home(root.path())
        .args(["app", "import"])
        .arg(&capsule)
        .output()
        .unwrap();
    assert!(second.status.success());
    let second: Value = serde_json::from_slice(&second.stdout).unwrap();

    let snapshot_digest = first["data_snapshot_ref"]
        .as_str()
        .unwrap()
        .strip_prefix("sha256:")
        .unwrap();
    let first_snapshot_path = root
        .path()
        .join("instances")
        .join(first["instance_id"].as_str().unwrap())
        .join("snapshots")
        .join(snapshot_digest)
        .join("snapshot.json");
    let second_snapshot_path = root
        .path()
        .join("instances")
        .join(second["instance_id"].as_str().unwrap())
        .join("snapshots")
        .join(snapshot_digest)
        .join("snapshot.json");
    let first_snapshot: Value =
        serde_json::from_slice(&fs::read(first_snapshot_path).unwrap()).unwrap();
    let second_snapshot: Value =
        serde_json::from_slice(&fs::read(second_snapshot_path).unwrap()).unwrap();
    assert_ne!(first["instance_id"], second["instance_id"]);
    assert_ne!(
        first_snapshot["assets"][0]["asset_id"],
        second_snapshot["assets"][0]["asset_id"]
    );
    assert_eq!(
        first_snapshot["assets"][0]["content_ref"],
        second_snapshot["assets"][0]["content_ref"]
    );

    let exported = root.path().join("exported.capsule");
    ato_with_home(root.path())
        .args([
            "app",
            "export",
            first["instance_id"].as_str().unwrap(),
            "--output",
        ])
        .arg(&exported)
        .assert()
        .success();
    assert_eq!(fs::read(exported).unwrap(), original);
}

#[test]
fn snapshot_bundle_start_records_the_installed_snapshot_in_its_receipt() {
    let root = tempfile::tempdir().unwrap();
    let capsule = root.path().join("saved.capsule");
    snapshot_fixture(&capsule);
    let imported = ato_with_home(root.path())
        .args(["app", "import"])
        .arg(&capsule)
        .output()
        .unwrap();
    let instance: Value = serde_json::from_slice(&imported.stdout).unwrap();
    let instance_id = instance["instance_id"].as_str().unwrap();

    ato_with_home(root.path())
        .args(["app", "start", instance_id, "--no-open"])
        .assert()
        .success();

    let instance_root = root.path().join("instances").join(instance_id);
    let active: Value =
        serde_json::from_slice(&fs::read(instance_root.join("active-run.json")).unwrap()).unwrap();
    let receipt: Value = serde_json::from_slice(
        &fs::read(
            instance_root
                .join("runs")
                .join(active["run_id"].as_str().unwrap())
                .join(active["receipt_path"].as_str().unwrap()),
        )
        .unwrap(),
    )
    .unwrap();

    assert_eq!(receipt["fully_satisfied"], true);
    assert_eq!(
        receipt["observations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|observation| observation["id"] == "instance-snapshot")
            .unwrap()["outcome"],
        "satisfied"
    );
    ato_with_home(root.path())
        .args(["app", "stop", instance_id])
        .assert()
        .success();
}

#[test]
fn process_snapshot_start_fails_before_claiming_a_restore() {
    let root = tempfile::tempdir().unwrap();
    let capsule = root.path().join("process-saved.capsule");
    let source = match decode_capsule_bundle_document(&fs::read(multi_fixture()).unwrap()).unwrap()
    {
        CapsuleBundleDocument::PortableApplicationV3(bundle) => bundle,
        _ => panic!("multi-route fixture must remain portable v3"),
    };
    fs::write(&capsule, attach_test_snapshot(source)).unwrap();
    let process_ref = "sha256:5d1ec4660745130f196102c1bd81828993ab75d69130f0fdf2e1e2598fd9f3cc";
    let imported = ato_with_home(root.path())
        .args(["app", "import"])
        .arg(&capsule)
        .args(["--derivation", process_ref])
        .output()
        .unwrap();
    assert!(imported.status.success());
    let instance: Value = serde_json::from_slice(&imported.stdout).unwrap();

    ato_with_home(root.path())
        .args([
            "app",
            "start",
            instance["instance_id"].as_str().unwrap(),
            "--no-open",
        ])
        .assert()
        .failure();
    let runs = root
        .path()
        .join("instances")
        .join(instance["instance_id"].as_str().unwrap())
        .join("runs");
    let log = fs::read_dir(runs)
        .unwrap()
        .next()
        .map(|entry| fs::read_to_string(entry.unwrap().path().join("output.log")).unwrap())
        .unwrap();

    assert!(log.contains(
        "local dynamic Instance snapshot restore supports only its declared filesystem state"
    ));
}
