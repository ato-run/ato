//! Workload definition and manifest processing
//!
//! This module handles capsule workload specifications:
//! - Manifest: Legacy manifest types and resource definitions
//! - ManifestLoader: Capsule manifest loading and parsing
//! - RunPlan: (removed) proto-based conversion was deprecated

pub mod manifest;
pub mod manifest_loader;

#[cfg(test)]
mod tests;

// Re-export commonly used types
pub use manifest::{Manifest, Resource};
pub use manifest_loader::ResourceRequirements;
