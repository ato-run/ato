//! Typed adapter from the local Launcher command surface to `runner`.
//!
//! Every command validates its caller in addition to Tauri capability scoping.
//! The shell never accepts a pre-classified privileged intent from JavaScript.

use std::path::PathBuf;
use std::sync::Mutex;

use protocol::intent::PrivilegedIntent;
use runner::{HostError, NativeHost, OutputSink, ProcessSupervisor, RunnerHost, SpawnSpec};
use serde::Serialize;
use tauri::{Manager, State, WebviewWindow};

use crate::{MAIN_WINDOW_LABEL, build_home_window};

pub struct DesktopHost {
    runner_agent: Mutex<ProcessSupervisor<NativeHost>>,
}

impl DesktopHost {
    pub fn new() -> Self {
        Self {
            runner_agent: Mutex::new(ProcessSupervisor::new(NativeHost::new(resolve_ato_binary))),
        }
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

        let ato = agent
            .host()
            .resolve_binary("ato")
            .map_err(HostControlError::Resolve)?;
        let log_path = runner_serve_log_path();
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent).map_err(HostControlError::CreateLogDirectory)?;
        }
        agent
            .spawn(&SpawnSpec {
                program: ato,
                args: vec!["runner".into(), "serve".into()],
                env: Vec::new(),
                output: OutputSink::LogFile(log_path),
            })
            .map_err(HostControlError::Spawn)?;
        Ok(())
    }

    pub fn stop_runner(&self) -> Result<(), HostControlError> {
        self.runner_agent
            .lock()
            .map_err(|_| HostControlError::Poisoned)?
            .shutdown()
            .map_err(HostControlError::Teardown)
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
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HostControlError {
    #[error("command is not available to window '{0}'")]
    ForbiddenWindow(String),
    #[error("host state is unavailable")]
    Poisoned,
    #[error("could not resolve the ato binary: {0}")]
    Resolve(#[source] HostError),
    #[error("could not create the runner log directory: {0}")]
    CreateLogDirectory(#[source] std::io::Error),
    #[error("could not start the runner agent: {0}")]
    Spawn(#[source] HostError),
    #[error("could not stop the runner agent: {0}")]
    Teardown(#[source] HostError),
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
pub fn open_home(window: WebviewWindow, app: tauri::AppHandle) -> Result<(), String> {
    require_main_window(&window)?;
    if let Some(home) = app.get_webview_window(crate::HOME_WINDOW_LABEL) {
        home.show().map_err(|error| error.to_string())?;
        return home.set_focus().map_err(|error| error.to_string());
    }
    build_home_window(&app).map_err(|error| error.to_string())?;
    Ok(())
}

fn resolve_ato_binary(name: &str) -> Result<PathBuf, HostError> {
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    runner::resolve_on_path(name)
}

fn runner_serve_log_path() -> PathBuf {
    let base = std::env::var_os("ATO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".ato")))
        .or_else(|| std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".ato")))
        .unwrap_or_else(|| std::env::temp_dir().join("ato"));
    base.join("logs/runner-serve.log")
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
}
