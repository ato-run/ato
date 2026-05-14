use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

use crate::config::{
    CapsulePolicyOverride, ContentWindowPresentation, ControlBarPosition, DesktopConfig,
    EgressPolicyMode, LanguageConfig, LogLevel, SecretStore, StartupSurface, ThemeConfig,
    UpdateChannel,
};
use crate::state::{ActivityTone, AppState, GuestRoute, HostPanelRoute, PaneId, PaneSurface};
use crate::ui::share::web_favicon_origin;

#[derive(Clone, Copy)]
enum SettingSource {
    Global,
    Manifest,
    UserOverride,
    Session,
}

impl SettingSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Manifest => "manifest",
            Self::UserOverride => "user_override",
            Self::Session => "session",
        }
    }
}

#[derive(Clone, Copy)]
enum SafetyClass {
    Immediate,
    ConfirmBeforeCommit,
    ActionOnly,
}

impl SafetyClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::Immediate => "immediate",
            Self::ConfirmBeforeCommit => "confirm_before_commit",
            Self::ActionOnly => "action_only",
        }
    }
}

#[derive(Debug)]
enum SettingsError {
    Validation { field: String, message: String },
    ConfirmRequired { field: String, message: String },
    PolicyDenied { field: String, message: String },
    UnknownCommand(String),
}

impl SettingsError {
    fn to_json(&self) -> Value {
        match self {
            Self::Validation { field, message } => json!({
                "type": "validation_error",
                "field": field,
                "message": message,
            }),
            Self::ConfirmRequired { field, message } => json!({
                "type": "confirm_required",
                "field": field,
                "message": message,
            }),
            Self::PolicyDenied { field, message } => json!({
                "type": "policy_denied",
                "field": field,
                "message": message,
            }),
            Self::UnknownCommand(command) => json!({
                "type": "unknown_command",
                "message": format!("unknown host panel settings command: {command}"),
            }),
        }
    }
}

pub fn host_panel_payload(state: &AppState, route: Option<&HostPanelRoute>) -> Value {
    let capsule_settings = match route {
        Some(HostPanelRoute::CapsuleDetail { pane_id, .. }) => {
            Some(capsule_snapshot(state, Some(*pane_id)))
        }
        _ => None,
    };

    json!({
        "revision": state.host_panel_payload_revision,
        "globalSettings": global_settings_snapshot(state),
        "capsuleSettings": capsule_settings,
        "launcherData": launcher_snapshot(state),
        "commandResponse": state.host_panel_last_response,
    })
}

pub fn host_panel_payload_for_url(state: &AppState, url: &str) -> Value {
    let pane_id = url
        .split("/capsule/")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .and_then(|value| value.parse::<usize>().ok());

    let capsule_settings = pane_id.map(|id| capsule_snapshot(state, Some(id)));

    json!({
        "revision": state.host_panel_payload_revision,
        "globalSettings": global_settings_snapshot(state),
        "capsuleSettings": capsule_settings,
        "launcherData": launcher_snapshot(state),
        "commandResponse": state.host_panel_last_response,
    })
}

pub fn handle_host_panel_command(
    state: &mut AppState,
    pane_id: PaneId,
    command: &str,
    payload: Value,
    request_id: Option<String>,
) -> Value {
    let response = match command {
        "load-global-settings-snapshot" => Ok(json!({
            "snapshot": global_settings_snapshot(state),
        })),
        "patch-global-settings" => patch_global_settings(state, payload),
        "run-global-settings-action" => run_global_settings_action(state, payload),
        "load-capsule-snapshot" => Ok(json!({
            "snapshot": capsule_snapshot(state, payload.get("paneId").and_then(Value::as_u64).map(|v| v as usize).or(Some(pane_id))),
        })),
        "patch-capsule-policy" => patch_capsule_policy(state, pane_id, payload),
        "run-capsule-action" => run_capsule_action(state, pane_id, payload),
        _ => Err(SettingsError::UnknownCommand(command.to_string())),
    };

    match response {
        Ok(payload) => json!({
            "ok": true,
            "requestId": request_id,
            "command": command,
            "payload": payload,
        }),
        Err(error) => json!({
            "ok": false,
            "requestId": request_id,
            "command": command,
            "error": error.to_json(),
        }),
    }
}

fn launcher_snapshot(state: &AppState) -> Value {
    let mut open_capsules: Vec<Value> = Vec::new();

    for workspace in &state.workspaces {
        for task in &workspace.tasks {
            for pane in &task.panes {
                let entry = match &pane.surface {
                    PaneSurface::Web(web)
                        if matches!(
                            web.route,
                            GuestRoute::CapsuleHandle { .. }
                                | GuestRoute::Capsule { .. }
                                | GuestRoute::CapsuleUrl { .. }
                        ) =>
                    {
                        let handle = web
                            .canonical_handle
                            .clone()
                            .unwrap_or_else(|| web.route.to_string());
                        let log_count = state
                            .capsule_logs
                            .get(&pane.id)
                            .map(|l| l.len())
                            .unwrap_or(0);
                        Some(json!({
                            "paneId": pane.id,
                            "title": pane.title,
                            "handle": handle,
                            "sessionLabel": format!("{:?}", web.session),
                            "runtimeLabel": web.runtime_label,
                            "logCount": log_count,
                        }))
                    }
                    PaneSurface::CapsuleStatus(capsule) => {
                        let handle = capsule
                            .canonical_handle
                            .clone()
                            .unwrap_or_else(|| capsule.route.to_string());
                        let log_count = state
                            .capsule_logs
                            .get(&pane.id)
                            .map(|l| l.len())
                            .unwrap_or(0);
                        Some(json!({
                            "paneId": pane.id,
                            "title": pane.title,
                            "handle": handle,
                            "sessionLabel": format!("{:?}", capsule.session),
                            "runtimeLabel": capsule.runtime_label,
                            "logCount": log_count,
                        }))
                    }
                    _ => None,
                };
                if let Some(entry) = entry {
                    open_capsules.push(entry);
                }
            }
        }
    }

    json!({
        "openCapsules": open_capsules,
        "authStatus": format!("{:?}", state.desktop_auth.status),
        "publisherHandle": state.desktop_auth.publisher_handle,
    })
}

