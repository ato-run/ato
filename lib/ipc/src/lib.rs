//! Process-to-process wire DTOs.
//!
//! Semantic protocol and port identities live in `ato-computation`. This
//! crate contains only transport shapes used across real process boundaries:
//! computation commands, runtime binding leases, network control, and
//! terminal/session presentation messages.

#![forbid(unsafe_code)]

pub mod binding_control;
pub mod binding_lease;
pub mod computation;
pub mod desktop_control;
pub mod error;
pub mod net;
pub mod runtime_launch;
pub mod session_surface;
pub mod terminal_surface;

pub use error::{Result, WireError};
