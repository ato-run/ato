#![cfg(unix)]

use std::fs;
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;

static NETWORK_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn ato(ato_home: &Path) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("ato"));
    command.env("ATO_HOME", ato_home);
    command
}

fn unused_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn write_http_project(root: &Path, listen_port: u16, upstream_port: u16) {
    fs::write(
        root.join("capsule.toml"),
        format!(
            r#"schema = 1

[[process]]
id = "app"
command = ["sh", "-c", "exec sleep 30"]
cwd = "."

[[adapter]]
target = "app"
use = "ato.process@1"

[[adapter]]
target = "workspace"
use = "ato.workspace@1"

[[port]]
id = "app.http"
node = "app"
protocol = "ato.http@1"
role = "server"

[[adapter]]
port = "app.http"
use = "ato.http@1"
listen = "127.0.0.1:{listen_port}"
upstream = "127.0.0.1:{upstream_port}"

[encap]
materializers = ["ato.replay@1"]
"#,
        ),
    )
    .unwrap();
}

fn inspect(ato_home: &Path, project: &Path) -> serde_json::Value {
    let output = ato(ato_home)
        .args(["__desktop", "inspect", project.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "inspect failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).expect("inspect stdout must be a single JSON object")
}

#[test]
fn inspect_is_pure_json_and_tracks_the_run_lifecycle() {
    let _network = NETWORK_TEST_LOCK.lock().unwrap();
    let project = tempfile::tempdir().unwrap();
    let ato_home = tempfile::tempdir().unwrap();
    let listen_port = unused_port();
    let upstream_port = unused_port();
    write_http_project(project.path(), listen_port, upstream_port);

    let inactive = inspect(ato_home.path(), project.path());
    assert_eq!(inactive["status"], "inactive");
    assert_eq!(inactive["branch"], "");
    assert_eq!(inactive["head"], "");
    assert_eq!(inactive["surfaces"].as_array().unwrap().len(), 0);

    ato(ato_home.path())
        .args(["init", project.path().to_str().unwrap(), "--initial-only"])
        .assert()
        .success();
    let after_init = inspect(ato_home.path(), project.path());
    assert_eq!(after_init["status"], "inactive");

    ato(ato_home.path())
        .args(["resume", &format!("{}@main", project.path().display())])
        .assert()
        .success();
    let active = inspect(ato_home.path(), project.path());
    assert_eq!(active["status"], "active");
    assert_eq!(active["branch"], "main");
    assert!(active["head"].as_str().unwrap().starts_with("blake3:"));
    let surfaces = active["surfaces"].as_array().unwrap();
    assert_eq!(surfaces.len(), 1);
    assert_eq!(surfaces[0]["kind"], "web");
    assert_eq!(
        surfaces[0]["url"],
        format!("http://127.0.0.1:{listen_port}")
    );

    ato(ato_home.path())
        .args(["stop", project.path().to_str().unwrap()])
        .assert()
        .success();
    let after_stop = inspect(ato_home.path(), project.path());
    assert_eq!(after_stop["status"], "inactive");
    assert_eq!(after_stop["surfaces"].as_array().unwrap().len(), 0);
}

#[test]
fn inspect_on_a_missing_project_fails_cleanly() {
    let ato_home = tempfile::tempdir().unwrap();
    let missing = tempfile::tempdir().unwrap().path().join("does-not-exist");
    ato(ato_home.path())
        .args(["__desktop", "inspect", missing.to_str().unwrap()])
        .assert()
        .failure();
}
