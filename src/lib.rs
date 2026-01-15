//! Nacelle - Unified Runtime for Capsules
//!
//! This crate provides the core Runtime functionality for executing Capsules.
//! It can be used as a library (embedded mode) or as a standalone executable.
//!
//! ## Module Structure (v2.0 Simplified)
//!
//! - `common/` - Shared utilities (auth, config, failure_codes)
//! - `engine/` - Execution core (supervisor, socket activation)
//! - `interface/` - External APIs (HTTP, discovery)
//! - `observability/` - L5 Observability (audit, job_history, logs, metrics)
//! - `resource/` - Resource management (artifact, cas, storage, oci)
//! - `runtime/` - Execution runtimes (Source, JIT provisioning)
//! - `schema/` - Cap'n Proto schema and conversion
//! - `system/` - System-level (hardware, network)
//! - `verification/` - Security layers (sandbox, verification)
//! - `workload/` - Workload definitions (manifest, runplan)
//!
//! ## Feature Flags
//!
//! - `wasm` - WebAssembly runtime support (cross-platform, default)
//! - `oci` - OCI container runtime support (Linux only)
//!
//! ## CLI Usage
//!
//! ```ignore
//! nacelle pack --bundle
//! ./my-app-bundle
//! ```

// =============================================================================
// Primary Modules (v2.0 Simplified Architecture)
// =============================================================================

pub mod bundle;
pub mod bundle_rules; // v3.0: Pre-validated sandbox rules loader
pub mod capsule_types; // Capsule type definitions (extracted from capsule-core)
pub mod common;
pub mod egress;
pub mod engine;
pub mod interface;
pub mod observability;
pub mod proto;
pub mod resource;
pub mod runtime;
pub mod runtime_config; // R3 config.json loader
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

// From interface (v2.0: gRPC/API removed, daemon removed)
pub use interface::http as http_server;

// From schema
#[allow(dead_code)]
pub use schema::capnp as capsule_capnp;
pub use schema::converter as capnp_to_manifest;

// From engine (v2.0: manager removed, supervisor-based)
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
