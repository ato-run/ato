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
use capsule::snapshot_manifest::{
    CapturePolicyV1, HostRestoreCapabilityV1, SnapshotBackendKind, SnapshotCompatibilityContractV1,
    SnapshotManifestV1,
};
use capsulefs::CasStore;
use snapshot::{
    ArtifactEnvelopeV1, ReadyStateManifest, RestoreReadyStateInput, RestoreReceipt,
    RestoredSession, SnapshotBackend, TeardownReceipt,
};

/// How the restore path authenticates the sealed artifact before restoring it.
pub(crate) enum RestoreVerification {
    /// `ato build` → `ato run` on the SAME host: the artifact was just sealed
    /// locally by this same operator, so there is no cross-host lease to verify
    /// against. Restore proceeds straight to `backend.restore` (which still
    /// enforces its own fail-closed runner-class gate).
    LegacyLocal,
    /// A runner-dispatched restore whose lease carries only the LEGACY opaque
    /// `execution_id` (no Capsule v1 sidecar) — verified against this host's
    /// real backend facts.
    RunnerLease { expected_execution_id: String },
    /// A runner-dispatched restore of an explicit Capsule v1 Snapshot: the
    /// candidate manifest + its authenticated Artifact Envelope, verified
    /// against the legacy artifact and this host's real backend facts.
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

/// Build a [`HostRestoreCapabilityV1`] that presents EXACTLY the facts of
/// `contract` — i.e. "a host that is precisely this backend's own declared
/// compatibility contract, right now". Used to prove a v1 candidate is
/// restorable on THIS host by checking it against the backend's own current
/// facts, without a separately-invented host-capability prober. `pub(crate)`
/// so both this module's own gate and `mod::decide_ready_state_run`'s
/// candidate-selection gate share the ONE derivation rule.
pub(crate) fn exact_host_capability(
    contract: &SnapshotCompatibilityContractV1,
) -> HostRestoreCapabilityV1 {
    HostRestoreCapabilityV1 {
        backend: contract.backend,
        supported_format_versions: vec![contract.format_version],
        vmm_identity: contract.vmm_identity.clone(),
        state_codec: contract.state_codec.clone(),
        guest_kernel_identity: contract.guest_kernel_identity.clone(),
        cpu_templates: vec![contract.cpu_template.clone()],
        runner_restore_contract: contract.runner_restore_contract.clone(),
        compatibility_class_identity: Some(contract.compatibility_class_identity),
        // Both v1 capture policies are accepted here: which one a given
        // candidate actually used is enforced by `is_satisfied_by` matching
        // `candidate.capture_policy` against this list, not by this helper.
        supported_capture_policies: vec![CapturePolicyV1::Running, CapturePolicyV1::WorkloadIdle],
    }
}

/// Verify an explicit Capsule v1 restore candidate: the Artifact Envelope
/// authentically binds `candidate` to `legacy` (proving `candidate`'s entire
/// content — including its `restore_contract` and layer refs, which are part
/// of its own content-addressed `snapshot_id` — is exactly what was accepted
/// for `legacy`'s bytes, so no separate re-derivation/diff is needed here),
/// the two manifest generations agree on execution identity, and this host's
/// backend can actually prove it satisfies the candidate's compatibility
/// contract.
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
    let host_contract = backend
        .snapshot_compatibility_contract()
        .context("resolve restore host Snapshot compatibility")?;
    if !candidate
        .compatibility_contract
        .is_satisfied_by(&exact_host_capability(&host_contract))
    {
        anyhow::bail!("no exact compatible Snapshot for the requested execution_id");
    }
    Ok(())
}

