use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(any(unix, windows))]
use std::process::Command;
use std::time::{Duration, SystemTime};

const PID_FILE_EXT: &str = ".pid";
const RUN_SESSIONS_DIR_NAME: &str = "run-sessions";
const DEPENDENCY_SESSION_FILE: &str = "graph.json";
const IMPORT_PREVIEW_SESSIONS_DIR_NAME: &str = "import-preview-sessions";
const IMPORT_PREVIEW_SESSION_FILE: &str = "session.json";

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
    /// OS-reported start time of `pid` (ms since UNIX epoch), captured at
    /// registration. Distinct from `start_time` (which is registration
    /// wall-clock time and used for uptime / display) — this field is the
    /// canonical comparator for `ato-session-core`'s startup orphan sweep
    /// to defeat PID reuse. `None` on platforms without OS-query support
    /// or when the OS query fails.
    #[serde(default)]
    pub os_start_time_unix_ms: Option<u64>,
    /// Same shape as `os_start_time_unix_ms` but for `workload_pid`. `None`
    /// when `workload_pid` is `None` or the OS query is unsupported.
    #[serde(default)]
    pub workload_os_start_time_unix_ms: Option<u64>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyContractProcessInfo {
    pub alias: String,
    pub pid: i32,
    pub state_dir: PathBuf,
    pub resolved: String,
    #[serde(default)]
    pub allocated_port: Option<u16>,
    #[serde(default)]
    pub log_path: Option<PathBuf>,
    #[serde(default)]
    pub runtime_export_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyContractSessionSnapshot {
    pub session_id: String,
    pub consumer_pid: i32,
    #[serde(default)]
    pub providers: Vec<DependencyContractProcessInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportPreviewSession {
    pub run_session_id: String,
    pub owner_kind: String,
    pub owner_pid: i32,
    pub owner_process_start_time_unix_ms: Option<u64>,
    pub ato_run_pid: i32,
    pub ato_run_process_start_time_unix_ms: Option<u64>,
    #[serde(default)]
    pub process_group_ids: Vec<i32>,
    pub primary_port: Option<u16>,
    pub primary_url: Option<String>,
    pub shadow_dir: PathBuf,
    pub log_path: PathBuf,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
    #[serde(default)]
    pub expires_at_unix_ms: Option<u64>,
    pub readiness_state: String,
    pub cleanup_policy: String,
    #[serde(default)]
    pub last_sweep_status: Option<String>,
    #[serde(default)]
    pub last_sweep_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportPreviewStopStatus {
    Stopped,
    AlreadyGone,
    NotAtoOwned,
    Failed,
}

impl std::fmt::Display for ImportPreviewStopStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportPreviewStopStatus::Stopped => write!(f, "Stopped"),
            ImportPreviewStopStatus::AlreadyGone => write!(f, "AlreadyGone"),
            ImportPreviewStopStatus::NotAtoOwned => write!(f, "NotAtoOwned"),
            ImportPreviewStopStatus::Failed => write!(f, "Failed"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImportPreviewStopResult {
    pub session: ImportPreviewSession,
    pub status: ImportPreviewStopStatus,
    pub error: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportPreviewSweepReport {
    pub active_sessions_kept: usize,
    pub stale_sessions_stopped: usize,
    pub stale_sessions_already_gone: usize,
    pub stale_sessions_failed: usize,
    pub env_process_groups_stopped: usize,
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
        let run_dir = capsule_core::common::paths::ato_path_or_workspace_tmp("run");

        if !run_dir.exists() {
            fs::create_dir_all(&run_dir).with_context(|| {
                format!("Failed to create run directory: {}", run_dir.display())
            })?;
        }

        Ok(Self { run_dir })
    }

    /// Test-only constructor pinning the run directory to an explicit path, so
    /// unit tests can exercise pid persistence against a tempdir without
    /// touching the real ATO run directory.
    #[cfg(test)]
    pub(crate) fn with_run_dir_for_test(run_dir: PathBuf) -> Self {
        Self { run_dir }
    }

    #[allow(dead_code)]
    pub fn get_run_dir(&self) -> &Path {
        &self.run_dir
    }

    pub fn pid_file_path(&self, id: &str) -> PathBuf {
        self.run_dir.join(format!("{}{}", id, PID_FILE_EXT))
    }

    fn run_sessions_dir(&self) -> PathBuf {
        self.run_dir
            .parent()
            .map(|parent| parent.join(RUN_SESSIONS_DIR_NAME))
            .unwrap_or_else(|| self.run_dir.join(RUN_SESSIONS_DIR_NAME))
    }

    fn import_preview_sessions_dir(&self) -> PathBuf {
        self.run_dir
            .parent()
            .map(|parent| parent.join(IMPORT_PREVIEW_SESSIONS_DIR_NAME))
            .unwrap_or_else(|| self.run_dir.join(IMPORT_PREVIEW_SESSIONS_DIR_NAME))
    }

    fn import_preview_session_dir(&self, id: &str) -> PathBuf {
        self.import_preview_sessions_dir().join(id)
    }

    fn import_preview_session_path(&self, id: &str) -> PathBuf {
        self.import_preview_session_dir(id)
            .join(IMPORT_PREVIEW_SESSION_FILE)
    }

    fn dependency_session_dir(&self, id: &str) -> PathBuf {
        self.run_sessions_dir().join(id)
    }

    fn dependency_session_path(&self, id: &str) -> PathBuf {
        self.dependency_session_dir(id)
            .join(DEPENDENCY_SESSION_FILE)
    }

    pub fn write_dependency_session_snapshot(
        &self,
        id: &str,
        snapshot: &DependencyContractSessionSnapshot,
    ) -> Result<PathBuf> {
        let session_dir = self.dependency_session_dir(id);
        fs::create_dir_all(&session_dir).with_context(|| {
            format!(
                "Failed to create dependency session directory: {}",
                session_dir.display()
            )
        })?;
        let path = session_dir.join(DEPENDENCY_SESSION_FILE);
        let content = serde_json::to_string_pretty(snapshot)
            .with_context(|| "Failed to serialize dependency session snapshot")?;
        fs::write(&path, content).with_context(|| {
            format!(
                "Failed to write dependency session snapshot: {}",
                path.display()
            )
        })?;
        Ok(path)
    }

    pub fn read_dependency_session_snapshot(
        &self,
        id: &str,
    ) -> Result<Option<DependencyContractSessionSnapshot>> {
        let path = self.dependency_session_path(id);
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&path).with_context(|| {
            format!(
                "Failed to read dependency session snapshot: {}",
                path.display()
            )
        })?;
        let snapshot = serde_json::from_str(&content).with_context(|| {
            format!(
                "Failed to parse dependency session snapshot: {}",
                path.display()
            )
        })?;
        Ok(Some(snapshot))
    }

    fn delete_dependency_session_snapshot(&self, id: &str) -> Result<()> {
        let session_dir = self.dependency_session_dir(id);
        if session_dir.exists() {
            fs::remove_dir_all(&session_dir).with_context(|| {
                format!(
                    "Failed to remove dependency session directory: {}",
                    session_dir.display()
                )
            })?;
        }
        Ok(())
    }

    pub fn write_import_preview_session(&self, session: &ImportPreviewSession) -> Result<PathBuf> {
        let session_dir = self.import_preview_session_dir(&session.run_session_id);
        fs::create_dir_all(&session_dir).with_context(|| {
            format!(
                "Failed to create import preview session directory: {}",
                session_dir.display()
            )
        })?;
        let path = session_dir.join(IMPORT_PREVIEW_SESSION_FILE);
        let content = serde_json::to_string_pretty(session)
            .with_context(|| "Failed to serialize import preview session")?;
        fs::write(&path, content).with_context(|| {
            format!("Failed to write import preview session: {}", path.display())
        })?;
        Ok(path)
    }

    pub fn read_import_preview_session(&self, id: &str) -> Result<Option<ImportPreviewSession>> {
        let path = self.import_preview_session_path(id);
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&path).with_context(|| {
            format!("Failed to read import preview session: {}", path.display())
        })?;
        let session = serde_json::from_str(&content).with_context(|| {
            format!("Failed to parse import preview session: {}", path.display())
        })?;
        Ok(Some(session))
    }

    pub fn list_import_preview_sessions(&self) -> Result<Vec<ImportPreviewSession>> {
        let root = self.import_preview_sessions_dir();
        let mut sessions = Vec::new();
        if !root.exists() {
            return Ok(sessions);
        }
        for entry in fs::read_dir(&root).with_context(|| {
            format!("Failed to read import preview sessions: {}", root.display())
        })? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path().join(IMPORT_PREVIEW_SESSION_FILE);
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            if let Ok(session) = serde_json::from_str::<ImportPreviewSession>(&content) {
                sessions.push(session);
            }
        }
        Ok(sessions)
    }

    pub fn delete_import_preview_session(&self, id: &str) -> Result<()> {
        let session_dir = self.import_preview_session_dir(id);
        if session_dir.exists() {
            fs::remove_dir_all(&session_dir).with_context(|| {
                format!(
                    "Failed to remove import preview session directory: {}",
                    session_dir.display()
                )
            })?;
        }
        Ok(())
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
        let transient_workspace = transient_workspace_for_pid_file(&pid_path);
        if pid_path.exists() {
            fs::remove_file(&pid_path)
                .with_context(|| format!("Failed to remove PID file: {}", pid_path.display()))?;
        }
        self.delete_dependency_session_snapshot(id)?;
        if let Some(workspace) = transient_workspace {
            let _ = fs::remove_dir_all(workspace);
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
                && let Some(filename) = path.file_stem()
                && let Some(id) = filename.to_str()
                && let Ok(info) = self.read_pid(id)
            {
                processes.push(info);
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

        if process_info_is_alive(info) {
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
            let has_dependency_session = self
                .read_dependency_session_snapshot(&process.id)
                .map(|snapshot| snapshot.is_some())
                .unwrap_or(false);
            if process.status.is_active() || has_dependency_session {
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
            Err(_) => {
                let stopped_deps = self.stop_dependency_session(id, force)?;
                if stopped_deps {
                    self.delete_pid(id)?;
                }
                return Ok(stopped_deps);
            }
        };

        if !info.status.is_active() {
            let stopped_deps = self.stop_dependency_session(id, force)?;
            if stopped_deps {
                self.delete_pid(id)?;
            }
            return Ok(stopped_deps);
        }

        if !process_info_is_alive(&info) {
            let stopped_deps = self.stop_dependency_session(id, force)?;
            self.delete_pid(id)?;
            return Ok(stopped_deps);
        }

        let stopped_consumer = self.stop_process_tree(&info, force)?;
        if stopped_consumer {
            let _ = self.stop_dependency_session(id, force)?;
            self.delete_pid(id)?;
            Ok(true)
        } else {
            let stopped_deps = self.stop_dependency_session(id, force)?;
            self.delete_pid(id)?;
            Ok(stopped_deps)
        }
    }

    pub fn stop_import_preview_session(
        &self,
        id: &str,
        force: bool,
    ) -> Result<Option<ImportPreviewStopResult>> {
        let Some(session) = self.read_import_preview_session(id)? else {
            return Ok(None);
        };
        let result = stop_import_preview_session_record(&session, force);
        if matches!(
            result.status,
            ImportPreviewStopStatus::Stopped | ImportPreviewStopStatus::AlreadyGone
        ) {
            let _ = self.delete_import_preview_session(id);
        }
        Ok(Some(result))
    }

    pub fn stop_all_import_preview_sessions(
        &self,
        force: bool,
    ) -> Result<Vec<ImportPreviewStopResult>> {
        let mut results = Vec::new();
        for session in self.list_import_preview_sessions()? {
            if let Some(result) =
                self.stop_import_preview_session(&session.run_session_id, force)?
            {
                results.push(result);
            }
        }
        Ok(results)
    }

    pub fn sweep_import_preview_sessions(&self, force: bool) -> Result<ImportPreviewSweepReport> {
        let mut report = ImportPreviewSweepReport::default();
        let sessions = self.list_import_preview_sessions()?;
        for session in &sessions {
            if !import_preview_session_is_stale(session) {
                report.active_sessions_kept += 1;
                continue;
            }

            match self.stop_import_preview_session(&session.run_session_id, force)? {
                Some(result) => match result.status {
                    ImportPreviewStopStatus::Stopped => report.stale_sessions_stopped += 1,
                    ImportPreviewStopStatus::AlreadyGone => report.stale_sessions_already_gone += 1,
                    ImportPreviewStopStatus::NotAtoOwned | ImportPreviewStopStatus::Failed => {
                        report.stale_sessions_failed += 1;
                        let mut failed = result.session;
                        failed.updated_at_unix_ms =
                            now_unix_ms_lossy().unwrap_or(failed.updated_at_unix_ms);
                        failed.last_sweep_status = Some(result.status.to_string());
                        failed.last_sweep_error = result.error;
                        let _ = self.write_import_preview_session(&failed);
                    }
                },
                None => report.stale_sessions_already_gone += 1,
            }
        }

        #[cfg(unix)]
        {
            report.env_process_groups_stopped = sweep_import_env_process_groups(force, &sessions);
        }

        Ok(report)
    }

    pub fn active_import_preview_session_for_workspace(
        &self,
        workspace: &Path,
    ) -> Result<Option<ImportPreviewSession>> {
        for session in self.list_import_preview_sessions()? {
            if import_preview_session_is_stale(&session) {
                continue;
            }
            if session.shadow_dir.starts_with(workspace)
                || workspace.starts_with(&session.shadow_dir)
            {
                return Ok(Some(session));
            }
        }
        Ok(None)
    }

    fn stop_process_tree(&self, info: &ProcessInfo, force: bool) -> Result<bool> {
        let mut stopped = false;
        if is_process_alive(info.pid)
            && process_identity_matches(info)
            && terminate_process(info.pid, force)?
        {
            wait_for_process_exit(info.pid, 10)?;
            stopped = true;
        }
        if let Some(workload_pid) = info.workload_pid
            && is_process_alive(workload_pid)
            && terminate_process(workload_pid, force)?
        {
            wait_for_process_exit(workload_pid, 10)?;
            stopped = true;
        }
        Ok(stopped)
    }

    fn stop_dependency_session(&self, id: &str, force: bool) -> Result<bool> {
        let Some(snapshot) = self.read_dependency_session_snapshot(id)? else {
            return Ok(false);
        };
        if snapshot.providers.is_empty() && !is_process_alive(snapshot.consumer_pid) {
            return Ok(false);
        }

        // Reap the orphan consumer first (before tearing down its
        // providers). Without this the previous session's orphan
        // consumer (e.g. uvicorn from a SIGKILL'd ato run) keeps
        // holding the configured port and the next session's app
        // target either fails to bind or, worse, the next session's
        // `/docs` probe gets a false-positive ready served by the
        // orphan. The orchestrator's spawn now uses
        // `cmd.process_group(0)` (#121), so signaling the negative
        // pid reaps the consumer's whole subtree (uvicorn + its
        // worker forks, npm + its node, …) atomically. SIGTERM →
        // grace → SIGKILL escalates to handle consumers that trap
        // SIGTERM (e.g. uvicorn waiting for its own startup hook to
        // finish before honoring shutdown).
        let consumer_pid = snapshot.consumer_pid;
        if consumer_pid > 0 && is_process_alive(consumer_pid) {
            let grace = if force {
                Duration::from_millis(0)
            } else {
                Duration::from_secs(3)
            };
            terminate_pgroup_with_escalation(consumer_pid, grace);
        }

        if !snapshot.providers.is_empty() {
            let targets = snapshot
                .providers
                .iter()
                .map(
                    |provider| crate::application::dependency_runtime::TeardownTarget {
                        dep: provider.alias.clone(),
                        pid: provider.pid,
                        state_dir: provider.state_dir.clone(),
                        needs: Vec::new(),
                    },
                )
                .collect();
            let grace = if force {
                Duration::from_secs(0)
            } else {
                Duration::from_secs(10)
            };
            let result = crate::application::dependency_runtime::teardown_reverse_topological(
                targets, grace,
            );
            for provider in &snapshot.providers {
                let _ = crate::application::dependency_runtime::orphan::sweep_stale_sentinel(
                    &provider.state_dir,
                );
            }
            result.with_context(|| format!("Failed to stop dependency contracts for {id}"))?;
        }
        Ok(true)
    }

    /// Sweep orphaned files in `~/.ato/run/` left behind by previous
    /// process instances (#80). Run on every binary startup BEFORE
    /// writing this process's own records, so a fresh `ato-desktop` or
    /// `ato` does not inherit a directory full of pids/sockets/sock-txt
    /// files that point at PIDs no longer alive.
    ///
    /// Three classes of files are reaped:
    ///
    /// 1. **PID files** (`<id>.pid`) — delegated to the existing
    ///    `cleanup_dead_processes_with_details`, which already validates
    ///    pid+identity and tears down the dependency session sidecar.
    /// 2. **Socket files** (`*.sock`) — parsed from filenames like
    ///    `ato-desktop-<pid>.sock`. If the embedded `<pid>` is not
    ///    alive AND the file's mtime is older than `socket_grace`, the
    ///    socket is removed. The grace window keeps a rapid ato-desktop
    ///    restart from racing its own outgoing socket.
    /// 3. **Socket TXT artifacts** (`*.sock.txt`) — same logic as
    ///    sockets; left over from earlier socket-discovery code paths.
    ///
    /// Errors per-entry are absorbed (logged via `tracing::debug`) so a
    /// single permission-denied file does not abort the rest of the
    /// sweep — the goal is "clean what we can on this startup", not
    /// transactional consistency.
    ///
    /// Concurrency: this method does not take a lock. The PID file
    /// path uses identity matching (pid + start_time) inside
    /// `cleanup_dead_processes_with_details`, so deleting another
    /// running ato process's pid file is impossible. Socket files are
    /// keyed by `<pid>` so the alive-check defends against the same
    /// race. v0.5.0 deliberately does not relocate sockets to the OS
    /// runtime dir (#73 v0.6.0 work); a startup-only sweep is the
    /// minimum reliable cleanup we can do at the current path.
    pub fn sweep_run_dir_orphans(&self) -> Result<RunDirSweepReport> {
        let socket_grace = Duration::from_secs(30);
        let mut report = RunDirSweepReport::default();

        // Class 1: stale PID files (re-uses existing helper).
        match self.cleanup_dead_processes_with_details() {
            Ok(cleaned) => report.pid_files_removed = cleaned.len(),
            Err(error) => {
                tracing::debug!(error = %error, "sweep_run_dir_orphans: pid sweep failed");
            }
        }
        match self.sweep_import_preview_sessions(false) {
            Ok(import_report) => report.import_preview = import_report,
            Err(error) => {
                tracing::debug!(error = %error, "sweep_run_dir_orphans: import preview sweep failed");
            }
        }

        // Class 2 & 3: orphaned sockets / sock-txt artifacts.
        let entries = match fs::read_dir(&self.run_dir) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::debug!(error = %error, run_dir = %self.run_dir.display(), "sweep_run_dir_orphans: cannot read run dir");
                return Ok(report);
            }
        };
        let now = SystemTime::now();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            // Match `*.sock` and `*.sock.txt` only. Other extensions
            // (.pid handled above; runtime files we don't manage) are
            // skipped.
            let is_socket = name.ends_with(".sock") || name.ends_with(".sock.txt");
            if !is_socket {
                continue;
            }
            let Some(pid) = parse_socket_pid(name) else {
                continue;
            };
            if is_process_alive(pid) {
                continue;
            }
            // Grace window: only reap sockets older than 30s to avoid
            // racing a sibling ato-desktop that just spawned and is
            // about to bind.
            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(error) => {
                    tracing::debug!(path = %path.display(), error = %error, "sweep_run_dir_orphans: stat failed");
                    continue;
                }
            };
            let mtime = metadata.modified().ok();
            let age = mtime.and_then(|t| now.duration_since(t).ok());
            if age.is_some_and(|a| a < socket_grace) {
                continue;
            }
            match fs::remove_file(&path) {
                Ok(()) => report.sockets_removed += 1,
                Err(error) => {
                    tracing::debug!(path = %path.display(), error = %error, "sweep_run_dir_orphans: socket remove failed");
                }
            }
        }

        Ok(report)
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
                && let Some(filename) = path.file_stem()
                && let Some(id) = filename.to_str()
                && let Ok(info) = self.read_pid(id)
                && (!info.status.is_active()
                    || (info.status.is_active() && !process_info_is_alive(&info)))
                && self.stop_dependency_session(id, false).is_ok()
            {
                let _ = self.delete_pid(id);
                cleaned.push(info);
            }
        }
        // Clean stale port allocations for dead processes
        if let Ok(port_mgr) = super::port_manager::PortManager::new() {
            let _ = port_mgr.gc();
        }

        Ok(cleaned)
    }
}

