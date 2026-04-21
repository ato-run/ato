#![allow(unused_imports)]
//! Signing Module for Ato CLI
//!
//! Handles Ed25519 signature creation and verification.

pub mod sign;
pub mod verify;

// Re-export common types
pub use sign::{sign_artifact, sign_bundle};
