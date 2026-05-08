#![allow(dead_code)]

use std::error::Error as StdError;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Error;
use capsule_core::execution_plan::error::{
    CleanupActionRecord, CleanupActionStatus, CleanupStatus,
};

use crate::application::pipeline::hourglass::HourglassPhase;

type CleanupHandle = usize;

trait CleanupAction: Send {
    fn run(self: Box<Self>) -> CleanupActionRecord;
}

impl<F> CleanupAction for F
where
    F: FnOnce() -> CleanupActionRecord + Send + 'static,
{
    fn run(self: Box<Self>) -> CleanupActionRecord {
        (*self)()
    }
}

struct CleanupEntry {
    id: CleanupHandle,
    action: Option<Box<dyn CleanupAction + Send>>,
}

#[derive(Debug, Clone)]
pub(crate) struct CleanupReport {
    pub(crate) status: CleanupStatus,
    pub(crate) actions: Vec<CleanupActionRecord>,
}

impl Default for CleanupReport {
    fn default() -> Self {
        Self {
            status: CleanupStatus::NotRequired,
            actions: Vec::new(),
        }
    }
}

impl CleanupReport {
    fn from_actions(actions: Vec<CleanupActionRecord>) -> Self {
        let status = if actions.is_empty() {
            CleanupStatus::NotRequired
        } else if actions
            .iter()
            .all(|action| action.status == CleanupActionStatus::Succeeded)
        {
            CleanupStatus::Complete
        } else {
            CleanupStatus::Partial
        };

        Self { status, actions }
    }
}

#[derive(Default)]
pub(crate) struct CleanupJournal {
    entries: Vec<CleanupEntry>,
    next_id: CleanupHandle,
}

impl CleanupJournal {
    pub(crate) fn register<F>(&mut self, action: F) -> CleanupHandle
    where
        F: FnOnce() -> CleanupActionRecord + Send + 'static,
    {
        let id = self.next_id;
        self.next_id += 1;
        self.entries.push(CleanupEntry {
            id,
            action: Some(Box::new(action)),
        });
        id
    }

    pub(crate) fn commit(&mut self, handle: CleanupHandle) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == handle) {
            entry.action = None;
        }
    }

    pub(crate) fn unwind(&mut self) -> CleanupReport {
        let mut actions = Vec::new();

        while let Some(mut entry) = self.entries.pop() {
            if let Some(action) = entry.action.take() {
                actions.push(action.run());
            }
        }

        CleanupReport::from_actions(actions)
    }
}

#[derive(Clone, Default)]
struct SharedCleanupJournal {
    inner: Arc<Mutex<CleanupJournal>>,
}

impl SharedCleanupJournal {
    fn register<F>(&self, action: F) -> CleanupHandle
    where
        F: FnOnce() -> CleanupActionRecord + Send + 'static,
    {
        let mut journal = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        journal.register(action)
    }

    fn commit(&self, handle: CleanupHandle) {
        let mut journal = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        journal.commit(handle);
    }

    fn unwind(&self) -> CleanupReport {
        let mut journal = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        journal.unwind()
    }
}

// ---------------------------------------------------------------------------
// SIGINT cleanup registry
//
// When a pipeline attempt is active, we store a weak reference to its cleanup
// journal here. If the user presses Ctrl+C (SIGINT), `run_sigint_cleanup()`
// is called from the ctrlc handler, which unwinds any registered dirs before
// the process exits.
// ---------------------------------------------------------------------------

static SIGINT_JOURNAL: OnceLock<Mutex<Option<SharedCleanupJournal>>> = OnceLock::new();

fn sigint_journal() -> &'static Mutex<Option<SharedCleanupJournal>> {
    SIGINT_JOURNAL.get_or_init(|| Mutex::new(None))
}

/// Called from the ctrlc handler to clean up in-flight run artifacts.
/// Best-effort: errors are silently ignored so the process can exit cleanly.
///
/// Order matters here: dep-contract teardown FIRST (#24 — provider
/// processes need to die before we touch their state dirs), THEN
/// pipeline-attempt artifact cleanup (temp dirs etc.).
pub(crate) fn run_sigint_cleanup() {
    drain_dep_contract_teardown_hooks();
    let journal = sigint_journal()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
    if let Some(j) = journal {
        j.unwind();
    }
}