/// Verify a legacy-only runner-lease restore: the manifest's opaque
/// `execution_id` matches what the lease expects, and the sealed backend
/// facts agree with this host's real, current compatibility facts.
///
/// `snapshot_format_version` (a descriptive legacy string, e.g. `"fc-v2"`) has
/// no commensurable Gate-0 v1 counterpart (`format_version` is now a `u32`
/// generation number) — compatibility here is otherwise fully pinned by
/// `vmm_identity` + `cpu_template` + the runner-restore-contract check below,
/// so the format-version dimension is intentionally not compared.
fn verify_runner_lease_candidate(
    legacy: &ReadyStateManifest,
    expected_execution_id: &str,
    host: &SnapshotCompatibilityContractV1,
) -> Result<()> {
    if legacy.execution_id.as_deref() != Some(expected_execution_id) {
        anyhow::bail!("runner lease/manifest execution_id mismatch");
    }
    let backend_kind = match host.backend {
        SnapshotBackendKind::Firecracker => "firecracker",
        SnapshotBackendKind::Cloud => "cloud",
        SnapshotBackendKind::Qemu => "qemu",
        SnapshotBackendKind::Kata => "kata",
        SnapshotBackendKind::Fake => "fake",
    };
    let sealed = &legacy.snapshot_backend;
    if sealed.kind != backend_kind || sealed.version != host.vmm_identity {
        anyhow::bail!("runner lease Snapshot backend compatibility mismatch");
    }
    if sealed.cpu_template.as_deref().unwrap_or("none") != host.cpu_template {
        anyhow::bail!("runner lease Snapshot backend compatibility mismatch");
    }
    if legacy.runner_class_id.as_ref().map(RunnerClassId::as_str)
        != Some(host.runner_restore_contract.as_str())
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
                execution_id: None,
                supervisor: None,
            })
            .unwrap()
            .manifest;

        let overlay = dir.path().join("ov");
        let receipt = restore_and_expose(
            &backend,
            &store,
            manifest,
            RestoreVerification::LegacyLocal,
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

    fn runner_lease_manifest() -> (ReadyStateManifest, SnapshotCompatibilityContractV1) {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let backend = FakeSnapshotBackend::new();
        let compatibility = backend.snapshot_compatibility_contract().unwrap();
        let mut manifest = backend
            .build_ready_state(BuildReadyStateInput {
                store: &store,
                capsule_manifest_hash: "blake3:capsule".to_string(),
                runner_class: Some(
                    capsule::foundation::install_lifecycle::RunnerClassId::from_hash(
                        compatibility.runner_restore_contract.clone(),
                    ),
                ),
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
                supervisor: None,
            })
            .unwrap()
            .manifest;
        manifest.snapshot_backend.kind = "fake".to_string();
        manifest.snapshot_backend.version = compatibility.vmm_identity.clone();
        manifest.snapshot_backend.cpu_template = Some(compatibility.cpu_template.clone());
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
        compatibility.guest_kernel_identity = "sha256:different-kernel".to_string();
        compatibility.runner_restore_contract = "blake3:different-runner".to_string();

        let error =
            verify_runner_lease_candidate(&manifest, "sha256:legacy-execution", &compatibility)
                .unwrap_err();

        assert!(error.to_string().contains("runner contract"), "{error}");
    }

    #[test]
    fn v1_candidate_gate_accepts_an_authenticated_matching_snapshot() {
        use capsule::execution_contract::ExecutionId;
        use capsule::snapshot_manifest::{
            RestoreContractV1, SNAPSHOT_MANIFEST_V1_SCHEMA, SNAPSHOT_RESTORE_CONTRACT_V1_SCHEMA,
            SNAPSHOT_SANITIZATION_ATTESTATION_V1_SCHEMA,
            SNAPSHOT_SECRET_SCAN_ATTESTATION_V1_SCHEMA, SanitizationAttestationV1,
            SecretScanAttestationV1, SnapshotCaptureProvenance,
        };

        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("cas")).unwrap();
        let backend = FakeSnapshotBackend::new();
        let execution_id = ExecutionId::new(format!("blake3:{}", "a".repeat(64))).unwrap();
        let legacy = backend
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
                    memory: (0..50_000u32).map(|i| (i % 256) as u8).collect(),
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
        let host_contract = backend.snapshot_compatibility_contract().unwrap();
        // `SnapshotManifestV1::validate` requires `restore_contract.restore_protocol`
        // to equal `compatibility_contract.runner_restore_contract` (the SAME
        // restore protocol identity) — reuse the backend's real value rather
        // than inventing an unrelated string that would then disagree with the
        // copied `host_contract` below.
        let restore_protocol = host_contract.runner_restore_contract.clone();
        let digest = |fill: char| {
            capsule::execution_contract::ContentDigest::try_from(format!(
                "blake3:{}",
                fill.to_string().repeat(64)
            ))
            .unwrap()
        };
        let candidate = SnapshotManifestV1 {
            schema: SNAPSHOT_MANIFEST_V1_SCHEMA.to_string(),
            execution_id: execution_id.clone(),
            compatibility_contract: host_contract,
            memory_layer_refs: vec![digest('1')],
            vmstate_layer_refs: vec![digest('2')],
            disk_layer_refs: vec![digest('3')],
            restore_contract: RestoreContractV1 {
                schema: SNAPSHOT_RESTORE_CONTRACT_V1_SCHEMA.to_string(),
                restore_protocol,
                steps: Vec::new(),
            },
            capture_policy: CapturePolicyV1::Running,
            capture_provenance: SnapshotCaptureProvenance::default(),
            sanitization_attestation: SanitizationAttestationV1 {
                schema: SNAPSHOT_SANITIZATION_ATTESTATION_V1_SCHEMA.to_string(),
                steps: Vec::new(),
            },
            secret_scan_attestation: SecretScanAttestationV1 {
                schema: SNAPSHOT_SECRET_SCAN_ATTESTATION_V1_SCHEMA.to_string(),
                scanner_identity: "ato-secret-scan/1.0".to_string(),
                policy_identity: "default/v1".to_string(),
                scanned_layers: Vec::new(),
                verdict: "clean".to_string(),
            },
        };

        let envelope = ArtifactEnvelopeV1::accepted(&legacy, &candidate).unwrap();

        verify_v1_candidate(&backend, &legacy, &candidate, &envelope).unwrap();

        // A tampered envelope (locally "promoted" acceptance) is rejected.
        let mut tampered_envelope = envelope.clone();
        tampered_envelope.acceptance.status = snapshot::ArtifactAcceptanceStatus::Quarantined;
        assert!(verify_v1_candidate(&backend, &legacy, &candidate, &tampered_envelope).is_err());

        // A candidate whose execution_id disagrees with the legacy manifest's
        // opaque id is rejected even with a self-consistent envelope.
        let mut other_identity = candidate.clone();
        other_identity.execution_id =
            ExecutionId::new(format!("blake3:{}", "b".repeat(64))).unwrap();
        let other_envelope = ArtifactEnvelopeV1::accepted(&legacy, &other_identity).unwrap();
        let error =
            verify_v1_candidate(&backend, &legacy, &other_identity, &other_envelope).unwrap_err();
        assert!(
            error.to_string().contains("execution_id mismatch"),
            "{error}"
        );
    }
}
