/// Storage management module for LVM and LUKS
///
/// This module provides abstractions for:
/// - LVM (Logical Volume Manager) volume management
/// - LUKS (Linux Unified Key Setup) encryption
/// - Unified StorageManager for capsule workloads
///
/// All implementations are pure Rust with no CGO dependencies.
pub mod error;
pub mod luks;
pub mod lvm;
pub mod manager;

pub use error::{StorageError, StorageResult};
pub use luks::{EncryptedVolumeInfo, KeyStorage, LuksManager};
pub use lvm::{LvmManager, ThinPoolInfo, VolumeInfo};
pub use manager::{CapsuleStorage, StorageConfig, StorageManager};
