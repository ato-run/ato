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
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use assert_cmd::Command as AssertCommand;
use base64::Engine as _;
use serde_json::{Value, json};

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

fn read_request(mut stream: TcpStream) -> String {
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
    String::from_utf8_lossy(&buffer).into_owned()
}

struct StubApi {
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl StubApi {
    fn new(responses: Vec<Vec<u8>>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind a loopback port");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let requests = Arc::clone(&requests);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                for response in responses {
                    let deadline = std::time::Instant::now() + Duration::from_secs(30);
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
                    let request = read_request(stream.try_clone().unwrap());
                    requests.lock().unwrap().push(request);
                    let mut stream = stream;
                    let _ = stream.write_all(&response);
                    let _ = stream.flush();
                }
            })
        };
        Self {
            base,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    fn wait_requests(&self, count: usize) -> Vec<String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let requests = self.requests.lock().unwrap().clone();
            if requests.len() >= count || std::time::Instant::now() > deadline {
                return requests;
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
