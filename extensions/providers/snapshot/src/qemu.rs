//! `QemuBackend` — skeleton only.
//!
//! QEMU is the intended **filesystem/device-heavy** path (virtio-fs, richer
//! device model, VFIO/GPU-passthrough investigation) — a sibling backend that
//! satisfies the same [`SnapshotBackend`] contract as Firecracker. This is a
//! deliberate stub: [`QemuBackend::probe`] advertises the full-VM capability
//! facets so placement can reason about it, but reports `available = false`
//! (not implemented yet) and every build/restore call fails closed with
//! [`SnapshotError::Unsupported`]. The real implementation lands on a KVM host
//! in a later milestone and slots in behind exactly this trait.

use std::path::Path;

use crate::layer_store::CasStore;
use capsule::snapshot_manifest::SnapshotCompatibilityContractV1;

use crate::backend::{
    BackendCapabilities, BuildReadyStateInput, BuildReadyStateReceipt, DeviceProfile,
    FilesystemModel, GpuMode, IsolationBoundary, RestoreReadyStateInput, RestoreReceipt,
    RestoredSession, SnapshotBackend, SnapshotError, SnapshotInspection, SnapshotKind,
    TeardownReceipt,
};
use crate::manifest::ReadyStateManifest;

/// Backend id reported by [`QemuBackend`].
pub const QEMU_BACKEND_ID: &str = "qemu";

const KVM_DEVICE: &str = "/dev/kvm";

/// QEMU full-VM snapshot backend (skeleton).
#[derive(Debug, Clone, Default)]
pub struct QemuBackend;

impl QemuBackend {
    pub fn new() -> Self {
        Self
    }

    /// Whether `/dev/kvm` is present (QEMU/KVM needs it for the real backend).
    pub fn kvm_present() -> bool {
        Path::new(KVM_DEVICE).exists()
    }

    fn unsupported(&self) -> SnapshotError {
        SnapshotError::Unsupported {
            backend: QEMU_BACKEND_ID.to_string(),
            reason: "QemuBackend is a skeleton: build/restore not implemented yet. It is the \
                     intended filesystem/device-heavy (virtio-fs, VFIO) path and slots in behind \
                     the SnapshotBackend contract on a KVM host."
                .to_string(),
        }
    }
}

impl SnapshotBackend for QemuBackend {
    fn id(&self) -> &str {
        QEMU_BACKEND_ID
    }

    fn probe(&self) -> BackendCapabilities {
        // Advertise what QEMU *would* provide so placement can match
        // filesystem/device-heavy requirements, but stay unavailable while a
        // skeleton (build/restore are Unsupported).
        BackendCapabilities {
            backend_id: QEMU_BACKEND_ID.to_string(),
            available: false,
            reason: Some("QemuBackend skeleton: build/restore not implemented yet".to_string()),
            arch: std::env::consts::ARCH.to_string(),
            kvm_present: Self::kvm_present(),
            vmm_version: None,
            snapshot_kind: SnapshotKind::FullVm,
            memory_snapshot: true,
            filesystem_model: FilesystemModel::Virtiofs,
            device_profile: DeviceProfile::Vfio,
            gpu_mode: GpuMode::None,
            oci_native: false,
            isolation_boundary: IsolationBoundary::Vm,
            supports_seal_before_bind: true,
            supports_disposable_overlay: true,
            // UFFD mem_backend is a Firecracker snapshot feature; QEMU is not it.
            supports_uffd_mem_backend: false,
            uffd_reason: Some("qemu is not a Firecracker UFFD mem-backend".to_string()),
            binding: Default::default(),
        }
    }

    fn snapshot_compatibility_contract(
        &self,
    ) -> Result<SnapshotCompatibilityContractV1, SnapshotError> {
        Err(self.unsupported())
    }

    fn host_runner_class(
        &self,
    ) -> Result<capsule::foundation::install_lifecycle::RunnerClassId, SnapshotError> {
        Err(self.unsupported())
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

    fn restore(&self, _input: RestoreReadyStateInput<'_>) -> Result<RestoreReceipt, SnapshotError> {
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
    fn probe_reports_full_vm_skeleton_facets() {
        let p = QemuBackend::new().probe();
        assert_eq!(p.backend_id, QEMU_BACKEND_ID);
        assert!(!p.available);
        assert!(p.reason.is_some());
        assert_eq!(p.snapshot_kind, SnapshotKind::FullVm);
        assert_eq!(p.filesystem_model, FilesystemModel::Virtiofs);
        assert_eq!(p.device_profile, DeviceProfile::Vfio);
        assert!(p.supports_seal_before_bind);
    }

    #[test]
    fn all_ops_unsupported() {
        let b = QemuBackend::new();
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let m = ReadyStateManifest {
            schema: crate::manifest::READY_STATE_SCHEMA.to_string(),
            capsule_manifest_hash: "blake3:x".to_string(),
            has_vsock: false,
            runner_class_id: None,
            execution_id: None,
            execution_identity_schema: None,
            surface_requirement: None,
            layers: crate::manifest::ReadyStateLayers::default(),
            hotset_profile: Default::default(),
            snapshot_backend: crate::manifest::SnapshotBackendInfo {
                kind: QEMU_BACKEND_ID.to_string(),
                version: "0".to_string(),
                snapshot_format_version: "qemu-v0".to_string(),
                cpu_template: None,
            },
            restore_contract: Default::default(),
            sanitizer_contract: Default::default(),
            no_secret_proof: None,
            build_receipt_id: None,
            supervisor_build: None,
        };
        assert!(matches!(
            b.inspect(&store, &m),
            Err(SnapshotError::Unsupported { .. })
        ));
        assert!(matches!(
            b.snapshot_compatibility_contract(),
            Err(SnapshotError::Unsupported { .. })
        ));
    }
}
