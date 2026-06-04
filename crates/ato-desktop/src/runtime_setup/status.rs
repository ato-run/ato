//! Runtime-setup status probe.
//!
//! The desktop never inspects the host directly; it shells out to
//! `ato internal runtime setup-status --json` and forwards the result to the
//! active Runtime Setup surface (onboarding or settings).

use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result as AnyhowResult, bail};
use serde_json::Value;

use crate::proc_util::CommandNoWindowExt;

use super::push_runtime_setup;
use super::types::{HELPER_TOO_OLD_MESSAGE, helper_lacks_runtime_subcommand};

/// Spawn the status probe off the UI thread and hydrate the active surface with
/// the result (or a typed error) once the WebView is idle.
pub(crate) fn spawn_runtime_setup_status(cx: &mut gpui::App, request_id: Option<String>) {
    let async_app = cx.to_async();
    let fe = cx.foreground_executor().clone();
    let be = cx.background_executor().clone();
    let be_for_work = be.clone();
    fe.spawn(async move {
        let payload = be_for_work
            .spawn(async move { runtime_setup_status_response(request_id) })
            .await;
        crate::webview_init_guard::wait_until_idle(&be).await;
        async_app.update(move |cx| {
            push_runtime_setup(cx, &payload.to_string());
            // #460 PR3b: keep the pending-launch banner in sync whenever a
            // surface (re)loads its status — reflects any recorded launch intent.
            let pending = super::launch_intent::peek_pending_launch();
            super::launch_intent::push_pending_launch(cx, pending.as_ref());
        });
    })
    .detach();
}

fn runtime_setup_status_response(request_id: Option<String>) -> Value {
    match crate::orchestrator::resolve_ato_binary().and_then(|ato| run_setup_status(&ato)) {
        Ok(status) => serde_json::json!({
            "ok": true,
            "requestId": request_id,
            "runtimeSetupStatus": status,
        }),
        Err(err) => serde_json::json!({
            "ok": false,
            "requestId": request_id,
            "error": { "message": format!("{err:#}") },
        }),
    }
}

/// Spawn the read-only resume-after-reboot probe off the UI thread and hydrate
/// the active surface with the result. Mirrors [`spawn_runtime_setup_status`]
/// but runs `ato internal runtime resume-after-reboot --json`, whose payload
/// already carries the refreshed `runtimeSetupStatus` and the `resumeOutcome`.
/// Read-only — never mutates host state. See #460 PR2.
pub(crate) fn spawn_runtime_setup_resume(cx: &mut gpui::App, request_id: Option<String>) {
    let async_app = cx.to_async();
    let fe = cx.foreground_executor().clone();
    let be = cx.background_executor().clone();
    let be_for_work = be.clone();
    fe.spawn(async move {
        let payload = be_for_work
            .spawn(async move { runtime_setup_resume_response(request_id) })
            .await;
        crate::webview_init_guard::wait_until_idle(&be).await;
        async_app.update(move |cx| {
            push_runtime_setup(cx, &payload.to_string());
            // #460 PR3b: reboot→launch continuity. The CLI's read-only resume
            // payload carries `launchContinuation`; the Desktop consumes its own
            // marker and re-gates on host readiness before resuming the launch.
            if let Some(inner) = payload.get("runtimeSetupResume") {
                super::launch_intent::apply_reboot_resume_launch(cx, inner);
            }
        });
    })
    .detach();
}

fn runtime_setup_resume_response(request_id: Option<String>) -> Value {
    let ato = match crate::orchestrator::resolve_ato_binary() {
        Ok(ato) => ato,
        Err(err) => {
            return serde_json::json!({
                "ok": false,
                "requestId": request_id,
                "error": { "message": format!("failed to resolve ato helper: {err:#}") },
            });
        }
    };
    let output = Command::new(&ato)
        .no_console_window()
        .args(["internal", "runtime", "resume-after-reboot", "--json"])
        .stdin(Stdio::null())
        .output();
    match output {
        Ok(out) if out.status.success() => {
            // The command already prints a JSON object with `resumeOutcome` and
            // `runtimeSetupStatus`; forward it under a `runtimeSetupResume` field
            // plus the request id so the surface can correlate it.
            let parsed: Value =
                serde_json::from_slice(&out.stdout).unwrap_or_else(|_| serde_json::json!({}));
            serde_json::json!({
                "ok": true,
                "requestId": request_id,
                "runtimeSetupResume": parsed,
            })
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let message = if helper_lacks_runtime_subcommand(&stderr) {
                HELPER_TOO_OLD_MESSAGE.to_string()
            } else if !stderr.trim().is_empty() {
                stderr.trim().to_string()
            } else {
                "resume-after-reboot failed".to_string()
            };
            serde_json::json!({
                "ok": false,
                "requestId": request_id,
                "error": { "message": message },
            })
        }
        Err(err) => serde_json::json!({
            "ok": false,
            "requestId": request_id,
            "error": { "message": format!("failed to run resume-after-reboot: {err}") },
        }),
    }
}

/// Run `ato internal runtime setup-status --json` and parse the result.
///
/// A helper that predates the `internal runtime` subcommand is mapped to a
/// clear "helper too old" error rather than the raw clap message — this is the
/// version-skew that broke onboarding install when the bundled/dev `ato` lagged
/// the desktop.
pub(crate) fn run_setup_status(ato: &Path) -> AnyhowResult<Value> {
    let output = Command::new(ato)
        .no_console_window()
        .args(["internal", "runtime", "setup-status", "--json"])
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("failed to run {}", ato.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if helper_lacks_runtime_subcommand(&stderr) {
            bail!("{HELPER_TOO_OLD_MESSAGE}");
        }
        bail!("runtime setup-status failed: {}", stderr.trim());
    }
    serde_json::from_slice(&output.stdout).context("runtime setup-status emitted invalid JSON")
}
