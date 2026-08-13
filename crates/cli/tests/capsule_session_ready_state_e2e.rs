//! KVM-free Ready-State Capsule Session acceptance.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use capsule::execution_contract::ExecutionId;
use capsule::protocol_bundle::{
    AllowAllPortableExportPolicy, PortableObjectRole, ProtocolBundleError, StreamingBundleWriter,
};
use capsule_protocol::{
    CURRENT_SCHEMA_VERSION, CapsuleDescriptor, ConnectorDef, ConnectorId, Direction, IoRecord,
    Payload, ProtocolId, RecordKindId, StateTypeId,
};
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

struct PublicSessionCleanup(PathBuf);

impl Drop for PublicSessionCleanup {
    fn drop(&mut self) {
        let Ok(output) = ato(&self.0).args(["decap", "list", "--json"]).output() else {
            return;
        };
        let Ok(rows) = serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout) else {
            return;
        };
        for row in rows {
            let Some(name) = row["name"].as_str() else {
                continue;
            };
            let _ = ato(&self.0).args(["decap", "stop", name]).output();
        }
    }
}

struct BundleFixtures {
    valid: PathBuf,
    connector: PathBuf,
    record: PathBuf,
    unknown_state: PathBuf,
}

fn make_bundles(root: &Path) -> BundleFixtures {
    let producer_root = root.join("producer-cas");
    let store = CasStore::open(&producer_root).unwrap();
    let backend = FakeSnapshotBackend::new();
    let host_runner_class = backend.host_runner_class().unwrap();
    let execution = ExecutionId::new(format!("blake3:{}", "a".repeat(64))).unwrap();
    let receipt = backend
        .build_ready_state(BuildReadyStateInput {
            store: &store,
            capsule_manifest_hash: format!("blake3:{}", "b".repeat(64)),
            runner_class: Some(host_runner_class),
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
    let bundle = root.join("prepared-ready-state.capsule");
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

    let connector_id = ConnectorId::parse("terminal.main").unwrap();
    let mut connector_descriptor = descriptor.clone();
    connector_descriptor.connectors.insert(
        connector_id.clone(),
        ConnectorDef {
            protocol: ProtocolId::parse("ato.io.pty@1").unwrap(),
            config_ref: None,
        },
    );
    let connector = root.join("ready-state-connector.capsule");
    StreamingBundleWriter::write_with_state_roles(
        &connector,
        &connector_descriptor,
        std::iter::empty::<Result<IoRecord, ProtocolBundleError>>(),
        &export.objects,
        &export.adapter_roles,
        &mut AllowAllPortableExportPolicy,
    )
    .unwrap();
    let record = root.join("ready-state-record.capsule");
    StreamingBundleWriter::write_with_state_roles(
        &record,
        &connector_descriptor,
        [Ok(IoRecord {
            seq: 1,
            offset_ns: None,
            observed_at_unix_ns: None,
            connector: connector_id,
            direction: Direction::Ingress,
            kind: RecordKindId::parse("stdin").unwrap(),
            payload: Payload::Inline(b"input".to_vec()),
        })],
        &export.objects,
        &export.adapter_roles,
        &mut AllowAllPortableExportPolicy,
    )
    .unwrap();
    let mut unknown_descriptor = descriptor;
    let unknown_type = StateTypeId::parse("ato.state.unknown@1").unwrap();
    unknown_descriptor.base_state.state_type = unknown_type.clone();
    let unknown_roles = export
        .adapter_roles
        .keys()
        .map(|reference| {
            (
                reference.clone(),
                vec![PortableObjectRole::StateAdapterObject {
                    state_type: unknown_type.clone(),
                }],
            )
        })
        .collect();
    let unknown_state = root.join("ready-state-unknown.capsule");
    StreamingBundleWriter::write_with_state_roles(
        &unknown_state,
        &unknown_descriptor,
        std::iter::empty::<Result<IoRecord, ProtocolBundleError>>(),
        &export.objects,
        &unknown_roles,
        &mut AllowAllPortableExportPolicy,
    )
    .unwrap();
    drop(store);
    fs::remove_dir_all(producer_root).unwrap();
    BundleFixtures {
        valid: bundle,
        connector,
        record,
        unknown_state,
    }
}

#[test]
fn developer_opens_lists_and_stops_a_shared_prepared_ready_state_capsule() {
    let root = scratch_dir();
    let bundles = make_bundles(root.path());
    let _cleanup = PublicSessionCleanup(root.path().to_path_buf());

    let first = ato(root.path())
        .args(["decap", "start"])
        .arg(&bundles.valid)
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "public start failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_stdout = String::from_utf8(first.stdout).unwrap();
    assert_eq!(first_stdout, "Starting prepared-ready-state...\nReady.\n");
    assert!(!first_stdout.contains("session-"));

    let second = ato(root.path())
        .args(["decap", "start"])
        .arg(&bundles.valid)
        .arg("--detach")
        .output()
        .unwrap();
    assert!(second.status.success());
    assert_eq!(
        String::from_utf8(second.stdout).unwrap(),
        "Starting prepared-ready-state-2...\nReady.\n"
    );

    let list = ato(root.path()).args(["decap", "list"]).output().unwrap();
    assert!(list.status.success());
    let list = String::from_utf8(list.stdout).unwrap();
    assert!(list.starts_with("NAME\tSTATE\tAGE\tSOURCE\n"));
    assert!(list.contains("prepared-ready-state\trunning\t"));
    assert!(list.contains("prepared-ready-state-2\trunning\t"));
    assert!(list.contains("prepared-ready-state.capsule"));
    assert!(!list.contains("session-"));

    let list_json = ato(root.path())
        .args(["decap", "list", "--json"])
        .output()
        .unwrap();
    assert!(list_json.status.success());
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&list_json.stdout).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["name"], "prepared-ready-state");
    assert_eq!(rows[0]["state"], "running");
    assert!(
        rows[0]["session_id"]
            .as_str()
            .unwrap()
            .starts_with("session-")
    );

    let attach = ato(root.path())
        .args(["decap", "attach", "prepared-ready-state"])
        .output()
        .unwrap();
    assert!(!attach.status.success());
    assert!(
        String::from_utf8_lossy(&attach.stderr)
            .contains("ReadyStateConnectorAttachmentUnsupported"),
        "unexpected attach error: {}",
        String::from_utf8_lossy(&attach.stderr)
    );

    let internal_id = rows[0]["session_id"].as_str().unwrap();
    let internal_id_stop = ato(root.path())
        .args(["decap", "stop", internal_id])
        .output()
        .unwrap();
    assert!(!internal_id_stop.status.success());

    let force = ato(root.path())
        .args(["decap", "stop", "prepared-ready-state", "--force"])
        .output()
        .unwrap();
    assert!(
        !force.status.success(),
        "unsupported --force must not parse"
    );

    let stop = ato(root.path())
        .args(["decap", "stop", "prepared-ready-state"])
        .output()
        .unwrap();
    assert!(stop.status.success());

    let reused = ato(root.path())
        .args(["decap", "start"])
        .arg(&bundles.valid)
        .output()
        .unwrap();
    assert!(reused.status.success());
    assert_eq!(
        String::from_utf8(reused.stdout).unwrap(),
        "Starting prepared-ready-state...\nReady.\n"
    );

    for name in ["prepared-ready-state", "prepared-ready-state-2"] {
        let stop = ato(root.path())
            .args(["decap", "stop", name])
            .output()
            .unwrap();
        assert!(
            stop.status.success(),
            "public stop failed for {name}: {}",
            String::from_utf8_lossy(&stop.stderr)
        );
        assert_eq!(
            String::from_utf8(stop.stdout).unwrap(),
            format!("Stopped {name}.\n")
        );
    }
}

