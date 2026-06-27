use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

use crate::config::{
    CapsuleOpenMode, ContentWindowPresentation, ControlBarMode, ControlBarPosition, DesktopConfig,
    LanguageConfig, LogLevel, OciBackendEngine, SecretStore, SourceBackendEngine, StartupSurface,
    ThemeConfig, UpdateChannel, WasmBackendEngine, WindowCloseBehavior,
};

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
                // Ato's Podman opt-in. Exposed so the Settings → Runtime tab can
                // distinguish "host Podman is ready" from "Podman is disabled in
                // Ato" — host status probing stays config-independent, so the
                // UI needs this flag from config to render the disabled state.
                "podmanEnabled": setting(config.runtime.podman_enabled, SettingSource::Global, false, None, SafetyClass::ConfirmBeforeCommit),
                "backendEngines": {
                    "source": setting(config.runtime.backend_engines.source, SettingSource::Global, false, None, SafetyClass::ConfirmBeforeCommit),
                    "oci": setting(config.runtime.backend_engines.oci, SettingSource::Global, false, None, SafetyClass::ConfirmBeforeCommit),
                    "wasm": setting(config.runtime.backend_engines.wasm, SettingSource::Global, false, None, SafetyClass::ConfirmBeforeCommit),
                },
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
        "startupSurface": setting(d.startup_surface, SettingSource::Global, false, None, SafetyClass::Immediate),
        "contentWindowDefaultPresentation": setting(d.content_window_default_presentation, SettingSource::Global, false, None, SafetyClass::Immediate),
        "capsuleOpenMode": setting(d.capsule_open_mode, SettingSource::Global, false, None, SafetyClass::Immediate),
        "restoreWindowFrames": setting(d.restore_window_frames, SettingSource::Global, false, None, SafetyClass::Immediate),
        "windowCloseBehavior": setting(d.window_close_behavior, SettingSource::Global, false, None, SafetyClass::Immediate),
        "controlBar": {
            "mode": setting(cb.mode, SettingSource::Global, false, None, SafetyClass::Immediate),
            "alwaysOnTop": setting(cb.always_on_top, SettingSource::Global, false, None, SafetyClass::Immediate),
            "visibleOnStartup": setting(cb.visible_on_startup, SettingSource::Global, false, None, SafetyClass::Immediate),
            "position": setting(cb.position, SettingSource::Global, false, None, SafetyClass::Immediate),
            "autoHide": setting(cb.auto_hide, SettingSource::Global, false, None, SafetyClass::Immediate),
        },
    })
}

