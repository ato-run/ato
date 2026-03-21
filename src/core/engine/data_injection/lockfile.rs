use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use capsule_core::lockfile::{
    parse_lockfile_text, resolve_existing_lockfile_path, verify_lockfile_manifest,
    LockedInjectedData,
};

pub(super) fn persist_lockfile_injected_data(
    manifest_path: &Path,
    injected_data: &HashMap<String, LockedInjectedData>,
) -> Result<()> {
    if injected_data.is_empty() {
        return Ok(());
    }

    let Some(lockfile_path) = manifest_path
        .parent()
        .and_then(resolve_existing_lockfile_path)
    else {
        return Ok(());
    };

    verify_lockfile_manifest(manifest_path, &lockfile_path)?;
    let raw = fs::read_to_string(&lockfile_path)?;
    let mut lockfile = parse_lockfile_text(&raw, &lockfile_path)?;
    let mut changed = false;
    for (key, value) in injected_data {
        match lockfile.injected_data.get(key) {
            Some(existing) if existing == value => {}
            Some(existing) => {
                anyhow::bail!(
                    "{} injected_data.{} does not match runtime-resolved data (expected {} / {}, got {} / {})",
                    lockfile_path.display(),
                    key,
                    existing.source,
                    existing.digest,
                    value.source,
                    value.digest
                );
            }
            None => {
                lockfile.injected_data.insert(key.clone(), value.clone());
                changed = true;
            }
        }
    }

    if changed {
        let bytes = serde_json::to_vec_pretty(&lockfile)?;
        fs::write(&lockfile_path, bytes)?;
    }

    Ok(())
}
