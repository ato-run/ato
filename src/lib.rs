//! Capsuled - UARC-compliant Capsule execution engine
//!
//! This crate provides the core Engine functionality for running Capsules.
//! It can be used as a library (embedded mode) or as a standalone server.
//!
//! ## Feature Flags
//!
//! - `wasm` - WebAssembly runtime support (cross-platform, default)
//! - `oci` - OCI container runtime support (Linux only)
//!
//! ## Embedded Usage (capsule-cli)
//!
//! ```ignore
//! use capsuled::dev_server::{DevServerConfig, DevServerHandle};
//!
//! let config = DevServerConfig::default();
//! let handle = DevServerHandle::start(config).await?;
//! // ... use handle.endpoint() to connect
//! handle.shutdown().await;
//! ```

pub mod common;
// pub mod coordinator_service;  // Disabled: proto definitions not present in capsuled/proto
pub mod engine; // Capsule execution core (manager, supervisor, pool)
pub mod interface; // External interfaces (gRPC, HTTP, API, DevServer)
pub mod resource; // Resource management (artifact, cas, storage, oci, downloader)
pub mod schema; // Cap'n Proto schema and conversion

// Re-exports from common for backward compatibility
pub use common::auth;
pub use common::config;
pub use common::failure_codes;

// Re-exports from interface for backward compatibility
pub use interface::api as api_server;
pub use interface::dev_server;
pub use interface::grpc as grpc_server;
pub use interface::http as http_server;

// Re-exports from schema for backward compatibility
#[allow(dead_code)]
pub use schema::capnp as capsule_capnp;
pub use schema::converter as capnp_to_manifest;

// Re-exports from engine for backward compatibility
pub use engine::manager as capsule_manager;
pub use engine::pool as pool_registry;
pub use engine::supervisor as process_supervisor;

// Re-exports from resource for backward compatibility
pub use resource::artifact;
pub use resource::cas;
pub use resource::downloader;
pub use resource::model_fetcher;
pub use resource::oci;
pub use resource::storage;

pub mod job_history; // Job history persistence (UARC V1.1.0)
pub mod logs;
pub mod metrics;
pub mod system; // System-level modules (hardware, network)

// Re-exports from system for backward compatibility
pub use system::hardware;
pub use system::network;

pub mod proto;
pub mod runtime;
pub mod security;
// pub mod status_reporter;
pub mod wasm_host;
pub mod workload;

// Re-exports from workload for backward compatibility
pub use workload::manifest;
pub use workload::runplan;

// Re-export key types for embedded usage
pub use interface::dev_server::{DevServerConfig, DevServerHandle};

// TODO: Re-enable when capnp proto generation is set up
// #[cfg(test)]
// mod capnp_roundtrip_test;
