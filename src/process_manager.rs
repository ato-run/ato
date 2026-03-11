use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(any(unix, windows))]
use std::process::Command;
use std::time::SystemTime;

const RUN_DIR: &str = ".ato/run";
const PID_FILE_EXT: &str = ".pid";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub id: String,
    pub name: String,
    pub pid: i32,
    #[serde(default)]
    pub workload_pid: Option<i32>,
    pub status: ProcessStatus,
    pub runtime: String,
    pub start_time: SystemTime,
    #[serde(default)]
    pub manifest_path: Option<PathBuf>,
    #[serde(default)]
    pub scoped_id: Option<String>,
    #[serde(default)]
    pub target_label: Option<String>,
    #[serde(default)]
    pub requested_port: Option<u16>,
    #[serde(default)]
    pub log_path: Option<PathBuf>,
    #[serde(default)]
    pub ready_at: Option<SystemTime>,
    #[serde(default)]
    pub last_event: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessStatus {
    Starting,
    Ready,
    Running,
    Exited,
    Failed,
    Stopped,
    Unknown,
}

impl std::fmt::Display for ProcessStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcessStatus::Starting => write!(f, "starting"),
            ProcessStatus::Ready => write!(f, "ready"),
            ProcessStatus::Running => write!(f, "running"),
            ProcessStatus::Exited => write!(f, "exited"),
            ProcessStatus::Failed => write!(f, "failed"),
            ProcessStatus::Stopped => write!(f, "stopped"),
            ProcessStatus::Unknown => write!(f, "unknown"),
        }
    }
}

impl ProcessStatus {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            ProcessStatus::Starting | ProcessStatus::Ready | ProcessStatus::Running
        )
    }
}

pub struct ProcessManager {
    run_dir: PathBuf,
}

impl ProcessManager {
    pub fn new() -> Result<Self> {
        let run_dir = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?
            .join(RUN_DIR);

        if !run_dir.exists() {
            fs::create_dir_all(&run_dir).with_context(|| {
                format!("Failed to create run directory: {}", run_dir.display())
            })?;
        }

        Ok(Self { run_dir })
    }

    #[allow(dead_code)]
    pub fn get_run_dir(&self) -> &Path {
        &self.run_dir
    }

    pub fn pid_file_path(&self, id: &str) -> PathBuf {
        self.run_dir.join(format!("{}{}", id, PID_FILE_EXT))
    }

    pub fn write_pid(&self, info: &ProcessInfo) -> Result<PathBuf> {
        let pid_path = self.pid_file_path(&info.id);
        let content = toml::to_string(info).with_context(|| "Failed to serialize process info")?;
        fs::write(&pid_path, content)
            .with_context(|| format!("Failed to write PID file: {}", pid_path.display()))?;
        Ok(pid_path)
    }

    pub fn read_pid(&self, id: &str) -> Result<ProcessInfo> {
        let pid_path = self.pid_file_path(id);
        let content = fs::read_to_string(&pid_path)
            .with_context(|| format!("Failed to read PID file: {}", pid_path.display()))?;
        let info: ProcessInfo = toml::from_str(&content)
            .with_context(|| format!("Failed to parse PID file: {}", pid_path.display()))?;
        let updated = self.update_process_status(&info);
        if updated != info {
            let serialized =
                toml::to_string(&updated).with_context(|| "Failed to serialize process info")?;
            fs::write(&pid_path, serialized)
                .with_context(|| format!("Failed to write PID file: {}", pid_path.display()))?;
            Ok(updated)
        } else {
            Ok(info)
        }
    }

    pub fn update_pid<F>(&self, id: &str, updater: F) -> Result<ProcessInfo>
    where
        F: FnOnce(&mut ProcessInfo),
    {
        let pid_path = self.pid_file_path(id);
        let mut info = self.read_pid(id)?;
        updater(&mut info);
        let serialized =
            toml::to_string(&info).with_context(|| "Failed to serialize process info")?;
        fs::write(&pid_path, serialized)
            .with_context(|| format!("Failed to write PID file: {}", pid_path.display()))?;
        Ok(info)
    }

