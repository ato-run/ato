//! `DesktopHost` — the typed command adapter the bundled first-party UI invokes.
//!
//! The Tauri shell owns NO capsule execution logic. Every privileged command
//! here delegates to the host-agnostic [`runner`] supervisor, which drives the
//! `ato` CLI (the sole owner of capsule execution / isolation / runner control)
//! as a supervised child. The shell's job is to translate `tauri::invoke`
//! calls from the local UI into `runner` operations and back.
//!
//! First slice: the **Connected Runner agent** lifecycle — the shell-side of the
//! `ato://runner/{start,stop}` verbs ([`protocol::intent::PrivilegedIntent`]).
//! `ato runner serve` is foreground-only: the agent is online only while the
//! shell runs, so it is spawned as a supervised process group and torn down on
//! stop / shutdown via the Step-4/6 primitives.
//!
//! Commands are registered only for the `main` window (the bundled local-asset
//! UI); see `capabilities/default.json`. Remote origins never reach them.

use std::path::PathBuf;
use std::sync::Mutex;

use protocol::intent::PrivilegedIntent;
use runner::{HostError, NativeHost, OutputSink, ProcessSupervisor, RunnerHost, SpawnSpec};
use serde::Serialize;
use tauri::State;

/// Shell-owned host state managed by Tauri. Holds one supervisor dedicated to
/// the local `ato runner serve` agent (at most one child at a time).
pub struct DesktopHost {
    runner_agent: Mutex<ProcessSupervisor<NativeHost>>,
}

impl DesktopHost {
    /// Build the host with a [`NativeHost`] that resolves ato-family binaries
    /// next to the shell executable first (the bundle layout), then on `PATH`.
    pub fn new() -> Self {
        Self {
            runner_agent: Mutex::new(ProcessSupervisor::new(NativeHost::new(resolve_ato_binary))),
        }
    }

    /// Whether the runner agent is currently supervised (alive). Reaps first so
    /// a `serve` child that exited on its own is not reported as running.
    pub fn runner_running(&self) -> bool {
        let mut agent = self.runner_agent.lock().expect("runner agent mutex poisoned");
        agent.reap();
        agent.supervised_count() > 0
    }

    /// Start `ato runner serve` as a supervised process group. Idempotent: a
    /// no-op (Ok) if the agent is already running.
    pub fn start_runner(&self) -> Result<(), HostControlError> {
        let mut agent = self.runner_agent.lock().map_err(|_| HostControlError::Poisoned)?;
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
            let _ = std::fs::create_dir_all(parent);
        }
        let spec = SpawnSpec {
            program: ato,
            args: vec!["runner".to_string(), "serve".to_string()],
            env: Vec::new(),
            output: OutputSink::LogFile(log_path),
        };
        agent.spawn(&spec).map_err(HostControlError::Spawn)?;
        Ok(())
    }

    /// Stop the runner agent, tearing down the whole `serve` process group.
    /// Idempotent: Ok when nothing is running.
    pub fn stop_runner(&self) -> Result<(), HostControlError> {
        let mut agent = self.runner_agent.lock().map_err(|_| HostControlError::Poisoned)?;
        agent.shutdown().map_err(HostControlError::Teardown)
    }

    /// Dispatch a classified [`PrivilegedIntent`] to the matching host action.
    /// The shell has already validated origin trust before producing the verb;
    /// this maps the shared vocabulary onto `runner` operations.
    pub fn dispatch_privileged_intent(
        &self,
        intent: &PrivilegedIntent,
    ) -> Result<(), HostControlError> {
        match intent {
            PrivilegedIntent::RunnerStart => self.start_runner(),
            PrivilegedIntent::RunnerStop => self.stop_runner(),
            // Registration and per-capsule Run are separate slices (device-flow
            // login window; the consent-gated launch path). Not yet wired in the
            // Tauri shell — reject rather than silently no-op.
            PrivilegedIntent::RunnerRegister => Err(HostControlError::Unsupported("runner/register")),
            PrivilegedIntent::Run { .. } => Err(HostControlError::Unsupported("run")),
        }
    }

    /// Classify an intercepted `ato://runner/*` navigation and dispatch it. The
    /// caller has already established trust (the bundled main window is the only
    /// window granted these commands); this maps the URI onto a host action via
    /// the shared, url-free [`protocol::intent::parse_runner_control_intent`]
    /// parser so the Tauri and GPUI shells never drift on verb spelling.
    pub fn dispatch_intent_uri(&self, uri: &str) -> Result<(), HostControlError> {
        match protocol::intent::parse_runner_control_intent(uri) {
            Some(intent) => self.dispatch_privileged_intent(&intent),
            None => Err(HostControlError::UnrecognizedIntent(uri.to_string())),
        }
    }
}

impl Default for DesktopHost {
    fn default() -> Self {
        Self::new()
    }
}

