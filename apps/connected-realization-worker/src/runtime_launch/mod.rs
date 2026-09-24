//! The Runner's side of the `ato.runtime-launch-spec.v1` contract.
//!
//! `ato-ipc` owns the wire type; this owns what the wire type deliberately
//! withholds. Keeping the two in different crates is what stops a redeemed
//! secret or a real host path from becoming reachable to the guest agent, the
//! CLI or netd, all of which link the contract crate.

// Process launch lives in the shared Runtime crate; the Runner uses it
// through these paths.
pub use ato_runtime_attempt::launch::{process_executor, resolved, sandbox, sandbox_exec};

pub mod lease;
pub mod network_broker;
pub mod recovery;
pub mod service_group;
pub mod session;
pub mod state_artifact;
pub mod volume;
pub mod volume_maintenance;
pub mod workspace;
