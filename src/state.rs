use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

use crate::registry_store::{NewPersistentStateRecord, PersistentStateRecord, RegistryStore};
use capsule_core::schema_registry::SchemaRegistry;
use capsule_core::types::{CapsuleManifest, StateDurability, StateKind};

pub const PERSISTENT_STATE_KIND_FILESYSTEM: &str = "filesystem";
pub const PERSISTENT_STATE_BACKEND_KIND_HOST_PATH: &str = "host_path";
const ATO_STATE_SCHEME: &str = "ato-state://";

#[derive(Debug, Clone)]
struct PersistentStateContract {
    owner_scope: String,
    state_name: String,
    kind: String,
    backend_kind: String,
    producer: String,
    purpose: String,
    schema_id: String,
}

pub fn open_state_store() -> Result<RegistryStore> {
    let store_dir = capsule_core::config::config_dir()?.join("state");
    RegistryStore::open(&store_dir)
}

pub fn parse_state_reference(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix(ATO_STATE_SCHEME) {
        let state_id = rest.trim();
        return (!state_id.is_empty()).then_some(state_id);
    }
    trimmed.starts_with("state-").then_some(trimmed)
}

pub fn persistent_state_owner_scope(manifest: &CapsuleManifest) -> Result<String> {
    manifest.persistent_state_owner_scope().ok_or_else(|| {
        anyhow::anyhow!("manifest name or state_owner_scope is required before persistent state can be attached")
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

pub fn ensure_registered_state_binding(
    manifest: &CapsuleManifest,
    state_name: &str,
    locator: &str,
) -> Result<PersistentStateRecord> {
    let store = open_state_store()?;

    ensure_registered_state_binding_in_store(manifest, state_name, locator, &store)
}

pub fn ensure_registered_state_binding_in_store(
    manifest: &CapsuleManifest,
    state_name: &str,
    locator: &str,
    store: &RegistryStore,
) -> Result<PersistentStateRecord> {
    let contract = persistent_state_contract(manifest, state_name)?;
    let backend_locator = prepare_backend_locator(locator)?;

    if let Some(existing) =
        store.find_persistent_state_by_owner_and_locator(&contract.owner_scope, &backend_locator)?
    {
        ensure_record_matches_contract(&existing, &contract)?;
        Ok(existing)
    } else {
        store.register_persistent_state(&NewPersistentStateRecord {
            owner_scope: contract.owner_scope,
            state_name: contract.state_name,
            kind: contract.kind,
            backend_kind: contract.backend_kind,
            backend_locator,
            producer: contract.producer,
            purpose: contract.purpose,
            schema_id: contract.schema_id,
        })
    }
}

pub fn resolve_registered_state_reference(
    manifest: &CapsuleManifest,
    state_name: &str,
    state_ref: &str,
) -> Result<PersistentStateRecord> {
    let state_id = parse_state_reference(state_ref).ok_or_else(|| {
        anyhow::anyhow!(
            "persistent state binding '{}' must use an absolute host path or state id",
            state_ref
        )
    })?;
    let contract = persistent_state_contract(manifest, state_name)?;
    let store = open_state_store()?;
    let existing = store
        .find_persistent_state_by_id(state_id)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "persistent state '{}' was not found in the registry",
                state_id
            )
        })?;
    ensure_record_matches_contract(&existing, &contract)?;

    let backend_locator = prepare_backend_locator(&existing.backend_locator)?;
    if backend_locator != existing.backend_locator {
        anyhow::bail!(
            "persistent state '{}' backend path changed outside the registry; re-register the state binding",
            existing.state_id
        );
    }
    Ok(existing)
}

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

fn persistent_state_contract(
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

fn ensure_record_matches_contract(
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

pub fn list_states(owner_scope: Option<&str>, state_name: Option<&str>, json: bool) -> Result<()> {
    let store = open_state_store()?;
    let records = store.list_persistent_states(owner_scope, state_name)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&records)?);
        return Ok(());
    }

    if records.is_empty() {
        println!("No persistent states registered.");
        return Ok(());
    }

    println!(
        "{:<40} {:<20} {:<16} {:<12} {:<12} BACKEND LOCATOR",
        "STATE ID", "OWNER SCOPE", "STATE", "KIND", "BACKEND"
    );
    for record in records {
        println!(
            "{:<40} {:<20} {:<16} {:<12} {:<12} {}",
            record.state_id,
            record.owner_scope,
            record.state_name,
            record.kind,
            record.backend_kind,
            record.backend_locator,
        );
        println!("   producer: {}", record.producer);
        println!("   purpose:  {}", record.purpose);
        println!("   schema:   {}", record.schema_id);
        println!();
    }
    Ok(())
}

pub fn inspect_state(state_ref: &str, json: bool) -> Result<()> {
    let state_id = parse_state_reference(state_ref).unwrap_or(state_ref);
    let store = open_state_store()?;
    let record = store
        .find_persistent_state_by_id(state_id)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "persistent state '{}' was not found in the registry",
                state_id
            )
        })?;

    if json {
        println!("{}", serde_json::to_string_pretty(&record)?);
        return Ok(());
    }

    println!("State ID: {}", record.state_id);
    println!("Owner Scope: {}", record.owner_scope);
    println!("State Name: {}", record.state_name);
    println!("Kind: {}", record.kind);
    println!("Backend Kind: {}", record.backend_kind);
    println!("Backend Locator: {}", record.backend_locator);
    println!("Producer: {}", record.producer);
    println!("Purpose: {}", record.purpose);
    println!("Schema ID: {}", record.schema_id);
    println!("Created At: {}", record.created_at);
    println!("Updated At: {}", record.updated_at);
    Ok(())
}

pub fn register_state_from_manifest(
    manifest_path: &Path,
    state_name: &str,
    locator: &str,
    json: bool,
) -> Result<()> {
    let manifest = load_manifest(manifest_path)?;
    let record = ensure_registered_state_binding(&manifest, state_name, locator)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&record)?);
        return Ok(());
    }

    println!("✅ Registered persistent state {}", record.state_id);
    println!("   state_id: {}", record.state_id);
    println!("   owner_scope: {}", record.owner_scope);
    println!("   state_name: {}", record.state_name);
    println!("   kind: {}", record.kind);
    println!("   backend_kind: {}", record.backend_kind);
    println!("   backend_locator: {}", record.backend_locator);
    Ok(())
}

fn create_state_directory(path: &Path) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_state_reference_accepts_bare_and_scheme_forms() {
        assert_eq!(parse_state_reference("state-demo"), Some("state-demo"));
        assert_eq!(
            parse_state_reference("ato-state://state-demo"),
            Some("state-demo")
        );
        assert_eq!(parse_state_reference("/absolute/path"), None);
    }
}
