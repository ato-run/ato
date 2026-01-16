//! Resource management modules
//!
//! This module handles all resource acquisition and storage:
//! - Artifact: Capsule artifact management and caching
//! - CAS: Content-Addressable Storage client
//! - Storage: Volume and directory management for capsules
//! - Ingest: External resource fetching and CAS ingestion

pub mod artifact;
pub mod cas;
pub mod ingest;
pub mod storage;

// Re-export commonly used types
pub use artifact::ArtifactManager;
pub use cas::{CasClient, HttpCasClient, LocalCasClient};
pub use storage::{StorageConfig, StorageManager};
