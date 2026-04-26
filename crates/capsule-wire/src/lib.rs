//! `capsule-wire` — the **IPC surface** shared by `ato-cli` (the producer
//! that emits JSON envelopes from `ato app {resolve,session start,…}`) and
//! `ato-desktop` (the consumer that parses them).
//!
//! # Why a separate crate
//!
//! The CLI and Desktop release independently and historically had their own
//! local type mirrors of the same wire shape. That made silent drift
//! possible: a producer-side rename would compile, ship, and only fail at
//! runtime when Desktop tried to deserialize the new field name. Collapsing
//! both sides onto one crate (M4 for `ccp`, M5 for `ConfigField`) made
//! drift physically impossible.
//!
//! Splitting that shared surface out of `capsule-core` (N2) is the same
//! move at workspace scope: `capsule-core` carries heavy runtime deps
//! (`bollard`, `tonic`, `rusqlite`, `reqwest`, `axum`, `prost`,
//! tokio-`full`) that the Desktop has no business linking. By moving the
//! pure types into `capsule-wire`, Desktop links only what it actually
//! needs (`serde`, `tracing`, `thiserror`), and the dependency direction
//! becomes a DAG that can be enforced at CI time (N4).
//!
//! # Modules
//!
//! - [`error`] — [`WireError`], the slim parser error returned by the
//!   handle classifier. Convertible into `capsule_core::CapsuleError` via
//!   a `From` impl living in capsule-core, so existing `?` paths in the
//!   CLI keep working unchanged.
//! - [`ccp`] — Capsule Control Protocol envelope header + tolerance rules.
//!   Single-sourced from M4.
//! - [`handle`] — URL/handle classifier (`classify_surface_input`,
//!   `normalize_capsule_handle`, `parse_host_route`, …). The Desktop
//!   omnibar and the CLI run/resolve commands route through the exact
//!   same parser tree.
//! - [`config`] — [`ConfigField`] / [`ConfigKind`], the dynamic config-form
//!   schema returned in the E103 missing-env envelope. Single-sourced from
//!   M5.
//!
//! # Stability
//!
//! Anything in this crate is wire-shape: changes are observable in the
//! JSON contract and must be coordinated with the spec docs
//! (`docs/specs/CCP_SPEC.md`, `docs/rfcs/accepted/CAPSULE_HANDLE_SPEC.md`).
//! Treat additions as additive (add new fields with `#[serde(default)]`,
//! new enum variants behind tolerance rules) and renames as breaking.

pub mod ccp;
pub mod config;
pub mod error;
pub mod handle;

pub use error::{Result, WireError};
