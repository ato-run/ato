//! Capsule execution manager core
//!
//! This module contains the core logic for managing and executing capsules:
//! - [`CapsuleManager`]: Lifecycle management for capsule instances (deploy, stop, query)
//! - [`ProcessSupervisor`]: Child process monitoring and cleanup
//! - [`SocketManager`]: Socket Activation for zero-downtime port binding
//!
//! ## Architecture
//!
//! The manager acts as the main orchestrator for running Capsules. It coordinates with:
//! - **Runtimes** (via [`crate::launcher::Runtime`]) for workload execution
//! - **Service Registry**: removed in v0.2.0
//!
//! ## Workload Lifecycle
//!
//! 1. **Launch**: Start process via [`crate::launcher::Runtime`]
//! 2. **Monitor**: Track status, collect logs, handle failures
//! 3. **Stop**: Gracefully terminate
//!
//! ## UARC V1.1.0 Compliance
//!
//! The engine implements UARC Layer 4 (Engine) runtime responsibilities:
//! - Source runtime execution
//! - Isolation enforcement
//!
//! ## Socket Activation (Phase 2)
//!
//! Socket Activation allows the parent process to bind listening sockets before
//! spawning child processes. This provides:
//! - Zero port clash risk (parent owns the port)
//! - Instant request acceptance (no startup delay)
//! - Systemd-compatible FD passing (LISTEN_FDS environment variable)

// pub mod pool; // Disabled: requires capsule_runtime dependency
pub mod r3_supervisor;
pub mod socket;
pub mod supervisor;
