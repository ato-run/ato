//! Provider-owned verification for Snapshot materializations.

use capsule::snapshot_manifest::{
    CapturePolicyV1, HostRestoreCapabilityV1, SnapshotCompatibilityContractV1, SnapshotManifestV1,
};
use thiserror::Error;

use crate::{ArtifactEnvelopeV1, ReadyStateManifest, SnapshotBackend};

#[derive(Debug, Error)]
pub enum SnapshotCompatibilityError {
    #[error("invalid Snapshot materialization: {0}")]
    Invalid(String),
}

pub fn verify_accepted_restore_candidate(
    backend: &dyn SnapshotBackend,
    legacy: &ReadyStateManifest,
    snapshot: &SnapshotManifestV1,
    envelope: &ArtifactEnvelopeV1,
) -> Result<(), SnapshotCompatibilityError> {
    envelope
        .verify(legacy, snapshot)
        .map_err(|error| SnapshotCompatibilityError::Invalid(error.to_string()))?;
    if legacy.execution_id.as_deref() != Some(snapshot.execution_id.as_str()) {
        return Err(SnapshotCompatibilityError::Invalid(
            "materialization/computation contract mismatch".to_owned(),
        ));
    }
    let contract = backend
        .snapshot_compatibility_contract()
        .map_err(|error| SnapshotCompatibilityError::Invalid(error.to_string()))?;
    let host = exact_host_restore_capability(&contract);
    if !snapshot.compatibility_contract.is_satisfied_by(&host) {
        return Err(SnapshotCompatibilityError::Invalid(
            "restore provider does not satisfy materialization compatibility".to_owned(),
        ));
    }
    Ok(())
}

pub fn exact_host_restore_capability(
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
        supported_capture_policies: vec![CapturePolicyV1::Running, CapturePolicyV1::WorkloadIdle],
    }
}
