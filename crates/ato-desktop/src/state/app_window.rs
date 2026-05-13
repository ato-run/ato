//! Multi-window state model — layer 1 of the Focus View redesign (issue #167).
//!
//! Today's renderer keys off `Workspace::panes` inside a single shell. The
//! redesign turns each running guest app into its own top-level NSWindow
//! with a paired floating Control Bar window; the registry here is the
//! data model that subsequent layers (#169 window orchestration, #171
//! control bar window, #173 card switcher) will key off.
//!
//! This file is intentionally additive: nothing in the rest of the crate
//! reads from `AppWindowRegistry` yet. The cut-over from the legacy
//! pane model to this registry happens in #169. The `dead_code` allow
//! below silences the resulting "never used" warnings; tests in this
//! module exercise the full API.

#![allow(dead_code)]

use std::collections::HashMap;
use std::time::Instant;

use crate::state::{GuestRoute, PaneBounds};

pub type AppWindowId = usize;

/// State for a single guest app's top-level window.
#[derive(Clone, Debug)]
pub struct AppWindow {
    pub id: AppWindowId,
    pub route: GuestRoute,
    /// Last time `AppWindowRegistry::focus` was called for this window.
    /// Drives MRU order for the future Card Switcher (#173).
    pub last_focused_at: Instant,
    /// Soft retention deadline: when set, the underlying capsule session
    /// is kept warm even after the window closes, until this time.
    /// Populated in #169 when the orchestration layer wires up the
    /// `RetentionTable` flow.
    pub retention_until: Option<Instant>,
    /// Last known content-bounds for the window, mirrored so the future
    /// `WebViewManager` per-window singleton (#169) can resize without
    /// a separate state field.
    pub bounds: Option<PaneBounds>,
    /// Opaque GPUI WindowId stored as `u64` (via `WindowId::as_u64`).
    /// Lets `cx.on_window_closed` map the closed window back to its
    /// registry entry so we can remove it on close.
    pub gpui_window_id: Option<u64>,
}

impl AppWindow {
    pub fn new(id: AppWindowId, route: GuestRoute) -> Self {
        Self {
            id,
            route,
            last_focused_at: Instant::now(),
            retention_until: None,
            bounds: None,
            gpui_window_id: None,
        }
    }
}

/// Top-level shell surface — replaces `ShellMode` once layer 2 (#169)
/// flips the renderer over. Both types coexist during the transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellSurface {
    /// Focused on a specific app window.
    Focus { window_id: AppWindowId },
    /// Focused on the Launcher / Shell View.
    Launcher,
}

/// Registry of currently-open app windows plus MRU bookkeeping.
///
/// IDs are monotonically increasing and never reused, so a stale
/// `AppWindowId` always resolves to `None` rather than aliasing a
/// later window.
#[derive(Clone, Debug, Default)]
pub struct AppWindowRegistry {
    windows: HashMap<AppWindowId, AppWindow>,
    next_id: AppWindowId,
}

impl AppWindowRegistry {
    /// Allocate a new `AppWindowId` and insert an `AppWindow` for the
    /// given route. The new window is marked as freshly focused so it
    /// leads MRU order until something else gets focus.
    pub fn open(&mut self, route: GuestRoute) -> AppWindowId {
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(1).expect("AppWindowId overflow");
        self.windows.insert(id, AppWindow::new(id, route));
        id
    }

    /// Remove the window with the given id, returning the removed
    /// record. Returns `None` if the id was unknown.
    pub fn close(&mut self, id: AppWindowId) -> Option<AppWindow> {
        self.windows.remove(&id)
    }

    pub fn get(&self, id: AppWindowId) -> Option<&AppWindow> {
        self.windows.get(&id)
    }

    pub fn get_mut(&mut self, id: AppWindowId) -> Option<&mut AppWindow> {
        self.windows.get_mut(&id)
    }

