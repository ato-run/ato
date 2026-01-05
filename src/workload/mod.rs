//! Workload definition and manifest processing
//!
//! This module handles capsule workload specifications:
//! - Manifest: Legacy manifest types and resource definitions
//! - ManifestLoader: Capsule manifest loading and parsing
//! - RunPlan: Coordinator RunPlan to CapsuleManifestV1 conversion

pub mod manifest;
pub mod manifest_loader;
pub mod runplan;

#[cfg(test)]
mod tests;

// Re-export commonly used types
pub use manifest::{Manifest, Resource};
pub use manifest_loader::ResourceRequirements;
