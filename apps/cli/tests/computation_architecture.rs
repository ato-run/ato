#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier, Mutex};

use assert_cmd::prelude::*;
use ato_computation::{ComputationRef, PortId, ProtocolId};
use ato_objects::{Direction, LocalCapsuleRepository, ObjectStore, RecordEnvelope, RecordId};
use base64::Engine;
use predicates::prelude::*;

static NETWORK_TEST_LOCK: Mutex<()> = Mutex::new(());

fn ato(ato_home: &Path) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("ato"));
    command.env("ATO_HOME", ato_home);
    command
}

fn write_project(root: &Path, command: &str, extra: &str) {
    fs::write(
        root.join("capsule.toml"),
        format!(
            r#"schema = 1

[[process]]
id = "app"
command = {command}
cwd = "."

[[adapter]]
target = "app"
use = "ato.process@1"

[[adapter]]
target = "workspace"
use = "ato.workspace@1"

{extra}

[encap]
materializers = ["ato.replay@1"]
"#,
        ),
    )
    .unwrap();
}

fn write_memory_counter(root: &Path, upstream_port: u16, public_port: u16, requests: usize) {
    fs::write(
        root.join("counter.rs"),
        format!(
            r#"use std::io::{{Read, Write}};
use std::net::TcpListener;

fn main() {{
    let address = std::env::args().nth(1).unwrap();
    let listener = TcpListener::bind(address).unwrap();
    let mut count = 0_u64;
    let mut handled = 0;
    while handled < {requests} {{
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let size = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..size]);
        if request.starts_with("GET /ready ") {{
            stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            continue;
        }}
        handled += 1;
        if request.starts_with("POST /increment ") {{
            count += 1;
            stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        }} else {{
            let count = count.to_string();
            let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {{}}\r\nConnection: close\r\n\r\n{{}}", count.len(), count);
            stream.write_all(response.as_bytes()).unwrap();
        }}
    }}
}}
"#,
        ),
    )
    .unwrap();
    assert!(
        Command::new("rustc")
            .args(["counter.rs", "-o", "counter-bin"])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    write_project(
        root,
        &format!(
            r#"["sh", "-c", "chmod +x counter-bin && exec ./counter-bin 127.0.0.1:{upstream_port}"]"#
        ),
        &format!(
            r#"[[port]]
id = "app.http"
node = "app"
protocol = "ato.http@1"
role = "server"

[[adapter]]
port = "app.http"
use = "ato.http@1"
listen = "127.0.0.1:{public_port}"
upstream = "127.0.0.1:{upstream_port}"
ready_path = "/ready""#
        ),
    );
}

fn write_quiesce_counter(root: &Path, upstream_port: u16, public_port: u16) {
    fs::write(
        root.join("counter.rs"),
        r#"use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

fn main() {
    let address = std::env::args().nth(1).unwrap();
    let listener = TcpListener::bind(address).unwrap();
    for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        let mut request = [0_u8; 4096];
        let size = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..size]);
        if request.starts_with("GET /ready ") {
            stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            continue;
        }
        fs::write(".capsule/runs/request.started", b"started").unwrap();
        while !std::path::Path::new(".capsule/runs/request.release").exists() {
            thread::sleep(Duration::from_millis(5));
        }
        stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        break;
    }
}
"#,
    )
    .unwrap();
    assert!(
        Command::new("rustc")
            .args(["counter.rs", "-o", "counter-bin"])
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    write_project(
        root,
        &format!(
            r#"["sh", "-c", "chmod +x counter-bin && exec ./counter-bin 127.0.0.1:{upstream_port}"]"#
        ),
        &format!(
            r#"[[port]]
id = "app.http"
node = "app"
protocol = "ato.http@1"
role = "server"

[[adapter]]
port = "app.http"
use = "ato.http@1"
listen = "127.0.0.1:{public_port}"
upstream = "127.0.0.1:{upstream_port}"
ready_path = "/ready""#
        ),
    );
}

