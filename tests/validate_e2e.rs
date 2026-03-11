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

#[test]
fn validate_help() {
    capsule()
        .args(["validate", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Validate capsule build/run inputs without executing",
        ));
}

#[test]
fn validate_succeeds_for_valid_ipc_schema() {
    let temp = TempDir::new().unwrap();

    write_file(
        &temp.path().join("capsule.toml"),
        r#"
schema_version = "1"
name = "validate-ok"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "deno"
runtime_version = "1.46.3"
entrypoint = "main.ts"

[ipc.exports]
name = "validate-ok"

[[ipc.exports.methods]]
name = "ping"
input_schema = "schemas/ping-input.json"
"#,
    );
    write_file(&temp.path().join("main.ts"), r#"console.log("ok");"#);
    write_file(
        &temp.path().join("schemas/ping-input.json"),
        r#"{
  "type": "object",
  "properties": {
    "name": { "type": "string" }
  },
  "required": ["name"]
}"#,
    );

    capsule()
        .args(["validate", "--json"])
        .arg(temp.path())
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"target_label\": \"cli\"")
                .and(predicate::str::contains("\"warnings\": []")),
        );
}

#[test]
fn validate_fails_for_invalid_ipc_schema_reference() {
    let temp = TempDir::new().unwrap();

    write_file(
        &temp.path().join("capsule.toml"),
        r#"
schema_version = "1"
name = "validate-bad-schema"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "deno"
runtime_version = "1.46.3"
entrypoint = "main.ts"

[ipc.exports]
name = "validate-bad-schema"

[[ipc.exports.methods]]
name = "ping"
input_schema = "schemas/missing.json"
"#,
    );
    write_file(&temp.path().join("main.ts"), r#"console.log("ok");"#);

    capsule()
        .args(["validate"])
        .arg(temp.path())
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("ATO_ERR_POLICY_VIOLATION")
                .and(predicate::str::contains("IPC-008")),
        );
}
