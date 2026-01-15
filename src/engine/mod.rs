//! Capsule execution engine core
//!
//! This module contains the core logic for managing and executing capsules:
//! - [`CapsuleManager`]: Lifecycle management for capsule instances (deploy, stop, query)
//! - [`ProcessSupervisor`]: Child process monitoring and cleanup
//! - [`SocketManager`]: Socket Activation for zero-downtime port binding
//!
//! ## Architecture
//!
//! The engine acts as the main orchestrator for running Capsules. It coordinates with:
//! - **Runtimes** (via [`crate::runtime::Runtime`]) for workload execution
//! - **Artifact Manager** (via [`crate::resource::artifact::ArtifactManager`]) for CAS lookups
//! - **Manifest Verifier** (via [`crate::verification::verifier::ManifestVerifier`]) for security
//! - **Service Registry** (via [`crate::network::service_registry::ServiceRegistry`]) for networking
//!
//! ## Workload Lifecycle
//!
//! 1. **DeployCapsule**: Receive deployment request with manifest & resources
//! 2. **Verify**: Check signatures, validate manifest, verify CAS digests
//! 3. **Launch**: Select runtime, prepare bundle, start process via [`crate::runtime::Runtime`]
//! 4. **Monitor**: Track resource usage, collect logs, handle failures
//! 5. **Stop**: Gracefully terminate, cleanup resources, archive logs
//!
//! ## UARC V1.1.0 Compliance
//!
//! The engine implements all UARC Layer 4 (Engine) responsibilities:
//! - Manifest validation and signature verification
//! - CAS resource management and integrity checking
//! - Multi-runtime support (Wasm, Source, OCI)
//! - SPIFFE ID-based workload identity
//! - Resource quotas and isolation enforcement
//!
//! ## Socket Activation (Phase 2)
//!
//! Socket Activation allows the parent process to bind listening sockets before
//! spawning child processes. This provides:
//! - Zero port clash risk (parent owns the port)
//! - Instant request acceptance (no startup delay)
//! - Systemd-compatible FD passing (LISTEN_FDS environment variable)

// pub mod pool; // Disabled: requires capsule_runtime dependency
pub mod socket;
pub mod supervisor;
pub mod supervisor_mode;
