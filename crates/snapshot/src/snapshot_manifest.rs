use capsule::execution_contract::ExecutionId;
use capsulefs::BlobManifest;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::manifest::{ReadyStateManifest, RestoreContract};

pub const SNAPSHOT_MANIFEST_V1_SCHEMA: &str = "ato.snapshot-manifest/v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotManifestV1 {
    pub schema: String,
    pub snapshot_id: String,
    pub execution_id: ExecutionId,
    pub compatibility: SnapshotCompatibilityContract,
    pub layers: SnapshotLayerRefs,
    pub restore_contract: RestoreContract,
    pub capture_policy: CapturePolicy,
    pub capture_provenance: CaptureProvenance,
    pub sanitization_attestation: SanitizationAttestation,
    pub secret_scan_attestation: SecretScanAttestation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SnapshotIdentityProjection<'a> {
    schema: &'a str,
    execution_id: &'a ExecutionId,
    compatibility: &'a SnapshotCompatibilityContract,
    layers: &'a SnapshotLayerRefs,
    restore_contract: &'a RestoreContract,
    capture_policy: CapturePolicy,
    capture_provenance: &'a CaptureProvenance,
    sanitization_attestation: &'a SanitizationAttestation,
    secret_scan_attestation: &'a SecretScanAttestation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotCompatibilityContract {
    pub backend: String,
    pub format: String,
    pub vmm_version: String,
    pub kernel_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_template: Option<String>,
    pub codec: String,
    pub runner_contract: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotRestoreCapabilities {
    pub backend: Option<String>,
    pub formats: Vec<String>,
    pub vmm_versions: Vec<String>,
    pub kernel_digests: Vec<String>,
    pub cpu_templates: Vec<String>,
    pub codecs: Vec<String>,
    pub runner_contracts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotLayerRefs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<BlobManifest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vmstate: Option<BlobManifest>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disk_layers: Vec<BlobManifest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapturePolicy {
    Running,
    WorkloadIdle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureProvenance {
    pub builder: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_receipt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capsule_manifest_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SanitizationAttestation {
    pub policy: String,
    pub steps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretScanAttestation {
    pub scanner: String,
    pub findings: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redacted_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotCatalogRecord {
    pub manifest: SnapshotManifestV1,
    pub status: SnapshotCatalogStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotCatalogStatus {
    Accepted,
    Quarantined { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SnapshotManifestError {
    #[error("snapshot manifest schema must be ato.snapshot-manifest/v1")]
    InvalidSchema,
    #[error("snapshot manifest field '{0}' must be known and non-empty")]
    UnknownCompatibility(&'static str),
    #[error("snapshot manifest contains no captured state layers")]
    MissingSnapshotLayers,
    #[error("snapshot_id mismatch: expected {expected}, got {actual}")]
    SnapshotIdMismatch { expected: String, actual: String },
    #[error("failed to canonicalize snapshot manifest: {0}")]
    Canonicalization(String),
    #[error("legacy execution_id does not match the verified Capsule v1 execution_id")]
    LegacyExecutionIdMismatch,
}

impl SnapshotManifestV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        execution_id: ExecutionId,
        compatibility: SnapshotCompatibilityContract,
        layers: SnapshotLayerRefs,
        restore_contract: RestoreContract,
        capture_policy: CapturePolicy,
        capture_provenance: CaptureProvenance,
        sanitization_attestation: SanitizationAttestation,
        secret_scan_attestation: SecretScanAttestation,
    ) -> Result<Self, SnapshotManifestError> {
        let mut manifest = Self {
            schema: SNAPSHOT_MANIFEST_V1_SCHEMA.to_string(),
            snapshot_id: String::new(),
            execution_id,
            compatibility,
            layers,
            restore_contract,
            capture_policy,
            capture_provenance,
            sanitization_attestation,
            secret_scan_attestation,
        };
        manifest.validate_payload()?;
        manifest.snapshot_id = manifest.compute_snapshot_id()?;
        Ok(manifest)
    }

    pub fn compute_snapshot_id(&self) -> Result<String, SnapshotManifestError> {
        let projection = SnapshotIdentityProjection {
            schema: &self.schema,
            execution_id: &self.execution_id,
            compatibility: &self.compatibility,
            layers: &self.layers,
            restore_contract: &self.restore_contract,
            capture_policy: self.capture_policy,
            capture_provenance: &self.capture_provenance,
            sanitization_attestation: &self.sanitization_attestation,
            secret_scan_attestation: &self.secret_scan_attestation,
        };
        let canonical = serde_jcs::to_vec(&projection)
            .map_err(|error| SnapshotManifestError::Canonicalization(error.to_string()))?;
        Ok(format!("blake3:{}", blake3::hash(&canonical).to_hex()))
    }

    pub fn validate(&self) -> Result<(), SnapshotManifestError> {
        self.validate_payload()?;
        let expected = self.compute_snapshot_id()?;
        if expected != self.snapshot_id {
            return Err(SnapshotManifestError::SnapshotIdMismatch {
                expected,
                actual: self.snapshot_id.clone(),
            });
        }
        Ok(())
    }

    fn validate_payload(&self) -> Result<(), SnapshotManifestError> {
        if self.schema != SNAPSHOT_MANIFEST_V1_SCHEMA {
            return Err(SnapshotManifestError::InvalidSchema);
        }
        self.compatibility.validate()?;
        if self.layers.memory.is_none()
            && self.layers.vmstate.is_none()
            && self.layers.disk_layers.is_empty()
        {
            return Err(SnapshotManifestError::MissingSnapshotLayers);
        }
        for (field, value) in [
            (
                "capture_provenance.builder",
                self.capture_provenance.builder.as_str(),
            ),
            (
                "sanitization_attestation.policy",
                self.sanitization_attestation.policy.as_str(),
            ),
            (
                "secret_scan_attestation.scanner",
                self.secret_scan_attestation.scanner.as_str(),
            ),
        ] {
            if value.trim().is_empty() {
                return Err(SnapshotManifestError::UnknownCompatibility(field));
            }
        }
        Ok(())
    }
}

impl SnapshotCompatibilityContract {
    fn validate(&self) -> Result<(), SnapshotManifestError> {
        for (field, value) in [
            ("compatibility.backend", self.backend.as_str()),
            ("compatibility.format", self.format.as_str()),
            ("compatibility.vmm_version", self.vmm_version.as_str()),
            ("compatibility.kernel_digest", self.kernel_digest.as_str()),
            ("compatibility.codec", self.codec.as_str()),
            (
                "compatibility.runner_contract",
                self.runner_contract.as_str(),
            ),
        ] {
            if value.trim().is_empty() {
                return Err(SnapshotManifestError::UnknownCompatibility(field));
            }
        }
        if self
            .cpu_template
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(SnapshotManifestError::UnknownCompatibility(
                "compatibility.cpu_template",
            ));
        }
        Ok(())
    }

    fn is_satisfied_by(
        &self,
        capabilities: &SnapshotRestoreCapabilities,
    ) -> Result<bool, SnapshotManifestError> {
        capabilities.validate()?;
        Ok(
            capabilities.backend.as_deref() == Some(self.backend.as_str())
                && capabilities.formats.contains(&self.format)
                && capabilities.vmm_versions.contains(&self.vmm_version)
                && capabilities.kernel_digests.contains(&self.kernel_digest)
                && self
                    .cpu_template
                    .as_ref()
                    .is_none_or(|required| capabilities.cpu_templates.contains(required))
                && capabilities.codecs.contains(&self.codec)
                && capabilities
                    .runner_contracts
                    .contains(&self.runner_contract),
        )
    }
}

impl SnapshotRestoreCapabilities {
    fn validate(&self) -> Result<(), SnapshotManifestError> {
        if self
            .backend
            .as_ref()
            .is_none_or(|value| value.trim().is_empty())
        {
            return Err(SnapshotManifestError::UnknownCompatibility("host.backend"));
        }
        for (field, values) in [
            ("host.formats", &self.formats),
            ("host.vmm_versions", &self.vmm_versions),
            ("host.kernel_digests", &self.kernel_digests),
            ("host.codecs", &self.codecs),
            ("host.runner_contracts", &self.runner_contracts),
        ] {
            if values.is_empty() || values.iter().any(|value| value.trim().is_empty()) {
                return Err(SnapshotManifestError::UnknownCompatibility(field));
            }
        }
        Ok(())
    }
}

impl SnapshotCatalogRecord {
    pub fn accepted(manifest: SnapshotManifestV1) -> Self {
        Self {
            manifest,
            status: SnapshotCatalogStatus::Accepted,
        }
    }

    pub fn quarantine(&mut self, reason: impl Into<String>) {
        self.status = SnapshotCatalogStatus::Quarantined {
            reason: reason.into(),
        };
    }
}

pub fn select_compatible_snapshot<'a>(
    execution_id: &ExecutionId,
    capabilities: &SnapshotRestoreCapabilities,
    ranked_candidates: &'a [SnapshotCatalogRecord],
) -> Result<Option<&'a SnapshotManifestV1>, SnapshotManifestError> {
    capabilities.validate()?;
    for record in ranked_candidates {
        if !matches!(record.status, SnapshotCatalogStatus::Accepted) {
            continue;
        }
        let candidate = &record.manifest;
        candidate.validate()?;
        if &candidate.execution_id != execution_id {
            continue;
        }
        if candidate.compatibility.is_satisfied_by(capabilities)? {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

pub fn migrate_legacy_manifest(
    legacy: &ReadyStateManifest,
    verified_execution_id: ExecutionId,
    compatibility: SnapshotCompatibilityContract,
) -> Result<SnapshotManifestV1, SnapshotManifestError> {
    if legacy.execution_id.as_deref().is_some_and(|legacy_id| {
        legacy_id.starts_with("blake3:") && legacy_id != verified_execution_id.as_str()
    }) {
        return Err(SnapshotManifestError::LegacyExecutionIdMismatch);
    }
    let mut disk_layers = Vec::new();
    for layer in [
        &legacy.layers.rootfs,
        &legacy.layers.runtime,
        &legacy.layers.dependency,
        &legacy.layers.app,
    ] {
        if let Some(layer) = layer {
            disk_layers.push(layer.clone());
        }
    }
    SnapshotManifestV1::new(
        verified_execution_id,
        compatibility,
        SnapshotLayerRefs {
            memory: legacy.layers.memory.clone(),
            vmstate: legacy.layers.vmstate.clone(),
            disk_layers,
        },
        legacy.restore_contract.clone(),
        CapturePolicy::Running,
        CaptureProvenance {
            builder: "legacy-ready-state-migration".to_string(),
            build_receipt_id: legacy.build_receipt_id.clone(),
            capsule_manifest_hash: Some(legacy.capsule_manifest_hash.clone()),
        },
        SanitizationAttestation {
            policy: "legacy-sanitizer-contract".to_string(),
            steps: legacy
                .sanitizer_contract
                .steps
                .iter()
                .map(|step| step.step.clone())
                .collect(),
        },
        SecretScanAttestation {
            scanner: legacy.no_secret_proof.as_ref().map_or_else(
                || "legacy-unavailable".to_string(),
                |proof| proof.scanner_version.clone(),
            ),
            findings: legacy
                .no_secret_proof
                .as_ref()
                .map_or(0, |proof| proof.findings.len() as u64),
            redacted_summary: Some(
                "migrated from ato.ready-state/v1; scan details omitted".to_string(),
            ),
        },
    )
}

#[cfg(test)]
mod tests {
    use capsule::execution_contract::ExecutionId;
    use capsulefs::{BlobManifest, ChunkingKind, HotsetProfile, LayerKind};

    use super::*;
    use crate::manifest::{
        READY_STATE_SCHEMA, ReadyStateLayers, SanitizerContract, SnapshotBackendInfo,
    };

    fn execution_id(byte: char) -> ExecutionId {
        ExecutionId::new(format!("blake3:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn compatibility(format: &str) -> SnapshotCompatibilityContract {
        SnapshotCompatibilityContract {
            backend: "firecracker".to_string(),
            format: format.to_string(),
            vmm_version: "1.10".to_string(),
            kernel_digest: "sha256:kernel".to_string(),
            cpu_template: Some("T2".to_string()),
            codec: "raw".to_string(),
            runner_contract: "ato-runner/v1".to_string(),
        }
    }

    fn capabilities() -> SnapshotRestoreCapabilities {
        SnapshotRestoreCapabilities {
            backend: Some("firecracker".to_string()),
            formats: vec!["fc-v1".to_string()],
            vmm_versions: vec!["1.10".to_string()],
            kernel_digests: vec!["sha256:kernel".to_string()],
            cpu_templates: vec!["T2".to_string()],
            codecs: vec!["raw".to_string()],
            runner_contracts: vec!["ato-runner/v1".to_string()],
        }
    }

    fn manifest(execution_id: ExecutionId, format: &str) -> SnapshotManifestV1 {
        SnapshotManifestV1::new(
            execution_id,
            compatibility(format),
            SnapshotLayerRefs {
                memory: Some(BlobManifest::new(
                    LayerKind::Memory,
                    0,
                    ChunkingKind::ContentDefined,
                    Vec::new(),
                )),
                vmstate: Some(BlobManifest::new(
                    LayerKind::VmState,
                    0,
                    ChunkingKind::ContentDefined,
                    Vec::new(),
                )),
                disk_layers: Vec::new(),
            },
            RestoreContract::default(),
            CapturePolicy::Running,
            CaptureProvenance {
                builder: "snapshot-builder".to_string(),
                build_receipt_id: Some("receipt-1".to_string()),
                capsule_manifest_hash: Some("blake3:legacy".to_string()),
            },
            SanitizationAttestation {
                policy: "no-external-state/v1".to_string(),
                steps: vec!["detach-build-inputs".to_string()],
            },
            SecretScanAttestation {
                scanner: "ato-scan/v1".to_string(),
                findings: 0,
                redacted_summary: None,
            },
        )
        .unwrap()
    }

    #[test]
    fn required_execution_id_and_snapshot_id_validate() {
        let manifest = manifest(execution_id('a'), "fc-v1");
        manifest.validate().unwrap();

        let mut value = serde_json::to_value(&manifest).unwrap();
        value.as_object_mut().unwrap().remove("execution_id");
        assert!(serde_json::from_value::<SnapshotManifestV1>(value).is_err());
    }

    #[test]
    fn snapshot_format_changes_snapshot_id_not_execution_id() {
        let first = manifest(execution_id('b'), "fc-v1");
        let second = manifest(execution_id('b'), "fc-v2");
        assert_eq!(first.execution_id, second.execution_id);
        assert_ne!(first.snapshot_id, second.snapshot_id);
    }

    #[test]
    fn selection_filters_exact_identity_before_compatibility() {
        let wanted = execution_id('c');
        let wrong = manifest(execution_id('d'), "fc-v1");
        let incompatible = manifest(wanted.clone(), "fc-v2");
        let compatible = manifest(wanted.clone(), "fc-v1");
        let candidates = vec![
            SnapshotCatalogRecord::accepted(wrong),
            SnapshotCatalogRecord::accepted(incompatible),
            SnapshotCatalogRecord::accepted(compatible.clone()),
        ];

        let selected = select_compatible_snapshot(&wanted, &capabilities(), &candidates)
            .unwrap()
            .unwrap();
        assert_eq!(selected.snapshot_id, compatible.snapshot_id);
    }

    #[test]
    fn unknown_compatibility_fails_closed() {
        let wanted = execution_id('e');
        let candidate = manifest(wanted.clone(), "fc-v1");
        let mut capabilities = capabilities();
        capabilities.backend = None;

        assert!(
            select_compatible_snapshot(
                &wanted,
                &capabilities,
                &[SnapshotCatalogRecord::accepted(candidate)]
            )
            .is_err()
        );
    }

    #[test]
    fn quarantine_is_catalog_state_not_manifest_mutation() {
        let manifest = manifest(execution_id('f'), "fc-v1");
        let id = manifest.snapshot_id.clone();
        let mut record = SnapshotCatalogRecord::accepted(manifest);
        record.quarantine("restore validation failed");

        assert_eq!(record.manifest.snapshot_id, id);
        assert!(matches!(
            record.status,
            SnapshotCatalogStatus::Quarantined { .. }
        ));
        assert!(
            select_compatible_snapshot(&execution_id('f'), &capabilities(), &[record])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn legacy_migration_creates_a_new_v1_manifest_id() {
        let memory = BlobManifest::new(
            LayerKind::Memory,
            0,
            ChunkingKind::ContentDefined,
            Vec::new(),
        );
        let legacy = ReadyStateManifest {
            schema: READY_STATE_SCHEMA.to_string(),
            capsule_manifest_hash: "blake3:legacy".to_string(),
            has_vsock: false,
            runner_class_id: None,
            execution_id: None,
            surface_requirement: None,
            layers: ReadyStateLayers {
                memory: Some(memory),
                ..ReadyStateLayers::default()
            },
            hotset_profile: HotsetProfile::default(),
            snapshot_backend: SnapshotBackendInfo {
                kind: "firecracker".to_string(),
                version: "1.10".to_string(),
                snapshot_format_version: "fc-v1".to_string(),
                cpu_template: Some("T2".to_string()),
            },
            restore_contract: RestoreContract::default(),
            sanitizer_contract: SanitizerContract::default(),
            no_secret_proof: None,
            build_receipt_id: None,
            supervisor_build: None,
        };

        let migrated =
            migrate_legacy_manifest(&legacy, execution_id('9'), compatibility("fc-v1")).unwrap();
        assert_eq!(migrated.schema, SNAPSHOT_MANIFEST_V1_SCHEMA);
        assert_ne!(migrated.snapshot_id, legacy.id());
        migrated.validate().unwrap();
    }
}
