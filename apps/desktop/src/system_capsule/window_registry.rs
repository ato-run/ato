//! Runtime window binding registry for active system capsule WebViews.
//!
//! `SystemCapsuleWindowRegistry` maps each `SystemCapsuleId` to the set of
//! GPUI `WindowId`s that currently host it.  A capsule may have **more than
//! one** active window (e.g. `AtoLaunch` opens separate consent / boot /
//! github-run windows), so the registry uses a `HashSet<WindowId>` per capsule
//! rather than a single slot.
//!
//! # Usage
//!
//! - **At window creation**: call `register(id, handle)` to add the window to
//!   the capsule's binding set.
//! - **At window close**: call `unregister_window(window_id)` to remove the
//!   window from *all* capsule sets (no need to know the capsule id in
//!   advance).
//! - **In the drain loop** (`spawn_drain_loop_inner`): call
//!   `has_binding_for_window(id, window_id)` to confirm that *this specific*
//!   host window is still registered before dispatching an IPC command.  This
//!   prevents stale drain loops from processing IPC after their window has
//!   closed, without affecting sibling windows of the same capsule.
//!
//! The `gpui::Global` impl lives in `window/mod.rs` (see the comment there).

use std::collections::{HashMap, HashSet};

use gpui::{AnyWindowHandle, WindowId};

use super::broker::SystemCapsuleId;

/// Registry mapping active system capsule IDs to their host window IDs.
///
/// Multiple windows per capsule are supported (see [`register`]).
#[derive(Debug, Default)]
pub struct SystemCapsuleWindowRegistry {
    windows: HashMap<SystemCapsuleId, HashSet<WindowId>>,
}

impl SystemCapsuleWindowRegistry {
    /// Add `handle` to the binding set for `id`.
    ///
    /// Idempotent: calling `register` twice with the same window is a no-op.
    pub fn register(&mut self, id: SystemCapsuleId, handle: AnyWindowHandle) {
        self.windows
            .entry(id)
            .or_default()
            .insert(handle.window_id());
    }

    /// Remove `window_id` from every capsule's binding set.
    ///
    /// Called from `on_window_closed` where the capsule id is not known in
    /// advance.  Empty sets are pruned so that `has_binding` stays accurate.
    pub fn unregister_window(&mut self, window_id: WindowId) {
        self.windows.retain(|_, ids| {
            ids.remove(&window_id);
            !ids.is_empty()
        });
    }

    /// Returns `true` if there is **at least one** registered window for `id`.
    pub fn has_binding(&self, id: SystemCapsuleId) -> bool {
        self.windows.get(&id).is_some_and(|s| !s.is_empty())
    }

    /// Returns `true` if `window_id` is in the binding set for `id`.
    ///
    /// Used by each drain loop to confirm that *its own* host window is still
    /// active — without affecting other windows of the same capsule.
    pub fn has_binding_for_window(&self, id: SystemCapsuleId, window_id: WindowId) -> bool {
        self.windows
            .get(&id)
            .is_some_and(|s| s.contains(&window_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_window_id(n: u64) -> WindowId {
        // WindowId implements From<u64> in GPUI (via slotmap::KeyData::from_ffi),
        // which is the public-API way to construct a WindowId without a running runtime.
        WindowId::from(n)
    }

    #[test]
    fn has_binding_false_when_empty() {
        let reg = SystemCapsuleWindowRegistry::default();
        assert!(!reg.has_binding(SystemCapsuleId::AtoStore));
    }

    #[test]
    fn register_then_has_binding() {
        let wid = make_window_id(1);
        let mut reg = SystemCapsuleWindowRegistry::default();
        // Simulate registration via raw window_id.
        reg.windows
            .entry(SystemCapsuleId::AtoStore)
            .or_default()
            .insert(wid);
        assert!(reg.has_binding(SystemCapsuleId::AtoStore));
        assert!(reg.has_binding_for_window(SystemCapsuleId::AtoStore, wid));
    }

    #[test]
    fn unregister_window_noop_on_unknown() {
        let mut reg = SystemCapsuleWindowRegistry::default();
        reg.unregister_window(make_window_id(99)); // must not panic
        assert!(!reg.has_binding(SystemCapsuleId::AtoLaunch));
    }

    #[test]
    fn multiwindow_launch_survives_partial_close() {
        // AtoLaunch can have multiple concurrent windows (consent, boot, …).
        // Closing one must NOT remove the binding for the remaining windows.
        let w1 = make_window_id(1);
        let w2 = make_window_id(2);
        let mut reg = SystemCapsuleWindowRegistry::default();
        reg.windows
            .entry(SystemCapsuleId::AtoLaunch)
            .or_default()
            .insert(w1);
        reg.windows
            .entry(SystemCapsuleId::AtoLaunch)
            .or_default()
            .insert(w2);

        // Close w2 — w1 must still have binding.
        reg.unregister_window(w2);
        assert!(reg.has_binding(SystemCapsuleId::AtoLaunch));
        assert!(reg.has_binding_for_window(SystemCapsuleId::AtoLaunch, w1));
        assert!(!reg.has_binding_for_window(SystemCapsuleId::AtoLaunch, w2));

        // Close w1 — now no binding.
        reg.unregister_window(w1);
        assert!(!reg.has_binding(SystemCapsuleId::AtoLaunch));
    }

    #[test]
    fn register_is_idempotent() {
        let wid = make_window_id(5);
        let mut reg = SystemCapsuleWindowRegistry::default();
        for _ in 0..3 {
            reg.windows
                .entry(SystemCapsuleId::AtoStore)
                .or_default()
                .insert(wid);
        }
        assert_eq!(
            reg.windows[&SystemCapsuleId::AtoStore].len(),
            1,
            "duplicate inserts must be deduplicated"
        );
    }
}
