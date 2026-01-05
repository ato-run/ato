//! Capsule execution engine core
//!
//! This module contains the core logic for managing and executing capsules:
//! - CapsuleManager: Lifecycle management for capsule instances
//! - ProcessSupervisor: Child process monitoring and cleanup

pub mod manager;
// pub mod pool; // Disabled: requires capsule_runtime dependency
pub mod supervisor;

// Re-export key types for convenience
pub use manager::{Capsule, CapsuleManager, CapsuleStatus, DeployCapsuleRequest};
// pub use pool::PoolRegistry; // Disabled
pub use supervisor::ProcessSupervisor;
