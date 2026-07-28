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

pub mod acceptance;
pub mod agent_channel;
/// Building from a Source Revision that was already materialized, and from
/// nothing else — the input type carries no repository coordinate, so a clone
/// or a branch re-resolution has nothing to work from.
pub mod archive_only_build;
pub mod artifact_envelope;
pub mod authoring_evidence;
mod backend;
pub mod bench;
/// The v1 producer lane: `ato build`'s `schema_version = "1"` path, which
/// builds a guest, measures it, mints an Execution Identity from those
/// measurements, and publishes it into `capsule.lock` (ADR-015 step 5-3).
///
/// It lives here rather than in the CLI because it composes THIS crate's
/// [`rootfs_builder`], [`docker_import`], [`guest_filesystem_digest`] and
/// [`v1_materialization`] and has no CLI-specific input — and because the
/// snapshot builder daemon runs the same lane for a pinned wizard build. Two
/// copies of a lane that mints Execution Identity would be two identities.
pub mod build_v1;
pub mod compose_plan;
#[cfg(test)]
mod contract_fixtures;
pub mod disposable_lifecycle;
pub mod docker_import;
pub mod external_state;
mod fake;
mod firecracker;
/// What a guest filesystem CONTAINS, as one digest — the value
/// `filesystem.view_digest` commits. Content rather than the ext4
/// serialization, because `mke2fs` stamps every inode with the wall clock.
pub mod guest_filesystem_digest;
mod kata;
mod manifest;
pub mod mem_backend_selector;
pub mod no_secret_scan;
/// Collecting the measurements a v1 Execution Contract is minted from
/// (ADR-015 step 4). Measuring lives here because the contract layer performs
/// no host I/O by design.
pub mod observe_v1;
mod placement;
mod qemu;
pub mod rootfs_builder;
mod scan_cache;
mod scanner;
/// Whether a checked-out source is one the v1 submission lane can carry —
/// read from the COMMIT tree, because an uninitialised submodule is invisible
/// on the filesystem.
pub mod source_eligibility;
/// Turning a pinned commit into a frozen, re-fetchable source archive, with the
/// round trip verified before anything is stored.
pub mod source_materialization;
/// The two receipts a source materialization produces — WHAT was resolved, and
/// WHERE it was stored. Their canonical bytes are pinned by shared fixtures
/// because the verifier is TypeScript in another repository.
pub mod source_receipt;
// Build-time screenshot capture (store thumbnail automation) — internal to
// `firecracker.rs`'s `build_ready_state`, no public API of its own.
mod screenshot;
mod seal;
pub mod state_volume;
pub mod state_volume_persistence;
mod uffd;
// U1 (#854) spike: plumbing exercised only by #[ignore]d KVM-gated smokes
// (ATO_FC_UFFD), not yet wired into the default restore path — see the
// module doc for scope. Real, tested, deliberately unused until U2+.
#[allow(dead_code)]
mod uffd_page_server;
pub mod v1_materialization;

pub use artifact_envelope::{
    ARTIFACT_ENVELOPE_V1_FILENAME, ARTIFACT_ENVELOPE_V1_SCHEMA, ArtifactAcceptance,
    ArtifactAcceptanceStatus, ArtifactEnvelopeError, ArtifactEnvelopeV1,
    SNAPSHOT_MANIFEST_V1_FILENAME,
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
pub use fake::{FAKE_BACKEND_ID, FakeSnapshotBackend};
/// TEST-ONLY (`test-support`): see the function's own doc.
#[cfg(any(test, feature = "test-support"))]
pub use firecracker::vsock_uds_path_for_capsule as firecracker_vsock_uds_path_for_capsule;
pub use firecracker::{
    FIRECRACKER_BACKEND_ID, FirecrackerBackend, FirecrackerConfig, HeldCandidate,
    HeldCaptureFailure, HeldGuest,
};
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
