//! Typed adapter from the bundled Launcher command surface to `runner`.
//!
//! Every command validates its caller in addition to Tauri capability scoping.
//! The shell never accepts a pre-classified privileged intent from JavaScript.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use protocol::desktop_library::{
    DesktopLibrarySnapshot, DesktopOperation, DesktopOperationKind, DesktopOperationStatus,
    InstalledAppSummary, InstalledRemoveResult,
};
use protocol::intent::PrivilegedIntent;
use runner::{
    ChildId, ClientError, HostError, InstallSource, InstalledAppsClient, NativeHost, OutputSink,
    ProcessSupervisor, RetentionTable, RunnerHost, SessionClient, SpawnSpec,
};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use crate::{
    MAIN_WINDOW_LABEL, build_home_window, close_app_window, focus_app_window, open_app_window,
};

struct OperationRecord {
    child_id: ChildId,
    kind: DesktopOperationKind,
    target: Option<String>,
    log_path: PathBuf,
    cancelled: bool,
    exit_code: Option<i32>,
}

pub struct DesktopHost {
    runner_agent: Mutex<ProcessSupervisor<NativeHost>>,
    operation_agent: Mutex<ProcessSupervisor<NativeHost>>,
    operations: Mutex<HashMap<String, OperationRecord>>,
    retained_sessions: Mutex<RetentionTable>,
    launch_guard: Mutex<()>,
    next_operation_id: AtomicU64,
}

impl DesktopHost {
    pub fn new() -> Self {
        Self {
            runner_agent: Mutex::new(ProcessSupervisor::new(native_host())),
            operation_agent: Mutex::new(ProcessSupervisor::new(native_host())),
            operations: Mutex::new(HashMap::new()),
            retained_sessions: Mutex::new(RetentionTable::with_defaults()),
            launch_guard: Mutex::new(()),
            next_operation_id: AtomicU64::new(1),
        }
    }

    fn installed_apps(&self) -> NativeHost {
        native_host()
    }

    pub fn runner_running(&self) -> bool {
        let mut agent = self
            .runner_agent
            .lock()
            .expect("runner agent mutex poisoned");
        agent.reap();
        agent.supervised_count() > 0
    }

    pub fn start_runner(&self) -> Result<(), HostControlError> {
        let mut agent = self
            .runner_agent
            .lock()
            .map_err(|_| HostControlError::Poisoned)?;
        agent.reap();
        if agent.supervised_count() > 0 {
            return Ok(());
        }

        let ato = agent.host().resolve_binary("ato")?;
        let log_path = desktop_log_path("runner-serve.log");
        ensure_parent(&log_path)?;
        agent.spawn(&SpawnSpec {
            program: ato,
            args: vec!["runner".into(), "serve".into()],
            env: Vec::new(),
            output: OutputSink::LogFile(log_path),
        })?;
        Ok(())
    }

    pub fn stop_runner(&self) -> Result<(), HostControlError> {
        self.runner_agent
            .lock()
            .map_err(|_| HostControlError::Poisoned)?
            .shutdown()?;
        Ok(())
    }

    pub fn dispatch_intent_uri(&self, uri: &str) -> Result<(), HostControlError> {
        match protocol::intent::parse_runner_control_intent(uri) {
            Some(PrivilegedIntent::RunnerStart) => self.start_runner(),
            Some(PrivilegedIntent::RunnerStop) => self.stop_runner(),
            Some(PrivilegedIntent::RunnerRegister) => {
                Err(HostControlError::Unsupported("runner/register"))
            }
            Some(PrivilegedIntent::Run { .. }) => Err(HostControlError::Unsupported("run")),
            None => Err(HostControlError::UnrecognizedIntent(uri.to_owned())),
        }
    }

    fn library_list(&self) -> Result<DesktopLibrarySnapshot, HostControlError> {
        self.sweep_retained()?;
        let host = self.installed_apps();
        Ok(InstalledAppsClient::new(&host).list()?)
    }

