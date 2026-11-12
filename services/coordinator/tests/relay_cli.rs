//! CLI-level: `ato-coordinator relay --once` polls an HTTP relay, decides and
//! replies, and the decision is recorded by the relay. A real socket and a real
//! SQLite file, no harness.
//!
//! The relay is a data-only path: every envelope must present the relay data
//! capability (--data-token), and the Coordinator admin bearer is not accepted
//! here. The relay these tests talk to is a loopback socket they start
//! themselves; no hosted service is contacted and the local SQLite file is read
//! back directly.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DATA_TOKEN: &str = "cli-data-token";
const ADMIN_TOKEN: &str = "cli-admin-token";

fn envelope(request_id: &str, epoch: i64, authorization: Option<&str>) -> String {
    let mut value = serde_json::json!({
        "schema": "ato.relay-envelope/1",
        "request_id": request_id,
        "origin_epoch": epoch,
        "method": "POST",
        "path": "/v1/data-resources",
        "body": serde_json::to_string(&serde_json::json!({
            "protocol": "todo.board@1",
            "document": {"tasks": {"t1": {"id": "t1"}}}
        }))
        .unwrap(),
    });
    if let Some(authorization) = authorization {
        value["authorization"] = serde_json::json!(authorization);
    }
    serde_json::to_string(&value).unwrap()
}

/// A loopback relay that serves the same envelope to every poll and records the
/// replies. One `--once` run opens two connections: the poll, then the reply.
fn spawn_relay(
    envelope: String,
    runs: usize,
) -> (
    u16,
    Arc<Mutex<Vec<serde_json::Value>>>,
    std::thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let replies = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&replies);
    let handle = std::thread::spawn(move || {
        for _ in 0..runs * 2 {
            let Some(socket) = accept_with_deadline(&listener) else {
                break;
            };
            socket.set_nonblocking(false).ok();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut writer = socket;
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line.trim().is_empty() {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            if content_length > 0 {
                reader.read_exact(&mut body).unwrap();
            }
            let body = String::from_utf8_lossy(&body).into_owned();
            if request_line.contains("/next") {
                let response_body = serde_json::json!({
                    "request_id": "relay-cli",
                    "origin_epoch": 7,
                    "envelope": envelope.as_str(),
                })
                .to_string();
                write!(
                    writer,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                )
                .unwrap();
            } else {
                let posted: serde_json::Value = serde_json::from_str(&body).unwrap();
                let reply: serde_json::Value =
                    serde_json::from_str(posted["reply"].as_str().unwrap()).unwrap();
                record.lock().unwrap().push(reply);
                write!(
                    writer,
                    "HTTP/1.1 200 OK\r\nContent-Length: 20\r\nConnection: close\r\n\r\n{{\"status\":\"recorded\"}}"
                )
                .unwrap();
            }
        }
    });
    (port, replies, handle)
}

fn accept_with_deadline(listener: &TcpListener) -> Option<TcpStream> {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Some(stream),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() > deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
}

fn init_database(directory: &tempfile::TempDir) -> PathBuf {
    let database = directory.path().join("coordinator.sqlite");
    let binary = env!("CARGO_BIN_EXE_ato-coordinator");
    assert!(
        Command::new(binary)
            .args(["init", "--db"])
            .arg(&database)
            .status()
            .unwrap()
            .success()
    );
    database
}

fn run_relay_once(database: &Path, port: u16, data_token: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ato-coordinator"))
        .arg("relay")
        .arg("--db")
        .arg(database)
        .args([
            "--relay-url",
            &format!("http://127.0.0.1:{port}"),
            "--origin-id",
            "relay-cli",
            "--origin-epoch",
            "7",
            "--relay-token",
            "origin-token",
            "--data-token",
            data_token,
            "--once",
        ])
        .env_remove("ATO_RELAY_DATA_TOKEN")
        .output()
        .unwrap()
}

fn stored_resources(database: &Path) -> Vec<ato_coordinator::ResourceSummary> {
    ato_coordinator::Coordinator::open(database)
        .unwrap()
        .list_resources("local")
        .unwrap()
}