/// Result of a single `sweep_run_dir_orphans` call (#80). Returned so
/// the caller can surface counts on a debug log line and tests can
/// assert behavior without diffing the on-disk directory.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RunDirSweepReport {
    pub pid_files_removed: usize,
    pub sockets_removed: usize,
    pub import_preview: ImportPreviewSweepReport,
}

/// Parse the embedded `<pid>` from a `*.sock` / `*.sock.txt` filename.
/// Filenames follow `ato-desktop-<pid>.sock` (and the `.sock.txt`
/// variant left behind by the earlier socket-discovery code path).
/// Returns `None` for files that don't match the convention so we
/// don't accidentally reap unrelated artifacts.
fn parse_socket_pid(name: &str) -> Option<i32> {
    let stem = name
        .strip_suffix(".sock.txt")
        .or_else(|| name.strip_suffix(".sock"))?;
    let pid_part = stem.rsplit_once('-').map(|(_, p)| p)?;
    pid_part.parse::<i32>().ok().filter(|p| *p > 0)
}

fn transient_workspace_for_pid_file(pid_path: &Path) -> Option<PathBuf> {
    let content = fs::read_to_string(pid_path).ok()?;
    let info = toml::from_str::<ProcessInfo>(&content).ok()?;
    let manifest_path = info.manifest_path?;
    let workspace = manifest_path.parent()?.to_path_buf();
    let ato_root = pid_path.parent()?.parent()?;
    let gh_run_root = ato_root.join("tmp").join("gh-run");
    if workspace.starts_with(&gh_run_root) {
        Some(workspace)
    } else {
        None
    }
}

