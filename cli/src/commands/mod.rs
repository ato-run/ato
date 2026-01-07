//! Command implementations for Capsule CLI
//!
//! Clean CLI structure following open/close paradigm:
//! - Lifecycle: new, init, open, close, logs, ps
//! - Packaging: pack, keygen
//! - System: doctor

pub mod close;
pub mod doctor;
pub mod init;
pub mod keygen;
pub mod logs;
pub mod new;
pub mod open;
pub mod pack;
pub mod ps;