/// Failures a host control command can surface to the UI.
#[derive(Debug, thiserror::Error)]
pub enum HostControlError {
    #[error("host state is unavailable")]
    Poisoned,
    #[error("could not resolve the ato binary: {0}")]
    Resolve(#[source] HostError),
    #[error("could not start the runner agent: {0}")]
    Spawn(#[source] HostError),
    #[error("could not stop the runner agent: {0}")]
    Teardown(#[source] HostError),
    #[error("intent verb '{0}' is not yet supported by the Tauri shell")]
    Unsupported(&'static str),
    #[error("not a recognized runner-control intent: {0}")]
    UnrecognizedIntent(String),
}

/// Runner-agent status returned to the UI.
#[derive(Debug, Clone, Serialize)]
pub struct RunnerStatus {
    pub running: bool,
}

// ── Tauri command surface (thin adapters over DesktopHost) ──────────────────

/// `invoke('runner_status')` — is the local runner agent online?
#[tauri::command]
pub fn runner_status(host: State<'_, DesktopHost>) -> RunnerStatus {
    RunnerStatus {
        running: host.runner_running(),
    }
}

/// `invoke('runner_start')` — bring the local runner agent online.
#[tauri::command]
pub fn runner_start(host: State<'_, DesktopHost>) -> Result<(), String> {
    host.start_runner().map_err(|e| e.to_string())
}

/// `invoke('runner_stop')` — take the local runner agent offline.
#[tauri::command]
pub fn runner_stop(host: State<'_, DesktopHost>) -> Result<(), String> {
    host.stop_runner().map_err(|e| e.to_string())
}

/// `invoke('dispatch_privileged_intent', { intent })` — route a classified
/// `ato://` verb (the shared [`PrivilegedIntent`] vocabulary) to its host
/// action. The UI has already validated origin trust before calling this.
#[tauri::command]
pub fn dispatch_privileged_intent(
    host: State<'_, DesktopHost>,
    intent: PrivilegedIntent,
) -> Result<(), String> {
    host.dispatch_privileged_intent(&intent)
        .map_err(|e| e.to_string())
}

/// `invoke('dispatch_intent_uri', { uri })` — hand the shell an intercepted
/// `ato://runner/*` URI to classify (via the shared parser) and dispatch. The
/// eventual navigation handler routes intercepted navigations through here.
#[tauri::command]
pub fn dispatch_intent_uri(host: State<'_, DesktopHost>, uri: String) -> Result<(), String> {
    host.dispatch_intent_uri(&uri).map_err(|e| e.to_string())
}

// ── Host policy (binary + log resolution) ───────────────────────────────────

/// Resolve an ato-family binary: next to the shell executable first (the bundle
/// ships `ato` alongside the shell), then on `PATH`.
fn resolve_ato_binary(name: &str) -> Result<PathBuf, HostError> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    runner::resolve_on_path(name)
}

/// Log file for the supervised `ato runner serve` child. Under `$ATO_HOME/logs`
/// when set, else the user home `~/.ato/logs`, else a temp dir — created lazily
/// by the caller.
fn runner_serve_log_path() -> PathBuf {
    ato_logs_dir().join("runner-serve.log")
}

fn ato_logs_dir() -> PathBuf {
    let base = std::env::var_os("ATO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".ato")))
        .or_else(|| std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join(".ato")))
        .unwrap_or_else(|| std::env::temp_dir().join("ato"));
    base.join("logs")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_host_reports_runner_offline() {
        let host = DesktopHost::new();
        assert!(!host.runner_running());
    }

    #[test]
    fn stop_is_idempotent_when_nothing_is_running() {
        let host = DesktopHost::new();
        // Tearing down an empty supervisor must succeed and stay offline.
        host.stop_runner().expect("stop on empty is a no-op");
        assert!(!host.runner_running());
    }

    #[test]
    fn log_path_is_under_an_ato_logs_dir() {
        let path = runner_serve_log_path();
        assert!(path.ends_with("logs/runner-serve.log"));
    }

    #[test]
    fn unwired_verbs_are_rejected_not_silently_ignored() {
        let host = DesktopHost::new();
        assert!(matches!(
            host.dispatch_privileged_intent(&PrivilegedIntent::RunnerRegister),
            Err(HostControlError::Unsupported("runner/register"))
        ));
        assert!(matches!(
            host.dispatch_privileged_intent(&PrivilegedIntent::Run {
                source: "capsule://ato.run/a/b".into(),
                run_id: None,
                origin: "https://app.ato.run".into(),
            }),
            Err(HostControlError::Unsupported("run"))
        ));
    }

    #[test]
    fn runner_stop_verb_routes_to_stop_and_stays_offline() {
        let host = DesktopHost::new();
        // Dispatching the stop verb on an idle host is a no-op success.
        host.dispatch_privileged_intent(&PrivilegedIntent::RunnerStop)
            .expect("stop verb on idle host is a no-op");
        assert!(!host.runner_running());
    }

    #[test]
    fn intent_uri_for_stop_routes_through_the_shared_parser() {
        let host = DesktopHost::new();
        // An `ato://runner/stop` navigation is classified and dispatched.
        host.dispatch_intent_uri("ato://runner/stop")
            .expect("runner/stop uri dispatches");
        assert!(!host.runner_running());
    }

    #[test]
    fn unrecognized_intent_uri_is_rejected() {
        let host = DesktopHost::new();
        assert!(matches!(
            host.dispatch_intent_uri("ato://run?source=x"),
            Err(HostControlError::UnrecognizedIntent(_))
        ));
        assert!(matches!(
            host.dispatch_intent_uri("https://evil.example"),
            Err(HostControlError::UnrecognizedIntent(_))
        ));
    }
}
