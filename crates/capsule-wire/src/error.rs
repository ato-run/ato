//! Slim error type for capsule-wire's pure parsers.
//!
//! `capsule-core::CapsuleError` is rich (it carries variants for HTTP,
//! container engines, sidecar IPC, crypto, etc.) and pulls in `reqwest`
//! transitively. The handle parser only ever needs to say "the input was
//! malformed" — there is no I/O, no network, no process. Defining a slim
//! local [`WireError`] here keeps `capsule-wire`'s dependency surface
//! minimal (`thiserror` only).
//!
//! Callers in `capsule-core` that propagate handle-parser errors into a
//! `Result<_, CapsuleError>` get auto-conversion via an `impl
//! From<WireError> for CapsuleError` declared in capsule-core's
//! `foundation::error` — the `?` operator at the call site is unchanged.

use thiserror::Error;

/// Parser/validator error for the wire surface.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum WireError {
    /// The input did not match the expected wire shape (malformed handle,
    /// missing required component, unsupported authority, …).
    #[error("Configuration error: {0}")]
    Config(String),
}

/// Convenience alias matching the `capsule-core` shape so files moved
/// from there keep their `Result<T>` signatures verbatim.
pub type Result<T> = std::result::Result<T, WireError>;
