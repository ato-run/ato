//! The desktop host: typed commands that delegate Capsule operations to the
//! `ato` CLI and inspect the active Run.
//!
//! Every command verifies its caller label before acting; the capability file
//! is defense in depth, not the only check. Computation is never advanced by
//! this shell — the CLI is the sole owner. Long-lived CLI operations (the
//! portable `run`, which blocks in realization) are spawned through
//! [`DesktopHost`]'s [`ProcessSupervisor`], so app exit or an explicit cancel
//! tears down the whole CLI process tree.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use ato_host_control::{
    ChildId, CommandSpec, CompletedCommand, HostError, NativeHost, OutputSink, ProcessSupervisor,
    RunnerHost, SpawnSpec,
};
use ato_ipc::computation::{ComputationCommand, ComputationCommandResult};
use ato_ipc::desktop_control::{DesktopRunStatus, DesktopRunView};
use ato_ipc::session_surface::WEB_SURFACE_PROFILE;
use serde::Serialize;
use tauri::{Manager, Url};

use crate::{binary, navigation, windows};

/// Static shell metadata surfaced to the launcher UI.
#[derive(Serialize)]
pub struct DesktopInfo {
    pub version: String,
    pub platform: String,
}

/// Failure to claim the single active portable-run slot.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RunError {
    #[error("a portable run is already active")]
    AlreadyActive,
    #[error("the desktop host is shut down")]
    Closed,
    #[error("{0}")]
    Supervisor(String),
}

/// The single portable-run slot. `Closed` is terminal: once the host is shut
/// down it can never accept a new run, so a spawn that raced app exit can
/// never leave a child that outlives the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunSlot {
    Idle,
    Active(ChildId),
    Closed,
}

/// Monotonic suffix for per-run log files, so one run's output can never bleed
/// into the next.
static RUN_LOG_SEQ: AtomicU64 = AtomicU64::new(0);

/// Shell-owned process supervision state. Short-lived CLI commands run to
/// completion; long-lived commands (portable `run`) are spawned and tracked
/// here so that app exit or cancellation can terminate their whole process
/// group. At most one portable run is active at a time; the claim is atomic so
/// two concurrent runs can never both own the supervisor slot.
pub struct DesktopHost {
    supervisor: Mutex<ProcessSupervisor<NativeHost>>,
    run_child: Mutex<RunSlot>,
}

impl DesktopHost {
    /// A host whose supervisor resolves the `ato` binary with the shell's
    /// explicit resolution policy.
    pub fn new() -> Self {
        let supervisor = ProcessSupervisor::new(NativeHost::new(move |_name| {
            binary::resolve_ato_binary()
                .map_err(|error| HostError::BinaryNotFound(error.to_string()))
        }));
        Self {
            supervisor: Mutex::new(supervisor),
            run_child: Mutex::new(RunSlot::Idle),
        }
    }

    fn supervisor(&self) -> Result<MutexGuard<'_, ProcessSupervisor<NativeHost>>, String> {
        self.supervisor
            .lock()
            .map_err(|_| "supervisor lock poisoned".to_owned())
    }

    /// Atomically claim the single portable-run slot and spawn the child under
    /// supervision. Returns [`RunError::AlreadyActive`] when a run is already
    /// in flight and [`RunError::Closed`] after the host is shut down; check,
    /// spawn, and slot registration happen under one lock so two callers can
    /// never both win and a spawn that races app exit is rejected.
    pub fn spawn_run(&self, spec: &SpawnSpec) -> Result<ChildId, RunError> {
        let mut slot = self
            .run_child
            .lock()
            .map_err(|_| RunError::Supervisor("run lock poisoned".to_owned()))?;
        match *slot {
            RunSlot::Closed => return Err(RunError::Closed),
            RunSlot::Active(_) => return Err(RunError::AlreadyActive),
            RunSlot::Idle => {}
        }
        let id = self
            .supervisor()
            .map_err(RunError::Supervisor)?
            .spawn(spec)
            .map_err(|error| RunError::Supervisor(error.to_string()))?;
        *slot = RunSlot::Active(id);
        Ok(id)
    }

    /// Whether `id` is still the owned active run.
    pub fn is_run_owned(&self, id: ChildId) -> bool {
        self.run_child
            .lock()
            .map(|slot| matches!(*slot, RunSlot::Active(owned) if owned == id))
            .unwrap_or(false)
    }

    /// Clear the run slot only if it is still owned by `id`. A caller that has
    /// already been superseded or cancelled must not clear a newer run's slot.
    pub fn finish_run(&self, id: ChildId) {
        if let Ok(mut slot) = self.run_child.lock()
            && matches!(*slot, RunSlot::Active(owned) if owned == id)
        {
            *slot = RunSlot::Idle;
        }
    }

    /// Terminate the tracked portable-run child, if one is running.
    pub fn cancel_run(&self) -> Result<(), String> {
        let mut slot = self
            .run_child
            .lock()
            .map_err(|_| "run lock poisoned".to_owned())?;
        let RunSlot::Active(id) = *slot else {
            return Ok(());
        };
        *slot = RunSlot::Idle;
        self.supervisor()?
            .terminate(id)
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Terminate every supervised process group and close the run slot for
    /// good. Idempotent. Holds the run slot across supervisor shutdown so a
    /// spawn racing app exit is serialized behind it and rejected once the
    /// host is closed.
    pub fn shutdown(&self) -> Result<(), String> {
        let mut slot = self
            .run_child
            .lock()
            .map_err(|_| "run lock poisoned".to_owned())?;
        *slot = RunSlot::Closed;
        self.supervisor()?
            .shutdown()
            .map_err(|error| error.to_string())
    }
}

