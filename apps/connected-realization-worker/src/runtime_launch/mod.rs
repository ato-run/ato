//! The Runner's side of the `ato.runtime-launch-spec.v1` contract.
//!
//! `ato-ipc` owns the wire type; this owns what the wire type deliberately
//! withholds. Keeping the two in different crates is what stops a redeemed
//! secret or a real host path from becoming reachable to the guest agent, the
//! CLI or netd, all of which link the contract crate.

pub mod process_executor;
pub mod resolved;
pub mod session;
pub mod state_artifact;
