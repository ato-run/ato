use std::collections::VecDeque;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use url::{form_urlencoded, Url};

use crate::bridge::ShellEvent;

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
}

impl CapabilityGrant {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReadFile => "read-file",
            Self::WorkspaceInfo => "workspace-info",
            Self::OpenExternal => "open-external",
            Self::ClipboardRead => "clipboard-read",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "read-file" => Some(Self::ReadFile),
            "workspace-info" => Some(Self::WorkspaceInfo),
            "open-external" => Some(Self::OpenExternal),
            "clipboard-read" => Some(Self::ClipboardRead),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuestRoute {
    Capsule { session: String, entry_path: String },
    ExternalUrl(Url),
    LocalCapsule { handle: String, label: String },
}

impl GuestRoute {
    pub fn label(&self) -> String {
        match self {
            Self::Capsule { session, .. } => format!("capsule://{session}/index.html"),
            Self::ExternalUrl(url) => url.as_str().to_string(),
            Self::LocalCapsule { label, .. } => format!("capsule://local/{label}"),
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
    Launching,
    Mounted,
    Closed,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneSurface {
    Web(WebPane),
    Native { body: String },
    Launcher,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebPane {
    pub route: GuestRoute,
    pub partition_id: String,
    pub session: WebSessionState,
    pub capabilities: Vec<CapabilityGrant>,
    pub profile: String,
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
pub enum SidebarTaskIconSpec {
    Monogram(String),
    ExternalUrl { origin: String },
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
    pub bounds: PaneBounds,
}

#[derive(Clone, Debug)]
pub struct AppState {
    pub shell_mode: ShellMode,
    pub active_workspace: WorkspaceId,
    pub workspaces: Vec<Workspace>,
    pub command_bar_text: String,
    pub activity: Vec<ActivityEntry>,
    pub browser_commands: VecDeque<BrowserCommand>,
    next_task_id: TaskSetId,
    next_pane_id: PaneId,
    next_new_tab_index: usize,
}

impl AppState {
    pub fn demo() -> Self {
        // The demo graph intentionally mixes local capsules, a bundled welcome page, and remote URLs
        // so the shell exercises every rendering path on boot.
        let local_tauri = GuestRoute::LocalCapsule {
            handle: demo_local_capsule("desky-real-tauri"),
            label: "desky-real-tauri".to_string(),
        };
        let local_electron = GuestRoute::LocalCapsule {
            handle: demo_local_capsule("desky-real-electron"),
            label: "desky-real-electron".to_string(),
        };
        let local_wails = GuestRoute::LocalCapsule {
            handle: demo_local_capsule("desky-real-wails"),
            label: "desky-real-wails".to_string(),
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
                }),
            }],
            split_ratio: 0.68,
            route_candidates: vec![store, wry],
            route_index: 0,
            preview: "ato.run landing page".to_string(),
        };

        Self {
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
            browser_commands: VecDeque::new(),
            next_task_id: 4,
            next_pane_id: 4,
            next_new_tab_index: 2,
        }
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

    pub fn toggle_overview(&mut self) {
        self.shell_mode = match self.shell_mode {
            ShellMode::Overview => ShellMode::Focus,
            _ => ShellMode::Overview,
        };
    }

    pub fn dismiss_transient(&mut self) {
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

    pub fn navigate_to_url(&mut self, input: &str) {
        let normalized = Self::normalize_input(input);
        let Ok(url) = Url::parse(&normalized) else {
            self.push_activity(
                ActivityTone::Error,
                format!("Unable to navigate to invalid URL: {input}"),
            );
            return;
        };

        let next_route = GuestRoute::ExternalUrl(url);
        let label = next_route.to_string();
        let partition_id = sanitize(&label);
        let mut navigated = false;

        if let Some(task) = self.active_task_mut() {
            if let Some(pane) = task.focused_pane_mut() {
                pane.title = label.clone();
                pane.surface = PaneSurface::Web(WebPane {
                    route: next_route.clone(),
                    partition_id,
                    session: WebSessionState::Launching,
                    capabilities: vec![CapabilityGrant::OpenExternal],
                    profile: "electron".to_string(),
                });
                navigated = true;
            }
        }

        if !navigated {
            self.push_activity(
                ActivityTone::Error,
                "No focused pane available for navigation",
            );
            return;
        }

        self.command_bar_text = label.clone();
        self.shell_mode = ShellMode::Focus;
        self.push_activity(ActivityTone::Info, format!("Navigating to {label}"));
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
                }
                ShellEvent::PermissionDenied {
                    pane_id,
                    capability,
                } => {
                    self.push_activity(
                        ActivityTone::Warning,
                        format!("Pane {pane_id} denied capability {capability}"),
                    );
                }
                ShellEvent::SessionClosed { pane_id } => {
                    self.sync_web_session_state(pane_id, WebSessionState::Closed);
                }
                ShellEvent::UrlChanged { pane_id, url } => {
                    let Ok(parsed) = Url::parse(&url) else {
                        continue;
                    };
                    let active_pane = self.active_web_pane().map(|pane| pane.pane_id);
                    self.update_pane(pane_id, |pane| {
                        pane.title = url.clone();
                        if let PaneSurface::Web(web) = &mut pane.surface {
                            web.route = GuestRoute::ExternalUrl(parsed.clone());
                            web.partition_id = sanitize(&url);
                            web.session = WebSessionState::Mounted;
                        }
                    });
                    if active_pane == Some(pane_id) {
                        self.command_bar_text = url;
                    }
                }
                ShellEvent::TitleChanged { pane_id, title } => {
                    self.update_pane(pane_id, |pane| {
                        pane.title = title.clone();
                    });
                }
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
                    title: "Capability inspector".to_string(),
                    role: PaneRole::Companion,
                    visible: true,
                    bounds: PaneBounds::empty(),
                    surface: PaneSurface::Native {
                        body: "Phase 2 keeps the primary pane live and uses this companion pane for diagnostics.".to_string(),
                    },
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
                bounds: pane.bounds,
            }),
            PaneSurface::Native { .. } | PaneSurface::Launcher => None,
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

    pub fn normalize_input(input: &str) -> String {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return "https://www.google.com".to_string();
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

    fn update_pane(&mut self, pane_id: PaneId, mut update: impl FnMut(&mut Pane)) {
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

    fn sync_command_bar_with_active_route(&mut self) {
        self.command_bar_text = self
            .active_web_pane()
            .map(|pane| pane.route.to_string())
            .unwrap_or_default();
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
            GuestRoute::Capsule { .. } | GuestRoute::LocalCapsule { .. } => {
                SidebarTaskIconSpec::Monogram(short_label(&task.title))
            }
        },
        PaneSurface::Native { .. } | PaneSurface::Launcher => {
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
        PaneSurface::Launcher => "Launchpad".to_string(),
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
        GuestRoute::LocalCapsule { label, .. } if label.contains("electron") => "electron",
        GuestRoute::LocalCapsule { label, .. } if label.contains("wails") => "wails",
        GuestRoute::ExternalUrl(_) => "electron",
        _ => "tauri",
    }
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
            AppState::normalize_input("example.com"),
            "https://example.com"
        );
        assert_eq!(
            AppState::normalize_input("hello world"),
            "https://www.google.com/search?q=hello%20world"
        );
        assert_eq!(AppState::normalize_input(""), "https://www.google.com");
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
    fn show_settings_panel_adds_new_settings_task() {
        let mut state = AppState::demo();
        let original_task_count = state.active_workspace().expect("workspace").tasks.len();
        let original_task_id = state.active_task().expect("task").id;

        state.show_settings_panel();

        let workspace = state.active_workspace().expect("workspace");
        assert_eq!(workspace.tasks.len(), original_task_count + 1);
        assert_eq!(workspace.active_task().expect("task").title, "Settings");

        let pane = workspace.active_task().and_then(|task| task.focused_pane()).expect("pane");
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
        assert_eq!(
            tasks[0].icon,
            SidebarTaskIconSpec::Monogram("NE".to_string())
        );
        assert_eq!(
            tasks[1].icon,
            SidebarTaskIconSpec::Monogram("GU".to_string())
        );
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

        assert!(suggestions.iter().any(|item| matches!(
            item.action,
            OmnibarSuggestionAction::Navigate { .. }
        )));
        assert!(suggestions.iter().any(|item| matches!(
            item.action,
            OmnibarSuggestionAction::SelectTask { task_id: 3 }
        )));
    }

    #[test]
    fn empty_omnibar_suggestions_include_settings() {
        let state = AppState::demo();

        let suggestions = state.omnibar_suggestions("");

        assert!(suggestions.iter().any(|item| matches!(
            item.action,
            OmnibarSuggestionAction::ShowSettings
        )));
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
}
