//! Snapshot — backend-agnostic build & restore of Ready-State Capsule warm
//! state.
//!
//! This crate defines the [`SnapshotBackend`] seam and the Ready-State artifact
//! types ([`ReadyStateManifest`] and friends), and ships two backends:
//!
//! * [`FakeSnapshotBackend`] — KVM-free; drives the full
//!   build→seal→restore→teardown pipeline through CapsuleFS so the Ready-State
//!   flow is end-to-end testable on a host without `/dev/kvm` (e.g. OCI A1).
//! * [`FirecrackerBackend`] — a skeleton that reports `Unsupported` until it
//!   runs on a KVM-capable host; the real VMM implementation slots in behind
//!   the same trait without changing callers.
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

mod backend;
mod fake;
mod firecracker;
mod manifest;

pub use backend::{
    BackendCapabilities, BuildLayers, BuildReadyStateInput, BuildReadyStateReceipt,
    RestoreReadyStateInput, RestoreReceipt, RestoredSession, SnapshotBackend, SnapshotError,
    SnapshotInspection, TeardownReceipt,
};
pub use fake::{FAKE_BACKEND_ID, FakeSnapshotBackend};
pub use firecracker::{FIRECRACKER_BACKEND_ID, FirecrackerBackend};
pub use manifest::{
    NoSecretProof, READY_STATE_SCHEMA, ReadyStateLayers, ReadyStateManifest, RestoreContract,
    SanitizerContract, SanitizerLayer, SanitizerStep, SnapshotBackendInfo,
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
            })
            .expect("build");
        let manifest = receipt.manifest;

        let restore = backend
            .restore(RestoreReadyStateInput {
                store: &store,
                manifest: manifest.clone(),
                overlay_root: dir.path().join("ov"),
                host_runner_class: None,
            })
            .expect("restore");
        assert_eq!(restore.session.restored_bytes, manifest.total_layer_bytes());
        assert_eq!(restore.session.guest_port, Some(3000));

        let teardown = backend.stop(restore.session).unwrap();
        assert!(teardown.overlay_removed);
    }
}
