//! `ato run capsules/<owner>/<name>` and `ato hub publish` integration tests.
//!
//! Each test runs the real `ato` binary against an in-process stub API on a
//! loopback port, so the CLI wiring (the reserved `capsules/` prefix, the
//! local-path precedence, digest verification before execution, run accounting
//! and publishing) is exercised end to end without touching the network.
//!
//! The fixture is the committed `samples/interop-static.capsule`: a wire v3
//! portable application the existing `ato run` path verifies and executes.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use assert_cmd::Command as AssertCommand;
use ato_portable_application::build_static_bundle;
use base64::Engine as _;
use serde_json::{Value, json};

const INFO_DERIVATION: &str =
    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn information_body(revision: u64, info: Value) -> Value {
    json!({"digest": SAMPLE_DIGEST, "derivation_ref": INFO_DERIVATION,
        "source": "publisher", "revision": revision, "updated_at": null, "info": info})
}

#[test]
fn describe_reads_a_pinned_version_without_downloading_or_authenticating() {
    let home = tempfile::tempdir().unwrap();
    let api = StubApi::new(vec![
        json_response("200 OK", resolve_body(&sample_bytes())),
        json_response("200 OK", information_body(0, json!({}))),
    ]);
    let output = ato_with_home(home.path())
        .args([
            "hub",
            "describe",
            "capsules/koh0920/interop-static",
            "--derivation",
            INFO_DERIVATION,
            "--api-url",
            &api.base,
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        serde_json::from_str::<Value>(&stdout(&output)).unwrap()["revision"],
        0
    );
    let requests = api.wait_requests(2);
    assert_eq!(requests.len(), 2);
    assert!(requests[1].starts_with(&format!("GET /v1/hub/capsules/koh0920/interop-static/versions/{SAMPLE_DIGEST}/derivations/{INFO_DERIVATION}/info ")));
    assert!(
        requests
            .iter()
            .all(|r| !r.to_ascii_lowercase().contains("authorization:"))
    );
}

#[test]
fn describe_writes_explicit_revision_to_resolved_digest_with_owner_auth() {
    let home = tempfile::tempdir().unwrap();
    let info = json!({"label":"Build from source", "estimated_first_run_seconds":2700,
        "dependencies":[{"name":"Rust", "version":"1.95.0", "provisioning":"managed", "purpose":"build"}]});
    let input = home.path().join("info.json");
    fs::write(
        &input,
        serde_json::to_vec(&json!({"expected_revision":4,"info":info})).unwrap(),
    )
    .unwrap();
    let api = StubApi::new(vec![
        json_response("200 OK", resolve_body(&sample_bytes())),
        json_response("200 OK", information_body(5, info.clone())),
    ]);
    let output = ato_with_home(home.path())
        .args([
            "hub",
            "describe",
            "capsules/koh0920/interop-static",
            "--derivation",
            INFO_DERIVATION,
            "--api-url",
            &api.base,
            "--file",
            input.to_str().unwrap(),
            "--token",
            "info-owner-token",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let requests = api.wait_requests(2);
    assert!(!requests[0].to_ascii_lowercase().contains("authorization:"));
    assert!(requests[1].starts_with("PUT "));
    assert!(
        requests[1]
            .to_ascii_lowercase()
            .contains("authorization: bearer info-owner-token")
    );
    let body: Value = serde_json::from_str(requests[1].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body, json!({"expected_revision":4,"info":info}));
    assert!(!stdout(&output).contains("info-owner-token"));
}

#[test]
fn describe_conflict_is_not_retried_or_reported_as_saved() {
    let home = tempfile::tempdir().unwrap();
    let input = home.path().join("info.json");
    fs::write(&input, br#"{"expected_revision":1,"info":{}}"#).unwrap();
    let api = StubApi::new(vec![
        json_response("200 OK", resolve_body(&sample_bytes())),
        json_response("409 Conflict", json!({"error":"revision_conflict"})),
    ]);
    let output = ato_with_home(home.path())
        .args([
            "hub",
            "describe",
            "capsules/koh0920/interop-static",
            "--derivation",
            INFO_DERIVATION,
            "--api-url",
            &api.base,
            "--file",
            input.to_str().unwrap(),
            "--token",
            "info-owner-token",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("revision_conflict"));
    assert!(stdout(&output).is_empty());
    assert_eq!(api.wait_requests(2).len(), 2);
}

#[test]
fn describe_refuses_malformed_write_intent_before_network() {
    let home = tempfile::tempdir().unwrap();
    let input = home.path().join("info.json");
    fs::write(&input, br#"{"info":{"label":"missing revision"}}"#).unwrap();
    let api = StubApi::new(vec![]);
    let output = ato_with_home(home.path())
        .args([
            "hub",
            "describe",
            "capsules/koh0920/interop-static",
            "--derivation",
            INFO_DERIVATION,
            "--api-url",
            &api.base,
            "--file",
            input.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("expected_revision"));
    assert!(api.requests.lock().unwrap().is_empty());
}

#[test]
fn describe_refuses_a_different_version_before_writing() {
    let home = tempfile::tempdir().unwrap();
    let api = StubApi::new(vec![json_response("200 OK", resolve_body(&sample_bytes()))]);
    let reference = format!("capsules/koh0920/interop-static@sha256:{}", "0".repeat(64));
    let output = ato_with_home(home.path())
        .args([
            "hub",
            "describe",
            &reference,
            "--derivation",
            INFO_DERIVATION,
            "--api-url",
            &api.base,
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(stderr(&output).contains("different digest"));
    assert_eq!(api.wait_requests(1).len(), 1);
}

#[test]
fn describe_refuses_redirects_and_mismatched_acknowledgements() {
    for response in [
        b"HTTP/1.1 302 Found\r\nlocation: https://other.invalid/\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_vec(),
        json_response("200 OK", information_body(7, json!({}))),
        json_response("200 OK", json!({"digest": SAMPLE_DIGEST,"derivation_ref":format!("sha256:{}","0".repeat(64)),
            "source":"publisher","revision":1,"info":{}})),
    ] {
        let home = tempfile::tempdir().unwrap();
        let input = home.path().join("info.json");
        fs::write(&input, br#"{"expected_revision":0,"info":{}}"#).unwrap();
        let api = StubApi::new(vec![json_response("200 OK", resolve_body(&sample_bytes())), response]);
        let output = ato_with_home(home.path()).args(["hub", "describe", "capsules/koh0920/interop-static",
            "--derivation", INFO_DERIVATION, "--api-url", &api.base, "--file", input.to_str().unwrap(),
            "--token", "info-owner-token"]).output().unwrap();
        assert!(!output.status.success());
        assert!(stdout(&output).is_empty());
        assert_eq!(api.wait_requests(2).len(), 2);
    }
}

/// SHA-256 of the committed interop-static bundle bytes.
const SAMPLE_DIGEST: &str =
    "sha256:95b837f4e8ed3c3354a4560e56820030cdb2ba34ba2a7c65aa6d26e139bdd813";

fn repo_sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static.capsule")
}

fn readme_sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static-README.md")
}

fn sample_bytes() -> Vec<u8> {
    fs::read(repo_sample()).expect("the interop-static fixture is committed")
}

fn ato() -> AssertCommand {
    let mut command =
        AssertCommand::cargo_bin("ato").expect("the ato binary is built for integration tests");
    command.timeout(Duration::from_secs(120));
    // Neither the API origin nor a bearer token may come from the environment.
    command.env_remove("ATO_API_URL");
    command.env_remove("ATO_API_TOKEN");
    command
}

fn ato_with_home(home: &Path) -> AssertCommand {
    let mut command = ato();
    command.env("ATO_HOME", home);
    command
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

// ---- a loopback stub API --------------------------------------------------

fn http_response(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

fn json_response(status: &str, value: Value) -> Vec<u8> {
    http_response(
        status,
        "application/json",
        &serde_json::to_vec(&value).unwrap(),
    )
}

fn binary_response(body: &[u8]) -> Vec<u8> {
    http_response("200 OK", "application/octet-stream", body)
}

fn resolve_body(bytes: &[u8]) -> Value {
    json!({
        "ref": "capsules/koh0920/interop-static",
        "tag": "latest",
        "digest": SAMPLE_DIGEST,
        "size_bytes": bytes.len(),
        "artifact_path": format!(
            "/v1/hub/capsules/koh0920/interop-static/artifacts/{SAMPLE_DIGEST}"
        ),
        "published_at": "2026-11-16 05:00:00",
        "verification": { "state": "experimental", "successful_runs": 0, "last_run_at": null },
    })
}

/// Read one request and report how many body bytes arrived.
///
/// With keep_body false only the head is materialised, so a multi-megabyte
/// presigned PUT never becomes a String. The body length is always recorded.
fn read_request_recording(mut stream: TcpStream, keep_body: bool) -> (String, usize) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        if std::time::Instant::now() > deadline {
            break;
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                buffer.extend_from_slice(&chunk[..read]);
                if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buffer[..position]).to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length:")
                                .map(|value| value.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if buffer.len() >= position + 4 + length {
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }
    let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
        return (String::from_utf8_lossy(&buffer).into_owned(), 0);
    };
    let body = buffer.len().saturating_sub(position + 4);
    let recorded = if keep_body {
        String::from_utf8_lossy(&buffer).into_owned()
    } else {
        String::from_utf8_lossy(&buffer[..position]).into_owned()
    };
    (recorded, body)
}

struct StubApi {
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
    body_lengths: Arc<Mutex<Vec<usize>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl StubApi {
    fn new(responses: Vec<Vec<u8>>) -> Self {
        Self::start(responses, false)
    }

    /// Records request heads and body lengths instead of whole bodies: a
    /// presigned PUT carries the multi-megabyte bundle itself.
    fn discarding(responses: Vec<Vec<u8>>) -> Self {
        Self::start(responses, true)
    }

    fn start(responses: Vec<Vec<u8>>, discard_body: bool) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind a loopback port");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let body_lengths = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let requests = Arc::clone(&requests);
            let body_lengths = Arc::clone(&body_lengths);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                for response in responses {
                    // The CLI may validate/hash the large bundle between requests.
                    // Keep the stub alive for the same bound as ato(), including
                    // debug builds on a loaded host. Drop still stops it promptly.
                    let deadline = std::time::Instant::now() + Duration::from_secs(120);
                    let stream = loop {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        match listener.accept() {
                            Ok((stream, _)) => break stream,
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                if std::time::Instant::now() > deadline {
                                    return;
                                }
                                thread::sleep(Duration::from_millis(2));
                            }
                            Err(_) => return,
                        }
                    };
                    let (request, body) =
                        read_request_recording(stream.try_clone().unwrap(), !discard_body);
                    requests.lock().unwrap().push(request);
                    body_lengths.lock().unwrap().push(body);
                    let mut stream = stream;
                    let _ = stream.write_all(&response);
                    let _ = stream.flush();
                }
            })
        };
        Self {
            base,
            requests,
            body_lengths,
            stop,
            handle: Some(handle),
        }
    }

    fn wait_requests(&self, count: usize) -> Vec<String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let requests = self.requests.lock().unwrap().clone();
            if requests.len() >= count || std::time::Instant::now() > deadline {
                return requests;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }

    fn wait_body_lengths(&self, count: usize) -> Vec<usize> {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let lengths = self.body_lengths.lock().unwrap().clone();
            if lengths.len() >= count || std::time::Instant::now() > deadline {
                return lengths;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for StubApi {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn body_of(request: &str) -> Value {
    let body = request
        .split("\r\n\r\n")
        .nth(1)
        .expect("the request has a body");
    serde_json::from_str(body).expect("the request body is JSON")
}

/// The DerivationRef of the route whose `ato.native@1` step declares `os`.
///
/// The bundle is the encoded JSON document `ato pack` writes; its payloads are
/// the canonical objects, so the declared platform is readable without running
/// the route.
fn derivation_with_native_os(bytes: &[u8], os: &str) -> String {
    let value: Value = serde_json::from_slice(bytes).expect("a .capsule is a JSON document");
    for payload in value["payloads"]
        .as_array()
        .expect("the bundle lists payloads")
    {
        let Some(encoded) = payload["bytes"].as_str() else {
            continue;
        };
        let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
            continue;
        };
        let Ok(object) = serde_json::from_slice::<Value>(&decoded) else {
            continue;
        };
        let matches = object["steps"].as_array().is_some_and(|steps| {
            steps
                .iter()
                .any(|step| step["runtimes"]["ato.native.os"] == os)
        });
        if matches {
            return payload["reference"]
                .as_str()
                .expect("a payload carries its reference")
                .to_owned();
        }
    }
    panic!("no Derivation declares ato.native.os = {os}");
}

/// The `capability ...` lines `ato pack` printed for one Derivation.
fn pack_capability_lines(printed: &str, derivation: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut capturing = false;
    for line in printed.lines() {
        if let Some(rest) = line.strip_prefix("capability derivation_ref=") {
            let reference = rest.split(' ').next().unwrap_or_default();
            capturing = reference == derivation;
            if capturing {
                lines.push(line.to_owned());
            }
            continue;
        }
        if capturing {
            if line.starts_with("capability ") {
                lines.push(line.to_owned());
            } else {
                capturing = false;
            }
        }
    }
    lines
}
// ---- ato hub publish ------------------------------------------------------

#[test]
fn publishing_a_valid_capsule_reports_the_reference_and_web_url() {
    let bytes = sample_bytes();
    let api = StubApi::new(vec![json_response(
        "200 OK",
        json!({
            "ref": "capsules/koh0920/interop-static",
            "tag": "latest",
            "digest": SAMPLE_DIGEST,
            "size_bytes": bytes.len(),
            "url": "https://stg-app.ato.run/capsules/koh0920/interop-static",
            "created": true,
        }),
    )]);
    let home = tempfile::tempdir().unwrap();
    let output = ato_with_home(home.path())
        .arg("hub")
        .arg("publish")
        .arg(repo_sample())
        .args(["--name", "koh0920/interop-static", "--tag", "latest"])
        .args(["--license", "MPL-2.0"])
        .arg("--readme")
        .arg(readme_sample())
        .args(["--repository", "https://github.com/ato-run/ato"])
        .args(["--surface", "web_ui"])
        .args(["--environment", "macos-arm64"])
        .args(["--summary", "Static web proof capsule"])
        .args(["--display-name", "Interop Static"])
        .args(["--api-url", &api.base])
        .args(["--token", "ato_dev_test_token"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "the publish failed: {}",
        stderr(&output)
    );
    let printed = stdout(&output);
    assert!(
        printed.contains(&format!("capsules/koh0920/interop-static@{SAMPLE_DIGEST}")),
        "{printed}"
    );
    assert!(
        printed.contains("https://stg-app.ato.run/capsules/koh0920/interop-static"),
        "{printed}"
    );
    assert!(!printed.contains("ato_dev_test_token"), "{printed}");
    assert!(!stderr(&output).contains("ato_dev_test_token"));

    let requests = api.wait_requests(1);
    assert_eq!(requests.len(), 1, "{requests:?}");
    let request = &requests[0];
    assert!(
        request.starts_with("PUT /v1/hub/capsules/koh0920/interop-static "),
        "{request}"
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer ato_dev_test_token"),
        "{request}"
    );
    let body = body_of(request);
    assert_eq!(body["tag"], "latest");
    assert_eq!(body["digest"], SAMPLE_DIGEST);
    assert_eq!(body["metadata"]["license_spdx"], "MPL-2.0");
    assert_eq!(body["metadata"]["display_name"], "Interop Static");
    assert_eq!(body["metadata"]["summary"], "Static web proof capsule");
    assert_eq!(
        body["metadata"]["repository"],
        "https://github.com/ato-run/ato"
    );
    // `--surface` / `--environment` stay declared; the capabilities object is
    // derived from the bundle and never overwritten by them.
    assert_eq!(body["metadata"]["surfaces"][0], "web_ui");
    assert_eq!(body["metadata"]["environments"][0], "macos-arm64");
    let capabilities = &body["metadata"]["capabilities"];
    assert_eq!(capabilities["source"], "bundle");
    assert!(
        capabilities["contract_ref"]
            .as_str()
            .is_some_and(|value| value.starts_with("sha256:") && value.len() == 71),
        "{capabilities}"
    );
    assert_eq!(capabilities["derivations"].as_array().unwrap().len(), 1);
    let derivation = &capabilities["derivations"][0];
    assert_eq!(derivation["realization"], "static_web");
    assert_eq!(derivation["platform"], Value::Null);
    assert_eq!(derivation["native_platform"], Value::Null);
    assert_eq!(derivation["requires"]["container"], false);
    assert_eq!(derivation["requires"]["state"], false);
    assert_eq!(derivation["surfaces"][0]["kind"], "static_web");
    assert_eq!(derivation["surfaces"][0]["port"], "app.http");
    assert!(
        body["metadata"]["readme_markdown"]
            .as_str()
            .unwrap()
            .contains("# Interop Static")
    );
    let decoded = {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(body["bundle_base64"].as_str().unwrap())
            .unwrap()
    };
    assert_eq!(decoded, bytes);
}

#[test]
fn publishing_without_a_credential_points_at_ato_login() {
    let home = tempfile::tempdir().unwrap();
    let output = ato_with_home(home.path())
        .arg("hub")
        .arg("publish")
        .arg(repo_sample())
        .args(["--name", "koh0920/interop-static"])
        .args(["--api-url", "https://staging.api.ato.run"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(
        message.contains("ato login --api-url https://staging.api.ato.run"),
        "{message}"
    );
}

#[test]
fn publishing_rejects_a_bundle_the_run_path_would_not_accept() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("computation-v2.capsule");
    fs::write(
        &path,
        format!(
            "{{\"index\":{{\"objects\":[],\"root\":\"blake3:{}\",\"version\":2}},\"payloads\":[]}}",
            "0".repeat(64)
        ),
    )
    .unwrap();
    let home = tempfile::tempdir().unwrap();
    let output = ato_with_home(home.path())
        .arg("hub")
        .arg("publish")
        .arg(&path)
        .args(["--name", "koh0920/interop-static"])
        .args(["--api-url", "http://127.0.0.1:9"])
        .args(["--token", "ato_dev_test_token"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("must not be published"),
        "{}",
        stderr(&output)
    );
}

// ---- ato run capsules/<owner>/<name> --------------------------------------

#[test]
fn running_a_hub_reference_resolves_verifies_executes_and_reports_the_run() {
    let bytes = sample_bytes();
    let api = StubApi::new(vec![
        json_response("200 OK", resolve_body(&bytes)),
        binary_response(&bytes),
        json_response("200 OK", json!({"counted": true, "run_count": 2})),
    ]);
    let home = tempfile::tempdir().unwrap();
    let evidence = tempfile::tempdir().unwrap();
    let receipt = evidence.path().join("receipt.json");
    let output = ato_with_home(home.path())
        .arg("run")
        .arg("capsules/koh0920/interop-static")
        .arg("--no-open")
        .arg("--verification-receipt")
        .arg(&receipt)
        .args(["--api-url", &api.base])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "the run failed: {}",
        stderr(&output)
    );

    let printed = stdout(&output);
    assert!(printed.contains("Capsule:"), "{printed}");
    assert!(
        stderr(&output).contains("Resolved capsules/koh0920/interop-static [tag=latest]"),
        "{}",
        stderr(&output)
    );

    let receipt_json: Value = serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
    assert_eq!(receipt_json["bundle_sha256"], SAMPLE_DIGEST);
    assert_eq!(receipt_json["fully_satisfied"], true);

    let cached = home.path().join("hub").join(format!(
        "{}.capsule",
        SAMPLE_DIGEST.trim_start_matches("sha256:")
    ));
    assert_eq!(fs::read(&cached).unwrap(), bytes);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&cached).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode is {:o}", mode & 0o777);
    }

    let requests = api.wait_requests(3);
    assert_eq!(requests.len(), 3, "{requests:?}");
    assert!(
        requests[0].starts_with("GET /v1/hub/capsules/koh0920/interop-static/resolve?tag=latest "),
        "{}",
        requests[0]
    );
    assert!(
        requests[1].starts_with(&format!(
            "GET /v1/hub/capsules/koh0920/interop-static/artifacts/{SAMPLE_DIGEST} "
        )),
        "{}",
        requests[1]
    );
    assert!(
        requests[2].starts_with("POST /v1/hub/capsules/koh0920/interop-static/runs "),
        "{}",
        requests[2]
    );
    let report = body_of(&requests[2]);
    assert_eq!(report["digest"], SAMPLE_DIGEST);
    assert_eq!(report["receipt"]["fully_satisfied"], true);
    assert_eq!(report["receipt"]["bundle_sha256"], SAMPLE_DIGEST);
    assert!(
        report["receipt"]["execution"]["attempt_id"].is_string(),
        "{report}"
    );
}

#[test]
fn a_tampered_hub_artifact_fails_before_the_run() {
    let bytes = sample_bytes();
    let mut tampered = bytes.clone();
    tampered[16] ^= 0xff;
    let api = StubApi::new(vec![
        json_response("200 OK", resolve_body(&bytes)),
        binary_response(&tampered),
    ]);
    let home = tempfile::tempdir().unwrap();
    let evidence = tempfile::tempdir().unwrap();
    let receipt = evidence.path().join("receipt.json");
    let output = ato_with_home(home.path())
        .arg("run")
        .arg("capsules/koh0920/interop-static")
        .arg("--no-open")
        .arg("--verification-receipt")
        .arg(&receipt)
        .args(["--api-url", &api.base])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("hub-digest-mismatch"), "{message}");
    assert!(!receipt.exists(), "a refused Run publishes no receipt");
    assert!(
        !home.path().join("hub").exists(),
        "a refused artifact is never stored or executed"
    );
}

#[test]
fn a_hub_artifact_of_the_wrong_size_fails_before_the_run() {
    let bytes = sample_bytes();
    let mut body = resolve_body(&bytes);
    body["size_bytes"] = Value::from(bytes.len() as u64 + 1);
    let api = StubApi::new(vec![json_response("200 OK", body), binary_response(&bytes)]);
    let home = tempfile::tempdir().unwrap();
    let output = ato_with_home(home.path())
        .arg("run")
        .arg("capsules/koh0920/interop-static")
        .arg("--no-open")
        .args(["--api-url", &api.base])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("hub-size-mismatch"),
        "{}",
        stderr(&output)
    );
    assert!(!home.path().join("hub").exists());
}

#[test]
fn a_computation_root_bundle_served_by_the_hub_is_refused() {
    // A Hub capsule is a portable application (wire v3/v4). If a server serves
    // computation-root v2 bytes, the Run path refuses them by reference domain
    // instead of relabelling a ComputationRef as a capsule identity.
    let bytes = format!(
        "{{\"index\":{{\"objects\":[],\"root\":\"blake3:{}\",\"version\":2}},\"payloads\":[]}}",
        "0".repeat(64)
    )
    .into_bytes();
    let digest = ato_portable_application::bundle_sha256(&bytes);
    let api = StubApi::new(vec![
        json_response(
            "200 OK",
            json!({
                "ref": "capsules/koh0920/interop-static",
                "tag": "latest",
                "digest": digest,
                "size_bytes": bytes.len(),
                "artifact_path": format!(
                    "/v1/hub/capsules/koh0920/interop-static/artifacts/{digest}"
                ),
            }),
        ),
        binary_response(&bytes),
    ]);
    let home = tempfile::tempdir().unwrap();
    let evidence = tempfile::tempdir().unwrap();
    let receipt = evidence.path().join("receipt.json");
    let output = ato_with_home(home.path())
        .arg("run")
        .arg("capsules/koh0920/interop-static")
        .arg("--no-open")
        .arg("--verification-receipt")
        .arg(&receipt)
        .args(["--api-url", &api.base])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("hub-invalid-bundle"),
        "{}",
        stderr(&output)
    );
    assert!(!receipt.exists(), "a refused Run publishes no receipt");
}

#[test]
fn a_run_refused_by_the_api_names_the_login_step() {
    let api = StubApi::new(vec![json_response(
        "401 Unauthorized",
        json!({"error": "unauthorized"}),
    )]);
    let home = tempfile::tempdir().unwrap();
    let output = ato_with_home(home.path())
        .arg("run")
        .arg("capsules/koh0920/interop-static")
        .arg("--no-open")
        .args(["--api-url", &api.base])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("hub-unauthorized"), "{message}");
    assert!(message.contains("ato login"), "{message}");
}

#[test]
fn an_existing_capsules_path_runs_locally_without_the_api() {
    let directory = tempfile::tempdir().unwrap();
    let local = directory.path().join("capsules/local");
    fs::create_dir_all(&local).unwrap();
    fs::copy(repo_sample(), local.join("interop-static.capsule")).unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut command = ato_with_home(home.path());
    command.current_dir(directory.path());
    let output = command
        .arg("run")
        .arg("capsules/local/interop-static.capsule")
        .arg("--no-open")
        // A dead API origin proves the local path never goes to the network.
        .args(["--api-url", "http://127.0.0.1:9"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "the local run failed: {}",
        stderr(&output)
    );
    assert!(!stderr(&output).contains("Resolved capsules/"));
}
#[test]
fn a_hub_capsule_this_host_cannot_run_is_refused_before_execution() {
    // A real native multi-derivation bundle, packed from the committed fixture.
    let built = tempfile::tempdir().unwrap();
    let bundle_path = built.path().join("native-multi.capsule");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/native-desktop-multi-d");
    let packed = ato()
        .arg("pack")
        .arg(&source)
        .arg("-o")
        .arg(&bundle_path)
        .output()
        .unwrap();
    assert!(packed.status.success(), "pack failed: {}", stderr(&packed));
    let packed_stdout = stdout(&packed);
    let bytes = fs::read(&bundle_path).unwrap();
    let windows = derivation_with_native_os(&bytes, "windows");
    let expected = pack_capability_lines(&packed_stdout, &windows);
    assert!(
        expected.iter().any(|line| line.contains("runnable=no")),
        "{packed_stdout}"
    );

    let digest = ato_portable_application::bundle_sha256(&bytes);
    let api = StubApi::new(vec![
        json_response(
            "200 OK",
            json!({
                "ref": "capsules/koh0920/native-multi",
                "tag": "latest",
                "digest": digest,
                "size_bytes": bytes.len(),
                "artifact_path": format!(
                    "/v1/hub/capsules/koh0920/native-multi/artifacts/{digest}"
                ),
            }),
        ),
        binary_response(&bytes),
    ]);
    let home = tempfile::tempdir().unwrap();
    let evidence = tempfile::tempdir().unwrap();
    let receipt = evidence.path().join("receipt.json");
    let output = ato_with_home(home.path())
        .arg("run")
        .arg("capsules/koh0920/native-multi")
        .args(["--derivation", &windows])
        .arg("--no-open")
        .arg("--verification-receipt")
        .arg(&receipt)
        .args(["--api-url", &api.base])
        .output()
        .unwrap();
    assert!(!output.status.success(), "the Run was not refused");
    let message = stderr(&output);
    assert!(message.contains("hub-capability-unavailable"), "{message}");
    assert!(message.contains("the Run was not started"), "{message}");
    assert!(message.contains("capability required "), "{message}");
    for line in &expected {
        assert!(
            message.contains(line),
            "the Hub Run diagnosis lacks `{line}`: {message}"
        );
    }
    assert!(
        !stdout(&output).contains("URL:"),
        "a refused Run printed a URL: {}",
        stdout(&output)
    );
    assert!(!receipt.exists(), "a refused Run publishes no receipt");
    // The refusal happens before the Run scratch is kept: no temporary Run
    // directory is left behind under the CLI's cache.
    let cache = home.path().join("cache");
    if cache.is_dir() {
        let leftovers: Vec<_> = fs::read_dir(&cache)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("ato-portable-run-"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "a refused Run kept scratch: {leftovers:?}"
        );
    }
    let requests = api.wait_requests(2);
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(
        requests.iter().all(|request| !request.starts_with("POST ")),
        "a refused Run reported nothing: {requests:?}"
    );
}

// ---- the large (presigned) lane -------------------------------------------

/// The static sample packed with one 9 MiB workspace file: a real wire v3
/// bundle above the 8 MiB Base64 bound that the local Run path accepts. Built
/// once per test binary, so no large fixture is committed.
const LARGE_PAYLOAD_BYTES: usize = 9 * 1024 * 1024;
const LARGE_REFERENCE: &str = "capsules/koh0920/large";
/// base64 of the raw SHA-256 bytes the API signs into the presigned PUT.
const LARGE_UPLOAD_CHECKSUM: &str = "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";
const LARGE_UPLOAD_ID: &str = "01J0000000000000000000000A";
const LARGE_UPLOAD_ID_RETRY: &str = "01J0000000000000000000000B";

struct LargeBundle {
    // The bundle file lives in this directory for the whole test process.
    _directory: tempfile::TempDir,
    path: PathBuf,
    bytes: Vec<u8>,
    digest: String,
}

fn large_bundle() -> &'static LargeBundle {
    static LARGE: OnceLock<LargeBundle> = OnceLock::new();
    LARGE.get_or_init(|| {
        let directory = tempfile::tempdir().expect("a temporary large-bundle directory");
        let source = directory.path().join("source");
        fs::create_dir_all(&source).unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static-k");
        for name in ["capsule.toml", "index.html", "proof.txt"] {
            fs::copy(fixture.join(name), source.join(name)).expect("copy the static sample");
        }
        // Deterministic bytes, so the fixture digest is stable across runs.
        let mut payload = vec![0_u8; LARGE_PAYLOAD_BYTES];
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        for byte in payload.iter_mut() {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            *byte = (state >> 33) as u8;
        }
        fs::write(source.join("big.bin"), &payload).unwrap();
        let (bytes, _bundle) =
            build_static_bundle(&source, "Large lane").expect("the large static sample packs");
        assert!(
            bytes.len() as u64 > 8 * 1024 * 1024,
            "the fixture must exceed the Base64 bound: {} bytes",
            bytes.len()
        );
        let path = directory.path().join("large.capsule");
        fs::write(&path, &bytes).unwrap();
        let digest = ato_portable_application::bundle_sha256(&bytes);
        LargeBundle {
            _directory: directory,
            path,
            bytes,
            digest,
        }
    })
}

fn large_upload_ticket(upload_url: &str, upload_id: &str) -> Value {
    let fixture = large_bundle();
    json!({
        "upload_id": upload_id,
        "ref": LARGE_REFERENCE,
        "tag": "latest",
        "digest": fixture.digest,
        "size_bytes": fixture.bytes.len(),
        "max_bytes": 1_000_000_000_u64,
        "method": "PUT",
        "upload_url": upload_url,
        "upload_headers": {
            "content-type": "application/octet-stream",
            "x-amz-checksum-sha256": LARGE_UPLOAD_CHECKSUM,
        },
        "upload_expires_in": 900,
        "expires_at": "2026-11-16T06:00:00.000Z",
        "complete_path": format!("/v1/hub/capsules/koh0920/large/uploads/{upload_id}/complete"),
    })
}

fn large_publish_body() -> Value {
    let fixture = large_bundle();
    json!({
        "ref": LARGE_REFERENCE,
        "tag": "latest",
        "digest": fixture.digest,
        "size_bytes": fixture.bytes.len(),
        "url": "https://stg-app.ato.run/capsules/koh0920/large",
        "created": true,
    })
}

fn large_resolve_body() -> Value {
    let fixture = large_bundle();
    json!({
        "ref": LARGE_REFERENCE,
        "tag": "latest",
        "digest": fixture.digest,
        "size_bytes": fixture.bytes.len(),
        "artifact_path": format!("{LARGE_REFERENCE}/artifacts/{}", fixture.digest)
            .replace("capsules/", "/v1/hub/capsules/"),
        "published_at": "2026-11-16 05:00:00",
        "verification": { "state": "experimental", "successful_runs": 0, "last_run_at": null },
    })
}

fn large_download_url_body(url: &str) -> Value {
    let fixture = large_bundle();
    json!({
        "ref": LARGE_REFERENCE,
        "digest": fixture.digest,
        "size_bytes": fixture.bytes.len(),
        "url": url,
        "expires_in": 600,
    })
}

fn publish_large_bundle(home: &Path, api_base: &str) -> Output {
    let fixture = large_bundle();
    ato_with_home(home)
        .arg("hub")
        .arg("publish")
        .arg(&fixture.path)
        .args(["--name", "koh0920/large", "--tag", "latest"])
        .args(["--token", "ato_dev_test_token"])
        .args(["--api-url", api_base])
        .output()
        .unwrap()
}

fn run_large_capsule(home: &Path, api_base: &str) -> Output {
    ato_with_home(home)
        .arg("run")
        .arg("capsules/koh0920/large")
        .arg("--no-open")
        .args(["--api-url", api_base])
        .output()
        .unwrap()
}

fn cached_large_path(home: &Path) -> PathBuf {
    let fixture = large_bundle();
    home.join("hub").join(format!(
        "{}.capsule",
        fixture.digest.trim_start_matches("sha256:")
    ))
}

#[test]
fn a_bundle_over_eight_mibuses_the_presigned_upload_lane() {
    let fixture = large_bundle();
    let upload = StubApi::discarding(vec![http_response(
        "200 OK",
        "application/octet-stream",
        b"",
    )]);
    let api = StubApi::new(vec![
        json_response(
            "201 Created",
            large_upload_ticket(&format!("{}/objects/one", upload.base), LARGE_UPLOAD_ID),
        ),
        json_response("200 OK", large_publish_body()),
    ]);
    let home = tempfile::tempdir().unwrap();
    let output = publish_large_bundle(home.path(), &api.base);
    assert!(output.status.success(), "{}", stderr(&output));
    let printed = stdout(&output);
    assert!(
        printed.contains(&format!("{LARGE_REFERENCE}@{}", fixture.digest)),
        "{printed}"
    );

    let requests = api.wait_requests(2);
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(
        requests[0].starts_with("POST /v1/hub/capsules/koh0920/large/uploads "),
        "{}",
        requests[0]
    );
    let start = body_of(&requests[0]);
    assert_eq!(start["size_bytes"], fixture.bytes.len());
    assert_eq!(start["digest"], fixture.digest);
    // The declared privilege is never asserted by the client.
    assert!(start.get("official").is_none(), "{start}");
    assert!(start.get("max_bytes").is_none(), "{start}");
    assert!(
        requests[1].starts_with(&format!(
            "POST /v1/hub/capsules/koh0920/large/uploads/{LARGE_UPLOAD_ID}/complete "
        )),
        "{}",
        requests[1]
    );
    let complete = body_of(&requests[1]);
    assert_eq!(complete["metadata"]["display_name"], Value::Null);
    assert!(
        complete["metadata"]["capabilities"]["derivations"]
            .as_array()
            .is_some_and(|derivations| !derivations.is_empty()),
        "{complete}"
    );
    assert!(
        complete["metadata"]["capabilities"]["contract_ref"].is_string(),
        "{complete}"
    );

    // The presigned PUT went to the upload URL only, carried the signed headers
    // verbatim, and streamed exactly the bundle bytes.
    let puts = upload.wait_requests(1);
    assert_eq!(puts.len(), 1, "{puts:?}");
    assert!(puts[0].starts_with("PUT /objects/one "), "{}", puts[0]);
    let head = puts[0].to_ascii_lowercase();
    assert!(
        head.contains("content-type: application/octet-stream"),
        "{head}"
    );
    assert!(
        head.contains(&format!(
            "x-amz-checksum-sha256: {}",
            LARGE_UPLOAD_CHECKSUM.to_ascii_lowercase()
        )),
        "{head}"
    );
    // No API credential ever reaches the presigned host.
    assert!(!head.contains("authorization:"), "{head}");
    assert_eq!(
        upload.wait_body_lengths(1),
        vec![fixture.bytes.len()],
        "the streamed body is the bundle itself"
    );
}

#[test]
fn an_official_publisher_refusal_falls_back_to_the_base64_lane() {
    let api = StubApi::new(vec![json_response(
        "403 Forbidden",
        json!({"error": "official_publisher_required", "max_bytes": 8 * 1024 * 1024}),
    )]);
    let home = tempfile::tempdir().unwrap();
    let output = publish_large_bundle(home.path(), &api.base);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(
        message.contains("this account is not an official publisher"),
        "{message}"
    );
    assert!(message.contains("Base64 JSON lane"), "{message}");
    // The fallback is the pre-M2 lane, whose local bound refuses the bundle.
    assert!(message.contains("hub-bundle-too-large"), "{message}");
    assert!(!stdout(&output).contains("@sha256:"), "{}", stdout(&output));
    let requests = api.wait_requests(1);
    assert_eq!(
        requests.len(),
        1,
        "no completion after the fallback: {requests:?}"
    );
}

#[test]
fn a_conflicting_completion_reuploads_the_bundle() {
    let fixture = large_bundle();
    let upload = StubApi::discarding(vec![
        http_response("200 OK", "application/octet-stream", b""),
        http_response("200 OK", "application/octet-stream", b""),
    ]);
    let api = StubApi::new(vec![
        json_response(
            "201 Created",
            large_upload_ticket(&format!("{}/objects/one", upload.base), LARGE_UPLOAD_ID),
        ),
        json_response("409 Conflict", json!({"error": "size_mismatch"})),
        json_response(
            "201 Created",
            large_upload_ticket(
                &format!("{}/objects/two", upload.base),
                LARGE_UPLOAD_ID_RETRY,
            ),
        ),
        json_response("200 OK", large_publish_body()),
    ]);
    let home = tempfile::tempdir().unwrap();
    let output = publish_large_bundle(home.path(), &api.base);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("restarting the staged upload"),
        "{}",
        stderr(&output)
    );

    let requests = api.wait_requests(4);
    assert_eq!(requests.len(), 4, "{requests:?}");
    assert!(
        requests[2].starts_with("POST /v1/hub/capsules/koh0920/large/uploads "),
        "{}",
        requests[2]
    );
    assert_eq!(
        upload.wait_body_lengths(2),
        vec![fixture.bytes.len(), fixture.bytes.len()],
        "the bundle was streamed again"
    );
}

#[test]
fn an_expired_upload_restarts_from_a_fresh_staged_upload() {
    let upload = StubApi::discarding(vec![
        http_response("200 OK", "application/octet-stream", b""),
        http_response("200 OK", "application/octet-stream", b""),
    ]);
    let api = StubApi::new(vec![
        json_response(
            "201 Created",
            large_upload_ticket(&format!("{}/objects/one", upload.base), LARGE_UPLOAD_ID),
        ),
        json_response("410 Gone", json!({"error": "upload_expired"})),
        json_response(
            "201 Created",
            large_upload_ticket(
                &format!("{}/objects/two", upload.base),
                LARGE_UPLOAD_ID_RETRY,
            ),
        ),
        json_response("200 OK", large_publish_body()),
    ]);
    let home = tempfile::tempdir().unwrap();
    let output = publish_large_bundle(home.path(), &api.base);
    assert!(output.status.success(), "{}", stderr(&output));
    let message = stderr(&output);
    assert!(message.contains("expired the staged upload"), "{message}");
    assert!(
        message.contains("restarting the staged upload"),
        "{message}"
    );
    let requests = api.wait_requests(4);
    assert_eq!(requests.len(), 4, "{requests:?}");
    assert_eq!(
        upload.request_count(),
        2,
        "both attempts streamed the bundle"
    );
}

#[test]
fn an_unreadable_bundle_index_is_a_bundle_defect() {
    let upload = StubApi::discarding(vec![http_response(
        "200 OK",
        "application/octet-stream",
        b"",
    )]);
    let api = StubApi::new(vec![
        json_response(
            "201 Created",
            large_upload_ticket(&format!("{}/objects/one", upload.base), LARGE_UPLOAD_ID),
        ),
        json_response(
            "422 Unprocessable Entity",
            json!({"error": "bundle_index_unreadable"}),
        ),
    ]);
    let home = tempfile::tempdir().unwrap();
    let output = publish_large_bundle(home.path(), &api.base);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("hub-invalid-bundle"), "{message}");
    assert!(message.contains("bundle_index_unreadable"), "{message}");
    assert!(!stdout(&output).contains("@sha256:"), "{}", stdout(&output));
    // A bundle defect is not retried: the bytes will not become readable.
    let requests = api.wait_requests(2);
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(upload.request_count(), 1);
}

#[test]
fn an_unavailable_presign_falls_back_to_the_base64_lane() {
    let api = StubApi::new(vec![json_response(
        "503 Service Unavailable",
        json!({"error": "presign_unavailable"}),
    )]);
    let home = tempfile::tempdir().unwrap();
    let output = publish_large_bundle(home.path(), &api.base);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(
        message.contains("cannot mint a presigned upload URL"),
        "{message}"
    );
    assert!(message.contains("Base64 JSON lane"), "{message}");
    assert!(message.contains("hub-bundle-too-large"), "{message}");
    let requests = api.wait_requests(1);
    assert_eq!(requests.len(), 1, "{requests:?}");
}

#[test]
fn a_large_hub_capsule_is_streamed_and_verified_into_the_cache() {
    let fixture = large_bundle();
    let objects = StubApi::new(vec![binary_response(&fixture.bytes)]);
    let api = StubApi::new(vec![
        json_response("200 OK", large_resolve_body()),
        json_response(
            "200 OK",
            large_download_url_body(&format!("{}/objects/one", objects.base)),
        ),
        json_response("200 OK", json!({"counted": true, "run_count": 1})),
    ]);
    let home = tempfile::tempdir().unwrap();
    let output = run_large_capsule(home.path(), &api.base);
    assert!(output.status.success(), "{}", stderr(&output));

    let cached = cached_large_path(home.path());
    assert_eq!(fs::read(&cached).unwrap(), fixture.bytes);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&cached).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode is {:o}", mode & 0o777);
    }

    let requests = api.wait_requests(3);
    assert_eq!(requests.len(), 3, "{requests:?}");
    assert!(
        requests[0].starts_with("GET /v1/hub/capsules/koh0920/large/resolve?tag=latest "),
        "{}",
        requests[0]
    );
    assert!(
        requests[1].starts_with(&format!(
            "GET /v1/hub/capsules/koh0920/large/artifacts/{}/download-url ",
            fixture.digest
        )),
        "{}",
        requests[1]
    );
    assert!(
        requests[2].starts_with("POST /v1/hub/capsules/koh0920/large/runs "),
        "{}",
        requests[2]
    );
    // The presigned GET is anonymous: the API credential stays with the API.
    let downloads = objects.wait_requests(1);
    assert_eq!(downloads.len(), 1, "{downloads:?}");
    assert!(
        downloads[0].starts_with("GET /objects/one "),
        "{}",
        downloads[0]
    );
    assert!(
        !downloads[0].to_ascii_lowercase().contains("authorization:"),
        "{}",
        downloads[0]
    );
}

