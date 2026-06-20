//! Foreground runtime-tool install + log access.
//!
//! Streams `ato internal runtime install --tools … --json` progress into the
//! active Runtime Setup surface. The install job is tracked in a global
//! ([`super::ActiveRuntimeInstall`]) so onboarding's `Complete` and the
//! explicit cancel command can both reach it.

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;

use anyhow::{Result as AnyhowResult, anyhow, bail};
use capsule::runtime_setup::ToolKind;
use gpui::App;
use serde_json::Value;

use crate::proc_util::CommandNoWindowExt;

use super::types::{HELPER_TOO_OLD_MESSAGE, helper_lacks_runtime_subcommand};
use super::{
    ActiveRuntimeInstall, RuntimeInstallJob, ensure_install_global, push_runtime_setup,
    status::run_setup_status,
};

/// One queued UI event from the install worker thread. `terminal` marks the
/// final event so the foreground drain can clear the active-job global.
pub(super) struct RuntimeInstallUiEvent {
    pub(super) payload: Value,
    pub(super) terminal: bool,
}

/// Which `ato internal runtime …` subcommand a streamed job runs. The desktop
/// keeps the two semantically distinct (managed-toolchain *install* vs.
/// host-runtime *prepare*) even though both stream progress through the same
/// `runtimeInstall*` hydrate fields and share the single-job guard.
#[derive(Clone)]
pub(super) enum RuntimeJobKind {
    Install,
    Prepare,
    /// Repair the Ato Podman machine (#460 PR2): restart + verify.
    RepairHostRuntime,
    /// Run a Windows substrate remediation (#460 PR2): the action token plus the
    /// initiating surface, forwarded to `prepare-windows-substrate`.
    PrepareWindowsSubstrate {
        action: String,
        source_surface: String,
    },
}

impl RuntimeJobKind {
    /// The `ato internal runtime …` argv for this job. Install streams with
    /// `--json`; the host-runtime jobs use `--emit-json` (PR #440's flag).
    fn cli_args(&self, tools_arg: &str) -> Vec<String> {
        let s = |v: &str| v.to_string();
        match self {
            RuntimeJobKind::Install => vec![
                s("internal"),
                s("runtime"),
                s("install"),
                s("--tools"),
                s(tools_arg),
                s("--json"),
            ],
            RuntimeJobKind::Prepare => vec![
                s("internal"),
                s("runtime"),
                s("prepare"),
                s("--tools"),
                s(tools_arg),
                s("--emit-json"),
            ],
            RuntimeJobKind::RepairHostRuntime => vec![
                s("internal"),
                s("runtime"),
                s("repair-host-runtime"),
                s("--emit-json"),
            ],
            RuntimeJobKind::PrepareWindowsSubstrate {
                action,
                source_surface,
            } => vec![
                s("internal"),
                s("runtime"),
                s("prepare-windows-substrate"),
                s("--action"),
                action.clone(),
                s("--source-surface"),
                source_surface.clone(),
                s("--emit-json"),
            ],
        }
    }

    /// Fallback message when the child exits non-zero with no usable stderr.
    fn generic_failure(&self) -> &'static str {
        match self {
            RuntimeJobKind::Install => "runtime install failed",
            RuntimeJobKind::Prepare => "Podman setup failed",
            RuntimeJobKind::RepairHostRuntime => "Podman machine repair failed",
            RuntimeJobKind::PrepareWindowsSubstrate { .. } => "Windows substrate setup failed",
        }
    }

    /// Message when the child could not even be spawned.
    fn spawn_failure(&self) -> &'static str {
        match self {
            RuntimeJobKind::Install => "failed to start runtime install",
            RuntimeJobKind::Prepare => "failed to start Podman setup",
            RuntimeJobKind::RepairHostRuntime => "failed to start Podman machine repair",
            RuntimeJobKind::PrepareWindowsSubstrate { .. } => {
                "failed to start Windows substrate setup"
            }
        }
    }

    /// Cancellation message (only the foreground install path is cancellable
    /// today, but the other jobs share the same terminal-event plumbing).
    fn cancel_message(&self) -> &'static str {
        match self {
            RuntimeJobKind::Install => "runtime install cancelled",
            RuntimeJobKind::Prepare => "Podman setup cancelled",
            RuntimeJobKind::RepairHostRuntime => "Podman machine repair cancelled",
            RuntimeJobKind::PrepareWindowsSubstrate { .. } => "Windows substrate setup cancelled",
        }
    }
}

