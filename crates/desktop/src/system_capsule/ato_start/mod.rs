//! `ato-start` system capsule — the "new window" start page.
//!
//! This capsule replaces the previously inlined mock HTML in
//! `assets/system/ato-windows/start.html` with a proper system
//! capsule served from `assets/system/ato-start/index.html`.
//!
//! ## Snapshot injection
//!
//! Real data (open windows, recent capsules, local apps) is pre-injected
//! as `window.__ATO_START_SNAPSHOT__` via Wry's `with_initialization_script`
//! at window construction time. This avoids a request-response IPC
//! round-trip and sidesteps the async evaluate_script callback timing
//! hazard documented in AGENTS.md.
//!
//! `LoadStartSnapshot` is kept as an IPC command for future dynamic
//! refresh but its handler is currently a documented no-op.
//!
//! ## History
//!
//! `StartPageHistoryStore` persists recent capsule launches to
//! `~/.ato/start-history.json`. It is updated by `ato_launch::dispatch`
//! after a successful Approve.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use gpui::{AnyWindowHandle, App};
use serde::{Deserialize, Serialize};

use crate::localization::{LocaleCode, tr};
use crate::state::GuestRoute;
use crate::state::session::{
    DesktopSessionKind, PresentationState, SessionRegistry, SessionViewEntry,
};
use crate::system_capsule::broker::{BrokerError, Capability};
use crate::window::content_windows::OpenContentWindows;

// ─── Command enum ────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AtoStartCommand {
    /// Pre-fetch the start snapshot for initial render. Currently a
    /// documented no-op because data is pre-injected via
    /// `with_initialization_script`. Retained for future dynamic refresh.
    LoadStartSnapshot {
        #[serde(rename = "requestId")]
        request_id: u64,
    },
    /// Interpret a free-form query string as a capsule handle, external
    /// URL, or local path and dispatch to the appropriate action.
    OpenQuery { value: String },
    /// Open a capsule via the launch consent flow. Requires `WebviewCreate`.
    OpenCapsule { handle: String },
    /// Open the ato-store system capsule. Requires `LaunchSystemCapsule`.
    OpenStore,
    /// Open the ato-settings system capsule. Requires `LaunchSystemCapsule`.
    OpenSettings,
    /// Open a local directory as a capsule. Requires `WebviewCreate`.
    OpenLocalPath { path: String },
    /// Open the GitHub Run wizard (`Run from GitHub`). Requires `WebviewCreate`.
    OpenGithubRun,
    /// Open the Community Import review surface for a Featured App. Queries
    /// the community registry for published recipes matching `source` and
    /// lets the user pick one — instead of routing the GitHub handle to the
    /// infer surface. Requires `WebviewCreate`.
    OpenCommunityImport {
        source: String,
        #[serde(default)]
        label: Option<String>,
    },
    /// Close the start window. Requires `WindowsClose`.
    Close,
    /// Quit the whole desktop application. Requires `AppQuit`. This is the
    /// explicit exit affordance surfaced as the quit button on the Start
    /// page — the only user-facing way to terminate the app in Focus View
    /// on platforms without a native app menu.
    Quit,
    /// Launch a session for an installed app profile via the Runtime Control
    /// API.  On success, the session URL is opened as a native app window.
    /// Requires `RuntimeControl`.
    RuntimeLaunchSession {
        install_profile_key: String,
        #[serde(default)]
        target_label: Option<String>,
    },
    /// Stop a running session via the Runtime Control API.
    /// Requires `RuntimeControl`.
    RuntimeStopSession { session_id: String },
    /// Open a session URL as a native app window.
    /// Requires `WebviewCreate`.
    RuntimeOpenSessionUrl { url: String },
}

impl AtoStartCommand {
    pub fn required_capability(&self) -> Capability {
        match self {
            AtoStartCommand::LoadStartSnapshot { .. } => Capability::WindowsList,
            AtoStartCommand::OpenQuery { .. } => Capability::WebviewCreate,
            AtoStartCommand::OpenCapsule { .. } => Capability::WebviewCreate,
            AtoStartCommand::OpenStore => Capability::LaunchSystemCapsule,
            AtoStartCommand::OpenSettings => Capability::LaunchSystemCapsule,
            AtoStartCommand::OpenLocalPath { .. } => Capability::WebviewCreate,
            AtoStartCommand::OpenGithubRun => Capability::WebviewCreate,
            AtoStartCommand::OpenCommunityImport { .. } => Capability::WebviewCreate,
            AtoStartCommand::Close => Capability::WindowsClose,
            AtoStartCommand::Quit => Capability::AppQuit,
            AtoStartCommand::RuntimeLaunchSession { .. } => Capability::RuntimeControl,
            AtoStartCommand::RuntimeStopSession { .. } => Capability::RuntimeControl,
            AtoStartCommand::RuntimeOpenSessionUrl { .. } => Capability::WebviewCreate,
        }
    }
}

// ─── Query classification ─────────────────────────────────────────────────────

#[derive(Debug, Eq, PartialEq)]
pub enum QueryIntent {
    CapsuleHandle(String),
    ExternalUrl(String),
    LocalPath(String),
    Invalid(String),
}

/// Classify a free-form query string into one of four intents.
///
/// - `capsule://...` or `github.com/...` → `CapsuleHandle`
/// - `http://...` or `https://...` → `ExternalUrl`
/// - `~/...` or an absolute `/...` path → `LocalPath`
/// - known featured sample aliases → canonical GitHub `CapsuleHandle`
/// - Anything else → `Invalid`
pub fn classify_query(value: &str) -> QueryIntent {
    let v = value.trim();
    if v.starts_with("capsule://") || v.starts_with("github.com/") {
        QueryIntent::CapsuleHandle(v.to_string())
    } else if v.starts_with("http://") || v.starts_with("https://") {
        QueryIntent::ExternalUrl(v.to_string())
    } else if v.starts_with("~/") || v.starts_with('/') {
        QueryIntent::LocalPath(v.to_string())
    } else if let Some(handle) = featured_sample_alias_to_github(v) {
        QueryIntent::CapsuleHandle(handle.to_string())
    } else {
        QueryIntent::Invalid(format!(
            "'{}' は有効な入力ではありません。capsule:// / github.com/owner/repo / https:// / ~/path のいずれかで入力してください。",
            v
        ))
    }
}

