//! Nacelle - Unified Runtime for Capsules
//!
//! This crate provides the core Runtime functionality for executing Capsules.
//! It can be used as a library (embedded mode) or as a standalone executable.
//!
//! ## Module Structure (v0.3.0 Thin Runtime)
//!
//! - `common/` - Shared utilities (constants, paths)
//! - `config/` - Runtime config.json structures and validation
//! - `manager/` - Execution management (supervisor, socket activation)
//! - `launcher/` - Execution runtimes (Source, JIT provisioning)
//! - `system/` - OS-specific system abstractions + security (sandbox)
//!
//! ## Feature Flags
//!
//! - `wasm` - WebAssembly runtime support (cross-platform, default)
//! - `source` - Source runtime support (native sandbox)
//!
//! ## CLI Usage
//!
//! ```ignore
//! nacelle internal features
//! ```

// =============================================================================
// Primary Modules (v0.2.0 Simplified Architecture)
// =============================================================================

pub mod bundle;
pub mod bundle_rules; // v3.0: Pre-validated sandbox rules loader
pub mod common;
pub mod config; // R3 config.json loader
pub mod manager;
pub mod launcher;
pub mod system;

// =============================================================================
// Backward Compatibility Re-exports
// =============================================================================

// From manager (v0.2.0: manager removed, supervisor-based)
pub use manager::supervisor as process_supervisor;

// From system (security alias)
pub use system as security;

