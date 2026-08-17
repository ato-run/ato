//! Network plane internals for `ato-netd`.
//!
//! The wire DTOs (control envelopes, resolver/receipt DTOs, stable-origin
//! derivation) live in `ato_ipc::net`; this module re-exports them under
//! the daemon-local `crate::net::*` paths and adds the runtime pieces that
//! cannot live in the wire crate: the Tokio control [`client::Client`] and the
//! `hickory`-backed resolver backends.
//!
//! This was previously the standalone `ato-net` crate; it was dissolved so the
//! wire surface single-sources in `protocol` and the runtime code lives
//! next to the daemon that runs it.

pub mod client;

/// Control-plane surface: wire DTOs from `ato_ipc::net::control`, plus
/// the daemon-local async transport [`Client`].
pub mod control {
    pub use ato_ipc::net::control::*;

    pub use super::client::Client;
}

/// DNS resolver surface: the transport-neutral DTOs from
/// `ato_ipc::net::resolver`, plus the `hickory`-backed backends.
pub mod resolver {
    pub use ato_ipc::net::resolver::*;

    pub use super::resolver_backends::{Chain, DohResolver, Resolver, SystemResolver};
}

#[path = "resolver.rs"]
mod resolver_backends;

pub use ato_ipc::net::receipt;
pub use ato_ipc::net::stable_origin;
