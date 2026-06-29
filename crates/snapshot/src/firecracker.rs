//! `FirecrackerBackend` — skeleton only.
//!
//! Firecracker requires `/dev/kvm`, which is absent on the OCI A1 box this work
//! started on. So this is a deliberate stub: [`FirecrackerBackend::probe`]
//! detects the absence of `/dev/kvm` and reports the backend unavailable, and
//! every build/restore call fails closed with [`SnapshotError::Unsupported`].
//! The real implementation (jailer process management, API socket client,
//! TAP/vsock setup, CreateSnapshot/LoadSnapshot, CPU template selection) lands
//! on a KVM-capable host in a later milestone (plan §6 spike, M3/M6) and slots
//! in behind exactly this trait — callers do not change.

use std::path::Path;

use capsulefs::CasStore;

use crate::backend::{
    BackendCapabilities, BuildReadyStateInput, BuildReadyStateReceipt, RestoreReadyStateInput,
    RestoreReceipt, RestoredSession, SnapshotBackend, SnapshotError, SnapshotInspection,
    TeardownReceipt,
};
use crate::manifest::ReadyStateManifest;

/// Backend id reported by [`FirecrackerBackend`].
pub const FIRECRACKER_BACKEND_ID: &str = "firecracker";

/// Path probed to decide KVM availability.
const KVM_DEVICE: &str = "/dev/kvm";

/// Firecracker microVM snapshot backend (skeleton).
#[derive(Debug, Clone, Default)]
pub struct FirecrackerBackend;

impl FirecrackerBackend {
    pub fn new() -> Self {
        Self
    }

    /// Whether `/dev/kvm` is present on this host.
    pub fn kvm_present() -> bool {
        Path::new(KVM_DEVICE).exists()
    }

    fn unsupported(&self) -> SnapshotError {
        SnapshotError::Unsupported {
            backend: FIRECRACKER_BACKEND_ID.to_string(),
            reason: format!(
                "{KVM_DEVICE} not present; Firecracker needs KVM. Build/restore must run on a \
                 KVM-capable host (e.g. an OCI bare-metal shape), not a KVM-less VM."
            ),
        }
    }
}

impl SnapshotBackend for FirecrackerBackend {
    fn id(&self) -> &str {
        FIRECRACKER_BACKEND_ID
    }

    fn probe(&self) -> BackendCapabilities {
        let kvm_present = Self::kvm_present();
        // `available` means "this backend can build/restore on this host right
        // now". This is a skeleton: every build/restore call returns
        // `Unsupported`, so it is NEVER available yet — not even on a
        // KVM-capable host. Reporting `available: true` here would let a runner
        // advertise a Ready-State capability it cannot honor. Until the real
        // VMM implementation lands, fail closed. `kvm_present` is still reported
        // truthfully so callers/diagnostics can see why.
        let reason = if !kvm_present {
            format!("{KVM_DEVICE} not present; Firecracker needs KVM")
        } else {
            "FirecrackerBackend is a skeleton: build/restore not implemented yet".to_string()
        };
        BackendCapabilities {
            backend_id: FIRECRACKER_BACKEND_ID.to_string(),
            available: false,
            reason: Some(reason),
            arch: std::env::consts::ARCH.to_string(),
            kvm_present,
            // Version detection (running `firecracker --version`) is part of the
            // real implementation; unknown in the skeleton.
            vmm_version: None,
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
    fn probe_is_never_available_while_skeleton() {
        let p = FirecrackerBackend::new().probe();
        assert_eq!(p.backend_id, FIRECRACKER_BACKEND_ID);
        // The skeleton can never build/restore, so it must NEVER advertise
        // availability — not even where /dev/kvm is present. It always carries a
        // reason, and reports kvm_present truthfully for diagnostics.
        assert!(!p.available, "skeleton must not advertise availability");
        assert!(p.reason.is_some());
        assert_eq!(p.kvm_present, FirecrackerBackend::kvm_present());
    }

    #[test]
    fn build_is_unsupported_without_kvm() {
        if FirecrackerBackend::kvm_present() {
            // On a real KVM host this skeleton would still be Unsupported (not
            // implemented yet), so the assertion holds either way — but only
            // assert the KVM-less expectation we actually run on.
            return;
        }
        let backend = FirecrackerBackend::new();
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let manifest = err_manifest();
        assert!(matches!(
            backend.inspect(&store, &manifest),
            Err(SnapshotError::Unsupported { .. })
        ));
    }

    fn err_manifest() -> ReadyStateManifest {
        use crate::manifest::{
            ReadyStateLayers, RestoreContract, SanitizerContract, SnapshotBackendInfo,
            READY_STATE_SCHEMA,
        };
        ReadyStateManifest {
            schema: READY_STATE_SCHEMA.to_string(),
            capsule_manifest_hash: "blake3:x".to_string(),
            runner_class_id: None,
            execution_id: None,
            layers: ReadyStateLayers::default(),
            hotset_profile: Default::default(),
            snapshot_backend: SnapshotBackendInfo {
                kind: FIRECRACKER_BACKEND_ID.to_string(),
                version: "0".to_string(),
                snapshot_format_version: "fc-v2".to_string(),
                cpu_template: None,
            },
            restore_contract: RestoreContract::default(),
            sanitizer_contract: SanitizerContract::default(),
            no_secret_proof: None,
            build_receipt_id: None,
        }
    }
}
