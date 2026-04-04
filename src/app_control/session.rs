use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::runtime::process::{ProcessInfo, ProcessManager, ProcessStatus};

use super::guest_contract::{parse_guest_contract, preview_guest_contract, GuestContractPreview};
use super::resolve::{derive_local_launch_plan, normalize_local_handle};

const SESSION_ACTION_START: &str = "session_start";
const SESSION_ACTION_STOP: &str = "session_stop";
const SESSION_RUNTIME: &str = "desky-session";
const SESSION_READY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize)]
struct SessionStartEnvelope {
    schema_version: &'static str,
    package_id: &'static str,
    action: &'static str,
    session: SessionInfo,
}

#[derive(Debug, Clone, Serialize)]
struct SessionStopEnvelope {
    schema_version: &'static str,
    package_id: &'static str,
    action: &'static str,
    session_id: String,
    stopped: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    session_id: String,
    handle: String,
    normalized_handle: String,
    status: String,
    adapter: String,
    frontend_entry: String,
    transport: String,
    healthcheck_url: String,
    invoke_url: String,
    capabilities: Vec<String>,
    pid: i32,
    log_path: String,
    manifest_path: String,
    target_label: String,
    notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSessionInfo {
    session_id: String,
    handle: String,
    normalized_handle: String,
    adapter: String,
    frontend_entry: String,
    transport: String,
    healthcheck_url: String,
    invoke_url: String,
    capabilities: Vec<String>,
    pid: i32,
    log_path: String,
    manifest_path: String,
    target_label: String,
    notes: Vec<String>,
}

pub fn start_session(
    handle: &str,
    target_label: Option<&str>,
    json: bool,
) -> Result<()> {
    let normalized = normalize_local_handle(handle)?;
    let (manifest_path, plan, launch, notes) = derive_local_launch_plan(&normalized, target_label)?;
    let raw_manifest = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest_value: toml::Value = toml::from_str(&raw_manifest)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    let guest = parse_guest_contract(
        &manifest_value,
        manifest_path
            .parent()
            .unwrap_or_else(|| Path::new(".")),
    )
    .ok_or_else(|| anyhow::anyhow!(
        "missing [metadata.desky_guest] contract in {}",
        manifest_path.display()
    ))?;

    let port = reserve_port(guest.default_port)?;
    let process_manager = ProcessManager::new()?;
    let session_root = session_root()?;
    fs::create_dir_all(&session_root)
        .with_context(|| format!("failed to create session root {}", session_root.display()))?;

    let log_path = session_root.join(format!("session-{}.log", port));
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("failed to open log file {}", log_path.display()))?;
    let stderr = stdout
        .try_clone()
        .with_context(|| format!("failed to clone log file {}", log_path.display()))?;

    let mut command = Command::new(&launch.command);
    command
        .args(&launch.args)
        .current_dir(&launch.working_dir)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));

    for (key, value) in &launch.env_vars {
        command.env(key, value);
    }
    command.env("PYTHONUNBUFFERED", "1");
    command.env("DESKY_SESSION_PORT", port.to_string());
    command.env("DESKY_SESSION_HOST", "127.0.0.1");
    command.env("DESKY_SESSION_ID", format!("desky-session-{port}"));
    command.env("DESKY_SESSION_ADAPTER", &guest.adapter);
    command.env("DESKY_SESSION_RPC_PATH", &guest.rpc_path);
    command.env("DESKY_SESSION_HEALTH_PATH", &guest.health_path);

    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to start guest backend '{}' from {}",
            launch.command,
            launch.working_dir.display()
        )
    })?;

    let session_id = format!("desky-session-{}", child.id());
    let process_info = ProcessInfo {
        id: session_id.clone(),
        name: plan
            .manifest
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("desky-guest")
            .to_string(),
        pid: child.id() as i32,
        workload_pid: None,
        status: ProcessStatus::Starting,
        runtime: SESSION_RUNTIME.to_string(),
        start_time: SystemTime::now(),
        manifest_path: Some(manifest_path.clone()),
        scoped_id: None,
        target_label: Some(plan.selected_target_label().to_string()),
        requested_port: Some(port),
        log_path: Some(log_path.clone()),
        ready_at: None,
        last_event: Some("spawned".to_string()),
        last_error: None,
        exit_code: None,
    };
    process_manager.write_pid(&process_info)?;

    let healthcheck_url = format!("http://127.0.0.1:{}{}", port, guest.health_path);
    let invoke_url = format!("http://127.0.0.1:{}{}", port, guest.rpc_path);

    match wait_for_http_ready(&mut child, port, &guest.health_path, SESSION_READY_TIMEOUT) {
        Ok(()) => {
            let _ = process_manager.update_pid(&session_id, |info| {
                info.status = ProcessStatus::Ready;
                info.ready_at = Some(SystemTime::now());
                info.last_event = Some("ready".to_string());
                info.last_error = None;
            })?;
        }
        Err(err) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = process_manager.update_pid(&session_id, |info| {
                info.status = ProcessStatus::Failed;
                info.last_event = Some("ready_failed".to_string());
                info.last_error = Some(err.to_string());
                info.exit_code = Some(-1);
            })?;
            anyhow::bail!(
                "guest backend failed to become ready: {}. See logs at {}",
                err,
                log_path.display()
            );
        }
    }

    let session = StoredSessionInfo {
        session_id: session_id.clone(),
        handle: handle.to_string(),
        normalized_handle: normalized.display().to_string(),
        adapter: guest.adapter.clone(),
        frontend_entry: guest.frontend_entry.display().to_string(),
        transport: guest.transport.clone(),
        healthcheck_url: healthcheck_url.clone(),
        invoke_url: invoke_url.clone(),
        capabilities: guest.capabilities.clone(),
        pid: child.id() as i32,
        log_path: log_path.display().to_string(),
        manifest_path: manifest_path.display().to_string(),
        target_label: plan.selected_target_label().to_string(),
        notes,
    };
    write_session_record(&session_root, &session)?;

    let info = SessionInfo {
        session_id,
        handle: session.handle,
        normalized_handle: session.normalized_handle,
        status: "ready".to_string(),
        adapter: session.adapter,
        frontend_entry: session.frontend_entry,
        transport: session.transport,
        healthcheck_url: session.healthcheck_url,
        invoke_url: session.invoke_url,
        capabilities: session.capabilities,
        pid: session.pid,
        log_path: session.log_path,
        manifest_path: session.manifest_path,
        target_label: session.target_label,
        notes: session.notes,
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&SessionStartEnvelope {
                schema_version: super::SCHEMA_VERSION,
                package_id: super::DESKY_PACKAGE_ID,
                action: SESSION_ACTION_START,
                session: info,
            })?
        );
    } else {
        print_session_info(&info, &preview_guest_contract(&guest));
    }

    Ok(())
}

