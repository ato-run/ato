//! Best-effort startup sweep for runtime artifacts that are safe to delete
//! only after their owning process is gone.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::debug;

use crate::process::{current_user_owns_process, pid_is_alive, process_start_time_unix_ms};
use crate::record::StoredSessionInfo;
use crate::store::session_root;

const DEFAULT_SOCKET_GRACE: Duration = Duration::from_secs(60);
const DEFAULT_RUN_DIR_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const SWEEP_LOCK_FILE: &str = ".startup-sweep.lock";

#[derive(Debug, Clone)]
pub struct StartupSweepOptions {
    pub run_dir: PathBuf,
    pub runs_dir: PathBuf,
    pub session_root: PathBuf,
    pub now: SystemTime,
    pub socket_grace: Duration,
    pub run_dir_ttl: Duration,
}

impl StartupSweepOptions {
    pub fn from_current_ato_home() -> Result<Self> {
        Ok(Self {
            run_dir: capsule_core::common::paths::ato_path("run")?,
            runs_dir: capsule_core::common::paths::ato_runs_dir(),
            session_root: session_root()?,
            now: SystemTime::now(),
            socket_grace: DEFAULT_SOCKET_GRACE,
            run_dir_ttl: DEFAULT_RUN_DIR_TTL,
        })
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StartupSweepReport {
    pub removed_pid_files: usize,
    pub removed_sockets: usize,
    pub removed_session_records: usize,
    pub removed_run_dirs: usize,
}

pub fn sweep_startup_runtime_artifacts_best_effort() {
    let options = match StartupSweepOptions::from_current_ato_home() {
        Ok(options) => options,
        Err(error) => {
            debug!(error = %error, "skipping startup runtime artifact sweep");
            return;
        }
    };
    if let Err(error) = sweep_startup_runtime_artifacts(&options) {
        debug!(error = %error, "startup runtime artifact sweep failed");
    }
}

pub fn sweep_startup_runtime_artifacts(
    options: &StartupSweepOptions,
) -> Result<StartupSweepReport> {
    fs::create_dir_all(&options.run_dir).with_context(|| {
        format!(
            "failed to create startup sweep run dir {}",
            options.run_dir.display()
        )
    })?;
    let Some(_guard) = SweepLock::try_acquire(&options.run_dir)? else {
        return Ok(StartupSweepReport::default());
    };

    let mut report = StartupSweepReport::default();
    report.removed_pid_files += sweep_pid_files(&options.run_dir)?;
    report.removed_sockets += sweep_socket_files(&options.run_dir, options.now, options.socket_grace)?;
    report.removed_session_records += sweep_session_records(&options.session_root)?;
    report.removed_run_dirs += sweep_run_dirs(&options.runs_dir, options.now, options.run_dir_ttl)?;
    Ok(report)
}

struct SweepLock {
    path: PathBuf,
}

impl SweepLock {
    fn try_acquire(run_dir: &Path) -> Result<Option<Self>> {
        let path = run_dir.join(SWEEP_LOCK_FILE);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => Ok(Some(Self { path })),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(error) => Err(error).with_context(|| {
                format!("failed to acquire startup sweep lock {}", path.display())
            }),
        }
    }
}

impl Drop for SweepLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug, Deserialize)]
struct PidRecord {
    pid: i32,
    #[serde(default)]
    workload_pid: Option<i32>,
    #[serde(default)]
    start_time: Option<SystemTime>,
}

fn sweep_pid_files(run_dir: &Path) -> Result<usize> {
    if !run_dir.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in fs::read_dir(run_dir)
        .with_context(|| format!("failed to read run dir {}", run_dir.display()))?
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                debug!(error = %error, "skipping unreadable startup sweep run entry");
                continue;
            }
        };
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("pid") {
            continue;
        }
        let record = match fs::read_to_string(&path)
            .ok()
            .and_then(|raw| toml::from_str::<PidRecord>(&raw).ok())
        {
            Some(record) => record,
            None => continue,
        };
        if pid_record_is_alive(&record) {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => debug!(path = %path.display(), error = %error, "failed to remove stale pid file"),
        }
    }
    Ok(removed)
}

