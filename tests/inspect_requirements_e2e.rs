use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use assert_cmd::Command;
use serde_json::Value;
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

fn requirements_manifest(name: &str) -> String {
    format!(
        r#"
schema_version = "0.2"
name = "{name}"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/{name}:latest"
required_env = ["CLOUDFLARE_API_TOKEN", "CLOUDFLARE_ACCOUNT_ID"]

[isolation]
allow_env = ["LOG_LEVEL"]

[network]
egress_allow = ["api.example.com"]
egress_id_allow = [{{ type = "cidr", value = "10.0.0.0/8" }}]

[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "primary-data"
attach = "explicit"
schema_id = "vaultwarden/data/v1"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#
    )
}

fn parse_json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("valid json")
}

fn spawn_capsule_detail_server(expected_path: &'static str, body: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind local server");
    let addr = listener.local_addr().expect("listener addr");

    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept request");
        let mut request = [0u8; 4096];
        let size = stream.read(&mut request).expect("read request");
        let request_text = String::from_utf8_lossy(&request[..size]);
        let status_line = if request_text.starts_with(&format!("GET {expected_path} ")) {
            "HTTP/1.1 200 OK"
        } else {
            "HTTP/1.1 404 Not Found"
        };
        let response_body = if status_line.ends_with("200 OK") {
            body
        } else {
            r#"{"error":"not found"}"#.to_string()
        };
        let response = format!(
            "{status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");
    });

    format!("http://{}", addr)
}

#[test]
fn inspect_requirements_json_succeeds_for_local_manifest() {
    let temp = TempDir::new().unwrap();
    write_file(
        &temp.path().join("capsule.toml"),
        &requirements_manifest("inspect-local"),
    );

    let output = capsule()
        .args(["inspect", "requirements"])
        .arg(temp.path())
        .arg("--json")
        .output()
        .unwrap();

    assert!(output.status.success(), "{:?}", output);
    let payload = parse_json(&output.stdout);

    assert_eq!(payload["schemaVersion"], "1");
    assert_eq!(payload["target"]["kind"], "local");
    assert_eq!(
        payload["target"]["resolved"]["path"],
        temp.path().canonicalize().unwrap().display().to_string()
    );
    assert_eq!(
        payload["requirements"]["secrets"][0]["key"],
        "CLOUDFLARE_API_TOKEN"
    );
    assert_eq!(payload["requirements"]["secrets"][0]["required"], true);
    assert_eq!(
        payload["requirements"]["env"][0]["key"],
        "CLOUDFLARE_ACCOUNT_ID"
    );
    assert_eq!(payload["requirements"]["env"][0]["required"], true);
    assert_eq!(payload["requirements"]["env"][1]["key"], "LOG_LEVEL");
    assert_eq!(payload["requirements"]["env"][1]["required"], false);
    assert_eq!(payload["requirements"]["state"][0]["key"], "data");
    assert_eq!(
        payload["requirements"]["state"][0]["durability"],
        "persistent"
    );
    assert_eq!(payload["requirements"]["services"][0]["key"], "main");
    assert_eq!(
        payload["requirements"]["network"][0]["key"],
        "external-network"
    );

    let consent = payload["requirements"]["consent"].as_array().unwrap();
    assert!(consent
        .iter()
        .any(|item| item.get("key").and_then(Value::as_str) == Some("filesystem.write")));
    assert!(consent
        .iter()
        .any(|item| item.get("key").and_then(Value::as_str) == Some("network.egress")));
    assert!(consent
        .iter()
        .any(|item| item.get("key").and_then(Value::as_str) == Some("secrets.access")));
}

#[test]
fn inspect_requirements_json_succeeds_for_remote_manifest() {
    let manifest = requirements_manifest("inspect-remote");
    let expected_path = "/v1/manifest/capsules/by/demo/inspect-remote";
    let base_url = spawn_capsule_detail_server(
        expected_path,
        serde_json::json!({
            "id": "capsule-demo-inspect-remote",
            "scoped_id": "demo/inspect-remote",
            "slug": "inspect-remote",
            "name": "inspect-remote",
            "description": "inspect remote fixture",
            "price": 0,
            "currency": "USD",
            "latestVersion": "1.0.0",
            "manifest_toml": manifest,
            "releases": [{"version": "1.0.0"}]
        })
        .to_string(),
    );

    let output = capsule()
        .args([
            "inspect",
            "requirements",
            "demo/inspect-remote",
            "--registry",
            &base_url,
            "--json",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "{:?}", output);
    let payload = parse_json(&output.stdout);

    assert_eq!(payload["schemaVersion"], "1");
    assert_eq!(payload["target"]["kind"], "remote");
    assert_eq!(payload["target"]["resolved"]["publisher"], "demo");
    assert_eq!(payload["target"]["resolved"]["slug"], "inspect-remote");
    assert_eq!(
        payload["requirements"]["state"][0]["schemaId"],
        "vaultwarden/data/v1"
    );
}

#[test]
fn inspect_requirements_json_fails_closed_when_manifest_is_missing() {
    let temp = TempDir::new().unwrap();

    let output = capsule()
        .args(["inspect", "requirements"])
        .arg(temp.path())
        .arg("--json")
        .output()
        .unwrap();

    assert!(!output.status.success(), "{:?}", output);
    assert!(String::from_utf8_lossy(&output.stdout).trim().is_empty());

    let payload = parse_json(&output.stderr);
    assert_eq!(payload["error"]["code"], "CAPSULE_TOML_NOT_FOUND");
    assert_eq!(payload["error"]["message"], "capsule.toml was not found");
    assert_eq!(
        payload["error"]["details"]["input"],
        temp.path().display().to_string()
    );
}
