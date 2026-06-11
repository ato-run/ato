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
    pub runtime_setup: RuntimeSetupSettings,
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
    /// Backend engine selection for source / OCI / Wasm capsules.
    /// Note: Podman is an OCI host dependency; Ato does not bundle it.
    /// PostgreSQL is NOT a backend engine — it is a per-capsule tool artifact
    /// fetched on-demand when a recipe/lock explicitly requires it.
    #[serde(default)]
    pub backend_engines: BackendEngineSettings,
    /// Whether Podman may be used as an OCI runtime provider. Default on
    /// (opt-out). When false, launch/preflight must not probe Podman,
    /// auto-start a Podman machine, or select Podman as the OCI provider;
    /// an OCI recipe that can only run on Podman surfaces an actionable
    /// "Podman disabled" error instead. Carried to the CLI via the
    /// `ATO_PODMAN_ENABLED` env var (interim Desktop → CLI carrier).
    #[serde(default = "default_podman_enabled")]
    pub podman_enabled: bool,
}

/// Host runtime-setup preferences (issue #420 revision).
///
/// Kept as its own config section (rather than folded into `runtime`) so the
/// distinction between "what Ato executes" (`runtime`) and "what Ato checks /
/// installs to make the host runnable" (`runtime_setup`) stays legible.
///
/// Policy: the language runtimes are Ato-managed-first — when a recipe needs
/// Node/uv/Python and the corresponding `*_install_enabled` toggle is on, Ato
/// installs its own managed copy rather than using a host PATH copy. Podman /
/// Docker are detection-only and have no toggle here (Podman usage is governed
/// by `runtime.podman_enabled`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeSetupSettings {
    /// Reserved opt-out for a future *startup-time* host-tool readiness check
    /// (run automatically when the app launches). Default on.
    ///
    /// It does NOT gate the on-demand Runtime Setup panels (onboarding Step 5
    /// and Settings → Runtime): those probe via `ato internal runtime
    /// setup-status` on explicit user action and always run. Nothing reads this
    /// field yet; the startup probe that will honour it is not implemented.
    #[serde(default = "default_true")]
    pub check_host_tools_on_startup: bool,
    /// Whether Ato may install an Ato-managed Node when a recipe needs it.
    #[serde(default = "default_true")]
    pub node_install_enabled: bool,
    /// Whether Ato may install an Ato-managed uv when a recipe needs it.
    #[serde(default = "default_true")]
    pub uv_install_enabled: bool,
    /// Whether Ato may install an Ato-managed Python when a recipe needs it.
    #[serde(default = "default_true")]
    pub python_install_enabled: bool,
}

/// Backend engine selection for the three capsule execution categories.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct BackendEngineSettings {
    /// Engine for source-execution capsules (e.g. nacelle).
    #[serde(default)]
    pub source: SourceBackendEngine,
    /// Engine for OCI capsules (e.g. podman).
    #[serde(default)]
    pub oci: OciBackendEngine,
    /// Engine for Wasm capsules (e.g. wasmtime).
    #[serde(default)]
    pub wasm: WasmBackendEngine,
}

/// Source execution engine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceBackendEngine {
    /// Nacelle sandboxed runtime (default, recommended).
    #[default]
    Nacelle,
    /// Host process fallback (advanced / unsafe).
    Host,
}

impl<'de> Deserialize<'de> for SourceBackendEngine {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d).unwrap_or_default();
        match s.as_str() {
            "nacelle" => Ok(Self::Nacelle),
            "host" => Ok(Self::Host),
            other => {
                warn!(
                    value = other,
                    "Unknown source backend engine in config; using default (nacelle)"
                );
                Ok(Self::default())
            }
        }
    }
}

/// OCI execution engine.
///
/// Podman is an OCI backend host dependency. Ato does not bundle or install
/// Podman automatically.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OciBackendEngine {
    /// Podman (default, recommended). Must be installed on the host.
    #[default]
    Podman,
    /// Docker-compatible daemon (experimental — not yet wired to runtime).
    Docker,
    /// Youki OCI runtime (experimental — not yet wired to runtime).
    Youki,
}

impl<'de> Deserialize<'de> for OciBackendEngine {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d).unwrap_or_default();
        match s.as_str() {
            "podman" => Ok(Self::Podman),
            "docker" => Ok(Self::Docker),
            "youki" => Ok(Self::Youki),
            other => {
                warn!(
                    value = other,
                    "Unknown OCI backend engine in config; using default (podman)"
                );
                Ok(Self::default())
            }
        }
    }
}