fn wait_until(mut predicate: impl FnMut() -> bool) {
    for _ in 0..200 {
        if predicate() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("condition did not become true");
}

#[test]
fn compiler_error_is_a_valid_portable_handoff_point() {
    let project = tempfile::tempdir().unwrap();
    let author_home = tempfile::tempdir().unwrap();
    let recipient_home = tempfile::tempdir().unwrap();
    write_project(
        project.path(),
        r#"["sh"]"#,
        r#"[[adapter]]
target = "app"
use = "ato.pty@1"
input = "rustc broken.rs 2>&1 || true\n""#,
    );
    fs::write(project.path().join("broken.rs"), "fn main() { missing( }\n").unwrap();

    ato(author_home.path())
        .args(["init", project.path().to_str().unwrap()])
        .assert()
        .success();
    let log = project.path().join(".capsule/runs/output.log");
    wait_until(|| fs::read_to_string(&log).is_ok_and(|text| text.contains("error")));
    ato(author_home.path())
        .args(["stop", project.path().to_str().unwrap()])
        .assert()
        .success();
    assert!(
        record_events(project.path(), "ato.pty@1")
            .iter()
            .any(|event| {
                event["kind"] == "input"
                    && event["bytes"]
                        .as_array()
                        .is_some_and(|bytes| !bytes.is_empty())
            })
    );
    let bundle = project.path().join("error.capsule");
    ato(author_home.path())
        .args([
            "encap",
            &format!("{}@main", project.path().display()),
            "-o",
            bundle.to_str().unwrap(),
        ])
        .assert()
        .success();
    let mut recipient = ato(recipient_home.path())
        .args(["run", bundle.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    recipient
        .stdin
        .take()
        .unwrap()
        .write_all(b"printf continued-from-error\\n\nexit\n")
        .unwrap();
    let output = recipient.wait_with_output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("error"));
    assert!(stdout.contains("continued-from-error"));
}

#[test]
fn stateful_protocol_state_survives_nonzero_process_and_replay() {
    let _network = NETWORK_TEST_LOCK.lock().unwrap();
    let project = tempfile::tempdir().unwrap();
    let author_home = tempfile::tempdir().unwrap();
    let recipient_home = tempfile::tempdir().unwrap();
    let upstream_port = unused_port();
    let public_port = unused_port();
    fs::write(
        project.path().join("counter.rs"),
        r#"use std::io::{Read, Write};
use std::net::TcpListener;

fn main() {
    let address = std::env::args().nth(1).unwrap();
    let listener = TcpListener::bind(address).unwrap();
    let mut count = 0_u64;
    let mut handled = 0;
    while handled < 5 {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let size = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..size]);
        if request.starts_with("GET /ready ") {
            stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            continue;
        }
        handled += 1;
        if request.starts_with("POST /increment ") {
            count += 1;
            stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        } else {
            let count = count.to_string();
            let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", count.len(), count);
            stream.write_all(response.as_bytes()).unwrap();
        }
    }
}

"#,
    )
    .unwrap();
    assert!(
        Command::new("rustc")
            .args(["counter.rs", "-o", "counter-bin"])
            .current_dir(project.path())
            .status()
            .unwrap()
            .success()
    );
    write_project(
        project.path(),
        &format!(
            r#"["sh", "-c", "chmod +x counter-bin && exec ./counter-bin 127.0.0.1:{upstream_port}"]"#
        ),
        &format!(
            r#"[[port]]
id = "app.http"
node = "app"
protocol = "ato.http@1"
role = "server"

[[adapter]]
port = "app.http"
use = "ato.http@1"
listen = "127.0.0.1:{public_port}"
upstream = "127.0.0.1:{upstream_port}"
ready_path = "/ready""#
        ),
    );
    ato(author_home.path())
        .args(["init", project.path().to_str().unwrap()])
        .assert()
        .success();
    let initial_root = fs::read_to_string(project.path().join(".capsule/refs/heads/main")).unwrap();
    let increment = http_request(public_port, "POST", "/increment");
    assert!(increment.starts_with("HTTP/1.1 204"));
    let count = http_request(public_port, "GET", "/count");
    assert!(count.ends_with("1"));
    wait_until(|| {
        fs::read_dir(project.path().join(".capsule/records/main"))
            .is_ok_and(|entries| entries.count() >= 4)
    });
    ato(author_home.path())
        .args(["stop", project.path().to_str().unwrap()])
        .assert()
        .success();
    let final_root = fs::read_to_string(project.path().join(".capsule/refs/heads/main")).unwrap();
    assert_ne!(
        initial_root, final_root,
        "a state-changing memory-only interaction must advance Capsule identity"
    );
    ato(author_home.path())
        .args(["resume", &format!("{}@main", project.path().display())])
        .assert()
        .success();
    assert!(
        http_request(public_port, "GET", "/count").ends_with("1"),
        "resume must realize the selected semantic state before publishing ACTIVE"
    );
    ato(author_home.path())
        .args(["stop", project.path().to_str().unwrap()])
        .assert()
        .success();
    let records = fs::read_dir(project.path().join(".capsule/records/main"))
        .unwrap()
        .count();
    assert!(records >= 1);
    let bundle = project.path().join("counter.capsule");
    ato(author_home.path())
        .args([
            "encap",
            &format!("{}@main", project.path().display()),
            "-o",
            bundle.to_str().unwrap(),
        ])
        .assert()
        .success();
    let mut portable = ato(recipient_home.path())
        .args(["run", bundle.to_str().unwrap()])
        .spawn()
        .unwrap();
    let count = http_request(public_port, "GET", "/count");
    assert!(count.ends_with("1"));
    let count_again = http_request(public_port, "GET", "/count");
    assert!(count_again.ends_with("1"));
    assert!(portable.wait().unwrap().success());
}

