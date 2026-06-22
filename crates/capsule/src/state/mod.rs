//! Runtime state schemas and helpers.
//!
//! - [`session`]: session record schema + validation helpers (formerly the
//!   standalone `capsule` crate, absorbed here so the wire/runtime
//!   surface lives in one place).

pub mod session;
