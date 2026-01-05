//! Schema and serialization modules
//!
//! This module handles Cap'n Proto schema and manifest conversion:
//! - capnp: Generated Cap'n Proto code for capsule schema
//! - converter: Cap'n Proto ↔ CapsuleManifestV1 conversion (UARC V1.1.0)

#[allow(dead_code)]
pub mod capnp;
pub mod converter;

// Re-export conversion functions
pub use converter::manifest_to_capnp_bytes;