/// Derive a human-readable label for the Community Import window title
/// from a source handle (`github.com/excalidraw/excalidraw` → `excalidraw`).
/// Falls back to the whole trimmed string when there is no `/`.
fn community_label_from_source(source: &str) -> String {
    source
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(source)
        .to_string()
}

fn featured_sample_alias_to_github(value: &str) -> Option<&'static str> {
    match value {
        "affine" => Some("github.com/toeverything/AFFiNE"),
        "open-webui" => Some("github.com/open-webui/open-webui"),
        "excalidraw" => Some("github.com/excalidraw/excalidraw"),
        _ => None,
    }
}

// ─── StartPageHistoryStore ───────────────────────────────────────────────────

/// A single entry in the start-page recent-capsules history.
///
/// `install_profile_key` / `app_url` carry the **install-owned identity** of an
/// entry that came from an installed app. When present, the entry can be
/// relaunched through its stable profile key (`ato launch <ipk>`) and the
/// Desktop opens its stable [`app_url`](capsule::foundation::install_lifecycle::derive_app_url)
/// instead of treating the `handle` as a fresh `ato run` target. Both are
/// `Option` and serde-defaulted so history files written before this field
/// existed (handle-only entries) continue to load unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StartHistoryEntry {
    /// Capsule handle string (e.g. `github.com/owner/repo`). Always present;
    /// used for display and as the legacy relaunch key when no install
    /// identity is known.
    pub handle: String,
    /// Human-readable label shown in the recent row.
    pub label: String,
    /// Unix timestamp (seconds) of the most recent open.
    pub last_opened_at: u64,
    /// Stable install profile key (`ipk_<32hex>`) when this entry is an
    /// installed app. `None` for legacy / non-installed entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_profile_key: Option<String>,
    /// Stable app URL (`ato://app/<ipk>`) derived from `install_profile_key`.
    /// Revision/port-independent open identity. `None` for legacy entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_url: Option<String>,
}

/// Persistent store for the start-page recent-capsule list.
///
/// Stored at `~/.ato/start-history.json`. At most 20 entries,
/// ordered most-recently-opened first.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct StartPageHistoryStore {
    pub entries: Vec<StartHistoryEntry>,
}

const MAX_HISTORY: usize = 20;

impl StartPageHistoryStore {
    /// Load from `~/.ato/start-history.json`. Returns an empty store
    /// if the file does not exist or cannot be parsed (non-fatal).
    pub fn load() -> Self {
        let path = match history_path() {
            Ok(p) => p,
            Err(_) => return Self::default(),
        };
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => return Self::default(),
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    /// Persist to disk. Silently drops errors (non-fatal for the caller).
    pub fn save(&self) -> Result<()> {
        let path = history_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(self)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }

    /// Upsert an entry by `handle`. If the handle already exists,
    /// its `last_opened_at` and `label` are updated. Entries are sorted
    /// descending by `last_opened_at` and capped at `MAX_HISTORY`.
    ///
    /// This is the legacy / non-installed path: it does not touch the
    /// install-owned identity fields. An existing entry's
    /// `install_profile_key` / `app_url` are preserved (so a legacy
    /// re-open never *downgrades* an installed entry).
    pub fn record_open(&mut self, handle: &str, label: &str) {
        self.upsert(handle, label, None, None);
    }

    /// Upsert an installed-app entry, stamping its stable install identity.
    /// Use this when the open resolved to an `install_profile_key` so future
    /// relaunches go through the installed-app launch path.
    pub fn record_open_installed(
        &mut self,
        handle: &str,
        label: &str,
        install_profile_key: &str,
        app_url: &str,
    ) {
        self.upsert(
            handle,
            label,
            Some(install_profile_key.to_string()),
            Some(app_url.to_string()),
        );
    }

    fn upsert(
        &mut self,
        handle: &str,
        label: &str,
        install_profile_key: Option<String>,
        app_url: Option<String>,
    ) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Some(existing) = self.entries.iter_mut().find(|e| e.handle == handle) {
            existing.label = label.to_string();
            existing.last_opened_at = now;
            // Only upgrade install identity; never clear an existing one with a
            // legacy (None) re-open.
            if install_profile_key.is_some() {
                existing.install_profile_key = install_profile_key;
                existing.app_url = app_url;
            }
        } else {
            self.entries.push(StartHistoryEntry {
                handle: handle.to_string(),
                label: label.to_string(),
                last_opened_at: now,
                install_profile_key,
                app_url,
            });
        }
        self.entries
            .sort_by_key(|e| std::cmp::Reverse(e.last_opened_at));
        self.entries.truncate(MAX_HISTORY);
    }
}

fn history_path() -> anyhow::Result<PathBuf> {
    capsule::common::paths::ato_path("start-history.json").map_err(anyhow::Error::from)
}

// ─── Local app scanner ───────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct LocalAppInfo {
    pub path: String,
    pub name: String,
}

const MAX_LOCAL_APPS: usize = 30;
const MAX_SCAN_DEPTH: usize = 3;

/// Walk `root` up to `MAX_SCAN_DEPTH` levels deep, collecting
/// directories that contain a `capsule.toml`. Returns at most
/// `MAX_LOCAL_APPS` results.
pub fn scan_local_apps(root: &Path) -> Vec<LocalAppInfo> {
    let mut results = Vec::new();
    scan_dir(root, 0, &mut results);
    results
}

fn scan_dir(dir: &Path, depth: usize, out: &mut Vec<LocalAppInfo>) {
    if depth >= MAX_SCAN_DEPTH || out.len() >= MAX_LOCAL_APPS {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_LOCAL_APPS {
            return;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // Skip hidden directories
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.starts_with('.'))
            .unwrap_or(false)
        {
            continue;
        }
        if path.join("capsule.toml").exists() {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string();
            let display_path = path.to_string_lossy().to_string();
            out.push(LocalAppInfo {
                path: display_path,
                name,
            });
        } else {
            scan_dir(&path, depth + 1, out);
        }
    }
}

// ─── Snapshot ────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct OpenWindowSnapshot {
    pub id: u64,
    pub title: String,
    pub subtitle: String,
    pub url: String,
    pub kind: String,
}

