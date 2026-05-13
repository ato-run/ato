//! Cross-window content registry — single source of truth for every
//! user-facing top-level window the user can navigate to. Drives both
//! the Control Bar's Card Switcher badge (count) and the Card Switcher
//! overlay's cards (one card per entry, MRU-ordered, click to focus).
//! Excludes chrome (Control Bar, Card Switcher overlay) by virtue of
//! those windows never registering.
//!
//! Replaces the simpler `state::OpenContentWindows` HashSet — the
//! richer data here lets the Card Switcher render real cards for
//! Store / StartWindow / Launcher, not just AppWindows. Lives in
//! `window/` rather than `state/` because `AnyWindowHandle` is a
//! gpui type and `state/` is intentionally gpui-free.

use std::collections::HashMap;
use std::time::Instant;

use gpui::{AnyWindowHandle, SharedString};

use crate::state::GuestRoute;

/// What KIND of content window an entry represents. Lets the card
/// renderer pick the right glyph / accent recipe without having to
/// re-classify by inspecting title strings. The `route` carried by
/// AppWindow is the same one used by `AppWindowRegistry` so the
/// Card Switcher's MRU previously sourced from there still has access
/// to the route via this entry.
#[derive(Clone, Debug)]
pub enum ContentWindowKind {
    AppWindow { route: GuestRoute },
    Store,
    Start,
    Launcher,
}

#[derive(Clone, Debug)]
pub struct ContentWindowEntry {
    pub handle: AnyWindowHandle,
    pub kind: ContentWindowKind,
    pub title: SharedString,
    pub subtitle: SharedString,
    pub last_focused_at: Instant,
}

#[derive(Default, Clone, Debug)]
pub struct OpenContentWindows {
    windows: HashMap<u64, ContentWindowEntry>,
}

impl OpenContentWindows {
    pub fn insert(&mut self, gpui_window_id: u64, entry: ContentWindowEntry) {
        self.windows.insert(gpui_window_id, entry);
    }
    pub fn remove(&mut self, gpui_window_id: u64) -> bool {
        self.windows.remove(&gpui_window_id).is_some()
    }
    pub fn len(&self) -> usize {
        self.windows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }
    pub fn get(&self, gpui_window_id: u64) -> Option<&ContentWindowEntry> {
        self.windows.get(&gpui_window_id)
    }
    /// Bump the MRU timestamp for the given window. Returns true if
    /// the window was tracked. No-op for unknown IDs (e.g. chrome
    /// windows that never registered).
    pub fn focus(&mut self, gpui_window_id: u64) -> bool {
        if let Some(entry) = self.windows.get_mut(&gpui_window_id) {
            entry.last_focused_at = Instant::now();
            true
        } else {
            false
        }
    }
    /// Entries cloned out in most-recently-focused-first order. Used
    /// by the Card Switcher to render its row at overlay spawn time.
    pub fn mru_order(&self) -> Vec<ContentWindowEntry> {
        let mut entries: Vec<ContentWindowEntry> = self.windows.values().cloned().collect();
        entries.sort_by(|a, b| b.last_focused_at.cmp(&a.last_focused_at));
        entries
    }
}
