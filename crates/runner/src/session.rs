//! Session lifecycle over a [`crate::supervisor::ProcessSupervisor`].
//!
//! **Skeleton (Phase 1 Step 0):** the types and the module boundary. The
//! concrete launch / stop / restart / list flow and the retention policy
//! (TTL = 5 min, LRU) migrate here from the desktop's `orchestrator.rs` and
//! `retention.rs` during the Phase 1 redistribution (groundwork §2 Steps 6–7).
//! Kept minimal so nothing speculative ships ahead of the real move.

/// Stable identifier for a supervised session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(pub String);

/// Lifecycle state of a supervised session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Spawned, not yet observed ready.
    Starting,
    /// Running and observed ready.
    Running,
    /// Teardown requested, not yet confirmed stopped.
    Stopping,
    /// Confirmed stopped.
    Stopped,
    /// Exited abnormally.
    Failed,
}

/// Supervises the set of live sessions on a host. Placeholder in the skeleton —
/// the retention (TTL/LRU) policy and the launch/stop wiring land here in
/// Phase 1. Exists now so the module boundary and public surface are fixed
/// before the redistribution begins.
#[derive(Debug, Default)]
pub struct SessionSupervisor {
    _private: (),
}

impl SessionSupervisor {
    /// Create an empty session supervisor.
    pub fn new() -> Self {
        Self::default()
    }
}
