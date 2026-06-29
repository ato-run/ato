//! Ready-State restore: Restore → (Sanitize) → Bind → Expose → Teardown
//! (F2/F3/F4/F6/F9), driven against the selected backend.
//!
//! The runner-class gate runs inside `backend.restore` (fail-closed, F7). The
//! disposable overlay is created by the backend and destroyed on `teardown`
//! (F4). Host-side sanitizer/bind steps are applied around the restore; under
//! the Fake backend they are no-ops, so the whole flow is exercised end-to-end
//! without a VMM.

use std::path::PathBuf;

use anyhow::{Context, Result};
use capsule::foundation::install_lifecycle::RunnerClassId;
use capsulefs::CasStore;
use snapshot::{
    ReadyStateManifest, RestoreReadyStateInput, RestoreReceipt, RestoredSession, SnapshotBackend,
    TeardownReceipt,
};

/// Restore a sealed artifact and prepare its session for exposure.
pub(crate) fn restore_and_expose(
    backend: &dyn SnapshotBackend,
    store: &CasStore,
    manifest: ReadyStateManifest,
    overlay_root: PathBuf,
    host_runner_class: Option<RunnerClassId>,
) -> Result<RestoreReceipt> {
    let receipt = backend
        .restore(RestoreReadyStateInput {
            store,
            manifest,
            overlay_root,
            host_runner_class,
        })
        .context("snapshot backend restore failed")?;
    Ok(receipt)
}

/// Tear down a restored session: stop the VM and destroy its disposable overlay.
pub(crate) fn teardown(
    backend: &dyn SnapshotBackend,
    session: RestoredSession,
) -> Result<TeardownReceipt> {
    backend.stop(session).context("snapshot backend stop failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use snapshot::{
        BuildLayers, BuildReadyStateInput, FakeSnapshotBackend, RestoreContract, SanitizerContract,
    };

    #[test]
    fn restore_then_teardown_destroys_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let backend = FakeSnapshotBackend::new();
        let manifest = backend
            .build_ready_state(BuildReadyStateInput {
                store: &store,
                capsule_manifest_hash: "blake3:capsule".to_string(),
                runner_class: None,
                layers: BuildLayers {
                    rootfs: b"rootfs".to_vec(),
                    runtime: None,
                    dependency: None,
                    app: Some(b"app".to_vec()),
                    vmstate: vec![1u8; 256],
                    memory: (0..100_000u32).map(|i| (i % 256) as u8).collect(),
                },
                restore_contract: RestoreContract { ports: vec![8080], ..Default::default() },
                sanitizer_contract: SanitizerContract::default(),
                declared_secret_markers: vec![],
            })
            .unwrap()
            .manifest;

        let overlay = dir.path().join("ov");
        let receipt = restore_and_expose(&backend, &store, manifest, overlay.clone(), None).unwrap();
        assert_eq!(receipt.session.guest_port, Some(8080));
        assert!(overlay.exists());

        let td = teardown(&backend, receipt.session).unwrap();
        assert!(td.overlay_removed);
        assert!(!overlay.exists());
    }
}
