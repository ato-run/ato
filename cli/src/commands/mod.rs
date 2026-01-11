//! Command implementations for Nacelle CLI
//!
//! Clean CLI structure following open/close paradigm:
//! - Lifecycle: new, init, open, close, logs, ps
//! - Packaging: pack, keygen
//! - System: doctor
//!
//! v2.0: Self-extracting bundle support

pub mod dev;
pub mod internal;
pub mod pack_v2; // v2.0 self-extracting bundler
