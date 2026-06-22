//! `ato-import` system capsule — GitHub Import review surface.
//!
//! Hosts the typed IPC commands posted by the import review HTML
//! (`assets/system/ato-import/index.html`). All long-running work
//! (subprocess + git clone + run) happens on the background executor;
//! the dispatch handler returns immediately after kicking off the
//! background task and pushing the transient "running" snapshot to
//! the UI.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use capsule::common::paths::ato_path_or_workspace_tmp;
use gpui::{AnyWindowHandle, App, BackgroundExecutor};
use serde::Deserialize;

use crate::source_import_api::{
    ApiClient, ApiCreds, AttemptStatus, discover as discover_api_creds,
};
use crate::source_import_runner::{
    infer as runner_infer, run_with_recipe as runner_run,
    stop_import_preview_session as runner_stop_import_preview,
};
use crate::source_import_session::{GitHubImportSessionState, ImportOutput};
use crate::system_capsule::broker::{BrokerError, Capability};
use crate::window::import_window::{ImportApiCreds, push_current_snapshot, session_arc};

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImportCommand {
    /// Begin a new import session for `url`. Replaces any existing
    /// session state. Triggers inference on a background thread.
    Open { url: String },
    /// Replace the editable recipe TOML with user input.
    EditRecipe { toml: String },
    /// Run the current editable TOML through `ato import --run`.
    Run,
    /// User clicked "Submit this working recipe". PR-2 stores intent
    /// only; PR-3 will POST to the source-imports API.
    SubmitIntent,
    /// Retry inference after a previous inference failure.
    RetryInference,
    /// User confirmed they want to allow unsafe execution
    /// (e.g. source/native runtime). After setting the session
    /// flag, re-dispatches Run.
    ConfirmUnsafeExecution,
    /// Community Import surface: user picked a published community
    /// recipe. Closes this window and opens the launch consent flow
    /// with the pre-selected `ctoml_id` threaded through, so the CLI
    /// resolves the community recipe instead of inferring.
    LaunchCommunityToml {
        handle: String,
        ctoml_id: String,
        label: String,
    },
    /// Community Import surface: fetch and display a single recipe's raw
    /// `capsule.toml` so the user can inspect how two same-titled community
    /// recipes differ. Read-only; does not close the window.
    ViewCommunityToml { ctoml_id: String },
    /// Community Import surface: re-run community discovery in place after
    /// a transient fetch error. Does NOT close the window — it re-fetches
    /// candidates and pushes a fresh snapshot into the same page.
    RetryCommunity { source: String, label: String },
    /// Community Import surface: explicit "Import from GitHub instead"
    /// secondary action shown when no community recipe matches. Closes
    /// this window and opens the GitHub Import (infer) surface for the
    /// same source. Never reached automatically — only on user click.
    ImportFromGithubSource { source: String },
    /// User dismissed the window. Closes the host window.
    Close,
}

impl ImportCommand {
    pub fn required_capability(&self) -> Capability {
        match self {
            // Import surface needs to spawn its own WebView/window the
            // first time. Subsequent commands reuse it via the slot global.
            ImportCommand::Open { .. } => Capability::WebviewCreate,
            ImportCommand::EditRecipe { .. } => Capability::WebviewCreate,
            ImportCommand::Run => Capability::WebviewCreate,
            ImportCommand::SubmitIntent => Capability::WebviewCreate,
            ImportCommand::RetryInference => Capability::WebviewCreate,
            ImportCommand::ConfirmUnsafeExecution => Capability::WebviewCreate,
            // Both community actions tear down this window and spawn a new
            // one (consent flow / GitHub import surface).
            ImportCommand::LaunchCommunityToml { .. } => Capability::WebviewCreate,
            ImportCommand::ViewCommunityToml { .. } => Capability::WebviewCreate,
            ImportCommand::RetryCommunity { .. } => Capability::WebviewCreate,
            ImportCommand::ImportFromGithubSource { .. } => Capability::WebviewCreate,
            ImportCommand::Close => Capability::WindowsClose,
        }
    }
}

