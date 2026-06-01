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

use crate::config::{SecretStore, load_config, load_secrets, save_config};
use crate::settings::{
    patch_config_for_capsule, secrets_snapshot_from_store, settings_snapshot_from_config,
};
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
    /// Probe the selected OCI engine and return live diagnostic data to the UI.
    LoadEnginesDiagnostics {
        #[serde(default)]
        request_id: Option<String>,
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
            Self::PutSecret {
                request_id, key, ..
            } => {
                write!(
                    f,
                    "PutSecret {{ request_id: {:?}, key: {:?}, value: [REDACTED] }}",
                    request_id, key
                )
            }
            Self::DeleteSecret { request_id, key } => {
                write!(
                    f,
                    "DeleteSecret {{ request_id: {:?}, key: {:?} }}",
                    request_id, key
                )
            }
            Self::GrantSecret {
                request_id,
                handle,
                key,
            } => {
                write!(
                    f,
                    "GrantSecret {{ request_id: {:?}, handle: {:?}, key: {:?} }}",
                    request_id, handle, key
                )
            }
            Self::RevokeSecret {
                request_id,
                handle,
                key,
            } => {
                write!(
                    f,
                    "RevokeSecret {{ request_id: {:?}, handle: {:?}, key: {:?} }}",
                    request_id, handle, key
                )
            }
            Self::LoadEnginesDiagnostics { .. } => write!(f, "LoadEnginesDiagnostics"),
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
            SettingsCommand::LoadEnginesDiagnostics { .. } => Capability::SettingsRead,
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
            if patch.get("controlBarMode").is_some()
                && let Err(err) =
                    crate::window::set_control_bar_mode(cx, config.desktop.control_bar.mode)
            {
                tracing::error!(error = %err, "ato_settings: applying Control Bar mode failed");
            }
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
        SettingsCommand::PutSecret {
            request_id,
            key,
            value,
        } => {
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
            if let Err(e) = store.add_secret(trimmed_key, trimmed_value) {
                tracing::error!(error = %e, "ato_settings: PutSecret failed");
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
            if let Err(e) = store.remove_secret(&trimmed_key) {
                tracing::error!(error = %e, "ato_settings: DeleteSecret failed");
                push_secrets_error(cx, request_id.as_deref(), &format!("save failed: {e}"));
                return Ok(());
            }
            push_secrets_ok(cx, request_id.as_deref(), &store);
        }
        SettingsCommand::GrantSecret {
            request_id,
            handle,
            key,
        } => {
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
            if let Err(e) = store.grant_secret(&trimmed_handle, &trimmed_key) {
                tracing::error!(error = %e, "ato_settings: GrantSecret failed");
                push_secrets_error(cx, request_id.as_deref(), &format!("save failed: {e}"));
                return Ok(());
            }
            push_secrets_ok(cx, request_id.as_deref(), &store);
        }
        SettingsCommand::RevokeSecret {
            request_id,
            handle,
            key,
        } => {
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
            if let Err(e) = store.revoke_secret(&trimmed_handle, &trimmed_key) {
                tracing::error!(error = %e, "ato_settings: RevokeSecret failed");
                push_secrets_error(cx, request_id.as_deref(), &format!("save failed: {e}"));
                return Ok(());
            }
            push_secrets_ok(cx, request_id.as_deref(), &store);
        }
        SettingsCommand::LoadEnginesDiagnostics { request_id } => {
            let diag = collect_podman_diagnostics();
            let response = serde_json::json!({
                "ok": true,
                "requestId": request_id,
                "engineDiagnostics": { "podman": diag },
            });
            push_to_settings_webview(cx, &response.to_string());
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

/// Probe the local Podman installation and return structured diagnostics.
///
/// On macOS/Windows this checks the Podman machine state.
/// On Linux, `podman machine` is not needed — native Podman is reported as ready.
fn collect_podman_diagnostics() -> serde_json::Value {
    use crate::proc_util::CommandNoWindowExt;
    use std::process::Command;

    let binary_found = Command::new("podman")
        .no_console_window()
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !binary_found {
        return serde_json::json!({
            "binary": "missing",
            "machine": "unknown",
            "guidance": "Install Podman to use OCI capsules.",
        });
    }

    // On Linux, native Podman does not use machine management.
    if std::env::consts::OS == "linux" {
        return serde_json::json!({
            "binary": "found",
            "machine": "native",
            "guidance": null,
        });
    }

    // macOS / Windows — inspect the machine list.
    let list_result = Command::new("podman")
        .no_console_window()
        .args(["machine", "list", "--format", "json"])
        .output();

    let output = match list_result {
        Err(e) => {
            return serde_json::json!({
                "binary": "found",
                "machine": "unknown",
                "guidance": format!("Could not query Podman machine state: {e}"),
            });
        }
        Ok(o) if !o.status.success() => {
            let msg = String::from_utf8_lossy(&o.stderr).trim().to_string();
            return serde_json::json!({
                "binary": "found",
                "machine": "unknown",
                "guidance": format!("podman machine list failed: {msg}"),
            });
        }
        Ok(o) => o,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    match serde_json::from_str::<serde_json::Value>(stdout.trim()) {
        Err(_) => serde_json::json!({
            "binary": "found",
            "machine": "unknown",
            "guidance": "Podman machine list output was not recognized.",
        }),
        Ok(serde_json::Value::Array(machines)) if machines.is_empty() => serde_json::json!({
            "binary": "found",
            "machine": "not_configured",
            "guidance": "No Podman machine found. Run: podman machine init && podman machine start",
        }),
        Ok(serde_json::Value::Array(machines)) => {
            podman_diagnostics_from_machine_entries(&machines)
        }
        Ok(_) => serde_json::json!({
            "binary": "found",
            "machine": "unknown",
            "guidance": "Podman machine list returned an unexpected format.",
        }),
    }
}

fn podman_diagnostics_from_machine_entries(machines: &[serde_json::Value]) -> serde_json::Value {
    let running: Vec<&str> = machines
        .iter()
        .filter(|m| m.get("Running").and_then(|v| v.as_bool()).unwrap_or(false))
        .filter_map(|m| m.get("Name").and_then(|v| v.as_str()))
        .collect();
    let names: Vec<&str> = machines
        .iter()
        .filter_map(|m| m.get("Name").and_then(|v| v.as_str()))
        .collect();

    if names.len() > 1 {
        serde_json::json!({
            "binary": "found",
            "machine": "ambiguous",
            "machineNames": names,
            "guidance": "Multiple Podman machines found. Choose one or clean up: podman machine rm",
        })
    } else if running.len() == 1 {
        serde_json::json!({
            "binary": "found",
            "machine": "running",
            "machineNames": running,
            "guidance": null,
        })
    } else {
        serde_json::json!({
            "binary": "found",
            "machine": "stopped",
            "machineNames": names,
            "guidance": "Podman machine is stopped. Ato can auto-start it on next OCI launch.",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::podman_diagnostics_from_machine_entries;
    use serde_json::json;

    #[test]
    fn diagnostics_one_running_one_stopped_reports_ambiguous() {
        let machines = vec![
            json!({"Name": "machine-a", "Running": false}),
            json!({"Name": "machine-b", "Running": true}),
        ];
        let diagnostics = podman_diagnostics_from_machine_entries(&machines);
        assert_eq!(diagnostics["machine"], "ambiguous");
        assert_eq!(diagnostics["machineNames"][0], "machine-a");
        assert_eq!(diagnostics["machineNames"][1], "machine-b");
    }
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
