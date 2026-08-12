//! Capsule Protocol State adapter for accepted Ready-State artifacts.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::Path;

use capsule::protocol_bundle::{
    ObjectMetadata, ObjectSource, PortableExportError, PortableExportPolicy, PortableObjectRole,
    ProtocolBundleError,
};
use capsule::snapshot_manifest::{
    HostRestoreCapabilityV1, SnapshotBackendKind, SnapshotManifestV1,
};
use capsule_protocol::{CapsuleDescriptor, ContentRef, IoRecord, StateRef, StateTypeId};
use capsulefs::{CasStore, ContentHash, validate_blob_manifest};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ArtifactEnvelopeV1, FakeSnapshotBackend, FirecrackerBackend, KataBackend, QemuBackend,
    READY_STATE_SCHEMA, ReadyStateManifest, SnapshotBackend,
};

pub const READY_STATE_STATE_TYPE: &str = "ato.state.ready-state@1";
pub const READY_STATE_STATE_OBJECT_SCHEMA: &str = "ato.state.ready-state-object/v1";
pub const READY_STATE_PRIMARY_OBJECT_MAX_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadyStateStateObjectV1 {
    pub schema: String,
    pub legacy_manifest: ReadyStateManifest,
    pub snapshot_manifest: SnapshotManifestV1,
    pub artifact_envelope: ArtifactEnvelopeV1,
}

pub struct ReadyStateObjectSource<'a> {
    primary_ref: ContentRef,
    primary_object: Vec<u8>,
    store: &'a CasStore,
    chunks: BTreeMap<ContentRef, u64>,
}

pub struct ReadyStateStateExport<'a> {
    pub state: StateRef,
    pub primary_object: Vec<u8>,
    pub objects: ReadyStateObjectSource<'a>,
    pub adapter_roles: BTreeMap<ContentRef, Vec<PortableObjectRole>>,
}

#[derive(Debug)]
pub struct ImportedReadyState {
    pub legacy_manifest: ReadyStateManifest,
    pub snapshot_manifest: SnapshotManifestV1,
    pub artifact_envelope: ArtifactEnvelopeV1,
    pub cas_store: CasStore,
}

