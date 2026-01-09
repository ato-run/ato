//! External interfaces for the Capsuled Engine
//!
//! This module contains all server implementations and external API surfaces:
//! - [`grpc`]: gRPC server implementing the UARC Engine service
//! - [`dev_server`]: Embedded mode for use in `capsule-cli` with hot-reload support
//! - [`http`]: HTTP health checks and metrics endpoints
//! - [`api`]: REST API server (Axum-based)
//! - [`discovery`]: mDNS announcer for .local domain discovery
//!
//! ## Usage Patterns
//!
//! ### Standalone Server
//! Run capsuled as a standalone gRPC service:
//! ```bash
//! capsuled server --grpc-port 50051
//! ```
//!
//! ### Embedded Mode (capsule-cli)
//! Use [`dev_server::DevServerHandle`] to run an in-process engine:
//! ```ignore
//! use capsuled::dev_server::{DevServerConfig, DevServerHandle};
//!
//! let config = DevServerConfig::default().with_dev_mode(true);
//! let handle = DevServerHandle::start(config).await?;
//! println!("Engine at {}", handle.grpc_endpoint());
//! ```
//!
//! ## gRPC API
//!
//! The gRPC service implements the UARC Engine specification with methods:
//! - `DeployCapsule`: Start a new capsule instance
//! - `StopCapsule`: Terminate a capsule
//! - `GetResources`: Query available hardware resources
//! - `ValidateManifest`: Check manifest validity
//! - `GetSystemStatus`: Query engine health and loaded capsules
//! - `StreamLogs`: Stream capsule logs in real-time
//! - `FetchModel`: Download and cache models
//! - And more...

pub mod api;
pub mod dev_server;
pub mod discovery;
pub mod grpc;
pub mod http;

