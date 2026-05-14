//! `ato-settings` system capsule — Settings UI.
//!
//! Provides real IPC handlers for the settings window. Commands:
//! - `LoadSnapshot` — serialise the current config and push it to JS
//! - `PatchGlobalSettings` — typed config mutation via `patch_config_for_capsule`
//! - `RunGlobalAction` — reserved for future side-effect actions
//! - `NavigateTab` — client-side navigation hint (no server state)
//! - `Close` — close the host window
//! - `LoadSecretsSnapshot` — return secrets metadata (no values) to JS
//! - `PutSecret` — add or overwrite a secret
//! - `DeleteSecret` — delete a secret and all its grants
//! - `GrantSecret` — grant a capsule handle access to a secret key
//! - `RevokeSecret` — revoke a capsule handle's access to a secret key

use gpui::{AnyWindowHandle, App};
use serde::Deserialize;
use serde_json::Value;

use crate::config::{load_config, load_secrets, save_config, save_secrets, SecretStore};
use crate::settings::{patch_config_for_capsule, secrets_snapshot_from_store, settings_snapshot_from_config};
use crate::system_capsule::broker::{BrokerError, Capability};
use crate::window::settings_window::ActiveSettingsShell;

#[derive(Deserialize)]
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

    /// Return secrets metadata (no values) to the settings UI.
    LoadSecretsSnapshot {
        #[serde(default)]
        request_id: Option<String>,
    },
    /// Add or overwrite a secret.
    PutSecret {
        #[serde(default)]
        request_id: Option<String>,
        key: String,
        value: String,
    },
    /// Delete a secret and remove it from all grants.
    DeleteSecret {
        #[serde(default)]
        request_id: Option<String>,
        key: String,
    },
    /// Grant a capsule handle access to a secret key.
    GrantSecret {
        #[serde(default)]
        request_id: Option<String>,
        handle: String,
        key: String,
    },
    /// Revoke a capsule handle's access to a secret key.
    RevokeSecret {
        #[serde(default)]
        request_id: Option<String>,
        handle: String,
        key: String,
    },
}

// Custom Debug so `PutSecret.value` never appears in logs.
impl std::fmt::Debug for SettingsCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LoadSnapshot { .. } => write!(f, "LoadSnapshot"),
            Self::PatchGlobalSettings { request_id, .. } => {
                write!(f, "PatchGlobalSettings {{ request_id: {:?} }}", request_id)
            }
            Self::RunGlobalAction { action, .. } => {
                write!(f, "RunGlobalAction {{ action: {:?} }}", action)
            }
            Self::NavigateTab { tab } => write!(f, "NavigateTab {{ tab: {:?} }}", tab),
            Self::Close => write!(f, "Close"),
            Self::LoadSecretsSnapshot { .. } => write!(f, "LoadSecretsSnapshot"),
            Self::PutSecret { request_id, key, .. } => {
                write!(f, "PutSecret {{ request_id: {:?}, key: {:?}, value: [REDACTED] }}", request_id, key)
            }
            Self::DeleteSecret { request_id, key } => {
                write!(f, "DeleteSecret {{ request_id: {:?}, key: {:?} }}", request_id, key)
            }
            Self::GrantSecret { request_id, handle, key } => {
                write!(f, "GrantSecret {{ request_id: {:?}, handle: {:?}, key: {:?} }}", request_id, handle, key)
            }
            Self::RevokeSecret { request_id, handle, key } => {
                write!(f, "RevokeSecret {{ request_id: {:?}, handle: {:?}, key: {:?} }}", request_id, handle, key)
            }
        }
    }
}