// ---------------------------------------------------------------------------
// Dep-contract SIGINT teardown registry (#24)
//
// `process::exit(130)` from the ctrlc handler skips Drop, so the
// `DependencyContractGuard::Drop` -> `RunningGraph::teardown` path that
// would normally kill provider/consumer processes is skipped on Ctrl+C.
// Without this hook, postgres / consumer / intermediate shells survive
// the parent and the next `ato run` of the same capsule trips on a
// stale `<PGDATA>/postmaster.pid`.
//
// Each `DependencyContractGuard` registers a teardown closure here on
// creation and removes it on normal Drop. The closure captures only the
// per-dep `(pid, state_dir, alias)` tuples needed by
// `teardown_reverse_topological` + `sweep_stale_sentinel` — NOT the
// `RunningGraph` itself, which is not `Send` in a way the static
// registry can hold across threads. SIGINT therefore runs the same
// teardown the Drop path would have run, just from a different code
// path.
// ---------------------------------------------------------------------------

type DepContractTeardownHook = Box<dyn FnOnce() + Send + 'static>;

static SIGINT_DEP_CONTRACT_HOOKS: OnceLock<Mutex<Vec<(u64, DepContractTeardownHook)>>> =
    OnceLock::new();
static DEP_CONTRACT_HOOK_NEXT_ID: OnceLock<Mutex<u64>> = OnceLock::new();

fn sigint_dep_contract_hooks() -> &'static Mutex<Vec<(u64, DepContractTeardownHook)>> {
    SIGINT_DEP_CONTRACT_HOOKS.get_or_init(|| Mutex::new(Vec::new()))
}

fn dep_contract_hook_next_id() -> &'static Mutex<u64> {
    DEP_CONTRACT_HOOK_NEXT_ID.get_or_init(|| Mutex::new(0))
}

/// Token returned by `register_dep_contract_sigint_teardown` so the
/// guard can remove its hook on normal Drop (avoiding a double
/// teardown on the happy path).
#[derive(Debug, Clone, Copy)]
pub(crate) struct DepContractTeardownToken(u64);

/// Register a teardown closure that runs from the SIGINT handler before
/// `process::exit`. Returns a token the guard uses to deregister on
/// normal shutdown so the closure does not run twice.
pub(crate) fn register_dep_contract_sigint_teardown<F>(action: F) -> DepContractTeardownToken
where
    F: FnOnce() + Send + 'static,
{
    let mut id_guard = dep_contract_hook_next_id()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let id = *id_guard;
    *id_guard += 1;
    drop(id_guard);

    let mut hooks = sigint_dep_contract_hooks()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    hooks.push((id, Box::new(action)));
    DepContractTeardownToken(id)
}

/// Called from `DependencyContractGuard::Drop` on the happy path so
/// the closure does not run a second time from SIGINT.
pub(crate) fn unregister_dep_contract_sigint_teardown(token: DepContractTeardownToken) {
    let mut hooks = sigint_dep_contract_hooks()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    hooks.retain(|(id, _)| *id != token.0);
}

/// Drain and run every registered teardown closure. Called only from
/// `run_sigint_cleanup`. Safe to call when no hooks are registered.
fn drain_dep_contract_teardown_hooks() {
    let mut hooks = sigint_dep_contract_hooks()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let drained: Vec<(u64, DepContractTeardownHook)> = std::mem::take(&mut *hooks);
    drop(hooks);
    for (_id, hook) in drained {
        hook();
    }
}

pub(crate) struct CleanupScope {
    journal: SharedCleanupJournal,
    handles: Vec<CleanupHandle>,
}

impl CleanupScope {
    fn new(journal: SharedCleanupJournal) -> Self {
        Self {
            journal,
            handles: Vec::new(),
        }
    }

    pub(crate) fn register<F>(&mut self, action: F)
    where
        F: FnOnce() -> CleanupActionRecord + Send + 'static,
    {
        let handle = self.journal.register(action);
        self.handles.push(handle);
    }

