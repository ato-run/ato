//! The Runtime's attempt: admission, a durable start record, execution of
//! one fixed Derivation, observation of the candidate over HTTP, and
//! verification against the frozen Contract with a receipt.
//!
//! Every entry that executes a Derivation — a local Formation, a Runtime
//! Network ticket, and (as they move here) the hosted job and a Run — uses
//! this crate for that. What it does NOT own: choosing which Derivation to
//! try, leases and fences, publication, and whether a verified candidate is
//! stopped or kept afterwards. Those stay with the caller.
//!
//! The verification point itself ([`verification`]) is also used by a
//! caller that observes a candidate it does not run — the Hosted `.capsule`
//! verifier — so K is decided and receipted the same way for both.
//!
//! Layer `runtime`: it may use the lower layers (lib, ipc, adapters,
//! materializers, objects) and is used by apps. It never depends on an app.

pub mod admission;
pub mod attempt;
pub mod browser_sandbox;
pub mod browser_verify;
pub mod build;
pub mod build_sandbox;
pub mod build_sandbox_exec;
pub mod ephemeral;
pub mod executor;
pub mod formation_realizer;
pub mod journal;
pub mod launch;
pub mod plan;
pub mod realize;
pub mod retained;
pub mod spec;
pub mod static_lane;
pub mod static_server;
pub mod text;
pub mod verification;
