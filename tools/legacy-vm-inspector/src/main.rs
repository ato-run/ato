//! Read-only integrity inspection for historical Ready-State VM artifacts.
//!
//! This tool deliberately does not restore, repack, or publish an artifact. It
//! verifies the old manifest/chunk graph without printing application bytes so
//! migration decisions can be made from bounded evidence.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_LAYERS: usize = 16;
const MAX_CHUNK_REFERENCES: usize = 100_000;

#[derive(Debug, Deserialize)]
struct LegacyManifest {
    schema: String,
    snapshot_backend: SnapshotBackend,
    runner_class_id: String,
    #[serde(default)]
    execution_id: Option<String>,
    #[serde(default)]
    has_vsock: bool,
    #[serde(default)]
    restore_contract: serde_json::Value,
    layers: BTreeMap<String, LegacyLayer>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SnapshotBackend {
    kind: String,
    version: String,
    snapshot_format_version: String,
}

#[derive(Debug, Deserialize)]
struct LegacyLayer {
    layer: String,
    total_len: u64,
    chunks: Vec<LegacyChunk>,
}

#[derive(Debug, Deserialize)]
struct LegacyChunk {
    hash: String,
    offset: u64,
    length: u64,
}

#[derive(Debug, Serialize)]
struct InspectionReceipt {
    schema: &'static str,
    result: &'static str,
    manifest_digest: String,
    raw_manifest_digest: String,
    legacy_schema: String,
    snapshot_backend: SnapshotBackend,
    runner_class_id: String,
    execution_id: Option<String>,
    has_vsock: bool,
    restore_contract: serde_json::Value,
    layer_count: usize,
    chunk_reference_count: usize,
    unique_chunk_count: usize,
    logical_bytes: u64,
    referenced_chunk_bytes: u64,
    unique_chunk_bytes: u64,
    layer_roles: Vec<String>,
    manifest_digest_verified: bool,
    chunk_lengths_verified: bool,
    chunk_digests_verified: bool,
    content_scanned_for_secrets: bool,
    mutates_artifact: bool,
}

struct Arguments {
    manifest: PathBuf,
    cas_root: PathBuf,
    expected_manifest_digest: String,
}

fn main() -> Result<()> {
    let arguments = parse_arguments(std::env::args_os().skip(1))?;
    let receipt = inspect(&arguments)?;
    println!("{}", serde_json::to_string_pretty(&receipt)?);
    Ok(())
}

fn parse_arguments(arguments: impl Iterator<Item = std::ffi::OsString>) -> Result<Arguments> {
    let mut arguments = arguments;
    let manifest = arguments.next().context(
        "usage: legacy-vm-inspector MANIFEST --cas-root DIRECTORY --expected-manifest-digest blake3:HEX",
    )?;
    let mut cas_root = None;
    let mut expected_manifest_digest = None;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--cas-root") => {
                cas_root = Some(PathBuf::from(
                    arguments.next().context("--cas-root requires a value")?,
                ));
            }
            Some("--expected-manifest-digest") => {
                expected_manifest_digest = Some(
                    arguments
                        .next()
                        .context("--expected-manifest-digest requires a value")?
                        .into_string()
                        .map_err(|_| anyhow::anyhow!("manifest digest must be UTF-8"))?,
                );
            }
            _ => bail!("unknown argument `{}`", argument.to_string_lossy()),
        }
    }
    Ok(Arguments {
        manifest: PathBuf::from(manifest),
        cas_root: cas_root.context("--cas-root is required")?,
        expected_manifest_digest: expected_manifest_digest
            .context("--expected-manifest-digest is required")?,
    })
}