impl Default for DesktopHost {
    fn default() -> Self {
        Self::new()
    }
}

/// Map a typed [`ComputationCommand`] to the exact `ato` argv. This is the
/// only boundary where a command becomes a process — arbitrary shell strings
/// are never accepted.
pub fn argv_for(command: &ComputationCommand) -> Vec<String> {
    match command {
        ComputationCommand::Init {
            capsule,
            initial_only,
        } => {
            let mut args = vec!["init".to_owned(), capsule.clone()];
            if *initial_only {
                args.push("--initial-only".to_owned());
            }
            args
        }
        ComputationCommand::Resume { selector, branch } => {
            let mut args = vec!["resume".to_owned(), selector.clone()];
            if let Some(branch) = branch {
                args.extend(["--branch".to_owned(), branch.clone()]);
            }
            args
        }
        ComputationCommand::Stop { capsule } => vec!["stop".to_owned(), capsule.clone()],
        ComputationCommand::Encap { selector, output } => vec![
            "encap".to_owned(),
            selector.clone(),
            "-o".to_owned(),
            output.clone(),
        ],
        ComputationCommand::RunPortable { capsule_file } => {
            vec!["run".to_owned(), capsule_file.clone()]
        }
    }
}

fn combined_output(completed: &CompletedCommand) -> String {
    let mut text = String::from_utf8_lossy(&completed.stdout).into_owned();
    if !completed.stderr.is_empty() {
        text.push_str(&String::from_utf8_lossy(&completed.stderr));
    }
    text
}

fn run_cli(args: Vec<String>) -> Result<CompletedCommand, String> {
    let program = binary::resolve_ato_binary().map_err(|error| error.to_string())?;
    let host = NativeHost::new(move |_name| Ok(program.clone()));
    let program = host
        .resolve_binary(binary::ato_binary_name())
        .map_err(|error| error.to_string())?;
    host.run_to_completion(&CommandSpec {
        program,
        args,
        env: vec![],
    })
    .map_err(|error| error.to_string())
}

/// A unique log path for one portable run, so consecutive runs never share (or
/// append onto) each other's output.
fn unique_run_log_path() -> std::path::PathBuf {
    let seq = RUN_LOG_SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join("ato-desktop")
        .join(format!("run-{}-{seq}.log", std::process::id()))
}

/// Spawn a long-lived CLI command through the shell's supervisor and wait for
/// it to exit, reading its combined output from a per-run log file. The child
/// remains supervisor-owned for the whole wait, so app exit or cancellation can
/// terminate its process group mid-run. Only one portable run is admitted at a
/// time — a second call fails with "already active" instead of overwriting the
/// ownership slot.
fn run_supervised(
    host: &DesktopHost,
    args: Vec<String>,
) -> Result<ComputationCommandResult, String> {
    let program = binary::resolve_ato_binary().map_err(|error| error.to_string())?;
    let log_path = unique_run_log_path();
    let log_dir = log_path
        .parent()
        .ok_or_else(|| "run log path has no parent".to_owned())?;
    std::fs::create_dir_all(log_dir).map_err(|error| error.to_string())?;
    let id = host
        .spawn_run(&SpawnSpec {
            program,
            args,
            env: vec![],
            output: OutputSink::LogFile(log_path.clone()),
        })
        .map_err(|error| error.to_string())?;

    let exit_code = loop {
        let reaped = host.supervisor()?.reap_with_status();
        if let Some((_, code)) = reaped.iter().find(|(child, _)| *child == id) {
            break *code;
        }
        std::thread::sleep(Duration::from_millis(50));
        if !host.is_run_owned(id) {
            // Cancelled: terminate() already removed ownership.
            break None;
        }
    };
    host.finish_run(id);

    let output = std::fs::read(&log_path).unwrap_or_default();
    let _ = std::fs::remove_file(&log_path);
    Ok(ComputationCommandResult {
        success: exit_code == Some(0),
        output: String::from_utf8_lossy(&output).into_owned(),
    })
}

