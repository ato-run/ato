//! External interfaces for the nacelle Engine (v2.0 - Simplified)
//!
//! This module contains HTTP interfaces for the nacelle runtime:
//! - [`dev_server`]: Development server with hot-reload support
//! - [`http`]: HTTP health checks and metrics endpoints
//! - [`api`]: REST API server (Axum-based)
//! - [`discovery`]: mDNS announcer for .local domain discovery
//!
//! ## Usage Patterns
//!
//! In v2.0, nacelle operates as a CLI-driven runtime without a central daemon.
//! Each capsule runs with its own embedded supervisor and can optionally expose
//! HTTP endpoints for monitoring and control.

// pub mod api; // Disabled in v2.0: daemon architecture removed
// pub mod dev_server; // Disabled in v2.0: daemon architecture removed
pub mod discovery;
pub mod http;
