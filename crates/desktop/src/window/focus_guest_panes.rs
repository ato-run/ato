//! Focus-mode guest capsule pane registry (#370).
//!
//! Focus View opens one `AppCapsuleShell` per launched capsule, and each
//! shell owns a *private* `wry::WebView` (`AppCapsuleShell::webview`). The
//! Focus automation dispatcher (`focus_dispatcher.rs`) historically only
//! knew about the dock WebView (`DOCK_AUTOMATION_PANE_ID`), so `browser_tabs`
//! returned `[]` for guest capsules and `browser_snapshot` /
//! `browser_take_screenshot` failed with "no WebView pane".
//!
//! This registry is the missing bridge: when the orchestrator opens an
//! `AppCapsuleShell` window it registers the shell here keyed by the GPUI
//! window id, exposing a stable automation pane id the dispatcher can route
//! MCP browser commands to. Closing the window unregisters it (see
//! `app.rs::on_window_closed`).
//!
//! Deliberately *not* folded into `WebViewManager`: Focus mode keeps the
//! legacy `DesktopShell` / `WebViewManager` out of the boot path, which is
//! exactly why `focus_dispatcher.rs` exists as an independent surface. The
//! minimal fix is this registry plus a narrow dispatch method on
//! `AppCapsuleShell`, not a merge of the two WebView systems.

use std::collections::HashMap;

use gpui::WeakEntity;

use crate::state::GuestRoute;
use crate::window::app_capsule_shell::AppCapsuleShell;

/// Base offset for Focus guest pane ids. Chosen well above
/// [`DOCK_AUTOMATION_PANE_ID`] (`999_000`) so the two pane-id spaces never
/// overlap and a caller's explicit `pane_id` unambiguously selects dock vs
/// guest. The GPUI window id is added to this base, so distinct windows get
/// distinct, stable pane ids for their lifetime.
pub const FOCUS_GUEST_PANE_ID_BASE: usize = 2_000_000;

/// Stable automation pane id for a guest capsule window.
pub fn focus_guest_pane_id(window_id: u64) -> usize {
    FOCUS_GUEST_PANE_ID_BASE + window_id as usize
}

/// True if `pane_id` belongs to the Focus guest pane id space.
pub fn is_focus_guest_pane_id(pane_id: usize) -> bool {
    pane_id >= FOCUS_GUEST_PANE_ID_BASE
}

/// One registered guest capsule automation pane.
#[derive(Clone)]
pub struct FocusGuestPaneEntry {
    pub pane_id: usize,
    pub window_id: u64,
    pub route: GuestRoute,
    /// Best-effort capsule handle string (e.g. `capsule://github.com/usememos/memos`)
    /// surfaced in `browser_tabs` output.
    pub handle: String,
    /// Weak handle to the owning shell. Upgraded at dispatch time; a dead
    /// weak means the window closed and the pane should be ignored.
    pub shell: WeakEntity<AppCapsuleShell>,
}

/// Window-id ↔ pane-id bookkeeping, kept separate from the entry payload so
/// the mapping invariant is unit-testable without a live GPUI entity.
#[derive(Default)]
struct PaneIndex {
    by_window_id: HashMap<u64, usize>,
}

impl PaneIndex {
    fn register(&mut self, window_id: u64) -> usize {
        let pane_id = focus_guest_pane_id(window_id);
        self.by_window_id.insert(window_id, pane_id);
        pane_id
    }

    fn unregister(&mut self, window_id: u64) -> Option<usize> {
        self.by_window_id.remove(&window_id)
    }

    fn pane_id_for_window(&self, window_id: u64) -> Option<usize> {
        self.by_window_id.get(&window_id).copied()
    }
}

/// GPUI global: the set of live Focus guest capsule panes.
#[derive(Default)]
pub struct FocusGuestPaneRegistry {
    index: PaneIndex,
    panes: HashMap<usize, FocusGuestPaneEntry>,
}

impl gpui::Global for FocusGuestPaneRegistry {}

impl FocusGuestPaneRegistry {
    /// Register (or replace) the guest pane for `window_id`. Returns the
    /// stable pane id.
    pub fn register(
        &mut self,
        window_id: u64,
        route: GuestRoute,
        handle: String,
        shell: WeakEntity<AppCapsuleShell>,
    ) -> usize {
        let pane_id = self.index.register(window_id);
        self.panes.insert(
            pane_id,
            FocusGuestPaneEntry {
                pane_id,
                window_id,
                route,
                handle,
                shell,
            },
        );
        pane_id
    }

    /// Drop the guest pane for a closed GPUI window. Returns the removed
    /// entry if one was tracked.
    pub fn unregister_window(&mut self, window_id: u64) -> Option<FocusGuestPaneEntry> {
        let pane_id = self.index.unregister(window_id)?;
        self.panes.remove(&pane_id)
    }

    pub fn get(&self, pane_id: usize) -> Option<&FocusGuestPaneEntry> {
        self.panes.get(&pane_id)
    }

    /// All registered guest panes (order unspecified).
    pub fn list(&self) -> Vec<FocusGuestPaneEntry> {
        self.panes.values().cloned().collect()
    }

    pub fn pane_id_for_window(&self, window_id: u64) -> Option<usize> {
        self.index.pane_id_for_window(window_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webview::DOCK_AUTOMATION_PANE_ID;

    #[test]
    fn focus_guest_pane_id_does_not_collide_with_dock_id() {
        // The dock pane id sits at 999_000; every guest pane id must be
        // strictly greater so the dispatcher can tell them apart.
        assert!(FOCUS_GUEST_PANE_ID_BASE > DOCK_AUTOMATION_PANE_ID);
        for window_id in [0u64, 1, 42, 123, u32::MAX as u64] {
            let pane = focus_guest_pane_id(window_id);
            assert_ne!(pane, DOCK_AUTOMATION_PANE_ID);
            assert!(is_focus_guest_pane_id(pane));
        }
        // The dock id must never be misclassified as a guest pane.
        assert!(!is_focus_guest_pane_id(DOCK_AUTOMATION_PANE_ID));
    }

    #[test]
    fn focus_guest_registry_registers_and_unregisters_by_window_id() {
        // Exercises the window-id ↔ pane-id bookkeeping that backs
        // `FocusGuestPaneRegistry::register` / `unregister_window` without
        // needing a live GPUI entity for the shell payload.
        let mut index = PaneIndex::default();

        assert_eq!(index.pane_id_for_window(7), None);

        let pane = index.register(7);
        assert_eq!(pane, focus_guest_pane_id(7));
        assert_eq!(index.pane_id_for_window(7), Some(pane));

        // Distinct windows map to distinct panes.
        let other = index.register(8);
        assert_ne!(other, pane);
        assert_eq!(index.pane_id_for_window(8), Some(other));

        // Unregister returns the pane id once, then the window is gone.
        assert_eq!(index.unregister(7), Some(pane));
        assert_eq!(index.pane_id_for_window(7), None);
        assert_eq!(index.unregister(7), None);

        // The unrelated window is untouched.
        assert_eq!(index.pane_id_for_window(8), Some(other));
    }
}