fn inspect(arguments: &Arguments) -> Result<InspectionReceipt> {
    reject_symlink(&arguments.manifest)?;
    let manifest_metadata = std::fs::metadata(&arguments.manifest)
        .with_context(|| format!("read metadata for {}", arguments.manifest.display()))?;
    ensure!(
        manifest_metadata.is_file(),
        "manifest must be a regular file"
    );
    ensure!(
        manifest_metadata.len() <= MAX_MANIFEST_BYTES,
        "manifest exceeds {MAX_MANIFEST_BYTES} bytes"
    );
    let manifest_bytes = std::fs::read(&arguments.manifest)
        .with_context(|| format!("read {}", arguments.manifest.display()))?;
    let manifest_value: serde_json::Value =
        serde_json::from_slice(&manifest_bytes).context("decode legacy manifest JSON")?;
    let manifest_digest =
        digest_bytes(&serde_jcs::to_vec(&manifest_value).context("canonicalize legacy manifest")?);
    ensure!(
        manifest_digest == arguments.expected_manifest_digest,
        "manifest digest mismatch: expected {}, measured {manifest_digest}",
        arguments.expected_manifest_digest
    );
    let manifest: LegacyManifest =
        serde_json::from_slice(&manifest_bytes).context("decode legacy manifest")?;
    ensure!(
        manifest.schema == "ato.ready-state/v1",
        "unsupported schema"
    );
    ensure!(
        manifest.snapshot_backend.kind == "firecracker",
        "artifact is not a Firecracker snapshot"
    );
    ensure!(
        !manifest.runner_class_id.is_empty(),
        "runner class is absent"
    );
    ensure!(
        !manifest.layers.is_empty() && manifest.layers.len() <= MAX_LAYERS,
        "legacy layer count is outside the supported inspection bound"
    );

    let blob_root = arguments.cas_root.join("blobs").join("blake3");
    reject_symlink(&arguments.cas_root)?;
    reject_symlink(&blob_root)?;
    ensure!(blob_root.is_dir(), "legacy CAS blob directory is absent");

    let mut chunk_reference_count = 0usize;
    let mut logical_bytes = 0u64;
    let mut referenced_chunk_bytes = 0u64;
    let mut unique_chunks = BTreeMap::<String, u64>::new();
    let mut layer_roles = Vec::new();
    for (role, layer) in &manifest.layers {
        ensure!(
            !role.is_empty() && !layer.layer.is_empty(),
            "empty layer role"
        );
        layer_roles.push(role.clone());
        logical_bytes = logical_bytes
            .checked_add(layer.total_len)
            .context("logical byte count overflow")?;
        let mut expected_offset = 0u64;
        for chunk in &layer.chunks {
            chunk_reference_count += 1;
            ensure!(
                chunk_reference_count <= MAX_CHUNK_REFERENCES,
                "manifest contains more than {MAX_CHUNK_REFERENCES} chunk references"
            );
            ensure!(
                chunk.offset == expected_offset,
                "non-contiguous layer `{role}`"
            );
            ensure!(chunk.length > 0, "zero-length chunk in layer `{role}`");
            expected_offset = expected_offset
                .checked_add(chunk.length)
                .context("chunk offset overflow")?;
            referenced_chunk_bytes = referenced_chunk_bytes
                .checked_add(chunk.length)
                .context("referenced chunk byte count overflow")?;
            let hash = parse_blake3(&chunk.hash)?;
            if let Some(previous) = unique_chunks.insert(hash.clone(), chunk.length) {
                ensure!(
                    previous == chunk.length,
                    "one content hash declares inconsistent lengths"
                );
            }
        }
        ensure!(
            expected_offset == layer.total_len,
            "layer `{role}` coverage mismatch"
        );
    }

    let mut unique_chunk_bytes = 0u64;
    for (hash, expected_length) in &unique_chunks {
        let path = blob_root.join(hash);
        reject_symlink(&path)?;
        let metadata = std::fs::metadata(&path)
            .with_context(|| format!("missing legacy CAS object blake3:{hash}"))?;
        ensure!(
            metadata.is_file(),
            "legacy CAS object is not a regular file"
        );
        ensure!(
            metadata.len() == *expected_length,
            "legacy CAS object blake3:{hash} length mismatch"
        );
        unique_chunk_bytes = unique_chunk_bytes
            .checked_add(metadata.len())
            .context("unique chunk byte count overflow")?;
        ensure!(
            digest_file(&path)? == format!("blake3:{hash}"),
            "legacy CAS object blake3:{hash} digest mismatch"
        );
    }

    Ok(InspectionReceipt {
        schema: "ato.legacy-vm-integrity-inspection.v0",
        result: "pass",
        manifest_digest,
        raw_manifest_digest: digest_bytes(&manifest_bytes),
        legacy_schema: manifest.schema,
        snapshot_backend: manifest.snapshot_backend,
        runner_class_id: manifest.runner_class_id,
        execution_id: manifest.execution_id,
        has_vsock: manifest.has_vsock,
        restore_contract: manifest.restore_contract,
        layer_count: manifest.layers.len(),
        chunk_reference_count,
        unique_chunk_count: unique_chunks.len(),
        logical_bytes,
        referenced_chunk_bytes,
        unique_chunk_bytes,
        layer_roles,
        manifest_digest_verified: true,
        chunk_lengths_verified: true,
        chunk_digests_verified: true,
        content_scanned_for_secrets: false,
        mutates_artifact: false,
    })
}