    pub fn delete_pid(&self, id: &str) -> Result<()> {
        let pid_path = self.pid_file_path(id);
        if pid_path.exists() {
            fs::remove_file(&pid_path)
                .with_context(|| format!("Failed to remove PID file: {}", pid_path.display()))?;
        }
        Ok(())
    }

    pub fn list_processes(&self) -> Result<Vec<ProcessInfo>> {
        let mut processes = Vec::new();

        if !self.run_dir.exists() {
            return Ok(processes);
        }

        for entry in fs::read_dir(&self.run_dir)
            .with_context(|| format!("Failed to read run directory: {}", self.run_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();

            if path
                .extension()
                .is_some_and(|ext| ext == PID_FILE_EXT.trim_start_matches('.'))
            {
                if let Some(filename) = path.file_stem() {
                    if let Some(id) = filename.to_str() {
                        if let Ok(info) = self.read_pid(id) {
                            processes.push(info);
                        }
                    }
                }
            }
        }

        Ok(processes)
    }

    fn update_process_status(&self, info: &ProcessInfo) -> ProcessInfo {
        if matches!(
            info.status,
            ProcessStatus::Stopped | ProcessStatus::Exited | ProcessStatus::Failed
        ) {
            return info.clone();
        }

        let is_alive = is_process_alive(info.pid) && process_identity_matches(info);
        if is_alive {
            return info.clone();
        }

        ProcessInfo {
            status: match info.status {
                ProcessStatus::Starting => ProcessStatus::Failed,
                ProcessStatus::Ready | ProcessStatus::Running => ProcessStatus::Exited,
                ProcessStatus::Stopped => ProcessStatus::Stopped,
                ProcessStatus::Exited => ProcessStatus::Exited,
                ProcessStatus::Failed => ProcessStatus::Failed,
                ProcessStatus::Unknown => ProcessStatus::Unknown,
            },
            exit_code: if info.exit_code.is_some() {
                info.exit_code
            } else {
                Some(-1)
            },
            last_error: if matches!(info.status, ProcessStatus::Starting)
                && info.last_error.is_none()
            {
                Some("process exited before readiness".to_string())
            } else {
                info.last_error.clone()
            },
            ..info.clone()
        }
    }

    pub fn find_by_name(&self, name: &str) -> Result<Vec<ProcessInfo>> {
        let all = self.list_processes()?;
        Ok(all
            .into_iter()
            .filter(|p| p.name.to_lowercase() == name.to_lowercase())
            .collect())
    }

    pub fn cleanup_scoped_processes(&self, scoped_id: &str, force: bool) -> Result<usize> {
        let mut cleaned = 0usize;
        let mut failures = Vec::new();

        for process in self
            .list_processes()?
            .into_iter()
            .filter(|process| process.scoped_id.as_deref() == Some(scoped_id))
        {
            if process.status.is_active() {
                match self.stop_process(&process.id, force) {
                    Ok(_) => {
                        cleaned += 1;
                    }
                    Err(err) => {
                        failures.push(format!("{}: {}", process.id, err));
                    }
                }
            } else {
                self.delete_pid(&process.id)?;
                cleaned += 1;
            }
        }

        if failures.is_empty() {
            Ok(cleaned)
        } else {
            anyhow::bail!(
                "Failed to clean up process state for '{}': {}",
                scoped_id,
                failures.join(", ")
            );
        }
    }

    pub fn stop_process(&self, id: &str, force: bool) -> Result<bool> {
        let info = match self.read_pid(id) {
            Ok(i) => i,
            Err(_) => return Ok(false),
        };

        if !info.status.is_active() {
            return Ok(false);
        }

        if !is_process_alive(info.pid) {
            self.delete_pid(id)?;
            return Ok(false);
        }

        if !process_identity_matches(&info) {
            self.delete_pid(id)?;
            return Ok(false);
        }

        if terminate_process(info.pid, force)? {
            wait_for_process_exit(info.pid, 10)?;
            self.delete_pid(id)?;
            Ok(true)
        } else {
            self.delete_pid(id)?;
            Ok(false)
        }
    }

    pub fn cleanup_dead_processes_with_details(&self) -> Result<Vec<ProcessInfo>> {
        let mut cleaned = Vec::new();
        for entry in fs::read_dir(&self.run_dir)
            .with_context(|| format!("Failed to read run directory: {}", self.run_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();

            if path
                .extension()
                .is_some_and(|ext| ext == PID_FILE_EXT.trim_start_matches('.'))
            {
                if let Some(filename) = path.file_stem() {
                    if let Some(id) = filename.to_str() {
                        if let Ok(info) = self.read_pid(id) {
                            if !info.status.is_active()
                                || (info.status.is_active()
                                    && (!is_process_alive(info.pid)
                                        || !process_identity_matches(&info)))
                            {
                                let _ = fs::remove_file(&path);
                                cleaned.push(info);
                            }
                        }
                    }
                }
            }
        }
        Ok(cleaned)
    }
}

fn is_process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }

