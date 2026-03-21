use anyhow::{Context, Result};
use std::io::{Cursor, Read};
use std::path::Path;

use capsule_core::types::CapsuleManifest;

pub(super) fn load_manifest(path: &Path) -> Result<CapsuleManifest> {
    let manifest_path = if path.is_dir() {
        path.join("capsule.toml")
    } else {
        path.to_path_buf()
    };

    if !manifest_path.exists() {
        anyhow::bail!("capsule.toml not found at {}", manifest_path.display());
    }

    if manifest_path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("capsule"))
    {
        let bytes = std::fs::read(&manifest_path).with_context(|| {
            format!(
                "failed to read capsule artifact {}",
                manifest_path.display()
            )
        })?;
        let manifest_raw = extract_manifest_from_capsule(&bytes)?;
        return CapsuleManifest::from_toml(&manifest_raw).map_err(Into::into);
    }

    CapsuleManifest::load_from_file(&manifest_path).map_err(Into::into)
}

fn extract_manifest_from_capsule(bytes: &[u8]) -> Result<String> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive
        .entries()
        .context("Failed to read .capsule archive entries")?;

    for entry in entries {
        let mut entry = entry.context("Invalid .capsule entry")?;
        let entry_path = entry
            .path()
            .context("Failed to read archive entry path")?
            .to_string_lossy()
            .to_string();
        if entry_path != "capsule.toml" {
            continue;
        }

        let mut manifest = String::new();
        entry
            .read_to_string(&mut manifest)
            .context("Failed to read capsule.toml from artifact")?;
        return Ok(manifest);
    }

    anyhow::bail!("Invalid artifact: capsule.toml not found in .capsule archive")
}
