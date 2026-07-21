use std::collections::BTreeSet;

use capsule::execution_contract::{ExternalStateAccess, ExternalStateContract, SnapshotExclusion};
use capsulefs::ContentHash;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::snapshot_manifest::SnapshotManifestV1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OpaqueStateRef(String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OpaqueStateGeneration(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalStateInstance {
    pub state_ref: OpaqueStateRef,
    pub generation: OpaqueStateGeneration,
    pub schema: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalStateAttachRequest {
    pub contract: ExternalStateContract,
    pub instance: ExternalStateInstance,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalStateAttachmentPlan {
    pub name: String,
    pub target: String,
    pub access: ExternalStateAccess,
    pub schema: String,
    pub state_ref: OpaqueStateRef,
    pub generation: OpaqueStateGeneration,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionExternalStateReceipt {
    pub name: String,
    pub schema: String,
    pub state_ref: OpaqueStateRef,
    pub state_generation: OpaqueStateGeneration,
    pub access: ExternalStateAccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExternalStateBoundaryError {
    #[error("External State opaque reference is invalid")]
    InvalidOpaqueReference,
    #[error("External State generation is invalid")]
    InvalidGeneration,
    #[error("External State contract must use snapshot = exclude")]
    SnapshotMustExclude,
    #[error("External State schema mismatch for '{name}': expected {expected}, got {actual}")]
    SchemaMismatch {
        name: String,
        expected: String,
        actual: String,
    },
    #[error("External State layer {0} is referenced by a shared Snapshot")]
    StateLayerIncluded(String),
}

impl OpaqueStateRef {
    pub fn new(value: String) -> Result<Self, ExternalStateBoundaryError> {
        validate_opaque(&value).map_err(|_| ExternalStateBoundaryError::InvalidOpaqueReference)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for OpaqueStateRef {
    type Error = ExternalStateBoundaryError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<OpaqueStateRef> for String {
    fn from(value: OpaqueStateRef) -> Self {
        value.0
    }
}

impl OpaqueStateGeneration {
    pub fn new(value: String) -> Result<Self, ExternalStateBoundaryError> {
        validate_opaque(&value).map_err(|_| ExternalStateBoundaryError::InvalidGeneration)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for OpaqueStateGeneration {
    type Error = ExternalStateBoundaryError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<OpaqueStateGeneration> for String {
    fn from(value: OpaqueStateGeneration) -> Self {
        value.0
    }
}

impl ExternalStateAttachmentPlan {
    pub fn receipt(&self) -> SessionExternalStateReceipt {
        SessionExternalStateReceipt {
            name: self.name.clone(),
            schema: self.schema.clone(),
            state_ref: self.state_ref.clone(),
            state_generation: self.generation.clone(),
            access: self.access,
        }
    }
}

pub fn plan_external_state_attach(
    request: ExternalStateAttachRequest,
) -> Result<ExternalStateAttachmentPlan, ExternalStateBoundaryError> {
    if request.contract.snapshot != SnapshotExclusion::Exclude {
        return Err(ExternalStateBoundaryError::SnapshotMustExclude);
    }
    if request.contract.schema != request.instance.schema {
        return Err(ExternalStateBoundaryError::SchemaMismatch {
            name: request.contract.name,
            expected: request.contract.schema,
            actual: request.instance.schema,
        });
    }
    Ok(ExternalStateAttachmentPlan {
        name: request.contract.name,
        target: request.contract.target,
        access: request.contract.access,
        schema: request.contract.schema,
        state_ref: request.instance.state_ref,
        generation: request.instance.generation,
    })
}

pub fn ensure_external_state_layers_excluded(
    snapshot: &SnapshotManifestV1,
    external_state_layer_ids: &BTreeSet<ContentHash>,
) -> Result<(), ExternalStateBoundaryError> {
    let layers = snapshot
        .layers
        .memory
        .iter()
        .chain(snapshot.layers.vmstate.iter())
        .chain(snapshot.layers.disk_layers.iter());
    for layer in layers {
        let id = layer.id();
        if external_state_layer_ids.contains(&id) {
            return Err(ExternalStateBoundaryError::StateLayerIncluded(
                id.to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_opaque(value: &str) -> Result<(), ()> {
    if value.is_empty()
        || value.len() > 512
        || value
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use capsule::execution_contract::{ExecutionId, ExternalStateAccess, SnapshotExclusion};
    use capsulefs::{BlobManifest, ChunkingKind, LayerKind};

    use super::*;
    use crate::manifest::RestoreContract;
    use crate::snapshot_manifest::{
        CapturePolicy, CaptureProvenance, SanitizationAttestation, SecretScanAttestation,
        SnapshotCompatibilityContract, SnapshotLayerRefs,
    };

    fn request(schema: &str) -> ExternalStateAttachRequest {
        ExternalStateAttachRequest {
            contract: ExternalStateContract {
                name: "data".to_string(),
                target: "/data".to_string(),
                access: ExternalStateAccess::ReadWrite,
                schema: "1".to_string(),
                snapshot: SnapshotExclusion::Exclude,
            },
            instance: ExternalStateInstance {
                state_ref: OpaqueStateRef::new("state:opaque/data".to_string()).unwrap(),
                generation: OpaqueStateGeneration::new("gen_456".to_string()).unwrap(),
                schema: schema.to_string(),
            },
        }
    }

    #[test]
    fn schema_mismatch_fails_before_an_attachment_plan_exists() {
        assert!(matches!(
            plan_external_state_attach(request("2")),
            Err(ExternalStateBoundaryError::SchemaMismatch { .. })
        ));
    }

    #[test]
    fn receipt_contains_only_opaque_compatibility_evidence() {
        let plan = plan_external_state_attach(request("1")).unwrap();
        let json = serde_json::to_string(&plan.receipt()).unwrap();
        assert!(json.contains("state:opaque/data"));
        assert!(json.contains("gen_456"));
        for forbidden in ["secret", "token", "content", "owner_id", "volume_id"] {
            assert!(!json.contains(forbidden));
        }
    }

    #[test]
    fn opaque_values_reject_whitespace_and_control_characters() {
        assert!(OpaqueStateRef::new("state with spaces".to_string()).is_err());
        assert!(OpaqueStateGeneration::new("gen\n1".to_string()).is_err());
    }

    #[test]
    fn shared_snapshot_rejects_an_external_state_layer_reference() {
        let state_layer =
            BlobManifest::new(LayerKind::App, 0, ChunkingKind::ContentDefined, Vec::new());
        let state_layer_id = state_layer.id();
        let snapshot = SnapshotManifestV1::new(
            ExecutionId::new(format!("blake3:{}", "a".repeat(64))).unwrap(),
            SnapshotCompatibilityContract {
                backend: "fake".to_string(),
                format: "fake-v1".to_string(),
                vmm_version: "1".to_string(),
                kernel_digest: "sha256:kernel".to_string(),
                cpu_template: None,
                codec: "raw".to_string(),
                runner_contract: "runner/v1".to_string(),
            },
            SnapshotLayerRefs {
                memory: None,
                vmstate: None,
                disk_layers: vec![state_layer],
            },
            RestoreContract::default(),
            CapturePolicy::Running,
            CaptureProvenance {
                builder: "test".to_string(),
                build_receipt_id: None,
                capsule_manifest_hash: None,
            },
            SanitizationAttestation {
                policy: "test".to_string(),
                steps: Vec::new(),
            },
            SecretScanAttestation {
                scanner: "test".to_string(),
                findings: 0,
                redacted_summary: None,
            },
        )
        .unwrap();

        assert!(matches!(
            ensure_external_state_layers_excluded(&snapshot, &BTreeSet::from([state_layer_id])),
            Err(ExternalStateBoundaryError::StateLayerIncluded(_))
        ));
    }
}
