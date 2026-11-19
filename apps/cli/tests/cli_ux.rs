use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn ato() -> Command {
    Command::new(env!("CARGO_BIN_EXE_ato"))
}

fn assert_success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
}

#[test]
fn help_groups_expose_public_tasks_and_the_init_example_works() {
    let help = assert_success(ato().arg("--help").output().unwrap());
    let use_apps = help.find("Use apps:").unwrap();
    let authoring = help.find("Author a recorded project:").unwrap();
    let tools = help.find("Formation and runtime tools:").unwrap();
    assert!(help[use_apps..authoring].contains("  run "));
    assert!(help[tools..].contains("  form-session "));
    assert!(!help.contains("__worker"));

    let root = tempfile::tempdir().unwrap();
    let help = assert_success(ato().args(["init", "--help"]).output().unwrap());
    let (_, example) = help.split_once("Example project/capsule.toml").unwrap();
    let (manifest, _) = example.split_once("Initialize:").unwrap();
    let manifest = manifest
        .lines()
        .skip(1)
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(root.path().join("capsule.toml"), manifest).unwrap();
    assert_success(
        ato()
            .args(["init", "--initial-only"])
            .arg(root.path())
            .output()
            .unwrap(),
    );
    assert!(root.path().join(".capsule/refs/heads/main").is_file());
}

#[test]
fn declared_connections_can_be_found_for_a_bundle_and_an_installed_app() {
    let root = tempfile::tempdir().unwrap();
    let capsule = root.path().join("mail.capsule");
    let (bytes, _) =
        ato_portable_application::build_authored_bundle_v2(&sample("ato-mail")).unwrap();
    fs::write(&capsule, bytes).unwrap();
    let declarations = assert_success(
        ato()
            .env("ATO_HOME", root.path())
            .args(["run", "--list-bindings", "--json"])
            .arg(&capsule)
            .output()
            .unwrap(),
    );
    let bindings: Vec<Value> = serde_json::from_str(&declarations).unwrap();
    assert!(bindings.iter().any(|binding| {
        binding == &serde_json::json!({"id":"relay_auth","protocol":"ato.secret@1","required":true})
    }));
    assert!(!root.path().join("cache").exists());

    let metadata = assert_success(
        ato()
            .env("ATO_HOME", root.path())
            .args(["app", "import"])
            .arg(capsule)
            .output()
            .unwrap(),
    );
    let instance: Value = serde_json::from_str(&metadata).unwrap();
    let id = instance["instance_id"].as_str().unwrap();
    let installed = assert_success(
        ato()
            .env("ATO_HOME", root.path())
            .args(["app", "start", id, "--list-bindings"])
            .output()
            .unwrap(),
    );
    assert_eq!(installed, declarations);
    assert!(
        !root
            .path()
            .join("instances")
            .join(id)
            .join("active-run.json")
            .exists()
    );
}

#[test]
fn hub_connection_failure_has_a_short_default_and_verbose_details() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);

    for verbose in [false, true] {
        let mut command = ato();
        command.env("ATO_HOME", root.path()).args([
            "run",
            "capsules/example/missing",
            "--api-url",
            &api,
        ]);
        if verbose {
            command.arg("--verbose");
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(&api), "{error}");
        if verbose {
            assert!(error.contains("/v1/hub/capsules"), "{error}");
        } else {
            assert!(
                error.contains("Could not fetch") && error.contains("--verbose"),
                "{error}"
            );
            assert!(!error.contains("/v1/hub/capsules"), "{error}");
        }
    }
}

#[test]
fn form_inspection_preserves_the_scriptable_json_document() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("package.json"),
        r#"{"scripts":{"build":"vite build"}}"#,
    )
    .unwrap();
    for json in [false, true] {
        let mut command = ato();
        command.arg("form-inspect").arg(root.path());
        if json {
            command.arg("--json");
        }
        let stdout = assert_success(command.output().unwrap());
        let value: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(value["repository"]["runtime_verified"], false);
        assert_eq!(value["declared_execution"], Value::Null);
    }
}
