use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde_json::Value;

#[test]
fn import_run_executes_with_shadow_manifest() -> Result<()> {
    let root = test_root("executes")?;
    let _cleanup = Cleanup(root.clone());
    let source = root.join("source");
    let recipe = root.join("recipe.toml");
    fs::create_dir_all(&source)?;
    fs::write(source.join("README.md"), "# shadow import\n")?;
    fs::write(
        &recipe,
        r#"schema_version = "0.3"
name = "shadow-import"
version = "0.1.0"
type = "app"
runtime = "source/native"
run = "true"
"#,
    )?;

    let output = run_import(&root, &source, Some(&recipe))?;
    assert_eq!(output["run"]["status"].as_str(), Some("passed"));
    assert_ne!(
        output["run"]["error_class"].as_str(),
        Some("run_execution_not_wired")
    );
    assert_eq!(
        output["source"]["source_url_normalized"].as_str(),
        Some("https://github.com/ato-run/shadow-import")
    );
    assert_eq!(output["recipe"]["origin"].as_str(), Some("manual"));
    assert_eq!(
        output["recipe"]["recipe_toml"].as_str(),
        fs::read_to_string(&recipe).ok().as_deref()
    );
    Ok(())
}

#[test]
fn import_run_does_not_write_capsule_toml_to_source() -> Result<()> {
    let root = test_root("source-clean")?;
    let _cleanup = Cleanup(root.clone());
    let source = root.join("source");
    let recipe = root.join("recipe.toml");
    fs::create_dir_all(&source)?;
    fs::write(source.join("app.txt"), "source bytes\n")?;
    fs::write(
        &recipe,
        r#"schema_version = "0.3"
name = "source-clean"
version = "0.1.0"
type = "app"
runtime = "source/native"
run = "true"
"#,
    )?;

    assert!(!source.join("capsule.toml").exists());
    let _output = run_import(&root, &source, Some(&recipe))?;
    assert!(
        !source.join("capsule.toml").exists(),
        "import run must write capsule.toml only to the shadow workspace"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn import_emit_json_uses_ato_home_when_cwd_is_root() -> Result<()> {
    let root = test_root("cwd-root")?;
    let _cleanup = Cleanup(root.clone());
    let source = root.join("source");
    let recipe = root.join("recipe.toml");
    let home = root.join("home");
    let ato_home = home.join(".ato");
    fs::create_dir_all(&source)?;
    fs::create_dir_all(&ato_home)?;
    fs::write(source.join("README.md"), "# cwd root import\n")?;
    fs::write(
        &recipe,
        r#"schema_version = "0.3"
name = "cwd-root-import"
version = "0.1.0"
type = "app"
runtime = "source/native"
run = "true"
"#,
    )?;

    let output = Command::new(assert_cmd::cargo::cargo_bin("ato"))
        .arg("import")
        .arg("github.com/ato-run/shadow-import")
        .arg("--emit-json")
        .arg("--recipe")
        .arg(&recipe)
        .env("ATO_IMPORT_LOCAL_SOURCE_OVERRIDE", &source)
        .env(
            "ATO_IMPORT_LOCAL_REVISION_ID",
            "1111111111111111111111111111111111111111",
        )
        .env("ATO_IMPORT_LOCAL_TREE_HASH", "blake3:test-tree")
        .env("ATO_IMPORT_KEEP_WORKSPACE", "1")
        .env("ATO_HOME", &ato_home)
        .env("HOME", &home)
        .current_dir(Path::new("/"))
        .output()
        .context("failed to run ato import from /")?;
    assert!(
        output.status.success(),
        "ato import failed from /\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(parsed["run"]["status"].as_str(), Some("not_run"));
    assert_eq!(parsed["recipe"]["origin"].as_str(), Some("manual"));

    let import_root = ato_home.join("tmp").join("import");
    let workspace_count = fs::read_dir(&import_root)
        .with_context(|| format!("missing import root {}", import_root.display()))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .count();
    assert!(
        workspace_count > 0,
        "import workspace must be created under ATO_HOME, not the process cwd"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn import_run_uses_ato_home_when_cwd_is_root() -> Result<()> {
    let root = test_root("cwd-root-run")?;
    let _cleanup = Cleanup(root.clone());
    let source = root.join("source");
    let recipe = root.join("recipe.toml");
    let home = root.join("home");
    let ato_home = home.join(".ato");
    fs::create_dir_all(&source)?;
    fs::create_dir_all(&ato_home)?;
    fs::write(source.join("README.md"), "# cwd root run import\n")?;
    fs::write(
        &recipe,
        r#"schema_version = "0.3"
name = "cwd-root-run-import"
version = "0.1.0"
type = "app"
runtime = "source/native"
run = "true"
"#,
    )?;

    let output = Command::new(assert_cmd::cargo::cargo_bin("ato"))
        .arg("import")
        .arg("github.com/ato-run/shadow-import")
        .arg("--run")
        .arg("--emit-json")
        .arg("--recipe")
        .arg(&recipe)
        .env("ATO_IMPORT_LOCAL_SOURCE_OVERRIDE", &source)
        .env(
            "ATO_IMPORT_LOCAL_REVISION_ID",
            "1111111111111111111111111111111111111111",
        )
        .env("ATO_IMPORT_LOCAL_TREE_HASH", "blake3:test-tree")
        .env("ATO_HOME", &ato_home)
        .env("HOME", &home)
        .env("CAPSULE_ALLOW_UNSAFE", "1")
        .current_dir(Path::new("/"))
        .output()
        .context("failed to run ato import --run from /")?;
    assert!(
        output.status.success(),
        "ato import --run failed from /\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(parsed["run"]["status"].as_str(), Some("passed"));

    let import_root = ato_home.join("tmp").join("import");
    assert!(
        import_root.exists(),
        "import root must be created under ATO_HOME, not the process cwd"
    );
    Ok(())
}

#[test]
fn import_run_emit_json_tears_down_ready_server() -> Result<()> {
    if !python3_available() {
        return Ok(());
    }

    let root = test_root("teardown-ready-server")?;
    let _cleanup = Cleanup(root.clone());
    let source = root.join("source");
    let recipe = root.join("recipe.toml");
    fs::create_dir_all(&source)?;
    fs::write(source.join("index.html"), "ready\n")?;
    let port = free_port()?;
    fs::write(
        &recipe,
        format!(
            r#"schema_version = "0.3"
name = "shadow-import-server"
version = "0.1.0"
type = "app"
runtime = "source/native"
run = "python3 -m http.server {port} --bind 127.0.0.1"
port = {port}
"#,
        ),
    )?;

    let output = run_import(&root, &source, Some(&recipe))?;
    assert_eq!(output["run"]["status"].as_str(), Some("passed"));
    assert_eq!(output["run"]["phase"].as_str(), Some("readiness"));
    assert!(
        matches!(
            output["run"]["cleanup_status"].as_str(),
            Some("terminated") | Some("killed")
        ),
        "unexpected cleanup status: {}",
        output["run"]["cleanup_status"]
    );
    assert!(
        TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse()?,
            Duration::from_millis(200),
        )
        .is_err(),
        "import probe server must be stopped before JSON returns"
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn import_run_cleanup_kills_detached_shadow_process() -> Result<()> {
    if !python3_available() {
        return Ok(());
    }

    let root = test_root("teardown-detached-server")?;
    let _cleanup = Cleanup(root.clone());
    let source = root.join("source");
    let recipe = root.join("recipe.toml");
    fs::create_dir_all(&source)?;
    fs::write(
        source.join("spawn_detached.py"),
        r#"import os
import socket
import subprocess
import sys
import time

port = sys.argv[1]
subprocess.Popen(
    [
        sys.executable,
        "-m",
        "http.server",
        port,
        "--bind",
        "127.0.0.1",
        "--directory",
        os.getcwd(),
    ],
    start_new_session=True,
)
deadline = time.time() + 20
while time.time() < deadline:
    try:
        with socket.create_connection(("127.0.0.1", int(port)), timeout=0.1):
            break
    except OSError:
        time.sleep(0.05)
"#,
    )?;
    let port = free_port()?;
    fs::write(
        &recipe,
        format!(
            r#"schema_version = "0.3"
name = "shadow-import-detached"
version = "0.1.0"
type = "app"
runtime = "source/native"
run = "python3 spawn_detached.py {port}"
port = {port}
"#,
        ),
    )?;

    let output = run_import(&root, &source, Some(&recipe))?;
    assert_eq!(output["run"]["status"].as_str(), Some("passed"));
    assert_eq!(output["run"]["phase"].as_str(), Some("readiness"));
    assert!(
        matches!(
            output["run"]["cleanup_status"].as_str(),
            Some("terminated") | Some("killed")
        ),
        "unexpected cleanup status: {}",
        output["run"]["cleanup_status"]
    );
    assert_port_closed(port)?;
    Ok(())
}

#[test]
#[cfg(unix)]
fn import_run_cleanup_escalates_when_server_ignores_term() -> Result<()> {
    if !python3_available() {
        return Ok(());
    }

    let root = test_root("teardown-ignore-term")?;
    let _cleanup = Cleanup(root.clone());
    let source = root.join("source");
    let recipe = root.join("recipe.toml");
    fs::create_dir_all(&source)?;
    fs::write(
        source.join("ignore_term_server.py"),
        r#"import http.server
import signal
import socketserver
import sys

signal.signal(signal.SIGTERM, signal.SIG_IGN)
port = int(sys.argv[1])
with socketserver.TCPServer(("127.0.0.1", port), http.server.SimpleHTTPRequestHandler) as httpd:
    httpd.serve_forever()
"#,
    )?;
    let port = free_port()?;
    fs::write(
        &recipe,
        format!(
            r#"schema_version = "0.3"
name = "shadow-import-ignore-term"
version = "0.1.0"
type = "app"
runtime = "source/native"
run = "python3 ignore_term_server.py {port}"
port = {port}
"#,
        ),
    )?;

    let output = run_import(&root, &source, Some(&recipe))?;
    assert_eq!(output["run"]["status"].as_str(), Some("passed"));
    assert_eq!(output["run"]["cleanup_status"].as_str(), Some("killed"));
    assert_port_closed(port)?;
    Ok(())
}

#[test]
fn import_run_declared_port_exits_before_readiness_fails() -> Result<()> {
    let root = test_root("exited-before-readiness")?;
    let _cleanup = Cleanup(root.clone());
    let source = root.join("source");
    let recipe = root.join("recipe.toml");
    fs::create_dir_all(&source)?;
    let port = free_port()?;
    fs::write(
        &recipe,
        format!(
            r#"schema_version = "0.3"
name = "shadow-import-exits"
version = "0.1.0"
type = "app"
runtime = "source/native"
run = "true"
port = {port}
"#,
        ),
    )?;

    let output = run_import(&root, &source, Some(&recipe))?;
    assert_eq!(output["run"]["status"].as_str(), Some("failed"));
    assert_eq!(
        output["run"]["error_class"].as_str(),
        Some("exited_before_readiness")
    );
    assert_port_closed(port)?;
    Ok(())
}

#[test]
fn import_run_keep_alive_requires_emit_json() -> Result<()> {
    let root = test_root("keep-alive-requires-json")?;
    let _cleanup = Cleanup(root.clone());
    let source = root.join("source");
    let recipe = root.join("recipe.toml");
    fs::create_dir_all(&source)?;
    fs::write(
        &recipe,
        r#"schema_version = "0.3"
name = "shadow-import-keep-alive-requires-json"
version = "0.1.0"
type = "app"
runtime = "source/native"
run = "true"
"#,
    )?;

    let home = root.join("home");
    fs::create_dir_all(&home)?;
    let output = Command::new(assert_cmd::cargo::cargo_bin("ato"))
        .arg("import")
        .arg("github.com/ato-run/shadow-import")
        .arg("--run")
        .arg("--keep-alive")
        .arg("--recipe")
        .arg(&recipe)
        .env("ATO_IMPORT_LOCAL_SOURCE_OVERRIDE", &source)
        .env(
            "ATO_IMPORT_LOCAL_REVISION_ID",
            "1111111111111111111111111111111111111111",
        )
        .env("ATO_IMPORT_LOCAL_TREE_HASH", "blake3:test-tree")
        .env("HOME", &home)
        .env("CAPSULE_ALLOW_UNSAFE", "1")
        .current_dir(&root)
        .output()
        .context("failed to run ato import")?;
    assert!(
        !output.status.success(),
        "ato import unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--keep-alive requires --emit-json"),
        "unexpected stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
#[cfg(unix)]
fn import_run_keep_alive_returns_session_and_leaves_server_running() -> Result<()> {
    if !python3_available() {
        return Ok(());
    }

    let root = test_root("keep-alive-session")?;
    let _cleanup = Cleanup(root.clone());
    let source = root.join("source");
    let recipe = root.join("recipe.toml");
    fs::create_dir_all(&source)?;
    fs::write(
        source.join("keep_alive_server.py"),
        r#"import http.server
import socketserver
import sys

port = int(sys.argv[1])
with socketserver.TCPServer(("127.0.0.1", port), http.server.SimpleHTTPRequestHandler) as httpd:
    httpd.serve_forever()
"#,
    )?;
    let port = free_port()?;
    fs::write(
        &recipe,
        format!(
            r#"schema_version = "0.3"
name = "shadow-import-keep-alive"
version = "0.1.0"
type = "app"
runtime = "source/native"
run = "python3 keep_alive_server.py {port}"
port = {port}
"#,
        ),
    )?;

    let output = run_import_with_args(&root, &source, Some(&recipe), &["--keep-alive"])?;
    assert_eq!(output["run"]["status"].as_str(), Some("running"));
    assert_eq!(output["run"]["phase"].as_str(), Some("readiness"));
    assert_eq!(output["run"]["readiness_state"].as_str(), Some("ready"));
    assert_eq!(
        output["run"]["cleanup_policy"].as_str(),
        Some("keep_until_explicit_stop")
    );
    assert!(output["run"]["cleanup_status"].is_null());
    assert!(output["run"]["run_session_id"].as_str().is_some());
    assert_eq!(output["run"]["primary_port"].as_u64(), Some(port as u64));
    let primary_url = format!("http://127.0.0.1:{port}/");
    assert_eq!(
        output["run"]["primary_url"].as_str(),
        Some(primary_url.as_str())
    );
    assert!(
        output["run"]["process_group_ids"]
            .as_array()
            .is_some_and(|pgids| !pgids.is_empty()),
        "keep-alive output must include process groups: {}",
        output["run"]
    );
    assert!(
        TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse()?,
            Duration::from_millis(500),
        )
        .is_ok(),
        "keep-alive import preview server should survive command return"
    );

    let run_session_id = output["run"]["run_session_id"]
        .as_str()
        .context("missing run_session_id")?;
    run_stop(&root, run_session_id)?;
    assert_port_closes_after_stop(port)?;
    Ok(())
}

#[test]
#[cfg(unix)]
fn import_run_keep_alive_stop_all_stops_session() -> Result<()> {
    if !python3_available() {
        return Ok(());
    }

    let root = test_root("keep-alive-stop-all")?;
    let _cleanup = Cleanup(root.clone());
    let source = root.join("source");
    let recipe = root.join("recipe.toml");
    fs::create_dir_all(&source)?;
    fs::write(
        source.join("server.py"),
        r#"import http.server
import socketserver
import sys

port = int(sys.argv[1])
with socketserver.TCPServer(("127.0.0.1", port), http.server.SimpleHTTPRequestHandler) as httpd:
    httpd.serve_forever()
"#,
    )?;
    let port = free_port()?;
    fs::write(
        &recipe,
        format!(
            r#"schema_version = "0.3"
name = "shadow-import-keep-alive-all"
version = "0.1.0"
type = "app"
runtime = "source/native"
run = "python3 server.py {port}"
port = {port}
"#,
        ),
    )?;

    let output = run_import_with_args(&root, &source, Some(&recipe), &["--keep-alive"])?;
    assert_eq!(output["run"]["status"].as_str(), Some("running"));
    run_stop_all(&root)?;
    assert_port_closes_after_stop(port)?;
    Ok(())
}

fn run_import(root: &Path, source: &Path, recipe: Option<&Path>) -> Result<Value> {
    run_import_with_args(root, source, recipe, &[])
}

fn run_import_with_args(
    root: &Path,
    source: &Path,
    recipe: Option<&Path>,
    extra_args: &[&str],
) -> Result<Value> {
    let home = root.join("home");
    fs::create_dir_all(&home)?;
    let mut command = Command::new(assert_cmd::cargo::cargo_bin("ato"));
    command
        .arg("import")
        .arg("github.com/ato-run/shadow-import")
        .arg("--run")
        .arg("--emit-json")
        .env("ATO_IMPORT_LOCAL_SOURCE_OVERRIDE", source)
        .env(
            "ATO_IMPORT_LOCAL_REVISION_ID",
            "1111111111111111111111111111111111111111",
        )
        .env("ATO_IMPORT_LOCAL_TREE_HASH", "blake3:test-tree")
        .env("HOME", &home)
        .env("CAPSULE_ALLOW_UNSAFE", "1")
        .current_dir(root);
    if let Some(recipe) = recipe {
        command.arg("--recipe").arg(recipe);
    }
    command.args(extra_args);
    let output = command.output().context("failed to run ato import")?;
    assert!(
        output.status.success(),
        "ato import failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "ato import did not emit valid JSON\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn run_stop(root: &Path, session_id: &str) -> Result<()> {
    run_stop_args(root, &[session_id])
}

fn run_stop_all(root: &Path) -> Result<()> {
    run_stop_args(root, &["--all"])
}

fn run_stop_args(root: &Path, args: &[&str]) -> Result<()> {
    let home = root.join("home");
    let output = Command::new(assert_cmd::cargo::cargo_bin("ato"))
        .arg("stop")
        .args(args)
        .arg("--force")
        .env("HOME", &home)
        .current_dir(root)
        .output()
        .context("failed to run ato stop")?;
    assert!(
        output.status.success(),
        "ato stop failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

/// A spawnable `python3` is not enough: on Windows the Microsoft Store
/// app-execution alias (`AppInstallerPythonRedirector.exe`) spawns fine,
/// prints a Store hint, and exits nonzero — so require a real interpreter
/// that reports its version successfully.
fn python3_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|output| {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).starts_with("Python 3")
        })
}

/// Post-stop teardown is observed asynchronously: `ato stop` returns once the
/// session record is cleaned, but a SIGTERM-ed child can hold its listener a
/// beat longer under CI load (seen on Linux runners). Poll briefly; a server
/// that survives the deadline is a real leak.
fn assert_port_closes_after_stop(port: u16) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let open = TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse()?,
            Duration::from_millis(200),
        )
        .is_ok();
        if !open {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("server on port {port} is still accepting connections after stop");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn assert_port_closed(port: u16) -> Result<()> {
    assert!(
        TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse()?,
            Duration::from_millis(200),
        )
        .is_err(),
        "import probe server on port {port} must be stopped before JSON returns"
    );
    Ok(())
}

fn test_root(name: &str) -> Result<PathBuf> {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_nanos()
        .to_string();
    let root = std::env::current_dir()?
        .join(".tmp")
        .join("import-cmd-e2e")
        .join(format!("{name}-{unique}"));
    fs::create_dir_all(&root)?;
    Ok(root)
}

struct Cleanup(PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