pub fn dispatch(
    cx: &mut App,
    host: AnyWindowHandle,
    command: ImportCommand,
) -> Result<(), BrokerError> {
    match command {
        ImportCommand::Open { url } => begin_open(cx, url),
        ImportCommand::EditRecipe { toml } => handle_edit(cx, toml),
        ImportCommand::Run => handle_run(cx),
        ImportCommand::SubmitIntent => handle_submit_intent(cx),
        ImportCommand::RetryInference => handle_retry_inference(cx),
        ImportCommand::ConfirmUnsafeExecution => handle_confirm_unsafe(cx),
        ImportCommand::LaunchCommunityToml {
            handle,
            ctoml_id,
            label,
        } => handle_launch_community_toml(cx, host, handle, ctoml_id, label),
        ImportCommand::ViewCommunityToml { ctoml_id } => {
            crate::window::community_import_window::fetch_candidate_detail(cx, ctoml_id);
        }
        ImportCommand::RetryCommunity { source, label } => {
            // Re-fetch in place; the page already reset itself to loading.
            crate::window::community_import_window::refetch(cx, source, label);
        }
        ImportCommand::ImportFromGithubSource { source } => {
            // Tear down the community review window, then hand off to the
            // GitHub Import (infer) surface for the same source.
            let _ = host.update(cx, |_, window, _| window.remove_window());
            if let Err(error) = crate::window::import_window::open_with_url(cx, source) {
                tracing::error!(
                    ?error,
                    "ato-import: community → GitHub import fallback failed"
                );
            }
        }
        ImportCommand::Close => {
            stop_active_import_preview(cx, "window_close");
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
    }
    Ok(())
}

/// Community Import → user picked a published recipe. Close the review
/// window and open the launch consent flow carrying the selected
/// `ctoml_id` so the CLI resolves the community recipe directly.
fn handle_launch_community_toml(
    cx: &mut App,
    host: AnyWindowHandle,
    handle: String,
    ctoml_id: String,
    label: String,
) {
    let route = crate::state::GuestRoute::CapsuleHandle {
        handle,
        label,
        community_toml_id: Some(ctoml_id),
    };
    let _ = host.update(cx, |_, window, _| window.remove_window());
    if let Err(error) = crate::window::launch_window::open_consent_window_for_route(cx, route) {
        tracing::error!(?error, "ato-import: community launch consent open failed");
    }
}

fn current_creds(cx: &App) -> Option<ApiCreds> {
    cx.try_global::<ImportApiCreds>().and_then(|c| c.0.clone())
}

fn store_creds(cx: &mut App, creds: Option<ApiCreds>) {
    cx.set_global(ImportApiCreds(creds));
}

/// Begin a new GitHub import session for `url`. Triggers source
/// resolution + recipe inference on the background executor and
/// pushes snapshots into the active import window's WebView.
///
/// Exposed as a free function so entry points (control bar URL bar,
/// ato-dock modal, ato-start search) can kick off an import after
/// opening the window, without going through the IPC envelope.
pub fn begin_open(cx: &mut App, url: String) {
    stop_active_import_preview(cx, "new_import");
    let session_arc = session_arc(cx);
    // begin_resolve fully resets the session; signed_in and
    // source_import_id come back to false / None. Clear any cached
    // creds from a previous session too so the next discover_api_creds
    // is the source of truth.
    {
        let mut session = match session_arc.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        match session.begin_resolve(&url) {
            Ok(_) => session.begin_inference(),
            Err(error) => {
                tracing::warn!(?error, %url, "ato-import: normalize failed");
                return;
            }
        }
    }
    store_creds(cx, None);
    push_current_snapshot(cx);

    // Spawn inference + auth discovery in parallel on the background
    // executor. After both complete: write inferred output to session,
    // record signed_in, and (if signed in) POST /v1/source-imports.
    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();
    let session_for_bg = session_arc.clone();
    let url_for_bg = url.clone();
    fe.spawn(async move {
        let infer_url = url_for_bg.clone();
        let infer_task = be.spawn(async move { runner_infer(&infer_url) });
        let creds_task = be.spawn(async move { discover_api_creds() });
        let outcome: Result<ImportOutput, anyhow::Error> = infer_task.await;
        let creds: Option<ApiCreds> = creds_task.await;

        // If inference succeeded AND we have creds, fire the
        // create_source_import call on the background executor too.
        let create_id_task = match (&outcome, creds.as_ref()) {
            (Ok(output), Some(creds)) => {
                let creds = creds.clone();
                let source = output.source.clone();
                Some(be.spawn(async move { ApiClient::new(creds).create_source_import(&source) }))
            }
            _ => None,
        };
        let create_id: Option<Result<String, anyhow::Error>> = match create_id_task {
            Some(task) => Some(task.await),
            None => None,
        };

        aa.update(move |cx| {
            store_creds(cx, creds.clone());
            match outcome {
                Ok(output) => {
                    if let Ok(mut session) = session_for_bg.lock() {
                        if let Err(error) = session.apply_inferred_output(output) {
                            tracing::warn!(?error, "ato-import: apply_inferred failed");
                        }
                        session.set_signed_in(creds.is_some());
                        if let Some(result) = create_id {
                            match result {
                                Ok(id) => session.set_source_import_id(id),
                                Err(error) => {
                                    tracing::warn!(
                                        ?error,
                                        "ato-import: create_source_import failed"
                                    );
                                }
                            }
                        }
                    }
                }
                Err(error) => {
                    if let Ok(mut session) = session_for_bg.lock() {
                        let _ = session.record_inference_failure(
                            "cli_inference_error".to_string(),
                            format!("{error:#}"),
                        );
                        session.set_signed_in(creds.is_some());
                    }
                }
            }
            push_current_snapshot(cx);
        });
    })
    .detach();
}

pub(crate) fn handle_confirm_unsafe(cx: &mut App) {
    let session_arc = session_arc(cx);
    {
        let mut session = match session_arc.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        session.confirm_unsafe_execution();
    }
    push_current_snapshot(cx);
    handle_run(cx);
}

fn handle_edit(cx: &mut App, toml: String) {
    let session_arc = session_arc(cx);
    {
        let mut session = match session_arc.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if let Err(error) = session.edit_recipe(toml) {
            tracing::debug!(?error, "ato-import: edit_recipe rejected");
        }
    }
    // Editing is high-frequency (keystrokes); skip pushing snapshot
    // back to avoid re-rendering the textarea under the user's cursor.
}

fn handle_retry_inference(cx: &mut App) {
    stop_active_import_preview(cx, "retry_inference");
    let session_arc = session_arc(cx);
    let repo_url = {
        let mut session = match session_arc.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if let Err(error) = session.retry_inference() {
            tracing::debug!(?error, "ato-import: retry_inference rejected");
            return;
        }
        match session.repo() {
            Some(r) => r.source_url_normalized.clone(),
            None => {
                tracing::warn!("ato-import: retry_inference without resolved repo");
                return;
            }
        }
    };
    push_current_snapshot(cx);

    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();
    let session_for_bg = session_arc.clone();
    fe.spawn(async move {
        let outcome: Result<ImportOutput, anyhow::Error> =
            be.spawn(async move { runner_infer(&repo_url) }).await;
        aa.update(move |cx| {
            match outcome {
                Ok(output) => {
                    if let Ok(mut session) = session_for_bg.lock()
                        && let Err(error) = session.apply_inferred_output(output)
                    {
                        tracing::warn!(?error, "ato-import: apply_inferred on retry failed");
                    }
                }
                Err(error) => {
                    if let Ok(mut session) = session_for_bg.lock() {
                        let _ = session.record_inference_failure(
                            "cli_inference_error".to_string(),
                            format!("{error:#}"),
                        );
                    }
                }
            }
            push_current_snapshot(cx);
        });
    })
    .detach();
}

pub(crate) fn handle_run(cx: &mut App) {
    let session_arc = session_arc(cx);
    let (repo_url, recipe_toml, allow_unsafe) = {
        let mut session = match session_arc.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if let Err(error) = session.start_run() {
            tracing::warn!(?error, "ato-import: start_run rejected");
            return;
        }
        let repo = match session.repo() {
            Some(r) => r.source_url_normalized.clone(),
            None => {
                tracing::warn!("ato-import: run requested without a resolved repo");
                return;
            }
        };
        let toml = session
            .editable_recipe_toml()
            .unwrap_or_default()
            .to_string();
        let allow_unsafe = session.unsafe_execution_confirmed();
        (repo, toml, allow_unsafe)
    };
    push_current_snapshot(cx);

    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();
    let session_for_bg = session_arc.clone();
    let creds = current_creds(cx);
    fe.spawn(async move {
        let outcome: Result<ImportOutput, anyhow::Error> = be
            .spawn(async move {
                let (temp_dir, recipe_path) = write_temp_recipe(&recipe_toml)?;
                let result = runner_run(&repo_url, &recipe_path, allow_unsafe);
                let _ = fs::remove_dir_all(&temp_dir);
                result
            })
            .await;

        // Snapshot the data we need to fire the /attempt POST off the
        // foreground executor. The session may have advanced since
        // we started; we only post when it lands cleanly in
        // Verified / FailedAwaitingRecipeEdit.
        let attempt_input: Option<(
            String,
            AttemptStatus,
            crate::source_import_session::ImportRun,
        )> = {
            let session_id = session_for_bg
                .lock()
                .ok()
                .and_then(|s| s.source_import_id().map(str::to_string));
            match (&outcome, creds.as_ref(), session_id) {
                (Ok(output), Some(_), Some(id)) => {
                    let status = match output.run.status.as_str() {
                        "passed" => AttemptStatus::Verified,
                        "running" if output.run.readiness_state.as_deref() == Some("ready") => {
                            AttemptStatus::Verified
                        }
                        "failed" => AttemptStatus::Failed,
                        _ => AttemptStatus::Running,
                    };
                    Some((id, status, output.run.clone()))
                }
                _ => None,
            }
        };
        let attempt_task =
            match (attempt_input, creds.as_ref()) {
                (Some((id, status, run)), Some(creds)) => {
                    let creds = creds.clone();
                    Some(be.spawn(async move {
                        ApiClient::new(creds).record_attempt(&id, status, &run)
                    }))
                }
                _ => None,
            };
        if let Some(task) = attempt_task
            && let Err(error) = task.await
        {
            tracing::warn!(?error, "ato-import: record_attempt failed");
        }

        aa.update(move |cx| {
            match outcome {
                Ok(output) => {
                    if let Ok(mut session) = session_for_bg.lock()
                        && let Err(error) = session.apply_run_result(output)
                    {
                        tracing::warn!(?error, "ato-import: apply_run_result failed");
                    }
                }
                Err(error) => {
                    tracing::warn!(?error, "ato-import: run failed before CLI completion");
                    // Push a synthetic failure into the session so the
                    // UI shows the user something rather than a stuck
                    // "Running…" spinner.
                    if let Ok(mut session) = session_for_bg.lock() {
                        let synthetic = ImportOutput {
                            source: session
                                .snapshot()
                                .source
                                .clone()
                                .unwrap_or_else(empty_source_for_failure),
                            recipe: session
                                .snapshot()
                                .recipe
                                .clone()
                                .unwrap_or_else(empty_recipe_for_failure),
                            run: crate::source_import_session::ImportRun {
                                status: "failed".to_string(),
                                phase: Some("install".to_string()),
                                error_class: Some("desktop_runner_error".to_string()),
                                error_excerpt: Some(format!("{error:#}")),
                                command_mode: None,
                                requires_host_shell: None,
                                shell_kind: None,
                                cleanup_status: None,
                                cleanup_error: None,
                                log_path: None,
                                run_session_id: None,
                                pid: None,
                                process_group_ids: Vec::new(),
                                primary_port: None,
                                primary_url: None,
                                shadow_dir: None,
                                readiness_state: None,
                                cleanup_policy: None,
                            },
                            recipe_resolution: None,
                        };
                        if session.state() != GitHubImportSessionState::Running {
                            // Session was reset / advanced concurrently.
                            // Drop the synthetic result.
                        } else {
                            let _ = session.apply_run_result(synthetic);
                        }
                    }
                }
            }
            push_current_snapshot(cx);
        });
    })
    .detach();
}

fn handle_submit_intent(cx: &mut App) {
    let session_arc = session_arc(cx);
    let (creds, source_import_id, payload, recipe_toml) = {
        let session = match session_arc.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let Some(creds) = current_creds(cx) else {
            tracing::info!(
                "ato-import: submit_intent ignored (not signed in — UI should gate this)"
            );
            return;
        };
        let Some(id) = session.source_import_id().map(str::to_string) else {
            tracing::warn!(
                "ato-import: submit_intent ignored (no source_import_id — session out of sync)"
            );
            return;
        };
        // `submit_payload` only returns Some when state==Verified, which is
        // exactly when we want to allow submit; if it returns None, the
        // session moved out of Verified between UI dispatch and our lock
        // (e.g. begin_resolve re-fired).
        let Some(payload) = session.submit_payload() else {
            tracing::warn!(
                "ato-import: submit_intent ignored (no submit payload — session out of Verified)"
            );
            return;
        };
        let toml = session
            .editable_recipe_toml()
            .map(str::to_string)
            .unwrap_or_default();
        (creds, id, payload, toml)
    };

    let async_app = cx.to_async();
    let fe = async_app.foreground_executor().clone();
    let be = async_app.background_executor().clone();
    let aa = async_app.clone();
    let session_for_bg = session_arc.clone();
    fe.spawn(async move {
        let result = be
            .spawn(async move {
                ApiClient::new(creds).submit_working_recipe(
                    &source_import_id,
                    &payload,
                    &recipe_toml,
                )
            })
            .await;
        aa.update(move |cx| {
            match result {
                Ok(()) => {
                    let run_session_id = session_for_bg
                        .lock()
                        .ok()
                        .and_then(|session| session.active_run_session_id().map(str::to_string));
                    if let Ok(mut session) = session_for_bg.lock()
                        && let Err(error) = session.mark_submitted()
                    {
                        tracing::warn!(?error, "ato-import: mark_submitted rejected");
                    }
                    if let Some(run_session_id) = run_session_id {
                        stop_import_preview_in_background(
                            be.clone(),
                            run_session_id,
                            "submit_complete",
                        );
                    }
                }
                Err(error) => {
                    if let Ok(mut session) = session_for_bg.lock() {
                        session.set_submit_error(format!("{error:#}"));
                    }
                }
            }
            push_current_snapshot(cx);
        });
    })
    .detach();
}

pub(crate) fn stop_active_import_preview(cx: &mut App, reason: &'static str) {
    let run_session_id = active_import_preview_session_id(cx);
    let Some(run_session_id) = run_session_id else {
        return;
    };
    let be = cx.to_async().background_executor().clone();
    stop_import_preview_in_background(be, run_session_id, reason);
}

pub(crate) fn stop_active_import_preview_blocking(cx: &mut App, reason: &'static str) {
    let Some(run_session_id) = active_import_preview_session_id(cx) else {
        return;
    };
    match runner_stop_import_preview(&run_session_id) {
        Ok(()) => {
            tracing::info!(%run_session_id, reason, "ato-import: stopped preview session");
        }
        Err(error) => {
            tracing::warn!(
                ?error,
                %run_session_id,
                reason,
                "ato-import: failed to stop preview session"
            );
        }
    }
}

fn active_import_preview_session_id(cx: &mut App) -> Option<String> {
    let session_arc = session_arc(cx);
    session_arc
        .lock()
        .ok()
        .and_then(|session| session.active_run_session_id().map(str::to_string))
}

fn stop_import_preview_in_background(
    be: BackgroundExecutor,
    run_session_id: String,
    reason: &'static str,
) {
    be.spawn(async move {
        match runner_stop_import_preview(&run_session_id) {
            Ok(()) => {
                tracing::info!(%run_session_id, reason, "ato-import: stopped preview session");
            }
            Err(error) => {
                tracing::warn!(
                    ?error,
                    %run_session_id,
                    reason,
                    "ato-import: failed to stop preview session"
                );
            }
        }
    })
    .detach();
}

fn write_temp_recipe(toml: &str) -> anyhow::Result<(PathBuf, PathBuf)> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let dir =
        ato_path_or_workspace_tmp(format!("tmp/desktop/import-recipes/ato-import-{pid}-{ts}"));
    fs::create_dir_all(&dir)?;
    let path = dir.join("recipe.toml");
    fs::write(&path, toml)?;
    Ok((dir, path))
}

