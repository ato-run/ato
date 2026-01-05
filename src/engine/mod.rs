//! Capsule execution engine core
//!
//! This module contains the core logic for managing and executing capsules:
//! - CapsuleManager: Lifecycle management for capsule instances
//! - ProcessSupervisor: Child process monitoring and cleanup
//! - PoolRegistry: Pre-warmed container pool management

pub mod manager;
pub mod pool;
pub mod supervisor;

// Re-export key types for convenience
pub use manager::{Capsule, CapsuleManager, CapsuleStatus, DeployCapsuleRequest};
pub use pool::PoolRegistry;
pub use supervisor::ProcessSupervisor;
