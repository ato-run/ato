use anyhow::Result;
use std::path::{Path, PathBuf};

use capsule_core::types::CapsuleManifest;

pub fn resolve_manifest_path(path: &Path) -> Result<PathBuf> {
    let manifest_path = if path.is_dir() {
        path.join("capsule.toml")
    } else {
        path.to_path_buf()
    };

    if !manifest_path.exists() {
        anyhow::bail!("capsule.toml not found at {}", manifest_path.display());
    }
    Ok(manifest_path)
}

pub fn load_manifest(path: &Path) -> Result<CapsuleManifest> {
    let manifest_path = resolve_manifest_path(path)?;
    CapsuleManifest::load_from_file(&manifest_path).map_err(Into::into)
}