fn empty_source_for_failure() -> crate::source_import_session::ImportSource {
    crate::source_import_session::ImportSource {
        source_url_normalized: String::new(),
        source_host: String::new(),
        repo_namespace: String::new(),
        repo_name: String::new(),
        revision_id: String::new(),
        source_tree_hash: String::new(),
        subdir: ".".to_string(),
    }
}

fn empty_recipe_for_failure() -> crate::source_import_session::ImportRecipe {
    crate::source_import_session::ImportRecipe {
        origin: "unknown".to_string(),
        target_label: None,
        platform_os: String::new(),
        platform_arch: String::new(),
        recipe_toml: String::new(),
        recipe_hash: String::new(),
    }
}

#[cfg(test)]
mod community_command_tests {
    use super::*;

    #[test]
    fn launch_community_toml_envelope_parses() {
        // Posted by community.html when the user picks a published recipe.
        let cmd: ImportCommand = serde_json::from_str(
            r#"{"kind":"launch_community_toml","handle":"github.com/excalidraw/excalidraw","ctoml_id":"ctoml_01ksza4np2yrs1mqe7jz10ep1g","label":"Excalidraw"}"#,
        )
        .expect("launch_community_toml must parse");
        match cmd {
            ImportCommand::LaunchCommunityToml {
                handle,
                ctoml_id,
                label,
            } => {
                assert_eq!(handle, "github.com/excalidraw/excalidraw");
                assert_eq!(ctoml_id, "ctoml_01ksza4np2yrs1mqe7jz10ep1g");
                assert_eq!(label, "Excalidraw");
            }
            other => panic!("expected LaunchCommunityToml, got {other:?}"),
        }
    }

