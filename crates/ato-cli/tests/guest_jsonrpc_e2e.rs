#![allow(deprecated)]

//! Phase 13b.9 — JSON-RPC 2.0 E2E tests for `ato guest`.
//!
//! Each test spawns the `ato` binary with stdin containing a JSON-RPC 2.0
//! request and asserts on the JSON-RPC response shape, error codes, and
//! envelope auto-detection rules.

use assert_cmd::Command;
use base64::{engine::general_purpose, Engine as _};
use serde_json::{json, Value};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use zip::{write::FileOptions, ZipWriter};

fn encode_b64(payload: &[u8]) -> String {
    general_purpose::STANDARD.encode(payload)
}

fn create_test_sync_file(temp_dir: &Path, payload: &[u8], write_allowed: bool) -> PathBuf {
    let manifest_toml = format!(
        r#"
[sync]
version = "1.2"
content_type = "application/octet-stream"
display_ext = "bin"

[meta]
created_by = "Capsule Guest JSON-RPC E2E"
created_at = "2099-01-23T12:00:00Z"
hash_algo = "blake3"

[policy]
ttl = 3600
timeout = 30

[permissions]
allow_hosts = []
allow_env = []

[ownership]
owner_capsule = "did:key:test"
write_allowed = {}
"#,
        if write_allowed { "true" } else { "false" }
    );

    let sync_path = temp_dir.join("guest-jsonrpc-e2e.sync");
    let file = File::create(&sync_path).unwrap();
    let mut zip = ZipWriter::new(file);

    let options: FileOptions<()> =
        FileOptions::default().compression_method(zip::CompressionMethod::Stored);

    zip.start_file("manifest.toml", options).unwrap();
    zip.write_all(manifest_toml.as_bytes()).unwrap();

    zip.start_file("payload", options).unwrap();
    zip.write_all(payload).unwrap();

    zip.start_file("context.json", options).unwrap();
    zip.write_all(br#"{"ok":true}"#).unwrap();

    zip.finish().unwrap();
    sync_path
}

fn context_value(sync_path: &Path, role: &str, permissions: Value) -> Value {
    json!({
        "mode": "Headless",
        "role": role,
        "permissions": permissions,
        "sync_path": sync_path.to_string_lossy(),
        "host_app": null
    })
}

fn full_permissions() -> Value {
    json!({
        "can_read_payload": true,
        "can_read_context": true,
        "can_write_payload": true,
        "can_write_context": true,
        "can_execute_wasm": false,
        "allowed_hosts": [],
        "allowed_env": []
    })
}

fn no_write_permissions() -> Value {
    json!({
        "can_read_payload": true,
        "can_read_context": true,
        "can_write_payload": false,
        "can_write_context": false,
        "can_execute_wasm": false,
        "allowed_hosts": [],
        "allowed_env": []
    })
}

fn run_with_stdin(sync_path: &Path, stdin_payload: &str) -> (Value, bool) {
    let mut cmd = Command::cargo_bin("ato").unwrap();
    let output = cmd
        .arg("guest")
        .arg(sync_path)
        .write_stdin(stdin_payload.to_string())
        .output()
        .unwrap();
    let body: Value = serde_json::from_slice(&output.stdout).expect("response is valid JSON");
    (body, output.status.success())
}

fn run_jsonrpc(sync_path: &Path, request: &Value) -> Value {
    let stdin = serde_json::to_string(request).unwrap();
    let (body, ok) = run_with_stdin(sync_path, &stdin);
    assert!(ok, "ato guest should exit cleanly even on protocol errors");
    body
}

#[test]
fn jsonrpc_payload_read_returns_object_wrapper() {
    let temp = TempDir::new().unwrap();
    let payload = b"hello world";
    let sync = create_test_sync_file(temp.path(), payload, false);

    let req = json!({
        "jsonrpc": "2.0",
        "id": "req-read-1",
        "method": "capsule/payload.read",
        "params": { "context": context_value(&sync, "Owner", full_permissions()) }
    });
    let resp = run_jsonrpc(&sync, &req);

    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], "req-read-1");
    let result = resp.get("result").expect("expected result, got error");
    assert_eq!(result["payload_b64"], encode_b64(payload));
    assert!(resp.get("error").is_none());
}

#[test]
fn jsonrpc_payload_write_returns_null_result() {
    let temp = TempDir::new().unwrap();
    let sync = create_test_sync_file(temp.path(), b"old", true);

    let req = json!({
        "jsonrpc": "2.0",
        "id": "req-write-1",
        "method": "capsule/payload.write",
        "params": {
            "context": context_value(&sync, "Owner", full_permissions()),
            "payload_b64": encode_b64(b"updated bytes"),
        }
    });
    let resp = run_jsonrpc(&sync, &req);

    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], "req-write-1");
    assert_eq!(resp["result"], Value::Null);
    assert!(resp.get("error").is_none());
}

