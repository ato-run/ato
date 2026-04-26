use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

fn capsule() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin!("ato"))
}

fn write_file(path: &std::path::Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn write_ipc_service_fixture(root: &std::path::Path) {
    write_file(
        &root.join("capsule.toml"),
        r#"
schema_version = "0.3"
name = "ipc-invoke-test"
version = "0.1.0"
type = "app"

runtime = "source/deno"
runtime_version = "1.46.3"
run = "main.ts"
[ipc.exports]
name = "invoke-svc"

[[ipc.exports.methods]]
name = "ping"
input_schema = "schemas/ping-input.json"
"#,
    );
    write_file(&root.join("main.ts"), r#"console.log("service");"#);
    write_file(
        &root.join("schemas/ping-input.json"),
        r#"{
  "type": "object",
  "properties": {
    "name": { "type": "string" }
  },
  "required": ["name"]
}"#,
    );
}

#[test]
fn ipc_invoke_help() {
    capsule()
        .args(["ipc", "invoke", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Validate and send a JSON-RPC invoke request",
        ));
}

#[test]
fn ipc_invoke_rejects_schema_violation_before_transport() {
    let temp = TempDir::new().unwrap();
    write_ipc_service_fixture(temp.path());

    capsule()
        .args([
            "ipc",
            "invoke",
            "--method",
            "ping",
            "--args",
            r#"{"name":123}"#,
            "--json",
        ])
        .arg(temp.path())
        .assert()
        .failure()
        .stdout(
            predicate::str::contains("\"code\": -32003")
                .and(predicate::str::contains("Schema validation failed"))
                .and(predicate::str::contains("invoke-svc").not()),
        );
}

#[test]
fn ipc_invoke_rejects_oversized_message_before_transport() {
    let temp = TempDir::new().unwrap();
    write_ipc_service_fixture(temp.path());
    let payload = format!(r#"{{"name":"{}"}}"#, "a".repeat(128));

    capsule()
        .args([
            "ipc",
            "invoke",
            "--method",
            "ping",
            "--args",
            &payload,
            "--max-message-size",
            "64",
            "--json",
        ])
        .arg(temp.path())
        .assert()
        .failure()
        .stdout(predicate::str::contains("\"code\": -32004"));
}

#[test]
fn ipc_invoke_returns_service_unavailable_after_preflight() {
    let temp = TempDir::new().unwrap();
    write_ipc_service_fixture(temp.path());

    capsule()
        .args([
            "ipc",
            "invoke",
            "--method",
            "ping",
            "--args",
            r#"{"name":"world"}"#,
            "--json",
        ])
        .arg(temp.path())
        .assert()
        .failure()
        .stdout(
            predicate::str::contains("\"code\": -32002")
                .and(predicate::str::contains("Socket not found")),
        );
}