#[test]
fn the_verified_large_cache_is_reused_without_another_download() {
    let fixture = large_bundle();
    let objects = StubApi::new(vec![binary_response(&fixture.bytes)]);
    let first = StubApi::new(vec![
        json_response("200 OK", large_resolve_body()),
        json_response(
            "200 OK",
            large_download_url_body(&format!("{}/objects/one", objects.base)),
        ),
        json_response("200 OK", json!({"counted": true, "run_count": 1})),
    ]);
    let home = tempfile::tempdir().unwrap();
    let output = run_large_capsule(home.path(), &first.base);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(objects.request_count(), 1);

    // The second Run re-hashes the stored file instead of trusting its name; a
    // dead download URL proves no second transfer happens.
    let second = StubApi::new(vec![
        json_response("200 OK", large_resolve_body()),
        json_response("200 OK", json!({"counted": true, "run_count": 2})),
    ]);
    let output = run_large_capsule(home.path(), &second.base);
    assert!(output.status.success(), "{}", stderr(&output));
    let requests = second.wait_requests(2);
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(
        requests[1].starts_with("POST /v1/hub/capsules/koh0920/large/runs "),
        "{requests:?}"
    );
    assert_eq!(objects.request_count(), 1, "no second download");
    assert_eq!(
        fs::read(cached_large_path(home.path())).unwrap(),
        fixture.bytes
    );
}