/// One running app/session row for the Start page "開いているアプリ" list.
///
/// Unlike [`OpenWindowSnapshot`] (which mirrors visible GPUI windows), this is
/// derived from the [`SessionRegistry`] — the single source of truth for
/// running capsule sessions. A session appears here whether it is shown in a
/// window, opened in the OS browser, or running headless in the background, so
/// OCI (Docker/Podman) sessions and window-less source sessions are both
/// represented. Multi-service OCI apps collapse to one row per session.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RunningAppSnapshot {
    pub session_id: String,
    pub display_name: String,
    pub handle: String,
    /// `"source"` for native source runtimes, `"oci"` for container sessions.
    pub runtime_kind: String,
    /// Number of OCI services backing the session, when known (OCI only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_count: Option<usize>,
    /// Lifecycle status: `running` | `background` | `failed` | `stopped`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_window_id: Option<u64>,
    /// Whether a visible AppWindow is currently bound to the session.
    pub has_window: bool,
}

#[derive(Debug, Serialize)]
pub struct StartSnapshot {
    pub open_windows: Vec<OpenWindowSnapshot>,
    /// Running sessions (source + OCI) from the `SessionRegistry`. This is the
    /// source of truth for the Start page "開いているアプリ" row — a session
    /// stays listed while its process runs even if its window is closed.
    pub running_apps: Vec<RunningAppSnapshot>,
    pub recent_capsules: Vec<StartHistoryEntry>,
    pub local_apps: Vec<LocalAppInfo>,
    pub featured_apps: Vec<FeaturedApp>,
    /// Base URL for the `ato serve` Runtime Control API.
    /// JS can call `${runtime_base_url}/v1/runtime/sessions` etc. directly.
    pub runtime_base_url: String,
}

/// Map one `SessionViewEntry` (registry view model) to a Start-page running app
/// row. Pure function so the mapping is unit-testable without an `App`.
pub fn running_app_from_entry(entry: &SessionViewEntry) -> RunningAppSnapshot {
    let (runtime_kind, service_count) = match &entry.session_kind {
        DesktopSessionKind::NativeSource => ("source".to_string(), None),
        DesktopSessionKind::Oci { service_count, .. } => ("oci".to_string(), Some(*service_count)),
    };
    let status = match entry.presentation_state {
        PresentationState::Visible | PresentationState::External => "running",
        PresentationState::Detached | PresentationState::Headless => "background",
        PresentationState::Failed => "failed",
        PresentationState::Stopped => "stopped",
    }
    .to_string();
    RunningAppSnapshot {
        session_id: entry.session_id.clone(),
        display_name: entry.title.clone(),
        handle: entry.handle.clone(),
        runtime_kind,
        service_count,
        status,
        primary_url: entry.local_url.clone(),
        primary_window_id: entry.primary_window_id,
        has_window: entry.primary_window_id.is_some(),
    }
}

/// Build the running-app list from the live `SessionRegistry`. Stopped sessions
/// are excluded so the list reflects only what is actually running. Returns an
/// empty list when the registry global is not installed (e.g. early boot).
pub fn build_running_apps(cx: &App) -> Vec<RunningAppSnapshot> {
    if !cx.has_global::<SessionRegistry>() {
        return Vec::new();
    }
    let apps: Vec<RunningAppSnapshot> = cx
        .global::<SessionRegistry>()
        .view_entries()
        .iter()
        .filter(|entry| entry.presentation_state != PresentationState::Stopped)
        .map(running_app_from_entry)
        .collect();
    let oci = apps.iter().filter(|a| a.runtime_kind == "oci").count();
    tracing::info!(
        total = apps.len(),
        oci,
        source = apps.len() - oci,
        "ato_start: running app list updated from session registry"
    );
    apps
}

#[derive(Debug, Serialize)]
pub struct FeaturedApp {
    pub handle: String,
    pub label: String,
    pub description: String,
    pub icon: String,
    pub icon_bg: String,
    pub tags: Vec<String>,
    pub rating: f32,
    pub installs: u32,
    pub installed: bool,
}

