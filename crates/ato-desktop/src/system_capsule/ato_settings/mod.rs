//! `ato-settings` system capsule — Settings UI.
//!
//! Provides real IPC handlers for the settings window. Commands:
//! - `LoadSnapshot` — serialise the current config and push it to JS
//! - `PatchGlobalSettings` — typed config mutation via `patch_config_for_capsule`
//! - `RunGlobalAction` — reserved for future side-effect actions
//! - `NavigateTab` — client-side navigation hint (no server state)
//! - `Close` — close the host window

use gpui::{AnyWindowHandle, App};
use serde::Deserialize;
use serde_json::Value;

use crate::config::{load_config, save_config};
use crate::settings::{patch_config_for_capsule, settings_snapshot_from_config};
use crate::system_capsule::broker::{BrokerError, Capability};
use crate::window::settings_window::ActiveSettingsShell;

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SettingsCommand {
    /// Reload the current config from disk and push the full snapshot to JS.
    LoadSnapshot {
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Apply a typed patch to the config file.
    PatchGlobalSettings {
        #[serde(default)]
        request_id: Option<String>,
        patch: Value,
    },
    /// Reserved for future side-effect actions (e.g. clear_cache, sign_out).
    RunGlobalAction {
        #[serde(default)]
        request_id: Option<String>,
        action: String,
    },
    /// Navigate to a named tab — handled entirely in JS; Rust just logs.
    NavigateTab { tab: String },
    /// Close the settings window.
    Close,
}

impl SettingsCommand {
    pub fn required_capability(&self) -> Capability {
        match self {
            SettingsCommand::Close => Capability::WindowsClose,
            SettingsCommand::NavigateTab { .. } => Capability::SettingsRead,
            SettingsCommand::LoadSnapshot { .. } => Capability::SettingsRead,
            SettingsCommand::PatchGlobalSettings { .. } => Capability::SettingsWrite,
            SettingsCommand::RunGlobalAction { .. } => Capability::SettingsWrite,
        }
    }
}

pub fn dispatch(
    cx: &mut App,
    host: AnyWindowHandle,
    command: SettingsCommand,
) -> Result<(), BrokerError> {
    match command {
        SettingsCommand::Close => {
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
        SettingsCommand::NavigateTab { tab } => {
            tracing::debug!(?tab, "ato_settings: NavigateTab");
        }
        SettingsCommand::LoadSnapshot { request_id } => {
            let config = load_config();
            let snap = settings_snapshot_from_config(&config);
            let response = serde_json::json!({
                "ok": true,
                "requestId": request_id,
                "snapshot": snap,
            });
            push_to_settings_webview(cx, &response.to_string());
        }
        SettingsCommand::PatchGlobalSettings { request_id, patch } => {
            let mut config = load_config();
            let patch_resp = patch_config_for_capsule(&mut config, &patch, request_id.as_deref());
            save_config(&config);
            let snap = settings_snapshot_from_config(&config);
            let mut response = patch_resp;
            response["snapshot"] = snap;
            push_to_settings_webview(cx, &response.to_string());
        }
        SettingsCommand::RunGlobalAction { request_id, action } => {
            tracing::info!(?action, "ato_settings: RunGlobalAction (stub)");
            let response = serde_json::json!({
                "ok": false,
                "requestId": request_id,
                "error": format!("action '{}' is not implemented", action),
            });
            push_to_settings_webview(cx, &response.to_string());
        }
    }
    Ok(())
}

/// Deliver `payload_json` to the currently open settings window via
/// `window.__ATO_SETTINGS_HYDRATE__`.
fn push_to_settings_webview(cx: &mut App, payload_json: &str) {
    let weak = cx
        .try_global::<ActiveSettingsShell>()
        .and_then(|g| g.0.clone());
    let Some(weak) = weak else {
        return;
    };
    let Some(entity) = weak.upgrade() else {
        return;
    };
    let payload = payload_json.to_string();
    entity.update(cx, |shell, _cx| {
        shell.hydrate(&payload);
    });
}

