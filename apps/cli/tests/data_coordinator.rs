//! `ato data` against a stub Coordinator: the CLI must print what the
//! authority returned and surface its error codes, never invent a revision.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::thread;

fn ato() -> Command {
    Command::new(assert_cmd::cargo::cargo_bin!("ato"))
}

fn stub(responses: Vec<(u16, &'static str)>) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
    let port = listener.local_addr().expect("stub addr").port();
    let handle = thread::spawn(move || {
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buffer = [0u8; 8192];
            let _ = stream.read(&mut buffer);
            let response = format!(
                "HTTP/1.1 {status} OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        }
    });
    (port, handle)
}

#[test]
fn health_prints_the_authority_status() {
    let (port, handle) = stub(vec![(
        200,
        r#"{"status":"ok","schema_version":1,"resources":0}"#,
    )]);
    let output = ato()
        .args([
            "data",
            "health",
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
            "--json",
        ])
        .output()
        .expect("run ato data health");
    handle.join().expect("stub");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"status\": \"ok\""), "{stdout}");
}

#[test]
fn create_prints_the_resource_the_authority_created() {
    let (port, handle) = stub(vec![(
        201,
        r#"{"resource_id":"dres_1","protocol":"todo.board@1","revision":1,"generation":0,"content_digest":"sha256:abc","byte_size":2,"document":{}}"#,
    )]);
    let output = ato()
        .args([
            "data",
            "resource",
            "create",
            "--protocol",
            "todo.board@1",
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data resource create");
    handle.join().expect("stub");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("created dres_1 (revision 1)"), "{stdout}");
}

#[test]
fn invoke_prints_the_committed_revision() {
    let (port, handle) = stub(vec![(
        200,
        r#"{"resource_id":"dres_1","revision":2,"generation":0,"content_digest":"sha256:def","replayed":false,"receipt":{"revision":2}}"#,
    )]);
    let output = ato()
        .args([
            "data",
            "operation",
            "invoke",
            "dres_1",
            "--operation-id",
            "op-1",
            "--expected-revision",
            "1",
            "--document",
            r#"{"tasks":{}}"#,
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data operation invoke");
    handle.join().expect("stub");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("committed revision 2"), "{stdout}");
}

#[test]
fn surfaces_the_authority_error_code() {
    let (port, handle) = stub(vec![(409, r#"{"error":"operation_id_conflict"}"#)]);
    let output = ato()
        .args([
            "data",
            "operation",
            "invoke",
            "dres_1",
            "--operation-id",
            "op-1",
            "--expected-revision",
            "2",
            "--coordinator",
            &format!("http://127.0.0.1:{port}"),
            "--token",
            "test-token",
        ])
        .output()
        .expect("run ato data operation invoke");
    handle.join().expect("stub");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("operation_id_conflict"), "{stderr}");
}
