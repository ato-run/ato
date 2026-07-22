use capsule::execution_contract::{ContentDigest, DigestAlgorithm};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ReadyStateManifest, SNAPSHOT_MANIFEST_V1_SCHEMA, SnapshotCompatibilityContract,
    SnapshotManifestV1,
};

pub const ARTIFACT_ENVELOPE_V1_SCHEMA: &str = "ato.snapshot-artifact-envelope/v1";
pub const ARTIFACT_ENVELOPE_V1_FILENAME: &str = "artifact-envelope-v1.json";
const ARTIFACT_ENVELOPE_ID_DOMAIN: &[u8] = b"ato.snapshot-artifact-envelope/v1\0";
const CAS_ROOT_ID_DOMAIN: &[u8] = b"ato.snapshot-cas-root/v1\0";
const ACCEPTANCE_RECEIPT_ID_DOMAIN: &[u8] = b"ato.snapshot-acceptance-receipt/v1\0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactEnvelopeV1 {
    pub schema: String,
    pub envelope_id: String,
    pub legacy_manifest_id: String,
    pub snapshot_manifest_schema: String,
    pub snapshot_manifest_id: String,
    pub compatibility: SnapshotCompatibilityContract,
    pub cas_root_digest: ContentDigest,
    pub acceptance: ArtifactAcceptance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactAcceptance {
    pub status: ArtifactAcceptanceStatus,
    pub receipt_id: ContentDigest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactAcceptanceStatus {
    Accepted,
    Quarantined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct EnvelopeIdentityProjection<'a> {
    schema: &'a str,
    legacy_manifest_id: &'a str,
    snapshot_manifest_schema: &'a str,
    snapshot_manifest_id: &'a str,
    compatibility: &'a SnapshotCompatibilityContract,
    cas_root_digest: ContentDigest,
    acceptance: &'a ArtifactAcceptance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct AcceptanceReceiptProjection<'a> {
    snapshot_id: &'a str,
    status: ArtifactAcceptanceStatus,
    verifier: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ArtifactEnvelopeError {
    #[error("artifact envelope schema must be ato.snapshot-artifact-envelope/v1")]
    InvalidSchema,
    #[error("artifact envelope carries an invalid content-addressed id")]
    InvalidId,
    #[error("artifact envelope does not authenticate the supplied legacy manifest")]
    LegacyManifestMismatch,
    #[error("artifact envelope does not authenticate the supplied Snapshot manifest")]
    SnapshotManifestMismatch,
    #[error("artifact envelope compatibility evidence differs from the Snapshot manifest")]
    CompatibilityMismatch,
    #[error("artifact envelope is not in accepted state")]
    NotAccepted,
    #[error("failed to canonicalize artifact envelope: {0}")]
    Canonicalization(String),
}

impl ArtifactEnvelopeV1 {
    pub fn accepted(
        legacy: &ReadyStateManifest,
        snapshot: &SnapshotManifestV1,
    ) -> Result<Self, ArtifactEnvelopeError> {
        snapshot
            .validate()
            .map_err(|_| ArtifactEnvelopeError::SnapshotManifestMismatch)?;
        let cas_root_digest = cas_root_digest(legacy)?;
        let acceptance = ArtifactAcceptance {
            status: ArtifactAcceptanceStatus::Accepted,
            receipt_id: acceptance_receipt_id(snapshot)?,
        };
        let mut envelope = Self {
            schema: ARTIFACT_ENVELOPE_V1_SCHEMA.to_string(),
            envelope_id: String::new(),
            legacy_manifest_id: legacy.id(),
            snapshot_manifest_schema: snapshot.schema.clone(),
            snapshot_manifest_id: snapshot.snapshot_id.clone(),
            compatibility: snapshot.compatibility.clone(),
            cas_root_digest,
            acceptance,
        };
        envelope.envelope_id = envelope.compute_envelope_id()?;
        envelope.verify(legacy, snapshot)?;
        Ok(envelope)
    }

    pub fn compute_envelope_id(&self) -> Result<String, ArtifactEnvelopeError> {
        let projection = EnvelopeIdentityProjection {
            schema: &self.schema,
            legacy_manifest_id: &self.legacy_manifest_id,
            snapshot_manifest_schema: &self.snapshot_manifest_schema,
            snapshot_manifest_id: &self.snapshot_manifest_id,
            compatibility: &self.compatibility,
            cas_root_digest: self.cas_root_digest,
            acceptance: &self.acceptance,
        };
        domain_hash(ARTIFACT_ENVELOPE_ID_DOMAIN, &projection).map(|digest| digest.to_string())
    }

    pub fn verify(
        &self,
        legacy: &ReadyStateManifest,
        snapshot: &SnapshotManifestV1,
    ) -> Result<(), ArtifactEnvelopeError> {
        if self.schema != ARTIFACT_ENVELOPE_V1_SCHEMA {
            return Err(ArtifactEnvelopeError::InvalidSchema);
        }
        if self.envelope_id != self.compute_envelope_id()? {
            return Err(ArtifactEnvelopeError::InvalidId);
        }
        if self.legacy_manifest_id != legacy.id()
            || self.cas_root_digest != cas_root_digest(legacy)?
        {
            return Err(ArtifactEnvelopeError::LegacyManifestMismatch);
        }
        snapshot
            .validate()
            .map_err(|_| ArtifactEnvelopeError::SnapshotManifestMismatch)?;
        if self.snapshot_manifest_schema != SNAPSHOT_MANIFEST_V1_SCHEMA
            || self.snapshot_manifest_schema != snapshot.schema
            || self.snapshot_manifest_id != snapshot.snapshot_id
        {
            return Err(ArtifactEnvelopeError::SnapshotManifestMismatch);
        }
        if self.compatibility != snapshot.compatibility {
            return Err(ArtifactEnvelopeError::CompatibilityMismatch);
        }
        if self.acceptance.status != ArtifactAcceptanceStatus::Accepted
            || self.acceptance.receipt_id != acceptance_receipt_id(snapshot)?
        {
            return Err(ArtifactEnvelopeError::NotAccepted);
        }
        Ok(())
    }
}

fn cas_root_digest(legacy: &ReadyStateManifest) -> Result<ContentDigest, ArtifactEnvelopeError> {
    domain_hash(CAS_ROOT_ID_DOMAIN, &legacy.layers)
}

fn acceptance_receipt_id(
    snapshot: &SnapshotManifestV1,
) -> Result<ContentDigest, ArtifactEnvelopeError> {
    domain_hash(
        ACCEPTANCE_RECEIPT_ID_DOMAIN,
        &AcceptanceReceiptProjection {
            snapshot_id: &snapshot.snapshot_id,
            status: ArtifactAcceptanceStatus::Accepted,
            verifier: "platform-disposable-restore/v1",
        },
    )
}

fn domain_hash(
    domain: &[u8],
    value: &impl Serialize,
) -> Result<ContentDigest, ArtifactEnvelopeError> {
    let canonical = serde_jcs::to_vec(value)
        .map_err(|error| ArtifactEnvelopeError::Canonicalization(error.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    hasher.update(&canonical);
    Ok(ContentDigest::new(
        DigestAlgorithm::Blake3,
        *hasher.finalize().as_bytes(),
    ))
}

#[cfg(test)]
mod tests {
    use capsule::execution_contract::ExecutionId;
    use capsulefs::{BlobManifest, ChunkingKind, HotsetProfile, LayerKind};

    use super::*;
    use crate::{
        CapturePolicy, CaptureProvenance, ReadyStateLayers, RestoreContract,
        SanitizationAttestation, SanitizerContract, SecretScanAttestation, SnapshotBackendInfo,
        SnapshotLayerRefs,
    };

    fn manifests() -> (ReadyStateManifest, SnapshotManifestV1) {
        let memory = BlobManifest::new(
            LayerKind::Memory,
            0,
            ChunkingKind::ContentDefined,
            Vec::new(),
        );
        let execution_id = ExecutionId::new(format!("blake3:{}", "1".repeat(64))).unwrap();
        let compatibility = SnapshotCompatibilityContract {
            backend: "firecracker".to_string(),
            format: "fc-v1".to_string(),
            vmm_version: "1.10".to_string(),
            kernel_digest: format!("sha256:{}", "2".repeat(64)),
            cpu_template: Some("T2".to_string()),
            codec: "raw".to_string(),
            runner_contract: format!("blake3:{}", "3".repeat(64)),
        };
        let legacy = ReadyStateManifest {
            schema: crate::READY_STATE_SCHEMA.to_string(),
            capsule_manifest_hash: format!("blake3:{}", "4".repeat(64)),
            has_vsock: false,
            runner_class_id: None,
            execution_id: Some(execution_id.to_string()),
            execution_identity_schema: Some(
                capsule::execution_contract::EXECUTION_CONTRACT_V1_SCHEMA.to_string(),
            ),
            surface_requirement: None,
            layers: ReadyStateLayers {
                memory: Some(memory.clone()),
                ..ReadyStateLayers::default()
            },
            hotset_profile: HotsetProfile::default(),
            snapshot_backend: SnapshotBackendInfo {
                kind: compatibility.backend.clone(),
                version: compatibility.vmm_version.clone(),
                snapshot_format_version: compatibility.format.clone(),
                cpu_template: compatibility.cpu_template.clone(),
            },
            restore_contract: RestoreContract::default(),
            sanitizer_contract: SanitizerContract::default(),
            no_secret_proof: None,
            build_receipt_id: None,
            supervisor_build: None,
        };
        let snapshot = SnapshotManifestV1::new(
            execution_id,
            compatibility,
            SnapshotLayerRefs {
                memory: Some(memory),
                vmstate: None,
                disk_layers: Vec::new(),
            },
            RestoreContract::default(),
            CapturePolicy::Running,
            CaptureProvenance {
                builder: "test".to_string(),
                build_receipt_id: None,
                capsule_manifest_hash: None,
            },
            SanitizationAttestation {
                policy: "none".to_string(),
                steps: Vec::new(),
            },
            SecretScanAttestation {
                scanner: "test".to_string(),
                findings: 0,
                redacted_summary: None,
            },
        )
        .unwrap();
        (legacy, snapshot)
    }

    #[test]
    fn tampered_sidecar_is_rejected_by_the_envelope_boundary() {
        let (legacy, snapshot) = manifests();
        let envelope = ArtifactEnvelopeV1::accepted(&legacy, &snapshot).unwrap();
        let mut tampered = snapshot;
        tampered.sanitization_attestation.policy = "attacker".to_string();
        tampered.snapshot_id = tampered.compute_snapshot_id().unwrap();

        assert_eq!(
            envelope.verify(&legacy, &tampered),
            Err(ArtifactEnvelopeError::SnapshotManifestMismatch)
        );
    }

    #[test]
    fn acceptance_state_is_authenticated_and_cannot_be_promoted_locally() {
        let (legacy, snapshot) = manifests();
        let mut envelope = ArtifactEnvelopeV1::accepted(&legacy, &snapshot).unwrap();
        envelope.acceptance.status = ArtifactAcceptanceStatus::Quarantined;

        assert_eq!(
            envelope.verify(&legacy, &snapshot),
            Err(ArtifactEnvelopeError::InvalidId)
        );
    }
}
