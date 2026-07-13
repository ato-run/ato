//! `ato-netd` — session-scoped ato network broker.
//!
//! The library target exposes the daemon's internals so the integration
//! tests (and the thin `main.rs` binary) can drive them. The control-plane
//! wire types are single-sourced from `protocol::net`; the runtime
//! transport client and the `hickory`-backed resolver backends live in
//! [`net`] (previously the standalone `ato-net` crate).

pub mod egress;
pub mod identity;
pub mod ingress;
pub mod net;
pub mod pixel_gateway;
pub mod server;
pub mod state;
