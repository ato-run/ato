#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;

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
        r#"["sh", "-c", "rustc broken.rs 2>&1 || true"]"#,
        "",
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
    ato(recipient_home.path())
        .args(["run", bundle.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("error"));
}

#[test]
fn stateful_protocol_state_survives_nonzero_process_and_replay() {
    let project = tempfile::tempdir().unwrap();
    let author_home = tempfile::tempdir().unwrap();
    let recipient_home = tempfile::tempdir().unwrap();
    fs::write(project.path().join("count.txt"), "0").unwrap();
    let upstream_port = unused_port();
    let public_port = unused_port();
    fs::write(
        project.path().join("counter.rs"),
        r#"use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;

fn main() {
    let address = std::env::args().nth(1).unwrap();
    let listener = TcpListener::bind(address).unwrap();
    for _ in 0..2 {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let size = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..size]);
        if request.starts_with("POST /increment ") {
            let count = fs::read_to_string("count.txt").unwrap().trim().parse::<u64>().unwrap() + 1;
            fs::write("count.txt", count.to_string()).unwrap();
            stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        } else {
            let count = fs::read_to_string("count.txt").unwrap();
            let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", count.len(), count);
            stream.write_all(response.as_bytes()).unwrap();
        }
    }
}
"#,
    )
    .unwrap();
    write_project(
        project.path(),
        &format!(
            r#"["sh", "-c", "rustc counter.rs -o counter-bin && ./counter-bin 127.0.0.1:{upstream_port}"]"#
        ),
        &format!(
            r#"[[port]]
id = "app.http"
protocol = "ato.http@1"
role = "server"

[[adapter]]
port = "app.http"
use = "ato.http@1"
listen = "127.0.0.1:{public_port}"
upstream = "127.0.0.1:{upstream_port}""#
        ),
    );
    ato(author_home.path())
        .args(["init", project.path().to_str().unwrap()])
        .assert()
        .success();
    let increment = http_request(public_port, "POST", "/increment");
    assert!(increment.starts_with("HTTP/1.1 204"));
    let count = http_request(public_port, "GET", "/count");
    assert!(count.ends_with("1"));
    wait_until(|| {
        fs::read_dir(project.path().join(".capsule/records"))
            .is_ok_and(|entries| entries.count() >= 4)
    });
    ato(author_home.path())
        .args(["stop", project.path().to_str().unwrap()])
        .assert()
        .success();
    let records = fs::read_dir(project.path().join(".capsule/records"))
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
        }
    }
    assert!(
        !fs::read(&bundle)
            .unwrap()
            .windows(b"alice-secret".len())
            .any(|value| value == b"alice-secret")
    );
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
    ato(ato_home.path())
        .args(["run", multi.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn one_root_computation_runs_an_explicit_multi_process_application() {
    let project = tempfile::tempdir().unwrap();
    let ato_home = tempfile::tempdir().unwrap();
    fs::write(
        project.path().join("capsule.toml"),
        r#"schema = 1

[[process]]
id = "api"
command = ["sh", "-c", "printf api > api.out"]
cwd = "."

[[process]]
id = "counter"
command = ["sh", "-c", "printf counter > counter.out"]
cwd = "."

[[adapter]]
target = "api"
use = "ato.process@1"

[[adapter]]
target = "counter"
use = "ato.process@1"

[[port]]
id = "app.http"
protocol = "ato.http@1"
role = "server"

[encap]
materializers = ["ato.replay@1"]
"#,
    )
    .unwrap();
    ato(ato_home.path())
        .args(["init", project.path().to_str().unwrap()])
        .assert()
        .success();
    wait_until(|| {
        project.path().join("api.out").is_file() && project.path().join("counter.out").is_file()
    });
    ato(ato_home.path())
        .args(["stop", project.path().to_str().unwrap()])
        .assert()
        .success();
    let root = fs::read_to_string(project.path().join(".capsule/refs/heads/main")).unwrap();
    assert!(root.starts_with("blake3:"));
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