    fn library_inspect(&self, key: &str) -> Result<InstalledAppSummary, HostControlError> {
        let host = self.installed_apps();
        Ok(InstalledAppsClient::new(&host).inspect(key)?)
    }

    fn library_update(&self, key: &str) -> Result<Value, HostControlError> {
        let host = self.installed_apps();
        Ok(InstalledAppsClient::new(&host).update(key)?)
    }

    fn library_rollback(
        &self,
        key: &str,
        revision: Option<&str>,
    ) -> Result<DesktopOperation, HostControlError> {
        let host = self.installed_apps();
        Ok(InstalledAppsClient::new(&host).rollback(key, revision)?)
    }

    fn library_remove(
        &self,
        key: &str,
        purge_state: bool,
    ) -> Result<InstalledRemoveResult, HostControlError> {
        let host = self.installed_apps();
        Ok(InstalledAppsClient::new(&host).remove_with_state_policy(key, purge_state)?)
    }

    fn library_repair(&self, package_id: &str, action: &str) -> Result<Value, HostControlError> {
        if !matches!(
            action,
            "restart-services" | "rewrite-config" | "switch-model-tier"
        ) {
            return Err(HostControlError::InvalidRepairAction(action.to_owned()));
        }
        let host = self.installed_apps();
        Ok(InstalledAppsClient::new(&host).repair(package_id, action)?)
    }

    fn session_list(&self) -> Result<Value, HostControlError> {
        let host = self.installed_apps();
        Ok(SessionClient::new(&host, |_| Ok(())).list()?)
    }

    fn session_launch(&self, key: &str) -> Result<Value, HostControlError> {
        let host = self.installed_apps();
        Ok(SessionClient::new(&host, |_| Ok(())).launch(key)?)
    }

    fn session_stop(&self, session_id: &str) -> Result<Value, HostControlError> {
        if let Ok(mut retained) = self.retained_sessions.lock() {
            retained.take_by_session_id(session_id);
        }
        let host = self.installed_apps();
        Ok(SessionClient::new(&host, |_| Ok(())).stop(session_id)?)
    }

    pub(crate) fn retain_session(
        &self,
        session_id: &str,
        handle: &str,
    ) -> Result<(), HostControlError> {
        let evicted = self
            .retained_sessions
            .lock()
            .map_err(|_| HostControlError::Poisoned)?
            .retain(session_id.to_owned(), handle.to_owned(), Instant::now());
        for (session, _) in evicted {
            let _ = self.session_stop(&session.session_id);
        }
        Ok(())
    }

    fn activate_session(&self, session_id: &str) -> Result<(), HostControlError> {
        self.retained_sessions
            .lock()
            .map_err(|_| HostControlError::Poisoned)?
            .take_by_session_id(session_id);
        Ok(())
    }

    pub(crate) fn sweep_retained(&self) -> Result<Vec<String>, HostControlError> {
        let expired = self
            .retained_sessions
            .lock()
            .map_err(|_| HostControlError::Poisoned)?
            .evict_expired(Instant::now());
        let mut stopped = Vec::with_capacity(expired.len());
        for (session, _) in expired {
            let session_id = session.session_id;
            self.session_stop(&session_id)?;
            stopped.push(session_id);
        }
        Ok(stopped)
    }

