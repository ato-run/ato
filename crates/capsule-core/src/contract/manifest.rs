use std::path::{Path, PathBuf};

use crate::common::paths::manifest_dir;
use crate::error::{CapsuleError, Result};
use crate::types::{CapsuleManifest, ValidationMode};

#[derive(Debug, Clone)]
pub struct LoadedManifest {
    pub raw: toml::Value,
    pub model: CapsuleManifest,
    pub raw_text: String,
    pub path: PathBuf,
    pub dir: PathBuf,
}

pub fn load_manifest(path: &Path) -> Result<LoadedManifest> {
    load_manifest_with_validation_mode(path, ValidationMode::Strict)
}

pub fn load_manifest_with_validation_mode(
    path: &Path,
    validation_mode: ValidationMode,
) -> Result<LoadedManifest> {
    let raw_text = std::fs::read_to_string(path).map_err(|e| {
        CapsuleError::Manifest(
            path.to_path_buf(),
            format!("Failed to read manifest: {}", e),
        )
    })?;

    let mut model = CapsuleManifest::from_toml_with_path(&raw_text, path).map_err(|e| {
        CapsuleError::Manifest(
            path.to_path_buf(),
            format!("Failed to parse manifest into schema: {}", e),
        )
    })?;

    if let Err(errors) = model.validate_for_mode(validation_mode) {
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
        model.schema_version = "0.3".to_string();
    }

    let normalized_text = model.to_toml().map_err(|e| {
        CapsuleError::Manifest(
            path.to_path_buf(),
            format!("Failed to serialize normalized manifest: {}", e),
        )
    })?;
    let raw: toml::Value = toml::from_str(&normalized_text).map_err(|e| {
        CapsuleError::Manifest(
            path.to_path_buf(),
            format!("Failed to parse normalized manifest TOML: {}", e),
        )
    })?;

    let dir = manifest_dir(path);

    Ok(LoadedManifest {
        raw,
        model,
        raw_text,
        path: path.to_path_buf(),
        dir,
    })
}
