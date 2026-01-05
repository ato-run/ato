use anyhow::{anyhow, Result};
use std::path::Path;

use crate::capsule_types::capsule_v1::CapsuleManifestV1;

/// Resource requirements extracted from capsule
#[derive(Debug, Clone, Default)]
pub struct ResourceRequirements {
    pub cpu_cores: Option<u32>,
    pub memory_bytes: Option<u64>,
    pub gpu_memory_bytes: Option<u64>,
}

/// Parse a manifest that may be either JSON (adep) or TOML (capsule.toml).
pub fn load_manifest_str(
    path: Option<&Path>,
    text: &str,
) -> Result<(CapsuleManifestV1, Option<ResourceRequirements>)> {
    // If extension suggests TOML, parse TOML first
    if let Some(p) = path {
        if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
            if ext.eq_ignore_ascii_case("toml") {
                return load_toml_manifest(text);
            }
        }
    }

    // Try JSON first
    match serde_json::from_str::<CapsuleManifestV1>(text) {
        Ok(manifest) => {
            let reqs = extract_requirements(&manifest);
            Ok((manifest, Some(reqs)))
        }
        Err(_) => {
            // Try TOML
            load_toml_manifest(text)
        }
    }
}

fn load_toml_manifest(text: &str) -> Result<(CapsuleManifestV1, Option<ResourceRequirements>)> {
    // Use libadep conversion
    let manifest =
        CapsuleManifestV1::from_toml(text).map_err(|e| anyhow!("failed to parse TOML: {}", e))?;

    let reqs = extract_requirements(&manifest);
    Ok((manifest, Some(reqs)))
}

fn extract_requirements(manifest: &CapsuleManifestV1) -> ResourceRequirements {
    let mut req = ResourceRequirements::default();

    // Extract VRAM
    if let Ok(Some(bytes)) = manifest.requirements.vram_min_bytes() {
        req.gpu_memory_bytes = Some(bytes);
    }

    // CPU/Memory checks if they exist in requirements?
    // CapsuleRequirements has: vram_min, vram_recommended, platform.
    // Doesn't seem to have explicit CPU/RAM requirements yet in V1 spec?
    // Checking libadep/core/src/capsule_v1.rs...
    // It has `vram_min`.
    // Maybe in metadata or assumed default?
    // Engine ResourceRequirements struct has cpu/memory.
    // For now we leave them None or check metadata.

    req
}
