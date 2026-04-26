//! Capsule Control Protocol (CCP) — wire shape shared by `ato-cli`
//! (producer) and `ato-desktop` (consumer).
//!
//! The CCP defines the JSON envelope emitted by every
//! `ato app {resolve,session start,session stop,bootstrap,status,repair}`
//! invocation. Both sides must agree on:
//!
//! - the version string [`SCHEMA_VERSION`] stamped onto every envelope, and
//! - the tolerance rules a consumer applies when the wire version differs
//!   from its compile-time expectation ([`tolerance`]).
//!
//! Living in `capsule-core` makes the contract single-sourced: a producer
//! change that breaks the consumer fails to compile against the same crate
//! version, instead of silently drifting across two repositories.
//!
//! See `docs/specs/CCP_SPEC.md` and `docs/monorepo-consolidation-plan.md`
//! §M4 for the broader contract.

pub mod schema;
pub mod tolerance;
pub mod version;

pub use schema::CcpHeader;
pub use tolerance::{
    classify_schema_version, enforce_ccp_compat, CcpCompat, HasSchemaVersion,
    MalformedSchemaVersion,
};
pub use version::SCHEMA_VERSION;