fn is_process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }

    #[cfg(unix)]
    unsafe {
        let result = libc::kill(pid, 0);
        if result != 0 && errno() == libc::ESRCH {
            return false;
        }
        !is_unix_zombie(pid)
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
fn is_unix_zombie(pid: i32) -> bool {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output();

    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }

    String::from_utf8_lossy(&output.stdout)
        .trim()
        .chars()
        .next()
        .is_some_and(|state| state == 'Z')
}

#[cfg(unix)]
fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn process_info_is_alive(info: &ProcessInfo) -> bool {
    (is_process_alive(info.pid)
        && process_identity_matches(info)
        && process_start_time_matches(info.pid, info.os_start_time_unix_ms))
        || info.workload_pid.is_some_and(|pid| {
            is_process_alive(pid)
                && process_start_time_matches(pid, info.workload_os_start_time_unix_ms)
        })
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

fn process_start_time_matches(pid: i32, expected_start_time_unix_ms: Option<u64>) -> bool {
    let Some(expected) = expected_start_time_unix_ms else {
        return true;
    };
    let Ok(pid) = u32::try_from(pid) else {
        return false;
    };
    ato_session_core::process::process_start_time_unix_ms(pid) == Some(expected)
}

fn import_preview_session_is_stale(session: &ImportPreviewSession) -> bool {
    if let Some(expires_at) = session.expires_at_unix_ms
        && now_unix_ms_lossy().is_some_and(|now| now >= expires_at)
    {
        return true;
    }
    if !session.shadow_dir.exists() {
        return true;
    }
    if !is_process_alive(session.owner_pid)
        || !process_start_time_matches(session.owner_pid, session.owner_process_start_time_unix_ms)
    {
        return true;
    }
    if !is_process_alive(session.ato_run_pid) {
        return true;
    }
    !process_start_time_matches(
        session.ato_run_pid,
        session.ato_run_process_start_time_unix_ms,
    )
}

fn now_unix_ms_lossy() -> Option<u64> {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_millis() as u64)
}