fn global_settings_snapshot(state: &AppState) -> Value {
    let config = &state.config;
    let auth = &state.desktop_auth;
    let cache_path = normalize_path_for_display(&config.runtime.cache_location);

    let mut snap = settings_snapshot_from_config(config);
    // Augment with runtime state that is only available through AppState.
    snap["runtime"] = json!({
        "auth": {
            "status": format!("{:?}", auth.status),
            "publisherHandle": auth.publisher_handle,
            "lastLoginOrigin": auth.last_login_origin,
            "secretValuesExposed": false,
        },
        "cache": {
            "path": cache_path,
            "usageBytes": null,
            "reclaimableBytes": null,
        },
        "nacelle": {
            "required": config.sandbox.require_nacelle,
            "status": "unknown",
        },
        "tailnet": {
            "enabled": config.sandbox.tailnet_sidecar,
            "status": "unknown",
        },
        "hostBridge": {
            "status": "local",
        },
    });
    snap["diagnostics"] = json!(diagnostics_for_global(state));
    snap
}

/// Build a settings snapshot from config alone — used by the
/// `ato-settings` capsule dispatch which does not have access to AppState.
pub fn settings_snapshot_from_config(config: &DesktopConfig) -> Value {
    let cache_path = normalize_path_for_display(&config.runtime.cache_location);
    let workspace_path = normalize_path_for_display(&config.runtime.workspace_root);

    json!({
        "declaration": config,
        "resolved": {
            "general": {
                "theme": setting(config.general.theme, SettingSource::Global, false, None, SafetyClass::Immediate),
                "language": setting(config.general.language, SettingSource::Global, false, None, SafetyClass::Immediate),
                "launchAtLogin": setting(config.general.launch_at_login, SettingSource::Global, false, None, SafetyClass::Immediate),
                "showInTray": setting(config.general.show_in_tray, SettingSource::Global, false, None, SafetyClass::Immediate),
                "showWhatsNew": setting(config.general.show_whats_new, SettingSource::Global, false, None, SafetyClass::Immediate),
            },
            "updates": {
                "channel": setting(config.updates.channel, SettingSource::Global, false, None, SafetyClass::Immediate),
                "automaticUpdates": setting(config.updates.automatic_updates, SettingSource::Global, false, None, SafetyClass::Immediate),
            },
            "runtime": {
                "cacheLocation": setting(cache_path, SettingSource::Global, false, None, SafetyClass::ConfirmBeforeCommit),
                "cacheSizeLimitGb": setting(config.runtime.cache_size_limit_gb, SettingSource::Global, false, None, SafetyClass::ConfirmBeforeCommit),
                "workspaceRoot": setting(workspace_path, SettingSource::Global, false, None, SafetyClass::ConfirmBeforeCommit),
                "watchDebounceMs": setting(config.runtime.watch_debounce_ms, SettingSource::Global, false, None, SafetyClass::ConfirmBeforeCommit),
                "executionBoundary": setting(config.runtime.execution_boundary, SettingSource::Global, false, None, SafetyClass::ConfirmBeforeCommit),
                "unsafePrompt": setting(config.runtime.unsafe_prompt, SettingSource::Global, false, None, SafetyClass::ConfirmBeforeCommit),
                "allowUnsafeEnv": setting(config.runtime.allow_unsafe_env, SettingSource::Global, false, None, SafetyClass::ConfirmBeforeCommit),
            },
            "sandbox": {
                "requireNacelle": setting(config.sandbox.require_nacelle, SettingSource::Global, false, None, SafetyClass::ConfirmBeforeCommit),
                "defaultEgressPolicy": setting(config.sandbox.default_egress_policy, SettingSource::Global, false, None, SafetyClass::ConfirmBeforeCommit),
                "defaultEgressAllow": setting(config.sandbox.default_egress_allow.clone(), SettingSource::Global, false, None, SafetyClass::ConfirmBeforeCommit),
                "tailnetSidecar": setting(config.sandbox.tailnet_sidecar, SettingSource::Global, false, None, SafetyClass::ConfirmBeforeCommit),
                "headscaleUrl": setting(config.sandbox.headscale_url.clone(), SettingSource::Global, false, None, SafetyClass::ConfirmBeforeCommit),
            },
            "trust": {
                "unknownPublisher": setting(config.trust.unknown_publisher, SettingSource::Global, false, None, SafetyClass::ConfirmBeforeCommit),
                "revocationSource": setting(config.trust.revocation_source, SettingSource::Global, false, None, SafetyClass::ConfirmBeforeCommit),
            },
            "developer": {
                "logLevel": setting(config.developer.log_level, SettingSource::Global, false, None, SafetyClass::Immediate),
                "telemetry": setting(config.developer.telemetry, SettingSource::Global, false, None, SafetyClass::Immediate),
                "autoOpenDevtools": setting(config.developer.auto_open_devtools, SettingSource::Global, false, None, SafetyClass::Immediate),
            },
            "desktop": desktop_settings_resolved(config),
        },
        "runtime": {
            "auth": { "status": "unavailable" },
            "cache": { "path": normalize_path_for_display(&config.runtime.cache_location) },
            "nacelle": { "required": config.sandbox.require_nacelle, "status": "unknown" },
            "tailnet": { "enabled": config.sandbox.tailnet_sidecar, "status": "unknown" },
            "hostBridge": { "status": "local" },
        },
        "diagnostics": [],
        "actions": [
            action("clear_cache", SafetyClass::ActionOnly, true),
            action("sign_out", SafetyClass::ActionOnly, true),
            action("sync_revocation_store", SafetyClass::ActionOnly, true)
        ],
    })
}