    #[cfg(unix)]
    unsafe {
        let result = libc::kill(pid, 0);
        result == 0 || errno() != libc::ESRCH
    }

    #[cfg(windows)]
    {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/FO", "CSV", "/NH"])
            .output();

        let Ok(output) = output else {
            return false;
        };
        if !output.status.success() {
            return false;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let pid_marker = format!(",\"{}\",", pid);
        return stdout.contains(&pid_marker) || stdout.contains(&format!(",\"{}\"", pid));
    }

    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

#[cfg(unix)]
fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn process_identity_matches(info: &ProcessInfo) -> bool {
    runtime_identity_matches(&info.runtime, read_process_commandline(info.pid).as_deref())
}

fn runtime_identity_matches(runtime: &str, commandline: Option<&str>) -> bool {
    if !runtime.eq_ignore_ascii_case("nacelle") {
        return true;
    }

    let Some(commandline) = commandline else {
        return false;
    };

    is_expected_nacelle_commandline(commandline)
}

fn is_expected_nacelle_commandline(commandline: &str) -> bool {
    let normalized = commandline.to_ascii_lowercase();
    normalized.contains("nacelle") || normalized.contains("capsule open")
}

fn read_process_commandline(pid: i32) -> Option<String> {
    if pid <= 0 {
        return None;
    }

    #[cfg(target_os = "linux")]
    {
        let proc_path = format!("/proc/{pid}/cmdline");
        if let Ok(raw) = fs::read(proc_path) {
            if !raw.is_empty() {
                let mut out = String::new();
                for byte in raw {
                    if byte == 0 {
                        out.push(' ');
                    } else {
                        out.push(byte as char);
                    }
                }
                let trimmed = out.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    #[cfg(unix)]
    {
        let output = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let cmd = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if cmd.is_empty() {
            None
        } else {
            Some(cmd)
        }
    }

    #[cfg(windows)]
    {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/FO", "CSV", "/NH"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout.lines().next()?.trim();
        if line.is_empty() || line.starts_with("INFO:") {
            return None;
        }
        let image = line
            .split(',')
            .next()
            .map(|v| v.trim_matches('"'))
            .unwrap_or("")
            .trim();
        if image.is_empty() {
            None
        } else {
            Some(image.to_string())
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

fn wait_for_process_exit(pid: i32, timeout_secs: u64) -> Result<()> {
    let start = std::time::Instant::now();
    while start.elapsed().as_secs() < timeout_secs {
        if !is_process_alive(pid) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    anyhow::bail!(
        "Process {} did not exit within {} seconds",
        pid,
        timeout_secs
    )
}

fn terminate_process(pid: i32, force: bool) -> Result<bool> {
    if pid <= 0 {
        return Ok(false);
    }

    #[cfg(unix)]
    {
        let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
        let result = unsafe { libc::kill(pid, signal) };
        if result == 0 {
            return Ok(true);
        }

        let err = errno();
        if err == libc::ESRCH {
            Ok(false)
        } else {
            Err(anyhow::anyhow!("Failed to send signal to process {}", pid))
        }
    }

    #[cfg(windows)]
    {
        let mut command = Command::new("taskkill");
        command.arg("/PID").arg(pid.to_string());
        if force {
            command.arg("/F");
        }
        let status = command
            .status()
            .with_context(|| format!("Failed to execute taskkill for PID {}", pid))?;

        if status.success() {
            return Ok(true);
        }

        if !is_process_alive(pid) {
            Ok(false)
        } else {
            Err(anyhow::anyhow!("Failed to terminate process {}", pid))
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = force;
        Err(anyhow::anyhow!(
            "Process termination is not supported on this platform"
        ))
    }
}

pub fn get_process_uptime(start_time: SystemTime) -> Result<std::time::Duration> {
    let now = SystemTime::now();
    now.duration_since(start_time)
        .with_context(|| "Process start time is in the future")
}

pub fn format_duration(duration: std::time::Duration) -> String {
    let total_secs = duration.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    if hours > 0 {
        format!("{}h {}m {}s", hours, minutes, seconds)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, seconds)
    } else {
        format!("{}s", seconds)
    }
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new().unwrap_or_else(|_| {
            let run_dir = dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(RUN_DIR);
            Self { run_dir }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_status_display() {
        assert_eq!(ProcessStatus::Starting.to_string(), "starting");
        assert_eq!(ProcessStatus::Ready.to_string(), "ready");
        assert_eq!(ProcessStatus::Running.to_string(), "running");
        assert_eq!(ProcessStatus::Exited.to_string(), "exited");
        assert_eq!(ProcessStatus::Failed.to_string(), "failed");
        assert_eq!(ProcessStatus::Stopped.to_string(), "stopped");
        assert_eq!(ProcessStatus::Unknown.to_string(), "unknown");
    }

    #[test]
    fn test_format_duration() {
        let one_hour = std::time::Duration::from_secs(3661);
        assert_eq!(format_duration(one_hour), "1h 1m 1s");

        let thirty_min = std::time::Duration::from_secs(1800);
        assert_eq!(format_duration(thirty_min), "30m 0s");

        let forty_five_sec = std::time::Duration::from_secs(45);
        assert_eq!(format_duration(forty_five_sec), "45s");

        let zero_sec = std::time::Duration::from_secs(0);
        assert_eq!(format_duration(zero_sec), "0s");
    }

    #[test]
    fn test_process_info_serialization() {
        let info = ProcessInfo {
            id: "test-123".to_string(),
            name: "my-capsule".to_string(),
            pid: 12345,
            workload_pid: Some(12346),
            status: ProcessStatus::Running,
            runtime: "nacelle".to_string(),
            start_time: SystemTime::UNIX_EPOCH,
            manifest_path: Some(PathBuf::from("/path/to/capsule.toml")),
            scoped_id: Some("dev/test".to_string()),
            target_label: Some("default".to_string()),
            requested_port: Some(4310),
            log_path: Some(PathBuf::from("/tmp/test.log")),
            ready_at: Some(SystemTime::UNIX_EPOCH),
            last_event: Some("spawned".to_string()),
            last_error: None,
            exit_code: None,
        };

        let serialized = toml::to_string(&info).expect("Failed to serialize");
        let deserialized: ProcessInfo = toml::from_str(&serialized).expect("Failed to deserialize");

        assert_eq!(info.id, deserialized.id);
        assert_eq!(info.name, deserialized.name);
        assert_eq!(info.pid, deserialized.pid);
        assert_eq!(info.workload_pid, deserialized.workload_pid);
        assert_eq!(info.status, deserialized.status);
        assert_eq!(info.runtime, deserialized.runtime);
        assert_eq!(info.manifest_path, deserialized.manifest_path);
        assert_eq!(info.scoped_id, deserialized.scoped_id);
        assert_eq!(info.target_label, deserialized.target_label);
        assert_eq!(info.requested_port, deserialized.requested_port);
        assert_eq!(info.log_path, deserialized.log_path);
        assert_eq!(info.ready_at, deserialized.ready_at);
        assert_eq!(info.last_event, deserialized.last_event);
        assert_eq!(info.last_error, deserialized.last_error);
        assert_eq!(info.exit_code, deserialized.exit_code);
    }

    #[test]
    fn test_process_info_without_manifest() {
        let info = ProcessInfo {
            id: "test-456".to_string(),
            name: "another-capsule".to_string(),
            pid: 67890,
            workload_pid: None,
            status: ProcessStatus::Stopped,
            runtime: "nacelle".to_string(),
            start_time: SystemTime::UNIX_EPOCH,
            manifest_path: None,
            scoped_id: None,
            target_label: None,
            requested_port: None,
            log_path: None,
            ready_at: None,
            last_event: None,
            last_error: None,
            exit_code: None,
        };

        let serialized = toml::to_string(&info).expect("Failed to serialize");
        let deserialized: ProcessInfo = toml::from_str(&serialized).expect("Failed to deserialize");

        assert_eq!(info.id, deserialized.id);
        assert!(deserialized.manifest_path.is_none());
        assert!(deserialized.requested_port.is_none());
    }

    #[test]
    fn cleanup_scoped_processes_removes_matching_records() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let run_dir = tmp.path().join("run");
        fs::create_dir_all(&run_dir).expect("create run dir");
        let pm = ProcessManager { run_dir };

        let matching = ProcessInfo {
            id: "match-1".to_string(),
            name: "demo".to_string(),
            pid: 0,
            workload_pid: None,
            status: ProcessStatus::Running,
            runtime: "host".to_string(),
            start_time: SystemTime::UNIX_EPOCH,
            manifest_path: None,
            scoped_id: Some("dev/demo".to_string()),
            target_label: None,
            requested_port: None,
            log_path: None,
            ready_at: None,
            last_event: None,
            last_error: None,
            exit_code: None,
        };
        let other = ProcessInfo {
            id: "other-1".to_string(),
            name: "other".to_string(),
            pid: 0,
            workload_pid: None,
            status: ProcessStatus::Stopped,
            runtime: "host".to_string(),
            start_time: SystemTime::UNIX_EPOCH,
            manifest_path: None,
            scoped_id: Some("dev/other".to_string()),
            target_label: None,
            requested_port: None,
            log_path: None,
            ready_at: None,
            last_event: None,
            last_error: None,
            exit_code: None,
        };

        pm.write_pid(&matching).expect("write matching");
        pm.write_pid(&other).expect("write other");

        let cleaned = pm
            .cleanup_scoped_processes("dev/demo", true)
            .expect("cleanup");
        assert_eq!(cleaned, 1);
        assert!(!pm.pid_file_path("match-1").exists());
        assert!(pm.pid_file_path("other-1").exists());
    }

    #[test]
    fn test_pid_file_extension() {
        assert_eq!(PID_FILE_EXT, ".pid");
    }

    #[test]
    fn nacelle_identity_matches_expected_commandline() {
        assert!(runtime_identity_matches(
            "nacelle",
            Some("/usr/local/bin/nacelle run ...")
        ));
        assert!(runtime_identity_matches(
            "nacelle",
            Some("/usr/bin/ato capsule open ./sample")
        ));
        assert!(!runtime_identity_matches(
            "nacelle",
            Some("/usr/sbin/launchd")
        ));
    }

    #[test]
    fn nacelle_identity_fails_closed_when_commandline_missing() {
        assert!(!runtime_identity_matches("nacelle", None));
    }

    #[test]
    fn non_nacelle_runtime_skips_strict_identity_check() {
        assert!(runtime_identity_matches("host", None));
        assert!(runtime_identity_matches(
            "host",
            Some("/usr/bin/python app.py")
        ));
    }
}