/// Kick off a foreground install of the requested managed tools.
pub(crate) fn start_runtime_install(cx: &mut App, request_id: Option<String>, tools: Vec<String>) {
    let tools = match parse_installable_tools(&tools) {
        Ok(tools) => tools,
        Err(err) => {
            ensure_install_global(cx);
            push_runtime_setup_error(cx, request_id, &format!("{err:#}"));
            return;
        }
    };
    spawn_runtime_job(cx, request_id, tools, RuntimeJobKind::Install);
}

/// Shared driver for both `install` and `prepare`: guard against a concurrent
/// job, resolve the helper, emit the `started` event, and spawn the streaming
/// worker. `tools` must already be validated/normalised for `kind`.
pub(super) fn spawn_runtime_job(
    cx: &mut App,
    request_id: Option<String>,
    tools: Vec<String>,
    kind: RuntimeJobKind,
) {
    ensure_install_global(cx);
    if cx.global::<ActiveRuntimeInstall>().0.is_some() {
        push_runtime_setup_error(cx, request_id, "a runtime install is already running");
        return;
    }

    let ato = match crate::orchestrator::resolve_ato_binary() {
        Ok(ato) => ato,
        Err(err) => {
            push_runtime_setup_error(
                cx,
                request_id,
                &format!("failed to resolve ato helper: {err:#}"),
            );
            return;
        }
    };

    let job = RuntimeInstallJob::new();
    cx.global_mut::<ActiveRuntimeInstall>().0 = Some(job.clone());
    let started = serde_json::json!({
        "ok": true,
        "requestId": request_id,
        "runtimeInstallStarted": { "tools": tools.clone() },
    });
    push_runtime_setup(cx, &started.to_string());

    let (tx, rx) = std::sync::mpsc::channel::<RuntimeInstallUiEvent>();
    let worker_request_id = request_id.clone();
    let worker_job = job.clone();
    std::thread::spawn(move || {
        run_runtime_worker(ato, tools, worker_request_id, worker_job, tx, kind);
    });

    let async_app = cx.to_async();
    let fe = cx.foreground_executor().clone();
    let be = cx.background_executor().clone();
    fe.spawn(async move {
        loop {
            let mut terminal = false;
            while let Ok(event) = rx.try_recv() {
                terminal |= event.terminal;
                // #460 PR3b: on a *successful* terminal event, the refreshed
                // setup status rides along under `runtimeInstallComplete.status`.
                // If the host runtime is now ready, this is where an interrupted
                // capsule launch resumes.
                let resume_status = if event.terminal {
                    refreshed_status_if_successful(&event.payload)
                } else {
                    None
                };
                let payload = event.payload.to_string();
                crate::webview_init_guard::wait_until_idle(&be).await;
                async_app.update(move |cx| {
                    if terminal {
                        ensure_install_global(cx);
                        cx.global_mut::<ActiveRuntimeInstall>().0 = None;
                    }
                    push_runtime_setup(cx, &payload);
                    if let Some(status) = resume_status {
                        super::launch_intent::try_resume_launch_if_ready(cx, &status);
                    }
                });
            }
            if terminal {
                break;
            }
            be.timer(std::time::Duration::from_millis(50)).await;
        }
    })
    .detach();
}

/// Validate and normalise a `--tools` request. Rejects detection-only / bundled
/// tools wholesale so a bad request cannot half-install.
pub(crate) fn parse_installable_tools(tools: &[String]) -> AnyhowResult<Vec<String>> {
    if tools.is_empty() {
        bail!("no runtime tools selected");
    }
    let mut parsed = Vec::with_capacity(tools.len());
    for tool in tools {
        let kind =
            ToolKind::parse_tool(tool).ok_or_else(|| anyhow!("unknown runtime tool: {tool}"))?;
        if !kind.is_managed_installable() {
            bail!(
                "{} cannot be installed by Ato during runtime setup",
                kind.as_str()
            );
        }
        let token = kind.as_str().to_string();
        if !parsed.contains(&token) {
            parsed.push(token);
        }
    }
    Ok(parsed)
}