#[test]
fn a_corrupt_large_cache_entry_is_downloaded_again_and_verified() {
    let fixture = large_bundle();
    let objects = StubApi::new(vec![
        binary_response(&fixture.bytes),
        binary_response(&fixture.bytes),
    ]);
    let home = tempfile::tempdir().unwrap();
    let first = StubApi::new(vec![
        json_response("200 OK", large_resolve_body()),
        json_response(
            "200 OK",
            large_download_url_body(&format!("{}/objects/one", objects.base)),
        ),
        json_response("200 OK", json!({"counted": true, "run_count": 1})),
    ]);
    let output = run_large_capsule(home.path(), &first.base);
    assert!(output.status.success(), "{}", stderr(&output));

    // Same length, different bytes: only a re-hash can notice.
    let cached = cached_large_path(home.path());
    let mut corrupt = fs::read(&cached).unwrap();
    let middle = corrupt.len() / 2;
    corrupt[middle] ^= 0xff;
    fs::write(&cached, &corrupt).unwrap();

    let second = StubApi::new(vec![
        json_response("200 OK", large_resolve_body()),
        json_response(
            "200 OK",
            large_download_url_body(&format!("{}/objects/two", objects.base)),
        ),
        json_response("200 OK", json!({"counted": true, "run_count": 2})),
    ]);
    let output = run_large_capsule(home.path(), &second.base);
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(fs::read(&cached).unwrap(), fixture.bytes);
    assert_eq!(objects.request_count(), 2, "the corrupt entry was replaced");
}

