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
