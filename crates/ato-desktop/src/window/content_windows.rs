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

/// Lifecycle phase reported for a capsule-backed content window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapsuleWindowStatus {
    Starting,
    Ready,
    Failed,
}

impl CapsuleWindowStatus {
    pub fn label(&self) -> &'static str {
        match self {
            CapsuleWindowStatus::Starting => "Starting",
            CapsuleWindowStatus::Ready => "Ready",
            CapsuleWindowStatus::Failed => "Failed",
        }
    }
}

/// Per-AppWindow capsule state surfaced to the Card Switcher / capsule
/// panel. `AppCapsuleShell` writes a fresh snapshot here on every boot
/// state transition via `OpenContentWindows::set_capsule_context`.
#[derive(Clone, Debug)]
pub struct CapsuleWindowContext {
    pub title: String,
    pub handle: String,
    pub canonical_handle: Option<String>,
    pub session_id: Option<String>,
    pub current_url: String,
    pub local_url: Option<String>,
    pub snapshot_label: Option<String>,
    pub trust_state: String,
    pub runtime_label: Option<String>,
    pub display_strategy: Option<String>,
    pub capabilities: Vec<String>,
    pub log_path: Option<String>,
    pub status: CapsuleWindowStatus,
    pub restricted: bool,
    pub error_message: Option<String>,
}

impl CapsuleWindowContext {
    /// Canonical handle if known, else the user-typed handle.
    pub fn active_handle(&self) -> &str {
        self.canonical_handle.as_deref().unwrap_or(&self.handle)
    }

    /// Short version-like string for the capsule panel header. Falls
    /// back to a dash when the snapshot label is unavailable.
    pub fn version_label(&self) -> &str {
        self.snapshot_label.as_deref().unwrap_or("—")
    }
}

/// What KIND of content window an entry represents. Lets the card
/// renderer pick the right glyph / accent recipe without having to
/// re-classify by inspecting title strings. The `route` carried by
/// AppWindow is the same one used by `AppWindowRegistry` so the
/// Card Switcher's MRU previously sourced from there still has access
/// to the route via this entry.
#[derive(Clone, Debug)]
pub enum ContentWindowKind {
    AppWindow {
        route: GuestRoute,
    },
    Store,
    Start,
    /// `ato-settings` system capsule. Replaces the Phase-1 `Launcher`
    /// variant — the legacy Launcher window was retired in Stage D
    /// of the system-capsule refactor.
    Settings,
    Dock,
}

#[derive(Clone, Debug)]
pub struct ContentWindowEntry {
    pub handle: AnyWindowHandle,
    pub kind: ContentWindowKind,
    pub title: SharedString,
    pub subtitle: SharedString,
    /// Canonical URL string for the Control Bar URL field. For
    /// AppWindow(ExternalUrl) this is `capsule://desktop.ato.run/web-viewer`;
    /// for AppWindow(CapsuleHandle) it is `capsule://<handle>`; for
    /// system capsules (Store, Start, Settings, Identity) it is
    /// `capsule://desktop.ato.run/<slug>`.
    /// Read by the bar when this entry is at the top of MRU.
    pub url: SharedString,
    /// Capsule lifecycle context for AppWindow entries backed by an
    /// `AppCapsuleShell`. `None` for unmanaged content windows (Store,
    /// Start, Settings, Dock, ExternalUrl placeholders).
    pub capsule: Option<CapsuleWindowContext>,
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
    /// The most-recently-focused entry, if any. Used by the capsule
    /// panel to snapshot the active capsule on demand.
    pub fn frontmost(&self) -> Option<ContentWindowEntry> {
        self.windows
            .values()
            .max_by_key(|entry| entry.last_focused_at)
            .cloned()
    }
    /// Replace the capsule lifecycle context for a tracked window.
    /// No-op if the window isn't registered (e.g. close raced).
    pub fn set_capsule_context(
        &mut self,
        gpui_window_id: u64,
        context: Option<CapsuleWindowContext>,
    ) {
        if let Some(entry) = self.windows.get_mut(&gpui_window_id) {
            entry.capsule = context;
        }
    }
}
