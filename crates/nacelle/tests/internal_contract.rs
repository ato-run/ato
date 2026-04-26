use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn nacelle_bin() -> &'static str {
    env!("CARGO_BIN_EXE_nacelle")
}

fn python3_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn sandbox_available() -> bool {
    let output = run_internal_command(
        "features",
        &serde_json::json!({
            "spec_version": "1.0"
        }),
    );

    if !output.status.success() {
        return false;
    }

    let value: Value = serde_json::from_slice(&output.stdout).expect("features response");
    value["capabilities"]["sandbox"]
        .as_array()
        .map(|items| !items.is_empty())
        .unwrap_or(false)
}

fn write_json_request(temp_dir: &Path, file_name: &str, request: &Value) -> PathBuf {
    let request_path = temp_dir.join(file_name);
    fs::write(&request_path, serde_json::to_vec(request).unwrap()).unwrap();
    request_path
}

fn run_internal_command(command: &str, request: &Value) -> std::process::Output {
    let temp_dir = tempfile::tempdir().unwrap();
    let request_path = write_json_request(temp_dir.path(), "request.json", request);
    Command::new(nacelle_bin())
        .args(["internal", "--input"])
        .arg(&request_path)
        .arg(command)
        .output()
        .unwrap()
}

fn write_request(temp_dir: &Path, manifest_path: &Path, ipc_socket_paths: &[PathBuf]) -> PathBuf {
    let request_path = temp_dir.join("exec-request.json");
    let request = serde_json::json!({
        "spec_version": "1.0",
        "workload": {
            "type": "source",
            "manifest": manifest_path,
        },
        "ipc_socket_paths": ipc_socket_paths,
    });
    fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
    request_path
}

fn stdout_lines(output: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(output)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[test]
fn internal_features_reports_machine_contract() {
    let output = run_internal_command(
        "features",
        &serde_json::json!({
            "spec_version": "1.0"
        }),
    );

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(response["spec_version"], "1.0");
    assert_eq!(response["engine"]["name"], "nacelle");
    assert!(response["engine"]["engine_version"].is_string());
    assert!(response["engine"]["platform"].is_string());

    let workloads = response["capabilities"]["workloads"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(workloads.contains(&"source"));
    assert!(workloads.contains(&"bundle"));

    let languages = response["capabilities"]["languages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    for language in ["python", "node", "deno", "bun"] {
        assert!(
            languages.contains(&language),
            "missing language {language} in {:?}",
            languages
        );
    }

    let sandbox = response["capabilities"]["sandbox"].as_array().unwrap();
    if sandbox.is_empty() {
        assert_eq!(response["capabilities"]["ipc_sandbox"], false);
    }
}

#[test]
fn internal_features_accepts_legacy_spec_version() {
    let output = run_internal_command(
        "features",
        &serde_json::json!({
            "spec_version": "0.1.0"
        }),
    );

    assert!(output.status.success());
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(response["spec_version"], "0.1.0");
}

#[test]
fn internal_features_accepts_next_spec_version() {
    let output = run_internal_command(
        "features",
        &serde_json::json!({
            "spec_version": "2.0"
        }),
    );

    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["spec_version"], "2.0");
    assert_eq!(response["ok"], true);
}

#[test]
fn internal_features_rejects_unknown_spec_version() {
    let output = run_internal_command(
        "features",
        &serde_json::json!({
            "spec_version": "3.0"
        }),
    );

    assert!(!output.status.success());
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["spec_version"], "3.0");
    assert_eq!(response["error"]["code"], "UNSUPPORTED");
}

#[test]
fn internal_exec_v2_rejects_missing_manifest() {
    let temp_dir = tempfile::tempdir().unwrap();
    let request_path = temp_dir.path().join("exec-v2-request.json");
    let request = serde_json::json!({
        "spec_version": "2.0",
        "workload": {
            "type": "source",
            "environment_spec": {
                "lower_source": {
                    "manifest": temp_dir.path().join("capsule.toml")
                },
                "upper_overlays": [],
                "derived_outputs": [],
                "runtime_artifacts": []
            }
        }
    });
    fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();

    let output = Command::new(nacelle_bin())
        .args(["internal", "--input"])
        .arg(&request_path)
        .arg("exec")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["spec_version"], "2.0");
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("manifest not found"));
}

