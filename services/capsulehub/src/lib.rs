//! Self-hosted capsulehub coordinator.
//!
//! Capsulehub is a blind distribution channel for already-formed `.capsule`
//! bundles. The client encrypts before upload, so this service only ever
//! receives opaque ciphertext addressed by the SHA-256 digest of its own bytes.
//!
//! The coordinator therefore:
//!
//! - stores and returns opaque bytes, nothing else,
//! - never decodes a bundle, never sees plaintext, and never receives a key,
//! - validates only the object id shape (`sha256:<64 lowercase hex>`),
//! - makes no Capsule identity, Contract, or admission decision.
//!
//! Client-side formed-capsule validation is a separate trust boundary owned by
//! the `ato hub push` / `ato hub pull` client (see the Capsulehub RFC).
//!
//! The official ato.run/capsulehub deployment is this same binary.
//!
//! # Deployment
//!
//! See `docs/ops/capsulehub.md` in the repository for the operator knobs,
//! retention, takedown-by-object-id, and the blinded-storage liability posture.

#![forbid(unsafe_code)]

pub mod auth;
pub mod config;
pub mod rate_limit;
pub mod storage;
pub mod sweep;

mod server;

pub use auth::Secret;
pub use config::{Config, PutAuth, ServerConfig};
pub use server::{RunningHub, serve, start_in_background};