#[test]
fn a_tampered_large_download_is_refused_and_leaves_no_cache_entry() {
    let fixture = large_bundle();
    let mut tampered = fixture.bytes.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0xff;
    let objects = StubApi::new(vec![binary_response(&tampered)]);
    let api = StubApi::new(vec![
        json_response("200 OK", large_resolve_body()),
        json_response(
            "200 OK",
            large_download_url_body(&format!("{}/objects/one", objects.base)),
        ),
    ]);
    let home = tempfile::tempdir().unwrap();
    let output = run_large_capsule(home.path(), &api.base);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("hub-digest-mismatch"),
        "{}",
        stderr(&output)
    );
    assert!(
        !cached_large_path(home.path()).exists(),
        "a mismatch never takes the cache name"
    );
    let hub = home.path().join("hub");
    if hub.is_dir() {
        let leftovers: Vec<_> = fs::read_dir(&hub)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty(), "temporary files left: {leftovers:?}");
    }
}

#[test]
fn a_missing_download_url_falls_back_to_the_artifact_endpoint() {
    let fixture = large_bundle();
    let api = StubApi::new(vec![
        json_response("200 OK", large_resolve_body()),
        json_response("404 Not Found", json!({"error": "not_found"})),
        binary_response(&fixture.bytes),
        json_response("200 OK", json!({"counted": true, "run_count": 1})),
    ]);
    let home = tempfile::tempdir().unwrap();
    let output = run_large_capsule(home.path(), &api.base);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("using the artifact endpoint"),
        "{}",
        stderr(&output)
    );
    let requests = api.wait_requests(4);
    assert_eq!(requests.len(), 4, "{requests:?}");
    assert!(
        requests[2].starts_with(&format!(
            "GET /v1/hub/capsules/koh0920/large/artifacts/{} ",
            fixture.digest
        )),
        "{}",
        requests[2]
    );
    assert_eq!(
        fs::read(cached_large_path(home.path())).unwrap(),
        fixture.bytes
    );
}

#[test]
fn an_unavailable_download_presign_falls_back_to_the_artifact_endpoint() {
    let fixture = large_bundle();
    let api = StubApi::new(vec![
        json_response("200 OK", large_resolve_body()),
        json_response(
            "503 Service Unavailable",
            json!({"error": "presign_unavailable"}),
        ),
        binary_response(&fixture.bytes),
        json_response("200 OK", json!({"counted": true, "run_count": 1})),
    ]);
    let home = tempfile::tempdir().unwrap();
    let output = run_large_capsule(home.path(), &api.base);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("cannot mint a presigned download URL"),
        "{}",
        stderr(&output)
    );
    let requests = api.wait_requests(4);
    assert_eq!(requests.len(), 4, "{requests:?}");
    assert_eq!(
        fs::read(cached_large_path(home.path())).unwrap(),
        fixture.bytes
    );
}