#[test]
fn forked_branch_replays_its_parent_semantic_closure() {
    let _network = NETWORK_TEST_LOCK.lock().unwrap();
    let project = tempfile::tempdir().unwrap();
    let author_home = tempfile::tempdir().unwrap();
    let recipient_home = tempfile::tempdir().unwrap();
    let upstream_port = unused_port();
    let public_port = unused_port();
    write_memory_counter(project.path(), upstream_port, public_port, 5);

    ato(author_home.path())
        .args(["init", project.path().to_str().unwrap()])
        .assert()
        .success();
    assert!(http_request(public_port, "POST", "/increment").starts_with("HTTP/1.1 204"));
    assert!(http_request(public_port, "GET", "/count").ends_with("1"));
    wait_until(|| {
        fs::read_dir(project.path().join(".capsule/records/main"))
            .is_ok_and(|entries| entries.count() >= 4)
    });
    ato(author_home.path())
        .args(["stop", project.path().to_str().unwrap()])
        .assert()
        .success();
    let fork_record = fs::read_dir(project.path().join(".capsule/records/main"))
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.path().file_stem()?.to_str()?.parse::<u64>().ok())
        .max()
        .unwrap();

    ato(author_home.path())
        .args([
            "resume",
            &format!("{}@main#{fork_record}", project.path().display()),
            "--branch",
            "experiment",
        ])
        .assert()
        .success();
    assert!(http_request(public_port, "POST", "/increment").starts_with("HTTP/1.1 204"));
    assert!(http_request(public_port, "GET", "/count").ends_with("2"));
    ato(author_home.path())
        .args(["stop", project.path().to_str().unwrap()])
        .assert()
        .success();

    let bundle = project.path().join("experiment.capsule");
    ato(author_home.path())
        .args([
            "encap",
            &format!("{}@experiment", project.path().display()),
            "-o",
            bundle.to_str().unwrap(),
        ])
        .assert()
        .success();
    let mut portable = ato(recipient_home.path())
        .args(["run", bundle.to_str().unwrap()])
        .spawn()
        .unwrap();
    assert!(http_request(public_port, "GET", "/count").ends_with("2"));
    assert!(portable.wait().unwrap().success());
}

