//! Nacelle - Unified Runtime for Capsules
//!
//! This crate provides the core Runtime functionality for executing Capsules.
//! It can be used as a library (embedded mode) or as a standalone executable.
//!
//! ## Module Structure (v0.2.0 Simplified)
//!
//! - `common/` - Shared utilities (config, constants, paths)
//! - `manager/` - Execution management (supervisor, socket activation)
//! - `resource/` - Resource management (artifact, cas, storage)
//! - `launcher/` - Execution runtimes (Source, JIT provisioning)
//! - `schema/` - Cap'n Proto schema and conversion
//! - `system/` - OS-specific system abstractions + security (sandbox, verification)
//! - `workload/` - Workload definitions (manifest)
//!
//! ## Feature Flags
//!
//! - `wasm` - WebAssembly runtime support (cross-platform, default)
//! - `source` - Source runtime support (native sandbox)
//!
//! ## CLI Usage
//!
//! ```ignore
//! nacelle pack --bundle
//! ./my-app-bundle
//! ```

// =============================================================================
// Primary Modules (v0.2.0 Simplified Architecture)
// =============================================================================

pub mod bundle;
pub mod bundle_rules; // v3.0: Pre-validated sandbox rules loader
pub mod capsule_types; // Capsule type definitions (extracted from capsule-core)
pub mod common;
pub mod egress;
pub mod manager;
pub mod resource;
pub mod launcher;
pub mod runtime_config; // R3 config.json loader
pub mod schema;
pub mod system;
pub mod workload;

// =============================================================================
// Backward Compatibility Re-exports
// =============================================================================

// From common
pub use common::config;

// From schema
#[allow(dead_code)]
pub use schema::capnp as capsule_capnp;
pub use schema::converter as capnp_to_manifest;

// From manager (v0.2.0: manager removed, supervisor-based)
pub use manager::supervisor as process_supervisor;

// From resource
pub use resource::artifact;
pub use resource::cas;
pub use resource::ingest;
pub use resource::storage;

// From system (security alias)
pub use system as security;

// From workload
pub use workload::manifest;