#[test]
fn jsonrpc_context_read_returns_value_wrapper() {
    let temp = TempDir::new().unwrap();
    let sync = create_test_sync_file(temp.path(), b"x", false);

    let req = json!({
        "jsonrpc": "2.0",
        "id": "req-ctx-read",
        "method": "capsule/context.read",
        "params": { "context": context_value(&sync, "Owner", full_permissions()) }
    });
    let resp = run_jsonrpc(&sync, &req);

    let result = resp.get("result").expect("expected result");
    assert_eq!(result["value"], json!({ "ok": true }));
}

#[test]
fn jsonrpc_context_write_returns_null_result() {
    let temp = TempDir::new().unwrap();
    let sync = create_test_sync_file(temp.path(), b"x", true);

    let req = json!({
        "jsonrpc": "2.0",
        "id": "req-ctx-write",
        "method": "capsule/context.write",
        "params": {
            "context": context_value(&sync, "Owner", full_permissions()),
            "value": { "k": "v" }
        }
    });
    let resp = run_jsonrpc(&sync, &req);

    assert_eq!(resp["result"], Value::Null);
    assert!(resp.get("error").is_none());
}

#[test]
fn jsonrpc_wasm_execute_is_deferred_returns_method_not_found() {
    let temp = TempDir::new().unwrap();
    let sync = create_test_sync_file(temp.path(), b"x", false);

    let req = json!({
        "jsonrpc": "2.0",
        "id": "req-wasm",
        "method": "capsule/wasm.execute",
        "params": { "context": context_value(&sync, "Owner", full_permissions()) }
    });
    let resp = run_jsonrpc(&sync, &req);

    let err = resp.get("error").expect("wasm.execute should be Method not found");
    assert_eq!(err["code"], -32601);
    assert_eq!(resp["id"], "req-wasm");
}

#[test]
fn jsonrpc_unknown_method_returns_minus_32601_with_id_echoed() {
    let temp = TempDir::new().unwrap();
    let sync = create_test_sync_file(temp.path(), b"x", false);

    let req = json!({
        "jsonrpc": "2.0",
        "id": 42,
        "method": "totally/unknown",
        "params": { "context": context_value(&sync, "Owner", full_permissions()) }
    });
    let resp = run_jsonrpc(&sync, &req);

    let err = resp.get("error").expect("unknown method must error");
    assert_eq!(err["code"], -32601);
    assert_eq!(resp["id"], 42);
}

#[test]
fn jsonrpc_invalid_params_returns_minus_32602_for_missing_payload() {
    let temp = TempDir::new().unwrap();
    let sync = create_test_sync_file(temp.path(), b"x", true);

    let req = json!({
        "jsonrpc": "2.0",
        "id": "req-bad-params",
        "method": "capsule/payload.write",
        "params": { "context": context_value(&sync, "Owner", full_permissions()) }
    });
    let resp = run_jsonrpc(&sync, &req);

    let err = resp.get("error").expect("missing payload_b64 → -32602");
    assert_eq!(err["code"], -32602);
}

#[test]
fn jsonrpc_parse_error_returns_minus_32700_with_id_null() {
    let temp = TempDir::new().unwrap();
    let sync = create_test_sync_file(temp.path(), b"x", false);

    let (resp, ok) = run_with_stdin(&sync, "{ this is not json");
    assert!(ok);
    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], Value::Null);
    let err = resp.get("error").expect("parse error must include error");
    assert_eq!(err["code"], -32700);
}

#[test]
fn jsonrpc_invalid_version_returns_minus_32600() {
    let temp = TempDir::new().unwrap();
    let sync = create_test_sync_file(temp.path(), b"x", false);

    // JSON-RPC envelope but wrong version → routed to JSON-RPC handler which
    // rejects via JsonRpcRequest::validate (-32600 INVALID_REQUEST).
    let req = json!({
        "jsonrpc": "1.0",
        "id": "req-bad-ver",
        "method": "capsule/payload.read",
        "params": { "context": context_value(&sync, "Owner", full_permissions()) }
    });
    // The envelope detector currently checks `jsonrpc == "2.0"` exactly. A
    // value of "1.0" falls through to the unknown-envelope path, which also
    // returns -32600. Either route is acceptable per the spec.
    let (resp, ok) = run_with_stdin(&sync, &serde_json::to_string(&req).unwrap());
    assert!(ok);
    let err = resp.get("error").expect("non-2.0 jsonrpc must error");
    assert_eq!(err["code"], -32600);
}

