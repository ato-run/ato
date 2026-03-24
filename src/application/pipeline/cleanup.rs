#![allow(dead_code)]

use std::error::Error as StdError;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Error;
use capsule_core::execution_plan::error::{
    CleanupActionRecord, CleanupActionStatus, CleanupStatus,
};

use crate::application::pipeline::hourglass::HourglassPhase;

type CleanupHandle = usize;

trait CleanupAction {
    fn run(self: Box<Self>) -> CleanupActionRecord;
}

impl<F> CleanupAction for F
where
    F: FnOnce() -> CleanupActionRecord + 'static,
{
    fn run(self: Box<Self>) -> CleanupActionRecord {
        (*self)()
    }
}

struct CleanupEntry {
    id: CleanupHandle,
    action: Option<Box<dyn CleanupAction>>,
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
        F: FnOnce() -> CleanupActionRecord + 'static,
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

pub(crate) struct CleanupScope<'a> {
    journal: &'a mut CleanupJournal,
    handles: Vec<CleanupHandle>,
}

impl<'a> CleanupScope<'a> {
    pub(crate) fn new(journal: &'a mut CleanupJournal) -> Self {
        Self {
            journal,
            handles: Vec::new(),
        }
    }

    pub(crate) fn register<F>(&mut self, action: F)
    where
        F: FnOnce() -> CleanupActionRecord + 'static,
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
    let pid_marker = format!(","{}",", pid);
    Ok(stdout.contains(&pid_marker) || stdout.contains(&format!(","{}"", pid)))
}

#[cfg(not(any(unix, windows)))]
fn kill_process_if_exists(_pid: u32) -> Result<(), std::io::Error> {
    Ok(())
}

#[derive(Default)]
pub(crate) struct PipelineAttemptContext {
    cleanup: CleanupJournal,
    current_phase: Option<HourglassPhase>,
    committed_terminal_state: bool,
}

impl PipelineAttemptContext {
    pub(crate) fn enter_phase(&mut self, phase: HourglassPhase) {
        self.current_phase = Some(phase);
    }

    pub(crate) fn cleanup_scope(&mut self) -> CleanupScope<'_> {
        CleanupScope::new(&mut self.cleanup)
    }

    pub(crate) fn unwind_cleanup(&mut self) -> CleanupReport {
        self.cleanup.unwind()
    }

    pub(crate) fn mark_committed(&mut self) {
        self.committed_terminal_state = true;
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
    use std::cell::RefCell;
    use std::rc::Rc;

    use capsule_core::execution_plan::error::{
        CleanupActionRecord, CleanupActionStatus, CleanupStatus,
    };
    use tempfile::tempdir;

    use super::{CleanupJournal, CleanupReport, PipelineAttemptContext};
    use crate::application::pipeline::hourglass::HourglassPhase;

    fn ok_record(action: &str) -> CleanupActionRecord {
        CleanupActionRecord {
            action: action.to_string(),
            status: CleanupActionStatus::Succeeded,
            detail: None,
        }
    }

    #[test]
    fn cleanup_journal_unwinds_in_reverse_order() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut journal = CleanupJournal::default();

        for action in ["one", "two", "three"] {
            let events = Rc::clone(&events);
            journal.register(move || {
                events.borrow_mut().push(action.to_string());
                ok_record(action)
            });
        }

        let report = journal.unwind();
        assert_eq!(
            events.borrow().as_slice(),
            ["three".to_string(), "two".to_string(), "one".to_string()]
        );
        assert_eq!(report.status, CleanupStatus::Complete);
    }

    #[test]
    fn cleanup_scope_can_commit_entries() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut attempt = PipelineAttemptContext::default();
        attempt.enter_phase(HourglassPhase::Prepare);

        {
            let events = Rc::clone(&events);
            let mut scope = attempt.cleanup_scope();
            scope.register(move || {
                events.borrow_mut().push("committed".to_string());
                ok_record("committed")
            });
            scope.commit_all();
        }

        let CleanupReport { status, actions } = attempt.unwind_cleanup();
        assert!(actions.is_empty());
        assert_eq!(status, CleanupStatus::NotRequired);
        assert!(events.borrow().is_empty());
    }

    #[test]
    fn cleanup_scope_remove_dir_is_idempotent() {
        let dir = tempdir().expect("tempdir");
        let nested = dir.path().join("nested");
        std::fs::create_dir_all(nested.join("child")).expect("create nested dir");

        let mut attempt = PipelineAttemptContext::default();
        {
            let mut scope = attempt.cleanup_scope();
            scope.register_remove_dir(nested.clone());
        }

        let report = attempt.unwind_cleanup();
        assert!(!nested.exists());
        assert_eq!(report.status, CleanupStatus::Complete);
        assert_eq!(report.actions.len(), 1);
        assert_eq!(report.actions[0].action, "remove_temp_dir");

        let mut retry_attempt = PipelineAttemptContext::default();
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
    fn cleanup_scope_kill_child_process_is_idempotent() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();

        let mut attempt = PipelineAttemptContext::default();
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

        let mut retry_attempt = PipelineAttemptContext::default();
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