fn apply_backend_engine_patch(
    config: &mut DesktopConfig,
    patch: &Value,
    changed: &mut Vec<String>,
    requires_reload: &mut bool,
) -> Result<(), SettingsError> {
    if let Some(value) = patch.get("sourceEngine").and_then(Value::as_str) {
        config.runtime.backend_engines.source = match value {
            "nacelle" => SourceBackendEngine::Nacelle,
            "host" => SourceBackendEngine::Host,
            _ => {
                return Err(SettingsError::Validation {
                    field: "sourceEngine".to_string(),
                    message: "Expected nacelle or host.".to_string(),
                });
            }
        };
        changed.push("sourceEngine".to_string());
        *requires_reload = true;
    }
    if let Some(value) = patch.get("ociEngine").and_then(Value::as_str) {
        config.runtime.backend_engines.oci = match value {
            "podman" => OciBackendEngine::Podman,
            _ => {
                return Err(SettingsError::Validation {
                    field: "ociEngine".to_string(),
                    message: "Expected podman. Docker and Youki are not yet supported.".to_string(),
                });
            }
        };
        changed.push("ociEngine".to_string());
        *requires_reload = true;
    }
    if let Some(value) = patch.get("wasmEngine").and_then(Value::as_str) {
        config.runtime.backend_engines.wasm = match value {
            "wasmtime" => WasmBackendEngine::Wasmtime,
            _ => {
                return Err(SettingsError::Validation {
                    field: "wasmEngine".to_string(),
                    message: "Expected wasmtime.".to_string(),
                });
            }
        };
        changed.push("wasmEngine".to_string());
        *requires_reload = true;
    }
    Ok(())
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

fn normalize_path_for_display(raw: &str) -> String {
    let path = expand_tilde(raw);
    trim_trailing_separator(path).to_string_lossy().to_string()
}

fn expand_tilde(raw: &str) -> PathBuf {
    if raw == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(raw));
    }
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
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
    if let Some(v) = patch.get("startupSurface").and_then(Value::as_str)
        && let Some(s) = parse_startup_surface(v)
    {
        config.desktop.startup_surface = s;
        changed.push("startupSurface".to_string());
    }
    if let Some(v) = patch
        .get("contentWindowDefaultPresentation")
        .and_then(Value::as_str)
        && let Some(p) = parse_content_window_presentation(v)
    {
        config.desktop.content_window_default_presentation = p;
        changed.push("contentWindowDefaultPresentation".to_string());
    }
    if let Some(v) = patch.get("capsuleOpenMode").and_then(Value::as_str)
        && let Some(mode) = parse_capsule_open_mode(v)
    {
        config.desktop.capsule_open_mode = mode;
        changed.push("capsuleOpenMode".to_string());
    }
    if let Some(v) = patch.get("restoreWindowFrames").and_then(Value::as_bool) {
        config.desktop.restore_window_frames = v;
        changed.push("restoreWindowFrames".to_string());
    }
    if let Some(v) = patch.get("windowCloseBehavior").and_then(Value::as_str)
        && let Some(behavior) = parse_window_close_behavior(v)
    {
        config.desktop.window_close_behavior = behavior;
        changed.push("windowCloseBehavior".to_string());
    }
    if let Some(v) = patch.get("controlBarAlwaysOnTop").and_then(Value::as_bool) {
        config.desktop.control_bar.always_on_top = v;
        changed.push("controlBarAlwaysOnTop".to_string());
    }
    if let Some(v) = patch.get("controlBarMode").and_then(Value::as_str)
        && let Some(mode) = parse_control_bar_mode(v)
    {
        let _ = mode;
        config.desktop.control_bar.mode = ControlBarMode::Floating;
        config.desktop.control_bar.visible_on_startup = true;
        config.desktop.control_bar.auto_hide = false;
        changed.push("controlBarMode".to_string());
    }
    if let Some(v) = patch
        .get("controlBarVisibleOnStartup")
        .and_then(Value::as_bool)
    {
        let _ = v;
        config.desktop.control_bar.visible_on_startup = true;
        config.desktop.control_bar.mode = ControlBarMode::Floating;
        config.desktop.control_bar.auto_hide = false;
        changed.push("controlBarVisibleOnStartup".to_string());
    }
    if let Some(v) = patch.get("controlBarPosition").and_then(Value::as_str)
        && let Some(pos) = parse_control_bar_position(v)
    {
        config.desktop.control_bar.position = pos;
        changed.push("controlBarPosition".to_string());
    }
    if let Some(v) = patch.get("controlBarAutoHide").and_then(Value::as_bool) {
        let _ = v;
        config.desktop.control_bar.auto_hide = false;
        config.desktop.control_bar.mode = ControlBarMode::Floating;
        config.desktop.control_bar.visible_on_startup = true;
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
    let mut requires_reload = false;

    const ENGINE_KEYS: &[&str] = &["sourceEngine", "ociEngine", "wasmEngine"];
    if let Some(engine_key) = ENGINE_KEYS.iter().find(|&&key| patch.get(key).is_some()) {
        let confirmed = patch
            .get("confirmed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !confirmed {
            let err = SettingsError::ConfirmRequired {
                field: (*engine_key).to_string(),
                message:
                    "This setting affects execution or connectivity and must be confirmed before commit."
                        .to_string(),
            };
            return json!({
                "ok": false,
                "requestId": request_id,
                "error": err.to_json(),
                "changedKeys": [],
                "appliesOnNextLaunch": false,
                "requiresReload": false,
                "requiresRestart": false,
            });
        }
        let mut next = config.clone();
        if let Err(err) =
            apply_backend_engine_patch(&mut next, patch, &mut changed, &mut requires_reload)
        {
            return json!({
                "ok": false,
                "requestId": request_id,
                "error": err.to_json(),
                "changedKeys": [],
                "appliesOnNextLaunch": false,
                "requiresReload": false,
                "requiresRestart": false,
            });
        }
        *config = next;
    }

    // General
    if let Some(v) = patch.get("theme").and_then(Value::as_str)
        && let Some(t) = parse_theme(v)
    {
        config.general.theme = t;
        changed.push("theme".to_string());
    }
    if let Some(v) = patch.get("language").and_then(Value::as_str)
        && let Some(l) = parse_language(v)
    {
        config.general.language = l;
        changed.push("language".to_string());
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
    if let Some(v) = patch.get("updateChannel").and_then(Value::as_str)
        && let Some(ch) = parse_update_channel(v)
    {
        config.updates.channel = ch;
        changed.push("updateChannel".to_string());
    }
    if let Some(v) = patch.get("automaticUpdates").and_then(Value::as_bool) {
        config.updates.automatic_updates = v;
        changed.push("automaticUpdates".to_string());
    }
    // Developer
    if let Some(v) = patch.get("logLevel").and_then(Value::as_str)
        && let Some(l) = parse_log_level(v)
    {
        config.developer.log_level = l;
        changed.push("logLevel".to_string());
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
        "requiresReload": requires_reload,
        "requiresRestart": false,
    })
}

