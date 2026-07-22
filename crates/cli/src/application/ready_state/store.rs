//! On-disk location + persistence of the sealed Ready-State artifact.
//!
//! Legacy artifacts live under `<root>/ready-state/<capsule_manifest_hash>/`.
//! Capsule v1 artifacts live under
//! `<root>/snapshots/<execution_id>/<snapshot_id>/`, which retains every resolved
//! target and every immutable cache independently. Identity components are
//! sanitized so untrusted values cannot escape the root.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use capsule::execution_contract::ExecutionId;
use capsulefs::CasStore;
use snapshot::{
    ARTIFACT_ENVELOPE_V1_FILENAME, ArtifactEnvelopeV1, ReadyStateManifest,
    SNAPSHOT_MANIFEST_V1_FILENAME, SnapshotManifestV1,
};

const ACCEPTANCE_CATALOG_SCHEMA: &str = "ato.snapshot-acceptance-catalog/v1";
const ACCEPTANCE_CATALOG_FILENAME: &str = "acceptance-catalog-v1.json";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptanceCatalogV1 {
    schema: String,
    execution_id: ExecutionId,
    entries: BTreeMap<String, AcceptedCatalogEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptedCatalogEntry {
    envelope_id: String,
}

/// Sanitize a `blake3:<hex>`-style id into one safe path component (hex/dash
/// only); anything else collapses to `_`.
fn safe_component(id: &str) -> String {
    let s: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() { "_".to_string() } else { s }
}

/// Directory holding one sealed artifact.
pub(crate) fn artifact_dir(root: &Path, capsule_manifest_hash: &str) -> PathBuf {
    root.join("ready-state")
        .join(safe_component(capsule_manifest_hash))
}

/// Open (creating if needed) the CapsuleFS store for an artifact.
pub(crate) fn open_store(root: &Path, capsule_manifest_hash: &str) -> Result<CasStore> {
    let dir = artifact_dir(root, capsule_manifest_hash).join("cas");
    CasStore::open(&dir).with_context(|| format!("open CapsuleFS store at {}", dir.display()))
}

fn manifest_path(root: &Path, capsule_manifest_hash: &str) -> PathBuf {
    artifact_dir(root, capsule_manifest_hash).join("manifest.json")
}

/// Persist a sealed manifest as JSON next to its CAS store.
pub(crate) fn save_manifest(root: &Path, manifest: &ReadyStateManifest) -> Result<PathBuf> {
    let path = manifest_path(root, &manifest.capsule_manifest_hash);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(manifest)?;
    std::fs::write(&path, json).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

/// Load a previously sealed manifest, if present.
pub(crate) fn load_manifest(
    root: &Path,
    capsule_manifest_hash: &str,
) -> Result<Option<ReadyStateManifest>> {
    let path = manifest_path(root, capsule_manifest_hash);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let manifest =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    Ok(Some(manifest))
}

#[derive(Debug, Clone)]
pub(crate) struct StoredSnapshotV1 {
    pub artifact_dir: PathBuf,
    pub legacy_manifest: ReadyStateManifest,
    pub snapshot_manifest: SnapshotManifestV1,
    pub envelope: ArtifactEnvelopeV1,
}

pub(crate) struct V1StagingArtifact {
    dir: PathBuf,
    committed: bool,
}

impl V1StagingArtifact {
    pub(crate) fn create(root: &Path, execution_id: &ExecutionId) -> Result<Self> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before UNIX epoch")?
            .as_nanos();
        let dir = root.join("snapshots").join(".staging").join(format!(
            "{}-{}-{nonce}",
            safe_component(execution_id.as_str()),
            std::process::id()
        ));
        std::fs::create_dir_all(dir.join("cas"))?;
        Ok(Self {
            dir,
            committed: false,
        })
    }

    pub(crate) fn artifact_dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn open_store(&self) -> Result<CasStore> {
        let dir = self.dir.join("cas");
        CasStore::open(&dir).with_context(|| format!("open CapsuleFS store at {}", dir.display()))
    }

    pub(crate) fn commit(
        mut self,
        root: &Path,
        legacy: &ReadyStateManifest,
        snapshot: &SnapshotManifestV1,
        envelope: &ArtifactEnvelopeV1,
    ) -> Result<PathBuf> {
        envelope
            .verify(legacy, snapshot)
            .map_err(anyhow::Error::new)?;
        write_json(&self.dir.join("manifest.json"), legacy)?;
        write_json(&self.dir.join(SNAPSHOT_MANIFEST_V1_FILENAME), snapshot)?;
        write_json(&self.dir.join(ARTIFACT_ENVELOPE_V1_FILENAME), envelope)?;
        let final_dir = snapshot_dir(root, &snapshot.execution_id, &snapshot.snapshot_id);
        if final_dir.exists() {
            let existing = load_v1_snapshot_with_expected_envelope(
                root,
                &snapshot.execution_id,
                &snapshot.snapshot_id,
                &envelope.envelope_id,
            )?
            .ok_or_else(|| anyhow::anyhow!("existing Snapshot v1 directory is incomplete"))?;
            if existing.legacy_manifest != *legacy
                || existing.snapshot_manifest != *snapshot
                || existing.envelope != *envelope
            {
                anyhow::bail!("immutable Snapshot v1 directory already contains different data");
            }
            record_accepted_envelope(root, snapshot, envelope)?;
            return Ok(final_dir);
        }
        if let Some(parent) = final_dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&self.dir, &final_dir).with_context(|| {
            format!(
                "publish Snapshot v1 {} to {}",
                snapshot.snapshot_id,
                final_dir.display()
            )
        })?;
        self.committed = true;
        record_accepted_envelope(root, snapshot, envelope)?;
        Ok(final_dir)
    }
}

