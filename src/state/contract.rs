use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

use crate::registry::store::PersistentStateRecord;
use capsule_core::schema_registry::SchemaRegistry;
use capsule_core::types::{CapsuleManifest, StateDurability, StateKind};

use super::{PERSISTENT_STATE_BACKEND_KIND_HOST_PATH, PERSISTENT_STATE_KIND_FILESYSTEM};

#[derive(Debug, Clone)]
pub(super) struct PersistentStateContract {
    pub owner_scope: String,
    pub state_name: String,
    pub kind: String,
    pub backend_kind: String,
    pub producer: String,
    pub purpose: String,
    pub schema_id: String,
}

pub fn persistent_state_owner_scope(manifest: &CapsuleManifest) -> Result<String> {
    manifest.persistent_state_owner_scope().ok_or_else(|| {
        anyhow::anyhow!(
            "manifest name or state_owner_scope is required before persistent state can be attached"
        )
    })
}

pub fn prepare_backend_locator(locator: &str) -> Result<String> {
    let path = PathBuf::from(locator);
    if !path.is_absolute() {
        anyhow::bail!(
            "persistent state binding '{}' must use an absolute host path or state id",
            locator
        );
    }

    if path.exists() {
        let metadata = fs::metadata(&path)
            .with_context(|| format!("failed to inspect state path: {}", path.display()))?;
        if !metadata.is_dir() {
            anyhow::bail!(
                "persistent state binding '{}' must point to a directory",
                path.display()
            );
        }
    } else {
        create_state_directory(&path)?;
    }

    Ok(fs::canonicalize(&path)
        .with_context(|| format!("failed to canonicalize state path: {}", path.display()))?
        .to_string_lossy()
        .to_string())
}

pub(super) fn persistent_state_contract(
    manifest: &CapsuleManifest,
    state_name: &str,
) -> Result<PersistentStateContract> {
    let requirement = manifest.state.get(state_name).ok_or_else(|| {
        anyhow::anyhow!(
            "persistent state '{}' is not declared in the manifest",
            state_name
        )
    })?;
    if requirement.durability != StateDurability::Persistent {
        anyhow::bail!(
            "--state only supports persistent manifest state; '{}' is {:?}",
            state_name,
            requirement.durability
        );
    }

    let schema_id = requirement
        .schema_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("persistent state '{}' is missing schema_id", state_name))
        .and_then(|value| {
            SchemaRegistry::load()?
                .resolve_schema_hash(value)
                .map_err(Into::into)
        })?;

    let producer = manifest
        .state_producer(state_name)
        .ok_or_else(|| anyhow::anyhow!("state '{}' is missing a producer identity", state_name))?;

    Ok(PersistentStateContract {
        owner_scope: persistent_state_owner_scope(manifest)?,
        state_name: state_name.to_string(),
        kind: state_kind_value(requirement.kind).to_string(),
        backend_kind: PERSISTENT_STATE_BACKEND_KIND_HOST_PATH.to_string(),
        producer,
        purpose: requirement.purpose.clone(),
        schema_id,
    })
}

pub(super) fn ensure_record_matches_contract(
    record: &PersistentStateRecord,
    contract: &PersistentStateContract,
) -> Result<()> {
    if record.owner_scope != contract.owner_scope {
        anyhow::bail!(
            "persistent state '{}' belongs to owner scope '{}' and cannot be attached to '{}'",
            record.state_id,
            record.owner_scope,
            contract.owner_scope
        );
    }

    if record.state_name != contract.state_name
        || record.kind != contract.kind
        || record.backend_kind != contract.backend_kind
        || record.producer != contract.producer
        || record.purpose != contract.purpose
        || record.schema_id != contract.schema_id
    {
        anyhow::bail!(
            "persistent state '{}' is incompatible with existing registry entry '{}': producer/purpose/schema_id must match exactly",
            contract.state_name,
            record.state_id
        );
    }

    Ok(())
}

fn state_kind_value(kind: StateKind) -> &'static str {
    match kind {
        StateKind::Filesystem => PERSISTENT_STATE_KIND_FILESYSTEM,
    }
}

fn create_state_directory(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;

        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(path)
            .with_context(|| format!("failed to create state directory: {}", path.display()))?;
    }

    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
            .with_context(|| format!("failed to create state directory: {}", path.display()))?;
    }

    Ok(())
}