pub fn stop_session(session_id: &str, json: bool) -> Result<()> {
    let process_manager = ProcessManager::new()?;
    let stopped = process_manager.stop_process(session_id, true)?;
    let session_path = session_root()?.join(format!("{session_id}.json"));
    if session_path.exists() {
        fs::remove_file(&session_path)
            .with_context(|| format!("failed to remove session file {}", session_path.display()))?;
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&SessionStopEnvelope {
                schema_version: super::SCHEMA_VERSION,
                package_id: super::DESKY_PACKAGE_ID,
                action: SESSION_ACTION_STOP,
                session_id: session_id.to_string(),
                stopped,
            })?
        );
        return Ok(());
    }

    if stopped {
        println!("Stopped session: {session_id}");
    } else {
        println!("Session was not active: {session_id}");
    }
    Ok(())
}

fn session_root() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("DESKY_SESSION_ROOT") {
        return Ok(PathBuf::from(path));
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("failed to resolve home directory"))?;
    Ok(home.join(".ato").join("apps").join("desky").join("sessions"))
}

fn write_session_record(root: &Path, session: &StoredSessionInfo) -> Result<()> {
    let path = root.join(format!("{}.json", session.session_id));
    fs::write(&path, serde_json::to_vec_pretty(session)?)
        .with_context(|| format!("failed to write session file {}", path.display()))
}

fn reserve_port(default_port: Option<u16>) -> Result<u16> {
    if let Some(port) = default_port {
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return Ok(port);
        }
    }

    let listener = TcpListener::bind(("127.0.0.1", 0)).context("failed to allocate local port")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

fn wait_for_http_ready(
    child: &mut std::process::Child,
    port: u16,
    path: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("process exited before readiness with status {status}");
        }

        if http_get_ok(port, path)? {
            return Ok(());
        }

        if std::time::Instant::now() >= deadline {
            anyhow::bail!("readiness timed out for http://127.0.0.1:{port}{path}");
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

fn http_get_ok(port: u16, path: &str) -> Result<bool> {
    let mut stream = match std::net::TcpStream::connect(("127.0.0.1", port)) {
        Ok(stream) => stream,
        Err(_) => return Ok(false),
    };
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    write!(
        stream,
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        path
    )?;
    stream.flush()?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200"))
}

fn print_session_info(info: &SessionInfo, guest: &GuestContractPreview) {
    println!("Session: {}", info.session_id);
    println!("Handle: {}", info.handle);
    println!("Adapter: {}", guest.adapter);
    println!("Frontend: {}", guest.frontend_entry);
    println!("Invoke URL: {}", info.invoke_url);
    println!("Health URL: {}", info.healthcheck_url);
    println!("PID: {}", info.pid);
    println!("Log: {}", info.log_path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserve_port_returns_requested_port_when_available() {
        let port = reserve_port(Some(43291)).expect("reserve port");
        assert_eq!(port, 43291);
    }
}
