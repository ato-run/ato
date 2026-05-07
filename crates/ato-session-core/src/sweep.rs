//! Best-effort startup sweep for runtime artifacts that are safe to delete
//! only after their owning process is gone.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::debug;

use crate::process::{current_user_owns_process, pid_is_alive, process_start_time_unix_ms};
use crate::record::StoredSessionInfo;
use crate::store::session_root;

const DEFAULT_SOCKET_GRACE: Duration = Duration::from_secs(60);
const DEFAULT_RUN_DIR_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Fallback retention for `runs/run-*/` whose `session.json` cannot be
/// reconstructed (missing, unreadable, or unparseable). Without ownership we
/// cannot verify liveness, so we hold the dir until it is unambiguously old
/// enough to be a leak rather than an in-flight write.
const RUN_DIR_LEGACY_TTL_MULTIPLIER: u32 = 2;
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
        debug!(
            run_dir = %options.run_dir.display(),
            "startup runtime artifact sweep skipped: another process holds the sweep lock"
        );
        return Ok(StartupSweepReport::default());
    };

    let mut report = StartupSweepReport::default();
    // Order matters: socket_files needs to see the matching
    // `ato-desktop-session-<pid>.pid` records BEFORE pid_files removes
    // start-time-mismatched (PID-reuse imposter) ones. Without this
    // ordering, `sweep_socket_files` would see "record missing" for
    // both legitimate cases (live ato-desktop never wrote a session
    // record under its own PID) and imposter cases (record was just
    // removed by pid_files), forcing one of them to misbehave. See
    // the regression test
    // `sweep_preserves_socket_for_live_pid_without_matching_session_record`
    // for the live-desktop scenario surfaced by #92 verification.
    report.removed_sockets +=
        sweep_socket_files(&options.run_dir, options.now, options.socket_grace)?;
    report.removed_pid_files += sweep_pid_files(&options.run_dir)?;
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
    /// OS-reported start time (`process_start_time_unix_ms`) of `pid`,
    /// captured at registration. Compared against a fresh query to defeat
    /// PID reuse. Absent on legacy records and on platforms where the OS
    /// query is unsupported; in both cases the sweep falls back to
    /// "alive AND owned by current user", which keeps liveness intact at
    /// the cost of weakened reuse defense for that record.
    #[serde(default)]
    os_start_time_unix_ms: Option<u64>,
    /// Same shape as `os_start_time_unix_ms` but for `workload_pid`. Lets
    /// the workload arm of the liveness check apply the same PID-reuse
    /// defense as the main `pid` arm instead of accepting any live owner
    /// of the recorded numeric `workload_pid`.
    #[serde(default)]
    workload_os_start_time_unix_ms: Option<u64>,
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
            Err(error) => {
                debug!(path = %path.display(), error = %error, "failed to remove stale pid file")
            }
        }
    }
    Ok(removed)
}

fn pid_record_is_alive(record: &PidRecord) -> bool {
    let self_pid = std::process::id() as i32;
    if record.pid == self_pid || record.workload_pid == Some(self_pid) {
        return true;
    }
    pid_record_process_is_alive(record.pid, record.os_start_time_unix_ms)
        || record.workload_pid.is_some_and(|workload_pid| {
            pid_record_process_is_alive(workload_pid, record.workload_os_start_time_unix_ms)
        })
}

