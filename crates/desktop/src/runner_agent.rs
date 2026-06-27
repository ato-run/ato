//! Desktop-side Connected Runner agent manager (PR 3).
//!
//! "Reuse PWA + spawn CLI": the runner backend (ato-api `/v1/runners`), the CLI
//! (`ato runner login` / `serve`), and the management UI (the embedded PWA
//! `/runners` + `ConnectRunnerGuide`) already exist. The Desktop's only job is to
//! perform the privileged local spawns and track the agent's local lifecycle:
//!
//!   - `register()` → `ato runner login` (browser device-flow; the operator's
//!     explicit authorization). The CLI writes the runner token to
//!     `~/.ato/runner/credentials.json`; **the Desktop never reads that token.**
//!   - `start()` / `stop()` → spawn / terminate `ato runner serve`.
//!   - `status()` → derived from credential presence + child liveness.
//!
//! **Foreground-only (beta):** the `serve` child is owned by the Desktop process
//! and killed on stop / shutdown, so the Connected Runner is online only while
//! Ato Desktop is running. State lives in process-wide statics (not a GPUI
//! global) precisely so the cx-less shutdown hook ([`shutdown`], called from
//! `crate::window::begin_shutdown`) can guarantee the agent is reaped.
//!
//! Daemonizing the agent (login item / service) so it stays online without the
//! Desktop is intentionally out of scope for beta.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::orchestrator::{RunnerChild, runner_credentials_path};

/// The `ato runner serve` child, when running. Owned by the Desktop.
static SERVE: Mutex<Option<RunnerChild>> = Mutex::new(None);
/// The serve child's PID, mirrored so the cx-less shutdown hook can group-kill
/// it without locking the (possibly poisoned) `SERVE` mutex. `0` = not running.
static SERVE_PID: AtomicU32 = AtomicU32::new(0);
/// The `ato runner login` child while a device-flow registration is in flight.
static LOGIN: Mutex<Option<RunnerChild>> = Mutex::new(None);
/// Last error surfaced by a register/serve child, for diagnostics.
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

/// Local lifecycle state of the Connected Runner agent, derived on demand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunnerStatus {
    /// No runner token on disk — never registered (or unregistered).
    NotRegistered,
    /// `ato runner login` (device-flow) is in progress.
    Registering,
    /// Registered and `ato runner serve` is running (online while Desktop is up).
    Serving,
    /// Registered but the serve agent is not running.
    Stopped,
    /// A register/serve child failed; carries a short reason.
    Error(String),
}

impl RunnerStatus {
    /// One-line, user-facing label (kept honest about the foreground-only model).
    pub fn label(&self) -> String {
        match self {
            RunnerStatus::NotRegistered => "Not registered".to_string(),
            RunnerStatus::Registering => "Registering…".to_string(),
            RunnerStatus::Serving => "Serving (online while Ato Desktop is open)".to_string(),
            RunnerStatus::Stopped => "Registered — runner stopped".to_string(),
            RunnerStatus::Error(reason) => format!("Error: {reason}"),
        }
    }
}

fn set_error(reason: impl Into<String>) {
    let reason = reason.into();
    tracing::warn!(%reason, "runner_agent: error");
    *LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason);
}

fn clear_error() {
    *LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Whether a runner token exists on disk (i.e. this device is registered).
pub fn is_registered() -> bool {
    runner_credentials_path().exists()
}

/// Reap any finished child processes and reconcile state. Cheap; safe to call
/// from the render/drain loop. Detects: device-flow completion (login child
/// exit), and unexpected serve exit (crash → surfaces an error + clears PID).
pub fn refresh() {
    // Login child: on exit, success means the credentials file now exists.
    {
        let mut login = LOGIN.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(child) = login.as_mut() {
            match child.child.try_wait() {
                Ok(Some(status)) => {
                    if status.success() && is_registered() {
                        clear_error();
                        tracing::info!("runner_agent: device registered");
                    } else {
                        set_error(format!(
                            "registration did not complete ({status}); see {}",
                            child.log_path.display()
                        ));
                    }
                    *login = None;
                }
                Ok(None) => {}
                Err(err) => {
                    set_error(format!("login wait failed: {err}"));
                    *login = None;
                }
            }
        }
    }

    // Serve child: an exit while we still hold a PID is unexpected (crash).
    {
        let mut serve = SERVE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(child) = serve.as_mut() {
            match child.child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        set_error(format!(
                            "runner agent exited ({status}); see {}",
                            child.log_path.display()
                        ));
                    }
                    *serve = None;
                    SERVE_PID.store(0, Ordering::SeqCst);
                }
                Ok(None) => {}
                Err(err) => {
                    set_error(format!("serve wait failed: {err}"));
                    *serve = None;
                    SERVE_PID.store(0, Ordering::SeqCst);
                }
            }
        }
    }
}

