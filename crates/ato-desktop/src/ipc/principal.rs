//! IPC principal — who is the caller?
//!
//! Every IPC request that enters the broker carries an `IpcPrincipal`
//! that was resolved **entirely on the Rust side** from the WebView
//! handle (system capsule) or from the active session context (guest
//! capsule).  The raw JS envelope is never used to determine identity.
//!
//! # Resolution paths
//!
//! * **System capsule** — the host `AnyWindowHandle` is looked up in
//!   the `SystemCapsuleBinding` registry; the binding supplies the
//!   `SystemCapsuleId` and canonical slug.  JS envelope `capsule`
//!   field is ignored.
//!
//! * **Guest capsule** — the active `GuestSessionContext`
//!   (`bridge.rs`/`state/session.rs`) supplies handle, target, ids,
//!   and the stable origin.  JS `isSystem` flag is ignored.
//!
//! * **Desktop shell** — internal actions dispatched directly from
//!   GPUI code, not from a WebView.

use std::path::PathBuf;

use crate::system_capsule::broker::SystemCapsuleId;

/// The resolved identity of an IPC caller.
#[derive(Debug, Clone)]
pub enum IpcPrincipal {
    /// A built-in system capsule WebView.
    SystemCapsule {
        id: SystemCapsuleId,
        /// Short canonical slug (e.g. `"onboarding"`, `"store"`).
        canonical_slug: String,
        /// Absolute path to the materialised capsule assets (used for
        /// origin verification).
        materialized_root: PathBuf,
    },
    /// A user-launched capsule running in a guest WebView pane.
    Capsule {
        /// Capsule handle (e.g. `"my-org/my-tool@0.1.0"`).
        handle: String,
        /// Resolved target spec.
        target: String,
        /// Runtime execution ID for this invocation.
        execution_id: String,
        /// The session this capsule belongs to.
        session_id: String,
        /// The stable origin of the capsule's WebView content.
        origin: String,
    },
    /// Direct dispatch from GPUI shell code (no WebView involved).
    DesktopShell,
}

impl IpcPrincipal {
    /// Returns `true` if this principal is a system capsule with the
    /// given `SystemCapsuleId`.
    pub fn is_system_capsule(&self, id: SystemCapsuleId) -> bool {
        matches!(self, Self::SystemCapsule { id: sid, .. } if *sid == id)
    }

    /// Returns `true` if this principal is any system capsule.
    pub fn is_any_system_capsule(&self) -> bool {
        matches!(self, Self::SystemCapsule { .. })
    }

    /// Returns `true` if this principal is a guest capsule.
    pub fn is_guest_capsule(&self) -> bool {
        matches!(self, Self::Capsule { .. })
    }

    /// Returns `true` if this principal is the desktop shell itself.
    pub fn is_desktop_shell(&self) -> bool {
        matches!(self, Self::DesktopShell)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_capsule_identity_checks() {
        let p = IpcPrincipal::SystemCapsule {
            id: SystemCapsuleId::AtoOnboarding,
            canonical_slug: "onboarding".into(),
            materialized_root: PathBuf::from("/tmp/onboarding"),
        };
        assert!(p.is_system_capsule(SystemCapsuleId::AtoOnboarding));
        assert!(!p.is_system_capsule(SystemCapsuleId::AtoStore));
        assert!(p.is_any_system_capsule());
        assert!(!p.is_guest_capsule());
        assert!(!p.is_desktop_shell());
    }

    #[test]
    fn guest_capsule_identity_checks() {
        let p = IpcPrincipal::Capsule {
            handle: "org/tool@0.1".into(),
            target: "wasm".into(),
            execution_id: "exec-1".into(),
            session_id: "sess-1".into(),
            origin: "capsule://atousercontent.com/org/tool".into(),
        };
        assert!(p.is_guest_capsule());
        assert!(!p.is_any_system_capsule());
        assert!(!p.is_desktop_shell());
    }
}
