use serde::Serialize;

use crate::capsule_v3::manifest::{validate_blake3_digest, CapsuleManifestV3, CdcParams};
use crate::error::{CapsuleError, Result};

#[derive(Serialize)]
struct CapsuleManifestV3HashCore<'a> {
    schema_version: u32,
    cdc_params: &'a CdcParams,
    total_raw_size: u64,
    chunks: Vec<ChunkMetaHashCore<'a>>,
}

#[derive(Serialize)]
struct ChunkMetaHashCore<'a> {
    raw_hash: &'a str,
    raw_size: u32,
}

pub fn compute_artifact_hash_jcs_blake3(manifest: &CapsuleManifestV3) -> Result<String> {
    manifest.validate_core()?;

    let core = build_hash_core(manifest);
    let canonical = serde_jcs::to_vec(&core).map_err(|e| {
        CapsuleError::Pack(format!(
            "failed to canonicalize capsule v3 manifest for hashing: {e}"
        ))
    })?;

    Ok(format!("blake3:{}", blake3::hash(&canonical).to_hex()))
}

pub fn set_artifact_hash(manifest: &mut CapsuleManifestV3) -> Result<()> {
    manifest.artifact_hash = compute_artifact_hash_jcs_blake3(manifest)?;
    Ok(())
}

pub fn verify_artifact_hash(manifest: &CapsuleManifestV3) -> Result<()> {
    validate_blake3_digest("artifact_hash", &manifest.artifact_hash)?;
    let expected = compute_artifact_hash_jcs_blake3(manifest)?;
    if manifest.artifact_hash != expected {
        return Err(CapsuleError::HashMismatch(
            expected,
            manifest.artifact_hash.clone(),
        ));
    }
    Ok(())
}

