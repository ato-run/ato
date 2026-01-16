//! Nacelle - Unified Runtime for Capsules
//!
//! This crate provides the core Runtime functionality for executing Capsules.
//! It can be used as a library (embedded mode) or as a standalone executable.
//!
//! ## Module Structure (v0.2.0 Simplified)
//!
//! - `common/` - Shared utilities (config, constants, paths)
//! - `engine/` - Execution core (supervisor, socket activation)
//! - `resource/` - Resource management (artifact, cas, storage)
//! - `runtime/` - Execution runtimes (Source, JIT provisioning)
//! - `schema/` - Cap'n Proto schema and conversion
//! - `system/` - OS-specific system abstractions (eBPF/WFP/PF)
//! - `verification/` - Security layers (sandbox, verification)
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
pub mod engine;
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
pub use common::config;

// From schema
#[allow(dead_code)]
pub use schema::capnp as capsule_capnp;
pub use schema::converter as capnp_to_manifest;

// From engine (v0.2.0: manager removed, supervisor-based)
pub use engine::supervisor as process_supervisor;

// From resource
pub use resource::artifact;
pub use resource::cas;
pub use resource::ingest;
pub use resource::storage;

// From verification (security alias)
pub use verification as security;

// From workload
pub use workload::manifest;
