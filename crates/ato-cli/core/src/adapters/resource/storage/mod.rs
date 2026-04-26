/// Storage management module for Capsule workloads (SPEC V1.1.0)
///
/// This module provides directory-based storage for capsules.
/// LVM/LUKS has been removed as per SPEC V1.1.0:
/// - Engine should be stateless  
/// - Complex block device management is delegated to OS or Coordinator
pub mod error;
pub mod manager;

pub use error::{StorageError, StorageResult};
pub use manager::{CapsuleStorage, StorageConfig, StorageManager};
