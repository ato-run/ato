//! Capsule Control Protocol (CCP) wire-version constant.
//!
//! Stamped as `schema_version` on every JSON envelope emitted by
//! `ato app {resolve,session start,session stop,bootstrap,status,repair}`.
//! See `docs/specs/CCP_SPEC.md` for the additive-only versioning contract.
//!
//! History:
//!   - `ccp/v1` (v0.5.0+): canonical name aligned with PAS §4.1

/// CCP wire version: `"ccp/v1"`. Producers stamp this verbatim into the
/// `schema_version` field; consumers run [`super::tolerance`] over the wire
/// value to decide whether to accept, warn, or reject.
pub const SCHEMA_VERSION: &str = "ccp/v1";
