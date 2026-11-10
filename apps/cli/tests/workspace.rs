//! `ato workspace inspect` / `checkpoint` validation, on a real v3 capsule.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use sha2::{Digest, Sha256};

fn repo_sample() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static.capsule");
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn write_workspace(dir: &Path, manifest: &str) -> PathBuf {
    let path = dir.join("workspace.toml");
    std::fs::write(&path, manifest).unwrap();
    path
}

fn manifest(split: &str, panes: &str) -> String {
    format!(
        "schema = \"ato.workspace/1\"\n\n[layout]\ndirection = \"horizontal\"\nsplit = [{split}]\n{panes}"
    )
}

#[test]
fn inspect_reports_one_contract_for_two_panes_of_the_same_capsule() {
    let capsule = repo_sample();
    let digest = sha256(&capsule);
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("app.capsule"), &capsule).unwrap();
    let panes = format!(
        "\n[[pane]]\nid = \"board\"\napp = \"whiteboard\"\ncapsule = \"app.capsule\"\ndigest = \"{digest}\"\n\
         \n[[pane]]\nid = \"calendar\"\napp = \"calendar\"\ncapsule = \"app.capsule\"\ndigest = \"{digest}\"\n"
    );
    let file = write_workspace(directory.path(), &manifest("0.65, 0.35", &panes));

    let output = Command::cargo_bin("ato")
        .unwrap()
        .args(["workspace", "inspect", "--json", file.to_str().unwrap()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let panes = value["panes"].as_array().unwrap();
    assert_eq!(panes.len(), 2);
    assert_eq!(panes[0]["pane_id"], "board");
    assert_eq!(panes[1]["pane_id"], "calendar");
    assert_eq!(panes[0]["realization"], "static_web");
    assert_eq!(panes[0]["contract_ref"], panes[1]["contract_ref"]);
    assert!(
        value["workspace_digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(panes[0]["capsule_sha256"].as_str().unwrap() == digest);
}

#[test]
fn inspect_refuses_a_declared_digest_that_does_not_match() {
    let capsule = repo_sample();
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("app.capsule"), &capsule).unwrap();
    let wrong = format!("sha256:{}", "0".repeat(64));
    let panes =
        format!("\n[[pane]]\nid = \"board\"\ncapsule = \"app.capsule\"\ndigest = \"{wrong}\"\n");
    let file = write_workspace(directory.path(), &manifest("1.0", &panes));
    Command::cargo_bin("ato")
        .unwrap()
        .args(["workspace", "inspect", file.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("digest mismatch"));
}

#[test]
fn inspect_refuses_computation_root_v2_panes() {
    let directory = tempfile::tempdir().unwrap();
    let v2 = format!(
        "{{\"index\":{{\"objects\":[],\"root\":\"blake3:{}\",\"version\":2}},\"payloads\":[]}}",
        "0".repeat(64)
    );
    std::fs::write(directory.path().join("v2.capsule"), v2).unwrap();
    let panes = "\n[[pane]]\nid = \"legacy\"\ncapsule = \"v2.capsule\"\n";
    let file = write_workspace(directory.path(), &manifest("1.0", panes));
    Command::cargo_bin("ato")
        .unwrap()
        .args(["workspace", "inspect", file.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("computation-root wire v2"));
}

#[test]
fn inspect_refuses_an_unknown_workspace_schema_before_reading_capsules() {
    let directory = tempfile::tempdir().unwrap();
    let text = "schema = \"ato.workspace/999\"\n\n[layout]\ndirection = \"horizontal\"\nsplit = [1.0]\n\n[[pane]]\nid = \"a\"\ncapsule = \"missing.capsule\"\n";
    let file = write_workspace(directory.path(), text);
    Command::cargo_bin("ato")
        .unwrap()
        .args(["workspace", "inspect", file.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("unsupported workspace schema"));
}

fn browser_checkpoint(
    active: &str,
    pane_ids: &[&str],
) -> ato_cli::workspace::WorkspaceCheckpointV1 {
    use ato_cli::workspace::{
        Direction, WorkspaceCheckpointPaneV1, WorkspaceCheckpointV1, WorkspaceViewStateV1,
    };
    let (snapshot, snapshot_ref) =
        ato_cli::workspace::browser_snapshot_for(&std::collections::BTreeMap::new()).unwrap();
    let pane = pane_ids
        .iter()
        .enumerate()
        .map(|(index, pane_id)| WorkspaceCheckpointPaneV1 {
            pane_id: (*pane_id).to_owned(),
            instance_id: format!("instance-{index}"),
            snapshot_ref: snapshot_ref.clone(),
            receipt_ref: None,
            snapshot: snapshot.clone(),
        })
        .collect();
    WorkspaceCheckpointV1 {
        schema: "ato.workspace-checkpoint/1".to_owned(),
        workspace_digest: format!("sha256:{}", "a".repeat(64)),
        view: WorkspaceViewStateV1 {
            direction: Direction::Horizontal,
            split: vec![0.5, 0.5],
            active_pane: active.to_owned(),
            pane: Vec::new(),
        },
        pane,
        data: None,
    }
}

#[test]
fn checkpoint_subcommand_validates_a_checkpoint_file() {
    let directory = tempfile::tempdir().unwrap();
    let checkpoint = browser_checkpoint("board", &["board", "todo"]);
    let path = directory.path().join("checkpoint.toml");
    std::fs::write(&path, toml::to_string(&checkpoint).unwrap()).unwrap();
    Command::cargo_bin("ato")
        .unwrap()
        .args(["workspace", "checkpoint", "--json", path.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"active_pane\": \"board\""));
}

#[test]
fn checkpoint_subcommand_refuses_unsorted_panes() {
    let directory = tempfile::tempdir().unwrap();
    let checkpoint = browser_checkpoint("todo", &["todo", "board"]);
    let path = directory.path().join("checkpoint.toml");
    std::fs::write(&path, toml::to_string(&checkpoint).unwrap()).unwrap();
    Command::cargo_bin("ato")
        .unwrap()
        .args(["workspace", "checkpoint", path.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicates::str::contains("sorted by unique pane_id"));
}

fn two_pane_workspace(directory: &Path) -> (PathBuf, Vec<u8>) {
    let capsule = repo_sample();
    std::fs::write(directory.join("app.capsule"), &capsule).unwrap();
    let digest = sha256(&capsule);
    let panes = format!(
        "\n[[pane]]\nid = \"board\"\napp = \"whiteboard\"\ncapsule = \"app.capsule\"\ndigest = \"{digest}\"\n\
         \n[[pane]]\nid = \"calendar\"\napp = \"calendar\"\ncapsule = \"app.capsule\"\ndigest = \"{digest}\"\n"
    );
    let file = write_workspace(directory, &manifest("0.6, 0.4", &panes));
    (file, capsule)
}

#[test]
fn open_serves_the_shell_and_every_pane() {
    let directory = tempfile::tempdir().unwrap();
    let (file, _) = two_pane_workspace(directory.path());
    let running = ato_cli::workspace::start(&file, None, None, None).expect("the workspace starts");
    assert_eq!(running.panes.len(), 2);

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let shell = client.get(&running.shell_url).send().unwrap();
    assert_eq!(shell.status(), reqwest::StatusCode::OK);
    let shell_body = shell.text().unwrap();
    assert!(
        shell_body.contains("<iframe"),
        "the shell embeds pane iframes"
    );
    for pane in &running.panes {
        assert!(
            shell_body.contains(&format!("{}/", pane.server.base_url())),
            "the shell names pane {}",
            pane.pane_id
        );
        let response = client
            .get(format!("{}/", pane.server.base_url()))
            .send()
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert!(!response.text().unwrap().is_empty());
    }
}

#[test]
fn checkpoint_capture_resume_round_trips_local_state() {
    let directory = tempfile::tempdir().unwrap();
    let (file, _) = two_pane_workspace(directory.path());
    let running = ato_cli::workspace::start(&file, None, None, None).expect("the workspace starts");

    // Drive one pane's state bridge the way its page would.
    let token = running
        .persistence_token("board")
        .expect("the pane has a token");
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let board_url = running
        .panes
        .iter()
        .find(|pane| pane.pane_id == "board")
        .unwrap()
        .server
        .base_url();
    let response = client
        .post(format!("{board_url}/__ato/instance-state/local-storage"))
        .header("x-ato-state-token", token)
        .header("content-type", "application/json")
        .body(r#"{"local_storage":{"todos":"[1,2]"}}"#)
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    let (checkpoint, objects) = running.capture().expect("the workspace captures");
    checkpoint.validate().unwrap();
    assert_eq!(
        objects.len(),
        2,
        "each pane contributes one snapshot object"
    );
    let board = checkpoint
        .pane
        .iter()
        .find(|pane| pane.pane_id == "board")
        .unwrap();
    assert_eq!(board.snapshot["schema"], "ato.portable-instance-snapshot/1");
    assert_eq!(
        board.snapshot["resources"][0]["protocol"],
        "ato.browser-instance-state@1"
    );
    let path = directory.path().join("checkpoint.toml");
    ato_cli::workspace::save_workspace_checkpoint(&path, &running, None).unwrap();
    let loaded = ato_cli::workspace::load_workspace_checkpoint(&path).unwrap();
    assert_eq!(
        loaded.state["board"].get("todos").map(String::as_str),
        Some("[1,2]")
    );
    drop(running);

    let resumed =
        ato_cli::workspace::start(&file, Some(&loaded), None, None).expect("the workspace resumes");
    let board = resumed
        .panes
        .iter()
        .find(|pane| pane.pane_id == "board")
        .unwrap();
    let storage = board.server.local_storage().unwrap().unwrap();
    assert_eq!(storage.get("todos").map(String::as_str), Some("[1,2]"));
}

#[test]
fn capture_reads_the_shell_view_state_from_its_state_bridge() {
    let directory = tempfile::tempdir().unwrap();
    let (file, _) = two_pane_workspace(directory.path());
    let running = ato_cli::workspace::start(&file, None, None, None).expect("the workspace starts");

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let view = serde_json::json!({
        "direction": "horizontal",
        "split": [0.3, 0.7],
        "active_pane": "calendar"
    });
    let body = serde_json::json!({
        "local_storage": { "ato.workspace.view": view.to_string() }
    });
    let response = client
        .post(format!(
            "{}__ato/instance-state/local-storage",
            running.shell_url
        ))
        .header("x-ato-state-token", running.shell_persistence_token())
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    let path = directory.path().join("checkpoint.toml");
    ato_cli::workspace::save_workspace_checkpoint(&path, &running, None).unwrap();
    let read = ato_cli::workspace::read_checkpoint(&path).unwrap();
    assert_eq!(read.view.split, vec![0.3, 0.7]);
    assert_eq!(read.view.active_pane, "calendar");
}

#[test]
fn a_tampered_snapshot_object_is_refused_on_load() {
    let directory = tempfile::tempdir().unwrap();
    let (file, _) = two_pane_workspace(directory.path());
    let running = ato_cli::workspace::start(&file, None, None, None).expect("the workspace starts");
    let path = directory.path().join("checkpoint.toml");
    ato_cli::workspace::save_workspace_checkpoint(&path, &running, None).unwrap();
    assert!(ato_cli::workspace::load_workspace_checkpoint(&path).is_ok());

    let state_directory = directory.path().join("checkpoint.toml.state");
    let object = std::fs::read_dir(&state_directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .next()
        .expect("one snapshot object");
    std::fs::write(&object, b"{\"version\":1,\"local_storage\":[]}").unwrap();
    let error = ato_cli::workspace::load_workspace_checkpoint(&path).unwrap_err();
    assert!(
        error.to_string().contains("content_ref"),
        "unexpected error: {error}"
    );
}

#[test]
fn inspect_reports_the_panes_declared_data_ports() {
    let source = tempfile::tempdir().unwrap();
    std::fs::write(
        source.path().join("index.html"),
        "<!doctype html><p>todo</p>",
    )
    .unwrap();
    std::fs::write(
        source.path().join("capsule.toml"),
        "schema = \"ato.capsule/1\"\n\n[[input]]\nid = \"workspace\"\nuse = \"ato.workspace@1\"\npath = \".\"\n\n[[derive.step]]\nid = \"site\"\nuse = \"ato.browser@1\"\nop = \"serve\"\nsource = \"workspace\"\nentry = \"index.html\"\nspa_fallback = false\n\n[[port]]\nid = \"app.http\"\nuse = \"ato.http@1\"\nfrom = \"site\"\n\n[[contract.require]]\nid = \"root\"\nuse = \"ato.contract.http@1\"\nport = \"app.http\"\nmethod = \"GET\"\npath = \"/\"\n\n[contract.require.expect]\nstatus = 200\n\n[[contract.require]]\nid = \"source-identity\"\nuse = \"ato.contract.workspace@1\"\ninput = \"workspace\"\n\n[contract.require.expect]\ndigest = \"capture\"\n",
    )
    .unwrap();
    std::fs::write(
        source.path().join("ato.data.toml"),
        "schema = \"ato.data-ports/2\"\n\n[[port]]\nname = \"tasks\"\nprotocol = \"todo.board@1\"\nscope = \"user\"\naccess = \"operations\"\noperations = [\"todo_list_due_dates\"]\nrequired = true\n",
    )
    .unwrap();
    std::fs::write(
        source.path().join("ato.operations.yaml"),
        "schema: ato.operations/v2\noperations: []\n",
    )
    .unwrap();
    let (bytes, _) = ato_portable_application::build_static_bundle_with_schema(
        source.path(),
        "todo",
        ato_portable_application::PORTABLE_TREE_V2_SCHEMA,
    )
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("app.capsule"), &bytes).unwrap();
    let panes = format!(
        "\n[[pane]]\nid = \"todo\"\ncapsule = \"app.capsule\"\ndigest = \"{}\"\n",
        sha256(&bytes)
    );
    let file = write_workspace(directory.path(), &manifest("1.0", &panes));
    let output = Command::cargo_bin("ato")
        .unwrap()
        .args(["workspace", "inspect", "--json", file.to_str().unwrap()])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        value["panes"][0]["data_ports"][0],
        "tasks:todo.board@1:operations"
    );
    assert_eq!(value["panes"][0]["operations_declared"], true);
}

#[test]
fn resume_embeds_the_saved_view_state_in_the_shell() {
    let directory = tempfile::tempdir().unwrap();
    let (file, _) = two_pane_workspace(directory.path());
    let mut checkpoint = browser_checkpoint("calendar", &["board", "calendar"]);
    checkpoint.workspace_digest = sha256(std::fs::read_to_string(&file).unwrap().as_bytes());
    checkpoint.view.split = vec![0.3, 0.7];
    let loaded = ato_cli::workspace::LoadedWorkspaceCheckpoint {
        checkpoint,
        state: std::collections::BTreeMap::new(),
    };
    let running =
        ato_cli::workspace::start(&file, Some(&loaded), None, None).expect("resume starts");
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap();
    let body = client
        .get(&running.shell_url)
        .send()
        .unwrap()
        .text()
        .unwrap();
    assert!(
        body.contains("0.3") && body.contains("0.7"),
        "the shell must carry the saved split"
    );
    assert!(
        body.contains("calendar"),
        "the shell must carry the saved active pane"
    );
}