#[derive(Debug, Error)]
pub enum ReadyStateStateError {
    #[error("unsupported Ready-State State type {0}")]
    UnsupportedStateType(String),
    #[error("ReadyStateBindingsUnsupported")]
    ReadyStateBindingsUnsupported,
    #[error("ReadyStateDurableVolumesUnsupported")]
    ReadyStateDurableVolumesUnsupported,
    #[error("invalid Ready-State State: {0}")]
    Invalid(String),
    #[error("Ready-State object source failed: {0}")]
    ObjectSource(#[from] ProtocolBundleError),
    #[error("CapsuleFS failed: {0}")]
    CapsuleFs(#[from] capsulefs::CapsuleFsError),
    #[error("Ready-State State JSON failed: {0}")]
    Json(#[from] serde_json::Error),
}

impl ReadyStateStateObjectV1 {
    pub fn accepted(
        legacy_manifest: ReadyStateManifest,
        snapshot_manifest: SnapshotManifestV1,
        artifact_envelope: ArtifactEnvelopeV1,
    ) -> Result<Self, ReadyStateStateError> {
        let object = Self {
            schema: READY_STATE_STATE_OBJECT_SCHEMA.to_string(),
            legacy_manifest,
            snapshot_manifest,
            artifact_envelope,
        };
        object.validate()?;
        Ok(object)
    }

    pub fn validate(&self) -> Result<(), ReadyStateStateError> {
        if self.schema != READY_STATE_STATE_OBJECT_SCHEMA {
            return Err(ReadyStateStateError::Invalid(
                "wrong Ready-State State object schema".to_string(),
            ));
        }
        if self.legacy_manifest.schema != READY_STATE_SCHEMA {
            return Err(ReadyStateStateError::Invalid(
                "wrong legacy Ready-State schema".to_string(),
            ));
        }
        if let Some(supervisor) = &self.legacy_manifest.supervisor_build {
            if !supervisor.binding_names.is_empty() {
                return Err(ReadyStateStateError::ReadyStateBindingsUnsupported);
            }
            if !supervisor.state_volumes.is_empty() {
                return Err(ReadyStateStateError::ReadyStateDurableVolumesUnsupported);
            }
            if supervisor.state_owner_scope.is_some() {
                return Err(ReadyStateStateError::Invalid(
                    "Ready-State state_owner_scope is present without durable volumes".to_string(),
                ));
            }
        }
        self.snapshot_manifest
            .validate()
            .map_err(|error| ReadyStateStateError::Invalid(error.to_string()))?;
        self.artifact_envelope
            .verify(&self.legacy_manifest, &self.snapshot_manifest)
            .map_err(|error| ReadyStateStateError::Invalid(error.to_string()))?;
        validate_backend_agreement(self)?;
        for (_, manifest) in self.legacy_manifest.layers.iter() {
            validate_blob_manifest(manifest)?;
        }
        if !self
            .legacy_manifest
            .no_secret_proof
            .as_ref()
            .is_some_and(|proof| proof.is_clean())
        {
            return Err(ReadyStateStateError::Invalid(
                "Ready-State no-secret proof is missing or not clean".to_string(),
            ));
        }
        Ok(())
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ReadyStateStateError> {
        self.validate()?;
        Ok(serde_jcs::to_vec(self)?)
    }
}

pub fn export_ready_state<'a>(
    object: ReadyStateStateObjectV1,
    store: &'a CasStore,
) -> Result<ReadyStateStateExport<'a>, ReadyStateStateError> {
    let primary_object = object.canonical_bytes()?;
    let primary_ref = content_ref_for_bytes(&primary_object)?;
    let state_type = ready_state_type()?;
    let mut chunks = BTreeMap::new();
    for (_, manifest) in object.legacy_manifest.layers.iter() {
        for chunk in &manifest.chunks {
            let reference = ContentRef::parse(chunk.hash.as_str())
                .map_err(|error| ReadyStateStateError::Invalid(error.to_string()))?;
            match chunks.insert(reference.clone(), chunk.length) {
                Some(previous) if previous != chunk.length => {
                    return Err(ReadyStateStateError::Invalid(format!(
                        "chunk {reference} has conflicting declared lengths {previous} and {}",
                        chunk.length
                    )));
                }
                _ => {}
            }
        }
    }
    let adapter_roles = chunks
        .keys()
        .map(|reference| {
            (
                reference.clone(),
                vec![PortableObjectRole::StateAdapterObject {
                    state_type: state_type.clone(),
                }],
            )
        })
        .collect();
    let state = StateRef {
        state_type,
        state_ref: primary_ref.clone(),
    };
    Ok(ReadyStateStateExport {
        state,
        objects: ReadyStateObjectSource {
            primary_ref,
            primary_object: primary_object.clone(),
            store,
            chunks,
        },
        primary_object,
        adapter_roles,
    })
}

impl ObjectSource for ReadyStateObjectSource<'_> {
    fn index(&self) -> Result<BTreeMap<ContentRef, ObjectMetadata>, ProtocolBundleError> {
        let mut index = self
            .chunks
            .iter()
            .map(|(reference, size)| {
                (
                    reference.clone(),
                    ObjectMetadata {
                        reference: reference.clone(),
                        size: *size,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        index.insert(
            self.primary_ref.clone(),
            ObjectMetadata {
                reference: self.primary_ref.clone(),
                size: self.primary_object.len() as u64,
            },
        );
        Ok(index)
    }

    fn open(&self, reference: &ContentRef) -> Result<Box<dyn Read + Send>, ProtocolBundleError> {
        if reference == &self.primary_ref {
            return Ok(Box::new(Cursor::new(self.primary_object.clone())));
        }
        let hash = ContentHash::parse(&reference.to_string())
            .map_err(|error| ProtocolBundleError::Invalid(error.to_string()))?;
        Ok(Box::new(self.store.open_chunk_reader(&hash).map_err(
            |error| ProtocolBundleError::Invalid(error.to_string()),
        )?))
    }
}

pub fn import_ready_state(
    state: &StateRef,
    objects: &dyn ObjectSource,
    cas_root: &Path,
) -> Result<ImportedReadyState, ReadyStateStateError> {
    if state.state_type.as_str() != READY_STATE_STATE_TYPE {
        return Err(ReadyStateStateError::UnsupportedStateType(
            state.state_type.to_string(),
        ));
    }
    let index = objects.index()?;
    let primary_metadata = index.get(&state.state_ref).ok_or_else(|| {
        ReadyStateStateError::Invalid("primary Ready-State object is missing".to_string())
    })?;
    if primary_metadata.size > READY_STATE_PRIMARY_OBJECT_MAX_BYTES {
        return Err(ReadyStateStateError::Invalid(format!(
            "primary Ready-State object exceeds the {READY_STATE_PRIMARY_OBJECT_MAX_BYTES}-byte limit"
        )));
    }
    let mut primary = Vec::with_capacity(primary_metadata.size as usize);
    let mut primary_reader = objects.open(&state.state_ref)?;
    primary_reader
        .by_ref()
        .take(READY_STATE_PRIMARY_OBJECT_MAX_BYTES + 1)
        .read_to_end(&mut primary)
        .map_err(|error| ReadyStateStateError::ObjectSource(ProtocolBundleError::Io(error)))?;
    if primary.len() as u64 > READY_STATE_PRIMARY_OBJECT_MAX_BYTES {
        return Err(ReadyStateStateError::Invalid(format!(
            "primary Ready-State object exceeds the {READY_STATE_PRIMARY_OBJECT_MAX_BYTES}-byte limit"
        )));
    }
    if primary.len() as u64 != primary_metadata.size {
        return Err(ReadyStateStateError::Invalid(
            "primary Ready-State object size does not match its metadata".to_string(),
        ));
    }
    if content_ref_for_bytes(&primary)? != state.state_ref {
        return Err(ReadyStateStateError::Invalid(
            "primary Ready-State object digest mismatch".to_string(),
        ));
    }
    let state_object: ReadyStateStateObjectV1 = serde_json::from_slice(&primary)?;
    state_object.validate()?;
    if serde_jcs::to_vec(&state_object)? != primary {
        return Err(ReadyStateStateError::Invalid(
            "primary Ready-State object is not JCS canonical".to_string(),
        ));
    }
    let required = required_chunks(&state_object.legacy_manifest)?;
    for reference in index.keys() {
        if reference != &state.state_ref && !required.contains_key(reference) {
            return Err(ReadyStateStateError::Invalid(format!(
                "unexpected object {reference} is not in the Ready-State closure"
            )));
        }
    }
    for reference in required.keys() {
        if !index.contains_key(reference) {
            return Err(ReadyStateStateError::Invalid(format!(
                "required Ready-State chunk {reference} is missing"
            )));
        }
    }
    let cas_store = CasStore::open(cas_root)?;
    for (reference, expected_size) in &required {
        let hash = ContentHash::parse(&reference.to_string())
            .map_err(|error| ReadyStateStateError::Invalid(error.to_string()))?;
        let mut reader = objects.open(reference)?;
        cas_store.import_verified_chunk(&hash, *expected_size, reader.as_mut())?;
    }
    for (_, manifest) in state_object.legacy_manifest.layers.iter() {
        if !cas_store.has_all_chunks(manifest) {
            return Err(ReadyStateStateError::Invalid(
                "Ready-State layer is not fully resident after import".to_string(),
            ));
        }
    }
    Ok(ImportedReadyState {
        legacy_manifest: state_object.legacy_manifest,
        snapshot_manifest: state_object.snapshot_manifest,
        artifact_envelope: state_object.artifact_envelope,
        cas_store,
    })
}

pub fn select_backend_for_ready_state(
    state: &ReadyStateStateObjectV1,
) -> Result<Box<dyn SnapshotBackend>, ReadyStateStateError> {
    state.validate()?;
    let backend: Box<dyn SnapshotBackend> =
        match state.snapshot_manifest.compatibility_contract.backend {
            SnapshotBackendKind::Fake => Box::new(FakeSnapshotBackend::new()),
            SnapshotBackendKind::Firecracker => Box::new(FirecrackerBackend::new()),
            SnapshotBackendKind::Qemu => Box::new(QemuBackend::new()),
            SnapshotBackendKind::Kata => Box::new(KataBackend::new()),
            SnapshotBackendKind::Cloud => {
                return Err(ReadyStateStateError::Invalid(
                    "cloud Ready-State backend is unsupported".to_string(),
                ));
            }
        };
    let capabilities = backend.probe();
    if !capabilities.available {
        return Err(ReadyStateStateError::Invalid(format!(
            "Ready-State backend {} is unavailable: {}",
            backend.id(),
            capabilities
                .reason
                .unwrap_or_else(|| "no reason reported".to_string())
        )));
    }
    verify_accepted_restore_candidate(
        backend.as_ref(),
        &state.legacy_manifest,
        &state.snapshot_manifest,
        &state.artifact_envelope,
    )?;
    Ok(backend)
}

pub fn verify_accepted_restore_candidate(
    backend: &dyn SnapshotBackend,
    legacy: &ReadyStateManifest,
    snapshot: &SnapshotManifestV1,
    envelope: &ArtifactEnvelopeV1,
) -> Result<(), ReadyStateStateError> {
    envelope
        .verify(legacy, snapshot)
        .map_err(|error| ReadyStateStateError::Invalid(error.to_string()))?;
    if legacy.execution_id.as_deref() != Some(snapshot.execution_id.as_str()) {
        return Err(ReadyStateStateError::Invalid(
            "legacy/v1 Snapshot execution_id mismatch".to_string(),
        ));
    }
    let contract = backend
        .snapshot_compatibility_contract()
        .map_err(|error| ReadyStateStateError::Invalid(error.to_string()))?;
    let host = exact_host_restore_capability(&contract);
    if !snapshot.compatibility_contract.is_satisfied_by(&host) {
        return Err(ReadyStateStateError::Invalid(
            "restore backend does not satisfy Snapshot compatibility".to_string(),
        ));
    }
    Ok(())
}

pub struct ReadyStatePortableExportPolicy {
    state: StateRef,
    required: BTreeMap<ContentRef, u64>,
}

impl ReadyStatePortableExportPolicy {
    pub fn new(object: &ReadyStateStateObjectV1) -> Result<Self, ReadyStateStateError> {
        let primary = object.canonical_bytes()?;
        Ok(Self {
            state: StateRef {
                state_type: ready_state_type()?,
                state_ref: content_ref_for_bytes(&primary)?,
            },
            required: required_chunks(&object.legacy_manifest)?,
        })
    }
}

impl PortableExportPolicy for ReadyStatePortableExportPolicy {
    fn inspect_descriptor(
        &mut self,
        descriptor: &CapsuleDescriptor,
    ) -> Result<(), PortableExportError> {
        if descriptor.base_state != self.state {
            return Err(PortableExportError::Rejected(
                "descriptor does not name the accepted Ready-State object".to_string(),
            ));
        }
        if !descriptor.connectors.is_empty() {
            return Err(PortableExportError::Rejected(
                "Ready-State F1 does not support Connectors".to_string(),
            ));
        }
        Ok(())
    }

    fn inspect_record(&mut self, _record: &IoRecord) -> Result<(), PortableExportError> {
        Err(PortableExportError::Rejected(
            "Ready-State F1 does not support Records".to_string(),
        ))
    }

    fn inspect_object(
        &mut self,
        metadata: &ObjectMetadata,
        roles: &[PortableObjectRole],
        _reader: &mut dyn Read,
    ) -> Result<(), PortableExportError> {
        let expected = if metadata.reference == self.state.state_ref {
            roles
                == [PortableObjectRole::BaseState {
                    state_type: self.state.state_type.clone(),
                }]
        } else {
            self.required.contains_key(&metadata.reference)
                && roles
                    == [PortableObjectRole::StateAdapterObject {
                        state_type: self.state.state_type.clone(),
                    }]
        };
        if !expected {
            return Err(PortableExportError::Rejected(format!(
                "unexpected Ready-State object role for {}",
                metadata.reference
            )));
        }
        Ok(())
    }
}

fn required_chunks(
    manifest: &ReadyStateManifest,
) -> Result<BTreeMap<ContentRef, u64>, ReadyStateStateError> {
    let mut required = BTreeMap::new();
    for (_, blob) in manifest.layers.iter() {
        validate_blob_manifest(blob)?;
        for chunk in &blob.chunks {
            let reference = ContentRef::parse(chunk.hash.as_str())
                .map_err(|error| ReadyStateStateError::Invalid(error.to_string()))?;
            if let Some(previous) = required.insert(reference.clone(), chunk.length)
                && previous != chunk.length
            {
                return Err(ReadyStateStateError::Invalid(format!(
                    "chunk {reference} has conflicting declared lengths"
                )));
            }
        }
    }
    Ok(required)
}

fn validate_backend_agreement(state: &ReadyStateStateObjectV1) -> Result<(), ReadyStateStateError> {
    let expected = match state.snapshot_manifest.compatibility_contract.backend {
        SnapshotBackendKind::Firecracker => "firecracker",
        SnapshotBackendKind::Qemu => "qemu",
        SnapshotBackendKind::Kata => "kata",
        SnapshotBackendKind::Fake => "fake",
        SnapshotBackendKind::Cloud => "cloud",
    };
    if state.legacy_manifest.snapshot_backend.kind != expected {
        return Err(ReadyStateStateError::Invalid(format!(
            "legacy backend {} differs from Snapshot compatibility backend {expected}",
            state.legacy_manifest.snapshot_backend.kind
        )));
    }
    Ok(())
}

pub fn exact_host_restore_capability(
    contract: &capsule::snapshot_manifest::SnapshotCompatibilityContractV1,
) -> HostRestoreCapabilityV1 {
    use capsule::snapshot_manifest::CapturePolicyV1;
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

fn ready_state_type() -> Result<StateTypeId, ReadyStateStateError> {
    StateTypeId::parse(READY_STATE_STATE_TYPE)
        .map_err(|error| ReadyStateStateError::Invalid(error.to_string()))
}

fn content_ref_for_bytes(bytes: &[u8]) -> Result<ContentRef, ReadyStateStateError> {
    ContentRef::parse(format!("blake3:{}", blake3::hash(bytes).to_hex()))
        .map_err(|error| ReadyStateStateError::Invalid(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule::execution_contract::ExecutionId;
    use capsule::protocol_bundle::{StreamingBundleReader, StreamingBundleWriter};
    use capsule_protocol::{CURRENT_SCHEMA_VERSION, CapsuleDescriptor};
    use capsulefs::CasStore;

    use crate::{
        ArtifactEnvelopeV1, BuildLayers, BuildReadyStateInput, FakeSnapshotBackend,
        RestoreContract, SanitizerContract, SnapshotBackend,
    };

    fn accepted_fixture(root: &Path) -> (CasStore, ReadyStateStateObjectV1) {
        let store = CasStore::open(root.join("producer-cas")).unwrap();
        let backend = FakeSnapshotBackend::new();
        let execution = ExecutionId::new(format!("blake3:{}", "a".repeat(64))).unwrap();
        let receipt = backend
            .build_ready_state(BuildReadyStateInput {
                store: &store,
                capsule_manifest_hash: format!("blake3:{}", "b".repeat(64)),
                runner_class: None,
                surface_requirement: None,
                layers: BuildLayers {
                    rootfs: b"rootfs".to_vec(),
                    runtime: None,
                    dependency: None,
                    app: Some(b"app".to_vec()),
                    vmstate: vec![1; 32],
                    memory: vec![2; 128 * 1024],
                },
                restore_contract: RestoreContract::default(),
                sanitizer_contract: SanitizerContract::default(),
                declared_secret_markers: Vec::new(),
                execution_id: Some(execution.as_str().to_string()),
                supervisor: None,
            })
            .unwrap();
        let snapshot =
            crate::disposable_lifecycle::build_v1_candidate_manifest(&backend, execution, &receipt)
                .unwrap();
        let envelope = ArtifactEnvelopeV1::accepted(&receipt.manifest, &snapshot).unwrap();
        let object =
            ReadyStateStateObjectV1::accepted(receipt.manifest, snapshot, envelope).unwrap();
        (store, object)
    }

    #[test]
    fn accepted_ready_state_round_trips_through_bundle_into_empty_cas() {
        let root = tempfile::tempdir().unwrap();
        let (producer, object) = accepted_fixture(root.path());
        let export = export_ready_state(object.clone(), &producer).unwrap();
        let descriptor = CapsuleDescriptor {
            schema_version: CURRENT_SCHEMA_VERSION,
            base_state: export.state.clone(),
            connectors: BTreeMap::new(),
        };
        let bundle = root.path().join("ready-state.capsule");
        let mut policy = ReadyStatePortableExportPolicy::new(&object).unwrap();
        StreamingBundleWriter::write_with_state_roles(
            &bundle,
            &descriptor,
            std::iter::empty::<Result<IoRecord, ProtocolBundleError>>(),
            &export.objects,
            &export.adapter_roles,
            &mut policy,
        )
        .unwrap();
        let spool = StreamingBundleReader::read_into(&bundle, &root.path().join("spool")).unwrap();
        let imported = import_ready_state(
            &spool.descriptor.base_state,
            &spool.objects,
            &root.path().join("consumer-cas"),
        )
        .unwrap();
        assert_eq!(imported.legacy_manifest, object.legacy_manifest);
        for (_, manifest) in imported.legacy_manifest.layers.iter() {
            assert!(imported.cas_store.has_all_chunks(manifest));
        }
    }

    #[test]
    fn state_adapter_rejects_tamper_backend_mismatch_and_missing_chunk() {
        let root = tempfile::tempdir().unwrap();
        let (producer, object) = accepted_fixture(root.path());
        let mut mismatch = object.clone();
        mismatch.legacy_manifest.snapshot_backend.kind = "firecracker".to_string();
        assert!(mismatch.validate().is_err());
        let mut no_proof = object.clone();
        no_proof.legacy_manifest.no_secret_proof = None;
        assert!(no_proof.validate().is_err());
        let mut envelope_mismatch = object.clone();
        envelope_mismatch.artifact_envelope.legacy_manifest_id =
            format!("blake3:{}", "0".repeat(64));
        assert!(envelope_mismatch.validate().is_err());

        let export = export_ready_state(object.clone(), &producer).unwrap();
        let missing = export.adapter_roles.keys().next().unwrap().clone();
        let filtered = MissingObjectSource {
            inner: &export.objects,
            missing,
        };
        assert!(import_ready_state(&export.state, &filtered, &root.path().join("empty")).is_err());

        let corrupt_primary = CorruptObjectSource {
            inner: &export.objects,
            target: export.state.state_ref.clone(),
        };
        assert!(
            import_ready_state(
                &export.state,
                &corrupt_primary,
                &root.path().join("primary-tamper")
            )
            .is_err()
        );
        let corrupt_chunk = CorruptObjectSource {
            inner: &export.objects,
            target: export.adapter_roles.keys().next().unwrap().clone(),
        };
        assert!(
            import_ready_state(
                &export.state,
                &corrupt_chunk,
                &root.path().join("chunk-tamper")
            )
            .is_err()
        );
    }

    #[test]
    fn state_adapter_rejects_f1_unsupported_supervisor_state() {
        let root = tempfile::tempdir().unwrap();
        let (_, object) = accepted_fixture(root.path());
        let mut binding = object.clone();
        binding.legacy_manifest.supervisor_build = Some(crate::manifest::SupervisorBuildReceipt {
            binding_names: vec!["api_key".to_string()],
            page_hygiene_boot_args: false,
            placeholder_absent_from_seal: None,
            state_volumes: Vec::new(),
            state_owner_scope: None,
        });
        assert!(matches!(
            binding.validate(),
            Err(ReadyStateStateError::ReadyStateBindingsUnsupported)
        ));

        let mut durable = object.clone();
        durable.legacy_manifest.supervisor_build = Some(crate::manifest::SupervisorBuildReceipt {
            binding_names: Vec::new(),
            page_hygiene_boot_args: false,
            placeholder_absent_from_seal: None,
            state_volumes: vec![crate::state_volume::DurableVolumeSpec {
                state_name: "data".to_string(),
                size_mb: 64,
            }],
            state_owner_scope: Some("owner/capsule".to_string()),
        });
        assert!(matches!(
            durable.validate(),
            Err(ReadyStateStateError::ReadyStateDurableVolumesUnsupported)
        ));

        let mut orphan_scope = object;
        orphan_scope.legacy_manifest.supervisor_build =
            Some(crate::manifest::SupervisorBuildReceipt {
                binding_names: Vec::new(),
                page_hygiene_boot_args: false,
                placeholder_absent_from_seal: None,
                state_volumes: Vec::new(),
                state_owner_scope: Some("owner/capsule".to_string()),
            });
        assert!(matches!(
            orphan_scope.validate(),
            Err(ReadyStateStateError::Invalid(message))
                if message.contains("state_owner_scope")
        ));
    }

    #[test]
    fn state_adapter_rejects_oversized_primary_before_open() {
        let root = tempfile::tempdir().unwrap();
        let (producer, object) = accepted_fixture(root.path());
        let export = export_ready_state(object, &producer).unwrap();
        let oversized = OversizedPrimarySource {
            inner: &export.objects,
            primary: export.state.state_ref.clone(),
        };
        let error = import_ready_state(
            &export.state,
            &oversized,
            &root.path().join("oversized-primary"),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ReadyStateStateError::Invalid(message) if message.contains("16")
        ));
    }

    #[test]
    fn state_adapter_rejects_noncanonical_primary_json() {
        let root = tempfile::tempdir().unwrap();
        let (producer, object) = accepted_fixture(root.path());
        let export = export_ready_state(object.clone(), &producer).unwrap();
        let replacement = serde_json::to_vec_pretty(&object).unwrap();
        let replacement_ref = content_ref_for_bytes(&replacement).unwrap();
        let state = StateRef {
            state_type: export.state.state_type.clone(),
            state_ref: replacement_ref.clone(),
        };
        let source = ReplacedPrimarySource {
            inner: &export.objects,
            original: export.state.state_ref,
            replacement_ref,
            replacement,
        };
        let error = import_ready_state(&state, &source, &root.path().join("noncanonical-primary"))
            .unwrap_err();
        assert!(matches!(
            error,
            ReadyStateStateError::Invalid(message) if message.contains("JCS canonical")
        ));
    }

    struct MissingObjectSource<'a> {
        inner: &'a dyn ObjectSource,
        missing: ContentRef,
    }

    impl ObjectSource for MissingObjectSource<'_> {
        fn index(&self) -> Result<BTreeMap<ContentRef, ObjectMetadata>, ProtocolBundleError> {
            let mut index = self.inner.index()?;
            index.remove(&self.missing);
            Ok(index)
        }

        fn open(
            &self,
            reference: &ContentRef,
        ) -> Result<Box<dyn Read + Send>, ProtocolBundleError> {
            self.inner.open(reference)
        }
    }

    struct CorruptObjectSource<'a> {
        inner: &'a dyn ObjectSource,
        target: ContentRef,
    }

    struct OversizedPrimarySource<'a> {
        inner: &'a dyn ObjectSource,
        primary: ContentRef,
    }

    impl ObjectSource for OversizedPrimarySource<'_> {
        fn index(&self) -> Result<BTreeMap<ContentRef, ObjectMetadata>, ProtocolBundleError> {
            let mut index = self.inner.index()?;
            index.get_mut(&self.primary).unwrap().size = READY_STATE_PRIMARY_OBJECT_MAX_BYTES + 1;
            Ok(index)
        }

        fn open(
            &self,
            _reference: &ContentRef,
        ) -> Result<Box<dyn Read + Send>, ProtocolBundleError> {
            panic!("oversized primary must be rejected before object open")
        }
    }

    struct ReplacedPrimarySource<'a> {
        inner: &'a dyn ObjectSource,
        original: ContentRef,
        replacement_ref: ContentRef,
        replacement: Vec<u8>,
    }

    impl ObjectSource for ReplacedPrimarySource<'_> {
        fn index(&self) -> Result<BTreeMap<ContentRef, ObjectMetadata>, ProtocolBundleError> {
            let mut index = self.inner.index()?;
            index.remove(&self.original);
            index.insert(
                self.replacement_ref.clone(),
                ObjectMetadata {
                    reference: self.replacement_ref.clone(),
                    size: self.replacement.len() as u64,
                },
            );
            Ok(index)
        }

        fn open(
            &self,
            reference: &ContentRef,
        ) -> Result<Box<dyn Read + Send>, ProtocolBundleError> {
            if reference == &self.replacement_ref {
                return Ok(Box::new(Cursor::new(self.replacement.clone())));
            }
            self.inner.open(reference)
        }
    }

    impl ObjectSource for CorruptObjectSource<'_> {
        fn index(&self) -> Result<BTreeMap<ContentRef, ObjectMetadata>, ProtocolBundleError> {
            self.inner.index()
        }

        fn open(
            &self,
            reference: &ContentRef,
        ) -> Result<Box<dyn Read + Send>, ProtocolBundleError> {
            if reference == &self.target {
                let size = self.inner.index()?[reference].size as usize;
                return Ok(Box::new(Cursor::new(vec![0_u8; size])));
            }
            self.inner.open(reference)
        }
    }
}
