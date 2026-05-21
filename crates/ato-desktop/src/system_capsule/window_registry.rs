//! Runtime window handle registry for active system capsule WebViews.
//!
//! `SystemCapsuleWindowRegistry` maps each `SystemCapsuleId` to the GPUI
//! window that currently hosts it.  It is a `'static` GPUI global.
//!
//! # Usage
//!
//! - **At window creation**: call `register(id, handle)` after opening the GPUI
//!   window.
//! - **At window close** (or when the slot becomes `None`): call
//!   `unregister(id)`.
//! - **In the drain loop** (`spawn_drain_loop_inner`): call `has_binding(id)` to
//!   confirm a live window exists before dispatching IPC commands.  Without a
//!   registered window handle the IPC is denied so that commands cannot be
//!   dispatched to a capsule that never opened a window.
//!
//! The `gpui::Global` impl lives in `window/mod.rs` (see the comment there).

use std::collections::HashMap;

use gpui::AnyWindowHandle;

use super::broker::SystemCapsuleId;

/// Registry mapping active system capsule IDs to their host window handles.
#[derive(Debug, Default)]
pub struct SystemCapsuleWindowRegistry {
    windows: HashMap<SystemCapsuleId, AnyWindowHandle>,
}

impl SystemCapsuleWindowRegistry {
    /// Register a live host window for a system capsule.
    ///
    /// If the capsule was already registered (e.g. the window was re-opened),
    /// the old handle is silently replaced.
    pub fn register(&mut self, id: SystemCapsuleId, handle: AnyWindowHandle) {
        self.windows.insert(id, handle);
    }

    /// Remove the binding for a system capsule (called on window close).
    pub fn unregister(&mut self, id: SystemCapsuleId) {
        self.windows.remove(&id);
    }

    /// Returns `true` if there is a registered host window for `id`.
    pub fn has_binding(&self, id: SystemCapsuleId) -> bool {
        self.windows.contains_key(&id)
    }

    /// Returns the registered handle, if any.
    pub fn get(&self, id: SystemCapsuleId) -> Option<AnyWindowHandle> {
        self.windows.get(&id).copied()
    }
    /// Returns the `SystemCapsuleId` registered under the given GPUI window
    /// handle, if any. Used by `on_window_closed` to unregister the binding
    /// by window id without knowing the capsule id in advance.
    pub fn find_by_window_id(&self, window_id: gpui::WindowId) -> Option<SystemCapsuleId> {
        self.windows
            .iter()
            .find(|(_, h)| h.window_id() == window_id)
            .map(|(id, _)| *id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_id() -> SystemCapsuleId {
        SystemCapsuleId::AtoStore
    }

    // Note: AnyWindowHandle cannot be cheaply constructed in unit tests (it
    // requires a running GPUI runtime), so we only test the registry logic.

    #[test]
    fn has_binding_false_when_empty() {
        let reg = SystemCapsuleWindowRegistry::default();
        assert!(!reg.has_binding(dummy_id()));
    }

    #[test]
    fn unregister_of_unknown_id_is_noop() {
        let mut reg = SystemCapsuleWindowRegistry::default();
        reg.unregister(dummy_id()); // must not panic
        assert!(!reg.has_binding(dummy_id()));
    }
}
