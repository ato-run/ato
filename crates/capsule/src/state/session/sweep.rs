//! Best-effort startup sweep for runtime artifacts that are safe to delete
//! only after their owning process is gone.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::debug;

use crate::state::session::process::{
    current_user_owns_process, oci_container_is_running, pid_is_alive, process_start_time_unix_ms,
};
use crate::state::session::record::StoredSessionInfo;
use crate::state::session::store::session_root;

const DEFAULT_SOCKET_GRACE: Duration = Duration::from_secs(60);
const DEFAULT_RUN_DIR_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Guest log files (`sessions/<id>.log`, `run/engine-*.log`) older than this
/// are reclaimed by the sweep regardless of how large they are. #767.
const DEFAULT_LOG_TTL: Duration = Duration::from_secs(14 * 24 * 60 * 60);
/// Aggregate byte budget for retained guest logs of one kind (session logs and
/// engine logs are budgeted independently). When the surviving (younger than
/// the TTL) logs of a kind exceed this, the oldest are deleted first until the
/// total fits. 512 MiB. #767.
const DEFAULT_LOG_SIZE_BUDGET: u64 = 512 * 1024 * 1024;
/// Fallback retention for `runs/run-*/` whose `session.json` cannot be
/// reconstructed (missing, unreadable, or unparseable). Without ownership we
/// cannot verify liveness, so we hold the dir until it is unambiguously old
/// enough to be a leak rather than an in-flight write.
const RUN_DIR_LEGACY_TTL_MULTIPLIER: u32 = 2;
/// Maximum number of rotated guest-log generations the spawn-time rotator
/// produces (`<log>.1` .. `<log>.N`). Kept in sync with
/// `cli::adapters::runtime::executors::log_rotation::LOG_ROTATE_MAX_GENERATIONS`
/// (the `cli` crate depends on `capsule`, so we cannot import it here without a
/// dependency cycle). The TTL/size sweep matches any numeric `.N` suffix
/// regardless of this value, so the two need not be byte-identical — this is the
/// expected upper bound, not a hard limit. #767.
// Referenced only by the candidate-matcher test (the production matchers accept
// any numeric `.N` suffix); kept as the documented sync anchor regardless.
#[cfg_attr(not(test), allow(dead_code))]
const LOG_ROTATE_MAX_GENERATIONS: u32 = 3;
const SWEEP_LOCK_FILE: &str = ".startup-sweep.lock";
const SWEEP_STAMP_FILE: &str = ".startup-sweep.stamp";
/// How long a completed sweep suppresses the next one. The sweep only
/// reclaims artifacts whose owning process is already gone, so re-running it
/// on every single `ato` invocation within a few seconds of the last sweep is
/// pure waste — the artifact set cannot have meaningfully changed. Each run
/// can spawn a blocking `podman inspect` (or `tasklist` on Windows) per OCI
/// orchestration record, so a desktop shelling out to `ato` repeatedly pays
/// that cost on every call. Throttling collapses bursts of invocations to a
/// single sweep while keeping the work itself byte-for-byte identical when it
/// does run.
const DEFAULT_SWEEP_THROTTLE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct StartupSweepOptions {
    pub run_dir: PathBuf,
    pub runs_dir: PathBuf,
    pub session_root: PathBuf,
    pub now: SystemTime,
    pub socket_grace: Duration,
    pub run_dir_ttl: Duration,
    /// Age beyond which a guest log file is reclaimed unconditionally.
    pub log_ttl: Duration,
    /// Aggregate byte budget per guest-log kind (oldest-first eviction).
    pub log_size_budget: u64,
}