impl Drop for V1StagingArtifact {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

pub(crate) fn snapshot_dir(root: &Path, execution_id: &ExecutionId, snapshot_id: &str) -> PathBuf {
    root.join("snapshots")
        .join(safe_component(execution_id.as_str()))
        .join(safe_component(snapshot_id))
}

pub(crate) fn open_store_at_artifact_dir(artifact_dir: &Path) -> Result<CasStore> {
    let dir = artifact_dir.join("cas");
    CasStore::open(&dir).with_context(|| format!("open CapsuleFS store at {}", dir.display()))
}

#[cfg(test)]
pub(crate) fn load_v1_snapshot(
    root: &Path,
    execution_id: &ExecutionId,
    snapshot_id: &str,
) -> Result<Option<StoredSnapshotV1>> {
    let catalog = load_acceptance_catalog(root, execution_id)?;
    let Some(expected) = catalog.entries.get(snapshot_id) else {
        return Ok(None);
    };
    load_v1_snapshot_with_expected_envelope(root, execution_id, snapshot_id, &expected.envelope_id)
}

fn load_v1_snapshot_with_expected_envelope(
    root: &Path,
    execution_id: &ExecutionId,
    snapshot_id: &str,
    expected_envelope_id: &str,
) -> Result<Option<StoredSnapshotV1>> {
    let dir = snapshot_dir(root, execution_id, snapshot_id);
    if !dir.is_dir() {
        return Ok(None);
    }
    let legacy_manifest: ReadyStateManifest = read_json(&dir.join("manifest.json"))?;
    let snapshot_manifest: SnapshotManifestV1 =
        read_json(&dir.join(SNAPSHOT_MANIFEST_V1_FILENAME))?;
    let envelope: ArtifactEnvelopeV1 = read_json(&dir.join(ARTIFACT_ENVELOPE_V1_FILENAME))?;
    if &snapshot_manifest.execution_id != execution_id
        || snapshot_manifest.snapshot_id != snapshot_id
    {
        anyhow::bail!("Snapshot v1 path identity does not match its authenticated metadata");
    }
    envelope
        .verify(&legacy_manifest, &snapshot_manifest)
        .map_err(anyhow::Error::new)?;
    if envelope.envelope_id != expected_envelope_id {
        anyhow::bail!("Snapshot v1 envelope does not match the trusted acceptance catalog");
    }
    Ok(Some(StoredSnapshotV1 {
        artifact_dir: dir,
        legacy_manifest,
        snapshot_manifest,
        envelope,
    }))
}

pub(crate) fn load_v1_snapshots(
    root: &Path,
    execution_id: &ExecutionId,
) -> Result<Vec<StoredSnapshotV1>> {
    let catalog = load_acceptance_catalog(root, execution_id)?;
    let mut snapshots = Vec::new();
    for (snapshot_id, expected) in catalog.entries {
        if let Ok(Some(snapshot)) = load_v1_snapshot_with_expected_envelope(
            root,
            execution_id,
            &snapshot_id,
            &expected.envelope_id,
        ) {
            snapshots.push(snapshot);
        }
    }
    Ok(snapshots)
}

fn acceptance_catalog_path(root: &Path, execution_id: &ExecutionId) -> PathBuf {
    root.join("snapshots")
        .join(safe_component(execution_id.as_str()))
        .join(ACCEPTANCE_CATALOG_FILENAME)
}

fn load_acceptance_catalog(root: &Path, execution_id: &ExecutionId) -> Result<AcceptanceCatalogV1> {
    let path = acceptance_catalog_path(root, execution_id);
    if !path.exists() {
        return Ok(AcceptanceCatalogV1 {
            schema: ACCEPTANCE_CATALOG_SCHEMA.to_string(),
            execution_id: execution_id.clone(),
            entries: BTreeMap::new(),
        });
    }
    let catalog: AcceptanceCatalogV1 = read_json(&path)?;
    if catalog.schema != ACCEPTANCE_CATALOG_SCHEMA || &catalog.execution_id != execution_id {
        anyhow::bail!("invalid Snapshot v1 acceptance catalog identity");
    }
    Ok(catalog)
}

fn record_accepted_envelope(
    root: &Path,
    snapshot: &SnapshotManifestV1,
    envelope: &ArtifactEnvelopeV1,
) -> Result<()> {
    let mut catalog = load_acceptance_catalog(root, &snapshot.execution_id)?;
    let entry = AcceptedCatalogEntry {
        envelope_id: envelope.envelope_id.clone(),
    };
    if let Some(existing) = catalog.entries.get(&snapshot.snapshot_id)
        && existing != &entry
    {
        anyhow::bail!("trusted acceptance catalog already pins a different envelope");
    }
    catalog.entries.insert(snapshot.snapshot_id.clone(), entry);
    let path = acceptance_catalog_path(root, &snapshot.execution_id);
    let pending = path.with_extension(format!("pending-{}", std::process::id()));
    write_json(&pending, &catalog)?;
    std::fs::rename(&pending, &path)
        .with_context(|| format!("publish trusted acceptance catalog {}", path.display()))
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let json = serde_json::to_vec_pretty(value)?;
    std::fs::write(path, json).with_context(|| format!("write {}", path.display()))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_component_strips_path_separators() {
        assert_eq!(safe_component("blake3:../../etc"), "blake3_______etc");
        assert_eq!(safe_component("blake3:abcDEF123"), "blake3_abcDEF123");
        assert_eq!(safe_component(""), "_");
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path().join("seed")).unwrap();
        let rootfs = capsulefs::store_blob(
            &store,
            capsulefs::LayerKind::Rootfs,
            b"rootfs-bytes",
            capsulefs::ChunkingKind::ContentDefined,
        )
        .unwrap();
        let manifest = ReadyStateManifest {
            schema: snapshot::READY_STATE_SCHEMA.to_string(),
            capsule_manifest_hash: "blake3:deadbeef".to_string(),
            has_vsock: false,
            runner_class_id: None,
            execution_id: None,
            execution_identity_schema: None,
            surface_requirement: None,
            layers: snapshot::ReadyStateLayers {
                rootfs: Some(rootfs),
                ..Default::default()
            },
            hotset_profile: Default::default(),
            snapshot_backend: snapshot::SnapshotBackendInfo {
                kind: "fake".to_string(),
                version: "0.1.0".to_string(),
                snapshot_format_version: "fake-v1".to_string(),
                cpu_template: None,
            },
            restore_contract: Default::default(),
            sanitizer_contract: Default::default(),
            no_secret_proof: None,
            build_receipt_id: None,
            supervisor_build: None,
        };
        let root = dir.path();
        assert!(load_manifest(root, "blake3:deadbeef").unwrap().is_none());
        save_manifest(root, &manifest).unwrap();
        let back = load_manifest(root, "blake3:deadbeef").unwrap().unwrap();
        assert_eq!(back, manifest);
    }

