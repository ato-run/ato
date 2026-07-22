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
    ArtifactEnvelopeV1, ReadyStateManifest, RestoreReadyStateInput, RestoreReceipt,
    RestoredSession, SnapshotBackend, SnapshotCatalogRecord, SnapshotCompatibilityContract,
    SnapshotManifestV1, SnapshotRestoreCapabilities, TeardownReceipt, migrate_legacy_manifest,
    select_compatible_snapshot,
};

pub(crate) enum RestoreVerification {
    LegacyLocal,
    RunnerLease {
        expected_execution_id: String,
    },
    V1 {
        manifest: Box<SnapshotManifestV1>,
        envelope: Box<ArtifactEnvelopeV1>,
    },
}

/// Restore a sealed artifact and prepare its session for exposure.
pub(crate) fn restore_and_expose(
    backend: &dyn SnapshotBackend,
    store: &CasStore,
    manifest: ReadyStateManifest,
    verification: RestoreVerification,
    overlay_root: PathBuf,
    host_runner_class: Option<RunnerClassId>,
    uffd_preview: bool,
) -> Result<RestoreReceipt> {
    match &verification {
        RestoreVerification::LegacyLocal => {}
        RestoreVerification::RunnerLease {
            expected_execution_id,
        } => {
            let compatibility = backend
                .snapshot_compatibility_contract()
                .context("resolve runner restore Snapshot compatibility")?;
            verify_runner_lease_candidate(&manifest, expected_execution_id, &compatibility)?;
        }
        RestoreVerification::V1 {
            manifest: v1_manifest,
            envelope,
        } => {
            verify_v1_candidate(backend, &manifest, v1_manifest, envelope)?;
        }
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
    envelope: &ArtifactEnvelopeV1,
) -> Result<()> {
    envelope
        .verify(legacy, candidate)
        .context("verify Snapshot Artifact Envelope")?;
    if legacy.execution_id.as_deref() != Some(candidate.execution_id.as_str()) {
        anyhow::bail!("legacy/v1 Snapshot execution_id mismatch");
    }
    let migrated = migrate_legacy_manifest(
        legacy,
        candidate.execution_id.clone(),
        candidate.compatibility.clone(),
    )
    .context("derive Snapshot v1 layer projection from legacy manifest")?;
    if migrated.restore_contract != candidate.restore_contract {
        anyhow::bail!("legacy/v1 Snapshot restore contract mismatch");
    }
    if migrated.layers != candidate.layers {
        anyhow::bail!("legacy/v1 Snapshot layer mismatch");
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

fn verify_runner_lease_candidate(
    legacy: &ReadyStateManifest,
    expected_execution_id: &str,
    host: &SnapshotCompatibilityContract,
) -> Result<()> {
    if legacy.execution_id.as_deref() != Some(expected_execution_id) {
        anyhow::bail!("runner lease/manifest execution_id mismatch");
    }
    let backend = &legacy.snapshot_backend;
    if backend.kind != host.backend
        || backend.version != host.vmm_version
        || backend.snapshot_format_version != host.format
        || backend.cpu_template != host.cpu_template
    {
        anyhow::bail!("runner lease Snapshot backend compatibility mismatch");
    }
    if legacy.runner_class_id.as_ref().map(RunnerClassId::as_str)
        != Some(host.runner_contract.as_str())
    {
        anyhow::bail!("runner lease Snapshot runner contract mismatch");
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

    fn runner_lease_manifest() -> (ReadyStateManifest, SnapshotCompatibilityContract) {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let backend = FakeSnapshotBackend::new();
        let compatibility = backend.snapshot_compatibility_contract().unwrap();
        let mut manifest = backend
            .build_ready_state(BuildReadyStateInput {
                store: &store,
                capsule_manifest_hash: "blake3:capsule".to_string(),
                runner_class: Some(RunnerClassId::from_hash(
                    compatibility.runner_contract.clone(),
                )),
                surface_requirement: None,
                layers: BuildLayers {
                    rootfs: b"rootfs".to_vec(),
                    runtime: None,
                    dependency: None,
                    app: None,
                    vmstate: vec![1; 16],
                    memory: vec![2; 16],
                },
                restore_contract: RestoreContract::default(),
                sanitizer_contract: SanitizerContract::default(),
                declared_secret_markers: Vec::new(),
                execution_id: Some("sha256:legacy-execution".to_string()),
                execution_identity_schema: None,
                supervisor: None,
            })
            .unwrap()
            .manifest;
        manifest.snapshot_backend.kind = compatibility.backend.clone();
        manifest.snapshot_backend.version = compatibility.vmm_version.clone();
        manifest.snapshot_backend.snapshot_format_version = compatibility.format.clone();
        manifest.snapshot_backend.cpu_template = compatibility.cpu_template.clone();
        (manifest, compatibility)
    }

    #[test]
    fn runner_lease_gate_accepts_exact_execution_and_compatibility() {
        let (manifest, compatibility) = runner_lease_manifest();

        verify_runner_lease_candidate(&manifest, "sha256:legacy-execution", &compatibility)
            .unwrap();
    }

    #[test]
    fn runner_lease_gate_rejects_host_compatibility_drift() {
        let (manifest, mut compatibility) = runner_lease_manifest();
        compatibility.kernel_digest = "sha256:different-kernel".to_string();
        compatibility.runner_contract = "blake3:different-runner".to_string();

        let error =
            verify_runner_lease_candidate(&manifest, "sha256:legacy-execution", &compatibility)
                .unwrap_err();

        assert!(error.to_string().contains("runner contract"), "{error}");
    }

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
                execution_identity_schema: Some(
                    capsule::execution_contract::EXECUTION_CONTRACT_V1_SCHEMA.to_string(),
                ),
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
        let envelope = snapshot::ArtifactEnvelopeV1::accepted(&manifest, &v1_manifest).unwrap();

        let overlay = dir.path().join("ov");
        let receipt = restore_and_expose(
            &backend,
            &store,
            manifest,
            RestoreVerification::V1 {
                manifest: Box::new(v1_manifest),
                envelope: Box::new(envelope),
            },
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