fn reply_body(reply: &serde_json::Value) -> serde_json::Value {
    serde_json::from_str(reply["body"].as_str().unwrap()).unwrap()
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn wait_for_health(port: u16) {
    for _ in 0..200 {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            write!(
                stream,
                "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let mut response = String::new();
            let _ = stream.read_to_string(&mut response);
            if response.contains("\"status\":\"ok\"") {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("coordinator did not become healthy on port {port}");
}

#[test]
fn the_data_capability_commits_into_the_local_sqlite() {
    let directory = tempfile::tempdir().unwrap();
    let database = init_database(&directory);
    let (port, replies, server) = spawn_relay(
        envelope("cli-data-1", 7, Some(&format!("Bearer {DATA_TOKEN}"))),
        1,
    );
    let output = run_relay_once(&database, port, DATA_TOKEN);
    server.join().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Answered"), "{stdout}");

    let replies = replies.lock().unwrap();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["status"], 201, "{:?}", replies[0]);
    let receipt = reply_body(&replies[0]);
    assert_eq!(receipt["revision"], 1, "{receipt}");
    assert!(
        receipt["content_digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"),
        "{receipt}"
    );
    let resource_id = receipt["resource_id"].as_str().unwrap().to_owned();

    // The local SQLite file is the only copy. Read it back directly: the only
    // HTTP this process talked to was the loopback relay started above.
    let stored = ato_coordinator::Coordinator::open(&database)
        .unwrap()
        .read_resource("local", &resource_id)
        .unwrap();
    assert_eq!(stored.revision, 1);
    assert_eq!(stored.document["tasks"]["t1"]["id"], "t1");
}

#[test]
fn a_wrong_or_missing_capability_is_refused_and_writes_nothing() {
    for (case, authorization) in [
        ("wrong", Some(format!("Bearer not-{DATA_TOKEN}"))),
        ("missing", None),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let database = init_database(&directory);
        let (port, replies, server) =
            spawn_relay(envelope("cli-denied-1", 7, authorization.as_deref()), 1);
        let output = run_relay_once(&database, port, DATA_TOKEN);
        server.join().unwrap();
        assert!(
            output.status.success(),
            "{case}: stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let replies = replies.lock().unwrap();
        assert_eq!(replies.len(), 1, "{case}");
        assert_eq!(replies[0]["status"], 401, "{case}: {:?}", replies[0]);
        assert_eq!(
            reply_body(&replies[0])["error"],
            "relay_data_capability_invalid",
            "{case}"
        );
        assert!(stored_resources(&database).is_empty(), "{case}");
    }
}

#[test]
fn the_admin_api_token_is_not_a_relay_capability() {
    let directory = tempfile::tempdir().unwrap();
    let database = init_database(&directory);
    let port = free_port();

    // The admin bearer is valid on the local API surface.
    let mut server = Command::new(env!("CARGO_BIN_EXE_ato-coordinator"))
        .arg("serve")
        .arg("--db")
        .arg(&database)
        .args([
            "--addr",
            &format!("127.0.0.1:{port}"),
            "--token",
            ADMIN_TOKEN,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for_health(port);
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "GET /v1/data-resources HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {ADMIN_TOKEN}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let _ = server.kill();
    let _ = server.wait();

    // The same bearer is not the relay data capability.
    let (relay_port, replies, relay) = spawn_relay(
        envelope("cli-admin-1", 7, Some(&format!("Bearer {ADMIN_TOKEN}"))),
        1,
    );
    let output = run_relay_once(&database, relay_port, DATA_TOKEN);
    relay.join().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let replies = replies.lock().unwrap();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["status"], 401, "{:?}", replies[0]);
    assert_eq!(
        reply_body(&replies[0])["error"],
        "relay_data_capability_invalid"
    );
    assert!(stored_resources(&database).is_empty());
}

#[test]
fn an_envelope_for_another_authority_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let database = init_database(&directory);
    let (port, replies, server) = spawn_relay(
        envelope("cli-stale-1", 6, Some(&format!("Bearer {DATA_TOKEN}"))),
        1,
    );
    let output = run_relay_once(&database, port, DATA_TOKEN);
    server.join().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let replies = replies.lock().unwrap();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0]["status"], 409, "{:?}", replies[0]);
    assert_eq!(reply_body(&replies[0])["error"], "origin_epoch_mismatch");
    assert!(stored_resources(&database).is_empty());
}

#[test]
fn a_replayed_request_id_is_not_applied_twice() {
    let directory = tempfile::tempdir().unwrap();
    let database = init_database(&directory);
    let (port, replies, server) = spawn_relay(
        envelope("cli-replay-1", 7, Some(&format!("Bearer {DATA_TOKEN}"))),
        2,
    );
    let first = run_relay_once(&database, port, DATA_TOKEN);
    let second = run_relay_once(&database, port, DATA_TOKEN);
    server.join().unwrap();
    for output in [first, second] {
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let replies = replies.lock().unwrap();
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0]["status"], 201, "{:?}", replies[0]);
    assert_eq!(
        replies[1], replies[0],
        "a replay answers the original receipt"
    );
    assert_eq!(stored_resources(&database).len(), 1);
}

#[test]
fn a_relay_without_a_data_capability_refuses_to_start() {
    let directory = tempfile::tempdir().unwrap();
    let database = init_database(&directory);
    let binary = env!("CARGO_BIN_EXE_ato-coordinator");
    let base = [
        "--relay-url",
        "http://127.0.0.1:9",
        "--origin-id",
        "relay-cli",
        "--origin-epoch",
        "7",
        "--relay-token",
        "origin-token",
        "--once",
    ];

    let missing = Command::new(binary)
        .arg("relay")
        .arg("--db")
        .arg(&database)
        .args(base)
        .env_remove("ATO_RELAY_DATA_TOKEN")
        .output()
        .unwrap();
    assert!(
        !missing.status.success(),
        "a relay without a capability must not start"
    );

    let mut empty_args = base.to_vec();
    empty_args.extend(["--data-token", ""]);
    let empty = Command::new(binary)
        .arg("relay")
        .arg("--db")
        .arg(&database)
        .args(empty_args)
        .env_remove("ATO_RELAY_DATA_TOKEN")
        .output()
        .unwrap();
    assert!(!empty.status.success());
    assert!(
        String::from_utf8_lossy(&empty.stderr).contains("data-token"),
        "stderr: {}",
        String::from_utf8_lossy(&empty.stderr)
    );
}

#[test]
fn a_one_shot_run_fails_when_the_relay_is_unreachable() {
    // Bind then drop: the port is closed, so the poll cannot connect.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let directory = tempfile::tempdir().unwrap();
    let database = init_database(&directory);
    let output = run_relay_once(&database, port, DATA_TOKEN);
    assert!(!output.status.success(), "a failed poll must not exit 0");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("relay_unavailable"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn serve_refuses_a_non_loopback_plaintext_bind() {
    let directory = tempfile::tempdir().unwrap();
    let database = init_database(&directory);
    let binary = env!("CARGO_BIN_EXE_ato-coordinator");
    let output = Command::new(binary)
        .arg("serve")
        .arg("--db")
        .arg(&database)
        .args(["--addr", "0.0.0.0:0", "--token", "cli-token"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("non-loopback"), "stderr: {stderr}");
}
