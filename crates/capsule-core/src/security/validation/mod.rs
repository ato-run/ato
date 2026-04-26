//! Validation Module for Ato CLI
//!
//! This module contains validation logic migrated from nacelle.
//! In the pure runtime architecture:
//! - ato-cli performs all validation at build/pack time
//! - nacelle executes pre-validated bundles

pub mod source_policy;