#[test]
fn public_metadata_commit_failure_stops_the_started_session() {
    let root = scratch_dir();
    let bundles = make_bundles(root.path());
    let output = ato(root.path())
        .env("ATO_TEST_FAIL_PUBLIC_METADATA_COMMIT", "1")
        .args(["decap", "start"])
        .arg(&bundles.valid)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Session stopped"),
        "unexpected metadata failure: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let list = ato(root.path())
        .args(["decap", "list", "--json"])
        .output()
        .unwrap();
    assert!(list.status.success());
    let rows: Vec<serde_json::Value> = serde_json::from_slice(&list.stdout).unwrap();
    assert!(
        rows.is_empty(),
        "failed start must not leave a public Session"
    );

    let sessions_root = root.path().join("ato-home/capsule-protocol-sessions");
    let records: Vec<serde_json::Value> = fs::read_dir(sessions_root)
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| fs::read(entry.path().join("session.json")).ok())
        .map(|bytes| serde_json::from_slice(&bytes).unwrap())
        .collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["lifecycle"], "stopped");
}

#[test]
fn ready_state_bundle_restores_runs_and_stops_under_supervisor() {
    let root = scratch_dir();
    let bundles = make_bundles(root.path());
    let start = ato(root.path())
        .args(["internal", "capsule-session", "start"])
        .arg(&bundles.valid)
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
    let resume = ato(root.path())
        .args(["internal", "capsule-session", "resume", &session_id])
        .output()
        .unwrap();
    assert!(!resume.status.success(), "resume must fail closed");

    for invalid in [&bundles.connector, &bundles.record, &bundles.unknown_state] {
        let output = ato(root.path())
            .args(["internal", "capsule-session", "start"])
            .arg(invalid)
            .arg("--into")
            .arg(root.path().join("unused-invalid"))
            .arg("--no-attach")
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "invalid Ready-State bundle unexpectedly started: {}",
            invalid.display()
        );
    }
}