fn build_hash_core(manifest: &CapsuleManifestV3) -> CapsuleManifestV3HashCore<'_> {
    CapsuleManifestV3HashCore {
        schema_version: manifest.schema_version,
        cdc_params: &manifest.cdc_params,
        total_raw_size: manifest.total_raw_size,
        chunks: manifest
            .chunks
            .iter()
            .map(|chunk| ChunkMetaHashCore {
                raw_hash: &chunk.raw_hash,
                raw_size: chunk.raw_size,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::{compute_artifact_hash_jcs_blake3, set_artifact_hash, verify_artifact_hash};
    use crate::capsule_v3::manifest::{blake3_digest, CdcParams, ChunkMeta};
    use crate::capsule_v3::CapsuleManifestV3;

    fn sample_manifest() -> CapsuleManifestV3 {
        CapsuleManifestV3 {
            schema_version: 3,
            artifact_hash: String::new(),
            cdc_params: CdcParams::default_fastcdc(),
            total_raw_size: 11,
            chunks: vec![
                ChunkMeta {
                    raw_hash: blake3_digest(b"hello"),
                    raw_size: 5,
                    zstd_size_hint: Some(12),
                },
                ChunkMeta {
                    raw_hash: blake3_digest(b" world"),
                    raw_size: 6,
                    zstd_size_hint: Some(13),
                },
            ],
        }
    }

    #[test]
    fn test_manifest_jcs_determinism() {
        let json_a = r#"{
            "schema_version":3,
            "artifact_hash":"",
            "cdc_params":{"algorithm":"fastcdc","min_size":2097152,"avg_size":4194304,"max_size":8388608,"seed":0},
            "total_raw_size":11,
            "chunks":[
                {"raw_hash":"blake3:ea8f163db38682925e4491c5e58d4bb3506ef8c14eb78a86e908c5624a67200f","raw_size":5,"zstd_size_hint":12},
                {"raw_hash":"blake3:d7894ae9716d38d2dfad0ec55424ca321ee12453d51f1b3adeb77d0475ed988c","raw_size":6,"zstd_size_hint":13}
            ]
        }"#;
        let json_b = r#"{ "chunks":[
                { "zstd_size_hint":12, "raw_size":5, "raw_hash":"blake3:ea8f163db38682925e4491c5e58d4bb3506ef8c14eb78a86e908c5624a67200f" },
                { "raw_hash":"blake3:d7894ae9716d38d2dfad0ec55424ca321ee12453d51f1b3adeb77d0475ed988c", "raw_size":6, "zstd_size_hint":13 }
            ],
            "total_raw_size":11,
            "cdc_params":{"seed":0,"max_size":8388608,"avg_size":4194304,"min_size":2097152,"algorithm":"fastcdc"},
            "artifact_hash":"",
            "schema_version":3
        }"#;
        let left: CapsuleManifestV3 = serde_json::from_str(json_a).unwrap();
        let right: CapsuleManifestV3 = serde_json::from_str(json_b).unwrap();
        let left_hash = compute_artifact_hash_jcs_blake3(&left).unwrap();
        let right_hash = compute_artifact_hash_jcs_blake3(&right).unwrap();
        assert_eq!(left_hash, right_hash);
    }

    #[test]
    fn artifact_hash_is_deterministic() {
        let manifest = sample_manifest();
        let left = compute_artifact_hash_jcs_blake3(&manifest).unwrap();
        let right = compute_artifact_hash_jcs_blake3(&manifest).unwrap();
        assert_eq!(left, right);
    }

    #[test]
    fn zstd_size_hint_does_not_change_artifact_hash() {
        let mut left = sample_manifest();
        let mut right = sample_manifest();
        left.chunks[0].zstd_size_hint = Some(111);
        right.chunks[0].zstd_size_hint = Some(222);

        let left_hash = compute_artifact_hash_jcs_blake3(&left).unwrap();
        let right_hash = compute_artifact_hash_jcs_blake3(&right).unwrap();
        assert_eq!(left_hash, right_hash);
    }

    #[test]
    fn test_manifest_chunk_order_changes_hash() {
        let left = sample_manifest();
        let mut right = sample_manifest();
        right.chunks.reverse();
        let left_hash = compute_artifact_hash_jcs_blake3(&left).unwrap();
        let right_hash = compute_artifact_hash_jcs_blake3(&right).unwrap();
        assert_ne!(left_hash, right_hash);
    }

    #[test]
    fn chunk_order_changes_artifact_hash() {
        let left = sample_manifest();
        let mut right = sample_manifest();
        right.chunks.reverse();

        let left_hash = compute_artifact_hash_jcs_blake3(&left).unwrap();
        let right_hash = compute_artifact_hash_jcs_blake3(&right).unwrap();
        assert_ne!(left_hash, right_hash);
    }

    #[test]
    fn raw_size_change_changes_artifact_hash() {
        let left = sample_manifest();
        let mut right = sample_manifest();
        right.chunks[0].raw_size += 1;
        right.total_raw_size += 1;

        let left_hash = compute_artifact_hash_jcs_blake3(&left).unwrap();
        let right_hash = compute_artifact_hash_jcs_blake3(&right).unwrap();
        assert_ne!(left_hash, right_hash);
    }

    #[test]
    fn raw_hash_change_changes_artifact_hash() {
        let left = sample_manifest();
        let mut right = sample_manifest();
        right.chunks[0].raw_hash = blake3_digest(b"changed");

        let left_hash = compute_artifact_hash_jcs_blake3(&left).unwrap();
        let right_hash = compute_artifact_hash_jcs_blake3(&right).unwrap();
        assert_ne!(left_hash, right_hash);
    }

    #[test]
    fn verify_artifact_hash_checks_value() {
        let mut manifest = sample_manifest();
        set_artifact_hash(&mut manifest).unwrap();
        assert!(verify_artifact_hash(&manifest).is_ok());

        manifest.artifact_hash = blake3_digest(b"tampered");
        assert!(verify_artifact_hash(&manifest).is_err());
    }

    #[test]
    fn test_verify_artifact_hash_detects_tampering() {
        let mut manifest = sample_manifest();
        set_artifact_hash(&mut manifest).unwrap();
        manifest.total_raw_size += 1;
        assert!(verify_artifact_hash(&manifest).is_err());
    }
}
