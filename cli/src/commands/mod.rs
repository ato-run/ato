//! Command implementations for Capsule CLI
//!
//! Clean CLI structure following open/close paradigm:
//! - Lifecycle: new, init, open, close, logs, ps
//! - Packaging: pack, keygen
//! - System: doctor

pub mod new;
pub mod init;
pub mod open;
pub mod close;
pub mod pack;
pub mod keygen;
pub mod logs;
pub mod ps;
pub mod doctor;