#[test]
fn resume_realizes_the_selected_memory_only_semantic_state() {
    let _network = NETWORK_TEST_LOCK.lock().unwrap();
    let project = tempfile::tempdir().unwrap();
    let ato_home = tempfile::tempdir().unwrap();
    let upstream_port = unused_port();
    let public_port = unused_port();
    write_memory_counter(project.path(), upstream_port, public_port, 2);
    ato(ato_home.path())
        .args(["init", project.path().to_str().unwrap()])
        .assert()
        .success();
    assert!(http_request(public_port, "POST", "/increment").starts_with("HTTP/1.1 204"));
    ato(ato_home.path())
        .args(["stop", project.path().to_str().unwrap()])
        .assert()
        .success();

    ato(ato_home.path())
        .args(["resume", &format!("{}@main", project.path().display())])
        .assert()
        .success();
    assert!(http_request(public_port, "GET", "/count").ends_with("1"));
    ato(ato_home.path())
        .args(["stop", project.path().to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn historical_resume_creates_a_sibling_future_without_rewrite() {
    let project = tempfile::tempdir().unwrap();
    let ato_home = tempfile::tempdir().unwrap();
    fs::write(project.path().join("state.txt"), "0").unwrap();
    write_project(project.path(), r#"["sh", "-c", "true"]"#, "");

    ato(ato_home.path())
        .args(["init", project.path().to_str().unwrap()])
        .assert()
        .success();
    fs::write(project.path().join("state.txt"), "A").unwrap();
    ato(ato_home.path())
        .args(["stop", project.path().to_str().unwrap()])
        .assert()
        .success();
    ato(ato_home.path())
        .args(["resume", &format!("{}@main", project.path().display())])
        .assert()
        .success();
    fs::write(project.path().join("state.txt"), "AB").unwrap();
    ato(ato_home.path())
        .args(["stop", project.path().to_str().unwrap()])
        .assert()
        .success();
    let main = fs::read_to_string(project.path().join(".capsule/refs/heads/main")).unwrap();

    ato(ato_home.path())
        .args(["resume", &format!("{}@main#1", project.path().display())])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--branch"));
    ato(ato_home.path())
        .args([
            "resume",
            &format!("{}@main#1", project.path().display()),
            "--branch",
            "experiment",
        ])
        .assert()
        .success();
    assert_eq!(
        fs::read_to_string(project.path().join("state.txt")).unwrap(),
        "A"
    );
    fs::write(project.path().join("state.txt"), "AC").unwrap();
    ato(ato_home.path())
        .args(["stop", project.path().to_str().unwrap()])
        .assert()
        .success();
    let experiment =
        fs::read_to_string(project.path().join(".capsule/refs/heads/experiment")).unwrap();
    assert_eq!(
        fs::read_to_string(project.path().join(".capsule/refs/heads/main")).unwrap(),
        main
    );
    assert_ne!(experiment, main);

    let bundle = project.path().join("experiment.capsule");
    ato(ato_home.path())
        .args([
            "encap",
            &format!("{}@experiment", project.path().display()),
            "-o",
            bundle.to_str().unwrap(),
        ])
        .assert()
        .success();
    assert!(!fs::read_to_string(bundle).unwrap().contains("QUI="));
}

#[test]
fn recipient_rebinds_secret_without_secret_transport() {
    let project = tempfile::tempdir().unwrap();
    let author_home = tempfile::tempdir().unwrap();
    let recipient_home = tempfile::tempdir().unwrap();
    write_project(
        project.path(),
        r#"["sh", "-c", "test -n \"$API_TOKEN\""]"#,
        r#"[[binding]]
id = "service.api_token"
environment = "API_TOKEN"
protocol = "ato.binding@1""#,
    );
    fs::write(
        project.path().join(".env"),
        "OPENAI_API_KEY=canary-workspace-secret\n",
    )
    .unwrap();
    ato(author_home.path())
        .args([
            "init",
            project.path().to_str().unwrap(),
            "--bind",
            "service.api_token=alice-secret",
        ])
        .assert()
        .success();
    ato(author_home.path())
        .args(["stop", project.path().to_str().unwrap()])
        .assert()
        .success();
    let binding_record = walk(project.path().join(".capsule/records/main"))
        .into_iter()
        .filter_map(|path| fs::read(path).ok())
        .filter_map(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .find(|record| record["adapter_id"] == "ato.binding@1")
        .expect("Binding Attach must be protocol evidence");
    let payload = binding_record["payload_ref"].as_str().unwrap();
    let event: serde_json::Value = serde_json::from_slice(
        &fs::read(
            project
                .path()
                .join(".capsule/objects/blake3")
                .join(payload.split_once(':').unwrap().1),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(event["kind"], "attach");
    assert_eq!(event["binding_id"], "service.api_token");
    let bundle = project.path().join("secret.capsule");
    ato(author_home.path())
        .args([
            "encap",
            &format!("{}@main", project.path().display()),
            "-o",
            bundle.to_str().unwrap(),
        ])
        .assert()
        .success();
    for entry in walk(project.path().join(".capsule")) {
        if let Ok(bytes) = fs::read(entry) {
            assert!(
                !bytes
                    .windows(b"alice-secret".len())
                    .any(|value| value == b"alice-secret")
            );
            assert!(
                !bytes
                    .windows(b"canary-workspace-secret".len())
                    .any(|value| value == b"canary-workspace-secret"),
                "workspace secret must not enter the local portable object closure"
            );
        }
    }
    assert!(
        !fs::read(&bundle)
            .unwrap()
            .windows(b"alice-secret".len())
            .any(|value| value == b"alice-secret")
    );
    assert!(
        !fs::read(&bundle)
            .unwrap()
            .windows(b"canary-workspace-secret".len())
            .any(|value| value == b"canary-workspace-secret"),
        "secure workspace capture must exclude project-local secrets"
    );
    let bundle_json: serde_json::Value =
        serde_json::from_slice(&fs::read(&bundle).unwrap()).unwrap();
    for payload in bundle_json["payloads"].as_array().unwrap() {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload["bytes"].as_str().unwrap())
            .unwrap();
        assert!(
            !bytes
                .windows(b"canary-workspace-secret".len())
                .any(|value| value == b"canary-workspace-secret"),
            "portable bundle payload must not contain the workspace secret"
        );
    }
    ato(recipient_home.path())
        .args([
            "run",
            bundle.to_str().unwrap(),
            "--bind",
            "service.api_token=bob-secret",
        ])
        .assert()
        .success();
}

#[test]
fn same_capsule_identity_supports_multiple_encap_materializations() {
    let project = tempfile::tempdir().unwrap();
    let ato_home = tempfile::tempdir().unwrap();
    write_project(project.path(), r#"["sh", "-c", "true"]"#, "");
    ato(ato_home.path())
        .args(["init", project.path().to_str().unwrap(), "--initial-only"])
        .assert()
        .success();
    let replay = project.path().join("replay.capsule");
    let multi = project.path().join("multi.capsule");
    for (output, extra) in [
        (&replay, Vec::<&str>::new()),
        (&multi, vec!["--materialize", "ato.snapshot@1"]),
    ] {
        let mut args = vec![
            "encap",
            project.path().to_str().unwrap(),
            "--materialize",
            "ato.replay@1",
        ];
        args.extend(extra);
        args.extend(["-o", output.to_str().unwrap()]);
        ato(ato_home.path()).args(args).assert().success();
    }
    let replay_json: serde_json::Value =
        serde_json::from_slice(&fs::read(&replay).unwrap()).unwrap();
    let multi_json: serde_json::Value = serde_json::from_slice(&fs::read(&multi).unwrap()).unwrap();
    assert_eq!(replay_json["index"]["root"], multi_json["index"]["root"]);
    assert_ne!(fs::read(&replay).unwrap(), fs::read(&multi).unwrap());
    assert_eq!(
        replay_json["index"]["materializations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        multi_json["index"]["materializations"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        multi_json["index"]["objects"].as_array().unwrap().len()
            >= replay_json["index"]["objects"].as_array().unwrap().len() + 2,
        "snapshot materialization must retain a descriptor and a physical artifact"
    );
    ato(ato_home.path())
        .args(["run", multi.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn one_root_computation_runs_an_explicit_multi_process_application() {
    let _network = NETWORK_TEST_LOCK.lock().unwrap();
    let project = tempfile::tempdir().unwrap();
    let ato_home = tempfile::tempdir().unwrap();
    let recipient_home = tempfile::tempdir().unwrap();
    let public_port = unused_port();
    let internal_port = unused_port();
    fs::write(
        project.path().join("composite.rs"),
        r#"use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

fn main() {
    let mode = std::env::args().nth(1).unwrap();
    let listen = if mode == "counter" {
        std::env::var("COUNTER_LISTEN").unwrap()
    } else {
        std::env::var("APP_LISTEN").unwrap()
    };
    if mode == "counter" {
        let listener = TcpListener::bind(listen).unwrap();
        let (mut client, _) = listener.accept().unwrap();
        let mut request = [0_u8; 64];
        let _ = client.read(&mut request).unwrap();
        client.write_all(b"1").unwrap();
        return;
    }
    let counter = std::env::var("COUNTER_ADDRESS").unwrap();
    let listener = TcpListener::bind(listen).unwrap();
    let (mut client, _) = listener.accept().unwrap();
    let mut request = [0_u8; 4096];
    let _ = client.read(&mut request).unwrap();
    let mut internal = loop {
        match TcpStream::connect(&counter) {
            Ok(stream) => break stream,
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    };
    internal.write_all(b"increment").unwrap();
    let mut count = String::new();
    internal.read_to_string(&mut count).unwrap();
    let body = format!("composed:{count}");
    let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    client.write_all(response.as_bytes()).unwrap();
}

"#,
    )
    .unwrap();
    assert!(
        Command::new("rustc")
            .args(["composite.rs", "-o", "composite-bin"])
            .current_dir(project.path())
            .status()
            .unwrap()
            .success()
    );
    let manifest = format!(
        r#"schema = 1

[[process]]
id = "api"
command = ["sh", "-c", "chmod +x composite-bin && exec ./composite-bin api"]
cwd = "."

[[process]]
id = "counter"
command = ["sh", "-c", "chmod +x composite-bin && exec ./composite-bin counter"]
cwd = "."

[[adapter]]
target = "api"
use = "ato.process@1"

[[adapter]]
target = "counter"
use = "ato.process@1"

[[port]]
id = "app.http"
node = "api"
protocol = "ato.http@1"
role = "server"
address = "127.0.0.1:{public_port}"
environment = "APP_LISTEN"

[[port]]
id = "api.counter"
node = "api"
protocol = "fixture.counter@1"
role = "client"
internal = true
environment = "COUNTER_ADDRESS"

[[port]]
id = "counter.api"
node = "counter"
protocol = "fixture.counter@1"
role = "server"
internal = true
address = "127.0.0.1:{internal_port}"
environment = "COUNTER_LISTEN"

[[connection]]
from = "api.counter"
to = "counter.api"

[encap]
materializers = ["ato.replay@1"]
"#
    );
    fs::write(project.path().join("capsule.toml"), &manifest).unwrap();
    let unwired = tempfile::tempdir().unwrap();
    fs::write(
        unwired.path().join("capsule.toml"),
        manifest.replace(
            "[[connection]]\nfrom = \"api.counter\"\nto = \"counter.api\"\n\n",
            "",
        ),
    )
    .unwrap();
    ato(ato_home.path())
        .args(["init", unwired.path().to_str().unwrap(), "--initial-only"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "internal client port `api.counter` is unwired",
        ));
    ato(ato_home.path())
        .args(["init", project.path().to_str().unwrap()])
        .assert()
        .success();
    assert!(http_request(public_port, "GET", "/").ends_with("composed:1"));
    ato(ato_home.path())
        .args(["stop", project.path().to_str().unwrap()])
        .assert()
        .success();
    let root = fs::read_to_string(project.path().join(".capsule/refs/heads/main")).unwrap();
    assert!(root.starts_with("blake3:"));
    let root_object: serde_json::Value = serde_json::from_slice(
        &fs::read(
            project
                .path()
                .join(".capsule/objects/blake3")
                .join(root.trim().split_once(':').unwrap().1),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(root_object["semantics"], "capsule.compose@1");
    let bundle = project.path().join("composite.capsule");
    ato(ato_home.path())
        .args([
            "encap",
            &format!("{}@main", project.path().display()),
            "-o",
            bundle.to_str().unwrap(),
        ])
        .assert()
        .success();
    let mut portable = ato(recipient_home.path())
        .args(["run", bundle.to_str().unwrap()])
        .spawn()
        .unwrap();
    assert!(http_request(public_port, "GET", "/").ends_with("composed:1"));
    assert!(portable.wait().unwrap().success());
}

#[test]
fn concurrent_writers_never_lose_refs_or_records() {
    let project = tempfile::tempdir().unwrap();
    let repository = LocalCapsuleRepository::open(project.path()).unwrap();
    let computation =
        |byte: &str| ComputationRef::parse(format!("blake3:{}", byte.repeat(64))).unwrap();
    let base = computation("a");
    repository.update_head("main", None, &base).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let updates: Vec<_> = [computation("b"), computation("c")]
        .into_iter()
        .map(|candidate| {
            let path = project.path().to_path_buf();
            let base = base.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let repository = LocalCapsuleRepository::open(path).unwrap();
                barrier.wait();
                repository.update_head("main", Some(&base), &candidate)
            })
        })
        .collect();
    barrier.wait();
    assert_eq!(
        updates
            .into_iter()
            .map(|update| update.join().unwrap())
            .filter(Result::is_ok)
            .count(),
        1
    );

    let payload = repository.objects().put(b"event").unwrap();
    let record_writers: Vec<_> = (0..32)
        .map(|_| {
            let path = project.path().to_path_buf();
            let payload = payload.clone();
            let base = base.clone();
            std::thread::spawn(move || {
                LocalCapsuleRepository::open(path)
                    .unwrap()
                    .append_record(RecordEnvelope {
                        id: RecordId::new("main", 0),
                        adapter_id: "test.adapter@1".to_owned(),
                        protocol_id: ProtocolId::parse("test.protocol@1").unwrap(),
                        port_id: PortId::parse("test.port").unwrap(),
                        direction: Direction::Internal,
                        payload_ref: payload,
                        head_before: base.clone(),
                        head_after: base,
                        caused_by: Vec::new(),
                        observed_at: "0".to_owned(),
                    })
                    .unwrap()
                    .id
            })
        })
        .collect();
    let mut ids: Vec<_> = record_writers
        .into_iter()
        .map(|writer| writer.join().unwrap().seq)
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, (1..=32).collect::<Vec<_>>());
    assert_eq!(
        repository.records_for_stream("main", None).unwrap().len(),
        32
    );
}

#[test]
fn concurrent_resume_claims_exactly_one_run_lease() {
    let project = tempfile::tempdir().unwrap();
    let ato_home = tempfile::tempdir().unwrap();
    write_project(project.path(), r#"["sh", "-c", "exec sleep 30"]"#, "");
    ato(ato_home.path())
        .args(["init", project.path().to_str().unwrap(), "--initial-only"])
        .assert()
        .success();

    let barrier = Arc::new(Barrier::new(3));
    let contenders: Vec<_> = (0..2)
        .map(|_| {
            let project = project.path().to_path_buf();
            let ato_home = ato_home.path().to_path_buf();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                ato(&ato_home)
                    .args(["resume", project.to_str().unwrap()])
                    .output()
                    .unwrap()
            })
        })
        .collect();
    barrier.wait();
    let outputs: Vec<_> = contenders
        .into_iter()
        .map(|contender| contender.join().unwrap())
        .collect();
    assert_eq!(
        outputs
            .iter()
            .filter(|output| output.status.success())
            .count(),
        1,
        "only one concurrent resume may own the active Run lease"
    );
    assert_eq!(
        outputs
            .iter()
            .filter(|output| String::from_utf8_lossy(&output.stderr).contains("active Run lease"))
            .count(),
        1
    );
    ato(ato_home.path())
        .args(["stop", project.path().to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn stop_seals_the_quiesced_observation_frontier() {
    let _network = NETWORK_TEST_LOCK.lock().unwrap();
    let project = tempfile::tempdir().unwrap();
    let ato_home = tempfile::tempdir().unwrap();
    let upstream_port = unused_port();
    let public_port = unused_port();
    write_quiesce_counter(project.path(), upstream_port, public_port);
    ato(ato_home.path())
        .args(["init", project.path().to_str().unwrap()])
        .assert()
        .success();

    let repository = LocalCapsuleRepository::open(project.path()).unwrap();
    let active = repository.active_run().unwrap().unwrap();
    let request = std::thread::spawn(move || http_request(public_port, "POST", "/increment"));
    wait_until(|| {
        project
            .path()
            .join(".capsule/runs/request.started")
            .exists()
    });
    let project_path = project.path().to_path_buf();
    let home_path = ato_home.path().to_path_buf();
    let stopping = std::thread::spawn(move || {
        ato(&home_path)
            .args(["stop", project_path.to_str().unwrap()])
            .output()
            .unwrap()
    });
    wait_until(|| project.path().join(".capsule/runs/stop.request").exists());
    fs::write(
        project.path().join(".capsule/runs/request.release"),
        b"release",
    )
    .unwrap();
    assert!(request.join().unwrap().starts_with("HTTP/1.1 204"));
    let stopped = stopping.join().unwrap();
    assert!(
        stopped.status.success(),
        "stop failed: {}",
        String::from_utf8_lossy(&stopped.stderr)
    );

    let records = repository.records_for_stream("main", None).unwrap();
    let final_evolution = records
        .iter()
        .find(|record| {
            record.adapter_id == "ato.http@1"
                && record.direction == Direction::Inbound
                && record.head_before == active.head
        })
        .expect("the in-flight request must commit before quiesce acknowledgement");
    assert_eq!(
        repository.head("main").unwrap().unwrap(),
        final_evolution.head_after,
        "stop must seal the head observed after live Adapters finish quiescing"
    );
    assert_ne!(final_evolution.head_before, final_evolution.head_after);
}

#[test]
fn encap_current_exports_a_point_in_time_and_live_run_continues() {
    let _network = NETWORK_TEST_LOCK.lock().unwrap();
    let project = tempfile::tempdir().unwrap();
    let author_home = tempfile::tempdir().unwrap();
    let recipient_home = tempfile::tempdir().unwrap();
    let upstream_port = unused_port();
    let public_port = unused_port();
    write_memory_counter(project.path(), upstream_port, public_port, 5);

    ato(author_home.path())
        .args(["init", project.path().to_str().unwrap()])
        .assert()
        .success();
    let sealed_before =
        fs::read_to_string(project.path().join(".capsule/refs/heads/main")).unwrap();
    assert!(http_request(public_port, "POST", "/increment").starts_with("HTTP/1.1 204"));
    assert!(http_request(public_port, "GET", "/count").ends_with('1'));

    let bundle = project.path().join("current.capsule");
    ato(author_home.path())
        .args([
            "encap",
            &format!("{}@main", project.path().display()),
            "--current",
            "-o",
            bundle.to_str().unwrap(),
        ])
        .assert()
        .success();
    let verified = ato(author_home.path())
        .args(["__bundle", "verify", bundle.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(verified.status.success());
    let report: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(report["format_version"], 2);
    assert_eq!(report["validation"]["status"], "valid");
    assert_eq!(report["materializations"][0]["id"], "ato.replay@1");
    assert!(report["object_count"].as_u64().unwrap() > 0);
    assert!(report["decoded_size"].as_u64().unwrap() > 0);
    assert_eq!(report["exported_ports"][0]["protocol"], "ato.http@1");
    assert_eq!(
        fs::read_to_string(project.path().join(".capsule/refs/heads/main")).unwrap(),
        sealed_before,
        "current encap must not advance the sealed branch ref"
    );

    assert!(http_request(public_port, "POST", "/increment").starts_with("HTTP/1.1 204"));
    assert!(http_request(public_port, "GET", "/count").ends_with('2'));
    ato(author_home.path())
        .args(["stop", project.path().to_str().unwrap()])
        .assert()
        .success();

    let mut portable = ato(recipient_home.path())
        .args(["run", bundle.to_str().unwrap()])
        .spawn()
        .unwrap();
    for _ in 0..3 {
        assert!(
            http_request(public_port, "GET", "/count").ends_with('1'),
            "the exported continuation must remain at count=1"
        );
    }
    assert!(portable.wait().unwrap().success());
}

#[test]
fn failed_process_never_publishes_an_active_run() {
    let project = tempfile::tempdir().unwrap();
    let ato_home = tempfile::tempdir().unwrap();
    write_project(
        project.path(),
        r#"["definitely-not-an-ato-test-command"]"#,
        "",
    );
    ato(ato_home.path())
        .args(["init", project.path().to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("before becoming active"));
    assert!(!project.path().join(".capsule/runs/active.json").exists());
}

fn walk(root: impl AsRef<Path>) -> Vec<std::path::PathBuf> {
    let mut pending = vec![root.as_ref().to_path_buf()];
    let mut files = Vec::new();
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            pending.extend(
                fs::read_dir(path)
                    .unwrap()
                    .filter_map(Result::ok)
                    .map(|entry| entry.path()),
            );
        } else {
            files.push(path);
        }
    }
    files
}

fn record_events(project: &Path, adapter_id: &str) -> Vec<serde_json::Value> {
    walk(project.join(".capsule/records/main"))
        .into_iter()
        .filter_map(|path| fs::read(path).ok())
        .filter_map(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .filter(|record| record["adapter_id"] == adapter_id)
        .filter_map(|record| {
            let reference = record["payload_ref"].as_str()?;
            let digest = reference.split_once(':')?.1;
            serde_json::from_slice(
                &fs::read(project.join(".capsule/objects/blake3").join(digest)).ok()?,
            )
            .ok()
        })
        .collect()
}

fn unused_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn http_request(port: u16, method: &str, path: &str) -> String {
    for _ in 0..200 {
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            let request = format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            if stream.write_all(request.as_bytes()).is_ok() {
                let mut response = String::new();
                if stream.read_to_string(&mut response).is_ok() && !response.is_empty() {
                    return response;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("HTTP endpoint on port {port} did not return a response")
}
