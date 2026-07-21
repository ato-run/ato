//! Snapshot — backend-agnostic build & restore of Ready-State Capsule warm
//! state.
//!
//! This crate defines the [`SnapshotBackend`] seam and the Ready-State artifact
//! types ([`ReadyStateManifest`] and friends), and ships these backends:
//!
//! * [`FirecrackerBackend`] — the real x86_64 KVM implementation (M0 GO,
//!   2026-06-29): drives Firecracker over its REST API to build and restore
//!   Ready-State microVM snapshots. See `firecracker.rs` for scope.
//! * [`FakeSnapshotBackend`] — KVM-free; drives the full
//!   build→seal→restore→teardown pipeline through CapsuleFS so the Ready-State
//!   flow is end-to-end testable on a host without `/dev/kvm` (e.g. OCI A1).
//! * [`KataBackend`] / QEMU — deliberate stubs reserving the OCI-alignment and
//!   virtio-fs/GPU paths.
//!
//! ## Design (plan §4)
//!
//! Snapshot/restore is a **separate trait**, never grafted onto `capsule`'s
//! `RuntimeHandle`. A restore *produces* a [`RestoredSession`] — the data a
//! later adapter (M6) turns into a `RuntimeHandle` (adding the
//! `RuntimeMetadata::MicroVm` variant along the way). The trait is additive: a
//! legacy cold `ato run` never touches it.
//!
//! ## Security invariants (plan §8)
//!
//! * **Seal before bind** — `build_ready_state` captures layers with no secret
//!   present and runs a no-secret gate over every sealed layer (rootfs/runtime/
//!   deps/app/vmstate/memory), failing closed on any finding.
//! * **Fail-closed restore class** — `restore` rejects a host whose
//!   `runner_class_id` differs from the one the snapshot was built for.

mod acceptance;
pub mod agent_channel;
mod backend;
pub mod bench;
pub mod compose_plan;
pub mod docker_import;
mod external_state;
mod fake;
mod firecracker;
mod kata;
mod manifest;
pub mod mem_backend_selector;
pub mod no_secret_scan;
mod placement;
mod qemu;
pub mod rootfs_builder;
mod scan_cache;
mod scanner;
mod seal;
mod snapshot_manifest;
pub mod state_volume;
pub mod state_volume_persistence;
mod uffd;
mod workload_idle;
// U1 (#854) spike: plumbing exercised only by #[ignore]d KVM-gated smokes
// (ATO_FC_UFFD), not yet wired into the default restore path — see the
// module doc for scope. Real, tested, deliberately unused until U2+.
#[allow(dead_code)]
mod uffd_page_server;

