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

use anyhow::Result;
use gpui::{AnyWindowHandle, App};
use serde::{Deserialize, Serialize};

use crate::localization::{LocaleCode, tr};
use crate::state::GuestRoute;
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StartHistoryEntry {
    /// Capsule handle string (e.g. `github.com/owner/repo`).
    pub handle: String,
    /// Human-readable label shown in the recent row.
    pub label: String,
    /// Unix timestamp (seconds) of the most recent open.
    pub last_opened_at: u64,
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
    pub fn record_open(&mut self, handle: &str, label: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Some(existing) = self.entries.iter_mut().find(|e| e.handle == handle) {
            existing.label = label.to_string();
            existing.last_opened_at = now;
        } else {
            self.entries.push(StartHistoryEntry {
                handle: handle.to_string(),
                label: label.to_string(),
                last_opened_at: now,
            });
        }
        self.entries
            .sort_by_key(|e| std::cmp::Reverse(e.last_opened_at));
        self.entries.truncate(MAX_HISTORY);
    }
}

fn history_path() -> anyhow::Result<PathBuf> {
    capsule_core::common::paths::ato_path("start-history.json").map_err(anyhow::Error::from)
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

#[derive(Debug, Serialize)]
pub struct StartSnapshot {
    pub open_windows: Vec<OpenWindowSnapshot>,
    pub recent_capsules: Vec<StartHistoryEntry>,
    pub local_apps: Vec<LocalAppInfo>,
    pub featured_apps: Vec<FeaturedApp>,
    /// Base URL for the `ato serve` Runtime Control API.
    /// JS can call `${runtime_base_url}/v1/runtime/sessions` etc. directly.
    pub runtime_base_url: String,
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
                let result =
                    crate::runtime_control_client::RuntimeControlClient::new(port)
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

        AtoStartCommand::RuntimeOpenSessionUrl { url } => {
            match url::Url::parse(&url) {
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
            }
        }
    }
    Ok(())
}

/// Try to open an already-running capsule session directly (no consent modal).
/// If no live session exists, fall back to the consent window.
///
/// This is the shared entry point for `OpenCapsule` (Recent Capsules click)
/// and `OpenQuery::CapsuleHandle` (URL bar handle re-entry) so both surfaces
/// behave consistently: if the capsule is already running, the window reopens
/// instantly; if it needs to be launched, the consent flow appears as normal.
fn open_capsule_from_start(cx: &mut App, route: GuestRoute, handle: &str) {
    use crate::state::session::SessionRegistry;

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
        }
        Ok(None) => {
            if let Err(err) = crate::window::launch_window::open_consent_window_for_route(cx, route)
            {
                tracing::error!(error = %err, handle, "ato_start: open_capsule consent fallback failed");
            }
        }
        Err(err) => {
            tracing::debug!(
                error = %err,
                handle,
                "ato_start: session fast-path failed; falling back to consent"
            );
            if let Err(err) = crate::window::launch_window::open_consent_window_for_route(cx, route)
            {
                tracing::error!(error = %err, handle, "ato_start: open_capsule consent fallback failed");
            }
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
}
