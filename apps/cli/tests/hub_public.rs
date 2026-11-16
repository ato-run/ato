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
use serde_json::{Value, json};

/// SHA-256 of the committed interop-static bundle bytes.
const SAMPLE_DIGEST: &str =
    "sha256:95b837f4e8ed3c3354a4560e56820030cdb2ba34ba2a7c65aa6d26e139bdd813";

fn repo_sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/interop-static.capsule")
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