#[test]
fn jsonrpc_unknown_envelope_returns_jsonrpc_minus_32600() {
    let temp = TempDir::new().unwrap();
    let sync = create_test_sync_file(temp.path(), b"x", false);

    // Valid JSON, but neither `jsonrpc=2.0` nor `version=guest.v1`.
    let body = json!({ "hello": "world" });
    let (resp, ok) = run_with_stdin(&sync, &body.to_string());
    assert!(ok);
    assert_eq!(resp["jsonrpc"], "2.0");
    let err = resp.get("error").expect("unknown envelope → -32600");
    assert_eq!(err["code"], -32600);
}

#[test]
fn jsonrpc_permission_denied_returns_minus_32001() {
    let temp = TempDir::new().unwrap();
    let sync = create_test_sync_file(temp.path(), b"x", true);

    // Write request with Owner role but no write capability.
    let req = json!({
        "jsonrpc": "2.0",
        "id": "req-perm",
        "method": "capsule/payload.write",
        "params": {
            "context": context_value(&sync, "Owner", no_write_permissions()),
            "payload_b64": encode_b64(b"new")
        }
    });
    let resp = run_jsonrpc(&sync, &req);

    let err = resp.get("error").expect("write without permission → -32001");
    assert_eq!(err["code"], -32001);
    assert_eq!(resp["id"], "req-perm");
}

#[test]
fn jsonrpc_envelope_priority_jsonrpc_wins_when_both_fields_present() {
    let temp = TempDir::new().unwrap();
    let sync = create_test_sync_file(temp.path(), b"hi", false);

    // Both `jsonrpc=2.0` and `version=guest.v1` set. Detector must pick JSON-RPC.
    let req = json!({
        "jsonrpc": "2.0",
        "version": "guest.v1",
        "id": "req-priority",
        "method": "capsule/payload.read",
        "params": { "context": context_value(&sync, "Owner", full_permissions()) }
    });
    let resp = run_jsonrpc(&sync, &req);

    // JSON-RPC path returns object-wrapped result; legacy path would return a
    // raw base64 string under `result`. Asserting on the wrapper proves which
    // dispatcher was selected.
    assert_eq!(resp["jsonrpc"], "2.0");
    let result = resp.get("result").expect("expected JSON-RPC success result");
    assert_eq!(result["payload_b64"], encode_b64(b"hi"));
}

#[test]
fn env_rename_capsule_ipc_role_rejects_mismatch() {
    let temp = TempDir::new().unwrap();
    let sync = create_test_sync_file(temp.path(), b"x", false);

    // Request says role=Owner; env CAPSULE_IPC_ROLE=consumer mismatches.
    let req = json!({
        "jsonrpc": "2.0",
        "id": "req-env",
        "method": "capsule/payload.read",
        "params": { "context": context_value(&sync, "Owner", full_permissions()) }
    });
    let mut cmd = Command::cargo_bin("ato").unwrap();
    let output = cmd
        .arg("guest")
        .arg(&sync)
        .env("CAPSULE_IPC_ROLE", "consumer")
        .write_stdin(serde_json::to_string(&req).unwrap())
        .output()
        .unwrap();
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    let err = body.get("error").expect("env mismatch → error");
    // `InvalidRequest` GuestErrorCode → -32602 INVALID_PARAMS via to_jsonrpc_code.
    assert_eq!(err["code"], -32602);
}

#[test]
fn env_rename_legacy_guest_role_is_ignored() {
    let temp = TempDir::new().unwrap();
    let sync = create_test_sync_file(temp.path(), b"x", false);

    // Old GUEST_ROLE env var must NOT be honoured (rename: no fallback).
    // Setting GUEST_ROLE=consumer while context.role=Owner should NOT trigger
    // a mismatch; the request should succeed.
    let req = json!({
        "jsonrpc": "2.0",
        "id": "req-legacy-env",
        "method": "capsule/payload.read",
        "params": { "context": context_value(&sync, "Owner", full_permissions()) }
    });
    let mut cmd = Command::cargo_bin("ato").unwrap();
    let output = cmd
        .arg("guest")
        .arg(&sync)
        .env("GUEST_ROLE", "consumer")
        .write_stdin(serde_json::to_string(&req).unwrap())
        .output()
        .unwrap();
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        body.get("error").is_none(),
        "legacy GUEST_ROLE must be ignored, got error: {body}"
    );
    assert_eq!(body["id"], "req-legacy-env");
}