#[test]
fn internal_pack_returns_machine_readable_unsupported_error() {
    let output = run_internal_command(
        "pack",
        &serde_json::json!({
            "spec_version": "1.0"
        }),
    );

    assert!(!output.status.success());

    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["spec_version"], "1.0");
    assert_eq!(response["error"]["code"], "UNSUPPORTED");
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("internal pack is not supported"));
}

#[test]
fn internal_exec_rejects_malformed_json() {
    let temp_dir = tempfile::tempdir().unwrap();
    let request_path = temp_dir.path().join("broken-request.json");
    fs::write(&request_path, b"{").unwrap();

    let output = Command::new(nacelle_bin())
        .args(["internal", "--input"])
        .arg(&request_path)
        .arg("exec")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "INVALID_INPUT");
}

#[test]
fn internal_exec_requires_manifest_path() {
    let output = run_internal_command(
        "exec",
        &serde_json::json!({
            "spec_version": "1.0",
            "workload": {
                "type": "source"
            }
        }),
    );

    assert!(!output.status.success());
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "INVALID_INPUT");
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("manifest path is required"));
}

#[test]
fn internal_exec_streams_initial_response_then_events() {
    if !python3_available() || !sandbox_available() {
        eprintln!("Skipping internal exec contract test: python3 or sandbox backend unavailable");
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    let socket_path = PathBuf::from(format!("/tmp/nacelle-ipc-{}-ok.sock", std::process::id()));
    let server_path = temp_dir.path().join("server.py");
    let manifest_path = temp_dir.path().join("capsule.toml");
    let request_path = write_request(
        temp_dir.path(),
        &manifest_path,
        std::slice::from_ref(&socket_path),
    );

    fs::write(
        &server_path,
        format!(
            r#"import os
import time

SOCKET_PATH = r"{socket_path}"

try:
    os.unlink(SOCKET_PATH)
except FileNotFoundError:
    pass

with open(SOCKET_PATH, "w", encoding="utf-8") as handle:
    handle.write("ready\n")
    handle.flush()
time.sleep(1.0)

try:
    os.unlink(SOCKET_PATH)
except FileNotFoundError:
    pass
"#,
            socket_path = socket_path.display(),
        ),
    )
    .unwrap();

    fs::write(
        &manifest_path,
        format!(
            r#"name = "stream-contract"
version = "0.1.0"

[execution]
entrypoint = "python3 server.py"

[isolation]
sandbox = false

[readiness_probe]
port = "0"
timeout_ms = 5000
interval_ms = 100
"#,
        ),
    )
    .unwrap();

    let output = Command::new(nacelle_bin())
        .args(["internal", "--input"])
        .arg(&request_path)
        .arg("exec")
        .current_dir(temp_dir.path())
        .output()
        .expect("run nacelle internal exec");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let lines = stdout_lines(&output.stdout);
    assert!(
        lines.len() >= 3,
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let response: Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(response["ok"], true);
    assert!(response["pid"].as_u64().is_some());

    let ready_event: Value = serde_json::from_str(&lines[1]).unwrap();
    assert_eq!(ready_event["event"], "ipc_ready");
    assert_eq!(ready_event["service"], "stream-contract");
    assert_eq!(
        ready_event["endpoint"],
        format!("unix://{}", socket_path.display())
    );

    let exit_event: Value = serde_json::from_str(&lines[2]).unwrap();
    assert_eq!(exit_event["event"], "service_exited");
    assert_eq!(exit_event["service"], "stream-contract");
    assert_eq!(exit_event["exit_code"], 0);
}

#[test]
fn internal_exec_emits_service_exit_when_workload_fails_before_ready() {
    if !python3_available() || !sandbox_available() {
        eprintln!("Skipping internal exec contract test: python3 or sandbox backend unavailable");
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    let socket_path = PathBuf::from(format!("/tmp/nacelle-ipc-{}-fail.sock", std::process::id()));
    let script_path = temp_dir.path().join("server.py");
    let manifest_path = temp_dir.path().join("capsule.toml");
    let request_path = write_request(
        temp_dir.path(),
        &manifest_path,
        std::slice::from_ref(&socket_path),
    );

    fs::write(&script_path, "import sys\nsys.exit(42)\n").unwrap();

    fs::write(
        &manifest_path,
        format!(
            r#"name = "fail-fast-contract"
version = "0.1.0"

[execution]
entrypoint = "python3 server.py"

[isolation]
sandbox = false

[readiness_probe]
port = "0"
timeout_ms = 5000
interval_ms = 100
"#,
        ),
    )
    .unwrap();

    let output = Command::new(nacelle_bin())
        .args(["internal", "--input"])
        .arg(&request_path)
        .arg("exec")
        .current_dir(temp_dir.path())
        .output()
        .expect("run nacelle internal exec");

    assert_eq!(output.status.code(), Some(42));

    let lines = stdout_lines(&output.stdout);
    assert!(
        lines.len() >= 2,
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let response: Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(response["ok"], true);
    assert!(response["pid"].as_u64().is_some());

    assert!(
        lines
            .iter()
            .skip(1)
            .all(|line| !line.contains("\"ipc_ready\"")),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let exit_event: Value = serde_json::from_str(&lines[1]).unwrap();
    assert_eq!(exit_event["event"], "service_exited");
    assert_eq!(exit_event["service"], "fail-fast-contract");
    assert_eq!(exit_event["exit_code"], 42);
}

#[cfg(target_os = "macos")]
#[test]
fn internal_exec_v2_runs_with_overlay_and_derived_output() {
    if !python3_available() {
        eprintln!("Skipping v2 exec contract test: python3 unavailable");
        return;
    }

    let temp_dir = tempfile::tempdir().unwrap();
    let lower_dir = temp_dir.path().join("lower");
    let overlay_dir = temp_dir.path().join("overlay");
    let derived_dir = temp_dir.path().join("derived");
    fs::create_dir_all(&lower_dir).unwrap();
    fs::create_dir_all(&overlay_dir).unwrap();

    let manifest_path = lower_dir.join("capsule.toml");
    let script_path = lower_dir.join("main.py");
    let overlay_path = overlay_dir.join("overlay.txt");
    fs::write(&overlay_path, "from-overlay\n").unwrap();

    fs::write(
        &script_path,
        r#"from pathlib import Path
import os

overlay = Path("overlay.txt").read_text(encoding="utf-8").strip()
artifact = os.environ["OVERLAY_ARTIFACT"]
target = Path(".derived") / "result.txt"
target.write_text(f"value={overlay};artifact={artifact}\n", encoding="utf-8")
"#,
    )
    .unwrap();

    fs::write(
        &manifest_path,
        r#"name = "v2-contract"
version = "0.1.0"

[execution]
entrypoint = "python3 main.py"

[isolation]
sandbox = false
"#,
    )
    .unwrap();

    let request_path = temp_dir.path().join("request-v2.json");
    let request = serde_json::json!({
        "spec_version": "2.0",
        "workload": {
            "type": "source",
            "environment_spec": {
                "lower_source": {
                    "manifest": manifest_path
                },
                "upper_overlays": [
                    {
                        "source": overlay_path,
                        "target": "overlay.txt",
                        "readonly": true
                    }
                ],
                "derived_outputs": [
                    {
                        "host_path": derived_dir,
                        "target": ".derived",
                        "kind": "artifact"
                    }
                ],
                "runtime_artifacts": [
                    {
                        "name": "overlay_file",
                        "path": overlay_path,
                        "env_var": "OVERLAY_ARTIFACT"
                    }
                ]
            }
        }
    });
    fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();

    let output = Command::new(nacelle_bin())
        .args(["internal", "--input"])
        .arg(&request_path)
        .arg("exec")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let lines = stdout_lines(&output.stdout);
    assert!(
        lines.len() >= 3,
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let response: Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(response["ok"], true);

    let completed = lines
        .iter()
        .skip(1)
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| event["event"] == "execution_completed")
        .expect("missing execution_completed event");

    assert_eq!(completed["service"], "v2-contract");
    assert!(completed["run_id"].as_str().unwrap().starts_with("exec-"));
    assert_eq!(
        completed["derived_output_path"],
        derived_dir.display().to_string()
    );
    assert_eq!(
        completed["cleanup_policy_applied"],
        "delete_workspace_preserve_outputs"
    );

    let derived_file = temp_dir.path().join("derived").join("result.txt");
    assert_eq!(
        fs::read_to_string(derived_file).unwrap(),
        format!("value=from-overlay;artifact={}\n", overlay_path.display())
    );
}