impl SettingsCommand {
    pub fn required_capability(&self) -> Capability {
        match self {
            SettingsCommand::Close => Capability::WindowsClose,
            SettingsCommand::NavigateTab { .. } => Capability::SettingsRead,
            SettingsCommand::LoadSnapshot { .. } => Capability::SettingsRead,
            SettingsCommand::LoadSecretsSnapshot { .. } => Capability::SettingsRead,
            SettingsCommand::PatchGlobalSettings { .. } => Capability::SettingsWrite,
            SettingsCommand::RunGlobalAction { .. } => Capability::SettingsWrite,
            SettingsCommand::PutSecret { .. } => Capability::SettingsWrite,
            SettingsCommand::DeleteSecret { .. } => Capability::SettingsWrite,
            SettingsCommand::GrantSecret { .. } => Capability::SettingsWrite,
            SettingsCommand::RevokeSecret { .. } => Capability::SettingsWrite,
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
        SettingsCommand::LoadSecretsSnapshot { request_id } => {
            let store = load_secrets();
            let secrets = secrets_snapshot_from_store(&store);
            let response = serde_json::json!({
                "ok": true,
                "requestId": request_id,
                "secrets": secrets,
            });
            push_to_settings_webview(cx, &response.to_string());
        }
        SettingsCommand::PutSecret { request_id, key, value } => {
            let trimmed_key = key.trim().to_string();
            let trimmed_value = value.trim().to_string();
            if let Some(err) = validate_secret_key(&trimmed_key) {
                push_secrets_error(cx, request_id.as_deref(), err);
                return Ok(());
            }
            if trimmed_value.is_empty() {
                push_secrets_error(cx, request_id.as_deref(), "value must not be empty");
                return Ok(());
            }
            let mut store = load_secrets();
            store.add_secret(trimmed_key, trimmed_value);
            if let Err(e) = save_secrets(&store) {
                tracing::error!(error = %e, "ato_settings: PutSecret save failed");
                push_secrets_error(cx, request_id.as_deref(), &format!("save failed: {e}"));
                return Ok(());
            }
            push_secrets_ok(cx, request_id.as_deref(), &store);
        }
        SettingsCommand::DeleteSecret { request_id, key } => {
            let trimmed_key = key.trim().to_string();
            if let Some(err) = validate_secret_key(&trimmed_key) {
                push_secrets_error(cx, request_id.as_deref(), err);
                return Ok(());
            }
            let mut store = load_secrets();
            if !store.secrets.iter().any(|s| s.key == trimmed_key) {
                push_secrets_error(cx, request_id.as_deref(), "key not found");
                return Ok(());
            }
            store.remove_secret(&trimmed_key);
            if let Err(e) = save_secrets(&store) {
                tracing::error!(error = %e, "ato_settings: DeleteSecret save failed");
                push_secrets_error(cx, request_id.as_deref(), &format!("save failed: {e}"));
                return Ok(());
            }
            push_secrets_ok(cx, request_id.as_deref(), &store);
        }
        SettingsCommand::GrantSecret { request_id, handle, key } => {
            let trimmed_handle = handle.trim().to_string();
            let trimmed_key = key.trim().to_string();
            if trimmed_handle.is_empty() {
                push_secrets_error(cx, request_id.as_deref(), "handle must not be empty");
                return Ok(());
            }
            if let Some(err) = validate_secret_key(&trimmed_key) {
                push_secrets_error(cx, request_id.as_deref(), err);
                return Ok(());
            }
            let mut store = load_secrets();
            if !store.secrets.iter().any(|s| s.key == trimmed_key) {
                push_secrets_error(cx, request_id.as_deref(), "key not found");
                return Ok(());
            }
            store.grant_secret(&trimmed_handle, &trimmed_key);
            if let Err(e) = save_secrets(&store) {
                tracing::error!(error = %e, "ato_settings: GrantSecret save failed");
                push_secrets_error(cx, request_id.as_deref(), &format!("save failed: {e}"));
                return Ok(());
            }
            push_secrets_ok(cx, request_id.as_deref(), &store);
        }
        SettingsCommand::RevokeSecret { request_id, handle, key } => {
            let trimmed_handle = handle.trim().to_string();
            let trimmed_key = key.trim().to_string();
            if trimmed_handle.is_empty() {
                push_secrets_error(cx, request_id.as_deref(), "handle must not be empty");
                return Ok(());
            }
            if let Some(err) = validate_secret_key(&trimmed_key) {
                push_secrets_error(cx, request_id.as_deref(), err);
                return Ok(());
            }
            let mut store = load_secrets();
            store.revoke_secret(&trimmed_handle, &trimmed_key);
            if let Err(e) = save_secrets(&store) {
                tracing::error!(error = %e, "ato_settings: RevokeSecret save failed");
                push_secrets_error(cx, request_id.as_deref(), &format!("save failed: {e}"));
                return Ok(());
            }
            push_secrets_ok(cx, request_id.as_deref(), &store);
        }
    }
    Ok(())
}

fn validate_secret_key(key: &str) -> Option<&'static str> {
    if key.is_empty() {
        return Some("key must not be empty");
    }
    if key.contains('\n') || key.contains('\r') {
        return Some("key must not contain newlines");
    }
    None
}

fn push_secrets_ok(cx: &mut App, request_id: Option<&str>, store: &SecretStore) {
    let secrets = secrets_snapshot_from_store(store);
    let response = serde_json::json!({
        "ok": true,
        "requestId": request_id,
        "secrets": secrets,
    });
    push_to_settings_webview(cx, &response.to_string());
}

fn push_secrets_error(cx: &mut App, request_id: Option<&str>, message: &str) {
    let response = serde_json::json!({
        "ok": false,
        "requestId": request_id,
        "error": { "message": message },
    });
    push_to_settings_webview(cx, &response.to_string());
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