    pub(crate) fn register_remove_dir(&mut self, path: impl Into<PathBuf>) {
        let path = path.into();
        self.register(move || remove_dir_action(path));
    }

    pub(crate) fn register_kill_child_process(
        &mut self,
        pid: u32,
        service_name: impl Into<String>,
    ) {
        let service_name = service_name.into();
        self.register(move || kill_child_process_action(pid, service_name));
    }

    pub(crate) fn commit_all(mut self) {
        for handle in self.handles.drain(..) {
            self.journal.commit(handle);
        }
    }
}

fn remove_dir_action(path: PathBuf) -> CleanupActionRecord {
    let detail = path.display().to_string();
    match remove_dir_if_exists(&path) {
        Ok(()) => CleanupActionRecord {
            action: "remove_temp_dir".to_string(),
            status: CleanupActionStatus::Succeeded,
            detail: Some(detail),
        },
        Err(error) => CleanupActionRecord {
            action: "remove_temp_dir".to_string(),
            status: CleanupActionStatus::Failed,
            detail: Some(format!("{}: {}", detail, error)),
        },
    }
}

fn remove_dir_if_exists(path: &Path) -> Result<(), std::io::Error> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn kill_child_process_action(pid: u32, service_name: String) -> CleanupActionRecord {
    let detail = format!("pid={}, service={}", pid, service_name);
    match kill_process_if_exists(pid) {
        Ok(()) => CleanupActionRecord {
            action: "kill_child_process".to_string(),
            status: CleanupActionStatus::Succeeded,
            detail: Some(detail),
        },
        Err(error) => CleanupActionRecord {
            action: "kill_child_process".to_string(),
            status: CleanupActionStatus::Failed,
            detail: Some(format!("{}: {}", detail, error)),
        },
    }
}

#[cfg(unix)]
fn kill_process_if_exists(pid: u32) -> Result<(), std::io::Error> {
    if pid == 0 {
        return Ok(());
    }

    // Signal the pgroup first (no-op if the pid was not spawned with
    // process_group(0); ESRCH there is fine). This is the consumer-
    // orphan reap path for partial-launch failures (#123 cascade): the
    // service spawn site uses cmd.process_group(0) so the spawned pid
    // IS the pgid, and a pgroup-wide SIGKILL terminates uv +
    // python uvicorn + any forked workers in one syscall — preventing
    // the "uv run" wrapper from surviving its own python child's
    // earlier death and leaking as a PID-1 orphan after a consent gate
    // aborts the launch.
    let pgroup_result = unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
    if pgroup_result != 0 {
        let pgroup_err = std::io::Error::last_os_error();
        if !matches!(
            pgroup_err.raw_os_error(),
            Some(libc::ESRCH) | Some(libc::EPERM)
        ) {
            // Anything other than "no such pgroup" / "permission" is a
            // real failure — surface it. ESRCH/EPERM falls through to
            // the pid-only path below as a defensive secondary attempt.
            return Err(pgroup_err);
        }
    }

    let result = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    if result == 0 {
        return Ok(());
    }

    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(windows)]
fn kill_process_if_exists(pid: u32) -> Result<(), std::io::Error> {
    if pid == 0 {
        return Ok(());
    }

    let output = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output()?;
    if output.status.success() || !windows_process_exists(pid)? {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            format!("taskkill exited with {}", output.status),
        ))
    }
}

#[cfg(windows)]
fn windows_process_exists(pid: u32) -> Result<bool, std::io::Error> {
    let output = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid), "/FO", "CSV", "/NH"])
        .output()?;
    if !output.status.success() {
        return Ok(false);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let pid_marker = format!(",\"{}\",", pid);
    Ok(stdout.contains(&pid_marker) || stdout.contains(&format!(",\"{}\"", pid)))
}

#[cfg(not(any(unix, windows)))]
fn kill_process_if_exists(_pid: u32) -> Result<(), std::io::Error> {
    Ok(())
}

