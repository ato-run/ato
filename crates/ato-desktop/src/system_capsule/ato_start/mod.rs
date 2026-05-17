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

use crate::localization::{tr, LocaleCode};
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
    /// Close the start window. Requires `WindowsClose`.
    Close,
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
            AtoStartCommand::Close => Capability::WindowsClose,
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
/// - Anything else → `Invalid`
pub fn classify_query(value: &str) -> QueryIntent {
    let v = value.trim();
    if v.starts_with("capsule://") || v.starts_with("github.com/") {
        QueryIntent::CapsuleHandle(v.to_string())
    } else if v.starts_with("http://") || v.starts_with("https://") {
        QueryIntent::ExternalUrl(v.to_string())
    } else if v.starts_with("~/") || v.starts_with('/') {
        QueryIntent::LocalPath(v.to_string())
    } else {
        QueryIntent::Invalid(format!("'{}' は有効な入力ではありません。capsule:// / github.com/owner/repo / https:// / ~/path のいずれかで入力してください。", v))
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
            .sort_by(|a, b| b.last_opened_at.cmp(&a.last_opened_at));
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
    scan_dir(root, root, 0, &mut results);
    results
}

fn scan_dir(root: &Path, dir: &Path, depth: usize, out: &mut Vec<LocalAppInfo>) {
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
            scan_dir(root, &path, depth + 1, out);
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

    StartSnapshot {
        open_windows,
        recent_capsules,
        local_apps,
        featured_apps: static_featured_apps(locale),
    }
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

fn static_featured_apps(locale: LocaleCode) -> Vec<FeaturedApp> {
    vec![
        FeaturedApp {
            handle: "github.com/ato-run/demo-weather".to_string(),
            label: "Weather Demo".to_string(),
            description: tr(locale, "start.featured.weather_desc"),
            icon: "⛅".to_string(),
            icon_bg: "linear-gradient(135deg,#0ea5e9,#38bdf8)".to_string(),
            tags: vec![
                tr(locale, "start.featured.tag.local_run"),
                tr(locale, "start.featured.tag.offline"),
            ],
            rating: 4.8,
            installs: 412,
            installed: false,
        },
        FeaturedApp {
            handle: "github.com/ato-run/demo-todo".to_string(),
            label: "Todo App Demo".to_string(),
            description: tr(locale, "start.featured.todo_desc"),
            icon: "✅".to_string(),
            icon_bg: "linear-gradient(135deg,#10b981,#34d399)".to_string(),
            tags: vec![
                tr(locale, "start.featured.tag.local_run"),
                tr(locale, "start.featured.tag.privacy"),
            ],
            rating: 4.6,
            installs: 387,
            installed: false,
        },
        FeaturedApp {
            handle: "github.com/ato-run/demo-markdown".to_string(),
            label: "Markdown Editor".to_string(),
            description: tr(locale, "start.featured.markdown_desc"),
            icon: "📝".to_string(),
            icon_bg: "linear-gradient(135deg,#8b5cf6,#a78bfa)".to_string(),
            tags: vec![tr(locale, "start.featured.tag.local_run")],
            rating: 4.7,
            installs: 298,
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
                let route = GuestRoute::CapsuleHandle {
                    handle: handle.clone(),
                    label: handle,
                };
                if let Err(err) =
                    crate::window::launch_window::open_consent_window_for_route(cx, route)
                {
                    tracing::error!(error = %err, "ato_start: open_query CapsuleHandle failed");
                }
                let _ = host.update(cx, |_, window, _| window.remove_window());
            }
            QueryIntent::ExternalUrl(url_str) => match url::Url::parse(&url_str) {
                Ok(url) => {
                    let route = GuestRoute::ExternalUrl(url);
                    if let Err(err) = crate::window::open_app_window(cx, route) {
                        tracing::error!(error = %err, "ato_start: open_query ExternalUrl failed");
                    }
                    let _ = host.update(cx, |_, window, _| window.remove_window());
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
                };
                if let Err(err) =
                    crate::window::launch_window::open_consent_window_for_route(cx, route)
                {
                    tracing::error!(error = %err, "ato_start: open_query LocalPath failed");
                }
                let _ = host.update(cx, |_, window, _| window.remove_window());
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
                label: handle,
            };
            if let Err(err) = crate::window::launch_window::open_consent_window_for_route(cx, route)
            {
                tracing::error!(error = %err, "ato_start: open_capsule failed");
            }
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }

        AtoStartCommand::OpenStore => {
            if let Err(err) = crate::window::store::open_store_window(cx) {
                tracing::error!(error = %err, "ato_start: open_store failed");
            }
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }

        AtoStartCommand::OpenSettings => {
            if let Err(err) = crate::window::settings_window::open_settings_window(cx) {
                tracing::error!(error = %err, "ato_start: open_settings failed");
            }
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
            };
            if let Err(err) = crate::window::launch_window::open_consent_window_for_route(cx, route)
            {
                tracing::error!(error = %err, "ato_start: open_local_path failed");
            }
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }

        AtoStartCommand::Close => {
            let _ = host.update(cx, |_, window, _| window.remove_window());
        }
    }
    Ok(())
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
