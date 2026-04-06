use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use url::Url;

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
        let store =
            GuestRoute::ExternalUrl(Url::parse("https://store.ato.run").expect("valid url"));
        let wry = GuestRoute::ExternalUrl(
            Url::parse("https://github.com/tauri-apps/wry").expect("valid url"),
        );

        let welcome_task = TaskSet {
            id: 1,
            title: "Guest surfaces".to_string(),
            focused_pane: 1,
            pane_tree: PaneTree::Leaf(1),
            panes: vec![Pane {
                id: 1,
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
            id: 2,
            title: "Ato store".to_string(),
            focused_pane: 2,
            pane_tree: PaneTree::Leaf(2),
            panes: vec![Pane {
                id: 2,
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
            preview: "remote URL in the shell".to_string(),
        };

        Self {
            shell_mode: ShellMode::Focus,
            active_workspace: 1,
            workspaces: vec![Workspace {
                id: 1,
                title: "Rust host".to_string(),
                active_task: 1,
                tasks: vec![welcome_task, store_task],
            }],
            command_bar_text: "capsule://local/desky-real-tauri".to_string(),
            activity: vec![ActivityEntry {
                tone: ActivityTone::Info,
                message: "Phase 3 shell bootstrapped with ato-cli guest orchestration".to_string(),
            }],
        }
    }

    pub fn focus_command_bar(&mut self) {
        self.shell_mode = ShellMode::CommandBar;
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
            PaneSurface::Native { .. } => None,
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
        self.command_bar_text = self
            .active_web_pane()
            .map(|pane| pane.route.to_string())
            .unwrap_or_default();
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
        self.command_bar_text = self
            .active_web_pane()
            .map(|pane| pane.route.to_string())
            .unwrap_or_default();
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

    #[test]
    fn cycle_handle_changes_route() {
        let mut state = AppState::demo();
        let before = state.active_web_pane().expect("pane").route.to_string();
        state.cycle_handle();
        let after = state.active_web_pane().expect("pane").route.to_string();
        assert_ne!(before, after);
    }
}