#[derive(Default)]
pub(crate) struct PipelineAttemptContext {
    cleanup: SharedCleanupJournal,
    current_phase: Option<HourglassPhase>,
    committed_terminal_state: bool,
}

impl PipelineAttemptContext {
    pub(crate) fn enter_phase(&mut self, phase: HourglassPhase) {
        self.current_phase = Some(phase);
    }

    pub(crate) fn cleanup_scope(&self) -> CleanupScope {
        CleanupScope::new(self.cleanup.clone())
    }

    pub(crate) fn unwind_cleanup(&self) -> CleanupReport {
        self.cleanup.unwind()
    }

    /// Register this attempt's cleanup journal as the SIGINT handler target.
    /// Call once at the start of a pipeline run; call `mark_committed` or let
    /// `unwind_cleanup` run when the pipeline completes normally.
    pub(crate) fn activate_sigint_cleanup(&self) {
        *sigint_journal().lock().unwrap_or_else(|p| p.into_inner()) = Some(self.cleanup.clone());
    }

    pub(crate) fn mark_committed(&mut self) {
        self.committed_terminal_state = true;
        // Dirs are committed — SIGINT should no longer clean them up.
        *sigint_journal().lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

#[derive(Debug)]
pub(crate) struct PipelineAttemptError {
    phase: HourglassPhase,
    source: Error,
    cleanup_report: CleanupReport,
}

impl PipelineAttemptError {
    pub(crate) fn new(phase: HourglassPhase, source: Error, cleanup_report: CleanupReport) -> Self {
        Self {
            phase,
            source,
            cleanup_report,
        }
    }

    pub(crate) fn phase(&self) -> HourglassPhase {
        self.phase
    }

    pub(crate) fn source_error(&self) -> &Error {
        &self.source
    }

    pub(crate) fn cleanup_report(&self) -> &CleanupReport {
        &self.cleanup_report
    }
}

impl std::fmt::Display for PipelineAttemptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "pipeline phase {} failed: {}",
            self.phase.as_str(),
            self.source
        )
    }
}

