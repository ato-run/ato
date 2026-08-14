//! Verify-only snapshot materializer.
//!
//! A snapshot is physical evidence that a materializer can realize a computation;
//! it never changes or replaces the computation's identity.

#![forbid(unsafe_code)]

use std::path::Path;

use ato_computation::{ComputationRef, ContentRef};
use ato_materializer_api::{
    Compatibility, Materializer, MaterializerContext, MaterializerError, RestoreCapability,
};
use ato_objects::{
    BundleError, MaterializationReferences, ObjectError, ObjectLink, ObjectResolver, ObjectStore,
    RetainedObjectReferences, read_exact_object, resolve_computation,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SNAPSHOT_MATERIALIZATION_VERSION: u32 = 1;
pub const SNAPSHOT_MATERIALIZER_ID: &str = "ato.snapshot@1";
const MAX_MATERIALIZATION_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealizationContract {
    pub materializer: String,
    pub architecture: String,
    pub operating_system: String,
    pub compatibility: String,
}

impl RealizationContract {
    pub fn host(materializer: impl Into<String>) -> Self {
        Self {
            materializer: materializer.into(),
            architecture: std::env::consts::ARCH.to_owned(),
            operating_system: std::env::consts::OS.to_owned(),
            compatibility: "exact-host-v1".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotMaterialization {
    pub version: u32,
    pub computation: String,
    pub contract: RealizationContract,
    pub artifacts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializationRef(ContentRef);

impl MaterializationRef {
    pub fn parse(value: impl Into<String>) -> Result<Self, SnapshotError> {
        ContentRef::parse(value.into())
            .map(Self)
            .map_err(|error| SnapshotError::InvalidReference(error.to_string()))
    }

    pub fn content_ref(&self) -> &ContentRef {
        &self.0
    }
}

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error(transparent)]
    Objects(#[from] ObjectError),
    #[error("snapshot materialization JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported snapshot materialization version {0}")]
    Version(u32),
    #[error("snapshot compatibility contract does not match this host")]
    IncompatibleHost,
    #[error("invalid snapshot reference: {0}")]
    InvalidReference(String),
    #[error("snapshot artifact contains a likely plaintext secret")]
    PlaintextSecret,
    #[error("snapshot artifact I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

pub fn register_materialization(
    computation: &ComputationRef,
    contract: RealizationContract,
    artifact_paths: &[impl AsRef<Path>],
    objects: &dyn ObjectStore,
) -> Result<MaterializationRef, SnapshotError> {
    resolve_computation(objects, computation)?;
    let mut artifacts = Vec::with_capacity(artifact_paths.len());
    for path in artifact_paths {
        let bytes = std::fs::read(path)?;
        reject_plaintext_secret(&bytes)?;
        artifacts.push(objects.put(&bytes)?.to_string());
    }
    let materialization = SnapshotMaterialization {
        version: SNAPSHOT_MATERIALIZATION_VERSION,
        computation: computation.to_string(),
        contract,
        artifacts,
    };
    let bytes = serde_jcs::to_vec(&materialization)?;
    Ok(MaterializationRef(objects.put(&bytes)?))
}

pub fn verify_materialization(
    reference: &MaterializationRef,
    expected: &RealizationContract,
    objects: &dyn ObjectResolver,
) -> Result<ComputationRef, SnapshotError> {
    let metadata = objects.metadata(reference.content_ref())?;
    let bytes = read_exact_object(
        objects,
        reference.content_ref(),
        metadata.size,
        MAX_MATERIALIZATION_BYTES,
    )?;
    let materialization: SnapshotMaterialization = serde_json::from_slice(&bytes)?;
    if serde_jcs::to_vec(&materialization)? != bytes {
        return Err(SnapshotError::Json(serde_json::Error::io(
            std::io::Error::new(std::io::ErrorKind::InvalidData, "non-canonical snapshot"),
        )));
    }
    if materialization.version != SNAPSHOT_MATERIALIZATION_VERSION {
        return Err(SnapshotError::Version(materialization.version));
    }
    if &materialization.contract != expected {
        return Err(SnapshotError::IncompatibleHost);
    }
    for artifact in materialization.artifacts {
        let reference = ContentRef::parse(artifact)
            .map_err(|error| SnapshotError::InvalidReference(error.to_string()))?;
        let metadata = objects.metadata(&reference)?;
        read_exact_object(
            objects,
            &reference,
            metadata.size,
            MAX_MATERIALIZATION_BYTES,
        )?;
    }
    let computation = ComputationRef::parse(materialization.computation)
        .map_err(|error| SnapshotError::InvalidReference(error.to_string()))?;
    resolve_computation(objects, &computation)?;
    Ok(computation)
}

/// Snapshot-owned object graph discovery for generic CAS retention.
#[derive(Default)]
pub struct SnapshotReferences;

impl RetainedObjectReferences for SnapshotReferences {
    fn outgoing(
        &self,
        root: &ContentRef,
        objects: &dyn ObjectResolver,
    ) -> Result<Vec<ContentRef>, ObjectError> {
        let metadata = objects.metadata(root)?;
        let bytes = read_exact_object(objects, root, metadata.size, MAX_MATERIALIZATION_BYTES)?;
        let materialization: SnapshotMaterialization = serde_json::from_slice(&bytes)
            .map_err(|error| ObjectError::Storage(error.to_string()))?;
        materialization
            .artifacts
            .into_iter()
            .map(|reference| {
                ContentRef::parse(reference)
                    .map_err(|error| ObjectError::Storage(error.to_string()))
            })
            .collect()
    }
}

#[derive(Default)]
pub struct SnapshotMaterializer;

impl Materializer for SnapshotMaterializer {
    fn id(&self) -> &str {
        SNAPSHOT_MATERIALIZER_ID
    }

    fn restore_capability(&self) -> RestoreCapability {
        RestoreCapability::VerifyOnly
    }

    fn encode(
        &self,
        target: &ComputationRef,
        context: &MaterializerContext<'_>,
    ) -> Result<ContentRef, MaterializerError> {
        let artifact = encode_workspace_artifact(context.workspace, context.workspace_policy)
            .map_err(|error| MaterializerError::Operation(error.to_string()))?;
        reject_plaintext_secret(&artifact)
            .map_err(|error| MaterializerError::Operation(error.to_string()))?;
        let artifact = context.objects.put(&artifact)?;
        let descriptor = SnapshotMaterialization {
            version: SNAPSHOT_MATERIALIZATION_VERSION,
            computation: target.to_string(),
            contract: RealizationContract::host(SNAPSHOT_MATERIALIZER_ID),
            artifacts: vec![artifact.to_string()],
        };
        Ok(context.objects.put(&serde_jcs::to_vec(&descriptor)?)?)
    }

    fn verify(
        &self,
        descriptor: &ContentRef,
        context: &MaterializerContext<'_>,
    ) -> Result<ComputationRef, MaterializerError> {
        verify_materialization(
            &MaterializationRef(descriptor.clone()),
            &RealizationContract::host(SNAPSHOT_MATERIALIZER_ID),
            context.objects,
        )
        .map_err(|error| MaterializerError::Operation(error.to_string()))
    }

    fn compatibility(
        &self,
        descriptor: &ContentRef,
        context: &MaterializerContext<'_>,
    ) -> Compatibility {
        match self.verify(descriptor, context) {
            Ok(_) => Compatibility::Compatible,
            Err(_) => Compatibility::Incompatible,
        }
    }
}

fn encode_workspace_artifact(
    root: &Path,
    policy: &ato_adapter_api::WorkspaceCapturePolicy,
) -> Result<Vec<u8>, std::io::Error> {
    fn visit(
        root: &Path,
        directory: &Path,
        policy: &ato_adapter_api::WorkspaceCapturePolicy,
        entries: &mut Vec<(String, Vec<u8>)>,
    ) -> std::io::Result<()> {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("snapshot cannot encode symlink {}", path.display()),
                ));
            }
            if kind.is_dir() {
                let relative = path.strip_prefix(root).map_err(std::io::Error::other)?;
                if policy.descends_into(relative) {
                    visit(root, &path, policy, entries)?;
                }
            } else if kind.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .map_err(std::io::Error::other)?
                    .to_path_buf();
                if !policy.captures(&relative) {
                    continue;
                }
                let relative = relative
                    .to_str()
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "non-UTF-8 snapshot path",
                        )
                    })?
                    .replace(std::path::MAIN_SEPARATOR, "/");
                entries.push((relative, std::fs::read(path)?));
            }
        }
        Ok(())
    }

    let mut entries = Vec::new();
    visit(root, root, policy, &mut entries)?;
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut bytes = b"ATO-WORKSPACE-SNAPSHOT\0\x01".to_vec();
    for (path, content) in entries {
        let path = path.as_bytes();
        bytes.extend_from_slice(&(path.len() as u64).to_be_bytes());
        bytes.extend_from_slice(&(content.len() as u64).to_be_bytes());
        bytes.extend_from_slice(path);
        bytes.extend_from_slice(&content);
    }
    Ok(bytes)
}

