//! Resource management modules
//!
//! This module handles all resource acquisition and storage:
//! - Artifact: Capsule artifact management and caching
//! - CAS: Content-Addressable Storage client
//! - Storage: Volume and directory management for capsules
//! - OCI: Container image and spec handling
//! - Downloader: File download utilities
//! - Model Fetcher: ML model download and caching

pub mod artifact;
pub mod cas;
pub mod downloader;
pub mod model_fetcher;
#[cfg(target_os = "linux")]
pub mod oci;
#[cfg(not(target_os = "linux"))]
pub mod oci {
    //! OCI module stub for non-Linux platforms
    //! OCI container runtime is only available on Linux.
    
    /// Stub module for spec_builder
    pub mod spec_builder {
        use capsule_core::capsule_v1::{CapsuleExecution, StorageVolume, CapsuleManifestV1};
        use crate::workload::manifest_loader::ResourceRequirements;
        
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
pub mod storage;

// Re-export commonly used types
pub use artifact::ArtifactManager;
pub use cas::{CasClient, HttpCasClient, LocalCasClient};
pub use storage::{StorageConfig, StorageManager};
