//! ShellTab — the view-model behind the Shell Icon Bar (the Focus-mode
//! Control Bar's replacement for the browser-style URL bar).
//!
//! A ShellTab is one icon in the floating pill:
//!   - the fixed leading **Ato Home** tab (the `ato-pwa` control surface —
//!     login, Discover, Run, runner settings all live there), and
//!   - one **Capsule** tab per open capsule AppWindow.
//!
//! Tabs are *derived* from [`OpenContentWindows`] on every render rather
//! than tracked in a parallel registry, so the bar can never drift from
//! the real window set. URLs / local origins are deliberately absent from
//! this model — the icon bar never shows them.

use std::path::PathBuf;

use gpui::SharedString;

use crate::launch_tracker::TrackedLaunch;
use crate::remote_runs::RemoteRun;
use crate::state::GuestRoute;
use crate::window::content_windows::{
    CapsuleWindowStatus, ContentWindowKind, OpenContentWindows,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellTabKind {
    /// The Ato PWA / Ato Home control surface — not a capsule.
    AtoHome,
    /// An open capsule session window.
    Capsule,
}

/// Lifecycle state surfaced as a badge on the tab icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellTabStatus {
    Running,
    Starting,
    /// Waiting on the user or an external condition (consent, billing,
    /// capacity) — a first-class visible state, never an endless spinner.
    Blocked,
    Error,
}

#[derive(Clone)]
pub struct ShellTab {
    pub kind: ShellTabKind,
    /// GPUI window id backing this tab. `None` for the Ato Home tab when
    /// no Home window is currently open (clicking it opens one), and for
    /// remote-run tabs (no local window yet).
    pub window_id: Option<u64>,
    /// For a tab with no local window (an app already running on another
    /// runner, or a ready launch): the URL to open when clicked.
    pub open_url: Option<String>,
    /// For a launch-backed tab: the tracked launch's id — the tab's
    /// identity while the launch has no window yet. A starting launch has
    /// neither `window_id` nor `open_url`; clicking it is a no-op.
    pub launch_id: Option<String>,
    /// Local cache file of the capsule's Store icon; `None` falls back to
    /// the letter avatar.
    pub icon_path: Option<PathBuf>,
    /// For a blocked launch tab: the first blocker kind (e.g.
    /// `billing_required`) shown in the diagnostic placeholder.
    pub blocked_reason: Option<String>,
    pub title: SharedString,
    /// Fallback avatar glyph — first letter of the capsule title.
    pub initial: SharedString,
    pub status: ShellTabStatus,
    pub is_active: bool,
}

/// True when `kind` is the Ato Home window — the dedicated Home shell,
/// or an ExternalUrl AppWindow whose initial route points at the
/// configured PWA origin.
pub fn is_ato_home_entry(kind: &ContentWindowKind, app_base_url: &str) -> bool {
    match kind {
        ContentWindowKind::Home => true,
        ContentWindowKind::AppWindow {
            route: GuestRoute::ExternalUrl(url),
        } => same_origin(url.as_str(), app_base_url),
        _ => false,
    }
}

/// True when `kind` should appear as a capsule tab in the icon bar:
/// capsule-backed AppWindows, plus ExternalUrl app windows that are NOT
/// the PWA Home itself (e.g. a cloud session window opened via
/// `ato://open?handle=<session-url>`). System surfaces (Store, Settings,
/// Dock, Launch, Import, Auth) are never tabs.
pub fn is_capsule_tab_entry(kind: &ContentWindowKind, app_base_url: &str) -> bool {
    match kind {
        ContentWindowKind::AppWindow {
            route: GuestRoute::ExternalUrl(url),
        } => !same_origin(url.as_str(), app_base_url),
        ContentWindowKind::AppWindow { .. } => true,
        _ => false,
    }
}

