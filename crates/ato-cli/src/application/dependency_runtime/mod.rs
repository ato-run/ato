#![allow(dead_code, unused_imports)]
//! Runtime orchestration for dependency contracts (RFC §10).
//!
//! This module is the runtime counterpart to the lock-time verifier in
//! `capsule-core::foundation::dependency_contracts`. It implements:
//!
//! - §7.5 Dynamic endpoint allocation (`port = "auto"`)
//! - §7.6 Ready probe runtime (`tcp` and `probe` variants)
//! - §10.2 Startup sequence
//! - §10.4 Teardown ordering and orphan detection (warn-only in v1)
//!
//! Credential resolution & materialization (RFC §7.3.2) is delegated to
//! `application::dependency_credentials`. This module wires the two
//! together at provider start.
//!
//! Scope: this is a building-block module. Higher-level orchestration
//! (provider fetch + start in topological order, integration with
//! existing `app_control` desktop bootstrap) is added incrementally.

pub mod endpoint;
pub mod orchestrator;
pub mod orphan;
pub mod ready;
pub mod teardown;

#[cfg(test)]
mod tests;

pub use endpoint::{EndpointAllocator, EndpointError};
pub use orphan::{
    detect_orphan_state, write_session_sentinel, OrphanCheckOutcome, SessionSentinel,
};
pub use ready::{wait_for_ready, ReadyError, ReadyOutcome, ReadyProbeKind};
pub use teardown::{teardown_reverse_topological, TeardownError, TeardownTarget};
