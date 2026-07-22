//! On-disk location + persistence of the sealed Ready-State artifact.
//!
//! A sealed capsule lives under `<root>/ready-state/<capsule_manifest_hash>/`:
//! `manifest.json` (the [`snapshot::ReadyStateManifest`]) next to a `cas/`
//! CapsuleFS store holding the chunked layers. The hash is sanitized into a
//! single safe path component so an untrusted value cannot escape the root.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use capsulefs::CasStore;
use snapshot::{ReadyStateManifest, SNAPSHOT_MANIFEST_V1_FILENAME, SnapshotManifestV1};

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

fn v1_manifest_path(root: &Path, capsule_manifest_hash: &str) -> PathBuf {
    artifact_dir(root, capsule_manifest_hash).join(SNAPSHOT_MANIFEST_V1_FILENAME)
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

/// Persist the Capsule-v1 identity/compatibility manifest beside the legacy
/// Ready-State manifest. The two files reference the same immutable CAS layers;
/// legacy readers remain byte-compatible and v1 readers never reinterpret the
/// old wire schema.
pub(crate) fn save_v1_manifest(
    root: &Path,
    capsule_manifest_hash: &str,
    manifest: &SnapshotManifestV1,
) -> Result<PathBuf> {
    manifest.validate().map_err(anyhow::Error::new)?;
    let path = v1_manifest_path(root, capsule_manifest_hash);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(manifest)?;
    std::fs::write(&path, json).with_context(|| format!("write {}", path.display()))?;
    Ok(path)
}

pub(crate) fn load_v1_manifest(
    root: &Path,
    capsule_manifest_hash: &str,
) -> Result<Option<SnapshotManifestV1>> {
    let path = v1_manifest_path(root, capsule_manifest_hash);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let manifest: SnapshotManifestV1 =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
    manifest.validate().map_err(anyhow::Error::new)?;
    Ok(Some(manifest))
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
}