fn parse_startup_surface(v: &str) -> Option<StartupSurface> {
    match v {
        "store" => Some(StartupSurface::Store),
        "start" => Some(StartupSurface::Start),
        "home" => Some(StartupSurface::Home),
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

fn parse_capsule_open_mode(v: &str) -> Option<CapsuleOpenMode> {
    match v {
        "window" => Some(CapsuleOpenMode::Window),
        "webviewer" => Some(CapsuleOpenMode::Webviewer),
        "os-browser" | "os-default-browser" => Some(CapsuleOpenMode::OsBrowser),
        _ => None,
    }
}

fn parse_window_close_behavior(v: &str) -> Option<WindowCloseBehavior> {
    match v {
        "keep-session-running" => Some(WindowCloseBehavior::KeepSessionRunning),
        "stop-session" => Some(WindowCloseBehavior::StopSession),
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

fn parse_control_bar_mode(v: &str) -> Option<ControlBarMode> {
    match v {
        "floating" | "auto-hide" | "compact-pill" | "hidden" => Some(ControlBarMode::Floating),
        _ => None,
    }
}

/// Build a JSON snapshot of the current secret store suitable for the settings UI.
///
/// **No secret values are ever included.** Only key names, masked indicators,
/// grant counts, and storage metadata are returned.
pub fn secrets_snapshot_from_store(store: &SecretStore) -> Value {
    let (grant_counts, grants) = match crate::secret_bridge::CliSecretBridge::list() {
        Ok(entries) => {
            let mut per_key_count: std::collections::HashMap<String, usize> =
                std::collections::HashMap::new();
            let mut handle_to_keys: std::collections::HashMap<String, Vec<String>> =
                std::collections::HashMap::new();
            for e in &entries {
                if let Some(ref allow) = e.allow {
                    per_key_count.insert(e.key.clone(), allow.len());
                    for handle in allow {
                        handle_to_keys
                            .entry(handle.clone())
                            .or_default()
                            .push(e.key.clone());
                    }
                }
            }
            let key_counts = entries
                .iter()
                .map(|e| {
                    let count = per_key_count.get(&e.key).copied().unwrap_or(0);
                    json!({
                        "key": e.key,
                        "hasValue": true,
                        "grantCount": count,
                    })
                })
                .collect::<Vec<_>>();
            let grant_entries: Vec<Value> = handle_to_keys
                .into_iter()
                .filter(|(_, keys)| !keys.is_empty())
                .map(|(handle, keys)| json!({ "handle": handle, "keys": keys }))
                .collect();
            (key_counts, grant_entries)
        }
        Err(_) => {
            let fallback_keys: Vec<Value> = store
                .secrets
                .iter()
                .map(|s| json!({ "key": s.key, "hasValue": false, "grantCount": 0 }))
                .collect();
            (fallback_keys, Vec::new())
        }
    };

    let path_str = crate::config::secrets_path_display();
    let mode = if cfg!(unix) { "0600" } else { "platform-acl" };

    json!({
        "keys": grant_counts,
        "grants": grants,
        "storage": {
            "path": path_str,
            "mode": mode,
            "backend": "age-file",
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ControlBarMode, ControlBarPosition, DesktopConfig, OciBackendEngine, SourceBackendEngine,
        StartupSurface, WasmBackendEngine,
    };

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
        assert!(desktop.get("capsuleOpenMode").is_some());
        assert!(desktop.get("windowCloseBehavior").is_some());
        assert!(desktop.get("controlBar").is_some());
        let cb = desktop.get("controlBar").unwrap();
        assert!(cb.get("mode").is_some());
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
        assert_eq!(
            config.desktop.control_bar.position,
            ControlBarPosition::Bottom
        );
        // controlBarPosition is NOT in NEXT_LAUNCH_KEYS
        assert_eq!(resp["appliesOnNextLaunch"], false);
    }

    #[test]
    fn patch_config_for_capsule_control_bar_mode_updates_declaration() {
        let mut config = default_config();
        let patch = serde_json::json!({"controlBarMode": "compact-pill"});
        let resp = patch_config_for_capsule(&mut config, &patch, None);
        assert_eq!(config.desktop.control_bar.mode, ControlBarMode::Floating);
        assert!(config.desktop.control_bar.visible_on_startup);
        assert!(!config.desktop.control_bar.auto_hide);
        assert_eq!(resp["appliesOnNextLaunch"], false);
    }

    #[test]
    fn patch_config_for_capsule_unknown_key_is_ignored_silently() {
        let mut config = default_config();
        let patch = serde_json::json!({"totallyUnknownKey": "some_value"});
        let resp = patch_config_for_capsule(&mut config, &patch, None);
        assert_eq!(resp["ok"], true);
        let changed: Vec<String> = serde_json::from_value(resp["changedKeys"].clone()).unwrap();
        assert!(
            changed.is_empty(),
            "unknown key must not appear in changedKeys"
        );
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

    #[test]
    fn snapshot_exposes_runtime_podman_enabled() {
        // The Settings → Runtime tab needs the Ato Podman opt-in from config to
        // render "Disabled in Ato" independently of host probing. Verify the
        // flag round-trips through the resolved snapshot for both states.
        let mut config = default_config();
        config.runtime.podman_enabled = true;
        let snap = settings_snapshot_from_config(&config);
        assert_eq!(
            snap["resolved"]["runtime"]["podmanEnabled"]["declared"],
            serde_json::json!(true)
        );
        assert_eq!(
            snap["resolved"]["runtime"]["podmanEnabled"]["effective"],
            serde_json::json!(true)
        );

        config.runtime.podman_enabled = false;
        let snap = settings_snapshot_from_config(&config);
        assert_eq!(
            snap["resolved"]["runtime"]["podmanEnabled"]["declared"],
            serde_json::json!(false),
            "disabled Podman must surface in the snapshot, not be omitted"
        );
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

    #[test]
    fn snapshot_includes_capsule_open_mode() {
        let config = default_config();
        let snap = settings_snapshot_from_config(&config);
        let desktop = snap
            .get("resolved")
            .and_then(|r| r.get("desktop"))
            .expect("snapshot must contain resolved.desktop");
        let entry = desktop
            .get("capsuleOpenMode")
            .expect("resolved.desktop.capsuleOpenMode must be present");
        assert_eq!(entry["declared"].as_str(), Some("window"));
        assert_eq!(entry["effective"].as_str(), Some("window"));
        let wcb = desktop
            .get("windowCloseBehavior")
            .expect("resolved.desktop.windowCloseBehavior must be present");
        assert_eq!(wcb["declared"].as_str(), Some("keep-session-running"));
    }

    #[test]
    fn patch_capsule_open_mode_webviewer() {
        let mut config = default_config();
        let patch = serde_json::json!({"capsuleOpenMode": "webviewer"});
        let resp = patch_config_for_capsule(&mut config, &patch, None);
        assert_eq!(config.desktop.capsule_open_mode, CapsuleOpenMode::Webviewer);
        let changed: Vec<String> = serde_json::from_value(resp["changedKeys"].clone()).unwrap();
        assert!(changed.contains(&"capsuleOpenMode".to_string()));
        assert_eq!(resp["appliesOnNextLaunch"], false);
    }

    #[test]
    fn capsule_open_mode_not_in_next_launch_keys() {
        assert!(
            !NEXT_LAUNCH_KEYS.contains(&"capsuleOpenMode"),
            "capsuleOpenMode must not be in NEXT_LAUNCH_KEYS"
        );
    }

    #[test]
    fn patch_capsule_open_mode_unknown_ignored() {
        let mut config = default_config();
        let original = config.desktop.capsule_open_mode;
        let patch = serde_json::json!({"capsuleOpenMode": "unknown-mode"});
        let resp = patch_config_for_capsule(&mut config, &patch, None);
        assert_eq!(config.desktop.capsule_open_mode, original);
        let changed: Vec<String> = serde_json::from_value(resp["changedKeys"].clone()).unwrap();
        assert!(!changed.contains(&"capsuleOpenMode".to_string()));
    }

    #[test]
    fn patch_window_close_behavior_stop_session() {
        let mut config = default_config();
        let patch = serde_json::json!({"windowCloseBehavior": "stop-session"});
        let resp = patch_config_for_capsule(&mut config, &patch, None);
        assert_eq!(
            config.desktop.window_close_behavior,
            WindowCloseBehavior::StopSession
        );
        let changed: Vec<String> = serde_json::from_value(resp["changedKeys"].clone()).unwrap();
        assert!(changed.contains(&"windowCloseBehavior".to_string()));
        assert_eq!(resp["appliesOnNextLaunch"], false);
    }

    #[test]
    fn window_close_behavior_not_in_next_launch_keys() {
        assert!(
            !NEXT_LAUNCH_KEYS.contains(&"windowCloseBehavior"),
            "windowCloseBehavior must not be in NEXT_LAUNCH_KEYS"
        );
    }

    #[test]
    fn patch_window_close_behavior_unknown_ignored() {
        let mut config = default_config();
        let original = config.desktop.window_close_behavior;
        let patch = serde_json::json!({"windowCloseBehavior": "unknown-behavior"});
        let resp = patch_config_for_capsule(&mut config, &patch, None);
        assert_eq!(config.desktop.window_close_behavior, original);
        let changed: Vec<String> = serde_json::from_value(resp["changedKeys"].clone()).unwrap();
        assert!(!changed.contains(&"windowCloseBehavior".to_string()));
    }

    #[test]
    fn patch_config_for_capsule_source_engine_requires_confirmation() {
        let mut config = default_config();
        let original = config.runtime.backend_engines.source;
        let patch = serde_json::json!({"sourceEngine": "host"});
        let resp = patch_config_for_capsule(&mut config, &patch, Some("req-engine"));
        assert_eq!(resp["ok"], false);
        assert_eq!(resp["requestId"], "req-engine");
        assert_eq!(resp["error"]["type"], "confirm_required");
        assert_eq!(resp["error"]["field"], "sourceEngine");
        assert_eq!(config.runtime.backend_engines.source, original);
    }

    #[test]
    fn patch_config_for_capsule_invalid_oci_engine_returns_validation_error() {
        let mut config = default_config();
        let patch = serde_json::json!({"ociEngine": "docker", "confirmed": true});
        let resp = patch_config_for_capsule(&mut config, &patch, None);
        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["type"], "validation_error");
        assert_eq!(resp["error"]["field"], "ociEngine");
        assert_eq!(config.runtime.backend_engines.oci, OciBackendEngine::Podman);
    }

    #[test]
    fn patch_config_for_capsule_invalid_engine_patch_does_not_partially_apply() {
        let mut config = default_config();
        let patch =
            serde_json::json!({"sourceEngine": "host", "ociEngine": "docker", "confirmed": true});
        let resp = patch_config_for_capsule(&mut config, &patch, None);
        assert_eq!(resp["ok"], false);
        assert_eq!(resp["error"]["type"], "validation_error");
        assert_eq!(
            config.runtime.backend_engines.source,
            SourceBackendEngine::Nacelle
        );
        assert_eq!(config.runtime.backend_engines.oci, OciBackendEngine::Podman);
    }

    #[test]
    fn patch_config_for_capsule_engine_change_updates_config_and_reload_flag() {
        let mut config = default_config();
        let patch = serde_json::json!({"sourceEngine": "host", "confirmed": true});
        let resp = patch_config_for_capsule(&mut config, &patch, None);
        let changed: Vec<String> = serde_json::from_value(resp["changedKeys"].clone()).unwrap();
        assert_eq!(resp["ok"], true);
        assert_eq!(
            config.runtime.backend_engines.source,
            SourceBackendEngine::Host
        );
        assert!(changed.contains(&"sourceEngine".to_string()));
        assert_eq!(resp["requiresReload"], true);

        let snapshot = settings_snapshot_from_config(&config);
        assert_eq!(
            snapshot["resolved"]["runtime"]["backendEngines"]["source"]["declared"],
            "host"
        );
    }

    #[test]
    fn patch_config_for_capsule_wasm_engine_updates_config() {
        let mut config = default_config();
        let patch = serde_json::json!({"wasmEngine": "wasmtime", "confirmed": true});
        let resp = patch_config_for_capsule(&mut config, &patch, None);
        assert_eq!(resp["ok"], true);
        assert_eq!(
            config.runtime.backend_engines.wasm,
            WasmBackendEngine::Wasmtime
        );
    }
}
