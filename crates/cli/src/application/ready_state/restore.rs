//! Ready-State restore: Restore → (Sanitize) → Bind → Expose → Teardown
//! (F2/F3/F4/F6/F9), driven against the selected backend.
//!
//! The runner-class gate runs inside `backend.restore` (fail-closed, F7). The
//! disposable overlay is created by the backend and destroyed on `teardown`
//! (F4). Host-side sanitizer/bind steps are applied around the restore; under
//! the Fake backend they are no-ops, so the whole flow is exercised end-to-end
//! without a VMM.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use capsule::foundation::install_lifecycle::RunnerClassId;
use capsulefs::CasStore;
use snapshot::{
    ReadyStateManifest, RestoreReadyStateInput, RestoreReceipt, RestoredSession, SnapshotBackend,
    SnapshotCatalogRecord, SnapshotManifestV1, SnapshotRestoreCapabilities, TeardownReceipt,
    select_compatible_snapshot,
};

/// Restore a sealed artifact and prepare its session for exposure.
pub(crate) fn restore_and_expose(
    backend: &dyn SnapshotBackend,
    store: &CasStore,
    manifest: ReadyStateManifest,
    v1_manifest: Option<SnapshotManifestV1>,
    overlay_root: PathBuf,
    host_runner_class: Option<RunnerClassId>,
    uffd_preview: bool,
) -> Result<RestoreReceipt> {
    if let Some(v1_manifest) = v1_manifest.as_ref() {
        verify_v1_candidate(backend, &manifest, v1_manifest)?;
    }
    let receipt = backend
        .restore(RestoreReadyStateInput {
            store,
            manifest,
            overlay_root,
            host_runner_class,
            uffd_preview,
        })
        .context("snapshot backend restore failed")?;
    Ok(receipt)
}

fn verify_v1_candidate(
    backend: &dyn SnapshotBackend,
    legacy: &ReadyStateManifest,
    candidate: &SnapshotManifestV1,
) -> Result<()> {
    if legacy.execution_id.as_deref() != Some(candidate.execution_id.as_str()) {
        anyhow::bail!("legacy/v1 Snapshot execution_id mismatch");
    }
    if legacy.restore_contract != candidate.restore_contract {
        anyhow::bail!("legacy/v1 Snapshot restore contract mismatch");
    }
    if legacy.layers.memory.as_ref().map(|layer| layer.id())
        != candidate.layers.memory.as_ref().map(|layer| layer.id())
        || legacy.layers.vmstate.as_ref().map(|layer| layer.id())
            != candidate.layers.vmstate.as_ref().map(|layer| layer.id())
    {
        anyhow::bail!("legacy/v1 Snapshot memory or VM-state layer mismatch");
    }
    let legacy_disk_layers: BTreeSet<_> = [
        &legacy.layers.rootfs,
        &legacy.layers.runtime,
        &legacy.layers.dependency,
        &legacy.layers.app,
    ]
    .into_iter()
    .flatten()
    .map(|layer| layer.id())
    .collect();
    let v1_disk_layers: BTreeSet<_> = candidate
        .layers
        .disk_layers
        .iter()
        .map(|layer| layer.id())
        .collect();
    if legacy_disk_layers != v1_disk_layers {
        anyhow::bail!("legacy/v1 Snapshot disk layer mismatch");
    }

    let compatibility = backend
        .snapshot_compatibility_contract()
        .context("resolve restore host Snapshot compatibility")?;
    let capabilities = SnapshotRestoreCapabilities::exact(&compatibility);
    let records = [SnapshotCatalogRecord::accepted(candidate.clone())];
    if select_compatible_snapshot(&candidate.execution_id, &capabilities, &records)
        .context("validate Snapshot v1 candidate")?
        .is_none()
    {
        anyhow::bail!("no exact compatible Snapshot for the requested execution_id");
    }
    Ok(())
}

/// Tear down a restored session: stop the VM and destroy its disposable overlay.
/// Long-lived serving (Firecracker) registers the session instead and leaves it
/// running for a later `ato stop`; this is the verify-only teardown taken by
/// backends with no serving process (Fake / KVM-free) and by the engine smoke
/// tests / future foreground-serve SIGINT hook.
pub(crate) fn teardown(
    backend: &dyn SnapshotBackend,
    session: RestoredSession,
) -> Result<TeardownReceipt> {
    backend
        .stop(session)
        .context("snapshot backend stop failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule::execution_contract::ExecutionId;
    use snapshot::{
        BuildLayers, BuildReadyStateInput, FakeSnapshotBackend, RestoreContract, SanitizerContract,
        migrate_legacy_manifest,
    };

    #[test]
    fn restore_then_teardown_destroys_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let backend = FakeSnapshotBackend::new();
        let execution_id = ExecutionId::new(format!("blake3:{}", "a".repeat(64))).unwrap();
        let manifest = backend
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
                    vmstate: vec![1u8; 256],
                    memory: (0..100_000u32).map(|i| (i % 256) as u8).collect(),
                },
                restore_contract: RestoreContract {
                    ports: vec![8080],
                    ..Default::default()
                },
                sanitizer_contract: SanitizerContract::default(),
                declared_secret_markers: vec![],
                execution_id: Some(execution_id.to_string()),
                supervisor: None,
            })
            .unwrap()
            .manifest;
        let v1_manifest = migrate_legacy_manifest(
            &manifest,
            execution_id,
            backend.snapshot_compatibility_contract().unwrap(),
        )
        .unwrap();

        let overlay = dir.path().join("ov");
        let receipt = restore_and_expose(
            &backend,
            &store,
            manifest,
            Some(v1_manifest),
            overlay.clone(),
            None,
            false,
        )
        .unwrap();
        assert_eq!(receipt.session.guest_port, Some(8080));
        assert!(overlay.exists());

        let td = teardown(&backend, receipt.session).unwrap();
        assert!(td.overlay_removed);
        assert!(!overlay.exists());
    }
}
