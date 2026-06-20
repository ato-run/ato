//! Runtime state schemas and helpers.
//!
//! - [`session`]: session record schema + validation helpers (formerly the
//!   standalone `ato-session-core` crate, absorbed here so the wire/runtime
//!   surface lives in one place).

pub mod session;
