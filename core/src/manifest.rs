use std::path::{Path, PathBuf};

use crate::error::{CapsuleError, Result};
use crate::types::{CapsuleManifest, TargetsConfig};

pub struct LoadedManifest {
    pub raw: toml::Value,
    pub model: CapsuleManifest,
    pub raw_text: String,
    pub path: PathBuf,
    pub dir: PathBuf,
}

pub fn load_manifest(path: &Path) -> Result<LoadedManifest> {
    let raw_text = std::fs::read_to_string(path).map_err(|e| {
        CapsuleError::Manifest(
            path.to_path_buf(),
            format!("Failed to read manifest: {}", e),
        )
    })?;

    let raw: toml::Value = toml::from_str(&raw_text).map_err(|e| {
        CapsuleError::Manifest(
            path.to_path_buf(),
            format!("Failed to parse manifest TOML: {}", e),
        )
    })?;

    if raw.get("execution").is_some() {
        return Err(CapsuleError::Manifest(
            path.to_path_buf(),
            "legacy [execution] section is not supported in schema_version=0.2".to_string(),
        ));
    }

    let mut model = CapsuleManifest::from_toml(&raw_text).map_err(|e| {
        CapsuleError::Manifest(
            path.to_path_buf(),
            format!("Failed to parse manifest into schema: {}", e),
        )
    })?;

    if let Err(errors) = model.validate() {
        let details = errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(CapsuleError::Manifest(
            path.to_path_buf(),
            format!("Manifest validation failed: {}", details),
        ));
    }

    if let Some(targets) = model.targets.as_ref() {
        if let Err(err) = targets.validate_source_digest() {
            return Err(CapsuleError::Manifest(path.to_path_buf(), err.to_string()));
        }
    }

    // Ensure schema_version is set for downstream consumers.
    if model.schema_version.trim().is_empty() {
        model.schema_version = "0.2".to_string();
    }

    let dir = path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    Ok(LoadedManifest {
        raw,
        model,
        raw_text,
        path: path.to_path_buf(),
        dir,
    })
}

#[allow(dead_code)]
pub fn manifest_requires_cas_source(targets: &TargetsConfig) -> bool {
    targets.source.is_some() && targets.source_digest.is_some()
}
