//! Capsuled - UARC-compliant Capsule execution engine
//!
//! This crate provides the core Engine functionality for running Capsules.
//! It can be used as a library (embedded mode) or as a standalone server.
//!
//! ## Module Structure (UARC V1.1.0 Aligned)
//!
//! - `common/` - Shared utilities (auth, config, failure_codes)
//! - `engine/` - Capsule execution core (manager, supervisor, pool)
//! - `interface/` - External APIs (gRPC, HTTP, REST, DevServer)
//! - `observability/` - L5 Observability (audit, job_history, logs, metrics)
//! - `resource/` - Resource management (artifact, cas, storage, oci)
//! - `runtime/` - Execution runtimes (Wasm, Source, OCI)
//! - `schema/` - Cap'n Proto schema and conversion
//! - `system/` - System-level (hardware, network)
//! - `verification/` - UARC verification layers (L1-L4)
//! - `workload/` - Workload definitions (manifest, runplan)
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

// =============================================================================
// Primary Modules (UARC V1.1.0 Architecture)
// =============================================================================

pub mod capsule_types; // Capsule type definitions (extracted from capsule-core)
pub mod common;
pub mod engine;
pub mod interface;
pub mod observability;
pub mod proto;
pub mod resource;
pub mod runtime;
pub mod schema;
pub mod system;
pub mod verification;
pub mod workload;

// =============================================================================
// Backward Compatibility Re-exports
// =============================================================================

// From common
pub use common::auth;
pub use common::config;
pub use common::failure_codes;

// From interface
pub use interface::api as api_server;
pub use interface::dev_server;
pub use interface::grpc as grpc_server;
pub use interface::http as http_server;

// From schema
#[allow(dead_code)]
pub use schema::capnp as capsule_capnp;
pub use schema::converter as capnp_to_manifest;

// From engine
pub use engine::manager as capsule_manager;
// pub use engine::pool as pool_registry; // Disabled: capsule_runtime dependency removed
pub use engine::supervisor as process_supervisor;

// From resource
pub use resource::artifact;
pub use resource::cas;
pub use resource::ingest;
pub use resource::oci;
pub use resource::storage;

// From observability
pub use observability::job_history;
pub use observability::logs;
pub use observability::metrics;

// From system
pub use system::hardware;
pub use system::network;

// From verification (security alias)
pub use verification as security;

// From workload
pub use workload::manifest;
pub use workload::runplan;

// =============================================================================
// Public API
// =============================================================================

// Re-export key types for embedded usage
pub use interface::dev_server::{DevServerConfig, DevServerHandle};
