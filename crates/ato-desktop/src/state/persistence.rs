//! On-disk persistence for the sidebar tab graph.
//!
//! `~/.ato/desktop-tabs.json` stores enough to rebuild the rail
//! contents — task titles + the route each tab pointed at — so a
//! restart lands the user on the same tab layout. Volatile fields
//! (live `WebSessionState`, automation state, terminal session ids,
//! pane bounds) are intentionally NOT persisted; the orchestrator
//! re-launches sessions on demand the way it does for a fresh tab.
//!
//! Tabs with no clean restore representation (Launcher, Terminal,
//! Inspector, AuthHandoff, capsule:// routes, ato://cli) are skipped
//! so we never resurrect an inconsistent half-state.
//!
//! Schema is versioned (`version: u32`); a mismatch falls back to
//! `AppState::initial()` and the file is overwritten on the next save.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use url::Url;

use super::{
    AppState, CapabilityGrant, GuestRoute, Pane, PaneBounds, PaneRole, PaneTree, PaneSurface,
    TaskSet, WebPane, WebSessionState, Workspace,
};

const PERSIST_FILE_NAME: &str = "desktop-tabs.json";
const PERSIST_DIR_NAME: &str = ".ato";
const SCHEMA_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Debug, Clone)]
struct PersistedShell {
    version: u32,
    active_workspace: usize,
    workspaces: Vec<PersistedWorkspace>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct PersistedWorkspace {
    id: usize,
    title: String,
    active_task: usize,
    tasks: Vec<PersistedTask>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct PersistedTask {
    title: String,
    route: PersistedRoute,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum PersistedRoute {
    ExternalUrl { url: String },
    CapsuleHandle { handle: String, label: String },
    CapsuleUrl { handle: String, label: String, url: String },
}

impl PersistedRoute {
    fn from_route(route: &GuestRoute) -> Option<Self> {
        match route {
            GuestRoute::ExternalUrl(url) => Some(Self::ExternalUrl {
                url: url.as_str().to_string(),
            }),
            GuestRoute::CapsuleHandle { handle, label } => Some(Self::CapsuleHandle {
                handle: handle.clone(),
                label: label.clone(),
            }),
            GuestRoute::CapsuleUrl { handle, label, url } => Some(Self::CapsuleUrl {
                handle: handle.clone(),
                label: label.clone(),
                url: url.as_str().to_string(),
            }),
            // capsule://… session-bound routes and Terminal panes
            // depend on runtime state we cannot rebuild from disk.
            GuestRoute::Capsule { .. } | GuestRoute::Terminal { .. } => None,
        }
    }

    fn into_route(self) -> Option<GuestRoute> {
        match self {
            Self::ExternalUrl { url } => Url::parse(&url).ok().map(GuestRoute::ExternalUrl),
            Self::CapsuleHandle { handle, label } => {
                Some(GuestRoute::CapsuleHandle { handle, label })
            }
            Self::CapsuleUrl { handle, label, url } => Url::parse(&url).ok().map(|url| {
                GuestRoute::CapsuleUrl {
                    handle,
                    label,
                    url,
                }
            }),
        }
    }
}

fn persist_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(PERSIST_DIR_NAME).join(PERSIST_FILE_NAME))
}

fn snapshot_state(state: &AppState) -> PersistedShell {
    let workspaces = state
        .workspaces
        .iter()
        .map(|ws| PersistedWorkspace {
            id: ws.id,
            title: ws.title.clone(),
            active_task: ws.active_task,
            tasks: ws
                .tasks
                .iter()
                .filter_map(|task| {
                    // Pick the primary pane's route — this is the
                    // route the omnibar shows and what the user
                    // intuitively associates with the tab. Skip the
                    // task entirely when nothing is restorable.
                    let primary = task.panes.iter().find(|p| p.role == PaneRole::Primary)?;
                    let route = match &primary.surface {
                        PaneSurface::Web(web) => PersistedRoute::from_route(&web.route)?,
                        PaneSurface::CapsuleStatus(status) => {
                            PersistedRoute::from_route(&status.route)?
                        }
                        // Launcher / Terminal / DevConsole / Inspector / AuthHandoff:
                        // no useful restore representation.
                        _ => return None,
                    };
                    Some(PersistedTask {
                        title: task.title.clone(),
                        route,
                    })
                })
                .collect(),
        })
        .collect();

    PersistedShell {
        version: SCHEMA_VERSION,
        active_workspace: state.active_workspace,
        workspaces,
    }
}

/// Write the current tab graph to `~/.ato/desktop-tabs.json`. Best
/// effort — failure is logged but never propagated, mirroring how
/// `config::save_config` treats persistence as advisory.
pub(crate) fn save_tabs(state: &AppState) {
    let Some(path) = persist_path() else {
        return;
    };
    let shell = snapshot_state(state);
    let body = match serde_json::to_string_pretty(&shell) {
        Ok(s) => s,
        Err(err) => {
            warn!(error = %err, "failed to serialize desktop tabs");
            return;
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(err) = fs::create_dir_all(parent) {
            warn!(error = %err, dir = %parent.display(), "failed to create desktop-tabs dir");
            return;
        }
    }
    if let Err(err) = fs::write(&path, body) {
        warn!(error = %err, path = %path.display(), "failed to write desktop tabs");
    } else {
        debug!(path = %path.display(), "saved desktop tabs");
    }
}

/// Load `~/.ato/desktop-tabs.json` and reconstruct an `AppState`. On
/// any error (missing file, parse failure, schema mismatch, empty
/// workspace) returns `None` so the caller can fall back to
/// `AppState::initial()`.
pub(crate) fn load_tabs() -> Option<AppState> {
    let path = persist_path()?;
    let body = fs::read_to_string(&path).ok()?;
    let shell: PersistedShell = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(err) => {
            warn!(error = %err, path = %path.display(), "failed to parse desktop tabs");
            return None;
        }
    };
    if shell.version != SCHEMA_VERSION {
        warn!(
            version = shell.version,
            expected = SCHEMA_VERSION,
            "ignoring desktop tabs with unknown schema version"
        );
        return None;
    }
    if shell.workspaces.is_empty() {
        return None;
    }
    Some(rebuild_state(shell))
}

fn rebuild_state(shell: PersistedShell) -> AppState {
    let mut next_pane_id: usize = 1;
    let mut next_task_id: usize = 1;

    let workspaces: Vec<Workspace> = shell
        .workspaces
        .into_iter()
        .map(|ws| {
            let tasks: Vec<TaskSet> = ws
                .tasks
                .into_iter()
                .filter_map(|task| {
                    let route = task.route.into_route()?;
                    let pane_id = next_pane_id;
                    next_pane_id += 1;
                    let task_id = next_task_id;
                    next_task_id += 1;
                    Some(TaskSet {
                        id: task_id,
                        title: task.title,
                        focused_pane: pane_id,
                        pane_tree: PaneTree::Leaf(pane_id),
                        panes: vec![Pane {
                            id: pane_id,
                            title: route.to_string(),
                            role: PaneRole::Primary,
                            visible: true,
                            bounds: PaneBounds::empty(),
                            surface: PaneSurface::Web(WebPane {
                                route: route.clone(),
                                partition_id: derive_partition_id(&route),
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
                        route_candidates: vec![route],
                        route_index: 0,
                        preview: String::new(),
                    })
                })
                .collect();
            let active_task = if tasks.iter().any(|t| t.id == ws.active_task) {
                ws.active_task
            } else {
                tasks.first().map(|t| t.id).unwrap_or(0)
            };
            Workspace {
                id: ws.id,
                title: ws.title,
                active_task,
                tasks,
            }
        })
        .filter(|ws| !ws.tasks.is_empty())
        .collect();

    if workspaces.is_empty() {
        return AppState::initial();
    }

    let mut state = AppState::initial();
    state.active_workspace = if workspaces.iter().any(|w| w.id == shell.active_workspace) {
        shell.active_workspace
    } else {
        workspaces.first().map(|w| w.id).unwrap_or(1)
    };
    state.workspaces = workspaces;
    state.next_task_id = next_task_id;
    state.next_pane_id = next_pane_id;
    state.next_new_tab_index = state
        .workspaces
        .iter()
        .flat_map(|w| w.tasks.iter())
        .filter(|t| t.title.starts_with("New Tab"))
        .count()
        + 1;
    state.sync_command_bar_with_active_route();
    state
}

fn derive_partition_id(route: &GuestRoute) -> String {
    match route {
        GuestRoute::ExternalUrl(url) => url
            .host_str()
            .map(|h| h.replace('.', "_"))
            .unwrap_or_else(|| "external".to_string()),
        GuestRoute::CapsuleHandle { handle, .. } | GuestRoute::CapsuleUrl { handle, .. } => {
            handle.replace('/', "_")
        }
        _ => "default".to_string(),
    }
}
