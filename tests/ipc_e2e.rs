#![allow(deprecated)]

//! IPC E2E tests
//!
//! Integration tests for `ato ipc` subcommands.
//! Tests are CLI-only via `assert_cmd` since ato-cli is a binary crate.
//!
//! Test categories:
//! - 13d.1: `ato ipc status / start / stop` CLI round-trip
//! - 13d.4: Error cases (missing toml, not-found service)
//!
//! IPC validation rules (IPC-001 through IPC-007) and JSON-RPC/schema
//! tests are in unit tests inside `src/ipc/validate.rs`, `src/ipc/jsonrpc.rs`,
//! and `src/ipc/schema.rs` (97 tests total, run via `cargo test --bin ato`).

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

fn capsule() -> Command {
    Command::cargo_bin("ato").expect("capsule binary not found")
}

fn fixture_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn write_file(path: &std::path::Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

// ═══════════════════════════════════════════════════════════════════════════
// 13d.1: `ato ipc` Help / Discovery
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn ipc_status_help() {
    capsule()
        .args(["ipc", "status", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Show status of running IPC services",
        ));
}

#[test]
fn ipc_start_help() {
    capsule()
        .args(["ipc", "start", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Start an IPC service"));
}

#[test]
fn ipc_stop_help() {
    capsule()
        .args(["ipc", "stop", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Stop a running IPC service"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 13d.1: `ato ipc status` (empty)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn ipc_status_shows_no_services() {
    capsule()
        .args(["ipc", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No IPC services running"));
}

#[test]
fn ipc_status_json_returns_empty_array() {
    capsule()
        .args(["ipc", "status", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[]"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 13d.1: `ato ipc start` — Registration
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn ipc_start_registers_service() {
    capsule()
        .args(["ipc", "start"])
        .arg(fixture_dir("ipc_service"))
        .assert()
        .success()
        .stdout(
            predicate::str::contains("registered").or(predicate::str::contains("already running")),
        );
}

#[test]
fn ipc_start_json_output_is_valid() {
    let output = capsule()
        .args(["ipc", "start", "--json"])
        .arg(fixture_dir("ipc_service"))
        .output()
        .expect("run ato ipc start");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("JSON output");
    assert!(
        json.get("status").is_some() || json.get("error").is_some(),
        "Expected 'status' or 'error' key, got: {}",
        stdout,
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 13d.1: `ato ipc stop` — Deregistration
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn ipc_stop_reports_not_found() {
    capsule()
        .args(["ipc", "stop", "--name", "nonexistent-svc-e2e-test"])
        .assert()
        .success()
        .stderr(predicate::str::contains("not running").or(predicate::str::contains("not_found")));
}

#[test]
fn ipc_stop_json_reports_not_found() {
    let output = capsule()
        .args([
            "ipc",
            "stop",
            "--name",
            "nonexistent-svc-e2e-test",
            "--json",
        ])
        .output()
        .expect("run ato ipc stop");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).expect("JSON output");
    assert_eq!(json["error"], "not_found");
}

// ═══════════════════════════════════════════════════════════════════════════
// 13d.1: Start → Stop Round-trip
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn ipc_start_then_stop_roundtrip() {
    // Start
    capsule()
        .args(["ipc", "start", "--json"])
        .arg(fixture_dir("ipc_service"))
        .assert()
        .success();

    // Stop
    capsule()
        .args(["ipc", "stop", "--name", "test-svc", "--json"])
        .assert()
        .success();
}

// ═══════════════════════════════════════════════════════════════════════════
// 13d.4: Error Cases
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn ipc_start_fails_without_capsule_toml() {
    let temp = TempDir::new().unwrap();

    capsule()
        .args(["ipc", "start"])
        .arg(temp.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains("capsule.toml not found"));
}

#[test]
fn ipc_start_with_no_ipc_section_uses_fallback_name() {
    let temp = TempDir::new().unwrap();
    std::fs::write(
        temp.path().join("capsule.toml"),
        r#"
schema_version = "1"
name = "no-ipc"
version = "0.1.0"
type = "app"

[execution]
runtime = "source"
entrypoint = "echo hello"
"#,
    )
    .unwrap();

    capsule()
        .args(["ipc", "start"])
        .arg(temp.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("registered").or(predicate::str::contains("no-ipc")));
}

#[test]
fn run_fails_closed_when_required_ipc_import_is_missing() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    fs::create_dir_all(&home).unwrap();

    write_file(
        &temp.path().join("capsule.toml"),
        r#"
schema_version = "1"
name = "ipc-fail-closed"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "deno"
runtime_version = "1.46.3"
entrypoint = "main.ts"

[ipc.imports.greeter]
from = "missing-service"
"#,
    );
    write_file(
        &temp.path().join("main.ts"),
        r#"console.log("should not run");"#,
    );

    capsule()
        .current_dir(temp.path())
        .env("HOME", &home)
        .args(["run", ".", "--yes"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("ATO_ERR_POLICY_VIOLATION")
                .and(predicate::str::contains("IPC-006"))
                .and(predicate::str::contains("missing-service")),
        );
}

#[test]
fn build_fails_when_ipc_schema_reference_is_invalid() {
    let temp = TempDir::new().unwrap();

    write_file(
        &temp.path().join("capsule.toml"),
        r#"
schema_version = "1"
name = "ipc-schema-invalid"
version = "0.1.0"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "deno"
runtime_version = "1.46.3"
entrypoint = "main.ts"

[ipc.exports]
name = "schema-service"

[[ipc.exports.methods]]
name = "ping"
input_schema = "schemas/missing.json"
"#,
    );
    write_file(
        &temp.path().join("main.ts"),
        r#"console.log("build should fail first");"#,
    );

    capsule()
        .args(["build"])
        .arg(temp.path())
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("ATO_ERR_POLICY_VIOLATION")
                .and(predicate::str::contains("IPC-008")),
        );
}