impl StartupSweepOptions {
    pub fn from_current_ato_home() -> Result<Self> {
        Ok(Self {
            run_dir: crate::common::paths::ato_path("run")?,
            runs_dir: crate::common::paths::ato_runs_dir(),
            session_root: session_root()?,
            now: SystemTime::now(),
            socket_grace: DEFAULT_SOCKET_GRACE,
            run_dir_ttl: DEFAULT_RUN_DIR_TTL,
            log_ttl: DEFAULT_LOG_TTL,
            log_size_budget: DEFAULT_LOG_SIZE_BUDGET,
        })
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StartupSweepReport {
    pub removed_pid_files: usize,
    pub removed_sockets: usize,
    pub removed_session_records: usize,
    pub removed_run_dirs: usize,
    /// Stale guest log files reclaimed by TTL/size sweep (session + engine).
    pub removed_log_files: usize,
}

pub fn sweep_startup_runtime_artifacts_best_effort() {
    let options = match StartupSweepOptions::from_current_ato_home() {
        Ok(options) => options,
        Err(error) => {
            debug!(error = %error, "skipping startup runtime artifact sweep");
            return;
        }
    };
    // Throttle: if a sweep completed within the throttle window, skip this
    // invocation entirely. This keeps the per-`ato`-call cost (including the
    // blocking OCI liveness probes) off the common path when invocations come
    // in bursts. The marker lives next to the sweep lock in `run_dir`.
    if sweep_recently_completed(&options.run_dir, options.now, DEFAULT_SWEEP_THROTTLE) {
        debug!(
            run_dir = %options.run_dir.display(),
            "startup runtime artifact sweep throttled: a recent sweep is still fresh"
        );
        return;
    }
    match sweep_startup_runtime_artifacts(&options) {
        Ok(_) => mark_sweep_completed(&options.run_dir, options.now),
        Err(error) => {
            debug!(error = %error, "startup runtime artifact sweep failed");
        }
    }
}

/// Returns `true` when a sweep stamp exists in `run_dir` recording a
/// completion time less than `throttle` before `now`.
///
/// The stamp stores the completion time as UNIX-epoch milliseconds in its
/// contents (rather than relying on filesystem mtime, which std cannot set to
/// a synthetic clock and which varies in resolution across platforms). A
/// missing, unreadable, or unparseable stamp — or a stamp dated in the future
/// relative to `now` (clock rollback) — is treated as "not fresh" so the sweep
/// runs. We always prefer a redundant sweep over silently skipping artifact
/// reclamation.
fn sweep_recently_completed(run_dir: &Path, now: SystemTime, throttle: Duration) -> bool {
    let stamp = run_dir.join(SWEEP_STAMP_FILE);
    let Ok(contents) = fs::read_to_string(&stamp) else {
        return false;
    };
    let Some(recorded) = parse_stamp_millis(&contents) else {
        return false;
    };
    let Some(now_millis) = system_time_to_unix_millis(now) else {
        return false;
    };
    // `now` before the recorded completion (clock moved backwards): not fresh.
    now_millis
        .checked_sub(recorded)
        .map(|elapsed_ms| Duration::from_millis(elapsed_ms) < throttle)
        .unwrap_or(false)
}

/// Records that a sweep just completed by writing the completion time
/// (`now`, as UNIX-epoch milliseconds) into the stamp file. Best-effort: a
/// failure here only means the next invocation re-runs the sweep, which is
/// harmless.
fn mark_sweep_completed(run_dir: &Path, now: SystemTime) {
    let Some(now_millis) = system_time_to_unix_millis(now) else {
        return;
    };
    let stamp = run_dir.join(SWEEP_STAMP_FILE);
    if let Err(error) = fs::write(&stamp, now_millis.to_string()) {
        debug!(error = %error, stamp = %stamp.display(), "failed to write startup sweep stamp");
    }
}

fn parse_stamp_millis(contents: &str) -> Option<u64> {
    contents.trim().parse::<u64>().ok()
}

fn system_time_to_unix_millis(time: SystemTime) -> Option<u64> {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
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
    // TTL + aggregate-size reclamation for OLD guest logs, independent of
    // process liveness (#767). Session logs (`sessions/<id>.log`) and engine
    // logs (`run/engine-*.log`) are each budgeted separately so a flood of one
    // kind cannot evict the other.
    report.removed_log_files += sweep_session_log_files(
        &options.session_root,
        options.now,
        options.log_ttl,
        options.log_size_budget,
    )?;
    report.removed_log_files += sweep_engine_log_files(
        &options.run_dir,
        options.now,
        options.log_ttl,
        options.log_size_budget,
    )?;
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
        if !session_record_path.exists() && pid_is_alive(pid) && current_user_owns_process(pid) {
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

/// A record is "alive" — and therefore must be retained by the startup
/// sweep — if **any** of the processes it points to is still running.
///
/// The legacy check used `record.pid` only, which is the wrapper / leaf
/// pid recorded at session start. For in-process orchestration sessions
/// (#73 PR-C), that pid is often a shell wrapper (`npm run dev`,
/// `uv run`) that has since exited — leaving its child (vite, uvicorn)
/// as an orphan still bound to the leaf port. Using only `record.pid`
/// caused those records to be swept here, hiding the still-running
/// services from `ato app session stop` and breaking #108.
///
/// We now consider the record alive if any of these is true:
///   1. `record.pid` is alive and ours (legacy invariant).
///   2. Any `orchestration_services.services[*].local_pid` is alive
///      and ours (#73 PR-D persists these).
///   3. Any `dependency_contracts.providers[*].pid` is alive and ours.
///
/// `current_user_owns_process` is required so a re-used pid belonging to
/// a different user cannot keep a stale record pinned. The
/// `process_start_time_unix_ms` cross-check is preserved on the legacy
/// `record.pid` path because it's the only pid for which we record a
/// start time. For the auxiliary pids (orchestration / dep contracts)
/// we accept "alive + ours" without start-time validation — losing one
/// teardown pass to a pid-reuse coincidence is preferable to losing the
/// teardown pass that would actually kill an orphan listener.
fn session_record_is_alive(record: &StoredSessionInfo) -> bool {
    if let Some(pid) = i32_to_pid(record.pid)
        && pid_is_alive(pid)
        && current_user_owns_process(pid)
    {
        let start_time_matches = match record.process_start_time_unix_ms {
            Some(expected) => process_start_time_unix_ms(pid).is_some_and(|live| live == expected),
            None => true,
        };
        if start_time_matches {
            return true;
        }
    }
    if let Some(snapshot) = record.orchestration_services.as_ref() {
        for service in &snapshot.services {
            // OCI services: addressed by container_id, not local_pid.
            if let Some(container_id) = service.container_id.as_deref()
                && oci_container_is_running(container_id)
            {
                return true;
            }
            // Managed (non-OCI) services: addressed by local_pid.
            if let Some(pid) = service.local_pid.and_then(i32_to_pid)
                && pid_is_alive(pid)
                && current_user_owns_process(pid)
            {
                return true;
            }
        }
    }
    if let Some(snapshot) = record.dependency_contracts.as_ref() {
        for provider in &snapshot.providers {
            if let Some(pid) = i32_to_pid(provider.pid)
                && pid_is_alive(pid)
                && current_user_owns_process(pid)
            {
                return true;
            }
        }
    }
    // ExecutionGraph nodes may carry container_ids for OCI services even
    // when orchestration_services is absent (older record format).
    if let Some(graph) = record.graph.as_ref() {
        for node in &graph.nodes {
            if let Some(container_id) = node.container_id.as_deref()
                && oci_container_is_running(container_id)
            {
                return true;
            }
        }
    }
    false
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

/// Strip a rotated-generation suffix (`.1`, `.2`, ... — any numeric `.N`) from
/// `name`, returning `(base, true)` when a suffix was present or `(name, false)`
/// otherwise. The spawn-time rotator (see `log_rotation`) appends `.N` to a base
/// log name, so `<base>` is the path the rest of the sweep reasons about (owner
/// lookup, pid parsing). A purely-numeric trailing component is required so a
/// genuine log name such as `engine-1234-5678.log` is never mistaken for a
/// generation of `engine-1234-5678` (its trailing component `log` is not
/// numeric). #767.
fn strip_log_generation_suffix(name: &str) -> (&str, bool) {
    if let Some((base, suffix)) = name.rsplit_once('.')
        && !suffix.is_empty()
        && suffix.bytes().all(|b| b.is_ascii_digit())
    {
        return (base, true);
    }
    (name, false)
}

/// `true` if `name` is a session guest log the TTL/size sweep should consider:
/// the base `<session_id>.log` OR any rotated generation `<session_id>.log.N`.
fn is_session_log_candidate(name: &str) -> bool {
    let (base, _) = strip_log_generation_suffix(name);
    Path::new(base).extension().and_then(|ext| ext.to_str()) == Some("log")
}

/// `true` if `name` is an engine guest log the TTL/size sweep should consider:
/// the base `engine-<pid>-<stamp>.log` OR any rotated generation `...log.N`.
fn is_engine_log_candidate(name: &str) -> bool {
    let (base, _) = strip_log_generation_suffix(name);
    base.starts_with("engine-") && base.ends_with(".log")
}

/// A guest log file the TTL/size sweep is considering, with the metadata the
/// retention decision needs. `protected` is `true` when the owning process is
/// known to still be alive (and therefore the file must never be reclaimed by
/// age or size); `None`-owner files (no record / unparseable pid) are not
/// protected and fall through to plain age/size sweeping of clearly-stale files.
#[derive(Debug, Clone)]
struct LogCandidate {
    path: PathBuf,
    /// File size in bytes (0 if unreadable).
    size: u64,
    /// Last-modified time, used both for the TTL check and oldest-first
    /// size eviction ordering. `None` if metadata is unavailable.
    modified: Option<SystemTime>,
    /// `true` when the owning process is known-alive: exempt from reclamation.
    protected: bool,
}

/// Decide which `candidates` to reclaim under TTL + aggregate-size policy.
///
/// Rules (protected candidates are never selected):
///   1. Any candidate older than `ttl` is selected (age sweep).
///   2. Of the survivors, if their combined size exceeds `size_budget`, the
///      oldest are selected until the retained total fits (size sweep).
///
/// Returned as indices into `candidates` so callers can map back to paths.
/// Pure and total — no I/O — so it is unit-testable in isolation.
fn select_logs_to_reclaim(
    candidates: &[LogCandidate],
    now: SystemTime,
    ttl: Duration,
    size_budget: u64,
) -> Vec<usize> {
    let mut selected = Vec::new();
    let mut survivors: Vec<usize> = Vec::new();

    for (idx, candidate) in candidates.iter().enumerate() {
        if candidate.protected {
            continue;
        }
        let age = candidate
            .modified
            .and_then(|modified| now.duration_since(modified).ok());
        if age.map(|age| age >= ttl).unwrap_or(false) {
            selected.push(idx);
        } else {
            survivors.push(idx);
        }
    }

    let mut retained: u64 = survivors
        .iter()
        .map(|&idx| candidates[idx].size)
        .fold(0u64, |acc, size| acc.saturating_add(size));
    if retained <= size_budget {
        return selected;
    }

    // Evict oldest-first until the retained total fits the budget. Files with
    // no modified time sort oldest (most eligible for eviction).
    survivors.sort_by_key(|&idx| candidates[idx].modified);
    for idx in survivors {
        if retained <= size_budget {
            break;
        }
        retained = retained.saturating_sub(candidates[idx].size);
        selected.push(idx);
    }
    selected
}

/// Build a [`LogCandidate`] for `path`, resolving `protected` via `is_alive`
/// (called only when ownership is knowable; `None` ⇒ unprotected).
fn log_candidate(path: PathBuf, protected: bool) -> LogCandidate {
    let metadata = path.metadata().ok();
    LogCandidate {
        size: metadata.as_ref().map(|m| m.len()).unwrap_or(0),
        modified: metadata.and_then(|m| m.modified().ok()),
        protected,
        path,
    }
}

/// Apply the TTL/size reclamation decision to `candidates`, deleting the
/// selected files. Returns the number removed.
fn reclaim_logs(
    candidates: &[LogCandidate],
    now: SystemTime,
    ttl: Duration,
    size_budget: u64,
) -> usize {
    let mut removed = 0;
    for idx in select_logs_to_reclaim(candidates, now, ttl, size_budget) {
        let path = &candidates[idx].path;
        match fs::remove_file(path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                debug!(path = %path.display(), error = %error, "failed to remove stale guest log")
            }
        }
    }
    removed
}

/// TTL/size sweep for session logs under `session_root` (`<session_id>.log`).
///
/// A session log is protected (never reclaimed by age/size) while its owning
/// session record (`<session_id>.json`) is still alive — reusing the same
/// liveness check as `sweep_session_records`. Once the record is gone or dead,
/// the log falls under plain TTL/size sweeping. (The liveness-gated path in
/// `sweep_session_records` already removes the log the moment the record dies;
/// this catches logs that outlived their record entirely — e.g. the record was
/// removed but the log delete failed, or older layouts.)
fn sweep_session_log_files(
    session_root: &Path,
    now: SystemTime,
    ttl: Duration,
    size_budget: u64,
) -> Result<usize> {
    if !session_root.exists() {
        return Ok(0);
    }
    let mut candidates = Vec::new();
    for entry in fs::read_dir(session_root)
        .with_context(|| format!("failed to read session root {}", session_root.display()))?
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                debug!(error = %error, "skipping unreadable session log sweep entry");
                continue;
            }
        };
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_session_log_candidate(name) {
            continue;
        }
        // Only the *base* `<session_id>.log` of a live session is being actively
        // written and must be protected. Rotated generations (`...log.N`) are
        // old-by-definition, so they stay eligible for age/size reclamation even
        // while the owning session lives. #767.
        let (_, is_generation) = strip_log_generation_suffix(name);
        let protected = !is_generation && session_log_owner_is_alive(session_root, &path);
        candidates.push(log_candidate(path, protected));
    }
    Ok(reclaim_logs(&candidates, now, ttl, size_budget))
}

/// `true` if the session record matching `log_path` exists and is still alive.
/// Resolves the owning session id by stripping any rotated-generation suffix
/// (`<id>.log.N` ⇒ `<id>.log`) and then the `.log` extension (`<id>.log` ⇒
/// `<id>`), so a rotated generation is attributed to the same session as its
/// base log (`<id>.json`). Missing/unparseable record ⇒ not alive (the log is an
/// orphan eligible for plain age/size sweeping). #767.
fn session_log_owner_is_alive(session_root: &Path, log_path: &Path) -> bool {
    let Some(name) = log_path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let (base, _) = strip_log_generation_suffix(name);
    let Some(session_id) = base.strip_suffix(".log") else {
        return false;
    };
    let record_path = session_root.join(format!("{session_id}.json"));
    let record = match fs::read_to_string(&record_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<StoredSessionInfo>(&raw).ok())
    {
        Some(record) => record,
        None => return false,
    };
    session_record_is_alive(&record)
}

/// TTL/size sweep for engine/run logs under `run_dir` (`engine-<pid>-*.log`).
///
/// An engine log is protected while the writing engine process (the `<pid>` in
/// its name) is still alive and owned by the current user — defeating PID reuse
/// the same way the rest of the sweep does. Logs whose pid is dead, foreign, or
/// unparseable fall under plain TTL/size sweeping.
fn sweep_engine_log_files(
    run_dir: &Path,
    now: SystemTime,
    ttl: Duration,
    size_budget: u64,
) -> Result<usize> {
    if !run_dir.exists() {
        return Ok(0);
    }
    let mut candidates = Vec::new();
    for entry in fs::read_dir(run_dir)
        .with_context(|| format!("failed to read run dir {}", run_dir.display()))?
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                debug!(error = %error, "skipping unreadable engine log sweep entry");
                continue;
            }
        };
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_engine_log_candidate(name) {
            continue;
        }
        let Some(pid) = parse_engine_log_pid(name) else {
            continue;
        };
        // Only the *base* `<engine>.log` of a live engine is being actively
        // written and must be protected. Rotated generations (`...log.N`) are
        // old-by-definition — the engine has already closed them — so they stay
        // eligible for age/size reclamation even while their engine lives. #767.
        let (_, is_generation) = strip_log_generation_suffix(name);
        let protected = !is_generation
            && (pid == std::process::id() || (pid_is_alive(pid) && current_user_owns_process(pid)));
        candidates.push(log_candidate(path, protected));
    }
    Ok(reclaim_logs(&candidates, now, ttl, size_budget))
}

