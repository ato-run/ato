//! CLI-level: `ato-coordinator relay --once` polls an HTTP relay, decides and
//! replies, and the decision is recorded by the relay. A real socket and a real
//! SQLite file, no harness.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::{Arc, Mutex};

fn envelope() -> String {
    serde_json::to_string(&serde_json::json!({
        "schema": "ato.relay-envelope/1",
        "request_id": "cli-relay-1",
        "origin_epoch": 7,
        "method": "POST",
        "path": "/v1/data-resources",
        "body": serde_json::to_string(&serde_json::json!({
            "protocol": "todo.board@1",
            "document": {"tasks": {"t1": {"id": "t1"}}}
        }))
        .unwrap(),
        "authorization": "Bearer cli-token"
    }))
    .unwrap()
}

#[test]
fn the_relay_command_answers_and_posts_the_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let replies: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&replies);

    let server = std::thread::spawn(move || {
        for request_index in 0..2 {
            let (socket, _) = listener.accept().unwrap();
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
                    "request_id": "cli-relay-1",
                    "origin_epoch": 7,
                    "envelope": envelope(),
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
            let _ = request_index;
        }
    });

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("coordinator.sqlite");
    let binary = env!("CARGO_BIN_EXE_ato-coordinator");
    let init = Command::new(binary)
        .args(["init", "--db"])
        .arg(&database)
        .status()
        .unwrap();
    assert!(init.success());

    let output = Command::new(binary)
        .arg("relay")
        .arg("--db")
        .arg(&database)
        .args([
            "--relay-url",
            &format!("http://127.0.0.1:{port}"),
            "--origin-id",
            "relay-cli",
            "--origin-epoch",
            "7",
            "--token",
            "cli-token",
            "--relay-token",
            "origin-token",
            "--once",
        ])
        .output()
        .unwrap();
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
    assert!(
        replies[0]["body"].as_str().unwrap().contains("resource_id"),
        "{:?}",
        replies[0]
    );
}

#[test]
fn a_one_shot_run_fails_when_the_relay_is_unreachable() {
    // Bind then drop: the port is closed, so the poll cannot connect.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    let directory = tempfile::tempdir().unwrap();
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
    let output = Command::new(binary)
        .arg("relay")
        .arg("--db")
        .arg(&database)
        .args([
            "--relay-url",
            &format!("http://127.0.0.1:{port}"),
            "--origin-id",
            "relay-cli",
            "--origin-epoch",
            "7",
            "--token",
            "cli-token",
            "--relay-token",
            "origin-token",
            "--once",
        ])
        .output()
        .unwrap();
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
