//! Capsuled - UARC-compliant Capsule execution engine
//!
//! This crate provides the core Engine functionality for running Capsules.
//! It can be used as a library (embedded mode) or as a standalone server.
//!
//! ## Feature Flags
//!
//! - `wasm` - WebAssembly runtime support (cross-platform, default)
//! - `oci` - OCI container runtime support (Linux only)
//!
//! ## Embedded Usage (capsule-cli)
//!
//! ```ignore
//! use capsuled::dev_server::{DevServerConfig, DevServerHandle};
//!
//! let config = DevServerConfig::default();
//! let handle = DevServerHandle::start(config).await?;
//! // ... use handle.endpoint() to connect
//! handle.shutdown().await;
//! ```

pub mod artifact;
#[allow(dead_code)]
pub mod capsule_capnp; // Cap'n Proto generated code
pub mod capsule_manager;
pub mod capnp_to_manifest; // Cap'n Proto ↔ CapsuleManifestV1 conversion (UARC V1.1.0)
pub mod cas; // CAS client abstraction (UARC V1.1.0)
pub mod common;
// pub mod coordinator_service;  // Disabled: proto definitions not present in capsuled/proto
pub mod downloader; // Enabled for Phase 2
pub mod interface; // External interfaces (gRPC, HTTP, API, DevServer)

// Re-exports from common for backward compatibility
pub use common::auth;
pub use common::config;
pub use common::failure_codes;

// Re-exports from interface for backward compatibility
pub use interface::api as api_server;
pub use interface::dev_server;
pub use interface::grpc as grpc_server;
pub use interface::http as http_server;
pub mod hardware;
pub mod job_history; // Job history persistence (UARC V1.1.0)
pub mod logs;
pub mod manifest;
pub mod metrics;
pub mod model_fetcher;
pub mod network;
#[cfg(target_os = "linux")]
pub mod oci;
#[cfg(not(target_os = "linux"))]
pub mod oci {
    //! OCI module stub for non-Linux platforms
    //! OCI container runtime is only available on Linux.
    
    /// Stub module for spec_builder
    pub mod spec_builder {
        use capsule_core::capsule_v1::{CapsuleExecution, StorageVolume};
        use crate::workload::manifest_loader::ResourceRequirements;
        use capsule_core::capsule_v1::CapsuleManifestV1;
        
        /// Stub function that returns an error on non-Linux platforms
        #[allow(clippy::too_many_arguments)]
        pub fn build_oci_spec(
            _rootfs_path: &std::path::Path,
            _execution: &CapsuleExecution,
            _volumes: &[StorageVolume],
            _gpu_uuids: Option<&[String]>,
            _allowed_paths: &[String],
            _resources: Option<&ResourceRequirements>,
            _extra_args: Option<&[String]>,
            _manifest: &CapsuleManifestV1,
        ) -> Result<oci_spec::runtime::Spec, String> {
            Err("OCI runtime is only available on Linux. Use source or wasm runtime instead.".into())
        }
    }
}
pub mod pool_registry;
pub mod process_supervisor;
pub mod proto;
pub mod runplan;
pub mod runtime;
pub mod security;
// pub mod status_reporter;
pub mod storage;
pub mod wasm_host;
pub mod workload;

// Re-export key types for embedded usage
pub use interface::dev_server::{DevServerConfig, DevServerHandle};

// TODO: Re-enable when capnp proto generation is set up
// #[cfg(test)]
// mod capnp_roundtrip_test;