/// Wasm execution engine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WasmBackendEngine {
    /// Wasmtime (default, only supported option currently).
    #[default]
    Wasmtime,
}

impl<'de> Deserialize<'de> for WasmBackendEngine {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d).unwrap_or_default();
        match s.as_str() {
            "wasmtime" => Ok(Self::Wasmtime),
            other => {
                warn!(
                    value = other,
                    "Unknown Wasm backend engine in config; using default (wasmtime)"
                );
                Ok(Self::default())
            }
        }
    }
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
    /// Which surface is shown after the app starts.
    #[serde(default)]
    pub startup_surface: StartupSurface,
    /// Initial presentation mode for content windows opened by Focus View.
    #[serde(default)]
    pub content_window_default_presentation: ContentWindowPresentation,
    /// Where capsule handles should open by default.
    #[serde(default)]
    pub capsule_open_mode: CapsuleOpenMode,
    /// Whether to restore the last window frames (position/size) on launch.
    #[serde(default)]
    pub restore_window_frames: bool,
    /// One-time onboarding flow completion state.
    #[serde(default)]
    pub onboarding: OnboardingSettings,
    #[serde(default)]
    pub control_bar: ControlBarSettings,
    /// Controls whether closing a window stops the capsule process.
    #[serde(default)]
    pub window_close_behavior: WindowCloseBehavior,
    /// Capsule handles that the user has starred (pinned) in the Control Bar.
    /// Keys are stored as `capsule://{handle}` and sorted on save for diff stability.
    #[serde(default)]
    pub pinned_capsules: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct OnboardingSettings {
    #[serde(default)]
    pub completed: bool,
    #[serde(default)]
    pub skipped: bool,
    #[serde(default)]
    pub version: u16,
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
        let _ = raw.mode;

        Ok(Self {
            // Temporary safety gate: force a single stable mode while
            // non-floating behaviors are being debugged.
            mode: ControlBarMode::Floating,
            always_on_top: raw.always_on_top,
            visible_on_startup: true,
            position: raw.position,
            auto_hide: false,
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
pub enum CapsuleOpenMode {
    #[default]
    Window,
    Webviewer,
    OsBrowser,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum WindowCloseBehavior {
    #[default]
    KeepSessionRunning,
    StopSession,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ControlBarPosition {
    #[default]
    Top,
    Bottom,
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
            startup_surface: StartupSurface::Start,
            content_window_default_presentation: ContentWindowPresentation::Windowed,
            capsule_open_mode: CapsuleOpenMode::Window,
            restore_window_frames: false,
            onboarding: OnboardingSettings::default(),
            control_bar: ControlBarSettings::default(),
            pinned_capsules: Vec::new(),
            window_close_behavior: WindowCloseBehavior::KeepSessionRunning,
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

fn default_podman_enabled() -> bool {
    true
}

/// Default for the opt-out `runtime_setup.*` toggles — all on.
fn default_true() -> bool {
    true
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

pub(crate) fn default_local_registry_port() -> u16 {
    8080
}

/// GPUI global carrying the configured local-registry port so any system-capsule
/// IPC handler can use the same port as the read path without requiring config
/// I/O or a separate parameter.
///
/// Set by `start_window::open_start_window` at window construction time.
pub(crate) struct LocalRegistryPort(pub(crate) u16);
impl gpui::Global for LocalRegistryPort {}

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
            runtime_setup: RuntimeSetupSettings::default(),
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
            backend_engines: BackendEngineSettings::default(),
            podman_enabled: default_podman_enabled(),
        }
    }
}

impl Default for RuntimeSetupSettings {
    fn default() -> Self {
        Self {
            check_host_tools_on_startup: default_true(),
            node_install_enabled: default_true(),
            uv_install_enabled: default_true(),
            python_install_enabled: default_true(),
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
            runtime_setup: RuntimeSetupSettings,
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
            runtime_setup: helper.runtime_setup,
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
    pub value: String,
}

/// Secret storage backed by the CLI's age-encrypted store via bridge.
///
/// The `secrets` field holds metadata-only entries (values are empty
/// or truncated) for UI display. Actual secret values are resolved
/// on demand through `secrets_for_capsule` which calls the age backend.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SecretStore {
    #[serde(default)]
    pub secrets: Vec<SecretEntry>,
}

/// Error surfaced when a bridge operation fails.
pub(crate) use crate::secret_bridge::BridgeError;

impl SecretStore {
    pub fn canonicalize_handle(handle: &str) -> &str {
        let last_sep = handle.rfind('/');
        let search_start = last_sep.map_or(0, |p| p + 1);
        if let Some(pos) = handle[search_start..].find('@') {
            let abs_pos = search_start + pos;
            if abs_pos > 0 && abs_pos < handle.len() - 1 {
                return &handle[..abs_pos];
            }
        }
        handle
    }

    pub fn add_secret(&mut self, key: String, value: String) -> Result<(), BridgeError> {
        crate::secret_bridge::CliSecretBridge::set(&key, &value, None, None, None)?;
        if let Some(existing) = self.secrets.iter_mut().find(|s| s.key == key) {
            existing.value = String::new();
        } else {
            self.secrets.push(SecretEntry {
                key: key.clone(),
                value: String::new(),
            });
        }
        Ok(())
    }

    pub fn remove_secret(&mut self, key: &str) -> Result<(), BridgeError> {
        crate::secret_bridge::CliSecretBridge::delete(key, None)?;
        self.secrets.retain(|s| s.key != key);
        Ok(())
    }

    pub fn secrets_for_capsule(&self, handle: &str) -> Vec<SecretEntry> {
        let canonical = Self::canonicalize_handle(handle);
        match crate::secret_bridge::CliSecretBridge::resolve_for_capsule(canonical) {
            Ok(resolved) => resolved
                .into_iter()
                .map(|r| SecretEntry {
                    key: r.key,
                    value: r.value,
                })
                .collect(),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    handle = %canonical,
                    "Failed to resolve secrets for capsule"
                );
                Vec::new()
            }
        }
    }

    pub fn grant_secret(&mut self, capsule_handle: &str, key: &str) -> Result<(), BridgeError> {
        let canonical = Self::canonicalize_handle(capsule_handle).to_string();
        let mut allow = self.current_allow_list(key)?;
        if !allow.contains(&canonical) {
            allow.push(canonical);
        }
        crate::secret_bridge::CliSecretBridge::update_acl(key, Some(allow), None)
    }

    pub fn revoke_secret(&mut self, capsule_handle: &str, key: &str) -> Result<(), BridgeError> {
        let canonical = Self::canonicalize_handle(capsule_handle).to_string();
        let mut allow = self.current_allow_list(key)?;
        allow.retain(|h| h != &canonical);
        crate::secret_bridge::CliSecretBridge::update_acl(key, Some(allow), None)
    }

    fn current_allow_list(&self, key: &str) -> Result<Vec<String>, BridgeError> {
        let entries = crate::secret_bridge::CliSecretBridge::list()?;
        Ok(entries
            .into_iter()
            .find(|e| e.key == key)
            .and_then(|e| e.allow)
            .unwrap_or_default())
    }

    /// Rebuild `secret_grant_keys_by_handle` cache from the bridge.
    /// Inverts per-key allow lists into per-handle key lists.
    pub fn build_grant_keys_cache()
    -> Result<std::collections::HashMap<String, Vec<String>>, BridgeError> {
        let entries = crate::secret_bridge::CliSecretBridge::list()?;
        let mut cache: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for e in &entries {
            if let Some(ref allow) = e.allow {
                for handle in allow {
                    cache.entry(handle.clone()).or_default().push(e.key.clone());
                }
            }
        }
        Ok(cache)
    }
}

/// Return a display path for the age-based credential store.
pub fn secrets_path_display() -> Option<String> {
    let ato_home = ato_path("credentials/secrets/default.age").ok()?;
    if let Ok(home) = home_dir_path()
        && let Ok(rel) = ato_home.strip_prefix(&home)
    {
        return Some(format!("~/{}", rel.display()));
    }
    Some(ato_home.display().to_string())
}

fn home_dir_path() -> Result<PathBuf, ()> {
    dirs::home_dir().ok_or(())
}

/// Load secret metadata from the age store via bridge.
pub fn load_secrets() -> SecretStore {
    match crate::secret_bridge::CliSecretBridge::list() {
        Ok(entries) => {
            let secrets = entries
                .into_iter()
                .map(|e| SecretEntry {
                    key: e.key,
                    value: String::new(),
                })
                .collect();
            SecretStore { secrets }
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to load secrets via bridge, using empty store");
            SecretStore::default()
        }
    }
}

/// Migrate legacy `secrets.json` entries into the age store.
///
/// Reads old-style secrets + grants map, calls the bridge to store
/// each entry, then renames `secrets.json` → `secrets.json.bak`.
/// Safe to call repeatedly (re-imports are idempotent, values
/// overwrite). Returns the number of migrated secrets on success.
pub fn migrate_legacy_secrets_if_present() -> Option<usize> {
    let json_path = ato_path("secrets.json").ok()?;
    if !json_path.exists() {
        return None;
    }

    #[derive(Deserialize)]
    struct LegacySecret {
        key: String,
        value: String,
    }
    #[derive(Deserialize)]
    struct LegacyStore {
        #[serde(default)]
        secrets: Vec<LegacySecret>,
        #[serde(default)]
        grants: std::collections::HashMap<String, Vec<String>>,
    }

    let content = match std::fs::read_to_string(&json_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path = %json_path.display(), error = %e, "Cannot read legacy secrets.json, skipping migration");
            return None;
        }
    };
    let legacy: LegacyStore = match serde_json::from_str(&content) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(path = %json_path.display(), error = %e, "Failed to parse legacy secrets.json, skipping migration");
            return None;
        }
    };

    let mut migrated = 0usize;

    // Step 1: migrate secrets (key → value entries).
    for entry in &legacy.secrets {
        if let Err(e) =
            crate::secret_bridge::CliSecretBridge::set(&entry.key, &entry.value, None, None, None)
        {
            tracing::warn!(key = %entry.key, error = %e, "Migration: failed to set secret, aborting");
            return None;
        }
        migrated += 1;
    }

    // Step 2: invert grants map and set per-key allow lists.
    // Old: grants[handle] = [KEY_A, KEY_B]
    // New: entry(KEY_A).allow += [canonical(handle)]
    let mut per_key_allows: std::collections::HashMap<String, std::collections::HashSet<String>> =
        std::collections::HashMap::new();
    for (raw_handle, allowed_keys) in &legacy.grants {
        let canonical = SecretStore::canonicalize_handle(raw_handle).to_string();
        for key in allowed_keys {
            per_key_allows
                .entry(key.clone())
                .or_default()
                .insert(canonical.clone());
        }
    }
    for (key, allow_set) in per_key_allows {
        let allow: Vec<String> = allow_set.into_iter().collect();
        if let Err(e) = crate::secret_bridge::CliSecretBridge::update_acl(&key, Some(allow), None) {
            tracing::warn!(key = %key, error = %e, "Migration: failed to update ACL, aborting");
            return None;
        }
    }

    // Step 3: rename on full success.
    let bak_path = json_path.with_extension("json.bak");
    if let Err(e) = std::fs::rename(&json_path, &bak_path) {
        tracing::warn!(src = %json_path.display(), dst = %bak_path.display(), error = %e, "Migration: age write succeeded but rename of secrets.json failed — file still present at original path");
        return None;
    } else {
        tracing::info!(
            path = %bak_path.display(),
            count = migrated,
            "Migrated legacy secrets.json to age store and renamed to .bak"
        );
    }

    Some(migrated)
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
    fn runtime_setup_defaults_are_enabled() {
        let config = DesktopConfig::default();
        assert!(
            config.runtime.podman_enabled,
            "podman must default to enabled (opt-out)"
        );
        assert!(config.runtime_setup.check_host_tools_on_startup);
        assert!(config.runtime_setup.node_install_enabled);
        assert!(config.runtime_setup.uv_install_enabled);
        assert!(config.runtime_setup.python_install_enabled);
    }

    #[test]
    fn runtime_setup_roundtrips_disabled() {
        let mut config = DesktopConfig::default();
        config.runtime.podman_enabled = false;
        config.runtime_setup.node_install_enabled = false;
        config.runtime_setup.uv_install_enabled = false;
        let json = serde_json::to_string(&config).unwrap();
        let parsed: DesktopConfig = serde_json::from_str(&json).unwrap();
        assert!(!parsed.runtime.podman_enabled);
        assert!(!parsed.runtime_setup.node_install_enabled);
        assert!(!parsed.runtime_setup.uv_install_enabled);
        assert!(parsed.runtime_setup.python_install_enabled);
    }

    #[test]
    fn runtime_setup_missing_fields_default_to_enabled() {
        // Pre-existing configs without the new fields must load with all on.
        let json = r#"{"general": {"theme": "dark"}}"#;
        let parsed: DesktopConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.runtime.podman_enabled);
        assert!(parsed.runtime_setup.check_host_tools_on_startup);
        assert!(parsed.runtime_setup.node_install_enabled);
        assert!(parsed.runtime_setup.uv_install_enabled);
        assert!(parsed.runtime_setup.python_install_enabled);
    }

    #[test]
    fn legacy_privacy_section_is_ignored_gracefully() {
        // A config written by the earlier host-device-detection build carries
        // a `privacy` section; it must load (ignored) without error.
        let json = r#"{"privacy": {"host_device_detection_enabled": false}}"#;
        let parsed: DesktopConfig = serde_json::from_str(json).unwrap();
        assert!(parsed.runtime_setup.node_install_enabled);
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
        assert_eq!(d.startup_surface, StartupSurface::Start);
        assert_eq!(
            d.content_window_default_presentation,
            ContentWindowPresentation::Windowed
        );
        assert!(!d.restore_window_frames);
        assert_eq!(d.capsule_open_mode, CapsuleOpenMode::Window);
        assert!(d.control_bar.always_on_top);
        assert_eq!(d.control_bar.mode, ControlBarMode::Floating);
        assert!(d.control_bar.visible_on_startup);
        assert_eq!(d.control_bar.position, ControlBarPosition::Top);
        assert!(!d.control_bar.auto_hide);
        assert_eq!(
            d.window_close_behavior,
            WindowCloseBehavior::KeepSessionRunning
        );
        assert!(!d.onboarding.completed);
        assert!(!d.onboarding.skipped);
        assert_eq!(d.onboarding.version, 0);
        assert!(d.pinned_capsules.is_empty());
    }

    #[test]
    fn desktop_settings_roundtrip_json() {
        let mut config = DesktopConfig::default();
        config.desktop.startup_surface = StartupSurface::RestoreLast;
        config.desktop.content_window_default_presentation = ContentWindowPresentation::Fullscreen;
        config.desktop.control_bar.position = ControlBarPosition::Bottom;
        config.desktop.control_bar.auto_hide = false;
        config.desktop.capsule_open_mode = CapsuleOpenMode::Webviewer;

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
        assert_eq!(parsed.desktop.capsule_open_mode, CapsuleOpenMode::Webviewer);
        assert_eq!(
            parsed.desktop.window_close_behavior,
            WindowCloseBehavior::KeepSessionRunning
        );
    }

    #[test]
    fn control_bar_settings_legacy_auto_hide_maps_to_mode() {
        let json = r#"{"auto_hide": true, "visible_on_startup": true}"#;
        let parsed: ControlBarSettings = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.mode, ControlBarMode::Floating);
        assert!(parsed.visible_on_startup);
        assert!(!parsed.auto_hide);
    }

    #[test]
    fn control_bar_settings_legacy_hidden_maps_to_mode() {
        let json = r#"{"visible_on_startup": false, "auto_hide": false}"#;
        let parsed: ControlBarSettings = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.mode, ControlBarMode::Floating);
        assert!(parsed.visible_on_startup);
        assert!(!parsed.auto_hide);
    }

    #[test]
    fn control_bar_settings_explicit_mode_wins_over_legacy_flags() {
        let json = r#"{"mode": "compact-pill", "visible_on_startup": false, "auto_hide": true}"#;
        let parsed: ControlBarSettings = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.mode, ControlBarMode::Floating);
        assert!(parsed.visible_on_startup);
        assert!(!parsed.auto_hide);
    }

    #[test]
    fn config_without_desktop_section_migrates_to_default_desktop() {
        // Existing config files that pre-date the desktop section must
        // deserialise cleanly and produce default desktop settings.
        let json = r#"{"general": {"theme": "light"}}"#;
        let parsed: DesktopConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.general.theme, ThemeConfig::Light);
        assert_eq!(parsed.desktop.startup_surface, StartupSurface::Start);
        assert!(parsed.desktop.control_bar.always_on_top);
        assert!(!parsed.desktop.onboarding.completed);
        assert!(!parsed.desktop.onboarding.skipped);
        assert_eq!(parsed.desktop.onboarding.version, 0);
        assert_eq!(parsed.desktop.capsule_open_mode, CapsuleOpenMode::Window);
        assert_eq!(
            parsed.desktop.window_close_behavior,
            WindowCloseBehavior::KeepSessionRunning
        );
    }

    #[test]
    fn desktop_section_without_onboarding_migrates_with_defaults() {
        let json = r#"{
            "desktop": {
                "startup_surface": "store"
            }
        }"#;
        let parsed: DesktopConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.desktop.startup_surface, StartupSurface::Store);
        assert!(!parsed.desktop.onboarding.completed);
        assert!(!parsed.desktop.onboarding.skipped);
        assert_eq!(parsed.desktop.onboarding.version, 0);
    }

    #[test]
    fn capsule_open_mode_deserialize_webviewer() {
        let json = r#"{"desktop": {"capsule_open_mode": "webviewer"}}"#;
        let parsed: DesktopConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.desktop.capsule_open_mode, CapsuleOpenMode::Webviewer);
    }

    #[test]
    fn capsule_open_mode_deserialize_os_browser() {
        let json = r#"{"desktop": {"capsule_open_mode": "os-browser"}}"#;
        let parsed: DesktopConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.desktop.capsule_open_mode, CapsuleOpenMode::OsBrowser);
    }

    #[test]
    fn capsule_open_mode_missing_defaults_to_window() {
        let json = r#"{"desktop": {}}"#;
        let parsed: DesktopConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.desktop.capsule_open_mode, CapsuleOpenMode::Window);
    }

    #[test]
    fn capsule_open_mode_unknown_value_ignored() {
        let json = r#"{"desktop": {"capsule_open_mode": "unknown-mode"}}"#;
        let result: Result<DesktopConfig, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "unknown capsule open mode should be rejected by serde"
        );
    }

    #[test]
    fn window_close_behavior_default_is_keep_session_running() {
        let config = DesktopConfig::default();
        assert_eq!(
            config.desktop.window_close_behavior,
            WindowCloseBehavior::KeepSessionRunning
        );
    }

    #[test]
    fn window_close_behavior_deserialize_stop_session() {
        let json = r#"{"desktop": {"window_close_behavior": "stop-session"}}"#;
        let parsed: DesktopConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.desktop.window_close_behavior,
            WindowCloseBehavior::StopSession
        );
    }

    #[test]
    fn window_close_behavior_missing_defaults_to_keep_session_running() {
        let json = r#"{"desktop": {}}"#;
        let parsed: DesktopConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.desktop.window_close_behavior,
            WindowCloseBehavior::KeepSessionRunning
        );
    }

    #[test]
    fn window_close_behavior_roundtrip() {
        let mut config = DesktopConfig::default();
        config.desktop.window_close_behavior = WindowCloseBehavior::StopSession;
        let json = serde_json::to_string(&config).unwrap();
        let parsed: DesktopConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.desktop.window_close_behavior,
            WindowCloseBehavior::StopSession
        );
    }

    // ── canonicalize_handle (#56) ────────────────────────────────

    // ── backend engine config (#329) ──────────────────────────────

    #[test]
    fn backend_engine_defaults_are_nacelle_podman_wasmtime() {
        let config = DesktopConfig::default();
        assert_eq!(
            config.runtime.backend_engines.source,
            SourceBackendEngine::Nacelle
        );
        assert_eq!(config.runtime.backend_engines.oci, OciBackendEngine::Podman);
        assert_eq!(
            config.runtime.backend_engines.wasm,
            WasmBackendEngine::Wasmtime
        );
    }

    #[test]
    fn backend_engine_roundtrip_json() {
        let mut config = DesktopConfig::default();
        config.runtime.backend_engines.source = SourceBackendEngine::Host;
        config.runtime.backend_engines.oci = OciBackendEngine::Docker;
        let json = serde_json::to_string(&config).unwrap();
        let parsed: DesktopConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.runtime.backend_engines.source,
            SourceBackendEngine::Host
        );
        assert_eq!(parsed.runtime.backend_engines.oci, OciBackendEngine::Docker);
        assert_eq!(
            parsed.runtime.backend_engines.wasm,
            WasmBackendEngine::Wasmtime
        );
    }

    #[test]
    fn backend_engine_missing_field_defaults() {
        // Pre-existing configs without backend_engines must load cleanly.
        let json = r#"{"general": {"theme": "dark"}}"#;
        let parsed: DesktopConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.runtime.backend_engines.source,
            SourceBackendEngine::Nacelle
        );
        assert_eq!(parsed.runtime.backend_engines.oci, OciBackendEngine::Podman);
        assert_eq!(
            parsed.runtime.backend_engines.wasm,
            WasmBackendEngine::Wasmtime
        );
    }

    #[test]
    fn backend_engine_unknown_value_warns_and_defaults() {
        // Unknown engine values must not cause a parse error; they fall back to
        // the default and emit a tracing::warn! (lenient-on-load policy).
        let json = r#"{
            "runtime": {
                "backend_engines": {
                    "source": "unknown-engine",
                    "oci": "unsupported-runtime",
                    "wasm": "bad-value"
                }
            }
        }"#;
        let parsed: DesktopConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            parsed.runtime.backend_engines.source,
            SourceBackendEngine::Nacelle,
            "unknown source engine must fall back to nacelle"
        );
        assert_eq!(
            parsed.runtime.backend_engines.oci,
            OciBackendEngine::Podman,
            "unknown OCI engine must fall back to podman"
        );
        assert_eq!(
            parsed.runtime.backend_engines.wasm,
            WasmBackendEngine::Wasmtime,
            "unknown Wasm engine must fall back to wasmtime"
        );
    }

    #[test]
    fn backend_engine_snapshot_contains_backend_engines() {
        use crate::settings::settings_snapshot_from_config;
        let config = DesktopConfig::default();
        let snapshot = settings_snapshot_from_config(&config);
        let engines = snapshot
            .get("resolved")
            .and_then(|r| r.get("runtime"))
            .and_then(|r| r.get("backendEngines"))
            .expect("resolved.runtime.backendEngines must exist in snapshot");
        assert!(
            engines.get("source").is_some(),
            "snapshot must include source engine"
        );
        assert!(
            engines.get("oci").is_some(),
            "snapshot must include oci engine"
        );
        assert!(
            engines.get("wasm").is_some(),
            "snapshot must include wasm engine"
        );
    }

    // ── canonicalize_handle (#56) ────────────────────────────────

    #[test]
    fn canonicalize_strips_at_version_from_last_path_segment() {
        assert_eq!(
            SecretStore::canonicalize_handle("capsule://ato.run/koh0920/app@0.3.4"),
            "capsule://ato.run/koh0920/app"
        );
    }

    #[test]
    fn canonicalize_preserves_bare_handle() {
        assert_eq!(
            SecretStore::canonicalize_handle("capsule://github.com/Koh0920/WasedaP2P"),
            "capsule://github.com/Koh0920/WasedaP2P"
        );
    }

    #[test]
    fn canonicalize_preserves_at_in_authority() {
        assert_eq!(
            SecretStore::canonicalize_handle("git@github.com:owner/repo"),
            "git@github.com:owner/repo"
        );
    }

    #[test]
    fn canonicalize_preserves_simple_handle() {
        assert_eq!(
            SecretStore::canonicalize_handle("capsule://handle"),
            "capsule://handle"
        );
        assert_eq!(
            SecretStore::canonicalize_handle("capsule://handle@1.0"),
            "capsule://handle"
        );
    }

    #[test]
    fn migration_grants_inversion_output_canonical_keys() {
        let legacy_grants: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::from([
                (
                    "capsule://org/app@1.2.3".into(),
                    vec!["API_KEY".into(), "OTHER_KEY".into()],
                ),
                ("capsule://org/app".into(), vec!["API_KEY".into()]),
            ]);
        let canonical = SecretStore::canonicalize_handle;
        let mut per_key_allows: std::collections::HashMap<
            String,
            std::collections::HashSet<String>,
        > = std::collections::HashMap::new();
        for (raw_handle, allowed_keys) in &legacy_grants {
            let ch = canonical(raw_handle).to_string();
            for key in allowed_keys {
                per_key_allows
                    .entry(key.clone())
                    .or_default()
                    .insert(ch.clone());
            }
        }
        let api_key_allows = per_key_allows.get("API_KEY").unwrap();
        assert!(api_key_allows.contains("capsule://org/app"));
        assert_eq!(
            api_key_allows.len(),
            1,
            "versioned grant must merge to canonical"
        );
        let other_allows = per_key_allows.get("OTHER_KEY").unwrap();
        assert!(other_allows.contains("capsule://org/app"));
    }
}
