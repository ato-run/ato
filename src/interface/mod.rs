//! External interfaces for the Capsuled Engine
//!
//! This module contains all server implementations and external API surfaces:
//! - gRPC server (Engine service)
//! - HTTP server (health checks, metrics)
//! - REST API server (Axum-based)
//! - DevServer (embedded mode for capsule-cli)
//! - Discovery (mDNS announcer for .local domain)

pub mod api;
pub mod dev_server;
pub mod discovery;
pub mod grpc;
pub mod http;

// Re-export key types for convenience
pub use dev_server::{DevServerConfig, DevServerHandle};