fn run_runtime_worker(
    ato: PathBuf,
    tools: Vec<String>,
    request_id: Option<String>,
    job: RuntimeInstallJob,
    tx: std::sync::mpsc::Sender<RuntimeInstallUiEvent>,
    kind: RuntimeJobKind,
) {
    let tools_arg = tools.join(",");
    let mut command = Command::new(&ato);
    command
        .no_console_window()
        .args(kind.cli_args(&tools_arg))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            send_terminal_install_event(
                &tx,
                serde_json::json!({
                    "ok": false,
                    "requestId": request_id,
                    "runtimeInstallComplete": {
                        "success": false,
                        "canceled": false,
                        "error": format!("{}: {err}", kind.spawn_failure()),
                    },
                }),
            );
            return;
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    if let Ok(mut slot) = job.child.lock() {
        *slot = Some(child);
    }

    let stderr_reader = stderr.map(|stderr| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let mut reader = BufReader::new(stderr);
            let _ = reader.read_to_string(&mut text);
            text
        })
    });

    if let Some(stdout) = stdout {
        for line in BufReader::new(stdout).lines() {
            if job.cancel_requested.load(Ordering::Acquire) {
                break;
            }
            match line {
                Ok(line) if !line.trim().is_empty() => {
                    let event = serde_json::from_str::<Value>(&line).unwrap_or_else(|err| {
                        serde_json::json!({
                            "phase": "failed",
                            "message": format!("invalid install progress JSON: {err}"),
                        })
                    });
                    let _ = tx.send(RuntimeInstallUiEvent {
                        payload: serde_json::json!({
                            "ok": true,
                            "requestId": request_id.clone(),
                            "runtimeInstallProgress": event,
                        }),
                        terminal: false,
                    });
                }
                Ok(_) => {}
                Err(err) => {
                    let _ = tx.send(RuntimeInstallUiEvent {
                        payload: serde_json::json!({
                            "ok": false,
                            "requestId": request_id.clone(),
                            "error": { "message": format!("failed to read install progress: {err}") },
                        }),
                        terminal: false,
                    });
                    break;
                }
            }
        }
    }

    let status = match job.child.lock() {
        Ok(mut slot) => slot.take().map(|mut child| child.wait()),
        Err(_) => None,
    };
    let stderr = stderr_reader
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();
    let canceled = job.cancel_requested.load(Ordering::Acquire);
    let success = matches!(status, Some(Ok(status)) if status.success()) && !canceled;
    let setup_status = if canceled {
        None
    } else {
        run_setup_status(&ato).ok()
    };
    let error = if success {
        None
    } else if canceled {
        Some(kind.cancel_message().to_string())
    } else if helper_lacks_runtime_subcommand(&stderr) {
        Some(HELPER_TOO_OLD_MESSAGE.to_string())
    } else if !stderr.trim().is_empty() {
        Some(stderr.trim().to_string())
    } else {
        Some(kind.generic_failure().to_string())
    };

    send_terminal_install_event(
        &tx,
        serde_json::json!({
            "ok": success,
            "requestId": request_id.clone(),
            "runtimeInstallComplete": {
                "success": success,
                "canceled": canceled,
                "error": error,
                "status": setup_status,
            },
        }),
    );
}

/// Reveal the desktop log directory (`~/.ato/logs`) in the OS file manager so
/// the user can read runtime-setup failures. Settings-only.
pub(crate) fn open_runtime_setup_logs(cx: &mut App, request_id: Option<String>) {
    let logs_dir = capsule::common::paths::ato_path_or_workspace_tmp("logs");
    let result = crate::proc_util::open_path(&logs_dir);
    let response = match &result {
        Ok(()) => serde_json::json!({
            "ok": true,
            "requestId": request_id,
            "runtimeSetupLogsOpened": { "path": logs_dir.display().to_string() },
        }),
        Err(err) => serde_json::json!({
            "ok": false,
            "requestId": request_id,
            "error": { "message": format!("failed to open log directory: {err}") },
        }),
    };
    push_runtime_setup(cx, &response.to_string());
}