fn pid_record_is_alive(record: &PidRecord) -> bool {
    let self_pid = std::process::id() as i32;
    if record.pid == self_pid || record.workload_pid == Some(self_pid) {
        return true;
    }
    pid_record_process_is_alive(record.pid, record.start_time)
        || record.workload_pid.is_some_and(pid_i32_is_alive_same_user)
}

fn pid_record_process_is_alive(pid: i32, recorded_start_time: Option<SystemTime>) -> bool {
    let pid = match i32_to_pid(pid) {
        Some(pid) => pid,
        None => return false,
    };
    if !pid_is_alive(pid) || !current_user_owns_process(pid) {
        return false;
    }
    let Some(expected_start_time) = recorded_start_time.and_then(system_time_to_unix_ms) else {
        return false;
    };
    process_start_time_unix_ms(pid).is_some_and(|live_start_time| live_start_time == expected_start_time)
}

fn sweep_socket_files(run_dir: &Path, now: SystemTime, grace: Duration) -> Result<usize> {
    if !run_dir.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in fs::read_dir(run_dir)
        .with_context(|| format!("failed to read run dir {}", run_dir.display()))?
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                debug!(error = %error, "skipping unreadable startup sweep socket entry");
                continue;
            }
        };
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(pid) = parse_desktop_socket_pid(name) else {
            continue;
        };
        if pid == std::process::id() || pid_is_alive(pid) || !path_is_older_than(&path, now, grace) {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => debug!(path = %path.display(), error = %error, "failed to remove stale socket file"),
        }
    }
    Ok(removed)
}

fn parse_desktop_socket_pid(name: &str) -> Option<u32> {
    let stem = name.strip_prefix("ato-desktop-")?;
    let stem = stem
        .strip_suffix(".sock")
        .or_else(|| stem.strip_suffix(".sock.txt"))?;
    stem.parse().ok()
}

fn sweep_session_records(session_root: &Path) -> Result<usize> {
    if !session_root.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in fs::read_dir(session_root)
        .with_context(|| format!("failed to read session root {}", session_root.display()))?
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                debug!(error = %error, "skipping unreadable startup sweep session entry");
                continue;
            }
        };
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let record = match fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<StoredSessionInfo>(&raw).ok())
        {
            Some(record) => record,
            None => continue,
        };
        if session_record_is_alive(&record) {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => {
                removed += 1;
                let log_path = PathBuf::from(record.log_path);
                if log_path.exists() {
                    let _ = fs::remove_file(log_path);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => debug!(path = %path.display(), error = %error, "failed to remove stale session record"),
        }
    }
    Ok(removed)
}

fn session_record_is_alive(record: &StoredSessionInfo) -> bool {
    let pid = match i32_to_pid(record.pid) {
        Some(pid) => pid,
        None => return false,
    };
    if !pid_is_alive(pid) || !current_user_owns_process(pid) {
        return false;
    }
    match record.process_start_time_unix_ms {
        Some(expected_start_time) => {
            process_start_time_unix_ms(pid).is_some_and(|live_start_time| live_start_time == expected_start_time)
        }
        None => true,
    }
}

#[derive(Debug, Deserialize)]
struct RunSessionOwner {
    #[serde(default)]
    pid: Option<i32>,
    #[serde(default)]
    owner_pid: Option<i32>,
}

fn sweep_run_dirs(runs_dir: &Path, now: SystemTime, ttl: Duration) -> Result<usize> {
    if !runs_dir.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in fs::read_dir(runs_dir)
        .with_context(|| format!("failed to read runs dir {}", runs_dir.display()))?
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                debug!(error = %error, "skipping unreadable startup sweep run dir entry");
                continue;
            }
        };
        let path = entry.path();
        let is_run_dir = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("run-"));
        if !is_run_dir || !path.is_dir() || !path_is_older_than(&path, now, ttl) {
            continue;
        }
        // Preserve ambiguous or legacy run dirs when ownership cannot be
        // reconstructed from session.json instead of guessing.
        let Some(owner) = read_run_dir_owner(&path) else {
            continue;
        };
        if owner_is_alive(owner) {
            continue;
        }
        match fs::remove_dir_all(&path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => debug!(path = %path.display(), error = %error, "failed to remove stale run dir"),
        }
    }
    Ok(removed)
}