fn stop_import_preview_session_record(
    session: &ImportPreviewSession,
    force: bool,
) -> ImportPreviewStopResult {
    let ato_run_alive = is_process_alive(session.ato_run_pid);
    let ato_run_owned = ato_run_alive
        && process_start_time_matches(
            session.ato_run_pid,
            session.ato_run_process_start_time_unix_ms,
        );
    if ato_run_alive && !ato_run_owned {
        return ImportPreviewStopResult {
            session: session.clone(),
            status: ImportPreviewStopStatus::NotAtoOwned,
            error: Some("recorded ato run pid is alive but its start time does not match".into()),
        };
    }

    let mut stopped = false;
    let mut errors = Vec::new();
    let mut _live_unverified_pgids = Vec::new();
    let grace = if force {
        Duration::from_millis(0)
    } else {
        Duration::from_secs(3)
    };

    #[cfg(unix)]
    {
        let processes = unix_ps_processes();
        let verified_pgids =
            verified_import_preview_process_groups(session, ato_run_owned, &processes);
        for pgid in verified_pgids {
            if terminate_process_group_id_with_escalation(pgid, grace) {
                stopped = true;
            }
        }
        if stopped {
            let _ = wait_for_process_exit(session.ato_run_pid, 10);
        }
    }

    if ato_run_owned {
        match terminate_import_preview_root(session.ato_run_pid, force) {
            Ok(true) => {
                let _ = wait_for_process_exit(session.ato_run_pid, 10);
                stopped = true;
            }
            Ok(false) => {}
            Err(error) => errors.push(error.to_string()),
        }
    }

    #[cfg(unix)]
    {
        let processes = unix_ps_processes();
        let verified_pgids =
            verified_import_preview_process_groups(session, ato_run_owned, &processes);
        _live_unverified_pgids =
            live_unverified_import_preview_process_groups(session, &verified_pgids, &processes);
    }

    if !errors.is_empty() {
        return ImportPreviewStopResult {
            session: session.clone(),
            status: ImportPreviewStopStatus::Failed,
            error: Some(errors.join("; ")),
        };
    }

    import_preview_stop_outcome(session, stopped, &_live_unverified_pgids)
}

