//! Snapshot materialization provider.
//!
//! A snapshot is physical evidence that a provider can realize a computation;
//! it never changes or replaces the computation's identity.

#![forbid(unsafe_code)]

use std::path::Path;

use ato_computation::{ComputationRef, ContentRef};
use ato_objects::{ObjectError, ObjectResolver, ObjectStore, read_exact_object};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SNAPSHOT_MATERIALIZATION_VERSION: u32 = 1;
const MAX_MATERIALIZATION_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealizationContract {
    pub provider: String,
    pub architecture: String,
    pub operating_system: String,
    pub compatibility: String,
}

impl RealizationContract {
    pub fn host(provider: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
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
    #[error("snapshot realization contract does not match this host")]
    IncompatibleHost,
    #[error("invalid snapshot reference: {0}")]
    InvalidReference(String),
    #[error("snapshot artifact contains a likely plaintext secret")]
    PlaintextSecret,
    #[error("snapshot artifact I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

pub fn capture(
    computation: &ComputationRef,
    contract: RealizationContract,
    artifact_paths: &[impl AsRef<Path>],
    objects: &dyn ObjectStore,
) -> Result<MaterializationRef, SnapshotError> {
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

pub fn restore(
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
    ComputationRef::parse(materialization.computation)
        .map_err(|error| SnapshotError::InvalidReference(error.to_string()))
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
    use ato_objects::{MemoryObjectStore, ObjectStore};

    use super::*;

    fn computation(store: &MemoryObjectStore) -> ComputationRef {
        let content = store.put(b"computation").unwrap();
        ComputationRef::parse(content.to_string()).unwrap()
    }

    #[test]
    fn snapshot_roundtrip_preserves_computation_identity() {
        let store = MemoryObjectStore::default();
        let computation = computation(&store);
        let directory = tempfile::tempdir().unwrap();
        let artifact = directory.path().join("memory");
        std::fs::write(&artifact, b"physical state").unwrap();
        let contract = RealizationContract::host("test");

        let materialization = capture(&computation, contract.clone(), &[artifact], &store).unwrap();

        assert_eq!(
            restore(&materialization, &contract, &store).unwrap(),
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
            capture(
                &computation,
                RealizationContract::host("test"),
                &[artifact],
                &store
            ),
            Err(SnapshotError::PlaintextSecret)
        ));
    }
}
