//! Common utilities and shared types
//!
//! This module contains cross-cutting concerns that are used throughout the codebase:
//! - Shared constants
//! - Shared paths

pub mod constants;
pub mod paths;

use std::sync::atomic::{AtomicBool, Ordering};

/// Whether this process is running the machine-clean `nacelle internal`
/// interface (stdout must stay JSON-only; progress/logs route to stderr).
///
/// This is process-local state, so it is tracked with a thread-safe global
/// rather than `std::env::set_var` (which is UB to call once worker threads
/// exist — `execute` runs inside the Tokio runtime).
static INTERNAL_MODE: AtomicBool = AtomicBool::new(false);

/// Mark this process as running in `nacelle internal` mode.
pub fn set_internal_mode() {
    INTERNAL_MODE.store(true, Ordering::Relaxed);
}

/// Returns true when this process is running the `nacelle internal` interface.
pub fn is_internal_mode() -> bool {
    INTERNAL_MODE.load(Ordering::Relaxed)
}