fn import_preview_stop_outcome(
    session: &ImportPreviewSession,
    stopped: bool,
    live_unverified_pgids: &[i32],
) -> ImportPreviewStopResult {
    if !live_unverified_pgids.is_empty() {
        return ImportPreviewStopResult {
            session: session.clone(),
            status: ImportPreviewStopStatus::NotAtoOwned,
            error: Some(format!(
                "recorded process groups could not be verified as Ato-owned: {}",
                live_unverified_pgids
                    .iter()
                    .map(i32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        };
    }

    ImportPreviewStopResult {
        session: session.clone(),
        status: if stopped {
            ImportPreviewStopStatus::Stopped
        } else {
            ImportPreviewStopStatus::AlreadyGone
        },
        error: None,
    }
}

fn terminate_import_preview_root(pid: i32, force: bool) -> Result<bool> {
    #[cfg(windows)]
    {
        return terminate_windows_process_tree(pid, force);
    }

    #[cfg(not(windows))]
    {
        terminate_process(pid, force)
    }
}

fn is_expected_nacelle_commandline(commandline: &str) -> bool {
    let normalized = commandline.to_ascii_lowercase();
    normalized.contains("nacelle") || normalized.contains("capsule run")
}

fn read_process_commandline(pid: i32) -> Option<String> {
    if pid <= 0 {
        return None;
    }

    #[cfg(target_os = "linux")]
    {
        let proc_path = format!("/proc/{pid}/cmdline");
        if let Ok(raw) = fs::read(proc_path)
            && !raw.is_empty()
        {
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
        if cmd.is_empty() { None } else { Some(cmd) }
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

/// SIGTERM the process group rooted at `pid`, wait up to `term_grace`
/// for it to exit, then SIGKILL the same group. Falls back to
/// signaling the pid alone on ESRCH/EPERM (legacy snapshots from
/// before the consumer was spawned with `cmd.process_group(0)`).
///
/// Used by the session-start sweep when the recorded consumer pid is
/// still alive after its parent ato run died (#121). The sweep cannot
/// trust the consumer to exit on SIGTERM alone — uvicorn/node/etc.
/// can have lifespan handlers that block shutdown — so the escalation
/// is mandatory for orphan reap to be reliable.
#[cfg(unix)]
fn terminate_pgroup_with_escalation(pid: i32, term_grace: Duration) {
    if pid <= 0 {
        return;
    }

    fn signal_group_or_pid(pid: i32, signal: libc::c_int) -> bool {
        // pgroup-wide first; on ESRCH/EPERM (no pgroup, or kernel
        // refused the wide-kill) fall back to pid-only.
        let res = unsafe { libc::kill(-pid, signal) };
        if res == 0 {
            return true;
        }
        let err = std::io::Error::last_os_error().raw_os_error();
        if err == Some(libc::ESRCH) || err == Some(libc::EPERM) {
            let res = unsafe { libc::kill(pid, signal) };
            return res == 0;
        }
        false
    }

    let _ = signal_group_or_pid(pid, libc::SIGTERM);

    if term_grace.is_zero() {
        // Force path — escalate immediately.
        let _ = signal_group_or_pid(pid, libc::SIGKILL);
        return;
    }

    let deadline = std::time::Instant::now() + term_grace;
    while std::time::Instant::now() < deadline {
        if !is_process_alive(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if is_process_alive(pid) {
        let _ = signal_group_or_pid(pid, libc::SIGKILL);
    }
}

#[cfg(unix)]
fn terminate_process_group_id_with_escalation(pgid: i32, term_grace: Duration) -> bool {
    if pgid <= 0 {
        return false;
    }

    let signal_group = |signal| unsafe { libc::kill(-pgid, signal) == 0 };
    let mut signaled = signal_group(libc::SIGTERM);
    if term_grace.is_zero() {
        return signal_group(libc::SIGKILL) || signaled;
    }

    let deadline = std::time::Instant::now() + term_grace;
    while std::time::Instant::now() < deadline {
        if unsafe { libc::kill(-pgid, 0) != 0 } {
            return signaled;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    signaled |= signal_group(libc::SIGKILL);
    signaled
}

#[cfg(unix)]
fn verified_import_preview_process_groups(
    session: &ImportPreviewSession,
    ato_run_owned: bool,
    processes: &[UnixPsProcess],
) -> std::collections::BTreeSet<i32> {
    let mut verified = std::collections::BTreeSet::new();
    let ato_run_pgid = if ato_run_owned {
        process_group_id_for_pid(session.ato_run_pid, processes)
    } else {
        None
    };
    if let Some(pgid) = ato_run_pgid.filter(|pgid| *pgid > 0) {
        verified.insert(pgid);
    }
    for pgid in session
        .process_group_ids
        .iter()
        .copied()
        .filter(|pgid| *pgid > 0)
    {
        if Some(pgid) == ato_run_pgid
            || process_group_matches_import_preview_session(pgid, session, processes)
            || (ato_run_owned
                && process_group_descends_from_pid(pgid, session.ato_run_pid, processes))
        {
            verified.insert(pgid);
        }
    }
    verified
}

/// True when any member of `pgid`'s process group descends — via the
/// parent chain — from `ancestor_pid`. On Linux a non-network `ato run`
/// launches its workload through `bwrap --unshare-all`, which leads its
/// OWN process group (nacelle puts the bwrap wrapper in `process_group(0)`)
/// and hides the sandboxed command line and cwd behind a PID namespace, so
/// neither the `ATO_IMPORT_SESSION_ID` marker nor a `shadow_dir` reference
/// is visible to the host `ps`. The ppid chain back to the Ato-owned
/// `ato run` pid is the proof that the group still belongs to this session.
/// Callers gate this on `ato_run_owned` (the root pid + start time matched),
/// so it can only ever verify groups rooted under a confirmed Ato process —
/// never a recycled or unrelated pid.
#[cfg(unix)]
fn process_group_descends_from_pid(
    pgid: i32,
    ancestor_pid: i32,
    processes: &[UnixPsProcess],
) -> bool {
    if pgid <= 0 || ancestor_pid <= 0 {
        return false;
    }
    processes
        .iter()
        .filter(|process| process.pgid == pgid)
        .any(|process| process_descends_from_pid(process.pid, ancestor_pid, processes))
}

/// Walk the parent chain from `pid` looking for `ancestor_pid`. Bounded by
/// the process count so a malformed/cyclic ppid graph can never loop
/// forever; reaching pid 0/1 (no recorded parent) ends the walk.
#[cfg(unix)]
fn process_descends_from_pid(pid: i32, ancestor_pid: i32, processes: &[UnixPsProcess]) -> bool {
    let mut current = pid;
    for _ in 0..processes.len().saturating_add(1) {
        if current == ancestor_pid {
            return true;
        }
        let Some(parent) = processes
            .iter()
            .find(|process| process.pid == current)
            .map(|process| process.ppid)
        else {
            return false;
        };
        if parent <= 0 || parent == current {
            return false;
        }
        current = parent;
    }
    false
}

#[cfg(unix)]
fn live_unverified_import_preview_process_groups(
    session: &ImportPreviewSession,
    verified_pgids: &std::collections::BTreeSet<i32>,
    processes: &[UnixPsProcess],
) -> Vec<i32> {
    session
        .process_group_ids
        .iter()
        .copied()
        .filter(|pgid| *pgid > 0)
        .filter(|pgid| !verified_pgids.contains(pgid))
        .filter(|pgid| processes.iter().any(|process| process.pgid == *pgid))
        .collect()
}

#[cfg(unix)]
fn process_group_id_for_pid(pid: i32, processes: &[UnixPsProcess]) -> Option<i32> {
    processes
        .iter()
        .find(|process| process.pid == pid)
        .map(|process| process.pgid)
}

#[cfg(unix)]
fn process_group_matches_import_preview_session(
    pgid: i32,
    session: &ImportPreviewSession,
    processes: &[UnixPsProcess],
) -> bool {
    if pgid <= 0 {
        return false;
    }
    processes.iter().any(|process| {
        process.pgid == pgid && process_matches_import_preview_session_process(process, session)
    })
}

#[cfg(unix)]
fn process_current_working_dir(pid: i32) -> Option<PathBuf> {
    if pid <= 0 {
        return None;
    }

    #[cfg(target_os = "linux")]
    {
        fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }

    #[cfg(not(target_os = "linux"))]
    {
        let output = Command::new("lsof")
            .args(["-a", "-d", "cwd", "-Fn", "-p", &pid.to_string()])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Some(path) = line.strip_prefix('n')
                && !path.is_empty()
            {
                return Some(PathBuf::from(path));
            }
        }
        None
    }
}

#[cfg(unix)]
fn process_matches_import_preview_session_process(
    process: &UnixPsProcess,
    session: &ImportPreviewSession,
) -> bool {
    let marker = format!("ATO_IMPORT_SESSION_ID={}", session.run_session_id);
    process.command.contains(&marker) || process_references_path(process, &session.shadow_dir)
}

#[cfg(unix)]
fn process_references_path(process: &UnixPsProcess, path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    process.command.contains(path_str.as_ref())
        || process_current_working_dir(process.pid).is_some_and(|cwd| cwd.starts_with(path))
}

#[cfg(unix)]
fn sweep_import_env_process_groups(force: bool, sessions: &[ImportPreviewSession]) -> usize {
    let processes = unix_ps_processes();
    let pgids = import_preview_env_sweep_candidates(sessions, &processes);
    let grace = if force {
        Duration::from_millis(0)
    } else {
        Duration::from_secs(3)
    };
    pgids
        .into_iter()
        .filter(|pgid| terminate_process_group_id_with_escalation(*pgid, grace))
        .count()
}

#[cfg(unix)]
fn import_preview_env_sweep_candidates(
    sessions: &[ImportPreviewSession],
    processes: &[UnixPsProcess],
) -> std::collections::BTreeSet<i32> {
    let protected_pgids = active_import_preview_process_groups(sessions, processes);
    let mut pgids = std::collections::BTreeSet::new();
    for process in processes {
        if process.pgid <= 0
            || protected_pgids.contains(&process.pgid)
            || command_is_known_non_target(&process.command)
        {
            continue;
        }
        if process_matches_import_preview_env_sweep(process, sessions) {
            pgids.insert(process.pgid);
        }
    }
    pgids
}

#[cfg(unix)]
fn active_import_preview_process_groups(
    sessions: &[ImportPreviewSession],
    processes: &[UnixPsProcess],
) -> std::collections::BTreeSet<i32> {
    let mut protected = std::collections::BTreeSet::new();
    for session in sessions
        .iter()
        .filter(|session| !import_preview_session_is_stale(session))
    {
        protected.extend(
            session
                .process_group_ids
                .iter()
                .copied()
                .filter(|pgid| *pgid > 0),
        );
        if let Some(pgid) =
            process_group_id_for_pid(session.ato_run_pid, processes).filter(|pgid| *pgid > 0)
        {
            protected.insert(pgid);
        }
    }
    protected
}

#[cfg(unix)]
fn process_matches_import_preview_env_sweep(
    process: &UnixPsProcess,
    sessions: &[ImportPreviewSession],
) -> bool {
    if let Some(session_id) = command_env_marker_value(&process.command, "ATO_IMPORT_SESSION_ID=") {
        return !sessions.iter().any(|session| {
            session.run_session_id == session_id && !import_preview_session_is_stale(session)
        });
    }

    let references_import_workspace = process_references_import_workspace(process, sessions);
    if !references_import_workspace {
        return false;
    }

    if command_env_marker_value(&process.command, "ATO_IMPORT_PROBE_ID=").is_some() {
        return !active_session_owns_process(process, sessions);
    }

    !active_session_owns_process(process, sessions)
}

#[cfg(unix)]
fn command_env_marker_value<'a>(command: &'a str, marker: &str) -> Option<&'a str> {
    command
        .split_whitespace()
        .find_map(|token| token.strip_prefix(marker))
        .filter(|value| !value.is_empty())
}

#[cfg(unix)]
fn active_session_owns_process(process: &UnixPsProcess, sessions: &[ImportPreviewSession]) -> bool {
    sessions
        .iter()
        .filter(|session| !import_preview_session_is_stale(session))
        .any(|session| process_matches_import_preview_session_process(process, session))
}

#[cfg(unix)]
fn process_references_import_workspace(
    process: &UnixPsProcess,
    sessions: &[ImportPreviewSession],
) -> bool {
    sessions
        .iter()
        .any(|session| process_references_path(process, &session.shadow_dir))
        || command_mentions_import_workspace(&process.command)
        || process_current_working_dir(process.pid)
            .is_some_and(|cwd| path_looks_like_import_workspace(&cwd))
}

#[cfg(unix)]
fn command_mentions_import_workspace(command: &str) -> bool {
    command.contains(".tmp/ato-import/")
        || command.contains(".tmp\\ato-import\\")
        || command.contains("/.tmp/ato-import")
        || command.contains("\\.tmp\\ato-import")
}

#[cfg(unix)]
fn path_looks_like_import_workspace(path: &Path) -> bool {
    let mut saw_tmp = false;
    for component in path.components() {
        let component = component.as_os_str().to_string_lossy();
        if component == ".tmp" {
            saw_tmp = true;
            continue;
        }
        if saw_tmp && component.starts_with("ato-import") {
            return true;
        }
    }
    false
}

#[cfg(unix)]
fn command_is_known_non_target(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    lower.contains("podman machine")
        || lower.contains("podman-machine")
        || lower.contains("/usr/sbin/fseventsd")
        || lower.contains(" fseventsd")
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct UnixPsProcess {
    pid: i32,
    ppid: i32,
    pgid: i32,
    command: String,
}

#[cfg(unix)]
fn unix_ps_processes() -> Vec<UnixPsProcess> {
    let output = Command::new("ps")
        .args(["eww", "-axo", "pid=,ppid=,pgid=,command="])
        .output()
        .or_else(|_| {
            Command::new("ps")
                .args(["-axo", "pid=,ppid=,pgid=,command="])
                .output()
        });
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_unix_ps_process_line)
        .collect()
}

#[cfg(unix)]
fn parse_unix_ps_process_line(line: &str) -> Option<UnixPsProcess> {
    let trimmed = line.trim();
    let mut parts = trimmed.split_whitespace();
    let pid = parts.next()?.parse().ok()?;
    let ppid = parts.next()?.parse().ok()?;
    let pgid = parts.next()?.parse().ok()?;
    let command = parts.collect::<Vec<_>>().join(" ");
    Some(UnixPsProcess {
        pid,
        ppid,
        pgid,
        command,
    })
}
#[cfg(not(unix))]
fn terminate_pgroup_with_escalation(pid: i32, _term_grace: Duration) {
    // Windows: no process-group concept that maps cleanly. Fall back
    // to terminating the pid; consumer subtrees on Windows are taken
    // care of by Job Objects elsewhere.
    let _ = terminate_process(pid, true);
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

#[cfg(windows)]
fn terminate_windows_process_tree(pid: i32, force: bool) -> Result<bool> {
    if pid <= 0 {
        return Ok(false);
    }

    let mut command = Command::new("taskkill");
    command.arg("/PID").arg(pid.to_string()).arg("/T");
    if force {
        command.arg("/F");
    }
    let status = command
        .status()
        .with_context(|| format!("Failed to execute taskkill /T for PID {}", pid))?;

    if status.success() {
        return Ok(true);
    }

    if !is_process_alive(pid) {
        Ok(false)
    } else {
        Err(anyhow::anyhow!(
            "Failed to terminate process tree rooted at {}",
            pid
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
            let run_dir = capsule_core::common::paths::ato_path_or_workspace_tmp("run");
            Self { run_dir }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_socket_pid_handles_real_filenames() {
        // Standard `*.sock` form left by ato-desktop.
        assert_eq!(parse_socket_pid("ato-desktop-12345.sock"), Some(12345));
        // `*.sock.txt` form from the earlier socket-discovery code path.
        assert_eq!(parse_socket_pid("ato-desktop-99.sock.txt"), Some(99));
        // session-pid files have a different shape (session-<pid>.pid)
        // and are NOT reaped by the socket sweep — they go through the
        // pid-file branch.
        assert_eq!(parse_socket_pid("ato-desktop-session-42.pid"), None);
        // Non-matching: no trailing -<pid>, weird suffix, zero pid.
        assert_eq!(parse_socket_pid("just.sock"), None);
        assert_eq!(parse_socket_pid("ato-desktop-abc.sock"), None);
        assert_eq!(parse_socket_pid("ato-desktop-0.sock"), None);
        // We only handle .sock / .sock.txt — .lock / .json must not match.
        assert_eq!(parse_socket_pid("ato-desktop-1.lock"), None);
    }

    #[test]
    fn sweep_run_dir_orphans_reaps_dead_socket_past_grace_period() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let run_dir = tmp.path().join("run");
        fs::create_dir_all(&run_dir).expect("mkdir run");
        let pm = ProcessManager {
            run_dir: run_dir.clone(),
        };

        // Pid 1 always exists on unix (init); do NOT use it. Pid that
        // certainly doesn't exist on any reasonable system: 2^31 - 1.
        let dead_pid = i32::MAX;
        let stale_socket = run_dir.join(format!("ato-desktop-{dead_pid}.sock"));
        fs::write(&stale_socket, b"").expect("write stale socket");

        // Make the socket older than the 30s grace window.
        let one_hour_ago = SystemTime::now() - Duration::from_secs(3600);
        let _ = filetime::set_file_mtime(
            &stale_socket,
            filetime::FileTime::from_system_time(one_hour_ago),
        );

        let report = pm.sweep_run_dir_orphans().expect("sweep");
        assert_eq!(report.sockets_removed, 1);
        assert!(!stale_socket.exists());
    }

    #[test]
    fn sweep_run_dir_orphans_preserves_recent_socket_within_grace() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let run_dir = tmp.path().join("run");
        fs::create_dir_all(&run_dir).expect("mkdir run");
        let pm = ProcessManager {
            run_dir: run_dir.clone(),
        };

        // Dead pid + fresh mtime → kept (defends against rapid restart
        // race where the new process binds the socket within the
        // first 30s after the old one exits).
        let dead_pid = i32::MAX;
        let fresh_socket = run_dir.join(format!("ato-desktop-{dead_pid}.sock"));
        fs::write(&fresh_socket, b"").expect("write fresh socket");
        // mtime defaults to "now" on write — explicitly leave it.

        let report = pm.sweep_run_dir_orphans().expect("sweep");
        assert_eq!(report.sockets_removed, 0);
        assert!(fresh_socket.exists());
    }

    #[test]
    fn sweep_import_preview_sessions_removes_stale_dead_session() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let run_dir = tmp.path().join("run");
        fs::create_dir_all(&run_dir).expect("mkdir run");
        let pm = ProcessManager {
            run_dir: run_dir.clone(),
        };
        let session = test_import_preview_session("preview-stale", i32::MAX, i32::MAX, false);
        pm.write_import_preview_session(&session)
            .expect("write session");

        let report = pm.sweep_import_preview_sessions(false).expect("sweep");

        assert_eq!(report.stale_sessions_already_gone, 1);
        assert!(
            pm.read_import_preview_session("preview-stale")
                .expect("read")
                .is_none()
        );
        let _ = fs::remove_dir_all(
            std::env::current_dir()
                .expect("cwd")
                .join(".tmp")
                .join("test-import-preview-preview-stale"),
        );
    }

    #[test]
    fn sweep_import_preview_sessions_keeps_live_owned_session() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let run_dir = tmp.path().join("run");
        fs::create_dir_all(&run_dir).expect("mkdir run");
        let pm = ProcessManager {
            run_dir: run_dir.clone(),
        };
        let pid = std::process::id() as i32;
        let session = test_import_preview_session("preview-live", pid, pid, true);
        pm.write_import_preview_session(&session)
            .expect("write session");

        let report = pm.sweep_import_preview_sessions(false).expect("sweep");

        assert_eq!(report.active_sessions_kept, 1);
        assert!(
            pm.read_import_preview_session("preview-live")
                .expect("read")
                .is_some()
        );
        let workspace = std::env::current_dir()
            .expect("cwd")
            .join(".tmp")
            .join("test-import-preview-preview-live");
        let active = pm
            .active_import_preview_session_for_workspace(&workspace)
            .expect("active session");
        assert_eq!(
            active
                .as_ref()
                .map(|session| session.run_session_id.as_str()),
            Some("preview-live")
        );
        let _ = fs::remove_dir_all(
            std::env::current_dir()
                .expect("cwd")
                .join(".tmp")
                .join("test-import-preview-preview-live"),
        );
    }

    #[test]
    #[cfg(unix)]
    fn verified_import_preview_process_groups_require_session_proof_for_saved_pgids() {
        let mut session =
            test_import_preview_session("preview-unverified", i32::MAX, i32::MAX, true);
        session.process_group_ids = vec![777];
        let processes = vec![UnixPsProcess {
            pid: 4242,
            ppid: 1,
            pgid: 777,
            command: "python3 unrelated_server.py".to_string(),
        }];

        let verified = verified_import_preview_process_groups(&session, false, &processes);
        assert!(verified.is_empty());
        assert_eq!(
            live_unverified_import_preview_process_groups(&session, &verified, &processes),
            vec![777]
        );
    }

    #[test]
    #[cfg(unix)]
    fn verified_import_preview_process_groups_accept_exact_session_marker() {
        let mut session = test_import_preview_session("preview-marker", i32::MAX, i32::MAX, true);
        session.process_group_ids = vec![888];
        let processes = vec![UnixPsProcess {
            pid: 5151,
            ppid: 1,
            pgid: 888,
            command: format!(
                "ATO_IMPORT_SESSION_ID={} python3 {}",
                session.run_session_id,
                session.shadow_dir.display()
            ),
        }];

        let verified = verified_import_preview_process_groups(&session, false, &processes);
        assert_eq!(verified.into_iter().collect::<Vec<_>>(), vec![888]);
    }

    #[test]
    #[cfg(unix)]
    fn verified_import_preview_process_groups_accept_sandboxed_descendant_group() {
        // Linux repro: the bwrap-sandboxed server leads its OWN process group
        // (pgid 888) and hides its command line / cwd behind a PID namespace.
        // The genuinely unverifiable member is the namespaced grandchild
        // (pid 5151): its host-visible argv uses guest paths and its
        // /proc/<pid>/cwd resolves inside the namespace mount, so it carries
        // neither the ATO_IMPORT_SESSION_ID marker (stripped by bwrap
        // --clearenv) nor a shadow_dir reference. (The real bwrap monitor argv
        // does carry the host shadow_dir via `--ro-bind <shadow_dir> /app`, so
        // it may already match on shadow_dir; this fixture models bwrap without
        // it to isolate the descendant proof. Either way the only proof the
        // namespaced workload belongs to the session is the ppid chain back to
        // the Ato-owned `ato run` (pid 4000, pgid 4000).)
        let mut session = test_import_preview_session("preview-sandboxed", i32::MAX, 4000, true);
        session.process_group_ids = vec![4000, 888];
        let processes = vec![
            // The Ato-owned outer `ato run` leads pgid 4000.
            UnixPsProcess {
                pid: 4000,
                ppid: 1,
                pgid: 4000,
                command: format!(
                    "ato run {} --yes ATO_IMPORT_SESSION_ID={}",
                    session.shadow_dir.display(),
                    session.run_session_id
                ),
            },
            // bwrap wrapper: child of the outer run but in its OWN group (888).
            UnixPsProcess {
                pid: 5000,
                ppid: 4000,
                pgid: 888,
                command: "bwrap --unshare-all --die-with-parent".to_string(),
            },
            // Sandboxed server: namespaced, no session marker, no shadow_dir.
            UnixPsProcess {
                pid: 5151,
                ppid: 5000,
                pgid: 888,
                command: "python3 keep_alive_server.py 1111".to_string(),
            },
        ];

        let verified = verified_import_preview_process_groups(&session, true, &processes);
        assert_eq!(
            verified.into_iter().collect::<Vec<_>>(),
            vec![888, 4000],
            "the sandboxed descendant group must verify as Ato-owned"
        );
    }

    #[test]
    #[cfg(unix)]
    fn verified_import_preview_process_groups_reject_descendant_when_root_unowned() {
        // Same topology, but the root pid is NOT confirmed Ato-owned
        // (ato_run_owned = false). The descendant proof must be ignored —
        // fail closed rather than kill a group rooted under an unverified pid.
        let mut session = test_import_preview_session("preview-unowned-root", i32::MAX, 4000, true);
        session.process_group_ids = vec![888];
        let processes = vec![
            UnixPsProcess {
                pid: 4000,
                ppid: 1,
                pgid: 4000,
                command: "ato run --yes".to_string(),
            },
            UnixPsProcess {
                pid: 5151,
                ppid: 4000,
                pgid: 888,
                command: "python3 keep_alive_server.py 1111".to_string(),
            },
        ];

        let verified = verified_import_preview_process_groups(&session, false, &processes);
        assert!(verified.is_empty());
        assert_eq!(
            live_unverified_import_preview_process_groups(&session, &verified, &processes),
            vec![888]
        );
    }

    #[test]
    fn import_preview_stop_outcome_prefers_unverified_groups_over_stopped() {
        let session =
            test_import_preview_session("preview-stop-outcome", i32::MAX, i32::MAX, false);
        let result = import_preview_stop_outcome(&session, true, &[777, 888]);

        assert_eq!(result.status, ImportPreviewStopStatus::NotAtoOwned);
        assert_eq!(
            result.error.as_deref(),
            Some("recorded process groups could not be verified as Ato-owned: 777, 888")
        );
    }

    #[test]
    #[cfg(unix)]
    fn import_preview_env_sweep_candidates_ignore_run_session_only_marker() {
        let processes = vec![UnixPsProcess {
            pid: 4242,
            ppid: 1,
            pgid: 777,
            command: "ATO_RUN_SESSION_ID=run-123 python3 server.py".to_string(),
        }];

        let selected = import_preview_env_sweep_candidates(&[], &processes);

        assert!(selected.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn import_preview_env_sweep_candidates_skip_active_import_session_marker() {
        let mut session = test_import_preview_session(
            "preview-active-marker",
            std::process::id() as i32,
            std::process::id() as i32,
            true,
        );
        session.process_group_ids = vec![888];
        let processes = vec![UnixPsProcess {
            pid: 5151,
            ppid: 1,
            pgid: 888,
            command: format!(
                "ATO_IMPORT_SESSION_ID={} python3 {}",
                session.run_session_id,
                session.shadow_dir.display()
            ),
        }];

        let selected = import_preview_env_sweep_candidates(&[session], &processes);

        assert!(selected.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn import_preview_env_sweep_candidates_select_missing_import_session_marker() {
        let processes = vec![UnixPsProcess {
            pid: 6262,
            ppid: 1,
            pgid: 999,
            command: "ATO_IMPORT_SESSION_ID=preview-missing python3 stale_server.py".to_string(),
        }];

        let selected = import_preview_env_sweep_candidates(&[], &processes);

        assert_eq!(selected.into_iter().collect::<Vec<_>>(), vec![999]);
    }

    fn test_import_preview_session(
        id: &str,
        owner_pid: i32,
        ato_run_pid: i32,
        create_shadow_dir: bool,
    ) -> ImportPreviewSession {
        let base = std::env::current_dir()
            .expect("cwd")
            .join(".tmp")
            .join(format!("test-import-preview-{id}"));
        let shadow_dir = base.join("shadow");
        if create_shadow_dir {
            fs::create_dir_all(&shadow_dir).expect("shadow dir");
        }
        let now = now_unix_ms_lossy().expect("now");
        ImportPreviewSession {
            run_session_id: id.to_string(),
            owner_kind: "cli".to_string(),
            owner_pid,
            owner_process_start_time_unix_ms: owner_pid
                .try_into()
                .ok()
                .and_then(ato_session_core::process::process_start_time_unix_ms),
            ato_run_pid,
            ato_run_process_start_time_unix_ms: ato_run_pid
                .try_into()
                .ok()
                .and_then(ato_session_core::process::process_start_time_unix_ms),
            process_group_ids: Vec::new(),
            primary_port: None,
            primary_url: None,
            shadow_dir,
            log_path: base.join("preview.log"),
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
            expires_at_unix_ms: None,
            readiness_state: "ready".to_string(),
            cleanup_policy: "keep_until_explicit_stop".to_string(),
            last_sweep_status: None,
            last_sweep_error: None,
        }
    }

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
            os_start_time_unix_ms: None,
            workload_os_start_time_unix_ms: None,
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
            os_start_time_unix_ms: None,
            workload_os_start_time_unix_ms: None,
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

    #[cfg(unix)]
    #[test]
    fn workload_pid_keeps_nacelle_record_active_and_is_stopped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let run_dir = tmp.path().join("run");
        fs::create_dir_all(&run_dir).expect("create run dir");
        let pm = ProcessManager { run_dir };

        let mut workload = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn workload");
        let info = ProcessInfo {
            id: "capsule-workload".to_string(),
            name: "demo".to_string(),
            pid: 0,
            workload_pid: Some(workload.id() as i32),
            status: ProcessStatus::Ready,
            runtime: "nacelle".to_string(),
            start_time: SystemTime::UNIX_EPOCH,
            os_start_time_unix_ms: None,
            workload_os_start_time_unix_ms: None,
            manifest_path: None,
            scoped_id: None,
            target_label: Some("app".to_string()),
            requested_port: None,
            log_path: None,
            ready_at: Some(SystemTime::UNIX_EPOCH),
            last_event: Some("ready".to_string()),
            last_error: None,
            exit_code: None,
        };
        pm.write_pid(&info).expect("write pid");

        let read = pm.read_pid("capsule-workload").expect("read pid");
        assert_eq!(read.status, ProcessStatus::Ready);

        let stopped = pm
            .stop_process("capsule-workload", true)
            .expect("stop workload");
        assert!(stopped);
        let _ = workload.wait().expect("wait workload");
        assert!(!pm.pid_file_path("capsule-workload").exists());
    }

    #[test]
    fn delete_pid_removes_transient_github_workspace() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let run_dir = tmp.path().join("home/.ato/run");
        fs::create_dir_all(&run_dir).expect("create run dir");
        let gh_workspace = tmp.path().join("home/.ato/tmp/gh-run/demo-123");
        fs::create_dir_all(&gh_workspace).expect("create gh workspace");
        fs::write(
            gh_workspace.join("capsule.toml"),
            "name = \"demo\"
",
        )
        .expect("write manifest");
        let pm = ProcessManager { run_dir };
        let info = ProcessInfo {
            id: "capsule-transient".to_string(),
            name: "demo".to_string(),
            pid: 0,
            workload_pid: None,
            status: ProcessStatus::Stopped,
            runtime: "nacelle".to_string(),
            start_time: SystemTime::UNIX_EPOCH,
            os_start_time_unix_ms: None,
            workload_os_start_time_unix_ms: None,
            manifest_path: Some(gh_workspace.join("capsule.toml")),
            scoped_id: None,
            target_label: Some("app".to_string()),
            requested_port: None,
            log_path: None,
            ready_at: None,
            last_event: None,
            last_error: None,
            exit_code: None,
        };
        pm.write_pid(&info).expect("write pid");

        pm.delete_pid("capsule-transient").expect("delete pid");
        assert!(!gh_workspace.exists());
    }

    #[test]
    fn dependency_session_snapshot_round_trips_through_run_sessions_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let run_dir = tmp.path().join("run");
        fs::create_dir_all(&run_dir).expect("create run dir");
        let pm = ProcessManager { run_dir };
        let snapshot = DependencyContractSessionSnapshot {
            session_id: "capsule-123".to_string(),
            consumer_pid: 123,
            providers: vec![DependencyContractProcessInfo {
                alias: "db".to_string(),
                pid: 456,
                state_dir: tmp.path().join("state/db"),
                resolved: "capsule://github.com/Koh0920/ato-postgres@65b3ee5".to_string(),
                allocated_port: Some(5432),
                log_path: Some(tmp.path().join("db.log")),
                runtime_export_keys: vec!["DATABASE_URL".to_string()],
            }],
        };

        let path = pm
            .write_dependency_session_snapshot("capsule-123", &snapshot)
            .expect("write dependency session snapshot");
        assert!(path.ends_with("run-sessions/capsule-123/graph.json"));

        let loaded = pm
            .read_dependency_session_snapshot("capsule-123")
            .expect("read dependency session snapshot")
            .expect("snapshot exists");
        assert_eq!(loaded, snapshot);

        pm.delete_pid("capsule-123")
            .expect("delete pid also removes session");
        assert!(
            pm.read_dependency_session_snapshot("capsule-123")
                .expect("read after delete")
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "flaky: spawns subprocess + races SIGTERM/try_wait, same #82 class as the session-level sibling tests"]
    fn stop_process_stops_dependency_session_when_pid_file_is_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let run_dir = tmp.path().join("run");
        fs::create_dir_all(&run_dir).expect("create run dir");
        let pm = ProcessManager { run_dir };
        let mut provider = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn provider");
        let snapshot = DependencyContractSessionSnapshot {
            session_id: "capsule-no-pid".to_string(),
            consumer_pid: 999_999_999,
            providers: vec![DependencyContractProcessInfo {
                alias: "db".to_string(),
                pid: provider.id() as i32,
                state_dir: tmp.path().join("state/db"),
                resolved: "capsule://github.com/Koh0920/ato-postgres@65b3ee5".to_string(),
                allocated_port: Some(5432),
                log_path: Some(tmp.path().join("db.log")),
                runtime_export_keys: vec!["DATABASE_URL".to_string()],
            }],
        };
        pm.write_dependency_session_snapshot("capsule-no-pid", &snapshot)
            .expect("write dependency session snapshot");

        assert!(
            pm.stop_process("capsule-no-pid", true)
                .expect("stop process")
        );
        assert!(provider.try_wait().expect("provider wait").is_some());
        assert!(
            pm.read_dependency_session_snapshot("capsule-no-pid")
                .expect("read after stop")
                .is_none()
        );
    }

    #[test]
    fn host_fallback_process_info_round_trips() {
        let info = ProcessInfo {
            id: "test-host-fallback".to_string(),
            name: "json-server".to_string(),
            pid: 22222,
            workload_pid: None,
            status: ProcessStatus::Ready,
            runtime: "source/node [host-fallback]".to_string(),
            start_time: SystemTime::UNIX_EPOCH,
            os_start_time_unix_ms: None,
            workload_os_start_time_unix_ms: None,
            manifest_path: Some(PathBuf::from("/workspace/capsule.toml")),
            scoped_id: Some("typicode/json-server".to_string()),
            target_label: Some("app".to_string()),
            requested_port: Some(3000),
            log_path: None,
            ready_at: Some(SystemTime::UNIX_EPOCH),
            last_event: Some("ready".to_string()),
            last_error: None,
            exit_code: None,
        };

        let serialized = toml::to_string(&info).expect("serialize host fallback process info");
        let deserialized: ProcessInfo =
            toml::from_str(&serialized).expect("deserialize host fallback process info");

        assert_eq!(deserialized.runtime, "source/node [host-fallback]");
        assert_eq!(deserialized.requested_port, Some(3000));
        assert_eq!(deserialized.status, ProcessStatus::Ready);
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
            os_start_time_unix_ms: None,
            workload_os_start_time_unix_ms: None,
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
            os_start_time_unix_ms: None,
            workload_os_start_time_unix_ms: None,
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
            Some("/usr/bin/ato capsule run ./sample")
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
            "source/node [host-fallback]",
            None
        ));
        assert!(runtime_identity_matches(
            "host",
            Some("/usr/bin/python app.py")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn stop_process_terminates_host_fallback_record() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let run_dir = tmp.path().join("run");
        fs::create_dir_all(&run_dir).expect("create run dir");
        let pm = ProcessManager { run_dir };

        let mut child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");

        let info = ProcessInfo {
            id: "host-fallback-stop".to_string(),
            name: "json-server".to_string(),
            pid: child.id() as i32,
            workload_pid: None,
            status: ProcessStatus::Running,
            runtime: "source/node [host-fallback]".to_string(),
            start_time: SystemTime::UNIX_EPOCH,
            os_start_time_unix_ms: None,
            workload_os_start_time_unix_ms: None,
            manifest_path: None,
            scoped_id: Some("typicode/json-server".to_string()),
            target_label: Some("app".to_string()),
            requested_port: Some(3000),
            log_path: None,
            ready_at: Some(SystemTime::UNIX_EPOCH),
            last_event: Some("ready".to_string()),
            last_error: None,
            exit_code: None,
        };

        pm.write_pid(&info).expect("write pid file");

        let stopped = pm
            .stop_process("host-fallback-stop", true)
            .expect("stop process");
        assert!(stopped);
        assert!(!pm.pid_file_path("host-fallback-stop").exists());
        assert!(child.try_wait().expect("try_wait").is_some());
    }
}