/// Inspect the active Run by asking the CLI, and decode its JSON response.
fn inspect(project: &str) -> Result<DesktopRunView, String> {
    let completed = run_cli(vec![
        "__desktop".to_owned(),
        "inspect".to_owned(),
        project.to_owned(),
    ])?;
    if !completed.success() {
        let stderr = String::from_utf8_lossy(&completed.stderr).into_owned();
        return Err(if stderr.is_empty() {
            combined_output(&completed)
        } else {
            stderr
        });
    }
    serde_json::from_slice(&completed.stdout).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn desktop_info<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
) -> Result<DesktopInfo, String> {
    windows::verify_main_caller(window.label())?;
    Ok(DesktopInfo {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        platform: std::env::consts::OS.to_owned(),
    })
}

#[tauri::command]
pub async fn computation_execute<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    app: tauri::AppHandle<R>,
    command: ComputationCommand,
) -> Result<ComputationCommandResult, String> {
    windows::verify_main_caller(window.label())?;
    let args = argv_for(&command);
    match command {
        // The portable run blocks in realization and can outlive the shell;
        // it is supervisor-owned so app exit or cancel reaps its process tree.
        ComputationCommand::RunPortable { .. } => tauri::async_runtime::spawn_blocking(move || {
            let host = app.state::<DesktopHost>();
            run_supervised(&host, args)
        })
        .await
        .map_err(|error| error.to_string())?,
        _ => tauri::async_runtime::spawn_blocking(move || run_cli(args))
            .await
            .map_err(|error| error.to_string())?
            .map(|completed| ComputationCommandResult {
                success: completed.success(),
                output: combined_output(&completed),
            }),
    }
}

#[tauri::command]
pub fn run_cancel<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    app: tauri::AppHandle<R>,
) -> Result<(), String> {
    windows::verify_main_caller(window.label())?;
    app.state::<DesktopHost>().cancel_run()
}

#[tauri::command]
pub async fn run_inspect<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    project: String,
) -> Result<DesktopRunView, String> {
    windows::verify_main_caller(window.label())?;
    tauri::async_runtime::spawn_blocking(move || inspect(&project))
        .await
        .map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn pick_project<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    app: tauri::AppHandle<R>,
) -> Result<Option<String>, String> {
    windows::verify_main_caller(window.label())?;
    use tauri_plugin_dialog::DialogExt;
    let (sender, receiver) = std::sync::mpsc::channel();
    app.dialog().file().pick_folder(move |path| {
        let _ = sender.send(path);
    });
    let path = receiver
        .recv()
        .map_err(|_| "folder picker was closed".to_owned())?;
    Ok(path
        .and_then(|path| path.into_path().ok())
        .map(|path| path.to_string_lossy().into_owned()))
}

