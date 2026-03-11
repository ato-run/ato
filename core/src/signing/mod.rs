#![allow(unused_imports)]
//! Signing Module for Ato CLI
//!
//! Migrated from nacelle/src/verification/signing.rs
//! Handles Ed25519 signature creation and verification.

pub mod legacy_signer;
pub mod sign;
pub mod verify;

// Re-export common types
pub use legacy_signer::CapsuleSigner;
pub use sign::{sign_artifact, sign_bundle};