/// Extract the engine pid from an `engine-<pid>-<stamp>.log` file name, or from
/// any rotated generation `engine-<pid>-<stamp>.log.N` (the `.N` suffix is
/// stripped first so a rotated generation is attributed to the same engine pid
/// as its base log). Returns `None` for any other name (only engine logs follow
/// this scheme, see `phases::run`). #767.
fn parse_engine_log_pid(name: &str) -> Option<u32> {
    let (base, _) = strip_log_generation_suffix(name);
    let stem = base.strip_prefix("engine-")?.strip_suffix(".log")?;
    let pid = stem.split('-').next()?;
    pid.parse().ok()
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
    use protocol::handle::{CapsuleDisplayStrategy, CapsuleRuntimeDescriptor, TrustState};
    use tempfile::tempdir;

    fn options(root: &Path) -> StartupSweepOptions {
        StartupSweepOptions {
            run_dir: root.join("run"),
            runs_dir: root.join("runs"),
            session_root: root.join("sessions"),
            now: SystemTime::now() + Duration::from_secs(48 * 60 * 60),
            socket_grace: Duration::from_secs(60),
            run_dir_ttl: Duration::from_secs(24 * 60 * 60),
            log_ttl: Duration::from_secs(14 * 24 * 60 * 60),
            log_size_budget: 512 * 1024 * 1024,
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
            launch_key: None,
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
            readiness_confirmed: false,
            guest: None,
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: None,
            graph: None,
            execution_id: None,
            execution_receipt_schema_version: None,
            declared_execution_id: None,
            resolved_execution_id: None,
            observed_execution_id: None,
            graph_completeness: None,
            reproducibility_class: None,
            schema_version: None,
            launch_digest: None,
            process_start_time_unix_ms: None,
            installed_app_id: None,
            install_profile_id: None,
            install_profile_key: None,
            install_revision_id: None,
            capsule_instance_key: None,
            placement_provider: None,
            placement_provider_id: None,
            placement_id: None,
            placement_fingerprint: None,
            placement_facets: None,
            user_visible_url: None,
            requested_by_client: None,
            runtime_owner: None,
            orchestration_services: None,
        };
        let record_path = options.session_root.join("ato-desktop-session-dead.json");
        fs::write(&record_path, serde_json::to_vec(&record).expect("record")).expect("write");

        let report = sweep_startup_runtime_artifacts(&options).expect("sweep");

        assert_eq!(report.removed_session_records, 1);
        assert!(!record_path.exists());
        assert!(!log_path.exists());
    }

    /// #108 regression: under PR-C in-process orchestration, the
    /// wrapper that ran `ato app session start` exits successfully
    /// after detaching the workload runtime. `record.pid` is therefore
    /// dead by the time the sweep runs, but `orchestration_services`
    /// still points at the live workload pids that survived. Sweeping
    /// the record here would hide those workloads from
    /// `ato app session stop`, leaking them on the leaf port.
    #[test]
    fn sweep_keeps_session_record_when_orchestration_services_pid_is_alive() {
        use crate::state::session::record::{
            StoredOrchestrationService, StoredOrchestrationServices,
        };

        let temp = tempdir().expect("tempdir");
        let options = options(temp.path());
        fs::create_dir_all(&options.session_root).expect("session root");

        // The "live workload" is just the test process itself — its
        // pid is guaranteed alive and ours for the duration of the
        // sweep call.
        let self_pid = std::process::id() as i32;
        let log_path = temp.path().join("orch.log");
        fs::write(&log_path, "log").expect("log");

        let record = StoredSessionInfo {
            session_id: "ato-desktop-session-orch".to_string(),
            launch_key: None,
            handle: "capsule://example/orch".to_string(),
            normalized_handle: "capsule://example/orch".to_string(),
            canonical_handle: None,
            trust_state: TrustState::Trusted,
            source: None,
            restricted: false,
            snapshot: None,
            runtime: CapsuleRuntimeDescriptor {
                target_label: "web".to_string(),
                runtime: Some("source".to_string()),
                driver: None,
                language: None,
                port: None,
            },
            display_strategy: CapsuleDisplayStrategy::WebUrl,
            // record.pid mimics the wrapper that already exited.
            pid: 999_999_999,
            log_path: log_path.display().to_string(),
            manifest_path: "capsule.toml".to_string(),
            target_label: "web".to_string(),
            notes: Vec::new(),
            readiness_confirmed: false,
            guest: None,
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: None,
            graph: None,
            execution_id: None,
            execution_receipt_schema_version: None,
            declared_execution_id: None,
            resolved_execution_id: None,
            observed_execution_id: None,
            graph_completeness: None,
            reproducibility_class: None,
            orchestration_services: Some(StoredOrchestrationServices {
                wrapper_pid: 999_999_999,
                services: vec![StoredOrchestrationService {
                    name: "web".to_string(),
                    target_label: "web".to_string(),
                    local_pid: Some(self_pid),
                    container_id: None,
                    host_ports: std::collections::BTreeMap::new(),
                    published_port: Some(5173),
                }],
                network_name: None,
                ephemeral_volumes: Vec::new(),
            }),
            schema_version: None,
            launch_digest: None,
            process_start_time_unix_ms: None,
            installed_app_id: None,
            install_profile_id: None,
            install_profile_key: None,
            install_revision_id: None,
            capsule_instance_key: None,
            placement_provider: None,
            placement_provider_id: None,
            placement_id: None,
            placement_fingerprint: None,
            placement_facets: None,
            user_visible_url: None,
            requested_by_client: None,
            runtime_owner: None,
        };
        let record_path = options.session_root.join("ato-desktop-session-orch.json");
        fs::write(&record_path, serde_json::to_vec(&record).expect("record")).expect("write");

        let report = sweep_startup_runtime_artifacts(&options).expect("sweep");

        assert_eq!(report.removed_session_records, 0);
        assert!(
            record_path.exists(),
            "record must be retained while any orchestration_services pid is alive"
        );
        assert!(log_path.exists());
    }

    /// Same shape as the previous test but for `dependency_contracts` —
    /// when the wrapper has exited but a dep-contract provider (e.g.
    /// postgres) is still alive, the record must be retained so
    /// `stop_session` can find the provider on the next `ato` invocation.
    #[test]
    fn sweep_keeps_session_record_when_dependency_contract_pid_is_alive() {
        use crate::state::session::record::{StoredDependencyContracts, StoredDependencyProvider};

        let temp = tempdir().expect("tempdir");
        let options = options(temp.path());
        fs::create_dir_all(&options.session_root).expect("session root");
        let self_pid = std::process::id() as i32;
        let log_path = temp.path().join("dep.log");
        fs::write(&log_path, "log").expect("log");

        let record = StoredSessionInfo {
            session_id: "ato-desktop-session-dep".to_string(),
            launch_key: None,
            handle: "capsule://example/dep".to_string(),
            normalized_handle: "capsule://example/dep".to_string(),
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
            readiness_confirmed: false,
            guest: None,
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: Some(StoredDependencyContracts {
                consumer_pid: 999_999_999,
                providers: vec![StoredDependencyProvider {
                    alias: "db".to_string(),
                    pid: self_pid,
                    state_dir: temp.path().join("state/db"),
                    resolved: "capsule://example/postgres@1".to_string(),
                    allocated_port: Some(5432),
                    log_path: None,
                    runtime_export_keys: Vec::new(),
                }],
            }),
            graph: None,
            execution_id: None,
            execution_receipt_schema_version: None,
            declared_execution_id: None,
            resolved_execution_id: None,
            observed_execution_id: None,
            graph_completeness: None,
            reproducibility_class: None,
            orchestration_services: None,
            schema_version: None,
            launch_digest: None,
            process_start_time_unix_ms: None,
            installed_app_id: None,
            install_profile_id: None,
            install_profile_key: None,
            install_revision_id: None,
            capsule_instance_key: None,
            placement_provider: None,
            placement_provider_id: None,
            placement_id: None,
            placement_fingerprint: None,
            placement_facets: None,
            user_visible_url: None,
            requested_by_client: None,
            runtime_owner: None,
        };
        let record_path = options.session_root.join("ato-desktop-session-dep.json");
        fs::write(&record_path, serde_json::to_vec(&record).expect("record")).expect("write");

        let report = sweep_startup_runtime_artifacts(&options).expect("sweep");

        assert_eq!(report.removed_session_records, 0);
        assert!(
            record_path.exists(),
            "record must be retained while any dependency_contract provider pid is alive"
        );
        assert!(log_path.exists());
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

    #[test]
    fn throttle_skips_when_stamp_is_within_window() {
        let temp = tempdir().expect("tempdir");
        let run_dir = temp.path().join("run");
        fs::create_dir_all(&run_dir).expect("run dir");
        let now = SystemTime::now();
        // Stamp recorded "now".
        mark_sweep_completed(&run_dir, now);
        // A check 10s later, with a 30s throttle, is still fresh → skip.
        let later = now + Duration::from_secs(10);
        assert!(sweep_recently_completed(
            &run_dir,
            later,
            Duration::from_secs(30)
        ));
    }

    #[test]
    fn throttle_runs_when_stamp_is_older_than_window() {
        let temp = tempdir().expect("tempdir");
        let run_dir = temp.path().join("run");
        fs::create_dir_all(&run_dir).expect("run dir");
        let now = SystemTime::now();
        mark_sweep_completed(&run_dir, now);
        // A check 31s later, with a 30s throttle, is stale → run.
        let later = now + Duration::from_secs(31);
        assert!(!sweep_recently_completed(
            &run_dir,
            later,
            Duration::from_secs(30)
        ));
    }

    #[test]
    fn throttle_runs_when_no_stamp_exists() {
        let temp = tempdir().expect("tempdir");
        let run_dir = temp.path().join("run");
        fs::create_dir_all(&run_dir).expect("run dir");
        // No stamp written → never throttled.
        assert!(!sweep_recently_completed(
            &run_dir,
            SystemTime::now(),
            Duration::from_secs(30)
        ));
    }

    #[test]
    fn throttle_runs_when_stamp_is_unparseable() {
        let temp = tempdir().expect("tempdir");
        let run_dir = temp.path().join("run");
        fs::create_dir_all(&run_dir).expect("run dir");
        fs::write(run_dir.join(SWEEP_STAMP_FILE), "not-a-number").expect("write stamp");
        assert!(!sweep_recently_completed(
            &run_dir,
            SystemTime::now(),
            Duration::from_secs(30)
        ));
    }

    #[test]
    fn throttle_runs_when_clock_moved_backwards() {
        let temp = tempdir().expect("tempdir");
        let run_dir = temp.path().join("run");
        fs::create_dir_all(&run_dir).expect("run dir");
        let now = SystemTime::now();
        // Stamp recorded in the future relative to the check time (clock
        // rollback). Conservatively treat as not-fresh so the sweep runs.
        mark_sweep_completed(&run_dir, now + Duration::from_secs(60));
        assert!(!sweep_recently_completed(
            &run_dir,
            now,
            Duration::from_secs(30)
        ));
    }

    #[test]
    fn throttle_stamp_is_not_swept_as_a_runtime_artifact() {
        // The stamp file shares `run_dir` with pid/socket artifacts; the
        // sweep must never mistake it for a reclaimable artifact.
        let temp = tempdir().expect("tempdir");
        let options = options(temp.path());
        fs::create_dir_all(&options.run_dir).expect("run dir");
        mark_sweep_completed(&options.run_dir, SystemTime::now());
        let stamp = options.run_dir.join(SWEEP_STAMP_FILE);
        assert!(stamp.exists());

        let _ = sweep_startup_runtime_artifacts(&options).expect("sweep");

        assert!(stamp.exists(), "sweep must not delete its own stamp file");
    }

    // ---- #767: TTL/size guest-log sweep ---------------------------------

    fn candidate(size: u64, modified: Option<SystemTime>, protected: bool) -> LogCandidate {
        LogCandidate {
            path: PathBuf::from("/unused"),
            size,
            modified,
            protected,
        }
    }

    #[test]
    fn parse_engine_log_pid_matches_only_engine_logs() {
        assert_eq!(parse_engine_log_pid("engine-1234-99887766.log"), Some(1234));
        // No stamp segment is still parseable (pid is the first token).
        assert_eq!(parse_engine_log_pid("engine-42.log"), Some(42));
        assert_eq!(parse_engine_log_pid("engine-abc-1.log"), None);
        assert_eq!(parse_engine_log_pid("not-an-engine.log"), None);
        assert_eq!(parse_engine_log_pid("engine-1234-1.txt"), None);
    }

    #[test]
    fn select_reclaims_logs_older_than_ttl() {
        let now = SystemTime::now();
        let ttl = Duration::from_secs(100);
        let candidates = vec![
            candidate(10, Some(now - Duration::from_secs(200)), false), // old → reclaim
            candidate(10, Some(now - Duration::from_secs(50)), false),  // fresh → keep
            candidate(10, None, false),                                 // no mtime → keep (not old)
        ];
        let selected = select_logs_to_reclaim(&candidates, now, ttl, u64::MAX);
        assert_eq!(selected, vec![0]);
    }

    #[test]
    fn select_never_reclaims_protected_logs() {
        let now = SystemTime::now();
        let ttl = Duration::from_secs(100);
        // Old AND oversized, but protected (owning process alive): untouched.
        let candidates = vec![candidate(1_000, Some(now - Duration::from_secs(999)), true)];
        let selected = select_logs_to_reclaim(&candidates, now, ttl, 10);
        assert!(selected.is_empty());
    }

    #[test]
    fn select_evicts_oldest_first_over_size_budget() {
        let now = SystemTime::now();
        let ttl = Duration::from_secs(10_000); // nothing reclaimed by age
        // Three fresh logs, 100 bytes each, budget = 150 → must drop 2 oldest.
        let candidates = vec![
            candidate(100, Some(now - Duration::from_secs(10)), false), // newest
            candidate(100, Some(now - Duration::from_secs(30)), false), // oldest
            candidate(100, Some(now - Duration::from_secs(20)), false), // middle
        ];
        let mut selected = select_logs_to_reclaim(&candidates, now, ttl, 150);
        selected.sort();
        // Retained must fit 150: keep only the newest (idx 0); evict 1 and 2.
        assert_eq!(selected, vec![1, 2]);
    }

    #[test]
    fn select_size_budget_keeps_all_when_under() {
        let now = SystemTime::now();
        let candidates = vec![
            candidate(100, Some(now - Duration::from_secs(10)), false),
            candidate(100, Some(now - Duration::from_secs(20)), false),
        ];
        let selected = select_logs_to_reclaim(&candidates, now, Duration::from_secs(10_000), 1_000);
        assert!(selected.is_empty());
    }

    #[test]
    fn sweep_engine_logs_removes_old_dead_pid_keeps_live_self() {
        let temp = tempdir().expect("tempdir");
        let run_dir = temp.path().join("run");
        fs::create_dir_all(&run_dir).expect("run dir");

        // Old engine log owned by a dead pid → reclaimed by TTL.
        let dead = run_dir.join("engine-999999999-111.log");
        fs::write(&dead, "old").expect("write dead log");
        // Our own engine log → protected even if "old".
        let mine = run_dir.join(format!("engine-{}-222.log", std::process::id()));
        fs::write(&mine, "mine").expect("write own log");
        // A non-engine file must be ignored entirely.
        let other = run_dir.join("ato-desktop-1.sock");
        fs::write(&other, "").expect("write other");

        let now = SystemTime::now() + Duration::from_secs(30 * 24 * 60 * 60);
        let removed = sweep_engine_log_files(
            &run_dir,
            now,
            Duration::from_secs(14 * 24 * 60 * 60),
            u64::MAX,
        )
        .expect("sweep engine logs");

        assert_eq!(removed, 1);
        assert!(!dead.exists());
        assert!(mine.exists(), "own engine log must be protected");
        assert!(other.exists(), "non-engine file untouched");
    }

    #[test]
    fn sweep_session_logs_removes_orphan_but_keeps_owned_alive() {
        let temp = tempdir().expect("tempdir");
        let session_root = temp.path().join("sessions");
        fs::create_dir_all(&session_root).expect("session root");

        // Orphan log: no matching record → reclaimed by TTL.
        let orphan = session_root.join("ato-desktop-session-orphan.log");
        fs::write(&orphan, "orphan").expect("write orphan");

        // Owned log: matching record whose pid is THIS process (alive) →
        // protected regardless of age/size.
        let owned_id = "ato-desktop-session-live";
        let owned_log = session_root.join(format!("{owned_id}.log"));
        fs::write(&owned_log, "owned").expect("write owned");
        let record = dead_session_record(owned_id, std::process::id() as i32, &owned_log);
        fs::write(
            session_root.join(format!("{owned_id}.json")),
            serde_json::to_vec(&record).expect("record"),
        )
        .expect("write record");

        let now = SystemTime::now() + Duration::from_secs(30 * 24 * 60 * 60);
        let removed = sweep_session_log_files(
            &session_root,
            now,
            Duration::from_secs(14 * 24 * 60 * 60),
            u64::MAX,
        )
        .expect("sweep session logs");

        assert_eq!(removed, 1);
        assert!(!orphan.exists());
        assert!(owned_log.exists(), "log owned by a live pid must survive");
    }

    #[test]
    fn strip_log_generation_suffix_handles_base_and_generations() {
        assert_eq!(strip_log_generation_suffix("id.log"), ("id.log", false));
        assert_eq!(strip_log_generation_suffix("id.log.1"), ("id.log", true));
        assert_eq!(strip_log_generation_suffix("id.log.3"), ("id.log", true));
        // Non-numeric trailing component is not a generation.
        assert_eq!(
            strip_log_generation_suffix("engine-1234-5678.log"),
            ("engine-1234-5678.log", false)
        );
    }

    #[test]
    fn log_candidate_matchers_accept_base_and_rotated_generations() {
        // Base log plus every rotated generation the spawn-time rotator can
        // produce (`.1` .. `.LOG_ROTATE_MAX_GENERATIONS`).
        let mut suffixes = vec![String::new()];
        suffixes.extend((1..=LOG_ROTATE_MAX_GENERATIONS).map(|n| format!(".{n}")));
        // …and an out-of-range numeric suffix, to prove matching is robust
        // (not hard-capped at the rotator's nominal max).
        suffixes.push(format!(".{}", LOG_ROTATE_MAX_GENERATIONS + 5));
        for n in &suffixes {
            assert!(is_session_log_candidate(&format!(
                "ato-desktop-session-x.log{n}"
            )));
            assert!(is_engine_log_candidate(&format!("engine-1234-5678.log{n}")));
        }
        assert!(!is_session_log_candidate("ato-desktop-session-x.json"));
        assert!(!is_engine_log_candidate("ato-desktop-1.sock"));
        // Generation suffix on the engine log still resolves the owning pid.
        assert_eq!(parse_engine_log_pid("engine-1234-5678.log.1"), Some(1234));
    }

    #[test]
    fn sweep_engine_logs_reclaims_old_rotated_generation_keeps_live_base() {
        let temp = tempdir().expect("tempdir");
        let run_dir = temp.path().join("run");
        fs::create_dir_all(&run_dir).expect("run dir");

        // A live engine (this process): its base .log is protected, but a
        // rotated generation of the SAME live engine is reclaimable by age.
        let self_pid = std::process::id();
        let base = run_dir.join(format!("engine-{self_pid}-222.log"));
        let generation = run_dir.join(format!("engine-{self_pid}-222.log.1"));
        fs::write(&base, "live base").expect("write base log");
        fs::write(&generation, "rotated").expect("write rotated log");

        let now = SystemTime::now() + Duration::from_secs(30 * 24 * 60 * 60);
        let removed = sweep_engine_log_files(
            &run_dir,
            now,
            Duration::from_secs(14 * 24 * 60 * 60),
            u64::MAX,
        )
        .expect("sweep engine logs");

        assert_eq!(removed, 1, "rotated generation reclaimed by TTL");
        assert!(base.exists(), "live engine base .log must be protected");
        assert!(
            !generation.exists(),
            "rotated generation of a live engine must be reclaimed by TTL"
        );
    }

    #[test]
    fn sweep_engine_logs_evicts_rotated_generation_by_size_budget() {
        let temp = tempdir().expect("tempdir");
        let run_dir = temp.path().join("run");
        fs::create_dir_all(&run_dir).expect("run dir");

        // Dead-pid engine: base + one rotated generation, both fresh (not past
        // TTL). A tiny size budget forces oldest-first eviction. The rotated
        // generation (.1) is older than the base, so it is evicted first.
        let base = run_dir.join("engine-999999999-222.log");
        let generation = run_dir.join("engine-999999999-222.log.1");
        // Back-date the generation so it sorts oldest for eviction ordering.
        fs::write(&generation, vec![b'x'; 100]).expect("write rotated log");
        std::thread::sleep(Duration::from_millis(20));
        fs::write(&base, vec![b'x'; 100]).expect("write base log");

        let now = SystemTime::now();
        let removed = sweep_engine_log_files(
            &run_dir,
            now,
            Duration::from_secs(14 * 24 * 60 * 60), // nothing reclaimed by age
            150,                                    // budget < 200 → evict oldest until fits
        )
        .expect("sweep engine logs");

        assert_eq!(removed, 1, "one log evicted to fit the size budget");
        assert!(
            !generation.exists(),
            "oldest (rotated generation) evicted first by size budget"
        );
        assert!(base.exists(), "newer base log retained under budget");
    }

    #[test]
    fn sweep_session_logs_reclaims_old_rotated_generation_keeps_live_base() {
        let temp = tempdir().expect("tempdir");
        let session_root = temp.path().join("sessions");
        fs::create_dir_all(&session_root).expect("session root");

        // A live session (record pid = this process): the base .log is
        // protected, but a rotated generation of the same session is reclaimable.
        let session_id = "ato-desktop-session-live";
        let base = session_root.join(format!("{session_id}.log"));
        let generation = session_root.join(format!("{session_id}.log.1"));
        fs::write(&base, "live base").expect("write base");
        fs::write(&generation, "rotated").expect("write rotated");
        let record = dead_session_record(session_id, std::process::id() as i32, &base);
        fs::write(
            session_root.join(format!("{session_id}.json")),
            serde_json::to_vec(&record).expect("record"),
        )
        .expect("write record");

        let now = SystemTime::now() + Duration::from_secs(30 * 24 * 60 * 60);
        let removed = sweep_session_log_files(
            &session_root,
            now,
            Duration::from_secs(14 * 24 * 60 * 60),
            u64::MAX,
        )
        .expect("sweep session logs");

        assert_eq!(removed, 1, "rotated generation reclaimed by TTL");
        assert!(base.exists(), "live session base .log must be protected");
        assert!(
            !generation.exists(),
            "rotated generation of a live session must be reclaimed by TTL"
        );
    }

    /// Minimal `StoredSessionInfo` for sweep tests. `pid` controls liveness.
    fn dead_session_record(session_id: &str, pid: i32, log_path: &Path) -> StoredSessionInfo {
        StoredSessionInfo {
            session_id: session_id.to_string(),
            launch_key: None,
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
            pid,
            log_path: log_path.display().to_string(),
            manifest_path: "capsule.toml".to_string(),
            target_label: "default".to_string(),
            notes: Vec::new(),
            readiness_confirmed: false,
            guest: None,
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: None,
            graph: None,
            execution_id: None,
            execution_receipt_schema_version: None,
            declared_execution_id: None,
            resolved_execution_id: None,
            observed_execution_id: None,
            graph_completeness: None,
            reproducibility_class: None,
            schema_version: None,
            launch_digest: None,
            process_start_time_unix_ms: None,
            installed_app_id: None,
            install_profile_id: None,
            install_profile_key: None,
            install_revision_id: None,
            capsule_instance_key: None,
            placement_provider: None,
            placement_provider_id: None,
            placement_id: None,
            placement_fingerprint: None,
            placement_facets: None,
            user_visible_url: None,
            requested_by_client: None,
            runtime_owner: None,
            orchestration_services: None,
        }
    }
}