pub use acceptance::{
    AcceptanceAttemptReceipt, AcceptanceCancellation, AcceptanceConfig, AcceptanceError,
    AcceptanceReceipt, AcceptanceResult, CandidateSnapshot, DisposableAcceptanceLifecycle,
    DisposableSessionHandle, RunningSnapshotAcceptance, SnapshotEligibility, VerificationOutcome,
};
pub use backend::{
    BackendCapabilities, BindingCapabilities, BuildLayers, BuildReadyStateInput,
    BuildReadyStateReceipt, DeviceProfile, FilesystemModel, GpuMode, IsolationBoundary,
    RestoreReadyStateInput, RestoreReceipt, RestoredSession, SnapshotBackend, SnapshotError,
    SnapshotInspection, SnapshotKind, SupervisorBindings, TeardownReceipt,
    ensure_gpu_not_in_snapshot,
};
pub use compose_plan::{
    DependencyKind, Healthcheck, ImportedService, ImportedServiceGraph, MountKind, RestartPolicy,
    ServiceDependency, ServiceMount, compose_to_graph,
};
pub use external_state::{
    ExternalStateAttachRequest, ExternalStateAttachmentPlan, ExternalStateBoundaryError,
    ExternalStateInstance, OpaqueStateGeneration, OpaqueStateRef, SessionExternalStateReceipt,
    ensure_external_state_layers_excluded, plan_external_state_attach,
};
pub use fake::{FAKE_BACKEND_ID, FakeSnapshotBackend};
pub use firecracker::{FIRECRACKER_BACKEND_ID, FirecrackerBackend, FirecrackerConfig};
pub use kata::{KATA_BACKEND_ID, KataBackend};
pub use manifest::{
    LayerScanCoverage, NoSecretProof, READY_STATE_SCHEMA, ReadyStateLayers, ReadyStateManifest,
    RestoreContract, SanitizerContract, SanitizerLayer, SanitizerStep, SnapshotBackendInfo,
    SupervisorBuildReceipt, WarmupRecipe,
};
pub use placement::{
    BackendRequirements, PlacementError, matches, ready_state_safe, select_ready_state_backend,
};
pub use qemu::{QEMU_BACKEND_ID, QemuBackend};
pub use scanner::{
    FindingKind, POLICY_VERSION, SCANNER_VERSION, ScanReport, SecretFinding, policy_fingerprint,
    scan_build_layers,
};
pub use snapshot_manifest::{
    CapturePolicy, CaptureProvenance, SNAPSHOT_MANIFEST_V1_SCHEMA, SanitizationAttestation,
    SecretScanAttestation, SnapshotCatalogRecord, SnapshotCatalogStatus,
    SnapshotCompatibilityContract, SnapshotLayerRefs, SnapshotManifestError, SnapshotManifestV1,
    SnapshotRestoreCapabilities, migrate_legacy_manifest, select_compatible_snapshot,
};
pub use workload_idle::{
    ExposedWorkloadIdleSession, SyntheticBindingSpec, WorkloadIdleAcceptanceReceipt,
    WorkloadIdleConfig, WorkloadIdleError, WorkloadIdleLifecycle, WorkloadIdleRunReceipt,
    accept_workload_idle_snapshot, restore_workload_idle_session,
};

#[cfg(test)]
mod e2e_tests {
    //! Cross-backend end-to-end: select a backend by probe, then build→restore.
    use super::*;
    use capsulefs::CasStore;

    /// Mirror the runner-capability selection: prefer Firecracker when KVM is
    /// present, else fall back to the Fake backend. On the A1 box (no KVM) this
    /// always selects Fake and the whole pipeline still runs.
    fn select_backend() -> Box<dyn SnapshotBackend> {
        let fc = FirecrackerBackend::new();
        if fc.probe().available {
            Box::new(fc)
        } else {
            Box::new(FakeSnapshotBackend::new())
        }
    }

    #[test]
    fn selected_backend_runs_build_and_restore() {
        let backend = select_backend();
        // On a KVM-less host the selected backend is the Fake one and the
        // pipeline completes; on a KVM host Firecracker is selected but is still
        // a skeleton (Unsupported), so only exercise the pipeline when the
        // backend can actually build.
        if !backend.probe().available {
            return;
        }
        // The Fake backend reports available; the Firecracker skeleton does only
        // when KVM is present, where build is still Unsupported — guard on that.
        if backend.id() == FIRECRACKER_BACKEND_ID {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let receipt = backend
            .build_ready_state(BuildReadyStateInput {
                store: &store,
                capsule_manifest_hash: "blake3:capsule".to_string(),
                runner_class: None,
                surface_requirement: None,
                layers: BuildLayers {
                    rootfs: b"rootfs".to_vec(),
                    runtime: None,
                    dependency: None,
                    app: Some(b"app".to_vec()),
                    vmstate: vec![1u8; 2048],
                    memory: vec![2u8; 250_000],
                },
                restore_contract: RestoreContract {
                    ports: vec![3000],
                    ..Default::default()
                },
                sanitizer_contract: SanitizerContract::default(),
                declared_secret_markers: vec![],
                execution_id: None,
                supervisor: None,
            })
            .expect("build");
        let manifest = receipt.manifest;

        let restore = backend
            .restore(RestoreReadyStateInput {
                store: &store,
                manifest: manifest.clone(),
                overlay_root: dir.path().join("ov"),
                host_runner_class: None,
                uffd_preview: false,
            })
            .expect("restore");
        assert_eq!(restore.session.restored_bytes, manifest.total_layer_bytes());
        assert_eq!(restore.session.guest_port, Some(3000));

        let teardown = backend.stop(restore.session).unwrap();
        assert!(teardown.overlay_removed);
    }
}