/// Current derived status. Reaps finished children first so the result is fresh.
pub fn status() -> RunnerStatus {
    refresh();
    if LOGIN.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
        return RunnerStatus::Registering;
    }
    if let Some(reason) = LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return RunnerStatus::Error(reason);
    }
    if !is_registered() {
        return RunnerStatus::NotRegistered;
    }
    if SERVE_PID.load(Ordering::SeqCst) != 0 {
        RunnerStatus::Serving
    } else {
        RunnerStatus::Stopped
    }
}

/// Begin device-flow registration (`ato runner login`). Opens the system browser
/// for the operator to authorize — that authorization is the explicit, native
/// confirmation for this privileged action. No-op if already in progress.
pub fn register() -> RunnerStatus {
    refresh();
    if LOGIN.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
        return RunnerStatus::Registering;
    }
    clear_error();
    match crate::orchestrator::spawn_runner_login() {
        Ok(child) => {
            *LOGIN.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
            RunnerStatus::Registering
        }
        Err(err) => {
            set_error(format!("could not start registration: {err}"));
            status()
        }
    }
}

/// Start the runner agent (`ato runner serve`). Requires registration first.
/// No-op if already serving.
pub fn start() -> RunnerStatus {
    refresh();
    if !is_registered() {
        set_error("register this device before starting the runner");
        return status();
    }
    if SERVE_PID.load(Ordering::SeqCst) != 0 {
        return RunnerStatus::Serving;
    }
    clear_error();
    match crate::orchestrator::spawn_runner_serve() {
        Ok(child) => {
            SERVE_PID.store(child.child.id(), Ordering::SeqCst);
            *SERVE.lock().unwrap_or_else(|e| e.into_inner()) = Some(child);
            tracing::info!("runner_agent: serve started");
            RunnerStatus::Serving
        }
        Err(err) => {
            set_error(format!("could not start runner: {err}"));
            status()
        }
    }
}

/// Stop the runner agent: terminate the whole `serve` process group (reaping any
/// sandboxed runs it spawned), then drop the child handle.
pub fn stop() -> RunnerStatus {
    let pid = SERVE_PID.swap(0, Ordering::SeqCst);
    if pid != 0 {
        crate::window::launch_window::kill_installed_launch_process_group(pid);
    }
    if let Some(mut child) = SERVE.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = child.child.kill();
        let _ = child.child.wait();
    }
    clear_error();
    tracing::info!("runner_agent: serve stopped");
    status()
}

/// Foreground-only teardown: kill the serve agent on Desktop shutdown so the
/// Connected Runner does not linger online after the Desktop exits. Safe to call
/// without a GPUI context (invoked from `crate::window::begin_shutdown`).
pub fn shutdown() {
    let pid = SERVE_PID.swap(0, Ordering::SeqCst);
    if pid != 0 {
        crate::window::launch_window::kill_installed_launch_process_group(pid);
        tracing::info!(pid, "runner_agent: serve terminated on shutdown");
    }
    if let Ok(mut guard) = SERVE.lock()
        && let Some(mut child) = guard.take()
    {
        let _ = child.child.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: register/start/stop touch the real `~/.ato` and spawn the CLI, so
    // they are exercised by the GUI/AODD smoke, not unit tests (a unit test must
    // not depend on host runner state or spawn processes). Only the pure,
    // side-effect-free derivations are unit-tested here.

    #[test]
    fn status_label_is_honest_about_foreground_only() {
        assert!(
            RunnerStatus::Serving
                .label()
                .contains("while Ato Desktop is open")
        );
        assert_eq!(RunnerStatus::NotRegistered.label(), "Not registered");
        assert_eq!(RunnerStatus::Registering.label(), "Registering…");
        assert_eq!(
            RunnerStatus::Error("boom".to_string()).label(),
            "Error: boom"
        );
    }
}
