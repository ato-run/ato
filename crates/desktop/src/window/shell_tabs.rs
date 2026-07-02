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

use gpui::SharedString;

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
    Error,
}

#[derive(Clone)]
pub struct ShellTab {
    pub kind: ShellTabKind,
    /// GPUI window id backing this tab. `None` for the Ato Home tab when
    /// no Home window is currently open (clicking it opens one).
    pub window_id: Option<u64>,
    pub title: SharedString,
    /// Fallback avatar glyph — first letter of the capsule title.
    pub initial: SharedString,
    pub status: ShellTabStatus,
    pub is_active: bool,
}

/// True when `kind` is the Ato Home window — an ExternalUrl AppWindow whose
/// initial route points at the configured PWA origin, or the native
/// `ato-start` fallback landing.
pub fn is_ato_home_entry(kind: &ContentWindowKind, app_base_url: &str) -> bool {
    match kind {
        ContentWindowKind::AppWindow {
            route: GuestRoute::ExternalUrl(url),
        } => same_origin(url.as_str(), app_base_url),
        ContentWindowKind::Start => true,
        _ => false,
    }
}

/// True when `kind` should appear as a capsule tab in the icon bar:
/// capsule-backed AppWindows only. System surfaces (Store, Settings, Dock,
/// Launch, Import, Auth) and plain web-viewer windows are not tabs.
pub fn is_capsule_tab_entry(kind: &ContentWindowKind) -> bool {
    matches!(
        kind,
        ContentWindowKind::AppWindow {
            route: GuestRoute::CapsuleHandle { .. }
                | GuestRoute::CapsuleUrl { .. }
                | GuestRoute::LocalManifest(_)
                | GuestRoute::Capsule { .. }
                | GuestRoute::Terminal { .. }
        }
    )
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
pub fn derive_shell_tabs(windows: &OpenContentWindows, app_base_url: &str) -> Vec<ShellTab> {
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
        title: SharedString::from("Ato"),
        initial: SharedString::from("A"),
        status: ShellTabStatus::Running,
        is_active: home_window_id.is_some() && home_window_id == frontmost_id,
    }];

    let mut capsule_tabs: Vec<ShellTab> = entries
        .iter()
        .filter(|entry| is_capsule_tab_entry(&entry.kind))
        .map(|entry| {
            let window_id = entry.handle.window_id().as_u64();
            let title = entry
                .capsule
                .as_ref()
                .map(|capsule| capsule.title.clone())
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| entry.title.to_string());
            ShellTab {
                kind: ShellTabKind::Capsule,
                window_id: Some(window_id),
                initial: SharedString::from(avatar_initial(&title)),
                title: SharedString::from(title),
                status: tab_status(entry.capsule.as_ref().map(|capsule| &capsule.status)),
                is_active: Some(window_id) == frontmost_id,
            }
        })
        .collect();
    capsule_tabs.sort_by_key(|tab| tab.window_id);
    tabs.extend(capsule_tabs);
    tabs
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn native_start_landing_counts_as_home() {
        assert!(is_ato_home_entry(
            &ContentWindowKind::Start,
            "https://app.ato.run"
        ));
    }

    #[test]
    fn capsule_windows_are_tabs_but_web_and_system_windows_are_not() {
        assert!(is_capsule_tab_entry(&capsule("hello-capsule")));
        assert!(!is_capsule_tab_entry(&external("https://example.com/")));
        assert!(!is_capsule_tab_entry(&ContentWindowKind::Store));
        assert!(!is_capsule_tab_entry(&ContentWindowKind::Settings));
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
