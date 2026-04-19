use std::path::{Component, Path};

use serde::Serialize;

use crate::error::{CapsuleError, Result};
use crate::resource::cas::chunk_bytes_fastcdc;
use crate::types::{CapsuleManifest, ChunkDescriptor, DistributionInfo};

pub const FASTCDC_MIN_SIZE: u32 = 16 * 1024;
pub const FASTCDC_AVG_SIZE: u32 = 64 * 1024;
pub const FASTCDC_MAX_SIZE: u32 = 256 * 1024;

#[derive(Serialize)]
struct SignableManifest<'a> {
    schema_version: &'a str,
    name: &'a str,
    version: &'a str,
    #[serde(rename = "type")]
    capsule_type: &'a crate::types::CapsuleType,
    default_target: &'a str,
    metadata: &'a crate::types::CapsuleMetadata,
    capabilities: &'a Option<crate::types::CapsuleCapabilities>,
    requirements: &'a crate::types::CapsuleRequirements,
    storage: &'a crate::types::CapsuleStorage,
    state: &'a std::collections::HashMap<String, crate::types::StateRequirement>,
    routing: &'a crate::types::CapsuleRouting,
    network: &'a Option<crate::types::NetworkConfig>,
    model: &'a Option<crate::types::ModelConfig>,
    transparency: &'a Option<crate::types::TransparencyConfig>,
    pool: &'a Option<crate::types::PoolConfig>,
    build: &'a Option<crate::types::BuildConfig>,
    pack: &'a Option<crate::types::PackConfig>,
    isolation: &'a Option<crate::types::IsolationConfig>,
    polymorphism: &'a Option<crate::types::PolymorphismConfig>,
    targets: &'a Option<crate::types::TargetsConfig>,
    services: &'a Option<std::collections::HashMap<String, crate::types::ServiceSpec>>,
    distribution: Option<SignableDistribution<'a>>,
}

#[derive(Serialize)]
struct SignableDistribution<'a> {
    merkle_root: &'a str,
    chunk_list: &'a [ChunkDescriptor],
}

pub fn normalize_relative_utf8_path(path: &Path) -> Result<String> {
    if path.is_absolute() {
        return Err(CapsuleError::Pack(format!(
            "absolute path is not allowed in payload: {}",
            path.display()
        )));
    }

    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(segment) => {
                let segment = segment.to_str().ok_or_else(|| {
                    CapsuleError::Pack(format!(
                        "non-UTF-8 path is not supported in payload: {}",
                        path.display()
                    ))
                })?;
                if !segment.is_empty() {
                    parts.push(segment);
                }
            }
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(CapsuleError::Pack(format!(
                    "unsafe relative path in payload: {}",
                    path.display()
                )))
            }
        }
    }

    if parts.is_empty() {
        return Err(CapsuleError::Pack(
            "empty payload path is not allowed".to_string(),
        ));
    }

    Ok(parts.join("/"))
}

pub fn build_distribution_manifest(
    base_manifest: &CapsuleManifest,
    payload_tar_bytes: &[u8],
) -> Result<(CapsuleManifest, Vec<u8>)> {
    let chunk_list = chunk_bytes_fastcdc(
        payload_tar_bytes,
        FASTCDC_MIN_SIZE,
        FASTCDC_AVG_SIZE,
        FASTCDC_MAX_SIZE,
    );
    let chunk_hashes = chunk_list
        .iter()
        .map(|chunk| chunk.chunk_hash.clone())
        .collect::<Vec<_>>();
    let merkle_root = compute_merkle_root(&chunk_hashes);

    let mut manifest = base_manifest.clone();
    manifest.schema_version = "0.3".to_string();
    manifest.distribution = Some(DistributionInfo {
        manifest_hash: String::new(),
        merkle_root,
        chunk_list,
        signatures: Vec::new(),
    });
    let manifest_hash = compute_manifest_hash_without_signatures(&manifest)?;
    manifest
        .distribution
        .as_mut()
        .expect("distribution just inserted")
        .manifest_hash = manifest_hash;

    let toml_bytes = toml::to_string_pretty(&manifest)
        .map_err(|err| CapsuleError::Pack(format!("failed to serialize capsule.toml: {err}")))?
        .into_bytes();

    Ok((manifest, toml_bytes))
}

pub fn canonicalize_signable_manifest(manifest: &CapsuleManifest) -> Result<Vec<u8>> {
    let distribution = manifest
        .distribution
        .as_ref()
        .map(|distribution| SignableDistribution {
            merkle_root: &distribution.merkle_root,
            chunk_list: &distribution.chunk_list,
        });
    let signable = SignableManifest {
        schema_version: &manifest.schema_version,
        name: &manifest.name,
        version: &manifest.version,
        capsule_type: &manifest.capsule_type,
        default_target: &manifest.default_target,
        metadata: &manifest.metadata,
        capabilities: &manifest.capabilities,
        requirements: &manifest.requirements,
        storage: &manifest.storage,
        state: &manifest.state,
        routing: &manifest.routing,
        network: &manifest.network,
        model: &manifest.model,
        transparency: &manifest.transparency,
        pool: &manifest.pool,
        build: &manifest.build,
        pack: &manifest.pack,
        isolation: &manifest.isolation,
        polymorphism: &manifest.polymorphism,
        targets: &manifest.targets,
        services: &manifest.services,
        distribution,
    };

    serde_jcs::to_vec(&signable).map_err(|err| {
        CapsuleError::Pack(format!(
            "failed to canonicalize semantic manifest for hashing: {err}"
        ))
    })
}

