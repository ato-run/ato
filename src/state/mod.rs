use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::path::PathBuf;

use capsule_core::handle::{
    classify_surface_input, normalize_capsule_handle, parse_host_route, HandleInput,
    InputSurface as CapsuleInputSurface, SurfaceInput as CapsuleSurfaceInput,
};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};
use url::{form_urlencoded, Url};

use crate::bridge::ShellEvent;
use crate::orchestrator::{register_pending_cli_command, CliLaunchSpec};

pub type WorkspaceId = usize;
pub type TaskSetId = usize;
pub type PaneId = usize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellMode {
    Focus,
    Overview,
    CommandBar,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeMode {
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PaneTree {
    Leaf(PaneId),
    Split {
        axis: SplitAxis,
        ratio: f32,
        first: Box<PaneTree>,
        second: Box<PaneTree>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityGrant {
    ReadFile,
    WorkspaceInfo,
    OpenExternal,
    ClipboardRead,
    Terminal,
    /// Grants AI-agent automation (JS injection + AutomationHost socket).
    Automation,
}

impl CapabilityGrant {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReadFile => "read-file",
            Self::WorkspaceInfo => "workspace-info",
            Self::OpenExternal => "open-external",
            Self::ClipboardRead => "clipboard-read",
            Self::Terminal => "terminal",
            Self::Automation => "automation",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "read-file" => Some(Self::ReadFile),
            "workspace-info" => Some(Self::WorkspaceInfo),
            "open-external" => Some(Self::OpenExternal),
            "clipboard-read" => Some(Self::ClipboardRead),
            "terminal" => Some(Self::Terminal),
            "automation" => Some(Self::Automation),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuestRoute {
    Capsule {
        session: String,
        entry_path: String,
    },
    ExternalUrl(Url),
    CapsuleHandle {
        handle: String,
        label: String,
    },
    CapsuleUrl {
        handle: String,
        label: String,
        url: Url,
    },
    /// Interactive PTY terminal session served via terminal:// custom protocol
    Terminal {
        session_id: String,
    },
}

impl GuestRoute {
    pub fn label(&self) -> String {
        match self {
            Self::Capsule { session, .. } => format!("capsule://{session}/index.html"),
            Self::ExternalUrl(url) => url.as_str().to_string(),
            Self::CapsuleHandle { label, .. } => label.clone(),
            Self::CapsuleUrl { label, .. } => label.clone(),
            Self::Terminal { session_id } => format!("terminal://{session_id}/"),
        }
    }
}

impl fmt::Display for GuestRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebSessionState {
    Detached,
    Resolving,
    Materializing,
    Launching,
    Mounted,
    Closed,
    /// Launch was attempted but failed permanently (e.g. broken workspace, spawn error).
    /// Unlike `Closed`, this state prevents automatic re-queuing on every render frame.
    /// It is cleared back to `Launching` when the user explicitly re-navigates.
    LaunchFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapsuleLogStage {
    Resolve,
    Materialize,
    Launch,
    Permission,
    Runtime,
}

impl CapsuleLogStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resolve => "resolve",
            Self::Materialize => "materialize",
            Self::Launch => "launch",
            Self::Permission => "permission",
            Self::Runtime => "runtime",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneBounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl PaneBounds {
    pub fn empty() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneRole {
    Primary,
    Companion,
    Agent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMode {
    EmbedAllowed,
    BrowserPreferred,
    BrowserRequired,
    FirstPartyNative, // treated as BrowserRequired for now
    Deny,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopAuthStatus {
    SignedOut,
    AwaitingBrowser,
    SignedIn,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingPostLoginTarget {
    CloudDock,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopAuthState {
    pub status: DesktopAuthStatus,
    pub publisher_handle: Option<String>,
    pub last_login_origin: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LauncherAction {
    OpenLocalRegistry,
    OpenCloudDock,
    SignInToAtoRun,
}

#[derive(Clone, Debug)]
pub struct AuthPolicy {
    pub origin_contains: String,
    pub path_prefix: Option<String>,
    pub mode: AuthMode,
}

#[derive(Clone, Debug)]
pub struct AuthPolicyRegistry {
    pub policies: Vec<AuthPolicy>,
    pub default_mode: AuthMode,
}

impl AuthPolicyRegistry {
    pub fn default_third_party() -> Self {
        Self {
            default_mode: AuthMode::BrowserPreferred,
            policies: vec![
                AuthPolicy {
                    origin_contains: "ato.run".into(),
                    path_prefix: Some("/auth".into()),
                    mode: AuthMode::FirstPartyNative,
                },
                // Google OAuth
                AuthPolicy {
                    origin_contains: "accounts.google.com".into(),
                    path_prefix: None,
                    mode: AuthMode::BrowserRequired,
                },
                // GitHub
                AuthPolicy {
                    origin_contains: "github.com".into(),
                    path_prefix: Some("/login".into()),
                    mode: AuthMode::BrowserRequired,
                },
                AuthPolicy {
                    origin_contains: "github.com".into(),
                    path_prefix: Some("/session".into()),
                    mode: AuthMode::BrowserRequired,
                },
                // Microsoft
                AuthPolicy {
                    origin_contains: "login.microsoftonline.com".into(),
                    path_prefix: None,
                    mode: AuthMode::BrowserRequired,
                },
                AuthPolicy {
                    origin_contains: "login.live.com".into(),
                    path_prefix: None,
                    mode: AuthMode::BrowserRequired,
                },
                // Generic OAuth paths
                AuthPolicy {
                    origin_contains: "".into(),
                    path_prefix: Some("/oauth/".into()),
                    mode: AuthMode::BrowserRequired,
                },
                AuthPolicy {
                    origin_contains: "".into(),
                    path_prefix: Some("/oauth2/".into()),
                    mode: AuthMode::BrowserRequired,
                },
                AuthPolicy {
                    origin_contains: "".into(),
                    path_prefix: Some("/authorize".into()),
                    mode: AuthMode::BrowserRequired,
                },
                AuthPolicy {
                    origin_contains: "".into(),
                    path_prefix: Some("/sso/".into()),
                    mode: AuthMode::BrowserRequired,
                },
            ],
        }
    }

    pub fn classify(&self, url: &str) -> AuthMode {
        for policy in &self.policies {
            let host_match =
                policy.origin_contains.is_empty() || url.contains(&policy.origin_contains);
            let path_match = policy
                .path_prefix
                .as_ref()
                .map(|p| url.contains(p.as_str()))
                .unwrap_or(true);
            if host_match && path_match {
                return policy.mode;
            }
        }
        self.default_mode
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthSessionStatus {
    Created,
    OpenedInBrowser,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct AuthSession {
    pub session_id: String,
    pub originating_pane_id: PaneId,
    pub auth_mode: AuthMode,
    pub origin: String,
    pub start_url: String,
    pub status: AuthSessionStatus,
    pub created_at: std::time::SystemTime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneSurface {
    Web(WebPane),
    Native {
        body: String,
    },
    CapsuleStatus(CapsuleStatusPane),
    Inspector,
    DevConsole,
    Launcher,
    AuthHandoff {
        session_id: String,
        origin: String,
        original_surface: Box<PaneSurface>,
    },
    /// Interactive PTY terminal backed by nacelle
    Terminal(TerminalPane),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalPane {
    /// Unique session ID assigned by nacelle
    pub session_id: String,
    /// Capsule handle (e.g. "myapp") — used for display title
    pub capsule_handle: String,
    /// Terminal width in columns
    pub cols: u16,
    /// Terminal height in rows
    pub rows: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebPane {
    pub route: GuestRoute,
    pub partition_id: String,
    pub session: WebSessionState,
    pub capabilities: Vec<CapabilityGrant>,
    pub profile: String,
    pub source_label: Option<String>,
    pub trust_state: Option<String>,
    pub restricted: bool,
    pub snapshot_label: Option<String>,
    pub canonical_handle: Option<String>,
    pub session_id: Option<String>,
    pub adapter: Option<String>,
    pub manifest_path: Option<String>,
    pub runtime_label: Option<String>,
    pub display_strategy: Option<String>,
    pub log_path: Option<String>,
    pub local_url: Option<String>,
    pub healthcheck_url: Option<String>,
    pub invoke_url: Option<String>,
    pub served_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapsuleStatusPane {
    pub route: GuestRoute,
    pub session: WebSessionState,
    pub source_label: Option<String>,
    pub trust_state: Option<String>,
    pub restricted: bool,
    pub snapshot_label: Option<String>,
    pub canonical_handle: Option<String>,
    pub session_id: Option<String>,
    pub adapter: Option<String>,
    pub manifest_path: Option<String>,
    pub runtime_label: Option<String>,
    pub display_strategy: Option<String>,
    pub log_path: Option<String>,
    pub local_url: Option<String>,
    pub healthcheck_url: Option<String>,
    pub invoke_url: Option<String>,
    pub served_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pane {
    pub id: PaneId,
    pub title: String,
    pub role: PaneRole,
    pub visible: bool,
    pub bounds: PaneBounds,
    pub surface: PaneSurface,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TaskSet {
    pub id: TaskSetId,
    pub title: String,
    pub focused_pane: PaneId,
    pub pane_tree: PaneTree,
    pub panes: Vec<Pane>,
    pub split_ratio: f32,
    pub route_candidates: Vec<GuestRoute>,
    pub route_index: usize,
    pub preview: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub title: String,
    pub active_task: TaskSetId,
    pub tasks: Vec<TaskSet>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SystemPageIcon {
    Console,
    Terminal,
    Launcher,
    Inspector,
    CapsuleStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarTaskIconSpec {
    Monogram(String),
    ExternalUrl { origin: String },
    SystemIcon(SystemPageIcon),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarTaskItem {
    pub id: TaskSetId,
    pub title: String,
    pub is_active: bool,
    pub icon: SidebarTaskIconSpec,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OmnibarSuggestionAction {
    Navigate { url: String },
    SelectTask { task_id: TaskSetId },
    ShowSettings,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OmnibarSuggestion {
    pub title: String,
    pub detail: String,
    pub action: OmnibarSuggestionAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActivityTone {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityEntry {
    pub tone: ActivityTone,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapsuleLogEntry {
    pub stage: CapsuleLogStage,
    pub tone: ActivityTone,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsoleLevel {
    Log,
    Info,
    Warn,
    Error,
    Debug,
}

impl ConsoleLevel {
    pub fn from_str(s: &str) -> Self {
        match s {
            "info" => Self::Info,
            "warn" => Self::Warn,
            "error" => Self::Error,
            "debug" => Self::Debug,
            _ => Self::Log,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Log => "log",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Debug => "debug",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsoleLogEntry {
    pub pane_id: PaneId,
    pub level: ConsoleLevel,
    pub message: String,
    pub source_label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkLogEntry {
    pub request_id: String,
    pub pane_id: PaneId,
    pub method: String,
    pub url: String,
    pub status: Option<u16>,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowserCommandKind {
    Back,
    Forward,
    Reload,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserCommand {
    pub pane_id: PaneId,
    pub kind: BrowserCommandKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionPrompt {
    pub pane_id: PaneId,
    pub route_label: String,
    pub capability: String,
    pub command: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActiveWebPane {
    pub workspace_id: WorkspaceId,
    pub task_id: TaskSetId,
    pub pane_id: PaneId,
    pub title: String,
    pub route: GuestRoute,
    pub partition_id: String,
    pub profile: String,
    pub capabilities: Vec<CapabilityGrant>,
    pub session: WebSessionState,
    pub source_label: Option<String>,
    pub trust_state: Option<String>,
    pub restricted: bool,
    pub snapshot_label: Option<String>,
    pub canonical_handle: Option<String>,
    pub session_id: Option<String>,
    pub adapter: Option<String>,
    pub manifest_path: Option<String>,
    pub runtime_label: Option<String>,
    pub display_strategy: Option<String>,
    pub log_path: Option<String>,
    pub local_url: Option<String>,
    pub healthcheck_url: Option<String>,
    pub invoke_url: Option<String>,
    pub served_by: Option<String>,
    pub bounds: PaneBounds,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActiveCapsulePane {
    pub pane_id: PaneId,
    pub title: String,
    pub route: GuestRoute,
    pub session: WebSessionState,
    pub source_label: Option<String>,
    pub trust_state: Option<String>,
    pub restricted: bool,
    pub snapshot_label: Option<String>,
    pub canonical_handle: Option<String>,
    pub session_id: Option<String>,
    pub adapter: Option<String>,
    pub manifest_path: Option<String>,
    pub runtime_label: Option<String>,
    pub display_strategy: Option<String>,
    pub log_path: Option<String>,
    pub local_url: Option<String>,
    pub healthcheck_url: Option<String>,
    pub invoke_url: Option<String>,
    pub served_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapsuleInspectorView {
    pub pane_id: PaneId,
    pub title: String,
    pub handle: String,
    pub canonical_handle: Option<String>,
    pub source_label: Option<String>,
    pub trust_state: Option<String>,
    pub restricted: bool,
    pub snapshot_label: Option<String>,
    pub session_state: WebSessionState,
    pub session_id: Option<String>,
    pub adapter: Option<String>,
    pub manifest_path: Option<String>,
    pub runtime_label: Option<String>,
    pub display_strategy: Option<String>,
    pub log_path: Option<String>,
    pub local_url: Option<String>,
    pub healthcheck_url: Option<String>,
    pub invoke_url: Option<String>,
    pub served_by: Option<String>,
    pub logs: Vec<CapsuleLogEntry>,
}

#[derive(Clone, Debug)]
pub struct AppState {
    pub shell_mode: ShellMode,
    pub active_workspace: WorkspaceId,
    pub workspaces: Vec<Workspace>,
    pub command_bar_text: String,
    pub activity: Vec<ActivityEntry>,
    pub capsule_logs: HashMap<PaneId, Vec<CapsuleLogEntry>>,
    pub browser_commands: VecDeque<BrowserCommand>,
    pub pending_permission_prompt: Option<PermissionPrompt>,
    pub theme_mode: ThemeMode,
    pub desktop_auth: DesktopAuthState,
    pub pending_post_login_target: Option<PendingPostLoginTarget>,
    pub auth_sessions: Vec<AuthSession>,
    pub auth_policy_registry: AuthPolicyRegistry,
    pub console_logs: Vec<ConsoleLogEntry>,
    pub network_logs: Vec<NetworkLogEntry>,
    pub config: crate::config::DesktopConfig,
    next_task_id: TaskSetId,
    next_pane_id: PaneId,
    next_new_tab_index: usize,
}

impl AppState {
    pub fn demo() -> Self {
        // The demo graph intentionally mixes local capsules, a bundled welcome page, and remote URLs
        // so the shell exercises every rendering path on boot.
        let local_tauri = GuestRoute::CapsuleHandle {
            handle: demo_local_capsule("desky-real-tauri"),
            label: demo_local_capsule("desky-real-tauri"),
        };
        let local_electron = GuestRoute::CapsuleHandle {
            handle: demo_local_capsule("desky-real-electron"),
            label: demo_local_capsule("desky-real-electron"),
        };
        let local_wails = GuestRoute::CapsuleHandle {
            handle: demo_local_capsule("desky-real-wails"),
            label: demo_local_capsule("desky-real-wails"),
        };
        let welcome = GuestRoute::Capsule {
            session: "welcome".to_string(),
            entry_path: "/index.html".to_string(),
        };
        let store = GuestRoute::ExternalUrl(Url::parse("https://ato.run").expect("valid url"));
        let wry = GuestRoute::ExternalUrl(
            Url::parse("https://github.com/tauri-apps/wry").expect("valid url"),
        );

        let launcher_task = TaskSet {
            id: 1,
            title: "New Tab".to_string(),
            focused_pane: 1,
            pane_tree: PaneTree::Leaf(1),
            panes: vec![Pane {
                id: 1,
                title: "New Tab".to_string(),
                role: PaneRole::Primary,
                visible: true,
                bounds: PaneBounds::empty(),
                surface: PaneSurface::Launcher,
            }],
            split_ratio: 0.5,
            route_candidates: vec![],
            route_index: 0,
            preview: "Launchpad".to_string(),
        };

        let welcome_task = TaskSet {
            id: 2,
            title: "Guest surfaces".to_string(),
            focused_pane: 2,
            pane_tree: PaneTree::Leaf(2),
            panes: vec![Pane {
                id: 2,
                title: local_tauri.to_string(),
                role: PaneRole::Primary,
                visible: true,
                bounds: PaneBounds::empty(),
                surface: PaneSurface::Web(WebPane {
                    route: local_tauri.clone(),
                    partition_id: "local-real-tauri".to_string(),
                    session: WebSessionState::Launching,
                    capabilities: vec![CapabilityGrant::ReadFile, CapabilityGrant::WorkspaceInfo],
                    profile: route_profile(&local_tauri).to_string(),
                    source_label: Some("local".to_string()),
                    trust_state: Some("local".to_string()),
                    restricted: true,
                    snapshot_label: None,
                    canonical_handle: Some(local_tauri.to_string()),
                    session_id: None,
                    adapter: None,
                    manifest_path: None,
                    runtime_label: None,
                    display_strategy: None,
                    log_path: None,
                    local_url: None,
                    healthcheck_url: None,
                    invoke_url: None,
                    served_by: None,
                }),
            }],
            split_ratio: 0.68,
            route_candidates: vec![
                local_tauri,
                local_electron,
                local_wails,
                welcome,
                store.clone(),
                wry.clone(),
            ],
            route_index: 0,
            preview: "ato-cli resolve/session start across tauri/electron/wails guests".to_string(),
        };

        let store_task = TaskSet {
            id: 3,
            title: "Ato".to_string(),
            focused_pane: 3,
            pane_tree: PaneTree::Leaf(3),
            panes: vec![Pane {
                id: 3,
                title: store.to_string(),
                role: PaneRole::Primary,
                visible: true,
                bounds: PaneBounds::empty(),
                surface: PaneSurface::Web(WebPane {
                    route: store.clone(),
                    partition_id: "store".to_string(),
                    session: WebSessionState::Launching,
                    capabilities: vec![CapabilityGrant::OpenExternal],
                    profile: "electron".to_string(),
                    source_label: Some("web".to_string()),
                    trust_state: None,
                    restricted: false,
                    snapshot_label: None,
                    canonical_handle: None,
                    session_id: None,
                    adapter: None,
                    manifest_path: None,
                    runtime_label: None,
                    display_strategy: None,
                    log_path: None,
                    local_url: None,
                    healthcheck_url: None,
                    invoke_url: None,
                    served_by: None,
                }),
            }],
            split_ratio: 0.68,
            route_candidates: vec![store, wry],
            route_index: 0,
            preview: "ato.run landing page".to_string(),
        };

        let mut state = Self {
            shell_mode: ShellMode::Focus,
            active_workspace: 1,
            workspaces: vec![Workspace {
                id: 1,
                title: "Rust host".to_string(),
                active_task: 3,
                tasks: vec![launcher_task, welcome_task, store_task],
            }],
            command_bar_text: "https://ato.run/".to_string(),
            activity: vec![ActivityEntry {
                tone: ActivityTone::Info,
                message: "Phase 3 shell bootstrapped with ato-cli guest orchestration".to_string(),
            }],
            capsule_logs: HashMap::new(),
            browser_commands: VecDeque::new(),
            pending_permission_prompt: None,
            theme_mode: ThemeMode::Light, // synced below from config
            desktop_auth: DesktopAuthState {
                status: DesktopAuthStatus::SignedOut,
                publisher_handle: None,
                last_login_origin: None,
            },
            pending_post_login_target: None,
            auth_sessions: Vec::new(),
            auth_policy_registry: AuthPolicyRegistry::default_third_party(),
            console_logs: Vec::new(),
            network_logs: Vec::new(),
            config: crate::config::load_config(),
            next_task_id: 4,
            next_pane_id: 4,
            next_new_tab_index: 2,
        };
        state.sync_theme_from_config();
        state
    }

    pub fn toggle_theme(&mut self) {
        self.theme_mode = match self.theme_mode {
            ThemeMode::Light => ThemeMode::Dark,
            ThemeMode::Dark => ThemeMode::Light,
        };
        self.config.theme = match self.theme_mode {
            ThemeMode::Light => crate::config::ThemeConfig::Light,
            ThemeMode::Dark => crate::config::ThemeConfig::Dark,
        };
        crate::config::save_config(&self.config);
    }

    /// Sync theme_mode from the persisted config.
    fn sync_theme_from_config(&mut self) {
        self.theme_mode = match self.config.theme {
            crate::config::ThemeConfig::Light => ThemeMode::Light,
            crate::config::ThemeConfig::Dark => ThemeMode::Dark,
        };
    }

    /// Update a config value and persist to disk.
    pub fn update_config(&mut self, f: impl FnOnce(&mut crate::config::DesktopConfig)) {
        f(&mut self.config);
        crate::config::save_config(&self.config);
    }

    pub fn focus_command_bar(&mut self) {
        self.shell_mode = ShellMode::CommandBar;
    }

    pub fn omnibar_suggestions(&self, input: &str) -> Vec<OmnibarSuggestion> {
        let trimmed = input.trim();
        let query = trimmed.to_lowercase();
        let mut suggestions = Vec::new();

        if !trimmed.is_empty() {
            suggestions.push(OmnibarSuggestion {
                title: Self::normalize_input(trimmed),
                detail: "Open URL or search".to_string(),
                action: OmnibarSuggestionAction::Navigate {
                    url: trimmed.to_string(),
                },
            });
        }

        if query.is_empty() || "settings".contains(&query) || "preferences".contains(&query) {
            suggestions.push(OmnibarSuggestion {
                title: "Settings".to_string(),
                detail: "Open desktop settings".to_string(),
                action: OmnibarSuggestionAction::ShowSettings,
            });
        }

        if let Some(workspace) = self.active_workspace() {
            for task in &workspace.tasks {
                let detail = task_route_label(task);
                let title_matches = query.is_empty() || task.title.to_lowercase().contains(&query);
                let detail_matches = query.is_empty() || detail.to_lowercase().contains(&query);
                if !(title_matches || detail_matches) {
                    continue;
                }

                suggestions.push(OmnibarSuggestion {
                    title: task.title.clone(),
                    detail,
                    action: OmnibarSuggestionAction::SelectTask { task_id: task.id },
                });
            }
        }

        suggestions.truncate(6);
        suggestions
    }

    pub fn launcher_actions(&self) -> Vec<LauncherAction> {
        let mut actions = vec![
            LauncherAction::OpenLocalRegistry,
            LauncherAction::OpenCloudDock,
        ];
        if self.desktop_auth.publisher_handle.is_none() {
            actions.push(LauncherAction::SignInToAtoRun);
        }
        actions
    }

    pub fn toggle_overview(&mut self) {
        self.shell_mode = match self.shell_mode {
            ShellMode::Overview => ShellMode::Focus,
            _ => ShellMode::Overview,
        };
    }

    pub fn dismiss_transient(&mut self) {
        self.pending_permission_prompt = None;
        match self.shell_mode {
            ShellMode::CommandBar | ShellMode::Overview => self.shell_mode = ShellMode::Focus,
            ShellMode::Focus => {}
        }
    }

    pub fn create_new_tab(&mut self) {
        let next_task_id = self.next_task_id;
        let next_pane_id = self.next_pane_id;
        let title = if self.next_new_tab_index == 1 {
            "New Tab".to_string()
        } else {
            format!("New Tab {}", self.next_new_tab_index)
        };

        let mut created = false;
        if let Some(workspace) = self.active_workspace_mut() {
            workspace.tasks.push(TaskSet {
                id: next_task_id,
                title: title.clone(),
                focused_pane: next_pane_id,
                pane_tree: PaneTree::Leaf(next_pane_id),
                panes: vec![Pane {
                    id: next_pane_id,
                    title: title.clone(),
                    role: PaneRole::Primary,
                    visible: true,
                    bounds: PaneBounds::empty(),
                    surface: PaneSurface::Launcher,
                }],
                split_ratio: 0.5,
                route_candidates: vec![],
                route_index: 0,
                preview: "Launchpad".to_string(),
            });
            workspace.active_task = next_task_id;
            created = true;
        }

        if created {
            self.next_task_id += 1;
            self.next_pane_id += 1;
            self.next_new_tab_index += 1;
            self.command_bar_text.clear();
            self.push_activity(ActivityTone::Info, format!("Opened {title}"));
        }
    }

    pub fn next_workspace(&mut self) {
        self.advance_workspace(1);
    }

    pub fn previous_workspace(&mut self) {
        self.advance_workspace(-1);
    }

    pub fn next_task(&mut self) {
        self.advance_task(1);
    }

    pub fn previous_task(&mut self) {
        self.advance_task(-1);
    }

    pub fn select_task(&mut self, task_id: TaskSetId) {
        let mut selected_title = None;
        if let Some(workspace) = self.active_workspace_mut() {
            if let Some(task) = workspace.tasks.iter().find(|task| task.id == task_id) {
                workspace.active_task = task.id;
                selected_title = Some(task.title.clone());
            }
        }

        if let Some(title) = selected_title {
            self.sync_command_bar_with_active_route();
            self.push_activity(ActivityTone::Info, format!("Switched task to {title}"));
        }
    }

    /// Open each dropped path in a new tab by routing it through `navigate_to_url`.
    /// Paths that are local capsule directories go through `ato app session start`;
    /// paths without a capsule.toml will surface an error in the activity log.
    pub fn launch_dropped_paths(&mut self, paths: Vec<PathBuf>) {
        for path in paths {
            let path_str = path.display().to_string();
            self.create_new_tab();
            self.navigate_to_url(&path_str);
        }
    }

    pub fn navigate_to_url(&mut self, input: &str) {
        let normalized = Self::normalize_input(input);
        info!(input, normalized = %normalized, "navigate_to_url");

        // Share URL fast-path: route share URLs into the unified ato://cli
        // REPL panel. The REPL auto-executes `<share-url>` as its prelude
        // (echoed at the `ato>` prompt), so users get a single terminal
        // experience with egress policy, `.allow`, Ctrl-C, and bare-slug
        // dispatch. Web-type shares still resolve through this path —
        // their `ato run` invocation prints the local URL and exits back
        // to the prompt (Phase 5 will add a browser-pane hint).
        if crate::orchestrator::is_share_url(&normalized) {
            let share_id = normalized
                .rsplit('/')
                .find(|seg| !seg.is_empty())
                .unwrap_or("share")
                .to_string();
            let short_id = share_id.chars().take(8).collect::<String>();
            let host = url::Url::parse(&normalized)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned));
            // Seed both the share host (e.g. `ato.run`) and its wildcard
            // subdomain pattern (`*.ato.run`). The second entry is crucial:
            // `ato run` fetches share metadata from the API host
            // (`api.ato.run` / `staging.api.ato.run`), which is a distinct
            // host from the share URL's own host. Without the wildcard the
            // egress proxy blocks that fetch and `ato run` exits with
            // "Failed to fetch share URL".
            let mut initial_allow_hosts = Vec::new();
            if let Some(h) = host {
                if !h.is_empty() {
                    initial_allow_hosts.push(h.clone());
                    // Don't add wildcard for localhost / bare IP — HostPattern::parse
                    // would reject "*.localhost" and the exact form already suffices.
                    let is_ip = h.parse::<std::net::IpAddr>().is_ok();
                    let is_localhost = h.eq_ignore_ascii_case("localhost");
                    if !is_ip && !is_localhost {
                        initial_allow_hosts.push(format!("*.{h}"));
                    }
                }
            }
            info!(
                share_url = %normalized,
                share_id = %share_id,
                allow_hosts = ?initial_allow_hosts,
                "share URL detected — opening unified ato://cli REPL panel"
            );
            let spec = CliLaunchSpec::AtoRunRepl {
                prelude: Some(normalized.clone()),
                initial_allow_hosts,
            };
            self.open_cli_panel_with_spec(spec, Some(format!("share:{short_id}")));
            return;
        }

        let (next_route, capabilities, profile, source_label, trust_state, restricted, session) =
            if crate::orchestrator::is_share_url(&normalized) {
                // Share URLs must be routed as CapsuleHandle so the orchestrator can
                // materialise them via `ato decap` before starting the session.
                // classify_surface_input would classify them as WebUrl (external browser).
                let share_id = normalized
                    .rsplit('/')
                    .find(|seg| !seg.is_empty())
                    .unwrap_or("share")
                    .to_string();
                info!(share_url = %normalized, share_id = %share_id, "detected share URL — routing via decap");
                (
                    GuestRoute::CapsuleHandle {
                        handle: normalized.clone(),
                        label: format!("share:{share_id}"),
                    },
                    vec![
                        CapabilityGrant::ReadFile,
                        CapabilityGrant::WorkspaceInfo,
                        CapabilityGrant::Automation,
                    ],
                    "tauri".to_string(),
                    Some("share".to_string()),
                    Some("untrusted".to_string()),
                    true,
                    WebSessionState::Resolving,
                )
            } else {
                match classify_surface_input(HandleInput {
                    raw: normalized.clone(),
                    surface: CapsuleInputSurface::DesktopOmnibar,
                }) {
                    Ok(CapsuleSurfaceInput::Capsule { canonical }) => {
                        let label = canonical.display_string();
                        (
                            GuestRoute::CapsuleHandle {
                                handle: label.clone(),
                                label,
                            },
                            vec![CapabilityGrant::ReadFile, CapabilityGrant::WorkspaceInfo],
                            route_profile_for_source(canonical.source_label()).to_string(),
                            Some(canonical.source_label().to_string()),
                            Some(if canonical.source_label() == "local" {
                                "local".to_string()
                            } else {
                                "untrusted".to_string()
                            }),
                            true,
                            WebSessionState::Resolving,
                        )
                    }
                    Ok(CapsuleSurfaceInput::HostRoute { route: _ }) => {
                        // Route ato:// URLs entered via the omnibar / MCP
                        // browser_navigate through the same deep-link dispatcher
                        // used for OS-level URL handlers. This enables, e.g.,
                        // `ato://cli` to open the interactive CLI panel.
                        self.handle_host_route(&normalized);
                        return;
                    }
                    Ok(CapsuleSurfaceInput::WebUrl { url }) => {
                        let Ok(url) = Url::parse(&url) else {
                            self.push_activity(
                                ActivityTone::Error,
                                format!("Unable to navigate to invalid URL: {input}"),
                            );
                            return;
                        };
                        (
                            GuestRoute::ExternalUrl(url),
                            vec![CapabilityGrant::OpenExternal],
                            "electron".to_string(),
                            Some("web".to_string()),
                            None,
                            false,
                            WebSessionState::Launching,
                        )
                    }
                    Ok(CapsuleSurfaceInput::SearchQuery { query }) => {
                        let fallback = Self::search_fallback(&query);
                        let Ok(url) = Url::parse(&fallback) else {
                            self.push_activity(
                                ActivityTone::Error,
                                format!("Unable to navigate to invalid URL: {input}"),
                            );
                            return;
                        };
                        (
                            GuestRoute::ExternalUrl(url),
                            vec![CapabilityGrant::OpenExternal],
                            "electron".to_string(),
                            Some("web".to_string()),
                            None,
                            false,
                            WebSessionState::Launching,
                        )
                    }
                    Err(error) => {
                        self.push_activity(ActivityTone::Error, error.to_string());
                        return;
                    }
                }
            }; // end if is_share_url else match
        let label = next_route.to_string();
        debug!(route = %label, "navigate_to_url resolved route");
        let partition_id = sanitize(&label);
        let mut navigated = None;

        if let Some(task) = self.active_task_mut() {
            if let Some(pane) = task.focused_pane_mut() {
                let pane_id = pane.id;
                pane.title = label.clone();
                pane.surface = PaneSurface::Web(WebPane {
                    route: next_route.clone(),
                    partition_id,
                    session,
                    capabilities,
                    profile,
                    source_label,
                    trust_state,
                    restricted,
                    snapshot_label: None,
                    canonical_handle: match &next_route {
                        GuestRoute::CapsuleHandle { handle, .. } => Some(handle.clone()),
                        GuestRoute::CapsuleUrl { handle, .. } => Some(handle.clone()),
                        _ => None,
                    },
                    session_id: None,
                    adapter: None,
                    manifest_path: None,
                    runtime_label: None,
                    display_strategy: None,
                    log_path: None,
                    local_url: None,
                    healthcheck_url: None,
                    invoke_url: None,
                    served_by: None,
                });
                navigated = Some(pane_id);
            }
        }

        let Some(pane_id) = navigated else {
            self.push_activity(
                ActivityTone::Error,
                "No focused pane available for navigation",
            );
            return;
        };

        self.command_bar_text = label.clone();
        self.shell_mode = ShellMode::Focus;
        self.push_activity(ActivityTone::Info, format!("Navigating to {label}"));
        if matches!(next_route, GuestRoute::CapsuleHandle { .. }) {
            self.capsule_logs.remove(&pane_id);
            self.push_capsule_log(
                pane_id,
                CapsuleLogStage::Resolve,
                ActivityTone::Info,
                format!("Queued capsule launch for {label}"),
            );
        } else {
            self.capsule_logs.remove(&pane_id);
        }
    }

    pub fn open_local_registry(&mut self) {
        self.navigate_to_url(local_registry_url());
    }

    pub fn open_cloud_dock(&mut self) {
        if let Some(handle) = self.desktop_auth.publisher_handle.as_deref() {
            self.desktop_auth.status = DesktopAuthStatus::SignedIn;
            self.navigate_to_url(&cloud_dock_url(Some(handle)));
            return;
        }

        if matches!(self.desktop_auth.status, DesktopAuthStatus::SignedIn) {
            self.navigate_to_url(&cloud_dock_url(None));
            return;
        }

        self.begin_ato_login(PendingPostLoginTarget::CloudDock);
    }

    pub fn begin_ato_login(&mut self, target: PendingPostLoginTarget) {
        let Some(pane_id) = self
            .active_task()
            .and_then(|task| task.focused_pane())
            .map(|pane| pane.id)
        else {
            self.push_activity(
                ActivityTone::Error,
                "No focused pane available for ato.run login",
            );
            return;
        };

        let start_url = ato_login_url(target);
        let session_id = format!("auth-{}", uuid_v4_simple());
        self.auth_sessions.push(AuthSession {
            session_id: session_id.clone(),
            originating_pane_id: pane_id,
            auth_mode: AuthMode::FirstPartyNative,
            origin: "ato.run".to_string(),
            start_url: start_url.clone(),
            status: AuthSessionStatus::Created,
            created_at: std::time::SystemTime::now(),
        });
        self.desktop_auth.status = DesktopAuthStatus::AwaitingBrowser;
        self.desktop_auth.last_login_origin = Some("ato.run".to_string());
        self.pending_post_login_target = Some(target);
        self.command_bar_text = start_url;

        self.update_pane(pane_id, |pane| {
            let original_surface = std::mem::replace(&mut pane.surface, PaneSurface::Launcher);
            pane.title = "Sign in to ato.run".to_string();
            pane.surface = PaneSurface::AuthHandoff {
                session_id: session_id.clone(),
                origin: "ato.run".to_string(),
                original_surface: Box::new(original_surface),
            };
        });

        self.push_activity(
            ActivityTone::Info,
            "Continuing ato.run sign-in in your browser",
        );
    }

    pub fn handle_host_route(&mut self, raw_route: &str) {
        // ato://open?handle=<percent-encoded-capsule-handle>
        // Lets external callers (browser, share menu, CLI) open a capsule in the desktop.
        if raw_route.starts_with("ato://open") {
            if let Ok(url) = Url::parse(raw_route) {
                if let Some(handle) = url
                    .query_pairs()
                    .find(|(k, _)| k == "handle")
                    .map(|(_, v)| v.into_owned())
                {
                    self.push_activity(
                        ActivityTone::Info,
                        format!("Opening capsule from deep link: {handle}"),
                    );
                    self.create_new_tab();
                    self.navigate_to_url(&handle);
                    return;
                }
            }
            self.push_activity(
                ActivityTone::Warning,
                "ato://open deep link is missing the 'handle' query parameter",
            );
            return;
        }

        // ato://cli[?cmd=<shell|ato|...>]
        // Opens a bare interactive terminal panel. By default every input line
        // is routed through `ato run` so dependencies are auto-resolved by the
        // CLI. `?cmd=bash` spawns a raw shell under nacelle; `?cmd=ato` runs
        // the `ato` binary directly.
        if raw_route.starts_with("ato://cli") {
            let cmd = Url::parse(raw_route).ok().and_then(|url| {
                url.query_pairs()
                    .find(|(k, _)| k == "cmd")
                    .map(|(_, v)| v.into_owned())
            });
            self.open_cli_panel(cmd);
            return;
        }

        let Ok(route) = parse_host_route(raw_route) else {
            self.push_activity(
                ActivityTone::Warning,
                format!("Ignored invalid host route {raw_route}"),
            );
            return;
        };

        if route.namespace != "auth"
            || route.path_segments.first().map(String::as_str) != Some("callback")
        {
            self.push_activity(
                ActivityTone::Info,
                format!(
                    "Host route {} is reserved for desktop callbacks",
                    route.namespace
                ),
            );
            return;
        }

        let callback_kind = route.path_segments.get(1).map(String::as_str);
        match callback_kind {
            Some("cloud-dock") | Some("authenticated") => {
                self.complete_ato_login(None);
            }
            Some("dock") => {
                let handle = route.path_segments.get(2).cloned();
                self.complete_ato_login(handle);
            }
            Some("error") => {
                self.fail_ato_login();
            }
            _ => {
                self.push_activity(
                    ActivityTone::Warning,
                    "Ignored unsupported auth callback route",
                );
            }
        }
    }

    pub fn show_settings_panel(&mut self) {
        let next_task_id = self.next_task_id;
        let next_pane_id = self.next_pane_id;
        let settings_index = self
            .active_workspace()
            .map(|workspace| {
                workspace
                    .tasks
                    .iter()
                    .filter(|task| task.title.starts_with("Settings"))
                    .count()
                    + 1
            })
            .unwrap_or(1);
        let title = if settings_index == 1 {
            "Settings".to_string()
        } else {
            format!("Settings {settings_index}")
        };

        let mut created = false;
        if let Some(workspace) = self.active_workspace_mut() {
            workspace.tasks.push(TaskSet {
                id: next_task_id,
                title: title.clone(),
                focused_pane: next_pane_id,
                pane_tree: PaneTree::Leaf(next_pane_id),
                panes: vec![Pane {
                    id: next_pane_id,
                    title: title.clone(),
                    role: PaneRole::Primary,
                    visible: true,
                    bounds: PaneBounds::empty(),
                    surface: PaneSurface::Native {
                        body: "Desktop settings and diagnostics will appear here.".to_string(),
                    },
                }],
                split_ratio: 0.5,
                route_candidates: vec![],
                route_index: 0,
                preview: "Desktop settings".to_string(),
            });
            workspace.active_task = next_task_id;
            created = true;
        }

        if !created {
            self.push_activity(
                ActivityTone::Error,
                "No focused pane available for settings",
            );
            return;
        }

        self.next_task_id += 1;
        self.next_pane_id += 1;
        self.sync_command_bar_with_active_route();
        self.shell_mode = ShellMode::Focus;
        self.push_activity(ActivityTone::Info, format!("Opened {title}"));
    }

    pub fn toggle_dev_console(&mut self) {
        let has_dev_console = self
            .active_task()
            .map(|task| {
                task.panes
                    .iter()
                    .any(|p| matches!(p.surface, PaneSurface::DevConsole))
            })
            .unwrap_or(false);

        if has_dev_console {
            if let Some(task) = self.active_task_mut() {
                task.panes
                    .retain(|p| !matches!(p.surface, PaneSurface::DevConsole));
                task.pane_tree = PaneTree::Leaf(task.focused_pane);
            }
            self.push_activity(ActivityTone::Info, "Closed developer console");
        } else {
            let next_id = self.next_pane_id;
            if let Some(task) = self.active_task_mut() {
                task.panes.push(Pane {
                    id: next_id,
                    title: "Developer console".to_string(),
                    role: PaneRole::Companion,
                    visible: true,
                    bounds: PaneBounds::empty(),
                    surface: PaneSurface::DevConsole,
                });
                task.pane_tree = PaneTree::Split {
                    axis: SplitAxis::Vertical,
                    ratio: task.split_ratio,
                    first: Box::new(PaneTree::Leaf(task.focused_pane)),
                    second: Box::new(PaneTree::Leaf(next_id)),
                };
            }
            self.next_pane_id += 1;
            self.push_activity(ActivityTone::Info, "Opened developer console");
        }
        self.shell_mode = ShellMode::Focus;
    }

    /// Remove the GPUI DevConsole companion pane if it is present, without opening a new one.
    pub fn dismiss_dev_console(&mut self) {
        if let Some(task) = self.active_task_mut() {
            if task
                .panes
                .iter()
                .any(|p| matches!(p.surface, PaneSurface::DevConsole))
            {
                task.panes
                    .retain(|p| !matches!(p.surface, PaneSurface::DevConsole));
                task.pane_tree = PaneTree::Leaf(task.focused_pane);
            }
        }
    }

    pub fn browser_back(&mut self) {
        self.enqueue_browser_command(BrowserCommandKind::Back);
    }

    pub fn browser_forward(&mut self) {
        self.enqueue_browser_command(BrowserCommandKind::Forward);
    }

    pub fn browser_reload(&mut self) {
        self.enqueue_browser_command(BrowserCommandKind::Reload);
    }

    pub fn drain_browser_commands(&mut self, pane_id: PaneId) -> Vec<BrowserCommandKind> {
        let mut drained = Vec::new();
        let mut remaining = VecDeque::new();

        while let Some(command) = self.browser_commands.pop_front() {
            if command.pane_id == pane_id {
                drained.push(command.kind);
            } else {
                remaining.push_back(command);
            }
        }

        self.browser_commands = remaining;
        drained
    }

    pub fn apply_shell_events(&mut self, events: Vec<ShellEvent>) {
        for event in events {
            match event {
                ShellEvent::SessionReady { pane_id } => {
                    self.sync_web_session_state(pane_id, WebSessionState::Mounted);
                    self.push_capsule_log(
                        pane_id,
                        CapsuleLogStage::Launch,
                        ActivityTone::Info,
                        "Capsule frontend mounted",
                    );
                }
                ShellEvent::PermissionDenied {
                    pane_id,
                    capability,
                    command,
                } => {
                    let route_label = self
                        .pane_route_label(pane_id)
                        .unwrap_or_else(|| format!("pane-{pane_id}"));
                    self.pending_permission_prompt = Some(PermissionPrompt {
                        pane_id,
                        route_label: route_label.clone(),
                        capability: capability.clone(),
                        command: command.clone(),
                    });
                    self.push_activity(
                        ActivityTone::Warning,
                        format!("Pane {pane_id} denied capability {capability} for {route_label}"),
                    );
                    self.push_capsule_log(
                        pane_id,
                        CapsuleLogStage::Permission,
                        ActivityTone::Warning,
                        format!(
                            "Capability {capability} was denied for {route_label}{}",
                            command
                                .as_deref()
                                .map(|value| format!(" via {value}"))
                                .unwrap_or_default()
                        ),
                    );
                }
                ShellEvent::SessionClosed { pane_id } => {
                    self.sync_web_session_state(pane_id, WebSessionState::Closed);
                    self.push_capsule_log(
                        pane_id,
                        CapsuleLogStage::Launch,
                        ActivityTone::Warning,
                        "Capsule session closed",
                    );
                }
                ShellEvent::UrlChanged { pane_id, url } => {
                    let Ok(parsed) = Url::parse(&url) else {
                        continue;
                    };
                    let active_pane = self.active_web_pane().map(|pane| pane.pane_id);
                    self.update_pane(pane_id, |pane| {
                        if let PaneSurface::Web(web) = &mut pane.surface {
                            pane.title = url.clone();
                            if matches!(web.route, GuestRoute::CapsuleUrl { .. }) {
                                web.session = WebSessionState::Mounted;
                            } else {
                                web.route = GuestRoute::ExternalUrl(parsed.clone());
                                web.partition_id = sanitize(&url);
                                web.source_label = Some("web".to_string());
                                web.trust_state = None;
                                web.restricted = false;
                                web.snapshot_label = None;
                                web.canonical_handle = None;
                                web.session_id = None;
                                web.adapter = None;
                                web.manifest_path = None;
                                web.runtime_label = None;
                                web.display_strategy = None;
                                web.log_path = None;
                                web.local_url = None;
                                web.healthcheck_url = None;
                                web.invoke_url = None;
                                web.served_by = None;
                            }
                            web.session = WebSessionState::Mounted;
                        }
                    });
                    if active_pane == Some(pane_id) {
                        if !matches!(
                            self.active_capsule_pane().map(|pane| pane.route),
                            Some(GuestRoute::CapsuleUrl { .. })
                        ) {
                            self.command_bar_text = url;
                        }
                    }
                }
                ShellEvent::TitleChanged { pane_id, title } => {
                    self.update_pane(pane_id, |pane| {
                        pane.title = title.clone();
                    });
                }
                ShellEvent::GuestConsoleLog {
                    pane_id,
                    level,
                    message,
                } => {
                    let source_label = self.pane_source_label(pane_id);
                    self.console_logs.push(ConsoleLogEntry {
                        pane_id,
                        level: ConsoleLevel::from_str(&level),
                        message,
                        source_label,
                    });
                    if self.console_logs.len() > 5000 {
                        self.console_logs.drain(0..500);
                    }
                }
                ShellEvent::GuestNetworkStart {
                    pane_id,
                    request_id,
                    method,
                    url,
                } => {
                    self.network_logs.push(NetworkLogEntry {
                        request_id,
                        pane_id,
                        method,
                        url,
                        status: None,
                        duration_ms: None,
                    });
                    if self.network_logs.len() > 2000 {
                        self.network_logs.drain(0..200);
                    }
                }
                ShellEvent::GuestNetworkEnd {
                    pane_id: _,
                    request_id,
                    status,
                    duration_ms,
                } => {
                    if let Some(entry) = self
                        .network_logs
                        .iter_mut()
                        .rev()
                        .find(|e| e.request_id == request_id)
                    {
                        entry.status = Some(status);
                        entry.duration_ms = Some(duration_ms);
                    }
                }
                // TerminalInput and TerminalResize are consumed upstream by the WebViewManager
                // (which has access to nacelle stdin writers) before apply_shell_events is called.
                // They should not appear here; silently ignore any that leak through.
                ShellEvent::TerminalInput { .. } | ShellEvent::TerminalResize { .. } => {}
            }
        }
    }

    pub fn cycle_handle(&mut self) {
        let update = if let Some(task) = self.active_task_mut() {
            if task.route_candidates.is_empty() {
                return;
            }
            // Rotating the handle must update the pane metadata so the webview manager can rebuild it.
            task.route_index = (task.route_index + 1) % task.route_candidates.len();
            let next_route = task.route_candidates[task.route_index].clone();
            let label = next_route.to_string();
            task.preview = format!("Switched handle to {label}");
            if let Some(pane) = task.focused_pane_mut() {
                pane.title = label.clone();
                if let PaneSurface::Web(web) = &mut pane.surface {
                    web.profile = route_profile(&next_route).to_string();
                    web.route = next_route;
                    web.partition_id = sanitize(&label);
                    web.session = WebSessionState::Launching;
                    web.source_label = match &web.route {
                        GuestRoute::CapsuleHandle { handle, .. } => {
                            Some(route_source_label(handle))
                        }
                        GuestRoute::CapsuleUrl { handle, .. } => Some(route_source_label(handle)),
                        GuestRoute::Capsule { .. } => Some("embedded".to_string()),
                        GuestRoute::ExternalUrl(_) => Some("web".to_string()),
                        GuestRoute::Terminal { .. } => Some("terminal".to_string()),
                    };
                    web.trust_state = match &web.route {
                        GuestRoute::CapsuleHandle { handle, .. } if handle.contains("samples") => {
                            Some("local".to_string())
                        }
                        GuestRoute::CapsuleUrl { .. } => web.trust_state.clone(),
                        GuestRoute::CapsuleHandle { .. } => Some("untrusted".to_string()),
                        _ => None,
                    };
                    web.restricted = !matches!(&web.route, GuestRoute::ExternalUrl(_));
                    web.snapshot_label = None;
                }
            }
            Some((label, task.preview.clone()))
        } else {
            None
        };

        if let Some((label, preview)) = update {
            self.command_bar_text = label;
            self.push_activity(ActivityTone::Info, preview);
        }
    }

    pub fn split_pane(&mut self) {
        let message = if let Some(task) = self.active_task_mut() {
            if task.panes.len() == 1 {
                let next_id = task.panes[0].id + 1;
                // The companion pane is deliberately native so diagnostics stay separate from guest content.
                task.panes.push(Pane {
                    id: next_id,
                    title: "Capsule inspector".to_string(),
                    role: PaneRole::Companion,
                    visible: true,
                    bounds: PaneBounds::empty(),
                    surface: PaneSurface::Inspector,
                });
                task.pane_tree = PaneTree::Split {
                    axis: SplitAxis::Vertical,
                    ratio: task.split_ratio,
                    first: Box::new(PaneTree::Leaf(task.focused_pane)),
                    second: Box::new(PaneTree::Leaf(next_id)),
                };
                "Attached companion native pane".to_string()
            } else {
                task.panes.truncate(1);
                task.pane_tree = PaneTree::Leaf(task.focused_pane);
                "Removed companion pane".to_string()
            }
        } else {
            return;
        };
        self.push_activity(ActivityTone::Info, message);
    }

    pub fn expand_split(&mut self) {
        self.adjust_split(0.04);
    }

    pub fn shrink_split(&mut self) {
        self.adjust_split(-0.04);
    }

    pub fn set_active_bounds(&mut self, bounds: PaneBounds) {
        if let Some(task) = self.active_task_mut() {
            let split = task.panes.len() > 1;
            let companion_width = if split {
                bounds.width * (1.0 - task.split_ratio)
            } else {
                0.0
            };
            let primary_width = if split {
                (bounds.width - companion_width).max(0.0)
            } else {
                bounds.width
            };
            // Keep the primary pane anchored to the left and let the companion consume the remainder.
            for pane in &mut task.panes {
                match pane.role {
                    PaneRole::Primary => {
                        pane.bounds = PaneBounds {
                            x: bounds.x,
                            y: bounds.y,
                            width: primary_width,
                            height: bounds.height,
                        };
                    }
                    PaneRole::Companion => {
                        pane.bounds = PaneBounds {
                            x: bounds.x + primary_width,
                            y: bounds.y,
                            width: companion_width,
                            height: bounds.height,
                        };
                    }
                    PaneRole::Agent => {}
                }
            }
        }
    }

    pub fn active_web_pane(&self) -> Option<ActiveWebPane> {
        let workspace = self.active_workspace()?;
        let task = workspace.active_task()?;
        let pane = task.focused_pane()?;
        match &pane.surface {
            PaneSurface::Web(web) => Some(ActiveWebPane {
                workspace_id: workspace.id,
                task_id: task.id,
                pane_id: pane.id,
                title: pane.title.clone(),
                route: web.route.clone(),
                partition_id: web.partition_id.clone(),
                profile: web.profile.clone(),
                capabilities: web.capabilities.clone(),
                session: web.session.clone(),
                source_label: web.source_label.clone(),
                trust_state: web.trust_state.clone(),
                restricted: web.restricted,
                snapshot_label: web.snapshot_label.clone(),
                canonical_handle: web.canonical_handle.clone(),
                session_id: web.session_id.clone(),
                adapter: web.adapter.clone(),
                manifest_path: web.manifest_path.clone(),
                runtime_label: web.runtime_label.clone(),
                display_strategy: web.display_strategy.clone(),
                log_path: web.log_path.clone(),
                local_url: web.local_url.clone(),
                healthcheck_url: web.healthcheck_url.clone(),
                invoke_url: web.invoke_url.clone(),
                served_by: web.served_by.clone(),
                bounds: pane.bounds,
            }),
            PaneSurface::Native { .. }
            | PaneSurface::CapsuleStatus(_)
            | PaneSurface::DevConsole
            | PaneSurface::Inspector
            | PaneSurface::Launcher
            | PaneSurface::AuthHandoff { .. } => None,
            PaneSurface::Terminal(terminal) => Some(ActiveWebPane {
                workspace_id: workspace.id,
                task_id: task.id,
                pane_id: pane.id,
                title: pane.title.clone(),
                route: GuestRoute::Terminal {
                    session_id: terminal.session_id.clone(),
                },
                partition_id: terminal.session_id.clone(),
                profile: "terminal".to_string(),
                capabilities: vec![CapabilityGrant::Terminal, CapabilityGrant::Automation],
                session: WebSessionState::Launching,
                source_label: None,
                trust_state: None,
                restricted: false,
                snapshot_label: None,
                canonical_handle: None,
                session_id: Some(terminal.session_id.clone()),
                adapter: None,
                manifest_path: None,
                runtime_label: None,
                display_strategy: None,
                log_path: None,
                local_url: None,
                healthcheck_url: None,
                invoke_url: None,
                served_by: None,
                bounds: pane.bounds,
            }),
        }
    }

    pub fn active_capsule_pane(&self) -> Option<ActiveCapsulePane> {
        let workspace = self.active_workspace()?;
        let task = workspace.active_task()?;
        let pane = task.focused_pane()?;
        match &pane.surface {
            PaneSurface::Web(web)
                if matches!(
                    web.route,
                    GuestRoute::CapsuleHandle { .. }
                        | GuestRoute::Capsule { .. }
                        | GuestRoute::CapsuleUrl { .. }
                ) =>
            {
                Some(ActiveCapsulePane {
                    pane_id: pane.id,
                    title: pane.title.clone(),
                    route: web.route.clone(),
                    session: web.session.clone(),
                    source_label: web.source_label.clone(),
                    trust_state: web.trust_state.clone(),
                    restricted: web.restricted,
                    snapshot_label: web.snapshot_label.clone(),
                    canonical_handle: web.canonical_handle.clone(),
                    session_id: web.session_id.clone(),
                    adapter: web.adapter.clone(),
                    manifest_path: web.manifest_path.clone(),
                    runtime_label: web.runtime_label.clone(),
                    display_strategy: web.display_strategy.clone(),
                    log_path: web.log_path.clone(),
                    local_url: web.local_url.clone(),
                    healthcheck_url: web.healthcheck_url.clone(),
                    invoke_url: web.invoke_url.clone(),
                    served_by: web.served_by.clone(),
                })
            }
            PaneSurface::CapsuleStatus(capsule) => Some(ActiveCapsulePane {
                pane_id: pane.id,
                title: pane.title.clone(),
                route: capsule.route.clone(),
                session: capsule.session.clone(),
                source_label: capsule.source_label.clone(),
                trust_state: capsule.trust_state.clone(),
                restricted: capsule.restricted,
                snapshot_label: capsule.snapshot_label.clone(),
                canonical_handle: capsule.canonical_handle.clone(),
                session_id: capsule.session_id.clone(),
                adapter: capsule.adapter.clone(),
                manifest_path: capsule.manifest_path.clone(),
                runtime_label: capsule.runtime_label.clone(),
                display_strategy: capsule.display_strategy.clone(),
                log_path: capsule.log_path.clone(),
                local_url: capsule.local_url.clone(),
                healthcheck_url: capsule.healthcheck_url.clone(),
                invoke_url: capsule.invoke_url.clone(),
                served_by: capsule.served_by.clone(),
            }),
            _ => None,
        }
    }

    pub fn active_workspace(&self) -> Option<&Workspace> {
        self.workspaces
            .iter()
            .find(|workspace| workspace.id == self.active_workspace)
    }

    pub fn active_workspace_mut(&mut self) -> Option<&mut Workspace> {
        self.workspaces
            .iter_mut()
            .find(|workspace| workspace.id == self.active_workspace)
    }

    pub fn active_task(&self) -> Option<&TaskSet> {
        self.active_workspace()?.active_task()
    }

    pub fn active_task_mut(&mut self) -> Option<&mut TaskSet> {
        self.active_workspace_mut()?.active_task_mut()
    }

    pub fn pane_source_label(&self, pane_id: PaneId) -> Option<String> {
        for workspace in &self.workspaces {
            for task in &workspace.tasks {
                for pane in &task.panes {
                    if pane.id == pane_id {
                        if let PaneSurface::Web(web) = &pane.surface {
                            return web.source_label.clone();
                        }
                    }
                }
            }
        }
        None
    }

    pub fn active_panes(&self) -> Vec<&Pane> {
        self.active_task()
            .map(|task| task.panes.iter().collect())
            .unwrap_or_default()
    }

    pub fn sidebar_task_items(&self) -> Vec<SidebarTaskItem> {
        let Some(workspace) = self.active_workspace() else {
            return Vec::new();
        };

        workspace
            .tasks
            .iter()
            .map(|task| SidebarTaskItem {
                id: task.id,
                title: task.title.clone(),
                is_active: task.id == workspace.active_task,
                icon: sidebar_icon_for_task(task),
            })
            .collect()
    }

    pub fn sync_web_session_state(&mut self, pane_id: PaneId, session: WebSessionState) {
        // Session updates arrive from the webview manager, so match by pane id instead of holding refs.
        for workspace in &mut self.workspaces {
            for task in &mut workspace.tasks {
                for pane in &mut task.panes {
                    if pane.id == pane_id {
                        if let PaneSurface::Web(web) = &mut pane.surface {
                            web.session = session.clone();
                        }
                    }
                }
            }
        }
    }

    pub fn update_web_route(
        &mut self,
        pane_id: PaneId,
        route: GuestRoute,
        session: WebSessionState,
        capabilities: Vec<CapabilityGrant>,
    ) {
        let label = route.to_string();
        self.update_pane(pane_id, |pane| {
            pane.title = label.clone();
            if let PaneSurface::Web(web) = &mut pane.surface {
                web.profile = route_profile(&route).to_string();
                web.route = route.clone();
                web.partition_id = sanitize(&label);
                web.session = session.clone();
                web.capabilities = capabilities.clone();
            }
        });
        self.sync_command_bar_with_active_route();
    }

    pub fn active_capsule_inspector(&self) -> Option<CapsuleInspectorView> {
        let active = self.active_capsule_pane()?;
        Some(CapsuleInspectorView {
            pane_id: active.pane_id,
            title: active.title,
            handle: active.route.to_string(),
            canonical_handle: active.canonical_handle,
            source_label: active.source_label,
            trust_state: active.trust_state,
            restricted: active.restricted,
            snapshot_label: active.snapshot_label,
            session_state: active.session,
            session_id: active.session_id,
            adapter: active.adapter,
            manifest_path: active.manifest_path,
            runtime_label: active.runtime_label,
            display_strategy: active.display_strategy,
            log_path: active.log_path,
            local_url: active.local_url,
            healthcheck_url: active.healthcheck_url,
            invoke_url: active.invoke_url,
            served_by: active.served_by,
            logs: self
                .capsule_logs
                .get(&active.pane_id)
                .cloned()
                .unwrap_or_default(),
        })
    }

    pub fn push_activity(&mut self, tone: ActivityTone, message: impl Into<String>) {
        self.activity.push(ActivityEntry {
            tone,
            message: message.into(),
        });
        // Keep the activity rail compact so it behaves like a live log, not an unbounded transcript.
        if self.activity.len() > 12 {
            let excess = self.activity.len() - 12;
            self.activity.drain(0..excess);
        }
    }

    pub fn extend_activity(&mut self, entries: Vec<ActivityEntry>) {
        for entry in entries {
            self.push_activity(entry.tone, entry.message);
        }
    }

    pub fn push_capsule_log(
        &mut self,
        pane_id: PaneId,
        stage: CapsuleLogStage,
        tone: ActivityTone,
        message: impl Into<String>,
    ) {
        let logs = self.capsule_logs.entry(pane_id).or_default();
        logs.push(CapsuleLogEntry {
            stage,
            tone,
            message: message.into(),
        });
        if logs.len() > 40 {
            let excess = logs.len() - 40;
            logs.drain(0..excess);
        }
    }

    pub fn normalize_input(input: &str) -> String {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return "https://www.google.com".to_string();
        }

        // Local filesystem paths — pass through to classify_surface_input which
        // will recognise them as LocalPath capsules.
        if trimmed.starts_with('/')
            || trimmed.starts_with("./")
            || trimmed.starts_with("../")
            || trimmed.starts_with("~/")
            || trimmed == "."
            || trimmed == ".."
        {
            return trimmed.to_string();
        }

        if trimmed.starts_with("capsule://")
            || trimmed.starts_with("ato://")
            || looks_like_registry_sugar(trimmed)
            || trimmed.starts_with("github.com/")
        {
            return trimmed.to_string();
        }

        if Url::parse(trimmed).is_ok() {
            return trimmed.to_string();
        }

        if trimmed.contains('.') && !trimmed.contains(' ') {
            let candidate = format!("https://{trimmed}");
            if Url::parse(&candidate).is_ok() {
                return candidate;
            }
        }

        Self::search_fallback(trimmed)
    }

    fn search_fallback(trimmed: &str) -> String {
        let encoded = form_urlencoded::byte_serialize(trimmed.as_bytes())
            .collect::<String>()
            .replace('+', "%20");
        format!("https://www.google.com/search?q={encoded}")
    }

    fn advance_workspace(&mut self, delta: isize) {
        if self.workspaces.is_empty() {
            return;
        }
        let current_index = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == self.active_workspace)
            .unwrap_or(0) as isize;
        let len = self.workspaces.len() as isize;
        let next = (current_index + delta).rem_euclid(len) as usize;
        self.active_workspace = self.workspaces[next].id;
        self.sync_command_bar_with_active_route();
        // Switching workspace should also refresh the omnibar so the shell always points at the active route.
        let title = self.workspaces[next].title.clone();
        self.push_activity(ActivityTone::Info, format!("Switched workspace to {title}"));
    }

    fn advance_task(&mut self, delta: isize) {
        let next_title = if let Some(workspace) = self.active_workspace_mut() {
            let current_index = workspace
                .tasks
                .iter()
                .position(|task| task.id == workspace.active_task)
                .unwrap_or(0) as isize;
            let len = workspace.tasks.len() as isize;
            let next = (current_index + delta).rem_euclid(len) as usize;
            workspace.active_task = workspace.tasks[next].id;
            Some(workspace.tasks[next].title.clone())
        } else {
            None
        };
        self.sync_command_bar_with_active_route();
        // Task switching keeps the shell focus model and the command bar text in sync.
        if let Some(title) = next_title {
            self.push_activity(ActivityTone::Info, format!("Switched task to {title}"));
        }
    }

    fn adjust_split(&mut self, delta: f32) {
        let ratio = if let Some(task) = self.active_task_mut() {
            // Clamp the split so neither pane can collapse below a readable minimum width.
            task.split_ratio = (task.split_ratio + delta).clamp(0.4, 0.85);
            if let PaneTree::Split { ratio, .. } = &mut task.pane_tree {
                *ratio = task.split_ratio;
            }
            Some(task.split_ratio)
        } else {
            None
        };
        if let Some(ratio) = ratio {
            self.push_activity(
                ActivityTone::Info,
                format!("Adjusted split ratio to {:.0}%", ratio * 100.0),
            );
        }
    }

    fn enqueue_browser_command(&mut self, kind: BrowserCommandKind) {
        let Some(active) = self.active_web_pane() else {
            return;
        };
        if !matches!(&active.route, GuestRoute::ExternalUrl(_)) {
            return;
        }

        self.browser_commands.push_back(BrowserCommand {
            pane_id: active.pane_id,
            kind,
        });
        self.sync_web_session_state(active.pane_id, WebSessionState::Launching);
    }

    pub(crate) fn update_pane(&mut self, pane_id: PaneId, mut update: impl FnMut(&mut Pane)) {
        for workspace in &mut self.workspaces {
            for task in &mut workspace.tasks {
                for pane in &mut task.panes {
                    if pane.id == pane_id {
                        update(pane);
                    }
                }
            }
        }
    }

    fn pane_route_label(&self, pane_id: PaneId) -> Option<String> {
        self.workspaces
            .iter()
            .flat_map(|workspace| workspace.tasks.iter())
            .flat_map(|task| task.panes.iter())
            .find(|pane| pane.id == pane_id)
            .map(|pane| match &pane.surface {
                PaneSurface::Web(web) => web.route.to_string(),
                PaneSurface::Native { .. } => pane.title.clone(),
                PaneSurface::CapsuleStatus(capsule) => capsule.route.to_string(),
                PaneSurface::DevConsole => "Developer console".to_string(),
                PaneSurface::Inspector => "Capsule inspector".to_string(),
                PaneSurface::Launcher => "Launchpad".to_string(),
                PaneSurface::AuthHandoff { origin, .. } => format!("Signing in to {origin}…"),
                PaneSurface::Terminal(terminal) => {
                    format!("terminal://{}/", terminal.session_id)
                }
            })
    }

    pub fn update_capsule_route_metadata(
        &mut self,
        pane_id: PaneId,
        canonical_handle: Option<String>,
        source_label: Option<String>,
        trust_state: Option<String>,
        restricted: bool,
        snapshot_label: Option<String>,
        session_id: Option<String>,
        adapter: Option<String>,
        manifest_path: Option<String>,
        runtime_label: Option<String>,
        display_strategy: Option<String>,
        log_path: Option<String>,
        local_url: Option<String>,
        healthcheck_url: Option<String>,
        invoke_url: Option<String>,
        served_by: Option<String>,
    ) {
        self.update_pane(pane_id, |pane| match &mut pane.surface {
            PaneSurface::Web(web) => {
                web.canonical_handle = canonical_handle.clone();
                web.source_label = source_label.clone();
                web.trust_state = trust_state.clone();
                web.restricted = restricted;
                web.snapshot_label = snapshot_label.clone();
                web.session_id = session_id.clone();
                web.adapter = adapter.clone();
                web.manifest_path = manifest_path.clone();
                web.runtime_label = runtime_label.clone();
                web.display_strategy = display_strategy.clone();
                web.log_path = log_path.clone();
                web.local_url = local_url.clone();
                web.healthcheck_url = healthcheck_url.clone();
                web.invoke_url = invoke_url.clone();
                web.served_by = served_by.clone();
            }
            PaneSurface::CapsuleStatus(capsule) => {
                capsule.canonical_handle = canonical_handle.clone();
                capsule.source_label = source_label.clone();
                capsule.trust_state = trust_state.clone();
                capsule.restricted = restricted;
                capsule.snapshot_label = snapshot_label.clone();
                capsule.session_id = session_id.clone();
                capsule.adapter = adapter.clone();
                capsule.manifest_path = manifest_path.clone();
                capsule.runtime_label = runtime_label.clone();
                capsule.display_strategy = display_strategy.clone();
                capsule.log_path = log_path.clone();
                capsule.local_url = local_url.clone();
                capsule.healthcheck_url = healthcheck_url.clone();
                capsule.invoke_url = invoke_url.clone();
                capsule.served_by = served_by.clone();
            }
            _ => {}
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn mount_capsule_status_pane(
        &mut self,
        pane_id: PaneId,
        route: GuestRoute,
        canonical_handle: Option<String>,
        source_label: Option<String>,
        trust_state: Option<String>,
        restricted: bool,
        snapshot_label: Option<String>,
        session_id: Option<String>,
        adapter: Option<String>,
        manifest_path: Option<String>,
        runtime_label: Option<String>,
        display_strategy: Option<String>,
        log_path: Option<String>,
        local_url: Option<String>,
        healthcheck_url: Option<String>,
        invoke_url: Option<String>,
        served_by: Option<String>,
    ) {
        self.update_pane(pane_id, |pane| {
            pane.title = route.to_string();
            pane.surface = PaneSurface::CapsuleStatus(CapsuleStatusPane {
                route: route.clone(),
                session: WebSessionState::Mounted,
                source_label: source_label.clone(),
                trust_state: trust_state.clone(),
                restricted,
                snapshot_label: snapshot_label.clone(),
                canonical_handle: canonical_handle.clone(),
                session_id: session_id.clone(),
                adapter: adapter.clone(),
                manifest_path: manifest_path.clone(),
                runtime_label: runtime_label.clone(),
                display_strategy: display_strategy.clone(),
                log_path: log_path.clone(),
                local_url: local_url.clone(),
                healthcheck_url: healthcheck_url.clone(),
                invoke_url: invoke_url.clone(),
                served_by: served_by.clone(),
            });
        });
        self.command_bar_text = route.to_string();
    }

    /// Open a bare CLI panel in a new tab.
    ///
    /// `cmd` maps to a `CliLaunchSpec`:
    /// - `None` or `"ato-run"` → `CliLaunchSpec::AtoRunRepl { prelude: None, .. }`
    ///   (the default: every input line is executed as `ato run -- <line>`).
    /// - `"ato"` → `CliLaunchSpec::RawAto` (runs the `ato` binary directly).
    /// - any other value → `CliLaunchSpec::RawShell(value)` (interactive shell
    ///   under nacelle, e.g. `bash` / `zsh` / `/bin/sh`).
    pub fn open_cli_panel(&mut self, cmd: Option<String>) {
        let spec = match cmd.as_deref().map(str::trim) {
            None | Some("") | Some("ato-run") => CliLaunchSpec::ato_run_repl(),
            Some("ato") => CliLaunchSpec::RawAto,
            Some(other) => CliLaunchSpec::RawShell(other.to_string()),
        };
        self.open_cli_panel_with_spec(spec, None);
    }

    /// Open a CLI panel with an explicit `CliLaunchSpec`.
    ///
    /// Used by share URL integration (`navigate_to_url`) to open an `ato://cli`
    /// REPL pre-loaded with `ato run <share-url>` as its prelude command. When
    /// `title_suffix` is provided it is appended to the base title (e.g.
    /// `"ato CLI · share:abcd1234"`), otherwise the default title derived from
    /// the spec is used.
    pub fn open_cli_panel_with_spec(
        &mut self,
        spec: CliLaunchSpec,
        title_suffix: Option<String>,
    ) {
        let base_title = match &spec {
            CliLaunchSpec::AtoRunRepl { .. } => "ato CLI".to_string(),
            CliLaunchSpec::RawShell(shell) => format!("CLI ({shell})"),
            CliLaunchSpec::RawAto => "ato".to_string(),
        };
        let title = match title_suffix {
            Some(suffix) if !suffix.is_empty() => format!("{base_title} · {suffix}"),
            _ => base_title,
        };

        // `create_new_tab` uses `self.next_pane_id` for the new pane, then
        // increments it. Capture it before the call so we can target the new
        // pane without searching for it afterwards.
        let new_pane_id = self.next_pane_id;
        self.create_new_tab();

        let session_id = format!("cli-{}-{}", new_pane_id, uuid_v4_simple());
        register_pending_cli_command(session_id.clone(), spec.clone());

        self.push_activity(
            ActivityTone::Info,
            format!("Opening {title} panel from ato://cli"),
        );
        self.mount_terminal_stream_pane(new_pane_id, session_id, title);
    }

    /// Switch pane to a `Terminal` surface for a `terminal_stream` capsule session.
    ///
    /// Called after `ato app session start` returns `display_strategy = terminal_stream`.
    /// Replaces whatever `Web(CapsuleHandle)` surface the pane currently has with a
    /// `Terminal` surface that routes through the `terminal://` custom protocol.
    pub fn mount_terminal_stream_pane(
        &mut self,
        pane_id: PaneId,
        session_id: String,
        title: String,
    ) {
        self.update_pane(pane_id, |pane| {
            pane.title = title.clone();
            pane.surface = PaneSurface::Terminal(TerminalPane {
                session_id: session_id.clone(),
                capsule_handle: title.clone(),
                cols: 80,
                rows: 24,
            });
        });
        self.command_bar_text = title;
    }

    pub fn active_permission_prompt(&self) -> Option<&PermissionPrompt> {
        self.pending_permission_prompt.as_ref()
    }

    pub fn allow_permission_once(&mut self) {
        let Some(prompt) = self.pending_permission_prompt.take() else {
            return;
        };
        self.push_capsule_log(
            prompt.pane_id,
            CapsuleLogStage::Permission,
            ActivityTone::Info,
            format!(
                "Recorded one-shot permission intent for {}",
                prompt.capability
            ),
        );
        self.push_activity(
            ActivityTone::Info,
            format!(
                "Host recorded one-shot permission intent for {} on {}. Runtime grant wiring is pending.",
                prompt.capability, prompt.route_label
            ),
        );
    }

    pub fn allow_permission_for_session(&mut self) {
        let Some(prompt) = self.pending_permission_prompt.take() else {
            return;
        };
        self.push_capsule_log(
            prompt.pane_id,
            CapsuleLogStage::Permission,
            ActivityTone::Info,
            format!(
                "Recorded session permission intent for {}",
                prompt.capability
            ),
        );
        self.push_activity(
            ActivityTone::Info,
            format!(
                "Host recorded session permission intent for {} on {}. Runtime grant wiring is pending.",
                prompt.capability, prompt.route_label
            ),
        );
    }

    pub fn deny_permission_prompt(&mut self) {
        let Some(prompt) = self.pending_permission_prompt.take() else {
            return;
        };
        self.push_capsule_log(
            prompt.pane_id,
            CapsuleLogStage::Permission,
            ActivityTone::Warning,
            format!("Denied permission {}", prompt.capability),
        );
        self.push_activity(
            ActivityTone::Warning,
            format!(
                "Denied permission {} for {}.",
                prompt.capability, prompt.route_label
            ),
        );
    }

    pub fn classify_url(&self, url: &str) -> AuthMode {
        self.auth_policy_registry.classify(url)
    }

    /// Called when a nav intercept fires. Swaps the pane surface to AuthHandoff and returns the session_id.
    pub fn begin_auth_handoff(&mut self, pane_id: PaneId, url: &str) -> String {
        let origin = Url::parse(url)
            .map(|u| u.host_str().unwrap_or("unknown").to_string())
            .unwrap_or_else(|_| "unknown".to_string());
        let session_id = format!("auth-{}", uuid_v4_simple());
        let auth_mode = self.classify_url(url);
        self.auth_sessions.push(AuthSession {
            session_id: session_id.clone(),
            originating_pane_id: pane_id,
            auth_mode,
            origin: origin.clone(),
            start_url: url.to_string(),
            status: AuthSessionStatus::Created,
            created_at: std::time::SystemTime::now(),
        });
        self.update_pane(pane_id, |pane| {
            let original_surface = std::mem::replace(&mut pane.surface, PaneSurface::Launcher);
            if matches!(original_surface, PaneSurface::Web(_)) {
                pane.surface = PaneSurface::AuthHandoff {
                    session_id: session_id.clone(),
                    origin: origin.clone(),
                    original_surface: Box::new(original_surface),
                };
            }
        });
        session_id
    }

    pub fn cancel_auth_handoff(&mut self, pane_id: PaneId) {
        let first_party = self
            .auth_sessions
            .iter()
            .find(|session| session.originating_pane_id == pane_id)
            .is_some_and(|session| matches!(session.auth_mode, AuthMode::FirstPartyNative));
        self.update_pane(pane_id, |pane| {
            if let PaneSurface::AuthHandoff {
                original_surface, ..
            } = std::mem::replace(&mut pane.surface, PaneSurface::Launcher)
            {
                pane.surface = *original_surface;
            }
        });
        if let Some(s) = self
            .auth_sessions
            .iter_mut()
            .find(|s| s.originating_pane_id == pane_id)
        {
            s.status = AuthSessionStatus::Cancelled;
        }
        if first_party {
            self.pending_post_login_target = None;
            if self.desktop_auth.publisher_handle.is_none() {
                self.desktop_auth.status = DesktopAuthStatus::SignedOut;
            }
        }
    }

    pub fn resume_after_auth(&mut self, pane_id: PaneId) {
        let first_party = self
            .auth_sessions
            .iter()
            .find(|session| session.originating_pane_id == pane_id)
            .is_some_and(|session| matches!(session.auth_mode, AuthMode::FirstPartyNative));
        self.update_pane(pane_id, |pane| {
            if let PaneSurface::AuthHandoff {
                original_surface, ..
            } = std::mem::replace(&mut pane.surface, PaneSurface::Launcher)
            {
                pane.surface = *original_surface;
            }
        });
        if let Some(s) = self
            .auth_sessions
            .iter_mut()
            .find(|s| s.originating_pane_id == pane_id)
        {
            s.status = AuthSessionStatus::Completed;
        }
        if first_party {
            self.pending_post_login_target = None;
            if matches!(self.desktop_auth.status, DesktopAuthStatus::AwaitingBrowser) {
                self.desktop_auth.status = DesktopAuthStatus::SignedIn;
            }
        }
    }

    fn complete_ato_login(&mut self, publisher_handle: Option<String>) {
        let pending_target = self.pending_post_login_target.take();
        if let Some(handle) = publisher_handle {
            self.desktop_auth.publisher_handle = Some(handle);
        }
        self.desktop_auth.status = DesktopAuthStatus::SignedIn;
        self.desktop_auth.last_login_origin = Some("ato.run".to_string());

        if let Some(pane_id) = self.find_active_auth_handoff_pane_id() {
            self.resume_after_auth(pane_id);
        }

        match pending_target {
            Some(PendingPostLoginTarget::CloudDock) => {
                let target = self.desktop_auth.publisher_handle.as_deref();
                self.navigate_to_url(&cloud_dock_url(target));
            }
            None => {
                self.command_bar_text =
                    cloud_dock_url(self.desktop_auth.publisher_handle.as_deref());
            }
        }
        self.push_activity(ActivityTone::Info, "ato.run sign-in completed");
    }

    fn fail_ato_login(&mut self) {
        self.desktop_auth.status = DesktopAuthStatus::Failed;
        self.desktop_auth.last_login_origin = Some("ato.run".to_string());
        self.pending_post_login_target = None;

        if let Some(session) = self
            .auth_sessions
            .iter_mut()
            .rev()
            .find(|session| matches!(session.auth_mode, AuthMode::FirstPartyNative))
        {
            session.status = AuthSessionStatus::Failed;
        }

        self.push_activity(
            ActivityTone::Warning,
            "ato.run sign-in did not complete. Finish in the browser or return manually.",
        );
    }

    fn sync_command_bar_with_active_route(&mut self) {
        self.command_bar_text = self
            .active_capsule_pane()
            .map(|pane| pane.route.to_string())
            .or_else(|| self.active_web_pane().map(|pane| pane.route.to_string()))
            .unwrap_or_default();
    }

    fn find_active_auth_handoff_pane_id(&self) -> Option<PaneId> {
        self.active_panes()
            .iter()
            .find(|pane| matches!(pane.surface, PaneSurface::AuthHandoff { .. }))
            .map(|pane| pane.id)
    }
}

impl Workspace {
    pub fn active_task(&self) -> Option<&TaskSet> {
        self.tasks.iter().find(|task| task.id == self.active_task)
    }

    pub fn active_task_mut(&mut self) -> Option<&mut TaskSet> {
        self.tasks
            .iter_mut()
            .find(|task| task.id == self.active_task)
    }
}

impl TaskSet {
    pub fn focused_pane(&self) -> Option<&Pane> {
        self.panes.iter().find(|pane| pane.id == self.focused_pane)
    }

    pub fn focused_pane_mut(&mut self) -> Option<&mut Pane> {
        self.panes
            .iter_mut()
            .find(|pane| pane.id == self.focused_pane)
    }
}

fn uuid_v4_simple() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    format!("{t:08x}")
}

fn local_registry_url() -> &'static str {
    "http://127.0.0.1:8787"
}

fn cloud_dock_url(publisher_handle: Option<&str>) -> String {
    match publisher_handle {
        Some(handle) if !handle.is_empty() => format!("https://ato.run/dock/{handle}"),
        _ => "https://ato.run/dock".to_string(),
    }
}

fn ato_login_url(target: PendingPostLoginTarget) -> String {
    let mut url = Url::parse("https://ato.run/auth").expect("valid ato.run auth url");
    url.query_pairs_mut().append_pair("next", "/dock");
    let desktop_return = match target {
        PendingPostLoginTarget::CloudDock => "ato://auth/callback/cloud-dock",
    };
    url.query_pairs_mut()
        .append_pair("desktop_return", desktop_return);
    url.to_string()
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect()
}

fn sidebar_icon_for_task(task: &TaskSet) -> SidebarTaskIconSpec {
    let Some(pane) = task.focused_pane() else {
        return SidebarTaskIconSpec::Monogram(short_label(&task.title));
    };

    match &pane.surface {
        PaneSurface::Web(web) => match &web.route {
            GuestRoute::ExternalUrl(url) => external_origin(url)
                .map(|origin| SidebarTaskIconSpec::ExternalUrl { origin })
                .unwrap_or_else(|| SidebarTaskIconSpec::Monogram(short_label(&task.title))),
            GuestRoute::Terminal { .. } => {
                SidebarTaskIconSpec::SystemIcon(SystemPageIcon::Terminal)
            }
            GuestRoute::Capsule { .. }
            | GuestRoute::CapsuleHandle { .. }
            | GuestRoute::CapsuleUrl { .. } => {
                SidebarTaskIconSpec::Monogram(short_label(&task.title))
            }
        },
        PaneSurface::DevConsole => SidebarTaskIconSpec::SystemIcon(SystemPageIcon::Console),
        PaneSurface::Terminal(_) => SidebarTaskIconSpec::SystemIcon(SystemPageIcon::Terminal),
        PaneSurface::Launcher => SidebarTaskIconSpec::SystemIcon(SystemPageIcon::Launcher),
        PaneSurface::Inspector => SidebarTaskIconSpec::SystemIcon(SystemPageIcon::Inspector),
        PaneSurface::CapsuleStatus(_) => {
            SidebarTaskIconSpec::SystemIcon(SystemPageIcon::CapsuleStatus)
        }
        PaneSurface::Native { .. } | PaneSurface::AuthHandoff { .. } => {
            SidebarTaskIconSpec::Monogram(short_label(&task.title))
        }
    }
}

fn external_origin(url: &Url) -> Option<String> {
    match url.scheme() {
        "http" | "https" => Some(url.origin().ascii_serialization()),
        _ => None,
    }
}

fn short_label(title: &str) -> String {
    title.chars().take(2).collect::<String>().to_uppercase()
}

fn task_route_label(task: &TaskSet) -> String {
    let Some(pane) = task.focused_pane() else {
        return "No pane".to_string();
    };

    match &pane.surface {
        PaneSurface::Web(web) => web.route.to_string(),
        PaneSurface::Native { .. } => "Native settings panel".to_string(),
        PaneSurface::CapsuleStatus(capsule) => capsule.route.to_string(),
        PaneSurface::DevConsole => "Developer console".to_string(),
        PaneSurface::Inspector => "Capsule inspector".to_string(),
        PaneSurface::Launcher => "Launchpad".to_string(),
        PaneSurface::AuthHandoff { origin, .. } => format!("Signing in to {origin}…"),
        PaneSurface::Terminal(terminal) => format!("terminal://{}/", terminal.session_id),
    }
}

fn demo_local_capsule(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
        .display()
        .to_string()
}

fn route_profile(route: &GuestRoute) -> &'static str {
    match route {
        GuestRoute::CapsuleHandle { handle, .. } if handle.contains("electron") => "electron",
        GuestRoute::CapsuleHandle { handle, .. } if handle.contains("wails") => "wails",
        GuestRoute::CapsuleUrl { .. } => "electron",
        GuestRoute::ExternalUrl(_) => "electron",
        _ => "tauri",
    }
}

fn route_profile_for_source(source: &str) -> &'static str {
    match source {
        "github" | "registry" | "local" => "tauri",
        _ => "electron",
    }
}

fn route_source_label(handle: &str) -> String {
    normalize_capsule_handle(handle)
        .map(|canonical| canonical.source_label().to_string())
        .unwrap_or_else(|_| {
            if handle.starts_with("capsule://github.com/") {
                "github".to_string()
            } else if handle.starts_with("capsule://") {
                "registry".to_string()
            } else {
                "local".to_string()
            }
        })
}

fn looks_like_registry_sugar(value: &str) -> bool {
    let candidate = value
        .split_once('@')
        .map(|(prefix, _)| prefix)
        .unwrap_or(value);
    let mut parts = candidate.split('/');
    let Some(first) = parts.next() else {
        return false;
    };
    let Some(second) = parts.next() else {
        return false;
    };
    parts.next().is_none() && !first.is_empty() && !second.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::ShellEvent;

    #[test]
    fn cycle_handle_changes_route() {
        let mut state = AppState::demo();
        state.select_task(2);
        let before = state.active_web_pane().expect("pane").route.to_string();
        state.cycle_handle();
        let after = state.active_web_pane().expect("pane").route.to_string();
        assert_ne!(before, after);
    }

    #[test]
    fn normalize_input_supports_urls_hosts_and_searches() {
        assert_eq!(
            AppState::normalize_input("https://example.com"),
            "https://example.com"
        );
        assert_eq!(
            AppState::normalize_input("capsule://github.com/acme/chat"),
            "capsule://github.com/acme/chat"
        );
        assert_eq!(
            AppState::normalize_input("capsule://localhost:8787/acme/chat"),
            "capsule://localhost:8787/acme/chat"
        );
        assert_eq!(AppState::normalize_input("acme/chat"), "acme/chat");
        assert_eq!(
            AppState::normalize_input("example.com"),
            "https://example.com"
        );
        assert_eq!(
            AppState::normalize_input("hello world"),
            "https://www.google.com/search?q=hello%20world"
        );
        assert_eq!(AppState::normalize_input(""), "https://www.google.com");

        // Local filesystem paths must pass through without URL-ification.
        assert_eq!(
            AppState::normalize_input("/Users/me/projects/my-capsule"),
            "/Users/me/projects/my-capsule"
        );
        assert_eq!(
            AppState::normalize_input("./samples/test-echo"),
            "./samples/test-echo"
        );
        assert_eq!(
            AppState::normalize_input("~/projects/capsule"),
            "~/projects/capsule"
        );
        assert_eq!(AppState::normalize_input("."), ".");
        assert_eq!(AppState::normalize_input(".."), "..");
    }

    #[test]
    fn navigate_to_url_updates_web_pane_state() {
        let mut state = AppState::demo();
        state.navigate_to_url("example.com");
        let pane = state.active_web_pane().expect("pane");

        assert_eq!(pane.route.to_string(), "https://example.com/");
        assert_eq!(pane.partition_id, "https---example-com-");
        assert_eq!(pane.profile, "electron");
        assert_eq!(pane.session, WebSessionState::Launching);
        assert_eq!(state.command_bar_text, "https://example.com/");
    }

    #[test]
    fn navigate_to_capsule_handle_uses_resolving_state_and_registry_metadata() {
        let mut state = AppState::demo();
        state.navigate_to_url("acme/chat");
        let pane = state.active_web_pane().expect("pane");

        assert_eq!(pane.route.to_string(), "capsule://ato.run/acme/chat");
        assert_eq!(pane.session, WebSessionState::Resolving);
        assert_eq!(pane.source_label.as_deref(), Some("registry"));
        assert_eq!(pane.trust_state.as_deref(), Some("untrusted"));
        assert!(pane.restricted);
        assert_eq!(state.command_bar_text, "capsule://ato.run/acme/chat");
    }

    #[test]
    fn navigate_to_loopback_capsule_handle_uses_registry_metadata() {
        let mut state = AppState::demo();
        state.navigate_to_url("capsule://localhost:8787/acme/chat");
        let pane = state.active_web_pane().expect("pane");

        assert_eq!(pane.route.to_string(), "capsule://localhost:8787/acme/chat");
        assert_eq!(pane.session, WebSessionState::Resolving);
        assert_eq!(pane.source_label.as_deref(), Some("registry"));
        assert_eq!(pane.trust_state.as_deref(), Some("untrusted"));
        assert!(pane.restricted);
        assert_eq!(state.command_bar_text, "capsule://localhost:8787/acme/chat");
    }

    #[test]
    fn apply_shell_events_updates_route_title_and_command_bar() {
        let mut state = AppState::demo();
        state.select_task(2);
        let pane_id = state.active_web_pane().expect("pane").pane_id;

        state.apply_shell_events(vec![
            ShellEvent::UrlChanged {
                pane_id,
                url: "https://docs.rs/".to_string(),
            },
            ShellEvent::TitleChanged {
                pane_id,
                title: "docs.rs".to_string(),
            },
        ]);

        let pane = state.active_web_pane().expect("pane");
        assert_eq!(pane.route.to_string(), "https://docs.rs/");
        assert_eq!(pane.title, "docs.rs");
        assert_eq!(state.command_bar_text, "https://docs.rs/");
    }

    #[test]
    fn apply_shell_events_marks_session_ready_as_mounted() {
        let mut state = AppState::demo();
        state.select_task(2);
        let pane_id = state.active_web_pane().expect("pane").pane_id;

        state.apply_shell_events(vec![ShellEvent::SessionReady { pane_id }]);

        let pane = state.active_web_pane().expect("pane");
        assert_eq!(pane.session, WebSessionState::Mounted);
    }

    #[test]
    fn permission_denied_event_raises_host_owned_prompt() {
        let mut state = AppState::demo();
        state.select_task(2);
        let pane_id = state.active_web_pane().expect("pane").pane_id;

        state.apply_shell_events(vec![ShellEvent::PermissionDenied {
            pane_id,
            capability: "read-file".to_string(),
            command: Some("fs.read".to_string()),
        }]);

        let prompt = state.active_permission_prompt().expect("permission prompt");
        assert_eq!(prompt.pane_id, pane_id);
        assert_eq!(prompt.capability, "read-file");
        assert_eq!(prompt.command.as_deref(), Some("fs.read"));
        let inspector = state.active_capsule_inspector().expect("inspector");
        assert!(inspector
            .logs
            .iter()
            .any(|entry| entry.stage == CapsuleLogStage::Permission));
    }

    #[test]
    fn capsule_inspector_tracks_navigation_metadata_and_logs() {
        let mut state = AppState::demo();

        state.navigate_to_url("capsule://ato.run/koh0920/ato-onboarding");
        let pane_id = state.active_web_pane().expect("pane").pane_id;
        state.update_capsule_route_metadata(
            pane_id,
            Some("capsule://ato.run/koh0920/ato-onboarding".to_string()),
            Some("registry".to_string()),
            Some("untrusted".to_string()),
            true,
            Some("version 0.1.0".to_string()),
            Some("desky-session-1".to_string()),
            Some("tauri".to_string()),
            Some("/tmp/capsule.toml".to_string()),
            Some("tauri".to_string()),
            Some("guest-webview".to_string()),
            Some("/tmp/capsule.log".to_string()),
            Some("http://127.0.0.1:4173".to_string()),
            Some("http://127.0.0.1:4173/health".to_string()),
            Some("http://127.0.0.1:4173/invoke".to_string()),
            Some("deno".to_string()),
        );
        state.push_capsule_log(
            pane_id,
            CapsuleLogStage::Launch,
            ActivityTone::Info,
            "Guest session ready",
        );

        let inspector = state.active_capsule_inspector().expect("inspector");
        assert_eq!(inspector.handle, "capsule://ato.run/koh0920/ato-onboarding");
        assert_eq!(
            inspector.canonical_handle.as_deref(),
            Some("capsule://ato.run/koh0920/ato-onboarding")
        );
        assert_eq!(inspector.source_label.as_deref(), Some("registry"));
        assert_eq!(inspector.session_id.as_deref(), Some("desky-session-1"));
        assert_eq!(inspector.adapter.as_deref(), Some("tauri"));
        assert!(inspector.logs.iter().any(|entry| {
            entry.stage == CapsuleLogStage::Resolve && entry.message.contains("Queued capsule")
        }));
        assert!(inspector.logs.iter().any(|entry| {
            entry.stage == CapsuleLogStage::Launch && entry.message.contains("ready")
        }));
    }

    #[test]
    fn show_settings_panel_adds_new_settings_task() {
        let mut state = AppState::demo();
        let original_task_count = state.active_workspace().expect("workspace").tasks.len();
        let original_task_id = state.active_task().expect("task").id;

        state.show_settings_panel();

        let workspace = state.active_workspace().expect("workspace");
        assert_eq!(workspace.tasks.len(), original_task_count + 1);
        assert_eq!(workspace.active_task().expect("task").title, "Settings");

        let pane = workspace
            .active_task()
            .and_then(|task| task.focused_pane())
            .expect("pane");
        assert!(matches!(pane.surface, PaneSurface::Native { .. }));
        assert_eq!(state.command_bar_text, "");

        let previous_task = workspace
            .tasks
            .iter()
            .find(|task| task.id == original_task_id)
            .expect("original task should remain");
        assert!(matches!(
            previous_task.focused_pane().expect("pane").surface,
            PaneSurface::Web(_) | PaneSurface::Launcher
        ));
    }

    #[test]
    fn select_task_updates_active_task_and_command_bar() {
        let mut state = AppState::demo();

        state.select_task(3);

        assert_eq!(state.active_task().expect("task").id, 3);
        assert_eq!(state.command_bar_text, "https://ato.run/");
    }

    #[test]
    fn create_new_tab_adds_task_to_active_workspace() {
        let mut state = AppState::demo();
        let workspace_count = state.workspaces.len();
        let task_count = state.active_workspace().expect("workspace").tasks.len();

        state.create_new_tab();

        let workspace = state.active_workspace().expect("workspace");
        assert_eq!(state.workspaces.len(), workspace_count);
        assert_eq!(workspace.tasks.len(), task_count + 1);
        assert_eq!(workspace.active_task().expect("task").title, "New Tab 2");
        assert_eq!(state.command_bar_text, "");
    }

    #[test]
    fn sidebar_task_items_flag_external_urls() {
        let state = AppState::demo();
        let tasks = state.sidebar_task_items();

        assert_eq!(tasks.len(), 3);
        // Task 1: Launcher → SystemIcon
        assert_eq!(
            tasks[0].icon,
            SidebarTaskIconSpec::SystemIcon(SystemPageIcon::Launcher)
        );
        // Task 2: CapsuleHandle web pane → Monogram
        assert_eq!(
            tasks[1].icon,
            SidebarTaskIconSpec::Monogram("GU".to_string())
        );
        // Task 3: ExternalUrl
        assert_eq!(
            tasks[2].icon,
            SidebarTaskIconSpec::ExternalUrl {
                origin: "https://ato.run".to_string()
            }
        );
    }

    #[test]
    fn omnibar_suggestions_include_settings_and_matching_tasks() {
        let state = AppState::demo();

        let suggestions = state.omnibar_suggestions("ato");

        assert!(suggestions
            .iter()
            .any(|item| matches!(item.action, OmnibarSuggestionAction::Navigate { .. })));
        assert!(suggestions.iter().any(|item| matches!(
            item.action,
            OmnibarSuggestionAction::SelectTask { task_id: 3 }
        )));
    }

    #[test]
    fn empty_omnibar_suggestions_include_settings() {
        let state = AppState::demo();

        let suggestions = state.omnibar_suggestions("");

        assert!(suggestions
            .iter()
            .any(|item| matches!(item.action, OmnibarSuggestionAction::ShowSettings)));
    }

    #[test]
    fn open_local_registry_navigates_to_loopback_store() {
        let mut state = AppState::demo();

        state.open_local_registry();

        let pane = state.active_web_pane().expect("pane");
        assert_eq!(pane.route.to_string(), "http://127.0.0.1:8787/");
        assert_eq!(state.command_bar_text, "http://127.0.0.1:8787/");
    }

    #[test]
    fn open_cloud_dock_without_auth_enters_handoff() {
        let mut state = AppState::demo();

        state.open_cloud_dock();

        let pane = state
            .active_task()
            .and_then(|task| task.focused_pane())
            .expect("pane");
        assert!(matches!(pane.surface, PaneSurface::AuthHandoff { .. }));
        assert_eq!(
            state.desktop_auth.status,
            DesktopAuthStatus::AwaitingBrowser
        );
        assert_eq!(
            state.pending_post_login_target,
            Some(PendingPostLoginTarget::CloudDock)
        );
        assert_eq!(
            state.auth_sessions.last().expect("session").start_url,
            "https://ato.run/auth?next=%2Fdock&desktop_return=ato%3A%2F%2Fauth%2Fcallback%2Fcloud-dock"
        );
    }

    #[test]
    fn host_route_dock_callback_restores_session_and_opens_personal_dock() {
        let mut state = AppState::demo();
        state.open_cloud_dock();

        state.handle_host_route("ato://auth/callback/dock/koh0920");

        let pane = state.active_web_pane().expect("pane");
        assert_eq!(pane.route.to_string(), "https://ato.run/dock/koh0920");
        assert_eq!(state.desktop_auth.status, DesktopAuthStatus::SignedIn);
        assert_eq!(
            state.desktop_auth.publisher_handle.as_deref(),
            Some("koh0920")
        );
        assert!(state.pending_post_login_target.is_none());
        assert_eq!(
            state.auth_sessions.last().expect("session").status,
            AuthSessionStatus::Completed
        );
    }

    #[test]
    fn host_route_authenticated_callback_falls_back_to_dock_index() {
        let mut state = AppState::demo();
        state.open_cloud_dock();

        state.handle_host_route("ato://auth/callback/authenticated");

        let pane = state.active_web_pane().expect("pane");
        assert_eq!(pane.route.to_string(), "https://ato.run/dock");
        assert_eq!(state.desktop_auth.status, DesktopAuthStatus::SignedIn);
        assert!(state.desktop_auth.publisher_handle.is_none());
    }

    #[test]
    fn host_route_error_marks_desktop_auth_as_failed() {
        let mut state = AppState::demo();
        state.open_cloud_dock();

        state.handle_host_route("ato://auth/callback/error");

        let pane = state
            .active_task()
            .and_then(|task| task.focused_pane())
            .expect("pane");
        assert!(matches!(pane.surface, PaneSurface::AuthHandoff { .. }));
        assert_eq!(state.desktop_auth.status, DesktopAuthStatus::Failed);
        assert_eq!(
            state.auth_sessions.last().expect("session").status,
            AuthSessionStatus::Failed
        );
    }

    #[test]
    fn navigate_to_share_url_opens_unified_cli_repl_panel() {
        // New design: share URLs open in the unified ato://cli REPL panel
        // with the share URL as an auto-executed prelude. This is the only
        // share-URL path now — the legacy CapsuleHandle/Resolving pane is
        // gone (dead code retained in `navigate_to_url` is unreachable).
        let mut state = AppState::demo();
        state.navigate_to_url("https://ato.run/s/abc123xyz");

        let workspace = state.active_workspace().expect("workspace");
        let task = workspace.tasks.last().expect("task");
        let pane = task
            .panes
            .iter()
            .find(|p| p.id == task.focused_pane)
            .expect("focused pane");

        // Share URL → Terminal pane (unified ato://cli REPL).
        let session_id = match &pane.surface {
            PaneSurface::Terminal(term) => term.session_id.clone(),
            other => panic!("expected Terminal surface for share URL, got {other:?}"),
        };

        // Title must carry the share short-id so the user still sees the
        // share origin in the tab label.
        assert!(
            pane.title.contains("share:abc123xy"),
            "title should include share short-id, got {:?}",
            pane.title
        );

        // The pending CLI spec must be AtoRunRepl with the share URL as
        // prelude and the share's host auto-allowed.
        let spec = crate::orchestrator::take_pending_cli_command(&session_id)
            .expect("pending CLI spec for share URL pane");
        match spec {
            crate::orchestrator::CliLaunchSpec::AtoRunRepl {
                prelude,
                initial_allow_hosts,
            } => {
                assert_eq!(
                    prelude.as_deref(),
                    Some("https://ato.run/s/abc123xyz"),
                    "prelude must be the share URL so the REPL auto-runs it"
                );
                assert!(
                    initial_allow_hosts.iter().any(|h| h == "ato.run"),
                    "share host must be in initial_allow_hosts, got {initial_allow_hosts:?}"
                );
            }
            other => panic!("expected AtoRunRepl, got {other:?}"),
        }
    }

    #[test]
    fn navigate_to_share_url_extracts_share_id_into_pane_title() {
        let mut state = AppState::demo();
        state.navigate_to_url("https://ato.run/s/myspecialrun");

        let workspace = state.active_workspace().expect("workspace");
        let task = workspace.tasks.last().expect("task");
        let pane = task
            .panes
            .iter()
            .find(|p| p.id == task.focused_pane)
            .expect("focused pane");

        // Short-id is the first 8 chars of the share segment.
        assert!(
            pane.title.contains("myspecia"),
            "title should contain share short-id, got {:?}",
            pane.title
        );
        assert!(matches!(pane.surface, PaneSurface::Terminal(_)));
    }

    #[test]
    fn navigate_share_url_is_idempotent_and_opens_fresh_panel_each_time() {
        // The share URL flow creates a new tab each navigation (there is no
        // per-pane "retry" state in the REPL world — each `ato> <url>` run
        // is just another submit). Confirm that back-to-back navigations
        // produce distinct pending CLI specs.
        let mut state = AppState::demo();
        let initial_tasks = state.active_workspace().expect("workspace").tasks.len();

        state.navigate_to_url("https://ato.run/s/abc123xyz");
        state.navigate_to_url("https://ato.run/s/abc123xyz");

        let tasks_after = state.active_workspace().expect("workspace").tasks.len();
        assert_eq!(
            tasks_after,
            initial_tasks + 2,
            "each share navigation creates a fresh tab"
        );
    }

    #[test]
    fn handle_host_route_open_deep_link_creates_new_tab_and_navigates() {
        let mut state = AppState::demo();
        let initial_task_count = state.active_workspace().expect("workspace").tasks.len();

        // Note: capsule://ato.run/acme/chat percent-encoded for the query param
        state.handle_host_route("ato://open?handle=capsule%3A%2F%2Fato.run%2Facme%2Fchat");

        let workspace = state.active_workspace().expect("workspace");
        assert_eq!(workspace.tasks.len(), initial_task_count + 1);
        let pane = state.active_web_pane().expect("pane");
        // GuestRoute::CapsuleHandle.to_string() returns the label (= display_string of canonical)
        assert!(pane.route.to_string().contains("acme") && pane.route.to_string().contains("chat"));
        assert_eq!(pane.session, WebSessionState::Resolving);
    }

    #[test]
    fn handle_host_route_open_with_share_url_routes_to_unified_cli_panel() {
        // New design: `ato://open?handle=<share-url>` routes the share URL
        // through `navigate_to_url`, which now opens the unified ato://cli
        // REPL panel (Terminal surface) rather than a Resolving Web pane.
        let mut state = AppState::demo();

        state.handle_host_route("ato://open?handle=https%3A%2F%2Fato.run%2Fs%2Fabc123");

        let workspace = state.active_workspace().expect("workspace");
        let task = workspace.tasks.last().expect("task");
        let pane = task
            .panes
            .iter()
            .find(|p| p.id == task.focused_pane)
            .expect("focused pane");
        assert!(
            matches!(pane.surface, PaneSurface::Terminal(_)),
            "share deep-link must open a Terminal (REPL) surface, got {:?}",
            pane.surface
        );
        assert!(
            pane.title.contains("share:abc123"),
            "pane title must carry the share short-id, got {:?}",
            pane.title
        );
    }

    #[test]
    fn handle_host_route_open_without_handle_param_does_not_navigate() {
        let mut state = AppState::demo();
        let initial_task_count = state.active_workspace().expect("workspace").tasks.len();

        state.handle_host_route("ato://open");

        // No new tab created; activity log should note the missing param
        let workspace = state.active_workspace().expect("workspace");
        assert_eq!(workspace.tasks.len(), initial_task_count);
        assert!(state.activity.iter().any(|e| e.message.contains("handle")));
    }

    #[test]
    fn handle_host_route_ato_cli_opens_new_terminal_tab_with_ato_run_repl() {
        let mut state = AppState::demo();
        let initial_task_count = state.active_workspace().expect("workspace").tasks.len();

        state.handle_host_route("ato://cli");

        let workspace = state.active_workspace().expect("workspace");
        assert_eq!(
            workspace.tasks.len(),
            initial_task_count + 1,
            "ato://cli should open a new tab"
        );

        let task = workspace.tasks.last().expect("newly opened task");
        let pane = task
            .panes
            .iter()
            .find(|p| p.id == task.focused_pane)
            .expect("focused pane");

        // The focused pane should carry a Terminal surface.
        let session_id = match &pane.surface {
            PaneSurface::Terminal(term) => term.session_id.clone(),
            other => panic!("expected Terminal surface, got {other:?}"),
        };
        assert!(
            session_id.starts_with("cli-"),
            "expected cli- prefixed session id, got {session_id}"
        );

        // The pending CLI spec must be registered for the webview render path.
        let spec = crate::orchestrator::take_pending_cli_command(&session_id)
            .expect("pending CLI spec must be registered before pane is mounted");
        assert!(matches!(
            spec,
            crate::orchestrator::CliLaunchSpec::AtoRunRepl { .. }
        ));

        assert!(state
            .activity
            .iter()
            .any(|e| e.message.contains("ato://cli")));
    }

    #[test]
    fn handle_host_route_ato_cli_with_cmd_bash_uses_raw_shell() {
        let mut state = AppState::demo();

        state.handle_host_route("ato://cli?cmd=bash");

        let workspace = state.active_workspace().expect("workspace");
        let task = workspace.tasks.last().expect("task");
        let pane = task
            .panes
            .iter()
            .find(|p| p.id == task.focused_pane)
            .expect("focused pane");

        let session_id = match &pane.surface {
            PaneSurface::Terminal(term) => term.session_id.clone(),
            other => panic!("expected Terminal surface, got {other:?}"),
        };

        let spec =
            crate::orchestrator::take_pending_cli_command(&session_id).expect("pending CLI spec");
        match spec {
            crate::orchestrator::CliLaunchSpec::RawShell(shell) => {
                assert_eq!(shell, "bash")
            }
            other => panic!("expected RawShell(bash), got {other:?}"),
        }
    }

    #[test]
    fn handle_host_route_ato_cli_with_cmd_ato_uses_raw_ato() {
        let mut state = AppState::demo();
        state.handle_host_route("ato://cli?cmd=ato");

        let workspace = state.active_workspace().expect("workspace");
        let task = workspace.tasks.last().expect("task");
        let pane = task
            .panes
            .iter()
            .find(|p| p.id == task.focused_pane)
            .expect("focused pane");

        let session_id = match &pane.surface {
            PaneSurface::Terminal(term) => term.session_id.clone(),
            other => panic!("expected Terminal surface, got {other:?}"),
        };

        let spec =
            crate::orchestrator::take_pending_cli_command(&session_id).expect("pending CLI spec");
        assert!(matches!(spec, crate::orchestrator::CliLaunchSpec::RawAto));
    }

    #[test]
    fn launch_dropped_paths_creates_one_tab_per_path() {
        let mut state = AppState::demo();
        let initial_task_count = state.active_workspace().expect("workspace").tasks.len();

        state.launch_dropped_paths(vec![
            std::path::PathBuf::from("/home/user/project-a"),
            std::path::PathBuf::from("/home/user/project-b"),
        ]);

        let workspace = state.active_workspace().expect("workspace");
        assert_eq!(workspace.tasks.len(), initial_task_count + 2);
    }

    #[test]
    fn launch_dropped_paths_with_empty_list_is_noop() {
        let mut state = AppState::demo();
        let initial_task_count = state.active_workspace().expect("workspace").tasks.len();

        state.launch_dropped_paths(vec![]);

        let workspace = state.active_workspace().expect("workspace");
        assert_eq!(workspace.tasks.len(), initial_task_count);
    }

    #[test]
    fn drain_browser_commands_only_returns_matching_pane() {
        let mut state = AppState::demo();
        state.browser_commands.push_back(BrowserCommand {
            pane_id: 1,
            kind: BrowserCommandKind::Back,
        });
        state.browser_commands.push_back(BrowserCommand {
            pane_id: 2,
            kind: BrowserCommandKind::Reload,
        });

        let commands = state.drain_browser_commands(1);
        assert_eq!(commands, vec![BrowserCommandKind::Back]);
        assert_eq!(
            state.browser_commands.into_iter().collect::<Vec<_>>(),
            vec![BrowserCommand {
                pane_id: 2,
                kind: BrowserCommandKind::Reload
            }]
        );
    }

    #[test]
    fn ato_open_url_query_param_is_percent_decoded_correctly() {
        // Regression: url crate must correctly decode ato://open?handle=... query params.
        // Specifically, capsule%3A%2F%2Fato.run (single 't') must not become atto.run.
        let raw = "ato://open?handle=capsule%3A%2F%2Fato.run%2Facme%2Fchat";
        let url = url::Url::parse(raw).expect("ato:// URL should parse");
        let handle = url
            .query_pairs()
            .find(|(k, _)| k == "handle")
            .map(|(_, v)| v.into_owned());
        assert_eq!(handle.as_deref(), Some("capsule://ato.run/acme/chat"));
    }

    // ────────────────────────────────────────────────────────────────────────
    // E2E: real share URL state-machine test (unified ato://cli REPL)
    //
    // Under the unified CLI design, pasting a share URL into the omnibar
    // opens an `ato://cli` REPL panel whose prelude auto-executes the share
    // URL. This test verifies that the Terminal pane is created with the
    // correct pending CliLaunchSpec (prelude = share URL, host auto-allow).
    // ────────────────────────────────────────────────────────────────────────
    #[test]
    fn e2e_share_url_state_routes_to_unified_cli_repl() {
        const SHARE_URL: &str = "https://ato.run/s/01KP5WDF81SQQTVZRF88RNY8MR";

        let mut state = AppState::demo();
        state.navigate_to_url(SHARE_URL);

        let workspace = state.active_workspace().expect("workspace");
        let task = workspace.tasks.last().expect("task");
        let pane = task
            .panes
            .iter()
            .find(|p| p.id == task.focused_pane)
            .expect("focused pane");

        let session_id = match &pane.surface {
            PaneSurface::Terminal(term) => term.session_id.clone(),
            other => panic!("expected Terminal surface for share URL, got {other:?}"),
        };

        // Pane title carries the share short-id so users can tell which
        // capsule this panel was opened for.
        assert!(
            pane.title.contains("share:01KP5WDF"),
            "pane title must carry share short-id, got {:?}",
            pane.title
        );

        // The pending CliLaunchSpec must be AtoRunRepl with:
        //  - prelude = the exact share URL (so the REPL auto-runs it)
        //  - initial_allow_hosts containing the share's host (ato.run)
        let spec = crate::orchestrator::take_pending_cli_command(&session_id)
            .expect("pending CLI spec must be registered for share URL pane");
        match spec {
            crate::orchestrator::CliLaunchSpec::AtoRunRepl {
                prelude,
                initial_allow_hosts,
            } => {
                assert_eq!(
                    prelude.as_deref(),
                    Some(SHARE_URL),
                    "prelude must equal the share URL so the REPL runs it"
                );
                assert!(
                    initial_allow_hosts.iter().any(|h| h == "ato.run"),
                    "share host must be in initial_allow_hosts, got {initial_allow_hosts:?}"
                );
            }
            other => panic!("expected AtoRunRepl, got {other:?}"),
        }
    }
}
