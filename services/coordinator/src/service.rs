//! Resident Coordinator service lifecycle (G3): start / stop / status /
//! restart around the `serve` process.
//!
//! The database file is the authority; the service state file only records
//! where the running process is and how it was started. No token is stored
//! here — the caller passes it for the commands that need it.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceState {
    pub pid: u32,
    pub addr: String,
    pub db: String,
    pub started_at: i64,
}

pub fn default_state_dir() -> PathBuf {
    std::env::var_os("ATO_COORDINATOR_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".ato/coordinator"))
                .unwrap_or_else(|| PathBuf::from(".ato/coordinator"))
        })
}

pub fn state_path(dir: &Path) -> PathBuf {
    dir.join("service.json")
}

pub fn read_state(dir: &Path) -> Result<Option<ServiceState>, String> {
    let path = state_path(dir);
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|error| error.to_string())
}

pub fn write_state(dir: &Path, state: &ServiceState) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let path = state_path(dir);
    let text = serde_json::to_string_pretty(state).map_err(|error| error.to_string())?;
    std::fs::write(&path, text).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub fn remove_state(dir: &Path) {
    let _ = std::fs::remove_file(state_path(dir));
}

/// Is this pid a live process? `kill -0` is the portable check we already
/// shell out to; it does not signal the process.
pub fn process_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn running(dir: &Path) -> Result<Option<ServiceState>, String> {
    Ok(read_state(dir)?.filter(|state| process_alive(state.pid)))
}

fn health(addr: &str) -> Option<String> {
    let mut stream = TcpStream::connect(addr).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    write!(
        stream,
        "GET /health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    Some(
        response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.trim().to_owned())
            .unwrap_or_default(),
    )
}

pub fn start(db: &Path, addr: &str, token: &str, dir: &Path) -> Result<ServiceState, String> {
    if let Some(state) = running(dir)? {
        return Err(format!(
            "already_running pid={} addr={}",
            state.pid, state.addr
        ));
    }
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let log_path = dir.join("coordinator.log");
    let log = std::fs::File::create(&log_path).map_err(|error| error.to_string())?;
    let err = log.try_clone().map_err(|error| error.to_string())?;
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let child = Command::new(executable)
        .arg("serve")
        .arg("--db")
        .arg(db)
        .arg("--addr")
        .arg(addr)
        .arg("--token")
        .arg(token)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err))
        .spawn()
        .map_err(|error| error.to_string())?;
    let state = ServiceState {
        pid: child.id(),
        addr: addr.to_owned(),
        db: db.display().to_string(),
        started_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0),
    };
    write_state(dir, &state)?;
    Ok(state)
}

pub fn stop(dir: &Path, token: Option<&str>) -> Result<bool, String> {
    let Some(state) = read_state(dir)? else {
        return Ok(false);
    };
    if !process_alive(state.pid) {
        remove_state(dir);
        return Ok(false);
    }
    if let Some(token) = token {
        let mut stream = TcpStream::connect(&state.addr).map_err(|error| error.to_string())?;
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
        let request = format!(
            "POST /control/shutdown HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            state.addr, token
        );
        let _ = stream.write_all(request.as_bytes());
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
    }
    if !wait_for_exit(state.pid, Duration::from_secs(3)) {
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(state.pid.to_string())
            .status();
        wait_for_exit(state.pid, Duration::from_secs(3));
    }
    remove_state(dir);
    Ok(true)
}

fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    !process_alive(pid)
}

pub fn status(dir: &Path) -> Result<Option<(ServiceState, Option<String>)>, String> {
    let Some(state) = read_state(dir)? else {
        return Ok(None);
    };
    if !process_alive(state.pid) {
        remove_state(dir);
        return Ok(None);
    }
    let health = health(&state.addr);
    Ok(Some((state, health)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_roundtrip_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let state = ServiceState {
            pid: std::process::id(),
            addr: "127.0.0.1:8788".to_owned(),
            db: "/tmp/coordinator.sqlite".to_owned(),
            started_at: 1,
        };
        write_state(dir.path(), &state).unwrap();
        assert_eq!(read_state(dir.path()).unwrap(), Some(state.clone()));
        assert_eq!(running(dir.path()).unwrap(), Some(state));
        remove_state(dir.path());
        assert_eq!(read_state(dir.path()).unwrap(), None);
    }

    #[test]
    fn detects_a_live_and_a_dead_pid() {
        assert!(process_alive(std::process::id()));
        assert!(!process_alive(4_000_000));
    }

    #[test]
    fn status_is_empty_without_a_state_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(status(dir.path()).unwrap().is_none());
    }
}
