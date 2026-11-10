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

#[test]
fn checkpoint_subcommand_validates_a_checkpoint_file() {
    let directory = tempfile::tempdir().unwrap();
    let checkpoint = format!(
        "schema = \"ato.workspace-checkpoint/1\"\nworkspace_digest = \"sha256:{zeros}\"\n\n[view]\ndirection = \"horizontal\"\nsplit = [0.5, 0.5]\nactive_pane = \"board\"\n\n\
         [[pane]]\npane_id = \"board\"\ninstance_id = \"instance-1\"\nsnapshot_ref = \"sha256:{zeros}\"\n\n\
         [[pane]]\npane_id = \"todo\"\ninstance_id = \"instance-2\"\nsnapshot_ref = \"sha256:{zeros}\"\n",
        zeros = "a".repeat(64)
    );
    let path = directory.path().join("checkpoint.toml");
    std::fs::write(&path, checkpoint).unwrap();
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
    let checkpoint = format!(
        "schema = \"ato.workspace-checkpoint/1\"\nworkspace_digest = \"sha256:{zeros}\"\n\n[view]\ndirection = \"horizontal\"\nsplit = [0.5, 0.5]\nactive_pane = \"todo\"\n\n\
         [[pane]]\npane_id = \"todo\"\ninstance_id = \"instance-1\"\nsnapshot_ref = \"sha256:{zeros}\"\n\n\
         [[pane]]\npane_id = \"board\"\ninstance_id = \"instance-2\"\nsnapshot_ref = \"sha256:{zeros}\"\n",
        zeros = "b".repeat(64)
    );
    let path = directory.path().join("checkpoint.toml");
    std::fs::write(&path, checkpoint).unwrap();
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
    let running = ato_cli::workspace::start(&file, None).expect("the workspace starts");
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
    let running = ato_cli::workspace::start(&file, None).expect("the workspace starts");

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

    let checkpoint = running.capture().expect("the workspace captures");
    checkpoint.validate().unwrap();
    let board = checkpoint
        .pane
        .iter()
        .find(|pane| pane.pane_id == "board")
        .unwrap();
    assert_eq!(
        board.state.as_ref().unwrap()["todos"],
        serde_json::Value::String("[1,2]".to_owned())
    );
    let path = directory.path().join("checkpoint.toml");
    ato_cli::workspace::write_checkpoint_atomic(&path, &checkpoint).unwrap();
    drop(running);

    let resumed =
        ato_cli::workspace::start(&file, Some(&checkpoint)).expect("the workspace resumes");
    let board = resumed
        .panes
        .iter()
        .find(|pane| pane.pane_id == "board")
        .unwrap();
    let storage = board.server.local_storage().unwrap().unwrap();
    assert_eq!(storage.get("todos").map(String::as_str), Some("[1,2]"));
}
