use anyhow::Result;

use crate::registry::store::{NewPersistentStateRecord, PersistentStateRecord, RegistryStore};
use capsule_core::types::CapsuleManifest;

use super::contract::{
    ensure_record_matches_contract, persistent_state_contract, prepare_backend_locator,
};
use super::parse_state_reference;

pub fn open_state_store() -> Result<RegistryStore> {
    let store_dir = capsule_core::config::config_dir()?.join("state");
    RegistryStore::open(&store_dir)
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
    let store = open_state_store()?;
    resolve_registered_state_reference_in_store(manifest, state_name, state_ref, &store)
}

pub fn resolve_registered_state_reference_in_store(
    manifest: &CapsuleManifest,
    state_name: &str,
    state_ref: &str,
    store: &RegistryStore,
) -> Result<PersistentStateRecord> {
    let state_id = parse_state_reference(state_ref).ok_or_else(|| {
        anyhow::anyhow!(
            "persistent state binding '{}' must use an absolute host path or state id",
            state_ref
        )
    })?;
    let contract = persistent_state_contract(manifest, state_name)?;
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