    fn start_install(&self, source: InstallSource) -> Result<DesktopOperation, HostControlError> {
        match &source {
            InstallSource::Local(path) if !path.is_dir() => {
                return Err(HostControlError::InvalidInstallSource(format!(
                    "local folder does not exist: {}",
                    path.display()
                )));
            }
            InstallSource::Capsule(path)
                if !path.is_file()
                    || path.extension().and_then(|value| value.to_str()) != Some("capsule") =>
            {
                return Err(HostControlError::InvalidInstallSource(format!(
                    "local capsule file is missing or has the wrong extension: {}",
                    path.display()
                )));
            }
            _ => {}
        }
        let operation_id = format!(
            "op-{}-{}",
            std::process::id(),
            self.next_operation_id.fetch_add(1, Ordering::Relaxed)
        );
        let log_path = desktop_log_path(&format!("{operation_id}.log"));
        ensure_parent(&log_path)?;

        let mut agent = self
            .operation_agent
            .lock()
            .map_err(|_| HostControlError::Poisoned)?;
        let ato = agent.host().resolve_binary("ato")?;
        let child_id = agent.spawn(&SpawnSpec {
            program: ato,
            args: source.ato_args(),
            env: Vec::new(),
            output: OutputSink::LogFile(log_path.clone()),
        })?;
        drop(agent);

        self.operations
            .lock()
            .map_err(|_| HostControlError::Poisoned)?
            .insert(
                operation_id.clone(),
                OperationRecord {
                    child_id,
                    kind: DesktopOperationKind::Install,
                    target: None,
                    log_path: log_path.clone(),
                    cancelled: false,
                    exit_code: None,
                },
            );
        Ok(DesktopOperation {
            operation_id,
            kind: DesktopOperationKind::Install,
            status: DesktopOperationStatus::Running,
            install_profile_key: None,
            session_id: None,
            message: Some(log_path.display().to_string()),
        })
    }

    fn operation_status(&self, operation_id: &str) -> Result<DesktopOperation, HostControlError> {
        let mut agent = self
            .operation_agent
            .lock()
            .map_err(|_| HostControlError::Poisoned)?;
        let completed = agent.reap_with_status();
        let mut operations = self
            .operations
            .lock()
            .map_err(|_| HostControlError::Poisoned)?;
        for (child_id, exit_code) in completed {
            if let Some(record) = operations
                .values_mut()
                .find(|record| record.child_id == child_id)
            {
                record.exit_code = Some(exit_code.unwrap_or(-1));
            }
        }
        let record = operations
            .get(operation_id)
            .ok_or_else(|| HostControlError::UnknownOperation(operation_id.to_owned()))?;
        let status = if record.cancelled {
            DesktopOperationStatus::Cancelled
        } else if agent.contains(record.child_id) {
            DesktopOperationStatus::Running
        } else if record.exit_code == Some(0) {
            DesktopOperationStatus::Succeeded
        } else {
            DesktopOperationStatus::Failed
        };
        Ok(DesktopOperation {
            operation_id: operation_id.to_owned(),
            kind: record.kind.clone(),
            status,
            install_profile_key: record.target.clone(),
            session_id: None,
            message: Some(record.log_path.display().to_string()),
        })
    }

    fn cancel_operation(&self, operation_id: &str) -> Result<DesktopOperation, HostControlError> {
        let child_id = self
            .operations
            .lock()
            .map_err(|_| HostControlError::Poisoned)?
            .get(operation_id)
            .map(|record| record.child_id)
            .ok_or_else(|| HostControlError::UnknownOperation(operation_id.to_owned()))?;
        self.operation_agent
            .lock()
            .map_err(|_| HostControlError::Poisoned)?
            .terminate(child_id)?;
        let mut operations = self
            .operations
            .lock()
            .map_err(|_| HostControlError::Poisoned)?;
        let record = operations
            .get_mut(operation_id)
            .ok_or_else(|| HostControlError::UnknownOperation(operation_id.to_owned()))?;
        record.cancelled = true;
        Ok(DesktopOperation {
            operation_id: operation_id.to_owned(),
            kind: record.kind.clone(),
            status: DesktopOperationStatus::Cancelled,
            install_profile_key: record.target.clone(),
            session_id: None,
            message: Some(record.log_path.display().to_string()),
        })
    }
}

