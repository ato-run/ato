//! The MCP tools submit narrowly scoped operations to the contained verifier.
//! The verifier rechecks the same document and DOM identity before acting.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Phase, Store, read_limited};
use crate::run_data;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Operation {
    preparation_id: String,
    session_id: String,
    observation_id: String,
    action: Action,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Click { element: u32 },
    Type { element: u32, text: String },
    Navigate { path: String },
    Reload,
    Finish,
}

pub(super) fn observe(store: &Store, id: &str) -> Result<Value> {
    let (_, observation) = active_observation(store, id)?;
    Ok(json!({"preparation_id":id, "observation":observation,
        "provenance":"untrusted_application_observation"}))
}

pub(super) fn act(store: &Store, operation: Operation) -> Result<Value> {
    let id = &operation.preparation_id;
    let _lock = store.edit(id)?;
    let (directory, observation) = active_observation(store, id)?;
    ensure!(
        observation["session_id"] == operation.session_id
            && observation["observation_id"] == operation.observation_id,
        "Stale browser observation. Request publication_browser_observe again"
    );
    if let Action::Click { element } | Action::Type { element, .. } = &operation.action {
        ensure!(
            observation["elements"]
                .as_array()
                .is_some_and(|list| list.iter().any(|item| {
                    item["id"].as_u64() == Some(u64::from(*element))
                        && (matches!(operation.action, Action::Click { .. })
                            || item["can_type"] == true)
                })),
            "The selected element cannot perform that operation. Observe again"
        );
    }
    if let Action::Type { text, .. } = &operation.action {
        ensure!(
            !text.is_empty() && text.len() <= 512 && !text.chars().any(char::is_control),
            "Supply a short synthetic test value without control characters"
        );
    }
    if let Action::Navigate { path } = &operation.action {
        ensure!(
            path.starts_with('/')
                && !path.starts_with("//")
                && path.len() <= 512
                && !path.contains(['?', '#', '\\'])
                && !path.chars().any(char::is_control),
            "Navigation must use an application path without query or fragment"
        );
    }
    let command = directory.join("command.json");
    ensure!(
        !command.try_exists()?,
        "A browser action is pending. Observe again after it completes"
    );
    let request_id = run_data::token()?;
    run_data::write_json(
        &command,
        &json!({
            "request_id":request_id, "session_id":operation.session_id,
            "observation_id":operation.observation_id, "action":operation.action
        }),
    )?;
    Ok(json!({"request_id":request_id,"status":"submitted","next":"publication_browser_observe"}))
}

fn active_observation(store: &Store, id: &str) -> Result<(PathBuf, Value)> {
    let entry = store.read(id)?;
    ensure!(
        entry.phase == Phase::Verifying && entry.plan.browser_interactive && !store.job_idle(id)?,
        "No live browser session for this preparation. Check publication_status"
    );
    let directory = store.directory(id)?.join("browser");
    let mut active = None;
    for session in children(&directory)?.into_iter().flatten() {
        let session = session?;
        if !session.file_type()?.is_dir() {
            continue;
        }
        let session_id = session.file_name();
        let location = session.path().join("location.json");
        let Some(scratch): Option<PathBuf> = run_data::read_optional(&location)? else {
            continue;
        };
        let parent = std::env::temp_dir().join("ato-browser-verify");
        ensure!(
            scratch.parent() == Some(parent.as_path())
                && scratch
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("v-")),
            "Invalid browser scratch location"
        );
        if fs::symlink_metadata(&scratch).is_ok_and(|meta| meta.is_dir()) {
            let mailbox = scratch.join("operator");
            let path = mailbox.join("observation.json");
            if !fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file()) {
                continue;
            }
            let file = match fs::File::open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let observation: Value = serde_json::from_slice(&read_limited(file, 64 * 1024)?)?;
            ensure!(
                Some(observation["session_id"].as_str().unwrap_or("")) == session_id.to_str()
                    && observation["observation_id"].is_string()
                    && observation["elements"].is_array(),
                "Invalid browser observation"
            );
            if active.replace((mailbox, observation)).is_some() {
                bail!("More than one browser session is visible. Check publication_status");
            }
        }
    }
    active.context("The browser has not published a live observation. Check publication_status")
}

fn children(path: &Path) -> Result<Option<fs::ReadDir>> {
    match fs::read_dir(path) {
        Ok(children) => Ok(Some(children)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