    /// Mark the window as the most-recently focused. Returns true if
    /// the id existed.
    pub fn focus(&mut self, id: AppWindowId) -> bool {
        if let Some(w) = self.windows.get_mut(&id) {
            w.last_focused_at = Instant::now();
            true
        } else {
            false
        }
    }

    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    pub fn len(&self) -> usize {
        self.windows.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &AppWindow> {
        self.windows.values()
    }

    /// Find the registry entry whose `gpui_window_id` matches the
    /// given raw id, if any. Used by `cx.on_window_closed` to map a
    /// GPUI close event back to the registry slot to evict.
    pub fn find_by_gpui_window_id(&self, gpui_window_id: u64) -> Option<AppWindowId> {
        self.windows
            .iter()
            .find(|(_, w)| w.gpui_window_id == Some(gpui_window_id))
            .map(|(id, _)| *id)
    }

    /// Open windows ordered most-recently-focused first.
    pub fn mru_order(&self) -> Vec<AppWindowId> {
        let mut entries: Vec<(AppWindowId, Instant)> = self
            .windows
            .iter()
            .map(|(id, w)| (*id, w.last_focused_at))
            .collect();
        // Reverse-sort by last_focused_at: most recent first.
        entries.sort_by(|a, b| b.1.cmp(&a.1));
        entries.into_iter().map(|(id, _)| id).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::thread::sleep;
    use std::time::Duration;

    use super::*;

    fn dummy_route(label: &str) -> GuestRoute {
        GuestRoute::Capsule {
            session: label.to_string(),
            entry_path: "index.html".to_string(),
        }
    }

    #[test]
    fn empty_registry_reports_empty() {
        let reg = AppWindowRegistry::default();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(reg.mru_order().is_empty());
        assert!(reg.get(0).is_none());
    }

    #[test]
    fn open_assigns_sequential_ids() {
        let mut reg = AppWindowRegistry::default();
        let a = reg.open(dummy_route("a"));
        let b = reg.open(dummy_route("b"));
        let c = reg.open(dummy_route("c"));
        assert_eq!(a, 0);
        assert_eq!(b, 1);
        assert_eq!(c, 2);
        assert_eq!(reg.len(), 3);
    }

    #[test]
    fn open_records_route() {
        let mut reg = AppWindowRegistry::default();
        let id = reg.open(dummy_route("foo"));
        let w = reg.get(id).expect("window should exist");
        match &w.route {
            GuestRoute::Capsule { session, .. } => assert_eq!(session, "foo"),
            other => panic!("unexpected route: {other:?}"),
        }
    }

    #[test]
    fn close_returns_record_and_removes() {
        let mut reg = AppWindowRegistry::default();
        let id = reg.open(dummy_route("a"));
        assert!(reg.get(id).is_some());
        let removed = reg.close(id).expect("close returns record");
        assert_eq!(removed.id, id);
        assert!(reg.get(id).is_none());
        assert!(reg.is_empty());
    }

    #[test]
    fn close_unknown_id_returns_none() {
        let mut reg = AppWindowRegistry::default();
        assert!(reg.close(42).is_none());
    }

    #[test]
    fn closed_ids_are_not_reused() {
        let mut reg = AppWindowRegistry::default();
        let a = reg.open(dummy_route("a"));
        reg.close(a);
        let b = reg.open(dummy_route("b"));
        assert_ne!(a, b, "ids must be monotonic, even after close");
    }

    #[test]
    fn focus_bumps_mru_order() {
        let mut reg = AppWindowRegistry::default();
        let a = reg.open(dummy_route("a"));
        sleep(Duration::from_millis(2));
        let b = reg.open(dummy_route("b"));
        sleep(Duration::from_millis(2));
        let c = reg.open(dummy_route("c"));

        // Initially, MRU order is reverse-insertion: c, b, a.
        assert_eq!(reg.mru_order(), vec![c, b, a]);

        sleep(Duration::from_millis(2));
        assert!(reg.focus(a));
        // After focusing a, MRU order becomes: a, c, b.
        assert_eq!(reg.mru_order(), vec![a, c, b]);

        sleep(Duration::from_millis(2));
        assert!(reg.focus(b));
        assert_eq!(reg.mru_order(), vec![b, a, c]);
    }

    #[test]
    fn focus_unknown_id_returns_false() {
        let mut reg = AppWindowRegistry::default();
        assert!(!reg.focus(99));
    }

    #[test]
    fn iter_returns_all_windows() {
        let mut reg = AppWindowRegistry::default();
        reg.open(dummy_route("a"));
        reg.open(dummy_route("b"));
        let count = reg.iter().count();
        assert_eq!(count, 2);
    }
}