fn desktop_settings_resolved(config: &DesktopConfig) -> Value {
    let d = &config.desktop;
    let cb = &d.control_bar;
    json!({
        "focusViewEnabled": setting(d.focus_view_enabled, SettingSource::Global, false, None, SafetyClass::Immediate),
        "startupSurface": setting(d.startup_surface, SettingSource::Global, false, None, SafetyClass::Immediate),
        "contentWindowDefaultPresentation": setting(d.content_window_default_presentation, SettingSource::Global, false, None, SafetyClass::Immediate),
        "restoreWindowFrames": setting(d.restore_window_frames, SettingSource::Global, false, None, SafetyClass::Immediate),
        "controlBar": {
            "alwaysOnTop": setting(cb.always_on_top, SettingSource::Global, false, None, SafetyClass::Immediate),
            "visibleOnStartup": setting(cb.visible_on_startup, SettingSource::Global, false, None, SafetyClass::Immediate),
            "position": setting(cb.position, SettingSource::Global, false, None, SafetyClass::Immediate),
            "autoHide": setting(cb.auto_hide, SettingSource::Global, false, None, SafetyClass::Immediate),
        },
    })
}

fn capsule_snapshot(state: &AppState, pane_id: Option<PaneId>) -> Value {
    let inspector = pane_id
        .and_then(|id| active_or_matching_capsule(state, id))
        .or_else(|| state.active_capsule_inspector());

    let Some(inspector) = inspector else {
        return json!({
            "identity": null,
            "policyDeclaration": {},
            "policyEffective": {},
            "runtime": {},
            "delivery": {},
            "connectivity": {},
            "activity": [],
            "diagnostics": [{"type": "not_found", "message": "No capsule is selected."}],
            "actions": [],
        });
    };

    let handle = inspector
        .canonical_handle
        .clone()
        .unwrap_or_else(|| inspector.handle.clone());
    let overrides = state.capsule_policy_overrides.override_for(&handle);
    let global_blocks_egress = matches!(
        state.config.sandbox.default_egress_policy,
        EgressPolicyMode::DenyAll
    );
    let egress_locked = global_blocks_egress && !overrides.egress_allow.is_empty();

    let logs = inspector
        .logs
        .iter()
        .map(|entry| {
            json!({
                "stage": entry.stage.as_str(),
                "tone": format!("{:?}", entry.tone),
                "message": entry.message,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "identity": {
            "paneId": inspector.pane_id,
            "title": inspector.title,
            "handle": inspector.handle,
            "canonicalHandle": inspector.canonical_handle,
            "trustLabel": inspector.trust_state.unwrap_or_else(|| if inspector.restricted { "untrusted".to_string() } else { "pending".to_string() }),
            "versionLabel": inspector.snapshot_label.unwrap_or_else(|| "unversioned".to_string()),
            "iconSource": resolve_icon_source(state, inspector.pane_id, inspector.local_url.as_deref()),
        },
        "policyDeclaration": {
            "networkKillSwitch": overrides.network_kill_switch,
            "egressAllow": overrides.egress_allow,
            "readonlyPaths": overrides.readonly_paths,
            "readwritePaths": overrides.readwrite_paths,
            "envGrants": overrides.env_grants,
            "revokedCapabilities": overrides.revoked_capabilities,
        },
        "policyEffective": {
            "networkKillSwitch": setting(overrides.network_kill_switch.unwrap_or(global_blocks_egress), SettingSource::UserOverride, false, None, SafetyClass::ConfirmBeforeCommit),
            "egressAllow": setting_with_lock(
                overrides.egress_allow,
                if egress_locked { SettingSource::Global } else { SettingSource::UserOverride },
                egress_locked,
                egress_locked.then_some("Global deny-all egress forbids capsule allowlist relaxation."),
                SafetyClass::ConfirmBeforeCommit
            ),
            "readonlyPaths": setting(overrides.readonly_paths, SettingSource::UserOverride, false, None, SafetyClass::ConfirmBeforeCommit),
            "readwritePaths": setting(overrides.readwrite_paths, SettingSource::UserOverride, false, None, SafetyClass::ConfirmBeforeCommit),
            "envGrants": setting(overrides.env_grants, SettingSource::UserOverride, false, None, SafetyClass::ConfirmBeforeCommit),
            "revokedCapabilities": setting(overrides.revoked_capabilities, SettingSource::UserOverride, false, None, SafetyClass::ConfirmBeforeCommit),
        },
        "runtime": {
            "sessionLabel": format!("{:?}", inspector.session_state),
            "sessionId": inspector.session_id,
            "adapter": inspector.adapter,
            "runtimeLabel": inspector.runtime_label,
            "displayStrategy": inspector.display_strategy,
            "servedBy": inspector.served_by,
        },
        "delivery": {
            "manifestPath": inspector.manifest_path,
            "logPath": inspector.log_path,
        },
        "connectivity": {
            "localUrl": inspector.local_url,
            "healthcheckUrl": inspector.healthcheck_url,
            "invokeUrl": inspector.invoke_url,
        },
        "activity": logs,
        "diagnostics": [],
        "actions": [
            action("restart_capsule", SafetyClass::ActionOnly, true),
            action("disconnect_session", SafetyClass::ActionOnly, inspector.session_id.is_some()),
            action("reset_policy_to_manifest_defaults", SafetyClass::ActionOnly, true)
        ],
    })
}

fn resolve_icon_source(
    state: &AppState,
    pane_id: PaneId,
    local_url: Option<&str>,
) -> Option<String> {
    if let Some(raw) = state.pane_icons.get(&pane_id) {
        if raw.starts_with("http://")
            || raw.starts_with("https://")
            || raw.starts_with("data:")
            || raw.starts_with("file://")
        {
            return Some(raw.clone());
        }
        let bytes = std::fs::read(raw).ok()?;
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let ext = Path::new(raw)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("png");
        let mime = match ext.to_lowercase().as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "svg" => "image/svg+xml",
            "webp" => "image/webp",
            "ico" => "image/x-icon",
            _ => "image/png",
        };
        return Some(format!("data:{mime};base64,{encoded}"));
    }
    // No manifest icon — fall back to the capsule's web favicon.
    local_url
        .and_then(|u| web_favicon_origin(u))
        .map(|origin| format!("{origin}/favicon.ico"))
}

fn active_or_matching_capsule(
    state: &AppState,
    pane_id: PaneId,
) -> Option<crate::state::CapsuleInspectorView> {
    // Fast path: the focused pane is the capsule pane.
    if let Some(active) = state.active_capsule_inspector() {
        if active.pane_id == pane_id {
            return Some(active);
        }
    }
    // Fallback: the HostPanel pane is focused (capsule detail page) but the capsule
    // pane is in the background — search all panes directly.
    state.capsule_inspector_by_pane_id(pane_id)
}

fn patch_global_settings(state: &mut AppState, payload: Value) -> Result<Value, SettingsError> {
    let confirmed = payload
        .get("confirmed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let patch = payload.get("patch").unwrap_or(&payload);
    let mut changed = Vec::new();
    let mut requires_reload = false;
    let mut requires_restart = false;

    for key in [
        "workspaceRoot",
        "cacheLocation",
        "projectionDirectory",
        "headscaleUrl",
        "defaultEgressPolicy",
        "requireNacelle",
        "tailnetSidecar",
    ] {
        if patch.get(key).is_some() && !confirmed {
            return Err(SettingsError::ConfirmRequired {
                field: key.to_string(),
                message: "This setting affects execution or connectivity and must be confirmed before commit.".to_string(),
            });
        }
    }

    state.update_config(|config| {
        if let Some(value) = patch.get("theme").and_then(Value::as_str) {
            if let Some(theme) = parse_theme(value) {
                config.general.theme = theme;
                changed.push("theme".to_string());
            }
        }
        if let Some(value) = patch.get("language").and_then(Value::as_str) {
            if let Some(language) = parse_language(value) {
                config.general.language = language;
                changed.push("language".to_string());
            }
        }
        if let Some(value) = patch.get("showInTray").and_then(Value::as_bool) {
            config.general.show_in_tray = value;
            changed.push("showInTray".to_string());
        }
        if let Some(value) = patch.get("launchAtLogin").and_then(Value::as_bool) {
            config.general.launch_at_login = value;
            changed.push("launchAtLogin".to_string());
        }
        if let Some(value) = patch.get("showWhatsNew").and_then(Value::as_bool) {
            config.general.show_whats_new = value;
            changed.push("showWhatsNew".to_string());
        }
        if let Some(value) = patch.get("updateChannel").and_then(Value::as_str) {
            if let Some(channel) = parse_update_channel(value) {
                config.updates.channel = channel;
                changed.push("updateChannel".to_string());
            }
        }
        if let Some(value) = patch.get("automaticUpdates").and_then(Value::as_bool) {
            config.updates.automatic_updates = value;
            changed.push("automaticUpdates".to_string());
        }
        if let Some(value) = patch.get("logLevel").and_then(Value::as_str) {
            if let Some(level) = parse_log_level(value) {
                config.developer.log_level = level;
                changed.push("logLevel".to_string());
            }
        }
        if let Some(value) = patch.get("telemetry").and_then(Value::as_bool) {
            config.developer.telemetry = value;
            changed.push("telemetry".to_string());
        }
        if let Some(value) = patch.get("autoOpenDevtools").and_then(Value::as_bool) {
            config.developer.auto_open_devtools = value;
            changed.push("autoOpenDevtools".to_string());
        }
        // ── Desktop settings ──────────────────────────────────────────────────
        apply_desktop_patch_immediate(config, patch, &mut changed);
    });

    if confirmed {
        apply_confirmed_global_patch(
            state,
            patch,
            &mut changed,
            &mut requires_reload,
            &mut requires_restart,
        )?;
    }

    state.sync_theme_from_settings();

    Ok(json!({
        "changedKeys": changed,
        "requiresReload": requires_reload,
        "requiresRestart": requires_restart,
        "diagnostics": diagnostics_for_global(state),
        "snapshot": global_settings_snapshot(state),
    }))
}

fn apply_confirmed_global_patch(
    state: &mut AppState,
    patch: &Value,
    changed: &mut Vec<String>,
    requires_reload: &mut bool,
    requires_restart: &mut bool,
) -> Result<(), SettingsError> {
    let mut next = state.config.clone();
    if let Some(value) = patch.get("workspaceRoot").and_then(Value::as_str) {
        next.runtime.workspace_root = normalize_user_path(value, "workspaceRoot")?;
        changed.push("workspaceRoot".to_string());
        *requires_reload = true;
    }
    if let Some(value) = patch.get("cacheLocation").and_then(Value::as_str) {
        next.runtime.cache_location = normalize_user_path(value, "cacheLocation")?;
        changed.push("cacheLocation".to_string());
        *requires_restart = true;
    }
    if let Some(value) = patch.get("projectionDirectory").and_then(Value::as_str) {
        next.delivery.projection_directory = normalize_user_path(value, "projectionDirectory")?;
        changed.push("projectionDirectory".to_string());
    }
    if let Some(value) = patch.get("headscaleUrl").and_then(Value::as_str) {
        validate_url(value, "headscaleUrl")?;
        next.sandbox.headscale_url = value.to_string();
        changed.push("headscaleUrl".to_string());
        *requires_restart = true;
    }
    if let Some(value) = patch.get("defaultEgressPolicy").and_then(Value::as_str) {
        next.sandbox.default_egress_policy = match value {
            "deny-all" => EgressPolicyMode::DenyAll,
            "allowlist" => EgressPolicyMode::Allowlist,
            "proxy-only" => EgressPolicyMode::ProxyOnly,
            _ => {
                return Err(SettingsError::Validation {
                    field: "defaultEgressPolicy".to_string(),
                    message: "Expected deny-all, allowlist, or proxy-only.".to_string(),
                })
            }
        };
        changed.push("defaultEgressPolicy".to_string());
        *requires_restart = true;
    }
    if let Some(value) = patch.get("requireNacelle").and_then(Value::as_bool) {
        next.sandbox.require_nacelle = value;
        changed.push("requireNacelle".to_string());
    }
    if let Some(value) = patch.get("tailnetSidecar").and_then(Value::as_bool) {
        next.sandbox.tailnet_sidecar = value;
        changed.push("tailnetSidecar".to_string());
        *requires_restart = true;
    }
    state.update_config(|config| *config = next);
    Ok(())
}

fn patch_capsule_policy(
    state: &mut AppState,
    pane_id: PaneId,
    payload: Value,
) -> Result<Value, SettingsError> {
    let confirmed = payload
        .get("confirmed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !confirmed {
        return Err(SettingsError::ConfirmRequired {
            field: "capsulePolicy".to_string(),
            message: "Capsule permission changes must be confirmed before commit.".to_string(),
        });
    }

    let handle = capsule_handle_for_payload(state, pane_id, &payload)?;
    let ops = payload
        .get("ops")
        .and_then(Value::as_array)
        .ok_or_else(|| SettingsError::Validation {
            field: "ops".to_string(),
            message: "patch-capsule-policy requires an ops array.".to_string(),
        })?;

    if matches!(
        state.config.sandbox.default_egress_policy,
        EgressPolicyMode::DenyAll
    ) && ops
        .iter()
        .any(|op| op.get("op").and_then(Value::as_str) == Some("add_egress_rule"))
    {
        return Err(SettingsError::PolicyDenied {
            field: "egressAllow".to_string(),
            message: "Global deny-all egress forbids capsule allowlist relaxation.".to_string(),
        });
    }

    let mut changed = Vec::new();
    let ops = ops.clone();
    state.update_capsule_policy_overrides(|store| {
        let override_entry = store.override_for_mut(&handle);
        for op in &ops {
            if apply_capsule_policy_op(override_entry, op) {
                if let Some(name) = op.get("op").and_then(Value::as_str) {
                    changed.push(name.to_string());
                }
            }
        }
    });

    Ok(json!({
        "changedKeys": changed,
        "requiresReload": false,
        "requiresRestart": true,
        "diagnostics": [],
        "snapshot": capsule_snapshot(state, Some(pane_id)),
    }))
}

fn apply_capsule_policy_op(override_entry: &mut CapsulePolicyOverride, op: &Value) -> bool {
    let Some(name) = op.get("op").and_then(Value::as_str) else {
        return false;
    };
    match name {
        "set_network_kill_switch" => {
            override_entry.network_kill_switch = op.get("value").and_then(Value::as_bool);
            true
        }
        "add_egress_rule" => add_unique(&mut override_entry.egress_allow, op_value(op)),
        "remove_egress_rule" => remove_value(&mut override_entry.egress_allow, op_value(op)),
        "add_mount_path" => {
            let mode = op.get("mode").and_then(Value::as_str).unwrap_or("readonly");
            if mode == "readwrite" {
                add_unique(&mut override_entry.readwrite_paths, op_value(op))
            } else {
                add_unique(&mut override_entry.readonly_paths, op_value(op))
            }
        }
        "remove_mount_path" => {
            remove_value(&mut override_entry.readonly_paths, op_value(op))
                | remove_value(&mut override_entry.readwrite_paths, op_value(op))
        }
        "grant_env_access" => add_unique(&mut override_entry.env_grants, op_value(op)),
        "revoke_env_access" => remove_value(&mut override_entry.env_grants, op_value(op)),
        "revoke_capability" => add_unique(&mut override_entry.revoked_capabilities, op_value(op)),
        _ => false,
    }
}

fn run_global_settings_action(
    state: &mut AppState,
    payload: Value,
) -> Result<Value, SettingsError> {
    let action_name = payload.get("action").and_then(Value::as_str).unwrap_or("");
    let status = match action_name {
        "clear_cache" => "queued",
        "sign_out" => {
            state.sign_out();
            "success"
        }
        "sync_revocation_store" => "queued",
        _ => {
            return Err(SettingsError::UnknownCommand(action_name.to_string()));
        }
    };
    state.push_activity(
        ActivityTone::Info,
        format!("Settings action {action_name}: {status}"),
    );
    Ok(json!({
        "result": {
            "action": action_name,
            "status": status,
        },
        "snapshot": global_settings_snapshot(state),
    }))
}

fn run_capsule_action(
    state: &mut AppState,
    pane_id: PaneId,
    payload: Value,
) -> Result<Value, SettingsError> {
    let action_name = payload.get("action").and_then(Value::as_str).unwrap_or("");
    if action_name == "reset_policy_to_manifest_defaults" {
        let handle = capsule_handle_for_payload(state, pane_id, &payload)?;
        state.update_capsule_policy_overrides(|store| store.reset(&handle));
        return Ok(json!({
            "result": {
                "action": action_name,
                "status": "success",
            },
            "snapshot": capsule_snapshot(state, Some(pane_id)),
        }));
    }
    Ok(json!({
        "result": {
            "action": action_name,
            "status": "not_implemented",
        },
        "snapshot": capsule_snapshot(state, Some(pane_id)),
    }))
}

fn capsule_handle_for_payload(
    state: &AppState,
    pane_id: PaneId,
    payload: &Value,
) -> Result<String, SettingsError> {
    if let Some(handle) = payload.get("handle").and_then(Value::as_str) {
        return Ok(handle.to_string());
    }
    let inspector =
        active_or_matching_capsule(state, pane_id).or_else(|| state.active_capsule_inspector());
    inspector
        .map(|view| view.canonical_handle.unwrap_or(view.handle))
        .ok_or_else(|| SettingsError::Validation {
            field: "handle".to_string(),
            message: "No capsule handle is available for this policy command.".to_string(),
        })
}

fn setting<T: Serialize>(
    value: T,
    source: SettingSource,
    locked: bool,
    lock_reason: Option<&str>,
    safety: SafetyClass,
) -> Value {
    setting_with_lock(value, source, locked, lock_reason, safety)
}

fn setting_with_lock<T: Serialize>(
    value: T,
    source: SettingSource,
    locked: bool,
    lock_reason: Option<&str>,
    safety: SafetyClass,
) -> Value {
    json!({
        "declared": value,
        "effective": value,
        "source": source.as_str(),
        "locked": locked,
        "lockReason": lock_reason,
        "safetyClass": safety.as_str(),
    })
}

fn action(id: &str, safety: SafetyClass, available: bool) -> Value {
    json!({
        "id": id,
        "safetyClass": safety.as_str(),
        "available": available,
    })
}

fn diagnostics_for_global(_state: &AppState) -> Vec<Value> {
    Vec::new()
}

fn op_value(op: &Value) -> Option<String> {
    op.get("value")
        .and_then(Value::as_str)
        .map(|value| value.to_string())
}

fn add_unique(target: &mut Vec<String>, value: Option<String>) -> bool {
    let Some(value) = value else {
        return false;
    };
    if target.contains(&value) {
        return false;
    }
    target.push(value);
    true
}

fn remove_value(target: &mut Vec<String>, value: Option<String>) -> bool {
    let Some(value) = value else {
        return false;
    };
    let before = target.len();
    target.retain(|entry| entry != &value);
    before != target.len()
}

fn normalize_user_path(raw: &str, field: &str) -> Result<String, SettingsError> {
    let path = expand_tilde(raw);
    if !path.is_absolute() {
        return Err(SettingsError::Validation {
            field: field.to_string(),
            message: "Path settings must be absolute or start with ~/.".to_string(),
        });
    }
    Ok(trim_trailing_separator(path).to_string_lossy().to_string())
}

fn normalize_path_for_display(raw: &str) -> String {
    let path = expand_tilde(raw);
    trim_trailing_separator(path).to_string_lossy().to_string()
}

fn expand_tilde(raw: &str) -> PathBuf {
    if raw == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(raw));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}

fn trim_trailing_separator(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if text.len() > 1 {
        Path::new(text.trim_end_matches('/')).to_path_buf()
    } else {
        path
    }
}

fn validate_url(value: &str, field: &str) -> Result<(), SettingsError> {
    url::Url::parse(value)
        .map(|_| ())
        .map_err(|_| SettingsError::Validation {
            field: field.to_string(),
            message: "Expected a valid absolute URL.".to_string(),
        })
}

fn parse_theme(value: &str) -> Option<ThemeConfig> {
    match value {
        "light" => Some(ThemeConfig::Light),
        "dark" => Some(ThemeConfig::Dark),
        _ => None,
    }
}

fn parse_language(value: &str) -> Option<LanguageConfig> {
    match value {
        "system" => Some(LanguageConfig::System),
        "english" | "en" => Some(LanguageConfig::English),
        "japanese" | "ja" => Some(LanguageConfig::Japanese),
        _ => None,
    }
}

fn parse_update_channel(value: &str) -> Option<UpdateChannel> {
    match value {
        "stable" => Some(UpdateChannel::Stable),
        "beta" => Some(UpdateChannel::Beta),
        "nightly" => Some(UpdateChannel::Nightly),
        _ => None,
    }
}

fn parse_log_level(value: &str) -> Option<LogLevel> {
    match value {
        "error" => Some(LogLevel::Error),
        "warn" => Some(LogLevel::Warn),
        "info" => Some(LogLevel::Info),
        "debug" => Some(LogLevel::Debug),
        _ => None,
    }
}

/// Apply the subset of desktop settings that do not require `confirmed=true`.
/// Called from both `patch_global_settings` (AppState path) and
/// `patch_config_for_capsule` (config-file-only path).
fn apply_desktop_patch_immediate(
    config: &mut DesktopConfig,
    patch: &Value,
    changed: &mut Vec<String>,
) {
    if let Some(v) = patch.get("focusViewEnabled").and_then(Value::as_bool) {
        config.desktop.focus_view_enabled = v;
        changed.push("focusViewEnabled".to_string());
    }
    if let Some(v) = patch.get("startupSurface").and_then(Value::as_str) {
        if let Some(s) = parse_startup_surface(v) {
            config.desktop.startup_surface = s;
            changed.push("startupSurface".to_string());
        }
    }
    if let Some(v) = patch.get("contentWindowDefaultPresentation").and_then(Value::as_str) {
        if let Some(p) = parse_content_window_presentation(v) {
            config.desktop.content_window_default_presentation = p;
            changed.push("contentWindowDefaultPresentation".to_string());
        }
    }
    if let Some(v) = patch.get("restoreWindowFrames").and_then(Value::as_bool) {
        config.desktop.restore_window_frames = v;
        changed.push("restoreWindowFrames".to_string());
    }
    if let Some(v) = patch.get("controlBarAlwaysOnTop").and_then(Value::as_bool) {
        config.desktop.control_bar.always_on_top = v;
        changed.push("controlBarAlwaysOnTop".to_string());
    }
    if let Some(v) = patch.get("controlBarVisibleOnStartup").and_then(Value::as_bool) {
        config.desktop.control_bar.visible_on_startup = v;
        changed.push("controlBarVisibleOnStartup".to_string());
    }
    if let Some(v) = patch.get("controlBarPosition").and_then(Value::as_str) {
        if let Some(pos) = parse_control_bar_position(v) {
            config.desktop.control_bar.position = pos;
            changed.push("controlBarPosition".to_string());
        }
    }
    if let Some(v) = patch.get("controlBarAutoHide").and_then(Value::as_bool) {
        config.desktop.control_bar.auto_hide = v;
        changed.push("controlBarAutoHide".to_string());
    }
}

/// Next-launch settings keys — changes are saved to disk but only take effect
/// after the user restarts the app.
const NEXT_LAUNCH_KEYS: &[&str] = &[
    "focusViewEnabled",
    "startupSurface",
    "contentWindowDefaultPresentation",
    "restoreWindowFrames",
    "controlBarVisibleOnStartup",
];

/// Apply a typed patch to a `DesktopConfig` loaded directly from disk (no
/// AppState).  Returns `(response_json, applies_on_next_launch)`.
///
/// This is the entry point used by the `ato-settings` capsule IPC dispatch.
pub fn patch_config_for_capsule(
    config: &mut DesktopConfig,
    patch: &Value,
    request_id: Option<&str>,
) -> Value {
    let mut changed = Vec::new();

    // General
    if let Some(v) = patch.get("theme").and_then(Value::as_str) {
        if let Some(t) = parse_theme(v) {
            config.general.theme = t;
            changed.push("theme".to_string());
        }
    }
    if let Some(v) = patch.get("language").and_then(Value::as_str) {
        if let Some(l) = parse_language(v) {
            config.general.language = l;
            changed.push("language".to_string());
        }
    }
    if let Some(v) = patch.get("launchAtLogin").and_then(Value::as_bool) {
        config.general.launch_at_login = v;
        changed.push("launchAtLogin".to_string());
    }
    if let Some(v) = patch.get("showInTray").and_then(Value::as_bool) {
        config.general.show_in_tray = v;
        changed.push("showInTray".to_string());
    }
    if let Some(v) = patch.get("showWhatsNew").and_then(Value::as_bool) {
        config.general.show_whats_new = v;
        changed.push("showWhatsNew".to_string());
    }
    // Updates
    if let Some(v) = patch.get("updateChannel").and_then(Value::as_str) {
        if let Some(ch) = parse_update_channel(v) {
            config.updates.channel = ch;
            changed.push("updateChannel".to_string());
        }
    }
    if let Some(v) = patch.get("automaticUpdates").and_then(Value::as_bool) {
        config.updates.automatic_updates = v;
        changed.push("automaticUpdates".to_string());
    }
    // Developer
    if let Some(v) = patch.get("logLevel").and_then(Value::as_str) {
        if let Some(l) = parse_log_level(v) {
            config.developer.log_level = l;
            changed.push("logLevel".to_string());
        }
    }
    if let Some(v) = patch.get("telemetry").and_then(Value::as_bool) {
        config.developer.telemetry = v;
        changed.push("telemetry".to_string());
    }
    if let Some(v) = patch.get("autoOpenDevtools").and_then(Value::as_bool) {
        config.developer.auto_open_devtools = v;
        changed.push("autoOpenDevtools".to_string());
    }
    // Desktop
    apply_desktop_patch_immediate(config, patch, &mut changed);

    let applies_on_next_launch = changed
        .iter()
        .any(|k| NEXT_LAUNCH_KEYS.contains(&k.as_str()));

    json!({
        "ok": true,
        "requestId": request_id,
        "changedKeys": changed,
        "appliesOnNextLaunch": applies_on_next_launch,
        "requiresRestart": false,
    })
}

fn parse_startup_surface(v: &str) -> Option<StartupSurface> {
    match v {
        "store" => Some(StartupSurface::Store),
        "start" => Some(StartupSurface::Start),
        "blank" => Some(StartupSurface::Blank),
        "restore-last" => Some(StartupSurface::RestoreLast),
        _ => None,
    }
}

fn parse_content_window_presentation(v: &str) -> Option<ContentWindowPresentation> {
    match v {
        "windowed" => Some(ContentWindowPresentation::Windowed),
        "maximized" => Some(ContentWindowPresentation::Maximized),
        "fullscreen" => Some(ContentWindowPresentation::Fullscreen),
        _ => None,
    }
}

fn parse_control_bar_position(v: &str) -> Option<ControlBarPosition> {
    match v {
        "top" => Some(ControlBarPosition::Top),
        "bottom" => Some(ControlBarPosition::Bottom),
        _ => None,
    }
}

/// Build a JSON snapshot of the current secret store suitable for the settings UI.
///
/// **No secret values are ever included.** Only key names, masked indicators,
/// grant counts, and storage metadata are returned.
pub fn secrets_snapshot_from_store(store: &SecretStore) -> Value {
    let keys: Vec<Value> = store
        .secrets
        .iter()
        .map(|s| {
            let grant_count = store
                .grants
                .values()
                .filter(|gkeys| gkeys.contains(&s.key))
                .count();
            json!({
                "key": s.key,
                "hasValue": !s.value.is_empty(),
                "grantCount": grant_count,
            })
        })
        .collect();

    let grants: Vec<Value> = {
        let mut g: Vec<_> = store
            .grants
            .iter()
            .filter(|(_, keys)| !keys.is_empty())
            .map(|(handle, keys)| json!({ "handle": handle, "keys": keys }))
            .collect();
        g.sort_by(|a, b| {
            a["handle"]
                .as_str()
                .cmp(&b["handle"].as_str())
        });
        g
    };

    let path_str = crate::config::secrets_path_display();
    let mode = if cfg!(unix) { "0600" } else { "platform-acl" };

    json!({
        "keys": keys,
        "grants": grants,
        "storage": {
            "path": path_str,
            "mode": mode,
            "backend": "json-file",
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ControlBarPosition, DesktopConfig, StartupSurface};

    fn default_config() -> DesktopConfig {
        DesktopConfig::default()
    }

    #[test]
    fn snapshot_from_config_includes_desktop_section() {
        let config = default_config();
        let snap = settings_snapshot_from_config(&config);
        let desktop = snap
            .get("resolved")
            .and_then(|r| r.get("desktop"))
            .expect("snapshot must contain resolved.desktop");
        assert!(desktop.get("focusViewEnabled").is_some());
        assert!(desktop.get("startupSurface").is_some());
        assert!(desktop.get("controlBar").is_some());
        let cb = desktop.get("controlBar").unwrap();
        assert!(cb.get("alwaysOnTop").is_some());
        assert!(cb.get("position").is_some());
    }

    #[test]
    fn patch_config_for_capsule_theme_change() {
        let mut config = default_config();
        let patch = serde_json::json!({"theme": "light"});
        let resp = patch_config_for_capsule(&mut config, &patch, Some("req-1"));
        assert_eq!(resp["ok"], true);
        assert_eq!(resp["requestId"], "req-1");
        let changed: Vec<String> = serde_json::from_value(resp["changedKeys"].clone()).unwrap();
        assert!(changed.contains(&"theme".to_string()));
        assert_eq!(resp["appliesOnNextLaunch"], false);
    }

    #[test]
    fn patch_config_for_capsule_startup_surface_applies_on_next_launch() {
        let mut config = default_config();
        let patch = serde_json::json!({"startupSurface": "start"});
        let resp = patch_config_for_capsule(&mut config, &patch, None);
        assert_eq!(config.desktop.startup_surface, StartupSurface::Start);
        assert_eq!(resp["appliesOnNextLaunch"], true);
    }

    #[test]
    fn patch_config_for_capsule_control_bar_position_not_next_launch() {
        let mut config = default_config();
        let patch = serde_json::json!({"controlBarPosition": "bottom"});
        let resp = patch_config_for_capsule(&mut config, &patch, None);
        assert_eq!(config.desktop.control_bar.position, ControlBarPosition::Bottom);
        // controlBarPosition is NOT in NEXT_LAUNCH_KEYS
        assert_eq!(resp["appliesOnNextLaunch"], false);
    }

    #[test]
    fn patch_config_for_capsule_unknown_key_is_ignored_silently() {
        let mut config = default_config();
        let patch = serde_json::json!({"totallyUnknownKey": "some_value"});
        let resp = patch_config_for_capsule(&mut config, &patch, None);
        assert_eq!(resp["ok"], true);
        let changed: Vec<String> = serde_json::from_value(resp["changedKeys"].clone()).unwrap();
        assert!(changed.is_empty(), "unknown key must not appear in changedKeys");
    }

    #[test]
    fn snapshot_includes_general_updates_developer() {
        let config = default_config();
        let snap = settings_snapshot_from_config(&config);
        let resolved = snap.get("resolved").unwrap();
        assert!(resolved.get("general").is_some());
        assert!(resolved.get("updates").is_some());
        assert!(resolved.get("developer").is_some());
    }

    // --- Secrets snapshot tests ---

    fn make_store_with_secrets() -> crate::config::SecretStore {
        let mut store = crate::config::SecretStore::default();
        store.add_secret("API_KEY".to_string(), "super-secret".to_string());
        store.add_secret("DB_PASS".to_string(), "hunter2".to_string());
        store.grant_secret("github.com/user/repo", "API_KEY");
        store
    }

    #[test]
    fn secrets_snapshot_has_no_values() {
        let store = make_store_with_secrets();
        let snap = secrets_snapshot_from_store(&store);
        let snap_str = serde_json::to_string(&snap).unwrap();
        assert!(
            !snap_str.contains("super-secret"),
            "secret value must not appear in snapshot"
        );
        assert!(
            !snap_str.contains("hunter2"),
            "secret value must not appear in snapshot"
        );
    }

    #[test]
    fn secrets_snapshot_keys_have_metadata() {
        let store = make_store_with_secrets();
        let snap = secrets_snapshot_from_store(&store);
        let keys = snap["keys"].as_array().unwrap();
        assert_eq!(keys.len(), 2);
        let api_key_entry = keys
            .iter()
            .find(|k| k["key"].as_str() == Some("API_KEY"))
            .expect("API_KEY must be in snapshot");
        assert_eq!(api_key_entry["hasValue"], true);
        assert_eq!(api_key_entry["grantCount"], 1);
        let db_entry = keys
            .iter()
            .find(|k| k["key"].as_str() == Some("DB_PASS"))
            .expect("DB_PASS must be in snapshot");
        assert_eq!(db_entry["grantCount"], 0);
    }

    #[test]
    fn secrets_snapshot_grants_normalized() {
        let store = make_store_with_secrets();
        let snap = secrets_snapshot_from_store(&store);
        let grants = snap["grants"].as_array().unwrap();
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0]["handle"].as_str(), Some("github.com/user/repo"));
        let grant_keys = grants[0]["keys"].as_array().unwrap();
        assert_eq!(grant_keys.len(), 1);
        assert_eq!(grant_keys[0].as_str(), Some("API_KEY"));
    }

    #[test]
    fn secrets_snapshot_empty_store() {
        let store = crate::config::SecretStore::default();
        let snap = secrets_snapshot_from_store(&store);
        assert_eq!(snap["keys"].as_array().unwrap().len(), 0);
        assert_eq!(snap["grants"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn secrets_snapshot_storage_metadata_present() {
        let store = crate::config::SecretStore::default();
        let snap = secrets_snapshot_from_store(&store);
        let storage = &snap["storage"];
        assert_eq!(storage["backend"].as_str(), Some("json-file"));
        // mode is platform-dependent but must be one of the two values
        let mode = storage["mode"].as_str().unwrap();
        assert!(mode == "0600" || mode == "platform-acl");
    }
}
