//! KVM-free Ready-State Capsule Session acceptance.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use capsule::execution_contract::ExecutionId;
use capsule::protocol_bundle::{ProtocolBundleError, StreamingBundleWriter};
use capsule_protocol::{CURRENT_SCHEMA_VERSION, CapsuleDescriptor, IoRecord};
use capsulefs::CasStore;
use snapshot::capsule_state::{
    ReadyStatePortableExportPolicy, ReadyStateStateObjectV1, export_ready_state,
};
use snapshot::{
    ArtifactEnvelopeV1, BuildLayers, BuildReadyStateInput, FakeSnapshotBackend, RestoreContract,
    SanitizerContract, SnapshotBackend,
};

fn scratch_dir() -> tempfile::TempDir {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".ato")
        .join("test-scratch");
    fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("ready-state-session-")
        .tempdir_in(root)
        .unwrap()
}

fn ato(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ato"));
    command.env("ATO_HOME", root.join("ato-home"));
    command.env("HOME", root.join("user-home"));
    command
}

fn make_bundle(root: &Path) -> PathBuf {
    let producer_root = root.join("producer-cas");
    let store = CasStore::open(&producer_root).unwrap();
    let backend = FakeSnapshotBackend::new();
    let execution = ExecutionId::new(format!("blake3:{}", "a".repeat(64))).unwrap();
    let receipt = backend
        .build_ready_state(BuildReadyStateInput {
            store: &store,
            capsule_manifest_hash: format!("blake3:{}", "b".repeat(64)),
            runner_class: None,
            surface_requirement: None,
            layers: BuildLayers {
                rootfs: b"rootfs".to_vec(),
                runtime: Some(b"runtime".to_vec()),
                dependency: Some(b"dependency".to_vec()),
                app: Some(b"app".to_vec()),
                vmstate: vec![1; 32 * 1024],
                memory: vec![2; 256 * 1024],
            },
            restore_contract: RestoreContract::default(),
            sanitizer_contract: SanitizerContract::default(),
            declared_secret_markers: Vec::new(),
            execution_id: Some(execution.as_str().to_string()),
            supervisor: None,
        })
        .unwrap();
    let snapshot =
        snapshot::disposable_lifecycle::build_v1_candidate_manifest(&backend, execution, &receipt)
            .unwrap();
    let envelope = ArtifactEnvelopeV1::accepted(&receipt.manifest, &snapshot).unwrap();
    let state_object =
        ReadyStateStateObjectV1::accepted(receipt.manifest, snapshot, envelope).unwrap();
    let export = export_ready_state(state_object.clone(), &store).unwrap();
    let descriptor = CapsuleDescriptor {
        schema_version: CURRENT_SCHEMA_VERSION,
        base_state: export.state,
        connectors: BTreeMap::new(),
    };
    let bundle = root.join("ready-state.capsule");
    let mut policy = ReadyStatePortableExportPolicy::new(&state_object).unwrap();
    StreamingBundleWriter::write_with_state_roles(
        &bundle,
        &descriptor,
        std::iter::empty::<Result<IoRecord, ProtocolBundleError>>(),
        &export.objects,
        &export.adapter_roles,
        &mut policy,
    )
    .unwrap();
    drop(store);
    fs::remove_dir_all(producer_root).unwrap();
    bundle
}

#[test]
fn ready_state_bundle_restores_runs_and_stops_under_supervisor() {
    let root = scratch_dir();
    let bundle = make_bundle(root.path());
    let start = ato(root.path())
        .args(["internal", "capsule-session", "start"])
        .arg(&bundle)
        .arg("--into")
        .arg(root.path().join("unused-workspace"))
        .arg("--no-attach")
        .output()
        .unwrap();
    assert!(
        start.status.success(),
        "start failed: {}",
        String::from_utf8_lossy(&start.stderr)
    );
    let session_id = String::from_utf8(start.stdout).unwrap().trim().to_string();

    let status = ato(root.path())
        .args(["internal", "capsule-session", "status", &session_id])
        .output()
        .unwrap();
    assert!(status.status.success());
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["lifecycle"], "running");

    for action in ["attach", "suspend"] {
        let output = ato(root.path())
            .args(["internal", "capsule-session", action, &session_id])
            .output()
            .unwrap();
        assert!(!output.status.success(), "{action} must fail closed");
    }
    let branch = ato(root.path())
        .args([
            "internal",
            "capsule-session",
            "branch",
            &session_id,
            "--into",
        ])
        .arg(root.path().join("branch"))
        .arg("--no-attach")
        .output()
        .unwrap();
    assert!(!branch.status.success(), "branch must fail closed");

    let kill = ato(root.path())
        .args(["internal", "capsule-session", "kill", &session_id])
        .output()
        .unwrap();
    assert!(
        kill.status.success(),
        "kill failed: {}",
        String::from_utf8_lossy(&kill.stderr)
    );
    let session_root = root
        .path()
        .join("ato-home/capsule-protocol-sessions")
        .join(&session_id);
    let stored: serde_json::Value =
        serde_json::from_slice(&fs::read(session_root.join("session.json")).unwrap()).unwrap();
    assert_eq!(stored["lifecycle"], "stopped");
    assert_eq!(stored["runtime_profile"]["kind"], "ready_state");
    assert!(!session_root.join("ready-state-overlay").exists());
    assert!(session_root.join("ready-state-cas/blobs/blake3").is_dir());
}