    #[test]
    fn view_community_toml_envelope_parses() {
        // Posted by community.html when the user clicks "View recipe".
        let cmd: ImportCommand = serde_json::from_str(
            r#"{"kind":"view_community_toml","ctoml_id":"ctoml_01ksza4np2yrs1mqe7jz10ep1g"}"#,
        )
        .expect("view_community_toml must parse");
        match cmd {
            ImportCommand::ViewCommunityToml { ctoml_id } => {
                assert_eq!(ctoml_id, "ctoml_01ksza4np2yrs1mqe7jz10ep1g");
            }
            other => panic!("expected ViewCommunityToml, got {other:?}"),
        }
        assert_eq!(
            ImportCommand::ViewCommunityToml {
                ctoml_id: String::new(),
            }
            .required_capability(),
            Capability::WebviewCreate
        );
    }

    #[test]
    fn retry_community_envelope_parses() {
        let cmd: ImportCommand = serde_json::from_str(
            r#"{"kind":"retry_community","source":"github.com/excalidraw/excalidraw","label":"Excalidraw"}"#,
        )
        .expect("retry_community must parse");
        match cmd {
            ImportCommand::RetryCommunity { source, label } => {
                assert_eq!(source, "github.com/excalidraw/excalidraw");
                assert_eq!(label, "Excalidraw");
            }
            other => panic!("expected RetryCommunity, got {other:?}"),
        }
        assert_eq!(
            ImportCommand::RetryCommunity {
                source: String::new(),
                label: String::new(),
            }
            .required_capability(),
            Capability::WebviewCreate
        );
    }

    #[test]
    fn import_from_github_source_envelope_parses() {
        let cmd: ImportCommand = serde_json::from_str(
            r#"{"kind":"import_from_github_source","source":"github.com/foo/bar"}"#,
        )
        .expect("import_from_github_source must parse");
        match cmd {
            ImportCommand::ImportFromGithubSource { source } => {
                assert_eq!(source, "github.com/foo/bar");
            }
            other => panic!("expected ImportFromGithubSource, got {other:?}"),
        }
    }

    #[test]
    fn community_actions_require_webview_create() {
        assert_eq!(
            ImportCommand::LaunchCommunityToml {
                handle: String::new(),
                ctoml_id: String::new(),
                label: String::new(),
            }
            .required_capability(),
            Capability::WebviewCreate
        );
        assert_eq!(
            ImportCommand::ImportFromGithubSource {
                source: String::new(),
            }
            .required_capability(),
            Capability::WebviewCreate
        );
    }
}