fn pid_record_process_is_alive(pid: i32, recorded_start_time_ms: Option<u64>) -> bool {
    let pid = match i32_to_pid(pid) {
        Some(pid) => pid,
        None => return false,
    };
    if !pid_is_alive(pid) || !current_user_owns_process(pid) {
        return false;
    }
    // Legacy record (no recorded start_time) or platform without OS-query
    // support: keep the record so we don't delete a live process's pid
    // file. PID reuse risk is accepted for these transitional records.
    let Some(expected_start_time) = recorded_start_time_ms else {
        return true;
    };
    match process_start_time_unix_ms(pid) {
        Some(live_start_time) => live_start_time == expected_start_time,
        // OS query failed for this PID even though it's alive — treat as
        // mismatched (fail-closed) so a stale record doesn't get pinned by
        // a transient query failure when start_time is recorded.
        None => false,
    }
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
        if pid == std::process::id() {
            continue;
        }
        // Cross-reference with the matching `ato-desktop-session-<pid>.pid`
        // record so PID reuse cannot pin a stale socket: if the recorded
        // start_time disagrees with the live process's start_time the
        // record is treated as dead and the socket falls through to the
        // grace check.
        if matching_pid_record_is_alive(run_dir, pid) {
            continue;
        }
        // Two distinct fall-through cases reach this point:
        //
        // 1. A `ato-desktop-session-<pid>.pid` record exists but the
        //    recorded start_time mismatches — that's a PID-reuse imposter,
        //    and the socket really is stale. Keep the original behaviour
        //    (fall through to the grace check below).
        // 2. NO record exists for this pid. The original v0.5.0 sweep (#85)
        //    treated this as orphan, but that reaped the live ato-desktop's
        //    own automation socket on every `ato session start`: the
        //    desktop binds `ato-desktop-<pid>.sock` itself, while the
        //    session pid file is written by the spawned CLI under a
        //    *different* PID — so the desktop's socket never has a
        //    matching record. Defensively, if no record exists AND the
        //    bare PID is alive AND owned by the current user, preserve
        //    the socket. PID reuse is still defended against because
        //    `current_user_owns_process` rejects sockets reused by
        //    another user, and the (record-exists) imposter case still
        //    falls through above.
        let session_record_path = run_dir.join(format!("ato-desktop-session-{pid}.pid"));
        if !session_record_path.exists()
            && pid_is_alive(pid)
            && current_user_owns_process(pid)
        {
            continue;
        }
        if !path_is_older_than(&path, now, grace) {
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                debug!(path = %path.display(), error = %error, "failed to remove stale socket file")
            }
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

fn matching_pid_record_is_alive(run_dir: &Path, pid: u32) -> bool {
    let record_path = run_dir.join(format!("ato-desktop-session-{pid}.pid"));
    let raw = match fs::read_to_string(&record_path) {
        Ok(raw) => raw,
        Err(_) => return false,
    };
    let record: PidRecord = match toml::from_str(&raw) {
        Ok(record) => record,
        Err(_) => return false,
    };
    pid_record_is_alive(&record)
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
            Err(error) => {
                debug!(path = %path.display(), error = %error, "failed to remove stale session record")
            }
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
        Some(expected_start_time) => process_start_time_unix_ms(pid)
            .is_some_and(|live_start_time| live_start_time == expected_start_time),
        None => true,
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
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
        match read_run_dir_owner(&path) {
            Some(owner) if owner_is_alive(owner) => continue,
            Some(_) => {} // owner identified and dead → fall through to remove
            None => {
                // Ambiguous (missing or unparseable session.json). Hold
                // until 2× ttl so an in-flight write isn't sniped, but
                // don't keep forever — a corrupted session.json should not
                // pin a leaked workspace indefinitely.
                let legacy_ttl = ttl.saturating_mul(RUN_DIR_LEGACY_TTL_MULTIPLIER);
                if !path_is_older_than(&path, now, legacy_ttl) {
                    continue;
                }
            }
        }
        match fs::remove_dir_all(&path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                debug!(path = %path.display(), error = %error, "failed to remove stale run dir")
            }
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

fn i32_to_pid(pid: i32) -> Option<u32> {
    (pid > 0).then_some(pid as u32)
}

fn path_is_older_than(path: &Path, now: SystemTime, duration: Duration) -> bool {
    let Some(modified) = path
        .metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
    else {
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

    #[derive(serde::Serialize)]
    struct PidRecordFixture {
        pid: i32,
        workload_pid: Option<i32>,
        os_start_time_unix_ms: Option<u64>,
        workload_os_start_time_unix_ms: Option<u64>,
    }

    fn write_pid(path: &Path, pid: i32) {
        let payload = toml::to_string(&PidRecordFixture {
            pid,
            workload_pid: None,
            os_start_time_unix_ms: None,
            workload_os_start_time_unix_ms: None,
        })
        .expect("serialize pid file");
        fs::write(path, payload).expect("write pid file");
    }

    fn write_pid_with_os_start(
        path: &Path,
        pid: i32,
        os_start_time_unix_ms: Option<u64>,
        workload_pid: Option<i32>,
        workload_os_start_time_unix_ms: Option<u64>,
    ) {
        let payload = toml::to_string(&PidRecordFixture {
            pid,
            workload_pid,
            os_start_time_unix_ms,
            workload_os_start_time_unix_ms,
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
        let temp = tempdir().expect("tempdir");
        let options = options(temp.path());
        fs::create_dir_all(&options.run_dir).expect("run dir");
        let pid_path = options.run_dir.join("reused.pid");
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 60"])
            .spawn()
            .expect("spawn child");
        // Stamp a recorded start_time that cannot match the live child's
        // (mtime=1ms is unambiguously stale relative to any spawn). On
        // platforms without OS start_time support this falls back to
        // "alive + same user" which would *keep* the record — that's
        // intentional fail-open for legacy/unsupported platforms.
        write_pid_with_os_start(&pid_path, child.id() as i32, Some(1), None, None);

        let report = sweep_startup_runtime_artifacts(&options).expect("sweep");
        let _ = child.kill();
        let _ = child.wait();

        // Only platforms with start_time support exercise this path.
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            assert_eq!(report.removed_pid_files, 1);
            assert!(!pid_path.exists());
        }
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn sweep_keeps_live_non_self_pid_with_matching_os_start_time() {
        let temp = tempdir().expect("tempdir");
        let options = options(temp.path());
        fs::create_dir_all(&options.run_dir).expect("run dir");
        let pid_path = options.run_dir.join("live-non-self.pid");
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 60"])
            .spawn()
            .expect("spawn child");
        // Allow the OS to register the child before querying its start_time.
        std::thread::sleep(Duration::from_millis(50));
        let live_start =
            process_start_time_unix_ms(child.id()).expect("os start_time available on macOS/Linux");
        write_pid_with_os_start(&pid_path, child.id() as i32, Some(live_start), None, None);

        let report = sweep_startup_runtime_artifacts(&options).expect("sweep");
        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(report.removed_pid_files, 0);
        assert!(pid_path.exists());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn sweep_removes_pid_file_when_workload_start_time_mismatches() {
        let temp = tempdir().expect("tempdir");
        let options = options(temp.path());
        fs::create_dir_all(&options.run_dir).expect("run dir");
        let pid_path = options.run_dir.join("workload-reused.pid");
        let self_pid = std::process::id();
        let mut workload = std::process::Command::new("sh")
            .args(["-c", "sleep 60"])
            .spawn()
            .expect("spawn workload");
        // Main pid: a clearly dead PID so it cannot save the record.
        // Workload pid: a real live process but with a bogus start_time so
        // the workload arm of pid_record_is_alive treats it as reused.
        write_pid_with_os_start(
            &pid_path,
            999_999_999,
            Some(0),
            Some(workload.id() as i32),
            Some(1),
        );
        // Sanity: the workload PID *is* alive, so the test exercises the
        // start_time mismatch path, not the dead-pid short-circuit.
        assert!(pid_is_alive(workload.id()));
        assert_ne!(workload.id(), self_pid);

        let report = sweep_startup_runtime_artifacts(&options).expect("sweep");
        let _ = workload.kill();
        let _ = workload.wait();

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

    /// Regression for the v0.5.0 #85 bug surfaced by #92 verification:
    /// the desktop's automation socket `ato-desktop-<pid>.sock` was reaped
    /// by the CLI's session-start sweep because no
    /// `ato-desktop-session-<pid>.pid` record exists for the desktop's
    /// own PID — sessions are spawned by the CLI under a different PID,
    /// so `matching_pid_record_is_alive` returned false for the live
    /// desktop and the socket fell through to the grace-check removal.
    ///
    /// Pre-fix, this test would remove the socket after the grace
    /// window. Post-fix, the bare `pid_is_alive(pid)` defensive check
    /// preserves it.
    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn sweep_preserves_socket_for_live_pid_without_matching_session_record() {
        let temp = tempdir().expect("tempdir");
        let mut options = options(temp.path());
        // Push `now` past the grace window so the only thing keeping the
        // socket alive is the bare-pid defense added in this fix.
        options.now = SystemTime::now() + Duration::from_secs(3_600);
        fs::create_dir_all(&options.run_dir).expect("run dir");

        // Spawn a real workload — its PID is alive and owned by the
        // current user, but no `ato-desktop-session-<pid>.pid` record
        // exists for it (matching the desktop-binds-socket-but-no-
        // session-yet shape from production).
        let mut workload = std::process::Command::new("sh")
            .args(["-c", "sleep 60"])
            .spawn()
            .expect("spawn workload");
        let live_pid = workload.id();
        let sock_path = options.run_dir.join(format!("ato-desktop-{live_pid}.sock"));
        fs::write(&sock_path, "").expect("sock");

        let report = sweep_startup_runtime_artifacts(&options).expect("sweep");
        let _ = workload.kill();
        let _ = workload.wait();

        assert_eq!(
            report.removed_sockets, 0,
            "live-PID socket without session record must NOT be reaped"
        );
        assert!(
            sock_path.exists(),
            "socket file must survive when its owner is alive"
        );
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
            orchestration_services: None,
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
        // now is +25h: past 1× ttl (so dead-owner runs sweep) but within
        // 2× ttl (so ambiguous runs are still preserved).
        let mut options = options(temp.path());
        options.now = SystemTime::now() + Duration::from_secs(25 * 60 * 60);
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

    #[test]
    fn sweep_removes_legacy_run_dir_after_double_ttl() {
        let temp = tempdir().expect("tempdir");
        let mut options = options(temp.path());
        // 49h = past 2× ttl (48h) with margin so the dir's real-time
        // mtime jitter doesn't push it below the threshold.
        options.now = SystemTime::now() + Duration::from_secs(49 * 60 * 60);
        fs::create_dir_all(&options.runs_dir).expect("runs dir");
        let legacy_run = options.runs_dir.join("run-legacy");
        fs::create_dir_all(&legacy_run).expect("legacy run");
        // No session.json — ambiguous owner. Past 2× ttl → swept.

        let report = sweep_startup_runtime_artifacts(&options).expect("sweep");

        assert_eq!(report.removed_run_dirs, 1);
        assert!(!legacy_run.exists());
    }

    #[test]
    fn sweep_keeps_legacy_run_dir_within_double_ttl() {
        let temp = tempdir().expect("tempdir");
        let mut options = options(temp.path());
        // 30h: past 1× ttl but inside 2× ttl (48h).
        options.now = SystemTime::now() + Duration::from_secs(30 * 60 * 60);
        fs::create_dir_all(&options.runs_dir).expect("runs dir");
        let legacy_run = options.runs_dir.join("run-legacy-young");
        fs::create_dir_all(&legacy_run).expect("legacy run");

        let report = sweep_startup_runtime_artifacts(&options).expect("sweep");

        assert_eq!(report.removed_run_dirs, 0);
        assert!(legacy_run.exists());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn sweep_removes_socket_when_pid_record_has_mismatched_start_time() {
        // Simulates PID reuse: socket and matching .pid record name PID
        // 999_999_999 (currently dead). Live socket-bound process check
        // would have considered it dead and the grace check would have
        // taken over; here we make sure the cross-reference does not
        // resurrect the socket via `pid_is_alive` against a reused PID.
        let temp = tempdir().expect("tempdir");
        let options = options(temp.path());
        fs::create_dir_all(&options.run_dir).expect("run dir");
        let pid_path = options.run_dir.join("ato-desktop-session-999999999.pid");
        let sock_path = options.run_dir.join("ato-desktop-999999999.sock");
        write_pid_with_os_start(&pid_path, 999_999_999, Some(1), None, None);
        fs::write(&sock_path, "").expect("sock");

        let report = sweep_startup_runtime_artifacts(&options).expect("sweep");

        // The .pid is dead (pid 999_999_999 isn't alive) → swept.
        // The socket falls through to grace and is also old enough → swept.
        assert!(report.removed_pid_files >= 1);
        assert_eq!(report.removed_sockets, 1);
        assert!(!pid_path.exists());
        assert!(!sock_path.exists());
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn sweep_removes_socket_when_matching_pid_record_is_a_pid_reuse_imposter() {
        // Live OS process P holds PID X. A stale .pid record names PID X
        // with a wrong recorded start_time (PID reuse imposter). Socket
        // for PID X should NOT be saved by `pid_is_alive(X)` because the
        // matching .pid record's start_time check fails.
        let temp = tempdir().expect("tempdir");
        let options = options(temp.path());
        fs::create_dir_all(&options.run_dir).expect("run dir");
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 60"])
            .spawn()
            .expect("spawn child");
        let pid = child.id();
        let pid_path = options
            .run_dir
            .join(format!("ato-desktop-session-{pid}.pid"));
        let sock_path = options.run_dir.join(format!("ato-desktop-{pid}.sock"));
        // Recorded os_start_time = 1ms (clearly different from live).
        write_pid_with_os_start(&pid_path, pid as i32, Some(1), None, None);
        fs::write(&sock_path, "").expect("sock");

        let report = sweep_startup_runtime_artifacts(&options).expect("sweep");
        let _ = child.kill();
        let _ = child.wait();

        // .pid is treated as dead by start_time mismatch → swept.
        // socket loses its cross-reference → falls to grace → swept.
        assert_eq!(report.removed_sockets, 1);
        assert!(!sock_path.exists());
    }

    #[test]
    fn sweep_removes_orphan_socket_without_matching_pid_record_after_grace() {
        // No matching ato-desktop-session-<pid>.pid record. Socket falls
        // through to grace check; with `now = +48h` it is unambiguously
        // older than grace (60s) so it gets swept.
        let temp = tempdir().expect("tempdir");
        let options = options(temp.path());
        fs::create_dir_all(&options.run_dir).expect("run dir");
        let sock_path = options.run_dir.join("ato-desktop-999999999.sock");
        fs::write(&sock_path, "").expect("sock");

        let report = sweep_startup_runtime_artifacts(&options).expect("sweep");

        assert_eq!(report.removed_sockets, 1);
        assert!(!sock_path.exists());
    }
}