impl Default for DesktopHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DesktopHost {
    fn drop(&mut self) {
        if let Ok(agent) = self.runner_agent.get_mut() {
            let _ = agent.shutdown();
        }
        if let Ok(agent) = self.operation_agent.get_mut() {
            let _ = agent.shutdown();
        }
        if let Ok(retained) = self.retained_sessions.get_mut() {
            for (session, _) in retained.drain() {
                let host = native_host();
                let _ = SessionClient::new(&host, |_| Ok(())).stop(&session.session_id);
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HostControlError {
    #[error("command is not available to window '{0}'")]
    ForbiddenWindow(String),
    #[error("host state is unavailable")]
    Poisoned,
    #[error(transparent)]
    Host(#[from] HostError),
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error("could not create the Desktop log directory: {0}")]
    CreateLogDirectory(#[source] std::io::Error),
    #[error("invalid install source: {0}")]
    InvalidInstallSource(String),
    #[error("invalid repair action: {0}")]
    InvalidRepairAction(String),
    #[error("native confirmation was declined")]
    ConfirmationDeclined,
    #[error("unknown operation: {0}")]
    UnknownOperation(String),
    #[error("intent verb '{0}' is not yet supported by the Tauri shell")]
    Unsupported(&'static str),
    #[error("not a recognized runner-control intent: {0}")]
    UnrecognizedIntent(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct RunnerStatus {
    pub running: bool,
}

fn require_main_window(window: &WebviewWindow) -> Result<(), String> {
    require_window_label(window.label(), MAIN_WINDOW_LABEL).map_err(|error| error.to_string())
}

fn require_window_label(actual: &str, expected: &str) -> Result<(), HostControlError> {
    if actual == expected {
        Ok(())
    } else {
        Err(HostControlError::ForbiddenWindow(actual.to_owned()))
    }
}

#[tauri::command]
pub fn runner_status(
    window: WebviewWindow,
    host: State<'_, DesktopHost>,
) -> Result<RunnerStatus, String> {
    require_main_window(&window)?;
    Ok(RunnerStatus {
        running: host.runner_running(),
    })
}

#[tauri::command]
pub fn runner_start(window: WebviewWindow, host: State<'_, DesktopHost>) -> Result<(), String> {
    require_main_window(&window)?;
    host.start_runner().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn runner_stop(window: WebviewWindow, host: State<'_, DesktopHost>) -> Result<(), String> {
    require_main_window(&window)?;
    host.stop_runner().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn library_list(
    window: WebviewWindow,
    host: State<'_, DesktopHost>,
) -> Result<DesktopLibrarySnapshot, String> {
    require_main_window(&window)?;
    host.library_list().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn library_inspect(
    window: WebviewWindow,
    host: State<'_, DesktopHost>,
    install_profile_key: String,
) -> Result<InstalledAppSummary, String> {
    require_main_window(&window)?;
    host.library_inspect(&install_profile_key)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn library_install(
    window: WebviewWindow,
    app: AppHandle,
    host: State<'_, DesktopHost>,
    source_kind: String,
    source: String,
) -> Result<DesktopOperation, String> {
    require_main_window(&window)?;
    let source = match source_kind.as_str() {
        "store" if !source.trim().is_empty() => InstallSource::Store(source),
        "github" if !source.trim().is_empty() => InstallSource::GitHub(source),
        "local" if !source.trim().is_empty() => InstallSource::Local(PathBuf::from(source)),
        "capsule" if !source.trim().is_empty() => InstallSource::Capsule(PathBuf::from(source)),
        _ => return Err("invalid install source kind or empty source".to_string()),
    };
    let approved = app
        .dialog()
        .message(format!(
            "Install this capsule on this device?\n\n{}",
            install_source_label(&source)
        ))
        .title("Confirm capsule install")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Review and install".into(),
            "Cancel".into(),
        ))
        .parent(&window)
        .blocking_show();
    if !approved {
        return Err(HostControlError::ConfirmationDeclined.to_string());
    }
    let operation = host
        .start_install(source)
        .map_err(|error| error.to_string())?;
    emit(&app, runner::events::OPERATION_PROGRESS, &operation)?;
    Ok(operation)
}

#[tauri::command]
pub fn operation_status(
    window: WebviewWindow,
    app: AppHandle,
    host: State<'_, DesktopHost>,
    operation_id: String,
) -> Result<DesktopOperation, String> {
    require_main_window(&window)?;
    let operation = host
        .operation_status(&operation_id)
        .map_err(|error| error.to_string())?;
    emit(&app, runner::events::OPERATION_PROGRESS, &operation)?;
    if operation.status == DesktopOperationStatus::Succeeded {
        emit(&app, runner::events::LIBRARY_CHANGED, &operation)?;
    } else if operation.status == DesktopOperationStatus::Failed {
        emit(&app, runner::events::OPERATION_FAILED, &operation)?;
    }
    Ok(operation)
}

#[tauri::command]
pub fn operation_cancel(
    window: WebviewWindow,
    app: AppHandle,
    host: State<'_, DesktopHost>,
    operation_id: String,
) -> Result<DesktopOperation, String> {
    require_main_window(&window)?;
    let operation = host
        .cancel_operation(&operation_id)
        .map_err(|error| error.to_string())?;
    emit(&app, runner::events::OPERATION_PROGRESS, &operation)?;
    Ok(operation)
}

#[tauri::command]
pub fn library_update(
    window: WebviewWindow,
    app: AppHandle,
    host: State<'_, DesktopHost>,
    install_profile_key: String,
) -> Result<Value, String> {
    require_main_window(&window)?;
    let value = host
        .library_update(&install_profile_key)
        .map_err(|error| error.to_string())?;
    emit(&app, runner::events::LIBRARY_CHANGED, &value)?;
    Ok(value)
}

#[tauri::command]
pub fn library_rollback(
    window: WebviewWindow,
    app: AppHandle,
    host: State<'_, DesktopHost>,
    install_profile_key: String,
    revision: Option<String>,
) -> Result<DesktopOperation, String> {
    require_main_window(&window)?;
    let value = host
        .library_rollback(&install_profile_key, revision.as_deref())
        .map_err(|error| error.to_string())?;
    emit(&app, runner::events::LIBRARY_CHANGED, &value)?;
    Ok(value)
}

#[tauri::command]
pub fn library_remove(
    window: WebviewWindow,
    app: AppHandle,
    host: State<'_, DesktopHost>,
    install_profile_key: String,
    purge_state: bool,
) -> Result<InstalledRemoveResult, String> {
    require_main_window(&window)?;
    let action = if purge_state {
        "Remove the app and permanently delete its persistent data?"
    } else {
        "Remove the app from this device? Persistent data will be preserved."
    };
    let approved = app
        .dialog()
        .message(action)
        .title("Confirm app removal")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            if purge_state {
                "Remove and delete data".into()
            } else {
                "Remove".into()
            },
            "Cancel".into(),
        ))
        .parent(&window)
        .blocking_show();
    if !approved {
        return Err(HostControlError::ConfirmationDeclined.to_string());
    }
    let value = host
        .library_remove(&install_profile_key, purge_state)
        .map_err(|error| error.to_string())?;
    emit(&app, runner::events::LIBRARY_CHANGED, &value)?;
    Ok(value)
}

#[tauri::command]
pub fn library_repair(
    window: WebviewWindow,
    app: AppHandle,
    host: State<'_, DesktopHost>,
    package_id: String,
    action: String,
) -> Result<Value, String> {
    require_main_window(&window)?;
    let value = host
        .library_repair(&package_id, &action)
        .map_err(|error| error.to_string())?;
    emit(&app, runner::events::LIBRARY_CHANGED, &value)?;
    Ok(value)
}

#[tauri::command]
pub fn session_list(window: WebviewWindow, host: State<'_, DesktopHost>) -> Result<Value, String> {
    require_main_window(&window)?;
    host.session_list().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn session_launch(
    window: WebviewWindow,
    app: AppHandle,
    host: State<'_, DesktopHost>,
    install_profile_key: String,
) -> Result<Value, String> {
    require_main_window(&window)?;
    // Serialize the inspect-and-launch critical section. The CLI currently
    // permits concurrent installed launches by remapping ports, while the
    // Desktop contract is single-instance: a second Launch means Focus.
    let _launch_guard = host
        .launch_guard
        .lock()
        .map_err(|_| HostControlError::Poisoned.to_string())?;
    let installed = host
        .library_inspect(&install_profile_key)
        .map_err(|error| error.to_string())?;
    if let Some(session) = installed.running_sessions.iter().find(|session| {
        session.install_profile_key.as_deref() == Some(install_profile_key.as_str())
    }) {
        focus_app_window(&app, &session.session_id).map_err(|error| {
            format!(
                "session {} is already running but its window could not be focused: {error}",
                session.session_id
            )
        })?;
        host.activate_session(&session.session_id)
            .map_err(|error| error.to_string())?;
        let value = serde_json::json!({
            "schema_version": "ccp/v1",
            "package_id": "ato/ato-desktop",
            "action": "session_focus",
            "reused": true,
            "session": {
                "session_id": session.session_id,
                "install_profile_key": install_profile_key,
                "status": session.status,
            }
        });
        emit(&app, runner::events::SESSION_CHANGED, &value)?;
        return Ok(value);
    }
    let value = host
        .session_launch(&install_profile_key)
        .map_err(|error| error.to_string())?;
    if let Err(presentation_error) = open_app_window(&app, &value) {
        let cleanup = value
            .pointer("/session/session_id")
            .and_then(Value::as_str)
            .map(|session_id| host.session_stop(session_id));
        return match cleanup {
            Some(Ok(_)) => Err(format!(
                "{presentation_error}; the launched session was stopped"
            )),
            Some(Err(cleanup_error)) => Err(format!(
                "{presentation_error}; failed to stop the launched session: {cleanup_error}"
            )),
            None => Err(format!(
                "{presentation_error}; launch response did not identify a session to stop"
            )),
        };
    }
    emit(&app, runner::events::SESSION_CHANGED, &value)?;
    Ok(value)
}

#[tauri::command]
pub fn session_focus(
    window: WebviewWindow,
    app: AppHandle,
    host: State<'_, DesktopHost>,
    session_id: String,
) -> Result<(), String> {
    require_main_window(&window)?;
    focus_app_window(&app, &session_id)?;
    host.activate_session(&session_id)
        .map_err(|error| error.to_string())?;
    emit(&app, runner::events::SESSION_CHANGED, &session_id)
}

#[tauri::command]
pub fn session_close(
    window: WebviewWindow,
    app: AppHandle,
    host: State<'_, DesktopHost>,
    session_id: String,
    handle: String,
) -> Result<(), String> {
    require_main_window(&window)?;
    close_app_window(&app, &session_id)?;
    host.retain_session(&session_id, &handle)
        .map_err(|error| error.to_string())?;
    emit(&app, runner::events::SESSION_CHANGED, &session_id)
}

#[tauri::command]
pub fn session_stop(
    window: WebviewWindow,
    app: AppHandle,
    host: State<'_, DesktopHost>,
    session_id: String,
) -> Result<Value, String> {
    require_main_window(&window)?;
    let value = host
        .session_stop(&session_id)
        .map_err(|error| error.to_string())?;
    let _ = close_app_window(&app, &session_id);
    emit(&app, runner::events::SESSION_CHANGED, &value)?;
    Ok(value)
}

#[tauri::command]
pub fn open_home(window: WebviewWindow, app: AppHandle) -> Result<(), String> {
    require_main_window(&window)?;
    if let Some(home) = app.get_webview_window(crate::HOME_WINDOW_LABEL) {
        home.show().map_err(|error| error.to_string())?;
        return home.set_focus().map_err(|error| error.to_string());
    }
    build_home_window(&app).map_err(|error| error.to_string())
}

fn emit<T: Serialize + Clone>(app: &AppHandle, event: &str, payload: &T) -> Result<(), String> {
    app.emit(event, payload).map_err(|error| error.to_string())
}

fn install_source_label(source: &InstallSource) -> String {
    match source {
        InstallSource::Store(value) => format!("Store: {value}"),
        InstallSource::GitHub(value) => format!("GitHub: {value}"),
        InstallSource::Local(path) => format!("Local folder: {}", path.display()),
        InstallSource::Capsule(path) => format!("Capsule file: {}", path.display()),
    }
}

fn native_host() -> NativeHost {
    NativeHost::new(resolve_ato_binary)
}

fn resolve_ato_binary(name: &str) -> Result<PathBuf, HostError> {
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        #[cfg(windows)]
        let bundled_name = if Path::new(name).extension().is_none() {
            format!("{name}.exe")
        } else {
            name.to_owned()
        };
        #[cfg(not(windows))]
        let bundled_name = name.to_owned();
        let candidate = directory.join(bundled_name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    runner::resolve_on_path(name)
}

fn ato_home() -> PathBuf {
    std::env::var_os("ATO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".ato")))
        .or_else(|| std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".ato")))
        .unwrap_or_else(|| PathBuf::from(".ato"))
}

fn desktop_log_path(name: &str) -> PathBuf {
    ato_home().join("logs/desktop-tauri").join(name)
}

fn ensure_parent(path: &Path) -> Result<(), HostControlError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(HostControlError::CreateLogDirectory)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_guard_only_accepts_main_window() {
        assert!(require_window_label("main", "main").is_ok());
        assert!(matches!(
            require_window_label("home", "main"),
            Err(HostControlError::ForbiddenWindow(label)) if label == "home"
        ));
        assert!(matches!(
            require_window_label("app-session", "main"),
            Err(HostControlError::ForbiddenWindow(label)) if label == "app-session"
        ));
    }

    #[test]
    fn stop_is_idempotent_when_nothing_is_running() {
        let host = DesktopHost::new();
        host.stop_runner().expect("stop on empty is a no-op");
        assert!(!host.runner_running());
    }

    #[test]
    fn unrecognized_and_unwired_intents_fail_closed() {
        let host = DesktopHost::new();
        assert!(matches!(
            host.dispatch_intent_uri("ato://runner/register"),
            Err(HostControlError::Unsupported("runner/register"))
        ));
        assert!(matches!(
            host.dispatch_intent_uri("https://evil.example"),
            Err(HostControlError::UnrecognizedIntent(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn install_cancel_terminates_the_owned_process_group_without_an_orphan() {
        let host = DesktopHost::new();
        let sleep = runner::resolve_on_path("sleep").expect("sleep on PATH");
        let child_id = host
            .operation_agent
            .lock()
            .expect("operation supervisor")
            .spawn(&SpawnSpec {
                program: sleep,
                args: vec!["30".into()],
                env: Vec::new(),
                output: OutputSink::Null,
            })
            .expect("spawn cancellable operation");
        host.operations.lock().expect("operation records").insert(
            "op-cancel-test".into(),
            OperationRecord {
                child_id,
                kind: DesktopOperationKind::Install,
                target: None,
                log_path: PathBuf::from(".tmp/op-cancel-test.log"),
                cancelled: false,
                exit_code: None,
            },
        );

        let cancelled = host
            .cancel_operation("op-cancel-test")
            .expect("cancel operation");
        assert_eq!(cancelled.status, DesktopOperationStatus::Cancelled);
        assert_eq!(
            host.operation_status("op-cancel-test")
                .expect("read cancelled status")
                .status,
            DesktopOperationStatus::Cancelled
        );

        // terminate_group waits for the direct child after signalling the
        // whole process group, so ESRCH is observable synchronously here.
        let rc = unsafe { libc::kill(child_id.0 as libc::pid_t, 0) };
        assert_eq!(rc, -1, "cancelled install process must be gone");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}