    #[test]
    fn v1_store_retains_multiple_execution_ids_for_one_capsule_manifest() {
        use capsule::execution_contract::EXECUTION_CONTRACT_V1_SCHEMA;
        use snapshot::{
            BuildLayers, BuildReadyStateInput, FakeSnapshotBackend, RestoreContract,
            SanitizerContract, SnapshotBackend, migrate_legacy_manifest,
        };

        fn persist(root: &Path, execution_id: ExecutionId) -> (String, PathBuf) {
            let backend = FakeSnapshotBackend::new();
            let staging = V1StagingArtifact::create(root, &execution_id).unwrap();
            let store = staging.open_store().unwrap();
            let legacy = backend
                .build_ready_state(BuildReadyStateInput {
                    store: &store,
                    capsule_manifest_hash: format!("blake3:{}", "c".repeat(64)),
                    runner_class: Some(
                        capsule::foundation::install_lifecycle::RunnerClassFacts::from_host().id(),
                    ),
                    surface_requirement: None,
                    layers: BuildLayers {
                        rootfs: format!("rootfs-{execution_id}").into_bytes(),
                        runtime: None,
                        dependency: None,
                        app: None,
                        vmstate: vec![1; 64],
                        memory: vec![2; 4096],
                    },
                    restore_contract: RestoreContract::default(),
                    sanitizer_contract: SanitizerContract::default(),
                    declared_secret_markers: Vec::new(),
                    execution_id: Some(execution_id.to_string()),
                    execution_identity_schema: Some(EXECUTION_CONTRACT_V1_SCHEMA.to_string()),
                    supervisor: None,
                })
                .unwrap()
                .manifest;
            let sidecar = migrate_legacy_manifest(
                &legacy,
                execution_id,
                backend.snapshot_compatibility_contract().unwrap(),
            )
            .unwrap();
            let envelope = ArtifactEnvelopeV1::accepted(&legacy, &sidecar).unwrap();
            let snapshot_id = sidecar.snapshot_id.clone();
            let path = staging.commit(root, &legacy, &sidecar, &envelope).unwrap();
            (snapshot_id, path)
        }

        let root = tempfile::tempdir().unwrap();
        let first = ExecutionId::new(format!("blake3:{}", "1".repeat(64))).unwrap();
        let second = ExecutionId::new(format!("blake3:{}", "2".repeat(64))).unwrap();
        let (first_snapshot, first_path) = persist(root.path(), first.clone());
        let (second_snapshot, second_path) = persist(root.path(), second.clone());

        assert_ne!(first_path, second_path);
        assert!(first_path.is_dir());
        assert!(second_path.is_dir());
        assert!(
            load_v1_snapshot(root.path(), &first, &first_snapshot)
                .unwrap()
                .is_some()
        );
        assert!(
            load_v1_snapshot(root.path(), &second, &second_snapshot)
                .unwrap()
                .is_some()
        );
        assert_eq!(load_v1_snapshots(root.path(), &first).unwrap().len(), 1);
        assert_eq!(load_v1_snapshots(root.path(), &second).unwrap().len(), 1);
        assert!(
            load_manifest(root.path(), &format!("blake3:{}", "c".repeat(64)))
                .unwrap()
                .is_none(),
            "Capsule v1 must not overwrite the legacy manifest-keyed store"
        );
    }