/// Extract the refreshed [`RuntimeSetupStatus`] from a terminal install/prepare
/// event, but only when the job **succeeded**. Returns `None` on failure /
/// cancellation or when no status snapshot rode along. Used by the foreground
/// drain to decide whether to resume an interrupted capsule launch (#460 PR3b).
fn refreshed_status_if_successful(
    payload: &Value,
) -> Option<capsule::runtime_setup::RuntimeSetupStatus> {
    let complete = payload.get("runtimeInstallComplete")?;
    if !complete
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let status = complete.get("status")?;
    serde_json::from_value(status.clone()).ok()
}

fn send_terminal_install_event(
    tx: &std::sync::mpsc::Sender<RuntimeInstallUiEvent>,
    payload: Value,
) {
    let _ = tx.send(RuntimeInstallUiEvent {
        payload,
        terminal: true,
    });
}

pub(super) fn push_runtime_setup_error(cx: &mut App, request_id: Option<String>, message: &str) {
    let response = serde_json::json!({
        "ok": false,
        "requestId": request_id,
        "error": { "message": message },
    });
    push_runtime_setup(cx, &response.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_installable_tools_allows_managed_language_tools() {
        let tools = vec!["node".to_string(), "uv".to_string(), "python".to_string()];
        assert_eq!(
            parse_installable_tools(&tools).unwrap(),
            vec!["node".to_string(), "uv".to_string(), "python".to_string()]
        );
    }

    #[test]
    fn parse_installable_tools_rejects_detection_only_tools() {
        let err = parse_installable_tools(&["podman".to_string()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot be installed"));
    }

    #[test]
    fn parse_installable_tools_rejects_empty() {
        assert!(parse_installable_tools(&[]).is_err());
    }

    // ── #460 PR2b: substrate job argv (the reboot_required CTA must route to
    //    prepare-windows-substrate, NOT resume) ────────────────────────────────

    #[test]
    fn install_and_prepare_cli_args() {
        assert_eq!(
            RuntimeJobKind::Install.cli_args("node,uv"),
            vec![
                "internal", "runtime", "install", "--tools", "node,uv", "--json"
            ]
        );
        assert_eq!(
            RuntimeJobKind::Prepare.cli_args("podman"),
            vec![
                "internal",
                "runtime",
                "prepare",
                "--tools",
                "podman",
                "--emit-json"
            ]
        );
    }

    #[test]
    fn repair_cli_args() {
        assert_eq!(
            RuntimeJobKind::RepairHostRuntime.cli_args(""),
            vec!["internal", "runtime", "repair-host-runtime", "--emit-json"]
        );
    }

    // ── #460 PR3b: only a successful terminal event yields a resume status ──────

    #[test]
    fn refreshed_status_requires_success_and_a_status_snapshot() {
        // Success + status snapshot → parsed status.
        let ok = serde_json::json!({
            "runtimeInstallComplete": {
                "success": true,
                "status": { "tools": [], "windows_substrate": null },
            },
        });
        assert!(refreshed_status_if_successful(&ok).is_some());

        // Failure → no resume status even if a snapshot is present.
        let failed = serde_json::json!({
            "runtimeInstallComplete": {
                "success": false,
                "status": { "tools": [], "windows_substrate": null },
            },
        });
        assert!(refreshed_status_if_successful(&failed).is_none());

        // Success but no snapshot (e.g. cancelled path) → none.
        let no_status = serde_json::json!({
            "runtimeInstallComplete": { "success": true, "status": null },
        });
        assert!(refreshed_status_if_successful(&no_status).is_none());

        // Not a terminal completion payload at all → none.
        let progress = serde_json::json!({ "runtimeInstallProgress": { "phase": "installing" } });
        assert!(refreshed_status_if_successful(&progress).is_none());
    }

    #[test]
    fn reboot_required_routes_through_prepare_windows_substrate() {
        // Regression guard (#467 review): the reboot_required action persists the
        // resume marker via prepare-windows-substrate; it must NOT be the
        // read-only resume-after-reboot command.
        let args = RuntimeJobKind::PrepareWindowsSubstrate {
            action: "reboot_required".to_string(),
            source_surface: "onboarding".to_string(),
        }
        .cli_args("");
        assert_eq!(
            args,
            vec![
                "internal",
                "runtime",
                "prepare-windows-substrate",
                "--action",
                "reboot_required",
                "--source-surface",
                "onboarding",
                "--emit-json",
            ]
        );
        assert!(!args.iter().any(|a| a == "resume-after-reboot"));
    }
}
