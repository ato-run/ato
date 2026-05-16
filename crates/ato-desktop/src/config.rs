use serde::{Deserialize, Deserializer, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use tracing::{info, warn};

use capsule_core::common::paths::ato_path;

/// Persistent configuration for the ato-desktop application.
///
/// Stored at `~/.ato/desktop-config.json` and loaded on startup.
#[derive(Clone, Debug, Serialize)]
pub struct DesktopConfig {
    #[serde(default)]
    pub general: GeneralSettings,
    #[serde(default)]
    pub updates: UpdateSettings,
    #[serde(default)]
    pub runtime: RuntimeSettings,
    #[serde(default)]
    pub sandbox: SandboxSettings,
    #[serde(default)]
    pub trust: TrustSettings,
    #[serde(default)]
    pub registry: RegistrySettings,
    #[serde(default)]
    pub delivery: DeliverySettings,
    #[serde(default)]
    pub developer: DeveloperSettings,
    #[serde(default)]
    pub desktop: DesktopSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GeneralSettings {
    /// Light or Dark theme. System theme is a UI-level option for now.
    #[serde(default)]
    pub theme: ThemeConfig,
    #[serde(default)]
    pub language: LanguageConfig,
    #[serde(default)]
    pub launch_at_login: bool,
    #[serde(default = "default_show_in_tray")]
    pub show_in_tray: bool,
    #[serde(default = "default_show_whats_new")]
    pub show_whats_new: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpdateSettings {
    #[serde(default)]
    pub channel: UpdateChannel,
    #[serde(default = "default_auto_updates")]
    pub automatic_updates: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeSettings {
    #[serde(default = "default_cache_location")]
    pub cache_location: String,
    #[serde(default = "default_cache_size_limit_gb")]
    pub cache_size_limit_gb: u16,
    #[serde(default = "default_workspace_root")]
    pub workspace_root: String,
    #[serde(default = "default_watch_debounce_ms")]
    pub watch_debounce_ms: u64,
    #[serde(default)]
    pub execution_boundary: ExecutionBoundary,
    #[serde(default)]
    pub unsafe_prompt: UnsafePrompt,
    #[serde(default)]
    pub allow_unsafe_env: bool,
    /// Terminal font size in pixels.
    #[serde(default = "default_terminal_font_size")]
    pub terminal_font_size: u16,
    /// Maximum number of concurrent terminal sessions.
    #[serde(default = "default_terminal_max_sessions")]
    pub terminal_max_sessions: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SandboxSettings {
    #[serde(default = "default_require_nacelle")]
    pub require_nacelle: bool,
    #[serde(default)]
    pub default_egress_policy: EgressPolicyMode,
    /// Default egress allow patterns for new sessions.
    #[serde(default)]
    pub default_egress_allow: Vec<String>,
    #[serde(default)]
    pub tailnet_sidecar: bool,
    #[serde(default = "default_headscale_url")]
    pub headscale_url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrustSettings {
    #[serde(default = "default_revocation_frequency_hours")]
    pub revocation_frequency_hours: u16,
    #[serde(default)]
    pub revocation_source: RevocationSource,
    #[serde(default)]
    pub unknown_publisher: UnknownPublisherPolicy,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegistrySettings {
    #[serde(default = "default_store_api_url")]
    pub store_api_url: String,
    #[serde(default = "default_store_site_url")]
    pub store_site_url: String,
    #[serde(default)]
    pub private_registries: Vec<PrivateRegistrySettings>,
    #[serde(default = "default_local_registry_port")]
    pub local_registry_port: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeliverySettings {
    #[serde(default)]
    pub projection_enabled_by_default: bool,
    #[serde(default = "default_projection_directory")]
    pub projection_directory: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeveloperSettings {
    #[serde(default)]
    pub log_level: LogLevel,
    #[serde(default)]
    pub telemetry: bool,
    #[serde(default)]
    pub auto_open_devtools: bool,
    #[serde(default)]
    pub feature_flags: HashSet<String>,
}

/// Desktop-shell specific settings (Control Bar, Focus View, window behaviour).
///
/// Defaults match the current hardcoded behaviour so existing users see no
/// change after the config section is introduced.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DesktopSettings {
    /// Whether the Focus View (multi-window) mode is enabled.
    /// Env `ATO_DESKTOP_MULTI_WINDOW` takes precedence over this value during
    /// the migration period.
    #[serde(default = "default_focus_view_enabled")]
    pub focus_view_enabled: bool,
    /// Which surface is shown after the app starts.
    #[serde(default)]
    pub startup_surface: StartupSurface,
    /// Initial presentation mode for content windows opened by Focus View.
    #[serde(default)]
    pub content_window_default_presentation: ContentWindowPresentation,
    /// Whether to restore the last window frames (position/size) on launch.
    #[serde(default)]
    pub restore_window_frames: bool,
    /// One-time onboarding flow completion state.
    #[serde(default)]
    pub onboarding: OnboardingSettings,
    #[serde(default)]
    pub control_bar: ControlBarSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OnboardingSettings {
    #[serde(default)]
    pub completed: bool,
    #[serde(default)]
    pub skipped: bool,
    #[serde(default)]
    pub version: u16,
}

impl Default for OnboardingSettings {
    fn default() -> Self {
        Self {
            completed: false,
            skipped: false,
            version: 0,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ControlBarSettings {
    /// Display mode for the process-global Control Bar palette.
    #[serde(default)]
    pub mode: ControlBarMode,
    /// Whether the Control Bar floats above all other windows.
    #[serde(default = "default_control_bar_always_on_top")]
    pub always_on_top: bool,
    /// Whether the Control Bar is shown when the app starts.
    #[serde(default = "default_control_bar_visible_on_startup")]
    pub visible_on_startup: bool,
    #[serde(default)]
    pub position: ControlBarPosition,
    /// Automatically hide the Control Bar when not in use.
    #[serde(default)]
    pub auto_hide: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ControlBarMode {
    #[default]
    Floating,
    AutoHide,
    CompactPill,
    Hidden,
}

impl<'de> Deserialize<'de> for ControlBarSettings {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawControlBarSettings {
            mode: Option<ControlBarMode>,
            #[serde(default = "default_control_bar_always_on_top")]
            always_on_top: bool,
            #[serde(default = "default_control_bar_visible_on_startup")]
            visible_on_startup: bool,
            #[serde(default)]
            position: ControlBarPosition,
            #[serde(default)]
            auto_hide: bool,
        }

        let raw = RawControlBarSettings::deserialize(deserializer)?;
        let mode = raw.mode.unwrap_or_else(|| {
            if !raw.visible_on_startup {
                ControlBarMode::Hidden
            } else if raw.auto_hide {
                ControlBarMode::AutoHide
            } else {
                ControlBarMode::Floating
            }
        });

        Ok(Self {
            mode,
            always_on_top: raw.always_on_top,
            visible_on_startup: raw.visible_on_startup,
            position: raw.position,
            auto_hide: raw.auto_hide,
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum StartupSurface {
    Store,
    #[default]
    Start,
    Blank,
    RestoreLast,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ContentWindowPresentation {
    #[default]
    Windowed,
    Maximized,
    Fullscreen,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ControlBarPosition {
    #[default]
    Top,
    Bottom,
}

fn default_focus_view_enabled() -> bool {
    true
}
fn default_control_bar_always_on_top() -> bool {
    true
}
fn default_control_bar_visible_on_startup() -> bool {
    true
}

impl Default for DesktopSettings {
    fn default() -> Self {
        Self {
            focus_view_enabled: default_focus_view_enabled(),
            startup_surface: StartupSurface::Start,
            content_window_default_presentation: ContentWindowPresentation::Windowed,
            restore_window_frames: false,
            onboarding: OnboardingSettings::default(),
            control_bar: ControlBarSettings::default(),
        }
    }
}

impl Default for ControlBarSettings {
    fn default() -> Self {
        Self {
            mode: ControlBarMode::Floating,
            always_on_top: default_control_bar_always_on_top(),
            visible_on_startup: default_control_bar_visible_on_startup(),
            position: ControlBarPosition::Top,
            auto_hide: false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrivateRegistrySettings {
    pub name: String,
    pub base_url: String,
    #[serde(default = "default_registry_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub trust_mode: RegistryTrustMode,
    #[serde(default)]
    pub priority: u16,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeConfig {
    Light,
    #[default]
    Dark,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum LanguageConfig {
    #[default]
    System,
    English,
    Japanese,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateChannel {
    #[default]
    Stable,
    Beta,
    Nightly,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionBoundary {
    #[default]
    Tier1Only,
    Tier1PlusTier2Confirm,
    Tier1PlusTier2Auto,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum UnsafePrompt {
    #[default]
    AlwaysConfirm,
    ConfirmOncePerCapsule,
    Never,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum EgressPolicyMode {
    #[default]
    DenyAll,
    Allowlist,
    ProxyOnly,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RevocationSource {
    #[default]
    DnsTxt,
    Https,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum UnknownPublisherPolicy {
    #[default]
    Prompt,
    AutoTrust,
    Reject,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    #[default]
    Warn,
    Info,
    Debug,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryTrustMode {
    #[default]
    Prompt,
    Pinned,
}

fn default_terminal_font_size() -> u16 {
    14
}

fn default_terminal_max_sessions() -> usize {
    4
}

fn default_show_in_tray() -> bool {
    true
}

fn default_show_whats_new() -> bool {
    true
}

fn default_auto_updates() -> bool {
    true
}

fn default_cache_location() -> String {
    "~/.ato/cache".to_string()
}

fn default_cache_size_limit_gb() -> u16 {
    10
}

fn default_workspace_root() -> String {
    "~/.ato/workspaces".to_string()
}

fn default_watch_debounce_ms() -> u64 {
    300
}

fn default_require_nacelle() -> bool {
    true
}

fn default_headscale_url() -> String {
    "https://hs.ato.run".to_string()
}

fn default_revocation_frequency_hours() -> u16 {
    24
}

fn default_store_api_url() -> String {
    "https://api.ato.run".to_string()
}

fn default_store_site_url() -> String {
    "https://ato.run".to_string()
}

fn default_local_registry_port() -> u16 {
    8080
}

fn default_projection_directory() -> String {
    "/Applications".to_string()
}

fn default_registry_enabled() -> bool {
    true
}

#[allow(clippy::derivable_impls)]
impl Default for DesktopConfig {
    fn default() -> Self {
        Self {
            general: GeneralSettings::default(),
            updates: UpdateSettings::default(),
            runtime: RuntimeSettings::default(),
            sandbox: SandboxSettings::default(),
            trust: TrustSettings::default(),
            registry: RegistrySettings::default(),
            delivery: DeliverySettings::default(),
            developer: DeveloperSettings::default(),
            desktop: DesktopSettings::default(),
        }
    }
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            theme: ThemeConfig::Dark,
            language: LanguageConfig::System,
            launch_at_login: false,
            show_in_tray: default_show_in_tray(),
            show_whats_new: default_show_whats_new(),
        }
    }
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            channel: UpdateChannel::Stable,
            automatic_updates: default_auto_updates(),
        }
    }
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self {
            cache_location: default_cache_location(),
            cache_size_limit_gb: default_cache_size_limit_gb(),
            workspace_root: default_workspace_root(),
            watch_debounce_ms: default_watch_debounce_ms(),
            execution_boundary: ExecutionBoundary::Tier1Only,
            unsafe_prompt: UnsafePrompt::AlwaysConfirm,
            allow_unsafe_env: false,
            terminal_font_size: default_terminal_font_size(),
            terminal_max_sessions: default_terminal_max_sessions(),
        }
    }
}

impl Default for SandboxSettings {
    fn default() -> Self {
        Self {
            require_nacelle: default_require_nacelle(),
            default_egress_policy: EgressPolicyMode::DenyAll,
            default_egress_allow: Vec::new(),
            tailnet_sidecar: false,
            headscale_url: default_headscale_url(),
        }
    }
}

impl Default for TrustSettings {
    fn default() -> Self {
        Self {
            revocation_frequency_hours: default_revocation_frequency_hours(),
            revocation_source: RevocationSource::DnsTxt,
            unknown_publisher: UnknownPublisherPolicy::Prompt,
        }
    }
}

impl Default for RegistrySettings {
    fn default() -> Self {
        Self {
            store_api_url: default_store_api_url(),
            store_site_url: default_store_site_url(),
            private_registries: Vec::new(),
            local_registry_port: default_local_registry_port(),
        }
    }
}

impl Default for DeliverySettings {
    fn default() -> Self {
        Self {
            projection_enabled_by_default: false,
            projection_directory: default_projection_directory(),
        }
    }
}

impl Default for DeveloperSettings {
    fn default() -> Self {
        Self {
            log_level: LogLevel::Warn,
            telemetry: false,
            auto_open_devtools: false,
            feature_flags: HashSet::new(),
        }
    }
}

impl<'de> Deserialize<'de> for DesktopConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Helper {
            #[serde(default)]
            general: GeneralSettings,
            #[serde(default)]
            updates: UpdateSettings,
            #[serde(default)]
            runtime: RuntimeSettings,
            #[serde(default)]
            sandbox: SandboxSettings,
            #[serde(default)]
            trust: TrustSettings,
            #[serde(default)]
            registry: RegistrySettings,
            #[serde(default)]
            delivery: DeliverySettings,
            #[serde(default)]
            developer: DeveloperSettings,
            #[serde(default)]
            desktop: DesktopSettings,
            #[serde(default)]
            theme: Option<ThemeConfig>,
            #[serde(default)]
            default_egress_allow: Option<Vec<String>>,
            #[serde(default)]
            terminal_font_size: Option<u16>,
            #[serde(default)]
            terminal_max_sessions: Option<usize>,
            #[serde(default)]
            auto_open_devtools: Option<bool>,
        }

        let helper = Helper::deserialize(deserializer)?;
        let mut config = DesktopConfig {
            general: helper.general,
            updates: helper.updates,
            runtime: helper.runtime,
            sandbox: helper.sandbox,
            trust: helper.trust,
            registry: helper.registry,
            delivery: helper.delivery,
            developer: helper.developer,
            desktop: helper.desktop,
        };

        if let Some(theme) = helper.theme {
            config.general.theme = theme;
        }
        if let Some(allow) = helper.default_egress_allow {
            config.sandbox.default_egress_allow = allow;
            if !config.sandbox.default_egress_allow.is_empty() {
                config.sandbox.default_egress_policy = EgressPolicyMode::Allowlist;
            }
        }
        if let Some(font_size) = helper.terminal_font_size {
            config.runtime.terminal_font_size = font_size;
        }
        if let Some(max_sessions) = helper.terminal_max_sessions {
            config.runtime.terminal_max_sessions = max_sessions;
        }
        if let Some(auto_open) = helper.auto_open_devtools {
            config.developer.auto_open_devtools = auto_open;
        }

        Ok(config)
    }
}

fn config_path() -> Option<PathBuf> {
    ato_path("desktop-config.json").ok()
}

/// Load configuration from `~/.ato/desktop-config.json`.
/// Returns `Default` if the file does not exist or is invalid.
pub fn load_config() -> DesktopConfig {
    let Some(path) = config_path() else {
        return DesktopConfig::default();
    };

    match std::fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(config) => {
                info!(path = %path.display(), "Loaded desktop config");
                config
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Failed to parse desktop config, using defaults");
                DesktopConfig::default()
            }
        },
        Err(_) => DesktopConfig::default(),
    }
}

/// Save configuration to `~/.ato/desktop-config.json`.
pub fn save_config(config: &DesktopConfig) {
    let Some(path) = config_path() else {
        warn!("Cannot determine home directory, config not saved");
        return;
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    match serde_json::to_string_pretty(config) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                warn!(path = %path.display(), error = %e, "Failed to write desktop config");
            }
        }
        Err(e) => {
            warn!(error = %e, "Failed to serialize desktop config");
        }
    }
}

// ── Secret Store ──────────────────────────────────────────────────────────────

/// A single secret key-value pair.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SecretEntry {
    pub key: String,
    /// Stored as plaintext in the JSON file (MVP).
    /// Phase 2: macOS Keychain integration.
    pub value: String,
}

/// Secret storage with per-capsule grant management.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SecretStore {
    /// Global secret entries.
    #[serde(default)]
    pub secrets: Vec<SecretEntry>,
    /// Per-capsule grants: capsule handle → list of secret keys allowed.
    #[serde(default)]
    pub grants: std::collections::HashMap<String, Vec<String>>,
}

impl SecretStore {
    pub fn add_secret(&mut self, key: String, value: String) {
        if let Some(existing) = self.secrets.iter_mut().find(|s| s.key == key) {
            existing.value = value;
        } else {
            self.secrets.push(SecretEntry { key, value });
        }
    }

    pub fn remove_secret(&mut self, key: &str) {
        self.secrets.retain(|s| s.key != key);
        for keys in self.grants.values_mut() {
            keys.retain(|k| k != key);
        }
    }

    pub fn secrets_for_capsule(&self, handle: &str) -> Vec<&SecretEntry> {
        let Some(allowed_keys) = self.grants.get(handle) else {
            return Vec::new();
        };
        self.secrets
            .iter()
            .filter(|s| allowed_keys.contains(&s.key))
            .collect()
    }

    pub fn grant_secret(&mut self, capsule_handle: &str, key: &str) {
        let keys = self.grants.entry(capsule_handle.to_string()).or_default();
        if !keys.contains(&key.to_string()) {
            keys.push(key.to_string());
        }
    }

    pub fn revoke_secret(&mut self, capsule_handle: &str, key: &str) {
        if let Some(keys) = self.grants.get_mut(capsule_handle) {
            keys.retain(|k| k != key);
        }
    }
}

fn secrets_path() -> Option<PathBuf> {
    ato_path("secrets.json").ok()
}

/// Return a human-readable display path for the secrets file, collapsing
/// the home directory to `~`. Used by the settings UI snapshot.
pub fn secrets_path_display() -> Option<String> {
    secrets_path().map(|p| {
        if let Ok(home) = home_dir_path() {
            if let Ok(rel) = p.strip_prefix(&home) {
                return format!("~/{}", rel.display());
            }
        }
        p.display().to_string()
    })
}

fn home_dir_path() -> Result<PathBuf, ()> {
    dirs::home_dir().ok_or(())
}

pub fn load_secrets() -> SecretStore {
    let Some(path) = secrets_path() else {
        return SecretStore::default();
    };

    match std::fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(store) => {
                info!(path = %path.display(), "Loaded secret store");
                store
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Failed to parse secret store, using empty");
                SecretStore::default()
            }
        },
        Err(_) => SecretStore::default(),
    }
}

/// Distinct error type so the UI can surface a precise reason — "your
/// secret was not saved" is the visible failure, "could not encode JSON"
/// vs "could not write file" vs "could not chmod 0600" are diagnostic.
#[derive(Debug, thiserror::Error)]
pub enum SaveSecretsError {
    #[error("home directory could not be resolved; secrets not saved")]
    HomeUnresolvable,
    #[error("failed to create secret store directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to encode secret store as JSON: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("failed to write secret store {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to set 0600 permissions on {path}: {source}")]
    Chmod {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Persist the secret store to `~/.ato/secrets.json` (#55, #57).
///
/// On unix the file is written with mode `0o600` (owner read/write only)
/// via `OpenOptions::mode` so a fresh write never goes through a
/// world-readable phase, plus an explicit `set_permissions` after for
/// defense in depth and for files that already exist with looser modes.
///
/// Errors are returned, not swallowed, so callers (`AppState::add_secret`
/// etc.) can surface failure to the UI instead of silently claiming
/// success while the secret was never persisted (#57).
pub fn save_secrets(store: &SecretStore) -> Result<(), SaveSecretsError> {
    let path = secrets_path().ok_or(SaveSecretsError::HomeUnresolvable)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| SaveSecretsError::CreateDir {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let json = serde_json::to_string_pretty(store)?;

    write_secret_file(&path, json.as_bytes())?;
    info!(path = %path.display(), "Saved secret store");
    Ok(())
}

#[cfg(unix)]
fn write_secret_file(path: &std::path::Path, bytes: &[u8]) -> Result<(), SaveSecretsError> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    // Mode 0o600 = owner-only read/write. `OpenOptions::mode` is honored
    // when the file is being CREATED; if the file already exists with a
    // looser mode we still need the explicit set_permissions below.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| SaveSecretsError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(bytes)
        .map_err(|source| SaveSecretsError::Write {
            path: path.to_path_buf(),
            source,
        })?;

    // Defense in depth: re-apply 0o600 even when the file pre-existed
    // with mode 0o644 (the bug this fixes — old secrets.json from before
    // this change carried world-readable perms).
    use std::os::unix::fs::PermissionsExt as _;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms).map_err(|source| SaveSecretsError::Chmod {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

#[cfg(not(unix))]
fn write_secret_file(path: &std::path::Path, bytes: &[u8]) -> Result<(), SaveSecretsError> {
    // Windows ACL-based permissioning is out of scope for v0.5.0 — the
    // file inherits its parent directory's ACL. The error type is the
    // same shape so callers don't branch on platform.
    std::fs::write(path, bytes).map_err(|source| SaveSecretsError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

// ── Capsule Config Store (non-secret) ─────────────────────────────────────────

/// Per-capsule plaintext configuration (model name, port, etc.).
///
/// Mirrors `SecretStore` for non-secret kinds — `String`, `Number`,
/// `Enum` from `ConfigField`. Two reasons we keep this separate from
/// the secret store rather than overloading `SecretStore`:
///
/// 1. **Threat model.** Secrets are write-only in the UI (masked
///    input, never re-displayed); non-secret values are read-write
///    and intentionally rendered back into the modal so the user can
///    see what they previously chose. Mixing them invites a bug
///    where a secret leaks into the read-back path.
/// 2. **Grant model.** Secrets require an explicit per-capsule grant
///    (`SecretStore.grants`) so a capsule can only read keys the
///    user has approved for it. Non-secret config has no such
///    isolation requirement — it lives next to the capsule that
///    asked for it. The shared map shape would force an unused
///    grant table on the non-secret path.
///
/// Persisted at `~/.ato/capsule-configs.json` as a flat JSON object.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CapsuleConfigStore {
    /// `handle` → (`name` → `value`). Empty maps are kept to make
    /// "this capsule has been configured before, just not for these
    /// keys" distinguishable from "never configured" — Day 6's UX
    /// may want to surface that distinction in the modal.
    #[serde(default)]
    pub configs: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
}

impl CapsuleConfigStore {
    /// Set (or overwrite) a single config value for a capsule.
    pub fn set_config(&mut self, capsule_handle: &str, key: String, value: String) {
        self.configs
            .entry(capsule_handle.to_string())
            .or_default()
            .insert(key, value);
    }

    /// Snapshot of all `KEY = value` pairs configured for `handle`.
    /// Returns an empty vec when the capsule has no recorded
    /// configuration yet — callers should treat the empty case as
    /// "let preflight tell us what's missing" rather than as an
    /// error.
    pub fn configs_for_capsule(&self, handle: &str) -> Vec<(String, String)> {
        match self.configs.get(handle) {
            Some(map) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            None => Vec::new(),
        }
    }

    /// Remove a single config entry. Used by future Day 7+ "Reset
    /// configuration" affordances; not wired into the modal yet.
    #[allow(dead_code)]
    pub fn clear_config(&mut self, capsule_handle: &str, key: &str) {
        if let Some(map) = self.configs.get_mut(capsule_handle) {
            map.remove(key);
        }
    }
}

fn capsule_configs_path() -> Option<PathBuf> {
    ato_path("capsule-configs.json").ok()
}

pub fn load_capsule_configs() -> CapsuleConfigStore {
    let Some(path) = capsule_configs_path() else {
        return CapsuleConfigStore::default();
    };

    match std::fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(store) => {
                info!(path = %path.display(), "Loaded capsule config store");
                store
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Failed to parse capsule config store, using empty");
                CapsuleConfigStore::default()
            }
        },
        Err(_) => CapsuleConfigStore::default(),
    }
}

pub fn save_capsule_configs(store: &CapsuleConfigStore) {
    let Some(path) = capsule_configs_path() else {
        warn!("Cannot determine home directory, capsule configs not saved");
        return;
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    match serde_json::to_string_pretty(store) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                warn!(path = %path.display(), error = %e, "Failed to write capsule config store");
            }
        }
        Err(e) => {
            warn!(error = %e, "Failed to serialize capsule config store");
        }
    }
}

// ── Capsule Policy Override Store ────────────────────────────────────────────

/// Per-capsule user overrides for security / execution boundary policy.
///
/// This store intentionally excludes non-policy capsule preferences. Those stay
/// in `CapsuleConfigStore`, while secret material stays in `SecretStore`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CapsulePolicyOverrideStore {
    #[serde(default)]
    pub overrides: HashMap<String, CapsulePolicyOverride>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CapsulePolicyOverride {
    #[serde(default)]
    pub network_kill_switch: Option<bool>,
    #[serde(default)]
    pub egress_allow: Vec<String>,
    #[serde(default)]
    pub readonly_paths: Vec<String>,
    #[serde(default)]
    pub readwrite_paths: Vec<String>,
    #[serde(default)]
    pub env_grants: Vec<String>,
    #[serde(default)]
    pub revoked_capabilities: Vec<String>,
}

impl CapsulePolicyOverrideStore {
    pub fn override_for(&self, handle: &str) -> CapsulePolicyOverride {
        self.overrides.get(handle).cloned().unwrap_or_default()
    }

    pub fn override_for_mut(&mut self, handle: &str) -> &mut CapsulePolicyOverride {
        self.overrides.entry(handle.to_string()).or_default()
    }

    pub fn reset(&mut self, handle: &str) {
        self.overrides.remove(handle);
    }
}

fn capsule_policy_overrides_path() -> Option<PathBuf> {
    ato_path("capsule-policy-overrides.json").ok()
}

pub fn load_capsule_policy_overrides() -> CapsulePolicyOverrideStore {
    let Some(path) = capsule_policy_overrides_path() else {
        return CapsulePolicyOverrideStore::default();
    };

    match std::fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(store) => {
                info!(path = %path.display(), "Loaded capsule policy override store");
                store
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, "Failed to parse capsule policy override store, using empty");
                CapsulePolicyOverrideStore::default()
            }
        },
        Err(_) => CapsulePolicyOverrideStore::default(),
    }
}

pub fn save_capsule_policy_overrides(store: &CapsulePolicyOverrideStore) {
    let Some(path) = capsule_policy_overrides_path() else {
        warn!("Cannot determine home directory, capsule policy overrides not saved");
        return;
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    match serde_json::to_string_pretty(store) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                warn!(path = %path.display(), error = %e, "Failed to write capsule policy override store");
            }
        }
        Err(e) => {
            warn!(error = %e, "Failed to serialize capsule policy override store");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_roundtrips() {
        let config = DesktopConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: DesktopConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.runtime.terminal_font_size, 14);
        assert_eq!(parsed.runtime.terminal_max_sessions, 4);
        assert!(!parsed.developer.auto_open_devtools);
        assert_eq!(parsed.general.theme, ThemeConfig::Dark);
    }

    #[test]
    fn legacy_partial_json_migrates_to_structured_config() {
        let json = r#"{"theme": "light"}"#;
        let parsed: DesktopConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.general.theme, ThemeConfig::Light);
        assert_eq!(parsed.runtime.terminal_font_size, 14);
        assert!(parsed.sandbox.default_egress_allow.is_empty());
    }

    #[test]
    fn legacy_flat_config_migrates_existing_settings() {
        let json = r#"{
            "theme": "light",
            "terminal_font_size": 16,
            "terminal_max_sessions": 8,
            "default_egress_allow": ["api.github.com"],
            "auto_open_devtools": true
        }"#;
        let parsed: DesktopConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.general.theme, ThemeConfig::Light);
        assert_eq!(parsed.runtime.terminal_font_size, 16);
        assert_eq!(parsed.runtime.terminal_max_sessions, 8);
        assert_eq!(
            parsed.sandbox.default_egress_policy,
            EgressPolicyMode::Allowlist
        );
        assert_eq!(parsed.sandbox.default_egress_allow, vec!["api.github.com"]);
        assert!(parsed.developer.auto_open_devtools);
    }

    #[test]
    fn capsule_policy_overrides_are_separate_from_capsule_config() {
        let mut configs = CapsuleConfigStore::default();
        configs.set_config("capsule.x", "MODEL".into(), "gpt-5".into());

        let mut policies = CapsulePolicyOverrideStore::default();
        policies
            .override_for_mut("capsule.x")
            .egress_allow
            .push("api.github.com".into());

        assert_eq!(
            configs.configs_for_capsule("capsule.x"),
            vec![("MODEL".to_string(), "gpt-5".to_string())]
        );
        assert_eq!(
            policies.override_for("capsule.x").egress_allow,
            vec!["api.github.com".to_string()]
        );
    }

    #[test]
    fn capsule_config_store_set_and_query_roundtrip() {
        let mut store = CapsuleConfigStore::default();
        store.set_config("capsule.byok-ai-chat", "MODEL".into(), "gpt-4".into());
        store.set_config("capsule.byok-ai-chat", "PORT".into(), "8080".into());
        store.set_config("capsule.other", "MODEL".into(), "claude".into());

        let mut byok = store.configs_for_capsule("capsule.byok-ai-chat");
        byok.sort();
        assert_eq!(
            byok,
            vec![
                ("MODEL".to_string(), "gpt-4".to_string()),
                ("PORT".to_string(), "8080".to_string()),
            ],
            "configs_for_capsule must isolate per-handle entries",
        );
        // Missing handle returns empty — never an error.
        assert!(store.configs_for_capsule("capsule.unknown").is_empty());

        // JSON round-trip preserves the nested shape.
        let json = serde_json::to_string(&store).unwrap();
        let parsed: CapsuleConfigStore = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.configs.len(), 2);
        assert_eq!(
            parsed
                .configs
                .get("capsule.byok-ai-chat")
                .unwrap()
                .get("MODEL"),
            Some(&"gpt-4".to_string())
        );
    }

    #[test]
    fn capsule_config_store_overwrites_same_key() {
        let mut store = CapsuleConfigStore::default();
        store.set_config("capsule.x", "MODEL".into(), "gpt-4".into());
        store.set_config("capsule.x", "MODEL".into(), "gpt-5".into());
        let configs = store.configs_for_capsule("capsule.x");
        assert_eq!(configs, vec![("MODEL".to_string(), "gpt-5".to_string())]);
    }

    #[test]
    fn desktop_settings_default_values() {
        let config = DesktopConfig::default();
        let d = &config.desktop;
        assert!(
            d.focus_view_enabled,
            "focus_view_enabled default must be true"
        );
        assert_eq!(d.startup_surface, StartupSurface::Start);
        assert_eq!(
            d.content_window_default_presentation,
            ContentWindowPresentation::Windowed
        );
        assert!(!d.restore_window_frames);
        assert!(d.control_bar.always_on_top);
        assert_eq!(d.control_bar.mode, ControlBarMode::Floating);
        assert!(d.control_bar.visible_on_startup);
        assert_eq!(d.control_bar.position, ControlBarPosition::Top);
        assert!(!d.control_bar.auto_hide);
        assert!(!d.onboarding.completed);
        assert!(!d.onboarding.skipped);
        assert_eq!(d.onboarding.version, 0);
    }

    #[test]
    fn desktop_settings_roundtrip_json() {
        let mut config = DesktopConfig::default();
        config.desktop.startup_surface = StartupSurface::RestoreLast;
        config.desktop.content_window_default_presentation = ContentWindowPresentation::Fullscreen;
        config.desktop.control_bar.position = ControlBarPosition::Bottom;
        config.desktop.control_bar.auto_hide = false;

        let json = serde_json::to_string(&config).unwrap();
        let parsed: DesktopConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.desktop.startup_surface, StartupSurface::RestoreLast);
        assert_eq!(
            parsed.desktop.content_window_default_presentation,
            ContentWindowPresentation::Fullscreen
        );
        assert_eq!(parsed.desktop.control_bar.mode, ControlBarMode::Floating);
        assert_eq!(
            parsed.desktop.control_bar.position,
            ControlBarPosition::Bottom
        );
        assert!(!parsed.desktop.control_bar.auto_hide);
    }

    #[test]
    fn control_bar_settings_legacy_auto_hide_maps_to_mode() {
        let json = r#"{"auto_hide": true, "visible_on_startup": true}"#;
        let parsed: ControlBarSettings = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.mode, ControlBarMode::AutoHide);
    }

    #[test]
    fn control_bar_settings_legacy_hidden_maps_to_mode() {
        let json = r#"{"visible_on_startup": false, "auto_hide": false}"#;
        let parsed: ControlBarSettings = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.mode, ControlBarMode::Hidden);
    }

    #[test]
    fn control_bar_settings_explicit_mode_wins_over_legacy_flags() {
        let json = r#"{"mode": "compact-pill", "visible_on_startup": false, "auto_hide": true}"#;
        let parsed: ControlBarSettings = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.mode, ControlBarMode::CompactPill);
    }

    #[test]
    fn config_without_desktop_section_migrates_to_default_desktop() {
        // Existing config files that pre-date the desktop section must
        // deserialise cleanly and produce default desktop settings.
        let json = r#"{"general": {"theme": "light"}}"#;
        let parsed: DesktopConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.general.theme, ThemeConfig::Light);
        assert!(
            parsed.desktop.focus_view_enabled,
            "missing desktop section must default to focus_view_enabled=true"
        );
        assert_eq!(parsed.desktop.startup_surface, StartupSurface::Start);
        assert!(parsed.desktop.control_bar.always_on_top);
        assert!(!parsed.desktop.onboarding.completed);
        assert!(!parsed.desktop.onboarding.skipped);
        assert_eq!(parsed.desktop.onboarding.version, 0);
    }

    #[test]
    fn desktop_section_without_onboarding_migrates_with_defaults() {
        let json = r#"{
            "desktop": {
                "startup_surface": "store",
                "focus_view_enabled": true
            }
        }"#;
        let parsed: DesktopConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.desktop.startup_surface, StartupSurface::Store);
        assert!(!parsed.desktop.onboarding.completed);
        assert!(!parsed.desktop.onboarding.skipped);
        assert_eq!(parsed.desktop.onboarding.version, 0);
    }
}