impl MaterializationReferences for SnapshotReferences {
    fn materializer_id(&self) -> &str {
        SNAPSHOT_MATERIALIZER_ID
    }

    fn outgoing(
        &self,
        root: &ContentRef,
        objects: &dyn ObjectResolver,
    ) -> Result<Vec<ObjectLink>, BundleError> {
        let metadata = objects.metadata(root)?;
        let bytes = read_exact_object(objects, root, metadata.size, MAX_MATERIALIZATION_BYTES)?;
        let materialization: SnapshotMaterialization = serde_json::from_slice(&bytes)?;
        let mut links = vec![ObjectLink::Computation(
            ComputationRef::parse(materialization.computation).map_err(|error| {
                BundleError::InvalidReference {
                    value: "snapshot computation".to_owned(),
                    reason: error.to_string(),
                }
            })?,
        )];
        for artifact in materialization.artifacts {
            links.push(ObjectLink::Content(ContentRef::parse(artifact).map_err(
                |error| BundleError::InvalidReference {
                    value: "snapshot artifact".to_owned(),
                    reason: error.to_string(),
                },
            )?));
        }
        Ok(links)
    }
}

fn reject_plaintext_secret(bytes: &[u8]) -> Result<(), SnapshotError> {
    let lower = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    if ["private_key=", "secret_key=", "authorization: bearer "]
        .iter()
        .any(|marker| lower.contains(marker))
    {
        return Err(SnapshotError::PlaintextSecret);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use ato_computation::{
        ComputationObject, SemanticsId, computation_ref, encode_computation_object,
    };
    use ato_objects::{
        FsObjectStore, MemoryObjectStore, ObjectStore, ReferenceRegistry, RetainedObjectRoot,
    };

    use super::*;

    fn computation(store: &MemoryObjectStore) -> ComputationRef {
        let residual = store.put(b"residual").unwrap();
        let object = ComputationObject {
            semantics: SemanticsId::parse("example.snapshot@1").unwrap(),
            boundary: BTreeMap::new(),
            residual,
        };
        let reference = computation_ref(&object).unwrap();
        store
            .insert(
                &reference.clone().content_ref().clone(),
                &encode_computation_object(&object).unwrap(),
            )
            .unwrap();
        reference
    }

    #[test]
    fn snapshot_roundtrip_preserves_computation_identity() {
        let store = MemoryObjectStore::default();
        let computation = computation(&store);
        let directory = tempfile::tempdir().unwrap();
        let artifact = directory.path().join("memory");
        std::fs::write(&artifact, b"physical state").unwrap();
        let contract = RealizationContract::host("test");

        let materialization =
            register_materialization(&computation, contract.clone(), &[artifact], &store).unwrap();

        assert_eq!(
            verify_materialization(&materialization, &contract, &store).unwrap(),
            computation
        );
    }

    #[test]
    fn incompatible_hosts_and_plaintext_secrets_fail_closed() {
        let store = MemoryObjectStore::default();
        let computation = computation(&store);
        let directory = tempfile::tempdir().unwrap();
        let artifact = directory.path().join("memory");
        std::fs::write(&artifact, b"SECRET_KEY=exposed").unwrap();
        assert!(matches!(
            register_materialization(
                &computation,
                RealizationContract::host("test"),
                &[artifact],
                &store
            ),
            Err(SnapshotError::PlaintextSecret)
        ));
    }

    #[test]
    fn capture_rejects_an_unresolvable_computation() {
        let store = MemoryObjectStore::default();
        let computation = ComputationRef::parse(format!("blake3:{}", "ab".repeat(32))).unwrap();
        assert!(matches!(
            register_materialization(
                &computation,
                RealizationContract::host("test"),
                &[] as &[&Path],
                &store,
            ),
            Err(SnapshotError::Objects(ObjectError::NotFound(_)))
        ));
    }

    #[test]
    fn retained_materialization_keeps_artifacts_during_gc() {
        let directory = tempfile::tempdir().unwrap();
        let store = FsObjectStore::open(directory.path()).unwrap();
        let residual = store.put(b"residual").unwrap();
        let object = ComputationObject {
            semantics: SemanticsId::parse("example.snapshot@1").unwrap(),
            boundary: BTreeMap::new(),
            residual,
        };
        let computation = computation_ref(&object).unwrap();
        store
            .insert(
                computation.content_ref(),
                &encode_computation_object(&object).unwrap(),
            )
            .unwrap();
        let artifact_file = directory.path().join("artifact.bin");
        std::fs::write(&artifact_file, b"physical artifact").unwrap();
        let materialization = register_materialization(
            &computation,
            RealizationContract::host("test"),
            &[artifact_file],
            &store,
        )
        .unwrap();
        let verified =
            verify_materialization(&materialization, &RealizationContract::host("test"), &store)
                .unwrap();
        assert_eq!(verified, computation);
        let snapshot_references = SnapshotReferences;

        let report = store
            .gc(
                &[],
                &[RetainedObjectRoot {
                    reference: materialization.content_ref(),
                    references: &snapshot_references,
                }],
                &ReferenceRegistry::default(),
            )
            .unwrap();

        assert_eq!(report.retained, 2);
        verify_materialization(&materialization, &RealizationContract::host("test"), &store)
            .unwrap_err();
        let artifact_reference = RetainedObjectReferences::outgoing(
            &snapshot_references,
            materialization.content_ref(),
            &store,
        )
        .unwrap()
        .pop()
        .unwrap();
        assert!(store.metadata(&artifact_reference).is_ok());
    }
}