/// Build a start snapshot from current app state. Called at window
/// construction time; injected as `window.__ATO_START_SNAPSHOT__`.
pub fn build_start_snapshot(
    cx: &App,
    config: &crate::config::DesktopConfig,
    locale: LocaleCode,
) -> StartSnapshot {
    let open_windows = if cx.has_global::<OpenContentWindows>() {
        cx.global::<OpenContentWindows>()
            .mru_order()
            .into_iter()
            .map(|e| {
                let kind_str = match &e.kind {
                    crate::window::content_windows::ContentWindowKind::AppWindow { .. } => {
                        "AppWindow"
                    }
                    crate::window::content_windows::ContentWindowKind::Store => "Store",
                    crate::window::content_windows::ContentWindowKind::Start => "Start",
                    crate::window::content_windows::ContentWindowKind::Settings => "Settings",
                    crate::window::content_windows::ContentWindowKind::Dock => "Dock",
                    crate::window::content_windows::ContentWindowKind::Onboarding => "Onboarding",
                    crate::window::content_windows::ContentWindowKind::Launch => "Launch",
                    crate::window::content_windows::ContentWindowKind::Import => "Import",
                    crate::window::content_windows::ContentWindowKind::Auth => "Auth",
                };
                OpenWindowSnapshot {
                    id: e.handle.window_id().as_u64(),
                    title: e.title.to_string(),
                    subtitle: e.subtitle.to_string(),
                    url: e.url.to_string(),
                    kind: kind_str.to_string(),
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    let running_apps = build_running_apps(cx);

    let recent_capsules = StartPageHistoryStore::load().entries;

    let workspace_root_raw = &config.runtime.workspace_root;
    let workspace_root_expanded = expand_tilde(workspace_root_raw);
    let local_apps = scan_local_apps(&workspace_root_expanded);

    let runtime_base_url = crate::runtime_control_client::RuntimeControlClient::new(
        config.registry.local_registry_port,
    )
    .base_url()
    .to_string();

    StartSnapshot {
        open_windows,
        running_apps,
        recent_capsules,
        local_apps,
        featured_apps: static_featured_apps(locale),
        runtime_base_url,
    }
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

fn static_featured_apps(locale: LocaleCode) -> Vec<FeaturedApp> {
    vec![
        FeaturedApp {
            handle: "github.com/toeverything/AFFiNE".to_string(),
            label: "AFFiNE".to_string(),
            description: tr(locale, "start.featured.affine_desc"),
            icon: "△".to_string(),
            icon_bg: "linear-gradient(135deg,#fb7185,#e11d48)".to_string(),
            tags: vec![
                tr(locale, "start.featured.tag.local_run"),
                tr(locale, "start.featured.tag.offline"),
            ],
            rating: 4.7,
            installs: 3200,
            installed: false,
        },
        FeaturedApp {
            handle: "github.com/open-webui/open-webui".to_string(),
            label: "Open WebUI".to_string(),
            description: tr(locale, "start.featured.open_webui_desc"),
            icon: "OI".to_string(),
            icon_bg: "linear-gradient(135deg,#0f172a,#334155)".to_string(),
            tags: vec![
                tr(locale, "start.featured.tag.local_run"),
                tr(locale, "start.featured.tag.privacy"),
            ],
            rating: 4.8,
            installs: 5800,
            installed: false,
        },
        FeaturedApp {
            handle: "github.com/excalidraw/excalidraw".to_string(),
            label: "Excalidraw".to_string(),
            description: tr(locale, "start.featured.excalidraw_desc"),
            icon: "✏️".to_string(),
            icon_bg: "linear-gradient(135deg,#f472b6,#e11d48)".to_string(),
            tags: vec![tr(locale, "start.featured.tag.local_run")],
            rating: 4.6,
            installs: 1600,
            installed: false,
        },
    ]
}

// ─── Dispatch ────────────────────────────────────────────────────────────────

pub fn dispatch(
    cx: &mut App,
    host: AnyWindowHandle,
    command: AtoStartCommand,
) -> Result<(), BrokerError> {
    match command {
        AtoStartCommand::LoadStartSnapshot { request_id: _ } => {
            // No-op for Phase 1: data is pre-injected via
            // `with_initialization_script` in `start_window::open_start_window`.
            // This command is reserved for future dynamic refresh.
            tracing::debug!("ato_start: LoadStartSnapshot (no-op in Phase 1)");
        }

        AtoStartCommand::OpenQuery { value } => match classify_query(&value) {
            QueryIntent::CapsuleHandle(handle) => {
                // GitHub repo inputs (`github.com/owner/repo`) route to
                // the GitHub Import review surface rather than the
                // capsule consent flow. Non-GitHub handles continue to
                // the launch consent path.
                if let Ok(normalized) =
                    crate::source_import_session::normalize_github_import_input(&handle)
                {
                    let source_url = normalized.source_url_normalized.clone();
                    crate::system_capsule::ipc::defer_after_dispatch(cx, move |cx| {
                        let _ = host.update(cx, |_, window, _| window.remove_window());
                        if let Err(err) =
                            crate::window::import_window::open_with_url(cx, source_url)
                        {
                            tracing::error!(error = %err, "ato_start: open_query GitHub import failed");
                        }
                    });
                    return Ok(());
                }
                let route = GuestRoute::CapsuleHandle {
                    handle: handle.clone(),
                    label: handle.clone(),
                    community_toml_id: None,
                };
                crate::system_capsule::ipc::defer_after_dispatch(cx, move |cx| {
                    let _ = host.update(cx, |_, window, _| window.remove_window());
                    open_capsule_from_start(cx, route, &handle);
                });
            }
            QueryIntent::ExternalUrl(url_str) => match url::Url::parse(&url_str) {
                Ok(url) => {
                    // https://github.com/owner/repo from the URL form
                    // also routes to GitHub Import.
                    if let Ok(normalized) =
                        crate::source_import_session::normalize_github_import_input(&url_str)
                    {
                        let source_url = normalized.source_url_normalized.clone();
                        crate::system_capsule::ipc::defer_after_dispatch(cx, move |cx| {
                            let _ = host.update(cx, |_, window, _| window.remove_window());
                            if let Err(err) =
                                crate::window::import_window::open_with_url(cx, source_url)
                            {
                                tracing::error!(error = %err, "ato_start: open_query GitHub URL failed");
                            }
                        });
                        return Ok(());
                    }
                    let route = GuestRoute::ExternalUrl(url);
                    crate::system_capsule::ipc::defer_after_dispatch(cx, move |cx| {
                        let _ = host.update(cx, |_, window, _| window.remove_window());
                        if let Err(err) = crate::window::open_app_window(cx, route) {
                            tracing::error!(error = %err, "ato_start: open_query ExternalUrl failed");
                        }
                    });
                }
                Err(err) => {
                    tracing::warn!(url = %url_str, error = %err, "ato_start: open_query URL parse failed");
                }
            },
            QueryIntent::LocalPath(path) => {
                let label = Path::new(&path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("local")
                    .to_string();
                let route = GuestRoute::CapsuleHandle {
                    handle: path,
                    label,
                    community_toml_id: None,
                };
                crate::system_capsule::ipc::defer_after_dispatch(cx, move |cx| {
                    let _ = host.update(cx, |_, window, _| window.remove_window());
                    if let Err(err) =
                        crate::window::launch_window::open_consent_window_for_route(cx, route)
                    {
                        tracing::error!(error = %err, "ato_start: open_query LocalPath failed");
                    }
                });
            }
            QueryIntent::Invalid(_msg) => {
                // Validation error: no action, no fallback.
                // The HTML page handles UI feedback via the snippet
                // already included in the initialization script.
                tracing::debug!(value = %value, "ato_start: open_query invalid input (no-op)");
            }
        },

        AtoStartCommand::OpenCapsule { handle } => {
            let route = GuestRoute::CapsuleHandle {
                handle: handle.clone(),
                label: handle.clone(),
                community_toml_id: None,
            };
            crate::system_capsule::ipc::defer_after_dispatch(cx, move |cx| {
                let _ = host.update(cx, |_, window, _| window.remove_window());
                open_capsule_from_start(cx, route, &handle);
            });
        }

        AtoStartCommand::OpenStore => {
            crate::system_capsule::ipc::defer_after_dispatch(cx, move |cx| {
                let _ = host.update(cx, |_, window, _| window.remove_window());
                if let Err(err) = crate::window::store::open_store_window(cx) {
                    tracing::error!(error = %err, "ato_start: open_store failed");
                }
            });
        }

        AtoStartCommand::OpenSettings => {
            crate::system_capsule::ipc::defer_after_dispatch(cx, move |cx| {
                let _ = host.update(cx, |_, window, _| window.remove_window());
                if let Err(err) = crate::window::settings_window::open_settings_window(cx) {
                    tracing::error!(error = %err, "ato_start: open_settings failed");
                }
            });
        }

        AtoStartCommand::OpenLocalPath { path } => {
            let label = Path::new(&path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("local")
                .to_string();
            let route = GuestRoute::CapsuleHandle {
                handle: path,
                label,
                community_toml_id: None,
            };
            crate::system_capsule::ipc::defer_after_dispatch(cx, move |cx| {
                let _ = host.update(cx, |_, window, _| window.remove_window());
                if let Err(err) =
                    crate::window::launch_window::open_consent_window_for_route(cx, route)
                {
                    tracing::error!(error = %err, "ato_start: open_local_path failed");
                }
            });
        }

        AtoStartCommand::OpenGithubRun => {
            crate::system_capsule::ipc::defer_after_dispatch(cx, move |cx| {
                let _ = host.update(cx, |_, window, _| window.remove_window());
                if let Err(err) = crate::window::launch_window::open_github_run_window(cx) {
                    tracing::error!(error = %err, "ato_start: open_github_run failed");
                }
            });
        }

        AtoStartCommand::OpenCommunityImport { source, label } => {
            let label = label.unwrap_or_else(|| community_label_from_source(&source));
            crate::system_capsule::ipc::defer_after_dispatch(cx, move |cx| {
                let _ = host.update(cx, |_, window, _| window.remove_window());
                if let Err(err) =
                    crate::window::community_import_window::open_for_source(cx, source, label)
                {
                    tracing::error!(error = %err, "ato_start: open_community_import failed");
                }
            });
        }

        AtoStartCommand::Close => {
            crate::system_capsule::ipc::defer_after_dispatch(cx, move |cx| {
                let _ = host.update(cx, |_, window, _| window.remove_window());
            });
        }

        AtoStartCommand::Quit => {
            // Latch shutdown BEFORE quitting so the `on_window_closed`
            // handler does not race to reopen the Start landing surface
            // as GPUI tears the windows down.
            crate::window::begin_shutdown();
            crate::system_capsule::ipc::defer_after_dispatch(cx, move |cx| {
                let count = cx
                    .global_mut::<crate::state::session::SessionRegistry>()
                    .stop_all_running();
                tracing::info!(count, "ato_start: quit — stopped running sessions");
                tracing::info!("ato_start: quit requested from Start page — quitting app");
                cx.quit();
            });
        }

        AtoStartCommand::RuntimeLaunchSession {
            install_profile_key,
            target_label,
        } => {
            let async_app = cx.to_async();
            let fe = cx.foreground_executor().clone();
            let be = cx.background_executor().clone();
            let port = cx
                .try_global::<crate::config::LocalRegistryPort>()
                .map(|g| g.0)
                .unwrap_or_else(crate::config::default_local_registry_port);

            fe.spawn(async move {
                let key = install_profile_key.clone();
                let label = target_label.clone();
                let result = be
                    .spawn(async move {
                        crate::runtime_control_client::RuntimeControlClient::new(port)
                            .launch_session(&key, label.as_deref())
                    })
                    .await;

                crate::webview_init_guard::wait_until_idle(&be).await;
                async_app.update(|cx| match result {
                    Ok(resp) => {
                        tracing::info!(
                            session_id = %resp.session_id,
                            url = ?resp.user_visible_url,
                            "ato_start: runtime_launch_session succeeded"
                        );
                        if let Some(url_str) = resp.user_visible_url {
                            match url::Url::parse(&url_str) {
                                Ok(url) => {
                                    let route = GuestRoute::ExternalUrl(url);
                                    if let Err(err) = crate::window::open_app_window(cx, route) {
                                        tracing::error!(
                                            error = %err,
                                            "ato_start: open session url failed"
                                        );
                                    }
                                }
                                Err(err) => tracing::warn!(
                                    url = %url_str,
                                    error = %err,
                                    "ato_start: session url parse failed"
                                ),
                            }
                        }
                    }
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            install_profile_key = %install_profile_key,
                            "ato_start: runtime_launch_session failed"
                        );
                    }
                });
            })
            .detach();
        }

        AtoStartCommand::RuntimeStopSession { session_id } => {
            let be = cx.background_executor().clone();
            let port = cx
                .try_global::<crate::config::LocalRegistryPort>()
                .map(|g| g.0)
                .unwrap_or_else(crate::config::default_local_registry_port);

            be.spawn(async move {
                let result = crate::runtime_control_client::RuntimeControlClient::new(port)
                    .stop_session(&session_id);
                match result {
                    Ok(()) => tracing::info!(
                        session_id = %session_id,
                        "ato_start: runtime_stop_session succeeded"
                    ),
                    Err(err) => tracing::error!(
                        error = %err,
                        session_id = %session_id,
                        "ato_start: runtime_stop_session failed"
                    ),
                }
            })
            .detach();
        }

        AtoStartCommand::RuntimeOpenSessionUrl { url } => match url::Url::parse(&url) {
            Ok(parsed) => {
                let route = GuestRoute::ExternalUrl(parsed);
                crate::system_capsule::ipc::defer_after_dispatch(cx, move |cx| {
                    if let Err(err) = crate::window::open_app_window(cx, route) {
                        tracing::error!(
                            error = %err,
                            "ato_start: runtime_open_session_url failed"
                        );
                    }
                });
            }
            Err(err) => {
                tracing::warn!(url = %url, error = %err, "ato_start: runtime_open_session_url — invalid URL");
            }
        },
    }
    Ok(())
}

/// Shared entry point for `OpenCapsule` (Recent Capsules click) and
/// `OpenQuery::CapsuleHandle` (URL bar handle re-entry).
///
/// Routing follows an explicit [`DesktopLaunchIntent`](crate::launch_intent::DesktopLaunchIntent)
/// boundary rather than always falling through to the consent wizard:
///
/// 1. **Live session** → reuse it instantly (no launch, no review).
/// 2. **Installed profile** (known via Start history or recovered from the
///    install store) → launch through the install-owned, pre-consented
///    `ato launch <install_profile_key>` path with **no consent wizard** — this
///    is the fix for installed apps re-showing review on every relaunch.
/// 3. **Otherwise** (not installed / ambiguous) → the existing consent flow.
fn open_capsule_from_start(cx: &mut App, route: GuestRoute, handle: &str) {
    use crate::launch_intent::DesktopLaunchIntent;
    use crate::state::session::SessionRegistry;

    // 1. Live session → focus/reuse (unchanged behavior).
    match crate::orchestrator::try_reuse_live_session_for_click(handle) {
        Ok(Some(session)) => {
            // Recover stored non-secret configs from the existing session so
            // a subsequent restart still has the correct values (e.g. MODEL=,
            // PORT=). If the session has not been registered yet (race), we
            // fall back to empty — the capsule is already running so configs
            // are already applied.
            let launch_configs = cx
                .global::<SessionRegistry>()
                .get_session(&session.session_id)
                .map(|s| s.launch_context.launch_configs.clone())
                .unwrap_or_default();

            if let Err(err) = crate::window::orchestrator::open_ready_capsule_window(
                cx,
                route,
                session,
                launch_configs,
            ) {
                tracing::error!(error = %err, handle, "ato_start: open_capsule ready-window failed");
            }
            return;
        }
        Ok(None) => {}
        Err(err) => {
            tracing::debug!(
                error = %err,
                handle,
                "ato_start: session fast-path failed; resolving launch intent"
            );
        }
    }

    // 2. Not running → resolve how to open it.
    match resolve_open_intent(handle) {
        DesktopLaunchIntent::LaunchInstalledProfile {
            install_profile_key,
            app_url,
            ..
        } => {
            // Persist the durable install identity up-front. A later legacy
            // `record_open` (from start_boot_launch's history write) preserves
            // it, so the entry never downgrades to handle-only.
            record_installed_history(&route, &install_profile_key, &app_url);
            tracing::info!(
                handle,
                %install_profile_key,
                %app_url,
                "ato_start: opening installed app via launch intent (no consent wizard)"
            );
            match crate::window::launch_window::open_boot_window(cx, Some(&route)) {
                Ok(boot_handle) => {
                    crate::window::launch_window::start_installed_launch(
                        cx,
                        route,
                        install_profile_key,
                        boot_handle,
                    );
                }
                Err(err) => {
                    tracing::error!(
                        error = %err,
                        handle,
                        "ato_start: open_boot_window for installed launch failed; \
                         falling back to consent flow"
                    );
                    if let Err(err) =
                        crate::window::launch_window::open_consent_window_for_route(cx, route)
                    {
                        tracing::error!(error = %err, handle, "ato_start: consent fallback failed");
                    }
                }
            }
        }
        // InstallThenLaunch / LegacyTryOpen (and the unreachable FocusSession,
        // already handled above): first-run / non-installed opens still go
        // through the existing consent flow.
        _ => {
            if let Err(err) = crate::window::launch_window::open_consent_window_for_route(cx, route)
            {
                tracing::error!(error = %err, handle, "ato_start: open_capsule consent fallback failed");
            }
        }
    }
}

/// Open an installed app by its durable `install_profile_key` — the identity
/// behind an `ato://app/<ipk>` URL (#261). This is the deep-link / automation
/// entry point that mirrors what a Start-window tile click does, but keyed by
/// the unambiguous install identity rather than a capsule handle, so it never
/// mis-resolves a handle shared by two installs.
///
/// Flow (same boundaries as [`open_capsule_from_start`], minus the consent
/// wizard — an installed profile is pre-consented):
/// 1. reverse-resolve the ipk to a launchable target (canonical handle + url);
/// 2. live session for that ipk → reuse instantly (no relaunch);
/// 3. otherwise record the durable identity, open the boot window, and run the
///    install-owned `ato launch <ipk>` path via `start_installed_launch`.
///
/// Returns `Err` only when the ipk does not resolve to a launchable installed
/// profile (unknown / degraded) or the store is unreadable — the caller surfaces
/// that rather than silently degrading to a handle launch + consent wizard.
pub(crate) fn open_installed_app_by_ipk(cx: &mut App, app_url_or_ipk: &str) -> Result<()> {
    use crate::state::session::SessionRegistry;

    // Accepts either an `ato://app/<ipk>` URL or a bare `ipk_…` — the resolver
    // strips the prefix when present.
    let target = crate::launch_intent::installed_target_for_app_url(app_url_or_ipk)
        .context("resolve install_profile_key against install store")?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no installed app matches '{app_url_or_ipk}' \
                 (not installed, or the install is degraded)"
            )
        })?;

    let label = target
        .handle
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(&target.handle)
        .to_string();
    let route = GuestRoute::CapsuleHandle {
        handle: target.handle.clone(),
        label,
        community_toml_id: None,
    };

    // 1. Live session keyed by ipk → reuse instantly (no relaunch, no boot
    //    window). Matches by install_profile_key, so a session whose handle has
    //    drifted from the record is still found.
    match crate::orchestrator::try_reuse_live_session_for_install_profile_key(
        &target.install_profile_key,
    ) {
        Ok(Some(session)) => {
            let launch_configs = cx
                .global::<SessionRegistry>()
                .get_session(&session.session_id)
                .map(|s| s.launch_context.launch_configs.clone())
                .unwrap_or_default();
            crate::window::orchestrator::open_ready_capsule_window(
                cx,
                route,
                session,
                launch_configs,
            )
            .context("open ready window for reused installed session")?;
            return Ok(());
        }
        Ok(None) => {}
        Err(err) => {
            tracing::debug!(
                error = %err,
                install_profile_key = %target.install_profile_key,
                "ato_start: ipk session fast-path failed; starting installed launch"
            );
        }
    }

    // 2. Not running → record durable identity, open boot window, launch.
    record_installed_history(&route, &target.install_profile_key, &target.app_url);
    tracing::info!(
        handle = %target.handle,
        install_profile_key = %target.install_profile_key,
        app_url = %target.app_url,
        "ato_start: opening installed app by ipk (no consent wizard)"
    );
    let boot_handle = crate::window::launch_window::open_boot_window(cx, Some(&route))
        .context("open boot window for installed launch")?;
    crate::window::launch_window::start_installed_launch(
        cx,
        route,
        target.install_profile_key,
        boot_handle,
    );
    Ok(())
}

/// Resolve the launch intent for a not-running handle by consulting Start
/// history (for a previously-stamped install identity) and the install store
/// (to recover identity for legacy handle-only history entries). The live
/// session case is handled by the caller, so `live_session_id` is `None` here.
fn resolve_open_intent(handle: &str) -> crate::launch_intent::DesktopLaunchIntent {
    use crate::launch_intent::{
        InstalledMatch, IntentInputs, installed_match_for_handle, resolve_launch_intent,
    };

    let history_install = StartPageHistoryStore::load()
        .entries
        .into_iter()
        .find(|e| e.handle == handle)
        .and_then(|e| match (e.install_profile_key, e.app_url) {
            (Some(ipk), Some(url)) => Some((ipk, url)),
            _ => None,
        });

    let installed_match = match open_install_store() {
        Ok(store) => installed_match_for_handle(&store, handle).unwrap_or(InstalledMatch::None),
        Err(err) => {
            tracing::debug!(error = %err, handle, "ato_start: install store unavailable for intent resolution");
            InstalledMatch::None
        }
    };

    resolve_launch_intent(IntentInputs {
        handle: handle.to_string(),
        live_session_id: None,
        history_install,
        installed_match,
    })
}

fn open_install_store()
-> anyhow::Result<capsule::foundation::install_lifecycle::InstallInstanceStore> {
    let root = capsule::common::paths::ato_path_or_workspace_tmp("instances");
    capsule::foundation::install_lifecycle::InstallInstanceStore::new(&root)
}

/// Persist an installed-app open with its stable install identity. Mirrors
/// `launch_window::record_start_history` but stamps `install_profile_key` /
/// `app_url` so future relaunches resolve straight to the installed-profile
/// launch path.
fn record_installed_history(route: &GuestRoute, install_profile_key: &str, app_url: &str) {
    let item = match route {
        GuestRoute::CapsuleHandle { handle, label, .. }
        | GuestRoute::CapsuleUrl { handle, label, .. } => Some((handle.as_str(), label.as_str())),
        _ => None,
    };
    if let Some((handle, label)) = item {
        let mut store = StartPageHistoryStore::load();
        store.record_open_installed(handle, label, install_profile_key, app_url);
        if let Err(err) = store.save() {
            tracing::warn!(error = %err, "ato_start: failed to save installed start history");
        }
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_capsule_handle_prefix() {
        assert_eq!(
            classify_query("capsule://github.com/owner/repo"),
            QueryIntent::CapsuleHandle("capsule://github.com/owner/repo".to_string())
        );
    }

    #[test]
    fn classify_github_prefix() {
        assert_eq!(
            classify_query("github.com/owner/repo"),
            QueryIntent::CapsuleHandle("github.com/owner/repo".to_string())
        );
    }

    #[test]
    fn classify_http_url() {
        assert_eq!(
            classify_query("https://ato.run/"),
            QueryIntent::ExternalUrl("https://ato.run/".to_string())
        );
        assert_eq!(
            classify_query("http://localhost:3000"),
            QueryIntent::ExternalUrl("http://localhost:3000".to_string())
        );
    }

    #[test]
    fn classify_local_tilde() {
        assert_eq!(
            classify_query("~/projects/my-app"),
            QueryIntent::LocalPath("~/projects/my-app".to_string())
        );
    }

    #[test]
    fn classify_local_abs_path() {
        assert_eq!(
            classify_query("/Users/alice/dev/capsule"),
            QueryIntent::LocalPath("/Users/alice/dev/capsule".to_string())
        );
    }

    #[test]
    fn classify_featured_sample_aliases() {
        assert_eq!(
            classify_query("affine"),
            QueryIntent::CapsuleHandle("github.com/toeverything/AFFiNE".to_string())
        );
        assert_eq!(
            classify_query("open-webui"),
            QueryIntent::CapsuleHandle("github.com/open-webui/open-webui".to_string())
        );
        assert_eq!(
            classify_query("excalidraw"),
            QueryIntent::CapsuleHandle("github.com/excalidraw/excalidraw".to_string())
        );
    }

    #[test]
    fn static_featured_apps_use_github_handles() {
        let handles: Vec<_> = static_featured_apps(LocaleCode::En)
            .into_iter()
            .map(|app| app.handle)
            .collect();

        assert_eq!(
            handles,
            vec![
                "github.com/toeverything/AFFiNE".to_string(),
                "github.com/open-webui/open-webui".to_string(),
                "github.com/excalidraw/excalidraw".to_string(),
            ]
        );
    }

    #[test]
    fn classify_invalid_bare_string() {
        match classify_query("hello world") {
            QueryIntent::Invalid(_) => {}
            other => panic!("expected Invalid, got {:?}", other),
        }
    }

    #[test]
    fn classify_trims_whitespace() {
        assert_eq!(
            classify_query("  github.com/owner/repo  "),
            QueryIntent::CapsuleHandle("github.com/owner/repo".to_string())
        );
    }

    // ─ Community Import routing ──────────────────────────────────────────────

    #[test]
    fn community_label_derives_repo_name_from_source() {
        assert_eq!(
            community_label_from_source("github.com/excalidraw/excalidraw"),
            "excalidraw"
        );
        assert_eq!(
            community_label_from_source("github.com/toeverything/AFFiNE"),
            "AFFiNE"
        );
        // Trailing slash and bare strings.
        assert_eq!(community_label_from_source("owner/repo/"), "repo");
        assert_eq!(community_label_from_source("solo"), "solo");
    }

    #[test]
    fn featured_card_envelope_parses_to_open_community_import() {
        // This mirrors the IPC envelope the ato-start Featured Apps cards
        // now post: `{ kind: 'open_community_import', source, label }`.
        // Regression guard: Featured Apps must NOT post `open_query`
        // (which reroutes github.com handles to the GitHub infer surface).
        let cmd: AtoStartCommand = serde_json::from_str(
            r#"{"kind":"open_community_import","source":"github.com/excalidraw/excalidraw","label":"Excalidraw"}"#,
        )
        .expect("featured-card envelope must parse");
        match cmd {
            AtoStartCommand::OpenCommunityImport { source, label } => {
                assert_eq!(source, "github.com/excalidraw/excalidraw");
                assert_eq!(label.as_deref(), Some("Excalidraw"));
            }
            other => panic!("expected OpenCommunityImport, got {other:?}"),
        }
        assert_eq!(
            AtoStartCommand::OpenCommunityImport {
                source: String::new(),
                label: None
            }
            .required_capability(),
            Capability::WebviewCreate
        );
    }

    #[test]
    fn open_community_import_label_is_optional() {
        let cmd: AtoStartCommand = serde_json::from_str(
            r#"{"kind":"open_community_import","source":"github.com/open-webui/open-webui"}"#,
        )
        .expect("envelope without label must parse");
        match cmd {
            AtoStartCommand::OpenCommunityImport { label, .. } => assert!(label.is_none()),
            other => panic!("expected OpenCommunityImport, got {other:?}"),
        }
    }

    // ─ StartPageHistoryStore ─────────────────────────────────────────────────

    #[test]
    fn history_record_and_dedup() {
        let mut store = StartPageHistoryStore::default();
        store.record_open("github.com/a/b", "A/B");
        store.record_open("github.com/c/d", "C/D");
        // Record A/B again — should update, not duplicate
        store.record_open("github.com/a/b", "A/B updated");
        assert_eq!(store.entries.len(), 2);
        // Most recently opened is first
        assert_eq!(store.entries[0].handle, "github.com/a/b");
        assert_eq!(store.entries[0].label, "A/B updated");
    }

    #[test]
    fn history_caps_at_max() {
        let mut store = StartPageHistoryStore::default();
        for i in 0..25 {
            store.record_open(&format!("github.com/owner/repo-{}", i), "Repo");
        }
        assert_eq!(store.entries.len(), MAX_HISTORY);
    }

    // ─ RunningAppSnapshot mapping ────────────────────────────────────────────

    use crate::state::session::{
        DesktopSessionKind, OciImportKind, OciSessionStatus, PresentationState, SessionViewEntry,
    };

    fn source_entry(state: PresentationState, window: Option<u64>) -> SessionViewEntry {
        SessionViewEntry {
            session_id: "s-source".to_string(),
            title: "My App".to_string(),
            handle: "capsule://github.com/owner/repo".to_string(),
            presentation_state: state,
            attached_clients: Vec::new(),
            primary_window_id: window,
            local_url: Some("http://127.0.0.1:8080/".to_string()),
            session_kind: DesktopSessionKind::NativeSource,
        }
    }

    fn oci_entry(state: PresentationState) -> SessionViewEntry {
        SessionViewEntry {
            session_id: "s-oci".to_string(),
            title: "blinko".to_string(),
            handle: "/work/blinko".to_string(),
            presentation_state: state,
            attached_clients: Vec::new(),
            primary_window_id: None,
            local_url: Some("http://127.0.0.1:43123/".to_string()),
            session_kind: DesktopSessionKind::Oci {
                import_kind: OciImportKind::Compose,
                status: OciSessionStatus::Running,
                endpoint_url: Some("http://127.0.0.1:43123/".to_string()),
                service_count: 3,
                source_path: Some("/work/blinko".to_string()),
                source_hash: None,
            },
        }
    }

    #[test]
    fn running_app_maps_source_runtime_kind() {
        let app = running_app_from_entry(&source_entry(PresentationState::Visible, Some(7)));
        assert_eq!(app.runtime_kind, "source");
        assert_eq!(app.service_count, None);
        assert_eq!(app.status, "running");
        assert!(app.has_window);
        assert_eq!(app.primary_window_id, Some(7));
        assert_eq!(app.display_name, "My App");
    }

    #[test]
    fn running_app_maps_oci_session_once_with_service_count() {
        let app = running_app_from_entry(&oci_entry(PresentationState::Headless));
        assert_eq!(app.runtime_kind, "oci");
        // Multi-service OCI session collapses to one row carrying its count.
        assert_eq!(app.service_count, Some(3));
        // No visible window → background, not "running".
        assert_eq!(app.status, "background");
        assert!(!app.has_window);
    }

    #[test]
    fn running_app_detached_window_is_background_not_dropped() {
        // Closing a window detaches the client; the session keeps running and
        // must still surface as a background app (not removed).
        let app = running_app_from_entry(&source_entry(PresentationState::Detached, None));
        assert_eq!(app.status, "background");
        assert!(!app.has_window);
    }

    #[test]
    fn running_app_failed_and_stopped_status() {
        assert_eq!(
            running_app_from_entry(&oci_entry(PresentationState::Failed)).status,
            "failed"
        );
        assert_eq!(
            running_app_from_entry(&source_entry(PresentationState::Stopped, None)).status,
            "stopped"
        );
    }

    #[test]
    fn history_mru_order() {
        let mut store = StartPageHistoryStore::default();
        store.record_open("github.com/first/one", "First");
        // Tiny sleep would be needed for guaranteed timestamp diff, but we
        // rely on monotonically increasing UNIX seconds. For unit testing,
        // just verify the dedup path preserves order of last record_open.
        store.record_open("github.com/second/two", "Second");
        store.record_open("github.com/first/one", "First again");
        // After re-recording first/one, it should be at index 0
        assert_eq!(store.entries[0].handle, "github.com/first/one");
    }

    #[test]
    fn old_history_json_without_install_fields_loads() {
        // History files written before the install-identity fields existed
        // must still deserialize, with the new fields defaulting to None.
        let json = r#"{
            "entries": [
                { "handle": "github.com/owner/repo", "label": "Repo", "last_opened_at": 1700000000 }
            ]
        }"#;
        let store: StartPageHistoryStore =
            serde_json::from_str(json).expect("legacy history must deserialize");
        assert_eq!(store.entries.len(), 1);
        assert_eq!(store.entries[0].handle, "github.com/owner/repo");
        assert!(store.entries[0].install_profile_key.is_none());
        assert!(store.entries[0].app_url.is_none());
    }

    #[test]
    fn record_open_installed_stamps_identity() {
        let mut store = StartPageHistoryStore::default();
        store.record_open_installed("acme/hello", "Hello", "ipk_abc", "ato://app/ipk_abc");
        assert_eq!(store.entries.len(), 1);
        assert_eq!(
            store.entries[0].install_profile_key.as_deref(),
            Some("ipk_abc")
        );
        assert_eq!(
            store.entries[0].app_url.as_deref(),
            Some("ato://app/ipk_abc")
        );
    }

    #[test]
    fn legacy_record_open_does_not_clear_install_identity() {
        // A plain re-open of an already-installed entry must not downgrade it
        // back to a handle-only (legacy) entry.
        let mut store = StartPageHistoryStore::default();
        store.record_open_installed("acme/hello", "Hello", "ipk_abc", "ato://app/ipk_abc");
        store.record_open("acme/hello", "Hello reopened");
        assert_eq!(
            store.entries[0].install_profile_key.as_deref(),
            Some("ipk_abc"),
            "legacy record_open must preserve existing install identity"
        );
        assert_eq!(store.entries[0].label, "Hello reopened");
    }

    #[test]
    fn install_fields_round_trip_through_serde() {
        let mut store = StartPageHistoryStore::default();
        store.record_open_installed("acme/hello", "Hello", "ipk_abc", "ato://app/ipk_abc");
        store.record_open("github.com/legacy/one", "Legacy");
        let json = serde_json::to_string(&store).unwrap();
        let back: StartPageHistoryStore = serde_json::from_str(&json).unwrap();
        let installed = back
            .entries
            .iter()
            .find(|e| e.handle == "acme/hello")
            .unwrap();
        assert_eq!(installed.install_profile_key.as_deref(), Some("ipk_abc"));
        // The legacy entry must serialize without the optional fields (skip_serializing_if).
        assert!(!json.contains("\"install_profile_key\":null"));
    }
}