fn read_run_dir_owner(path: &Path) -> Option<RunSessionOwner> {
    let raw = fs::read_to_string(path.join("session.json")).ok()?;
    serde_json::from_str(&raw).ok()
}

fn owner_is_alive(owner: RunSessionOwner) -> bool {
    owner.pid.is_some_and(pid_i32_is_alive) || owner.owner_pid.is_some_and(pid_i32_is_alive)
}

fn pid_i32_is_alive(pid: i32) -> bool {
    i32_to_pid(pid).is_some_and(pid_is_alive)
}

fn pid_i32_is_alive_same_user(pid: i32) -> bool {
    i32_to_pid(pid)
        .filter(|pid| current_user_owns_process(*pid))
        .is_some_and(pid_is_alive)
}

fn i32_to_pid(pid: i32) -> Option<u32> {
    (pid > 0).then_some(pid as u32)
}

fn system_time_to_unix_ms(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

fn path_is_older_than(path: &Path, now: SystemTime, duration: Duration) -> bool {
    let Some(modified) = path.metadata().and_then(|metadata| metadata.modified()).ok() else {
        return false;
    };
    now.duration_since(modified)
        .map(|age| age >= duration)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_wire::handle::{CapsuleDisplayStrategy, CapsuleRuntimeDescriptor, TrustState};
    use tempfile::tempdir;

    fn options(root: &Path) -> StartupSweepOptions {
        StartupSweepOptions {
            run_dir: root.join("run"),
            runs_dir: root.join("runs"),
            session_root: root.join("sessions"),
            now: SystemTime::now() + Duration::from_secs(48 * 60 * 60),
            socket_grace: Duration::from_secs(60),
            run_dir_ttl: Duration::from_secs(24 * 60 * 60),
        }
    }

    fn write_pid(path: &Path, pid: i32) {
        #[derive(serde::Serialize)]
        struct PidRecordFixture {
            pid: i32,
            workload_pid: Option<i32>,
            start_time: SystemTime,
        }

        let payload = toml::to_string(&PidRecordFixture {
            pid,
            workload_pid: None,
            start_time: SystemTime::UNIX_EPOCH,
        })
        .expect("serialize pid file");
        fs::write(path, payload).expect("write pid file");
    }

    #[test]
    fn sweep_removes_dead_pid_and_socket_files() {
        let temp = tempdir().expect("tempdir");
        let options = options(temp.path());
        fs::create_dir_all(&options.run_dir).expect("run dir");
        let pid_path = options.run_dir.join("dead.pid");
        let sock_path = options.run_dir.join("ato-desktop-999999999.sock");
        let sock_txt_path = options.run_dir.join("ato-desktop-999999999.sock.txt");
        write_pid(&pid_path, 999_999_999);
        fs::write(&sock_path, "").expect("sock");
        fs::write(&sock_txt_path, "").expect("sock txt");

        let report = sweep_startup_runtime_artifacts(&options).expect("sweep");

        assert_eq!(report.removed_pid_files, 1);
        assert_eq!(report.removed_sockets, 2);
        assert!(!pid_path.exists());
        assert!(!sock_path.exists());
        assert!(!sock_txt_path.exists());
    }

    #[test]
    fn sweep_preserves_live_pid_and_socket_files() {
        let temp = tempdir().expect("tempdir");
        let options = options(temp.path());
        fs::create_dir_all(&options.run_dir).expect("run dir");
        let self_pid = std::process::id();
        let pid_path = options.run_dir.join("live.pid");
        let sock_path = options.run_dir.join(format!("ato-desktop-{self_pid}.sock"));
        write_pid(&pid_path, self_pid as i32);
        fs::write(&sock_path, "").expect("sock");

        let report = sweep_startup_runtime_artifacts(&options).expect("sweep");

        assert_eq!(report.removed_pid_files, 0);
        assert_eq!(report.removed_sockets, 0);
        assert!(pid_path.exists());
        assert!(sock_path.exists());
    }

    #[test]
    fn sweep_removes_pid_file_when_start_time_mismatches_live_process() {
        #[derive(serde::Serialize)]
        struct PidRecordFixture {
            pid: i32,
            workload_pid: Option<i32>,
            start_time: SystemTime,
        }

        let temp = tempdir().expect("tempdir");
        let options = options(temp.path());
        fs::create_dir_all(&options.run_dir).expect("run dir");
        let pid_path = options.run_dir.join("reused.pid");
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 60"])
            .spawn()
            .expect("spawn child");
        let payload = toml::to_string(&PidRecordFixture {
            pid: child.id() as i32,
            workload_pid: None,
            start_time: SystemTime::UNIX_EPOCH,
        })
        .expect("serialize pid file");
        fs::write(&pid_path, payload).expect("write pid file");

        let report = sweep_startup_runtime_artifacts(&options).expect("sweep");
        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(report.removed_pid_files, 1);
        assert!(!pid_path.exists());
    }

    #[test]
    fn sweep_preserves_fresh_dead_socket_within_grace_period() {
        let temp = tempdir().expect("tempdir");
        let mut options = options(temp.path());
        options.now = SystemTime::now();
        fs::create_dir_all(&options.run_dir).expect("run dir");
        let sock_path = options.run_dir.join("ato-desktop-999999999.sock");
        fs::write(&sock_path, "").expect("sock");

        let report = sweep_startup_runtime_artifacts(&options).expect("sweep");

        assert_eq!(report.removed_sockets, 0);
        assert!(sock_path.exists());
    }

    #[test]
    fn sweep_removes_stale_session_record_and_log() {
        let temp = tempdir().expect("tempdir");
        let options = options(temp.path());
        fs::create_dir_all(&options.session_root).expect("session root");
        let log_path = temp.path().join("dead.log");
        fs::write(&log_path, "log").expect("log");
        let record = StoredSessionInfo {
            session_id: "ato-desktop-session-dead".to_string(),
            handle: "capsule://example/app".to_string(),
            normalized_handle: "capsule://example/app".to_string(),
            canonical_handle: None,
            trust_state: TrustState::Trusted,
            source: None,
            restricted: false,
            snapshot: None,
            runtime: CapsuleRuntimeDescriptor {
                target_label: "default".to_string(),
                runtime: Some("source".to_string()),
                driver: None,
                language: None,
                port: None,
            },
            display_strategy: CapsuleDisplayStrategy::TerminalStream,
            pid: 999_999_999,
            log_path: log_path.display().to_string(),
            manifest_path: "capsule.toml".to_string(),
            target_label: "default".to_string(),
            notes: Vec::new(),
            guest: None,
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: None,
            schema_version: None,
            launch_digest: None,
            process_start_time_unix_ms: None,
        };
        let record_path = options.session_root.join("ato-desktop-session-dead.json");
        fs::write(&record_path, serde_json::to_vec(&record).expect("record")).expect("write");

        let report = sweep_startup_runtime_artifacts(&options).expect("sweep");

        assert_eq!(report.removed_session_records, 1);
        assert!(!record_path.exists());
        assert!(!log_path.exists());
    }

    #[test]
    fn sweep_removes_stale_run_dir_with_dead_owner_only() {
        let temp = tempdir().expect("tempdir");
        let options = options(temp.path());
        fs::create_dir_all(&options.runs_dir).expect("runs dir");
        let dead_run = options.runs_dir.join("run-dead");
        let ambiguous_run = options.runs_dir.join("run-ambiguous");
        fs::create_dir_all(&dead_run).expect("dead run");
        fs::create_dir_all(&ambiguous_run).expect("ambiguous run");
        fs::write(dead_run.join("session.json"), r#"{"pid":999999999}"#).expect("session");

        let report = sweep_startup_runtime_artifacts(&options).expect("sweep");

        assert_eq!(report.removed_run_dirs, 1);
        assert!(!dead_run.exists());
        assert!(ambiguous_run.exists());
    }
}