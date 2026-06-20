//! Capsule IPC Broker — Inter-Process Communication for Capsule Services
//!
//! This module implements the IPC Broker Core (Phase 13b), which coordinates
//! communication between capsule workloads across all runtimes (Source/OCI/Wasm).
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    ato-cli (IPC Broker)                  │
//! ├─────────────────────────────────────────────────────────────┤
//! │  registry   — Service discovery and lifecycle tracking       │
//! │  token      — Bearer token generation, validation, revocation│
//! │  schema     — JSON Schema input validation                   │
//! │  jsonrpc    — JSON-RPC 2.0 wire protocol                     │
//! │  refcount   — Reference counting and idle-timeout management │
//! │  dag        — DAG integration for IPC dependency ordering     │
//! │  broker     — Main orchestrator (resolve → start → connect)  │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Design Principles
//!
//! - **Smart Build, Dumb Runtime**: All IPC validation and orchestration
//!   happens here in ato-cli. nacelle only provides sandbox passthrough.
//! - **Universal Runtime**: IPC works across Source, OCI, and Wasm runtimes.
//! - **Process Boundary Pattern**: JSON-RPC 2.0 over Unix Domain Sockets.

pub mod broker;
pub mod dag;
pub(crate) mod guest_protocol;
pub mod inject;
pub(crate) mod jsonrpc;
#[cfg(test)]
pub mod refcount;
pub mod registry;
pub mod schema;
pub mod token;
pub mod types;
pub mod validate;