impl StdError for PipelineAttemptError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.source.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use capsule_core::execution_plan::error::{
        CleanupActionRecord, CleanupActionStatus, CleanupStatus,
    };
    use tempfile::tempdir;

    use super::{run_sigint_cleanup, CleanupJournal, CleanupReport, PipelineAttemptContext};
    use crate::application::pipeline::hourglass::HourglassPhase;

    fn ok_record(action: &str) -> CleanupActionRecord {
        CleanupActionRecord {
            action: action.to_string(),
            status: CleanupActionStatus::Succeeded,
            detail: None,
        }
    }

    /// #24 SIGINT teardown: a registered dep-contract hook MUST run
    /// when `run_sigint_cleanup` fires, and MUST NOT run if the guard
    /// unregistered it on a happy-path Drop.
    ///
    /// `#[serial]` because the registry is a process-global static —
    /// two parallel tests would race each other's hooks.
    #[test]
    #[serial_test::serial(sigint_dep_contract_registry)]
    fn sigint_dep_contract_hook_runs_on_cleanup() {
        let counter = Arc::new(Mutex::new(0u32));
        {
            let counter = Arc::clone(&counter);
            let _token = super::register_dep_contract_sigint_teardown(move || {
                *counter.lock().unwrap() += 1;
            });
        }
        // Token is intentionally dropped without unregister — the hook
        // should still run on SIGINT.
        super::run_sigint_cleanup();
        assert_eq!(*counter.lock().unwrap(), 1);

        // Second SIGINT must be a no-op: drain consumed the hook.
        super::run_sigint_cleanup();
        assert_eq!(*counter.lock().unwrap(), 1);
    }

    #[test]
    #[serial_test::serial(sigint_dep_contract_registry)]
    fn sigint_dep_contract_hook_does_not_run_after_unregister() {
        let counter = Arc::new(Mutex::new(0u32));
        let token = {
            let counter = Arc::clone(&counter);
            super::register_dep_contract_sigint_teardown(move || {
                *counter.lock().unwrap() += 1;
            })
        };
        super::unregister_dep_contract_sigint_teardown(token);
        super::run_sigint_cleanup();
        assert_eq!(
            *counter.lock().unwrap(),
            0,
            "unregistered hook must not run on SIGINT"
        );
    }

    #[test]
    fn cleanup_journal_unwinds_in_reverse_order() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut journal = CleanupJournal::default();

        for action in ["one", "two", "three"] {
            let events = Arc::clone(&events);
            journal.register(move || {
                events.lock().unwrap().push(action.to_string());
                ok_record(action)
            });
        }

        let report = journal.unwind();
        assert_eq!(
            events.lock().unwrap().as_slice(),
            ["three".to_string(), "two".to_string(), "one".to_string()]
        );
        assert_eq!(report.status, CleanupStatus::Complete);
    }

    #[test]
    fn cleanup_scope_can_commit_entries() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut attempt = PipelineAttemptContext::default();
        attempt.enter_phase(HourglassPhase::Prepare);

        {
            let events = Arc::clone(&events);
            let mut scope = attempt.cleanup_scope();
            scope.register(move || {
                events.lock().unwrap().push("committed".to_string());
                ok_record("committed")
            });
            scope.commit_all();
        }

        let CleanupReport { status, actions } = attempt.unwind_cleanup();
        assert!(actions.is_empty());
        assert_eq!(status, CleanupStatus::NotRequired);
        assert!(events.lock().unwrap().is_empty());
    }

    #[test]
    fn cleanup_scope_remove_dir_is_idempotent() {
        let dir = tempdir().expect("tempdir");
        let nested = dir.path().join("nested");
        std::fs::create_dir_all(nested.join("child")).expect("create nested dir");

        let attempt = PipelineAttemptContext::default();
        {
            let mut scope = attempt.cleanup_scope();
            scope.register_remove_dir(nested.clone());
        }

        let report = attempt.unwind_cleanup();
        assert!(!nested.exists());
        assert_eq!(report.status, CleanupStatus::Complete);
        assert_eq!(report.actions.len(), 1);
        assert_eq!(report.actions[0].action, "remove_temp_dir");

        let retry_attempt = PipelineAttemptContext::default();
        {
            let mut scope = retry_attempt.cleanup_scope();
            scope.register_remove_dir(nested);
        }

        let retry_report = retry_attempt.unwind_cleanup();
        assert_eq!(retry_report.status, CleanupStatus::Complete);
        assert_eq!(
            retry_report.actions[0].status,
            CleanupActionStatus::Succeeded
        );
    }

    #[cfg(unix)]
    #[test]
    fn sigint_cleanup_kills_registered_child_process() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();

        let attempt = PipelineAttemptContext::default();
        attempt.activate_sigint_cleanup();
        {
            let mut scope = attempt.cleanup_scope();
            scope.register_kill_child_process(pid, "sleep-fixture");
        }

        run_sigint_cleanup();
        let _ = child.wait().expect("wait after sigint cleanup");
        assert!(child.try_wait().expect("try wait after reap").is_some());
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_scope_kill_child_process_is_idempotent() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();

        let attempt = PipelineAttemptContext::default();
        {
            let mut scope = attempt.cleanup_scope();
            scope.register_kill_child_process(pid, "sleep-fixture");
        }

        let report = attempt.unwind_cleanup();
        let _ = child.wait().expect("wait after kill");
        assert_eq!(report.status, CleanupStatus::Complete);
        assert_eq!(report.actions.len(), 1);
        assert_eq!(report.actions[0].action, "kill_child_process");
        assert_eq!(report.actions[0].status, CleanupActionStatus::Succeeded);

        let retry_attempt = PipelineAttemptContext::default();
        {
            let mut scope = retry_attempt.cleanup_scope();
            scope.register_kill_child_process(pid, "sleep-fixture");
        }

        let retry_report = retry_attempt.unwind_cleanup();
        assert_eq!(retry_report.status, CleanupStatus::Complete);
        assert_eq!(retry_report.actions[0].action, "kill_child_process");
        assert_eq!(
            retry_report.actions[0].status,
            CleanupActionStatus::Succeeded
        );
    }
}
