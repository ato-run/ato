//! `KataBackend` — skeleton only.
//!
//! Kata is the intended **OCI / container-ecosystem** alignment path (Dockerfile
//! / containerd / Kubernetes-native workloads). It satisfies the
//! [`SnapshotBackend`] contract but is **not** a warm-snapshot backend: its
//! `probe()` reports `supports_seal_before_bind = false`, so the placement gate
//! ([`crate::placement::select_ready_state_backend`]) will never pick it for a
//! Ready-State capsule. This is a deliberate stub; every op fails closed with
//! [`SnapshotError::Unsupported`]. If a real Kata warm-restore path is wanted
//! later, that flag flips with the real implementation.

use capsulefs::CasStore;

use crate::backend::{
    BackendCapabilities, BuildReadyStateInput, BuildReadyStateReceipt, DeviceProfile,
    FilesystemModel, GpuMode, IsolationBoundary, RestoreReadyStateInput, RestoreReceipt,
    RestoredSession, SnapshotBackend, SnapshotError, SnapshotInspection, SnapshotKind,
    TeardownReceipt,
};
use crate::manifest::ReadyStateManifest;

/// Backend id reported by [`KataBackend`].
pub const KATA_BACKEND_ID: &str = "kata";

/// Kata Containers backend (skeleton; OCI-native, not warm-snapshot).
#[derive(Debug, Clone, Default)]
pub struct KataBackend;

impl KataBackend {
    pub fn new() -> Self {
        Self
    }

    fn unsupported(&self) -> SnapshotError {
        SnapshotError::Unsupported {
            backend: KATA_BACKEND_ID.to_string(),
            reason: "KataBackend is a skeleton for OCI/Dockerfile/containerd workloads and is not \
                     a warm-snapshot backend (supports_seal_before_bind = false); it is never \
                     selected for a Ready-State capsule."
                .to_string(),
        }
    }
}

impl SnapshotBackend for KataBackend {
    fn id(&self) -> &str {
        KATA_BACKEND_ID
    }

    fn probe(&self) -> BackendCapabilities {
        BackendCapabilities {
            backend_id: KATA_BACKEND_ID.to_string(),
            available: false,
            reason: Some(
                "KataBackend skeleton: OCI-native, no warm-snapshot path implemented".to_string(),
            ),
            arch: std::env::consts::ARCH.to_string(),
            kvm_present: false,
            vmm_version: None,
            snapshot_kind: SnapshotKind::None,
            memory_snapshot: false,
            filesystem_model: FilesystemModel::Virtiofs,
            device_profile: DeviceProfile::Virtiofs,
            gpu_mode: GpuMode::None,
            oci_native: true,
            isolation_boundary: IsolationBoundary::MicroVm,
            // Deliberately false: Kata is OCI-native, not a seal-before-bind warm
            // snapshot backend. The placement seal gate relies on this.
            supports_seal_before_bind: false,
            supports_disposable_overlay: true,
            // UFFD mem_backend is a Firecracker snapshot feature; Kata is not it.
            supports_uffd_mem_backend: false,
            uffd_reason: Some("kata is not a Firecracker UFFD mem-backend".to_string()),
        }
    }

    fn build_ready_state(
        &self,
        _input: BuildReadyStateInput<'_>,
    ) -> Result<BuildReadyStateReceipt, SnapshotError> {
        Err(self.unsupported())
    }

    fn inspect(
        &self,
        _store: &CasStore,
        _manifest: &ReadyStateManifest,
    ) -> Result<SnapshotInspection, SnapshotError> {
        Err(self.unsupported())
    }

    fn restore(
        &self,
        _input: RestoreReadyStateInput<'_>,
    ) -> Result<RestoreReceipt, SnapshotError> {
        Err(self.unsupported())
    }

    fn stop(&self, _session: RestoredSession) -> Result<TeardownReceipt, SnapshotError> {
        Err(self.unsupported())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_reports_oci_native_skeleton_facets() {
        let p = KataBackend::new().probe();
        assert_eq!(p.backend_id, KATA_BACKEND_ID);
        assert!(!p.available);
        assert!(p.reason.is_some());
        assert!(p.oci_native);
        assert_eq!(p.snapshot_kind, SnapshotKind::None);
    }

    #[test]
    fn probe_does_not_support_seal_before_bind() {
        // So select_ready_state_backend can never pick Kata for a Ready-State capsule.
        assert!(!KataBackend::new().probe().supports_seal_before_bind);
    }

    #[test]
    fn all_ops_unsupported() {
        let b = KataBackend::new();
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let m = ReadyStateManifest {
            schema: crate::manifest::READY_STATE_SCHEMA.to_string(),
            capsule_manifest_hash: "blake3:x".to_string(),
            runner_class_id: None,
            execution_id: None,
            layers: crate::manifest::ReadyStateLayers::default(),
            hotset_profile: Default::default(),
            snapshot_backend: crate::manifest::SnapshotBackendInfo {
                kind: KATA_BACKEND_ID.to_string(),
                version: "0".to_string(),
                snapshot_format_version: "kata-v0".to_string(),
                cpu_template: None,
            },
            restore_contract: Default::default(),
            sanitizer_contract: Default::default(),
            no_secret_proof: None,
            build_receipt_id: None,
        };
        assert!(matches!(
            b.inspect(&store, &m),
            Err(SnapshotError::Unsupported { .. })
        ));
    }
}
