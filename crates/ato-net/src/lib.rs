//! `ato-net` — shared library for the ato network plane.
//!
//! Per #294 / #296 this crate is the API surface that consumers (CLI,
//! Desktop, future runtime wiring) depend on. The `ato-netd` daemon
//! depends on the same types so the wire protocol cannot drift between
//! client and server.
//!
//! Slice **A** ships the `control` module (typed client + wire types).
//! The remaining modules are intentional placeholders that follow-up
//! slices fill in:
//!
//! - `resolver` — DNS resolver abstraction (slice **D**, #299).
//! - `policy` — hostname / CIDR matcher (slice **E**, #300).
//! - `proxy_core` — shared HTTP/WebSocket plumbing for ingress and
//!   egress (slices **B** + **E**, #297 + #300).
//! - `receipt` — typed network-receipt event structs (slice **D**,
//!   #299).
//! - `stable_origin` — `stable_origin_key` derivation (slice **B**/**C**,
//!   #297 + #298 — `logical_capsule_key_for_stable_origin` migrates here
//!   from `ato-desktop`).
//!
//! Placeholders are public empty modules on purpose: downstream crates
//! can name them in `use` statements right now, and the follow-up PRs
//! add items without renaming the path.

pub mod control;

pub mod resolver;

pub mod policy {
    //! Hostname / CIDR policy matcher. Filled in by slice **E** (#300).
}

pub mod proxy_core {
    //! Shared HTTP / WebSocket proxy plumbing. Filled in by slices **B**
    //! (#297) and **E** (#300).
}

pub mod receipt;

/// Stable-origin key derivation — pure functions, no I/O.
///
/// Slice **B** (#297) fills this in with `stable_host_label_for_key`.
/// Slice **C** (#298) will add the `logical_capsule_key_for_stable_origin`
/// migration from `ato-desktop::stable_origin_proxy`.
pub mod stable_origin;