#[tauri::command]
pub fn open_home<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    app: tauri::AppHandle<R>,
) -> Result<(), String> {
    windows::verify_main_caller(window.label())?;
    crate::build_home_window(&app).map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn open_web_surface<R: tauri::Runtime>(
    window: tauri::WebviewWindow<R>,
    app: tauri::AppHandle<R>,
    project: String,
) -> Result<(), String> {
    windows::verify_main_caller(window.label())?;
    let view = tauri::async_runtime::spawn_blocking(move || inspect(&project))
        .await
        .map_err(|error| error.to_string())??;

    if view.status != DesktopRunStatus::Active {
        return Err(format!("Run is not active (status {:?})", view.status));
    }
    let [surface] = view.surfaces.as_slice() else {
        return Err(format!(
            "expected exactly one Web surface, found {}",
            view.surfaces.len()
        ));
    };
    let url_string = match surface {
        ato_ipc::desktop_control::DesktopSurfaceView::Web { url, profile } => {
            if profile != WEB_SURFACE_PROFILE {
                return Err(format!("unsupported surface profile {profile}"));
            }
            url.clone()
        }
        ato_ipc::desktop_control::DesktopSurfaceView::Terminal { .. } => {
            return Err("Terminal surfaces are not openable yet".to_owned());
        }
    };
    let url: Url = url_string
        .parse()
        .map_err(|error| format!("invalid surface URL: {error}"))?;
    if !navigation::is_loopback_surface_url(&url) {
        return Err("surface URL is not a loopback HTTP(S) origin".to_owned());
    }
    let origin = navigation::url_origin(&url);
    let label = windows::app_window_label(&view.project, &url_string);
    crate::open_app_window(&app, &label, url, &origin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_maps_to_exact_argv() {
        assert_eq!(
            argv_for(&ComputationCommand::Init {
                capsule: "demo".to_owned(),
                initial_only: false,
            }),
            vec!["init", "demo"]
        );
        assert_eq!(
            argv_for(&ComputationCommand::Init {
                capsule: "demo".to_owned(),
                initial_only: true,
            }),
            vec!["init", "demo", "--initial-only"]
        );
    }

    #[test]
    fn resume_maps_to_exact_argv_with_optional_branch() {
        assert_eq!(
            argv_for(&ComputationCommand::Resume {
                selector: "demo@main".to_owned(),
                branch: None,
            }),
            vec!["resume", "demo@main"]
        );
        assert_eq!(
            argv_for(&ComputationCommand::Resume {
                selector: "demo@main#1".to_owned(),
                branch: Some("experiment".to_owned()),
            }),
            vec!["resume", "demo@main#1", "--branch", "experiment"]
        );
    }

    #[test]
    fn stop_encap_and_run_map_to_exact_argv() {
        assert_eq!(
            argv_for(&ComputationCommand::Stop {
                capsule: "demo".to_owned()
            }),
            vec!["stop", "demo"]
        );
        assert_eq!(
            argv_for(&ComputationCommand::Encap {
                selector: "demo@main".to_owned(),
                output: "out.capsule".to_owned(),
            }),
            vec!["encap", "demo@main", "-o", "out.capsule"]
        );
        assert_eq!(
            argv_for(&ComputationCommand::RunPortable {
                capsule_file: "x.capsule".to_owned(),
            }),
            vec!["run", "x.capsule"]
        );
    }

    fn sleep_spec(seconds: &str) -> SpawnSpec {
        SpawnSpec {
            program: ato_host_control::resolve_on_path("sleep")
                .expect("sleep on PATH for the test"),
            args: vec![seconds.to_owned()],
            env: vec![],
            output: OutputSink::Null,
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_second_active_run_is_rejected() {
        let host = DesktopHost::new();
        let _first = host.spawn_run(&sleep_spec("30")).unwrap();
        assert_eq!(
            host.spawn_run(&sleep_spec("30")).unwrap_err(),
            RunError::AlreadyActive
        );
        host.shutdown().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cancel_releases_the_run_slot_for_the_next_run() {
        let host = DesktopHost::new();
        let first = host.spawn_run(&sleep_spec("30")).unwrap();
        assert!(host.is_run_owned(first));
        host.cancel_run().unwrap();
        assert!(!host.is_run_owned(first));
        // The next run can claim the now-free slot.
        let second = host.spawn_run(&sleep_spec("0")).unwrap();
        assert!(host.is_run_owned(second));
        host.shutdown().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn spawn_after_shutdown_is_rejected_and_leaves_no_children() {
        let host = DesktopHost::new();
        host.shutdown().unwrap();
        // A run that raced app exit can no longer be admitted, and the
        // supervisor is empty — the "no child outlives the shell" invariant.
        assert_eq!(
            host.spawn_run(&sleep_spec("30")).unwrap_err(),
            RunError::Closed
        );
        assert_eq!(host.supervisor().unwrap().supervised_count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn spawn_racing_shutdown_never_leaves_a_child_after_shutdown() {
        let host = std::sync::Arc::new(DesktopHost::new());
        let racer = std::sync::Arc::clone(&host);
        let handle = std::thread::spawn(move || {
            for _ in 0..200 {
                let _ = racer.spawn_run(&sleep_spec("30"));
            }
        });
        host.shutdown().unwrap();
        handle.join().unwrap();
        // Whatever the interleaving, once shutdown returns the supervisor is
        // empty and the slot is closed for good.
        assert_eq!(host.supervisor().unwrap().supervised_count(), 0);
        assert_eq!(
            host.spawn_run(&sleep_spec("30")).unwrap_err(),
            RunError::Closed
        );
    }

    #[test]
    fn run_log_paths_are_unique_per_run() {
        assert_ne!(unique_run_log_path(), unique_run_log_path());
    }
}