/// Display title for a remote run. `label` falls back to the raw run id
/// when the run has no source metadata — prefer the capsule's
/// `publisher/slug` slug in that case so the avatar letter is meaningful.
pub fn remote_run_title(run: &RemoteRun) -> String {
    if let Some(name) = run.display_name.as_deref().filter(|name| !name.is_empty()) {
        return name.to_string();
    }
    let label_is_run_id = run.label == run.id;
    if label_is_run_id
        && let Some(scoped) = run
            .capsule_scoped_id
            .as_deref()
            .and_then(|scoped| scoped.rsplit('/').next())
            .filter(|slug| !slug.is_empty())
    {
        return scoped.to_string();
    }
    run.label.clone()
}

/// Serving states count as Running; everything else still in the active
/// set (launching / provisioning / stopping…) shows the starting dot.
pub fn remote_run_status(status: &str) -> ShellTabStatus {
    match status {
        "running" | "ready" => ShellTabStatus::Running,
        _ => ShellTabStatus::Starting,
    }
}

/// Display title for a launch-backed tab — the `publisher/slug` ref's
/// slug; falls back to the launch id when the handoff carried no ref.
pub fn launch_tab_title(launch: &TrackedLaunch) -> String {
    launch
        .capsule_ref
        .rsplit('/')
        .next()
        .filter(|slug| !slug.is_empty())
        .unwrap_or(&launch.launch_id)
        .to_string()
}

/// Badge for a launch-backed tab. `None` means the launch is over
/// (cancelled / expired / stopped) and gets no tab at all. `blocked` is a
/// first-class visible state (the tracker pauses polling; the tab shows a
/// blocked badge and clicking it opens a diagnostic placeholder). Other
/// unknown future states (`stopping`, …) render honestly as still starting.
pub fn launch_tab_status(state: &str) -> Option<ShellTabStatus> {
    match state {
        "ready" => Some(ShellTabStatus::Running),
        "failed" => Some(ShellTabStatus::Error),
        "blocked" => Some(ShellTabStatus::Blocked),
        "cancelled" | "expired" | "stopped" => None,
        _ => Some(ShellTabStatus::Starting),
    }
}

/// Scheme + host + port comparison, tolerant of paths / trailing slashes.
fn same_origin(a: &str, b: &str) -> bool {
    match (url::Url::parse(a.trim()), url::Url::parse(b.trim())) {
        (Ok(a), Ok(b)) => a.origin() == b.origin(),
        _ => false,
    }
}

/// Map the capsule lifecycle status onto the badge shown on the tab.
/// A missing capsule context means the shell hasn't published its first
/// boot transition yet — render that honestly as still starting.
pub fn tab_status(capsule_status: Option<&CapsuleWindowStatus>) -> ShellTabStatus {
    match capsule_status {
        Some(CapsuleWindowStatus::Ready) => ShellTabStatus::Running,
        Some(CapsuleWindowStatus::Failed) => ShellTabStatus::Error,
        Some(CapsuleWindowStatus::Starting) | None => ShellTabStatus::Starting,
    }
}

/// First alphanumeric character of the title, uppercased — the fallback
/// avatar when no capsule icon asset is available.
pub fn avatar_initial(title: &str) -> String {
    title
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "·".to_string())
}

/// Deterministic accent hue (0.0–1.0) for a capsule's letter avatar so
/// each capsule keeps a stable colour across renders and restarts.
pub fn avatar_hue(title: &str) -> f32 {
    let mut hash: u32 = 2166136261;
    for byte in title.bytes() {
        hash ^= byte as u32;
        hash = hash.wrapping_mul(16777619);
    }
    (hash % 360) as f32 / 360.0
}

