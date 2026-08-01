//! `net` — the network-plane wire surface shared by `ato-netd` (the daemon)
//! and its consumers (`ato-cli`, `ato-desktop`).
//!
//! These are pure data types and deterministic helpers: the control-plane
//! request/response envelopes, the DNS resolution and egress receipt DTOs,
//! and the stable-origin key derivation. The transport clients
//! (`Client` / `SyncClient`) and the resolver backends live next to the
//! code that runs them, because they pull in a Tokio runtime and
//! `hickory-resolver` respectively — neither belongs in the wire crate.
//!
//! Originally these lived in the standalone `ato-net` crate; that crate was
//! dissolved so the wire surface single-sources here while the runtime
//! pieces moved into `ato-netd` and per-caller `net_client` modules.

pub mod control;
pub mod ingress_http;
pub mod receipt;
pub mod resolver;
pub mod stable_origin;