fn parse_blake3(value: &str) -> Result<String> {
    let hash = value
        .strip_prefix("blake3:")
        .context("legacy chunk uses a non-BLAKE3 reference")?;
    ensure!(
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "invalid BLAKE3 reference"
    );
    Ok(hash.to_owned())
}

fn digest_bytes(bytes: &[u8]) -> String {
    format!("blake3:{}", blake3::hash(bytes).to_hex())
}

fn digest_file(path: &Path) -> Result<String> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

fn reject_symlink(path: &Path) -> Result<()> {
    let metadata =
        std::fs::symlink_metadata(path).with_context(|| format!("inspect {}", path.display()))?;
    ensure!(
        !metadata.file_type().is_symlink(),
        "symlinks are not accepted: {}",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Arguments) {
        let directory = tempfile::tempdir().unwrap();
        let cas_root = directory.path().join("cas");
        let blobs = cas_root.join("blobs/blake3");
        std::fs::create_dir_all(&blobs).unwrap();
        let chunk = b"physical bytes";
        let chunk_hash = blake3::hash(chunk).to_hex().to_string();
        std::fs::write(blobs.join(&chunk_hash), chunk).unwrap();
        let manifest = serde_json::json!({
            "schema": "ato.ready-state/v1",
            "snapshot_backend": {
                "kind": "firecracker",
                "version": "1.16.0",
                "snapshot_format_version": "fc-full-file-v1"
            },
            "runner_class_id": format!("blake3:{}", "1".repeat(64)),
            "execution_id": format!("blake3:{}", "2".repeat(64)),
            "has_vsock": true,
            "restore_contract": {"ports": [8080]},
            "layers": {
                "memory": {
                    "layer": "memory",
                    "total_len": chunk.len(),
                    "chunks": [{
                        "hash": format!("blake3:{chunk_hash}"),
                        "offset": 0,
                        "length": chunk.len()
                    }]
                }
            },
            "historical_extra_field": true
        });
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        let manifest_path = directory.path().join("manifest.json");
        std::fs::write(&manifest_path, &manifest_bytes).unwrap();
        let arguments = Arguments {
            manifest: manifest_path,
            cas_root,
            expected_manifest_digest: digest_bytes(&serde_jcs::to_vec(&manifest).unwrap()),
        };
        (directory, arguments)
    }

    #[test]
    fn verifies_manifest_and_every_unique_chunk_without_emitting_contents() {
        let (_directory, arguments) = fixture();
        let receipt = inspect(&arguments).unwrap();
        assert_eq!(receipt.result, "pass");
        assert_eq!(receipt.chunk_reference_count, 1);
        assert_eq!(receipt.unique_chunk_count, 1);
        assert!(receipt.chunk_digests_verified);
        assert!(!receipt.content_scanned_for_secrets);
        assert!(!receipt.mutates_artifact);
    }

    #[test]
    fn fails_closed_when_a_chunk_changes() {
        let (_directory, arguments) = fixture();
        let manifest_bytes = std::fs::read(&arguments.manifest).unwrap();
        let manifest: LegacyManifest = serde_json::from_slice(&manifest_bytes).unwrap();
        let hash = parse_blake3(&manifest.layers["memory"].chunks[0].hash).unwrap();
        std::fs::write(
            arguments.cas_root.join("blobs/blake3").join(hash),
            b"tampered",
        )
        .unwrap();
        assert!(inspect(&arguments).is_err());
    }

    #[test]
    fn fails_closed_when_the_expected_manifest_identity_is_wrong() {
        let (_directory, mut arguments) = fixture();
        arguments.expected_manifest_digest = format!("blake3:{}", "0".repeat(64));
        assert!(inspect(&arguments).is_err());
    }

    #[test]
    fn rejects_non_contiguous_layer_coverage() {
        let (directory, mut arguments) = fixture();
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&arguments.manifest).unwrap()).unwrap();
        manifest["layers"]["memory"]["chunks"][0]["offset"] = 1.into();
        let bytes = serde_json::to_vec(&manifest).unwrap();
        std::fs::write(&arguments.manifest, &bytes).unwrap();
        arguments.expected_manifest_digest = digest_bytes(&serde_jcs::to_vec(&manifest).unwrap());
        assert!(inspect(&arguments).is_err());
        drop(directory);
    }
}