/// Derive the icon-bar tabs from the live window registry.
///
/// The Ato Home tab is always first. Capsule tabs follow in window-id
/// order (creation order) — deliberately NOT MRU order, so icons don't
/// jump around as the user switches between them.
pub fn derive_shell_tabs(
    windows: &OpenContentWindows,
    app_base_url: &str,
    remote_runs: &[RemoteRun],
    launches: &[TrackedLaunch],
) -> Vec<ShellTab> {
    let entries = windows.mru_order();
    let frontmost_id = entries
        .first()
        .map(|entry| entry.handle.window_id().as_u64());

    let home_entry = entries
        .iter()
        .find(|entry| is_ato_home_entry(&entry.kind, app_base_url));
    let home_window_id = home_entry.map(|entry| entry.handle.window_id().as_u64());

    let mut tabs = vec![ShellTab {
        kind: ShellTabKind::AtoHome,
        window_id: home_window_id,
        open_url: None,
        launch_id: None,
        blocked_reason: None,
        icon_path: None,
        title: SharedString::from("Ato"),
        initial: SharedString::from("A"),
        status: ShellTabStatus::Running,
        is_active: home_window_id.is_some() && home_window_id == frontmost_id,
    }];

    let mut capsule_tabs: Vec<ShellTab> = entries
        .iter()
        .filter(|entry| is_capsule_tab_entry(&entry.kind, app_base_url))
        .map(|entry| {
            let window_id = entry.handle.window_id().as_u64();
            // A window opened FROM a remote run (same origin) inherits the
            // run's Store icon and display name.
            let matching_run = match &entry.kind {
                ContentWindowKind::AppWindow {
                    route: GuestRoute::ExternalUrl(url),
                } => remote_runs.iter().find(|run| {
                    run.open_url()
                        .is_some_and(|open| same_origin(url.as_str(), open))
                }),
                _ => None,
            };
            let title = matching_run
                .map(remote_run_title)
                .or_else(|| {
                    entry
                        .capsule
                        .as_ref()
                        .map(|capsule| capsule.title.clone())
                        .filter(|title| !title.is_empty())
                })
                .unwrap_or_else(|| entry.title.to_string());
            ShellTab {
                kind: ShellTabKind::Capsule,
                window_id: Some(window_id),
                open_url: None,
                launch_id: None,
                blocked_reason: None,
                icon_path: matching_run.and_then(|run| run.icon_path.clone()),
                initial: SharedString::from(avatar_initial(&title)),
                title: SharedString::from(title),
                status: tab_status(entry.capsule.as_ref().map(|capsule| &capsule.status)),
                is_active: Some(window_id) == frontmost_id,
            }
        })
        .collect();
    capsule_tabs.sort_by_key(|tab| tab.window_id);
    tabs.extend(capsule_tabs);

    // Apps already running on the account's other runners. A remote run
    // that is already open as a local window (same origin) stays a
    // window tab; the rest get their own icon — clicking opens the run's
    // public URL as an independent window.
    let mut remote_tabs: Vec<ShellTab> = remote_runs
        .iter()
        .filter_map(|run| {
            let open_url = run.open_url()?;
            let already_open = entries.iter().any(|entry| match &entry.kind {
                ContentWindowKind::AppWindow {
                    route: GuestRoute::ExternalUrl(url),
                } => same_origin(url.as_str(), open_url),
                _ => false,
            });
            if already_open {
                return None;
            }
            let title = remote_run_title(run);
            Some(ShellTab {
                kind: ShellTabKind::Capsule,
                window_id: None,
                open_url: Some(open_url.to_string()),
                launch_id: None,
                blocked_reason: None,
                icon_path: run.icon_path.clone(),
                initial: SharedString::from(avatar_initial(&title)),
                title: SharedString::from(title),
                status: remote_run_status(&run.status),
                is_active: false,
            })
        })
        .collect();
    remote_tabs.sort_by(|a, b| a.title.cmp(&b.title));
    tabs.extend(remote_tabs);

    // Launches the desktop accepted ownership of (IPC / ato://launch).
    // Tab identity = launch_id. A launch whose app is already visible as
    // a local window or a remote-runs tab (same origin) is suppressed —
    // the covering tab is the click target; keeping both would show the
    // same app twice.
    let mut launch_tabs: Vec<ShellTab> = launches
        .iter()
        .filter_map(|launch| {
            let status = launch_tab_status(&launch.state)?;
            if let Some(app_url) = launch.app_url.as_deref().filter(|url| !url.is_empty()) {
                let covered_by_window = entries.iter().any(|entry| match &entry.kind {
                    ContentWindowKind::AppWindow {
                        route: GuestRoute::ExternalUrl(url),
                    } => same_origin(url.as_str(), app_url),
                    _ => false,
                });
                let covered_by_remote = remote_runs.iter().any(|run| {
                    run.open_url()
                        .is_some_and(|open| same_origin(open, app_url))
                });
                if covered_by_window || covered_by_remote {
                    return None;
                }
            }
            let title = launch_tab_title(launch);
            Some(ShellTab {
                kind: ShellTabKind::Capsule,
                window_id: None,
                // Ready → clicking opens the app URL; otherwise there is
                // nothing to open yet and the click is a no-op.
                open_url: (status == ShellTabStatus::Running)
                    .then(|| launch.app_url.clone())
                    .flatten(),
                launch_id: Some(launch.launch_id.clone()),
                icon_path: None,
                blocked_reason: (status == ShellTabStatus::Blocked).then(|| {
                    launch
                        .blocker
                        .clone()
                        .unwrap_or_else(|| "blocked".to_string())
                }),
                initial: SharedString::from(avatar_initial(&title)),
                title: SharedString::from(title),
                status,
                is_active: false,
            })
        })
        .collect();
    launch_tabs.sort_by(|a, b| a.launch_id.cmp(&b.launch_id));
    tabs.extend(launch_tabs);
    tabs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(label: &str, status: &str, app_url: Option<&str>) -> RemoteRun {
        RemoteRun {
            id: format!("run_{label}"),
            label: label.to_string(),
            capsule_scoped_id: None,
            status: status.to_string(),
            runner_display_name: Some("oci-a1".to_string()),
            app_url: app_url.map(str::to_string),
            ready_url: None,
            display_name: None,
            icon_path: None,
        }
    }

    #[test]
    fn remote_run_title_prefers_store_display_name() {
        let mut run = remote("x", "ready", Some("https://x.app.ato.run/"));
        run.display_name = Some("Immich".to_string());
        assert_eq!(remote_run_title(&run), "Immich");
    }

    #[test]
    fn remote_runs_become_capsule_tabs_after_home() {
        let windows = OpenContentWindows::default();
        let runs = vec![
            remote("hello", "running", Some("https://abc.app.ato.run/")),
            remote("booting", "launching", Some("https://def.app.ato.run/")),
            remote("no-url", "running", None),
        ];
        let tabs = derive_shell_tabs(&windows, "https://app.ato.run", &runs, &[]);
        assert_eq!(tabs.len(), 3); // home + 2 remote (no-url dropped)
        assert_eq!(tabs[0].kind, ShellTabKind::AtoHome);
        let hello = tabs.iter().find(|t| t.title.as_ref() == "hello").unwrap();
        assert_eq!(hello.status, ShellTabStatus::Running);
        assert_eq!(hello.initial.as_ref(), "H");
        assert_eq!(hello.open_url.as_deref(), Some("https://abc.app.ato.run/"));
        assert!(hello.window_id.is_none());
        let booting = tabs.iter().find(|t| t.title.as_ref() == "booting").unwrap();
        assert_eq!(booting.status, ShellTabStatus::Starting);
    }

    fn external(url: &str) -> ContentWindowKind {
        ContentWindowKind::AppWindow {
            route: GuestRoute::ExternalUrl(url::Url::parse(url).unwrap()),
        }
    }

    fn capsule(handle: &str) -> ContentWindowKind {
        ContentWindowKind::AppWindow {
            route: GuestRoute::CapsuleHandle {
                handle: handle.to_string(),
                label: handle.to_string(),
                community_toml_id: None,
            },
        }
    }

    #[test]
    fn home_entry_matches_pwa_origin_ignoring_path() {
        assert!(is_ato_home_entry(
            &external("https://app.ato.run/run/foo"),
            "https://app.ato.run"
        ));
        assert!(!is_ato_home_entry(
            &external("https://ato.run/"),
            "https://app.ato.run"
        ));
    }

    #[test]
    fn dedicated_home_window_counts_as_home() {
        assert!(is_ato_home_entry(
            &ContentWindowKind::Home,
            "https://app.ato.run"
        ));
    }

    #[test]
    fn capsule_windows_are_tabs_but_web_and_system_windows_are_not() {
        let base = "https://app.ato.run";
        assert!(is_capsule_tab_entry(&capsule("hello-capsule"), base));
        // A non-home ExternalUrl window (e.g. a cloud session opened via
        // ato://open) IS a tab; the PWA Home origin itself is not.
        assert!(is_capsule_tab_entry(
            &external("https://abc123.app.ato.run/"),
            base
        ));
        assert!(!is_capsule_tab_entry(&external("https://app.ato.run/run"), base));
        assert!(!is_capsule_tab_entry(&ContentWindowKind::Store, base));
        assert!(!is_capsule_tab_entry(&ContentWindowKind::Settings, base));
    }

    #[test]
    fn status_maps_missing_context_to_starting() {
        assert_eq!(tab_status(None), ShellTabStatus::Starting);
        assert_eq!(
            tab_status(Some(&CapsuleWindowStatus::Starting)),
            ShellTabStatus::Starting
        );
        assert_eq!(
            tab_status(Some(&CapsuleWindowStatus::Ready)),
            ShellTabStatus::Running
        );
        assert_eq!(
            tab_status(Some(&CapsuleWindowStatus::Failed)),
            ShellTabStatus::Error
        );
    }

    #[test]
    fn remote_run_title_prefers_capsule_slug_over_run_id_label() {
        let mut run = remote("01KW12MFHY", "ready", Some("https://x.app.ato.run/"));
        run.id = "01KW12MFHY".to_string();
        run.capsule_scoped_id = Some("community/immich".to_string());
        assert_eq!(remote_run_title(&run), "immich");
        run.label = "My Immich".to_string();
        assert_eq!(remote_run_title(&run), "My Immich");
    }

    #[test]
    fn remote_run_status_treats_ready_as_running() {
        assert_eq!(remote_run_status("ready"), ShellTabStatus::Running);
        assert_eq!(remote_run_status("running"), ShellTabStatus::Running);
        assert_eq!(remote_run_status("launching"), ShellTabStatus::Starting);
    }

    fn launch(launch_id: &str, capsule_ref: &str, state: &str, app_url: Option<&str>) -> TrackedLaunch {
        TrackedLaunch {
            launch_id: launch_id.to_string(),
            capsule_ref: capsule_ref.to_string(),
            state: state.to_string(),
            app_url: app_url.map(str::to_string),
            blocker: None,
            opened: false,
            polling: false,
        }
    }

    #[test]
    fn blocked_launches_get_a_first_class_blocked_tab() {
        assert_eq!(
            launch_tab_status("blocked"),
            Some(ShellTabStatus::Blocked)
        );
        // stopping still renders as starting (hot polling continues)
        assert_eq!(
            launch_tab_status("stopping"),
            Some(ShellTabStatus::Starting)
        );
    }

    #[test]
    fn launch_tab_title_uses_slug_and_falls_back_to_launch_id() {
        let l = launch("lch_1", "community/hello-capsule", "starting", None);
        assert_eq!(launch_tab_title(&l), "hello-capsule");
        let bare = launch("lch_2", "", "starting", None);
        assert_eq!(launch_tab_title(&bare), "lch_2");
    }

    #[test]
    fn launch_tab_status_maps_states_and_drops_finished_launches() {
        assert_eq!(launch_tab_status("starting"), Some(ShellTabStatus::Starting));
        assert_eq!(launch_tab_status("ready"), Some(ShellTabStatus::Running));
        assert_eq!(launch_tab_status("failed"), Some(ShellTabStatus::Error));
        // Unknown future states render honestly as non-ready; blocked is
        // a first-class visible state of its own.
        assert_eq!(launch_tab_status("stopping"), Some(ShellTabStatus::Starting));
        assert_eq!(launch_tab_status("blocked"), Some(ShellTabStatus::Blocked));
        assert_eq!(launch_tab_status("cancelled"), None);
        assert_eq!(launch_tab_status("expired"), None);
        assert_eq!(launch_tab_status("stopped"), None);
    }

    #[test]
    fn launches_become_tabs_with_launch_id_identity() {
        let windows = OpenContentWindows::default();
        let launches = vec![
            launch("lch_a", "community/hello-capsule", "starting", None),
            launch("lch_b", "community/immich", "ready", Some("https://abc.app.ato.run/")),
            launch("lch_c", "community/gone", "cancelled", None),
        ];
        let tabs = derive_shell_tabs(&windows, "https://app.ato.run", &[], &launches);
        assert_eq!(tabs.len(), 3); // home + 2 launches (cancelled dropped)

        let starting = tabs
            .iter()
            .find(|t| t.launch_id.as_deref() == Some("lch_a"))
            .unwrap();
        assert_eq!(starting.status, ShellTabStatus::Starting);
        assert_eq!(starting.title.as_ref(), "hello-capsule");
        // Starting launch: no window, no URL — clicking is a no-op.
        assert!(starting.window_id.is_none());
        assert!(starting.open_url.is_none());

        let ready = tabs
            .iter()
            .find(|t| t.launch_id.as_deref() == Some("lch_b"))
            .unwrap();
        assert_eq!(ready.status, ShellTabStatus::Running);
        assert_eq!(ready.open_url.as_deref(), Some("https://abc.app.ato.run/"));
    }

    #[test]
    fn ready_launch_tab_suppressed_when_remote_run_covers_same_origin() {
        let windows = OpenContentWindows::default();
        let runs = vec![remote("hello", "running", Some("https://abc.app.ato.run/x"))];
        let launches = vec![launch(
            "lch_b",
            "community/hello-capsule",
            "ready",
            Some("https://abc.app.ato.run/"),
        )];
        let tabs = derive_shell_tabs(&windows, "https://app.ato.run", &runs, &launches);
        // home + the remote-run tab; the launch tab is suppressed.
        assert_eq!(tabs.len(), 2);
        assert!(tabs.iter().all(|t| t.launch_id.is_none()));
    }

    #[test]
    fn failed_launch_keeps_error_badge_even_without_app_url() {
        let windows = OpenContentWindows::default();
        let launches = vec![launch("lch_x", "community/broken", "failed", None)];
        let tabs = derive_shell_tabs(&windows, "https://app.ato.run", &[], &launches);
        let failed = tabs
            .iter()
            .find(|t| t.launch_id.as_deref() == Some("lch_x"))
            .unwrap();
        assert_eq!(failed.status, ShellTabStatus::Error);
        assert!(failed.open_url.is_none());
    }

    #[test]
    fn avatar_initial_prefers_first_alphanumeric() {
        assert_eq!(avatar_initial("hello-capsule"), "H");
        assert_eq!(avatar_initial("  1password"), "1");
        assert_eq!(avatar_initial("——"), "·");
    }

    #[test]
    fn avatar_hue_is_stable_and_in_range() {
        let hue = avatar_hue("hello-capsule");
        assert_eq!(hue, avatar_hue("hello-capsule"));
        assert!((0.0..1.0).contains(&hue));
    }
}
