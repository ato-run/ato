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
                .or(predicate::str::contains("E302"))
                .and(predicate::str::contains("IPC-008")),
        );
}

#[test]
fn validate_succeeds_for_versionless_chml_manifest() {
    let temp = TempDir::new().unwrap();

    write_file(
        &temp.path().join("capsule.toml"),
        r#"
name = "validate-chml"
type = "app"
runtime = "source/deno"
run = "deno run --allow-net main.ts"
runtime_version = "1.46.3"

[ipc.exports]
name = "validate-chml"

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
            predicate::str::contains("\"target_label\": \"app\"")
                .and(predicate::str::contains("\"warnings\": []")),
        );
}

#[test]
fn validate_ignores_generated_nested_workspace_manifests() {
    let temp = TempDir::new().unwrap();
    let control_plane_dir = temp.path().join("apps/control-plane");
    let dashboard_dir = temp.path().join("apps/dashboard");
    let generated_duplicate_dir = dashboard_dir.join(".next/standalone/apps/control-plane");

    fs::create_dir_all(&control_plane_dir).unwrap();
    fs::create_dir_all(&dashboard_dir).unwrap();
    fs::create_dir_all(&generated_duplicate_dir).unwrap();

    write_file(
        &temp.path().join("capsule.toml"),
        r#"
name = "file2api"

[workspace]
members = ["apps/*"]

[workspace.defaults]
required_env = ["OPENAI_API_KEY", "CLOUDFLARE_ACCOUNT_ID", "CLOUDFLARE_API_TOKEN"]

[packages.control-plane]
type = "app"
runtime = "source/python"
port = 8000
run = "uvicorn control_plane.modal_webhook:app --host 0.0.0.0 --port $PORT"

[packages.dashboard]
type = "app"
runtime = "source/node"
build = "npm run build"
run = "npm start"
port = 3000

[packages.data-plane-worker]
type = "app"
runtime = "source/node"
run = "wrangler dev --config wrangler.dev.jsonc"
"#,
    );

    write_file(
        &control_plane_dir.join("capsule.toml"),
        "name = \"control-plane\"\ntype = \"app\"\nruntime = \"source/python\"\nrun = \"python main.py\"\n",
    );
    write_file(
        &dashboard_dir.join("capsule.toml"),
        "name = \"dashboard\"\ntype = \"app\"\nruntime = \"source/node\"\nrun = \"npm start\"\n",
    );
    write_file(
        &generated_duplicate_dir.join("capsule.toml"),
        "name = \"control-plane\"\ntype = \"app\"\nruntime = \"source/python\"\nrun = \"python generated.py\"\n",
    );

    capsule()
        .args(["validate", "--json"])
        .arg(temp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"warnings\": []"));
}