pub fn compute_manifest_hash_without_signatures(manifest: &CapsuleManifest) -> Result<String> {
    let canonical = canonicalize_signable_manifest(manifest)?;
    Ok(format!("blake3:{}", blake3::hash(&canonical).to_hex()))
}

pub fn manifest_hash(manifest: &CapsuleManifest) -> Result<String> {
    let distribution = manifest
        .distribution
        .as_ref()
        .ok_or_else(|| CapsuleError::Pack("distribution metadata is required".to_string()))?;
    if distribution.manifest_hash.trim().is_empty() {
        return compute_manifest_hash_without_signatures(manifest);
    }
    Ok(distribution.manifest_hash.clone())
}

pub fn compute_merkle_root(chunk_hashes: &[String]) -> String {
    if chunk_hashes.is_empty() {
        return format!("blake3:{}", blake3::hash(b"").to_hex());
    }

    let mut level: Vec<[u8; 32]> = chunk_hashes
        .iter()
        .map(|hash| {
            let normalized = hash.trim().trim_start_matches("blake3:");
            let mut out = [0u8; 32];
            let decoded = hex::decode(normalized).unwrap_or_else(|_| vec![0u8; 32]);
            if decoded.len() == 32 {
                out.copy_from_slice(&decoded);
            }
            out
        })
        .collect();

    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut i = 0usize;
        while i < level.len() {
            let left = level[i];
            let right = if i + 1 < level.len() {
                level[i + 1]
            } else {
                level[i]
            };
            let mut hasher = blake3::Hasher::new();
            hasher.update(&left);
            hasher.update(&right);
            let digest = hasher.finalize();
            let mut out = [0u8; 32];
            out.copy_from_slice(digest.as_bytes());
            next.push(out);
            i += 2;
        }
        level = next;
    }

    format!("blake3:{}", hex::encode(level[0]))
}

pub fn reconstruct_from_chunks(
    payload_tar_bytes: &[u8],
    chunk_list: &[ChunkDescriptor],
) -> Result<Vec<u8>> {
    let mut rebuilt = Vec::new();
    let mut next_offset = 0u64;
    for chunk in chunk_list {
        if chunk.offset != next_offset {
            return Err(CapsuleError::Pack(format!(
                "non-contiguous chunk offsets in chunk_list: expected {}, got {}",
                next_offset, chunk.offset
            )));
        }
        let start = chunk.offset as usize;
        let end = start.saturating_add(chunk.length as usize);
        if end > payload_tar_bytes.len() {
            return Err(CapsuleError::Pack(format!(
                "chunk range out of bounds in chunk_list: {}..{} (payload={})",
                start,
                end,
                payload_tar_bytes.len()
            )));
        }
        rebuilt.extend_from_slice(&payload_tar_bytes[start..end]);
        next_offset = chunk.offset.saturating_add(chunk.length);
    }
    Ok(rebuilt)
}

#[cfg(test)]
mod tests {
    use super::{
        build_distribution_manifest, compute_manifest_hash_without_signatures,
        normalize_relative_utf8_path, reconstruct_from_chunks,
    };
    use crate::types::CapsuleManifest;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::Path;

    const VALID_TOML: &str = r#"
schema_version = "0.3"
name = "sample"
version = "1.0.0"
type = "app"

runtime = "source"
run = "main.py""#;

    #[test]
    fn normalize_relative_utf8_path_rejects_parent_dir() {
        let err = normalize_relative_utf8_path(Path::new("../secret")).expect_err("must fail");
        assert!(err.to_string().contains("unsafe relative path"));
    }

    #[cfg(unix)]
    #[test]
    fn normalize_relative_utf8_path_rejects_non_utf8() {
        let raw = OsString::from_vec(vec![0x66, 0x6f, 0x80]);
        let path = Path::new(&raw);
        let err = normalize_relative_utf8_path(path).expect_err("must fail");
        assert!(err.to_string().contains("non-UTF-8 path"));
    }

    #[test]
    fn reconstruct_roundtrip_matches_payload() {
        let payload = b"payload-bytes-for-manifest".to_vec();
        let base = CapsuleManifest::from_toml(VALID_TOML).expect("manifest");
        let (manifest, _toml_bytes) =
            build_distribution_manifest(&base, &payload).expect("distribution manifest");
        let chunk_list = &manifest
            .distribution
            .as_ref()
            .expect("distribution")
            .chunk_list;
        let rebuilt = reconstruct_from_chunks(&payload, chunk_list).expect("rebuild");
        assert_eq!(rebuilt, payload);
        let recomputed = compute_manifest_hash_without_signatures(&manifest).expect("recompute");
        assert_eq!(
            recomputed,
            manifest
                .distribution
                .as_ref()
                .expect("distribution")
                .manifest_hash
        );
    }
}