    #[test]
    fn rehashed_acceptance_escalation_is_rejected_by_catalog_anchor() {
        use capsule::execution_contract::EXECUTION_CONTRACT_V1_SCHEMA;
        use snapshot::{
            ArtifactAcceptanceStatus, BuildLayers, BuildReadyStateInput, FakeSnapshotBackend,
            RestoreContract, SanitizerContract, SnapshotBackend, migrate_legacy_manifest,
        };

        let root = tempfile::tempdir().unwrap();
        let execution_id = ExecutionId::new(format!("blake3:{}", "3".repeat(64))).unwrap();
        let backend = FakeSnapshotBackend::new();
        let staging = V1StagingArtifact::create(root.path(), &execution_id).unwrap();
        let cas = staging.open_store().unwrap();
        let legacy = backend
            .build_ready_state(BuildReadyStateInput {
                store: &cas,
                capsule_manifest_hash: format!("blake3:{}", "c".repeat(64)),
                runner_class: None,
                surface_requirement: None,
                layers: BuildLayers {
                    rootfs: b"rootfs".to_vec(),
                    runtime: None,
                    dependency: None,
                    app: None,
                    vmstate: vec![1; 64],
                    memory: vec![2; 4096],
                },
                restore_contract: RestoreContract::default(),
                sanitizer_contract: SanitizerContract::default(),
                declared_secret_markers: Vec::new(),
                execution_id: Some(execution_id.to_string()),
                execution_identity_schema: Some(EXECUTION_CONTRACT_V1_SCHEMA.to_string()),
                supervisor: None,
            })
            .unwrap()
            .manifest;
        let snapshot = migrate_legacy_manifest(
            &legacy,
            execution_id.clone(),
            backend.snapshot_compatibility_contract().unwrap(),
        )
        .unwrap();
        let accepted = ArtifactEnvelopeV1::accepted(&legacy, &snapshot).unwrap();
        staging
            .commit(root.path(), &legacy, &snapshot, &accepted)
            .unwrap();

        // Model a trusted catalog that recorded the pre-acceptance envelope.
        // Replacing the artifact with a self-consistent accepted envelope and
        // recomputing its id must not change that external expectation.
        let mut quarantined = accepted.clone();
        quarantined.acceptance.status = ArtifactAcceptanceStatus::Quarantined;
        quarantined.envelope_id = quarantined.compute_envelope_id().unwrap();
        let mut catalog = load_acceptance_catalog(root.path(), &execution_id).unwrap();
        catalog.entries.insert(
            snapshot.snapshot_id.clone(),
            AcceptedCatalogEntry {
                envelope_id: quarantined.envelope_id,
            },
        );
        write_json(
            &acceptance_catalog_path(root.path(), &execution_id),
            &catalog,
        )
        .unwrap();

        let error =
            load_v1_snapshot(root.path(), &execution_id, &snapshot.snapshot_id).unwrap_err();
        assert!(error.to_string().contains("trusted acceptance catalog"));
    }
}
