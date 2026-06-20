use anyhow::Result;
use std::path::Path;

use super::manifest::load_manifest;
use super::store::{ensure_registered_state_binding, open_state_store};

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
    let state_id = super::parse_state_reference(state_ref).unwrap_or(state_ref);
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
