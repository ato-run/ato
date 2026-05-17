//! `ato-import` system capsule — GitHub Import review surface.
//!
//! Hosts the typed IPC commands posted by the import review HTML
//! (`assets/system/ato-import/index.html`). All long-running work
//! (subprocess + git clone + run) happens on the background executor;
//! the dispatch handler returns immediately after kicking off the
//! background task and pushing the transient "running" snapshot to
//! the UI.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::{AnyWindowHandle, App};
use serde::Deserialize;

use crate::source_import_api::{discover as discover_api_creds, ApiClient, ApiCreds, AttemptStatus};
use crate::source_import_runner::{infer as runner_infer, run_with_recipe as runner_run};
use crate::source_import_session::{GitHubImportSessionState, ImportOutput};
use crate::system_capsule::broker::{BrokerError, Capability};
use crate::window::import_window::{push_current_snapshot, session_arc, ImportApiCreds};

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
        ImportCommand::Close => {
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
    }
    Ok(())
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
                Some(be.spawn(async move {
                    ApiClient::new(creds).create_source_import(&source)
                }))
            }
            _ => None,
        };
        let create_id: Option<Result<String, anyhow::Error>> = match create_id_task {
            Some(task) => Some(task.await),
            None => None,
        };

        let _ = aa.update(move |cx| {
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
                                    tracing::warn!(?error, "ato-import: create_source_import failed");
                                }
                            }
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(?error, "ato-import: inference failed");
                    // Leave session in InferringRecipe; UI shows the
                    // status line. A later PR will surface this as a
                    // dedicated error card.
                    if let Ok(mut session) = session_for_bg.lock() {
                        session.set_signed_in(creds.is_some());
                    }
                }
            }
            push_current_snapshot(cx);
        });
    })
    .detach();
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
            return;
        }
    }
    // Editing is high-frequency (keystrokes); skip pushing snapshot
    // back to avoid re-rendering the textarea under the user's cursor.
}

fn handle_run(cx: &mut App) {
    let session_arc = session_arc(cx);
    let (repo_url, recipe_toml) = {
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
        let toml = session.editable_recipe_toml().unwrap_or_default().to_string();
        (repo, toml)
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
                let recipe_path = write_temp_recipe(&recipe_toml)?;
                let result = runner_run(&repo_url, &recipe_path);
                // Best-effort cleanup; ignore errors.
                let _ = fs::remove_file(&recipe_path);
                result
            })
            .await;

        // Snapshot the data we need to fire the /attempt POST off the
        // foreground executor. The session may have advanced since
        // we started; we only post when it lands cleanly in
        // Verified / FailedAwaitingRecipeEdit.
        let attempt_input: Option<(String, AttemptStatus, crate::source_import_session::ImportRun)> = {
            let session_id = session_for_bg
                .lock()
                .ok()
                .and_then(|s| s.source_import_id().map(str::to_string));
            match (&outcome, creds.as_ref(), session_id) {
                (Ok(output), Some(_), Some(id)) => {
                    let status = match output.run.status.as_str() {
                        "passed" => AttemptStatus::Verified,
                        "failed" => AttemptStatus::Failed,
                        _ => AttemptStatus::Running,
                    };
                    Some((id, status, output.run.clone()))
                }
                _ => None,
            }
        };
        let attempt_task = match (attempt_input, creds.as_ref()) {
            (Some((id, status, run)), Some(creds)) => {
                let creds = creds.clone();
                Some(be.spawn(async move {
                    ApiClient::new(creds).record_attempt(&id, status, &run)
                }))
            }
            _ => None,
        };
        if let Some(task) = attempt_task {
            if let Err(error) = task.await {
                tracing::warn!(?error, "ato-import: record_attempt failed");
            }
        }

        let _ = aa.update(move |cx| {
            match outcome {
                Ok(output) => {
                    if let Ok(mut session) = session_for_bg.lock() {
                        if let Err(error) = session.apply_run_result(output) {
                            tracing::warn!(?error, "ato-import: apply_run_result failed");
                        }
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
                            },
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
    let (creds, source_import_id, recipe, recipe_toml) = {
        let session = match session_arc.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let snap = session.snapshot();
        let Some(creds) = current_creds(cx) else {
            tracing::info!(
                "ato-import: submit_intent ignored (not signed in — UI should gate this)"
            );
            return;
        };
        let Some(id) = snap.source_import_id.clone() else {
            tracing::warn!(
                "ato-import: submit_intent ignored (no source_import_id — session out of sync)"
            );
            return;
        };
        let Some(recipe) = snap.recipe.clone() else {
            tracing::warn!("ato-import: submit_intent ignored (no recipe in snapshot)");
            return;
        };
        let toml = snap.editable_recipe_toml.unwrap_or_default();
        (creds, id, recipe, toml)
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
                    &recipe,
                    &recipe_toml,
                )
            })
            .await;
        let _ = aa.update(move |cx| {
            match result {
                Ok(()) => {
                    if let Ok(mut session) = session_for_bg.lock() {
                        if let Err(error) = session.mark_submitted() {
                            tracing::warn!(?error, "ato-import: mark_submitted rejected");
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(?error, "ato-import: submit_working_recipe failed");
                    // Stay in Verified; the UI surfaces this in a
                    // follow-up PR. For now the user can retry by
                    // clicking Submit again.
                }
            }
            push_current_snapshot(cx);
        });
    })
    .detach();
}

fn write_temp_recipe(toml: &str) -> anyhow::Result<PathBuf> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let dir = env::temp_dir().join(format!("ato-import-{pid}-{ts}"));
    fs::create_dir_all(&dir)?;
    let path = dir.join("recipe.toml");
    fs::write(&path, toml)?;
    Ok(path)
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
