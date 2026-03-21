use anyhow::{Context, Result};
use std::path::Path;

use crate::registry::store::{NewServiceBindingRecord, ServiceBindingRecord};
use crate::runtime::process::{ProcessInfo, ProcessManager};
use capsule_core::types::CapsuleManifest;

use super::contract::{
    auto_bindable_service_names, derive_service_endpoint_locator, host_service_binding_scope,
    ingress_binding_contract, local_service_binding_contract, normalize_endpoint_locator,
    normalize_local_service_locator, SERVICE_BINDING_KIND_SERVICE,
};
use super::manifest::load_manifest;
use super::store::open_binding_store;

pub fn register_ingress_binding_from_manifest(
    manifest_path: &Path,
    service_name: &str,
    url: &str,
    json: bool,
) -> Result<()> {
    let record = register_ingress_binding(manifest_path, service_name, url)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&record)?);
        return Ok(());
    }

    println!(
        "✅ Registered host-side ingress binding {}",
        record.binding_id
    );
    println!("   owner_scope: {}", record.owner_scope);
    println!("   service_name: {}", record.service_name);
    println!("   endpoint_locator: {}", record.endpoint_locator);
    println!("   tls_mode: {}", record.tls_mode);
    Ok(())
}

pub fn register_ingress_binding(
    manifest_path: &Path,
    service_name: &str,
    url: &str,
) -> Result<ServiceBindingRecord> {
    let manifest = load_manifest(manifest_path)?;
    let endpoint = normalize_endpoint_locator(url)?;
    let contract = ingress_binding_contract(&manifest, service_name, &endpoint)?;
    open_binding_store()?.register_service_binding(&NewServiceBindingRecord {
        owner_scope: contract.owner_scope,
        service_name: contract.service_name,
        binding_kind: contract.binding_kind,
        transport_kind: contract.transport_kind,
        adapter_kind: contract.adapter_kind,
        endpoint_locator: endpoint,
        tls_mode: contract.tls_mode,
        allowed_callers: contract.allowed_callers,
        target_hint: contract.target_hint,
    })
}

pub fn register_service_binding_from_manifest(
    manifest_path: &Path,
    service_name: &str,
    url: &str,
    json: bool,
) -> Result<()> {
    let record = register_service_binding(manifest_path, service_name, url)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&record)?);
        return Ok(());
    }

    println!("✅ Registered local service binding {}", record.binding_id);
    println!("   owner_scope: {}", record.owner_scope);
    println!("   service_name: {}", record.service_name);
    println!("   endpoint_locator: {}", record.endpoint_locator);
    println!("   tls_mode: {}", record.tls_mode);
    Ok(())
}

pub fn register_service_binding(
    manifest_path: &Path,
    service_name: &str,
    url: &str,
) -> Result<ServiceBindingRecord> {
    let manifest = load_manifest(manifest_path)?;
    let endpoint = normalize_local_service_locator(url)?;
    register_service_binding_from_parts(&manifest, service_name, endpoint)
}

pub fn register_service_binding_from_process(
    process_id: &str,
    service_name: &str,
    port: Option<u16>,
    json: bool,
) -> Result<()> {
    let record = register_service_binding_for_process(process_id, service_name, port)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&record)?);
        return Ok(());
    }

    println!("✅ Registered local service binding {}", record.binding_id);
    println!("   owner_scope: {}", record.owner_scope);
    println!("   service_name: {}", record.service_name);
    println!("   endpoint_locator: {}", record.endpoint_locator);
    println!("   tls_mode: {}", record.tls_mode);
    Ok(())
}

pub fn sync_service_bindings_from_process(process_id: &str, json: bool) -> Result<()> {
    let records = sync_service_bindings_for_process(process_id)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&records)?);
        return Ok(());
    }

    if records.is_empty() {
        println!(
            "No auto-bindable local services were found for process {}.",
            process_id
        );
        return Ok(());
    }

    println!(
        "✅ Registered {} local service binding(s) for {}",
        records.len(),
        process_id
    );
    for record in records {
        println!(
            "   {} -> {} ({})",
            record.service_name, record.endpoint_locator, record.binding_id
        );
    }
    Ok(())
}

pub fn register_service_binding_for_process(
    process_id: &str,
    service_name: &str,
    port: Option<u16>,
) -> Result<ServiceBindingRecord> {
    let process = ProcessManager::new()?
        .read_pid(process_id)
        .with_context(|| format!("failed to read process record '{}'", process_id))?;
    if !process.status.is_active() {
        anyhow::bail!(
            "process '{}' is not active (status={})",
            process_id,
            process.status
        );
    }

    let manifest_path = process.manifest_path.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "process '{}' does not record a manifest path required for service binding registration",
            process_id
        )
    })?;
    let manifest = load_manifest(manifest_path)?;
    let endpoint = derive_service_endpoint_locator(
        &manifest,
        service_name,
        process.target_label.as_deref(),
        port.or(process.requested_port),
    )?;
    register_service_binding_from_parts(&manifest, service_name, endpoint)
}

pub fn sync_service_bindings_for_process(process_id: &str) -> Result<Vec<ServiceBindingRecord>> {
    let process = load_active_process(process_id)?;
    let manifest = load_manifest_from_process(&process, process_id)?;
    let mut records = Vec::new();
    for service_name in auto_bindable_service_names(&manifest) {
        let endpoint = derive_service_endpoint_locator(
            &manifest,
            &service_name,
            process.target_label.as_deref(),
            process.requested_port,
        )?;
        records.push(register_service_binding_from_parts(
            &manifest,
            &service_name,
            endpoint,
        )?);
    }
    Ok(records)
}

pub fn cleanup_service_bindings_for_process_info(
    process: &ProcessInfo,
) -> Result<Vec<ServiceBindingRecord>> {
    let Some(manifest_path) = process.manifest_path.as_deref() else {
        return Ok(Vec::new());
    };
    let manifest = load_manifest(manifest_path)?;
    let owner_scope = host_service_binding_scope(&manifest)?;
    let store = open_binding_store()?;
    let mut removed = Vec::new();
    for service_name in auto_bindable_service_names(&manifest) {
        if let Some(record) = store.delete_service_binding_by_identity(
            &owner_scope,
            &service_name,
            SERVICE_BINDING_KIND_SERVICE,
        )? {
            removed.push(record);
        }
    }
    Ok(removed)
}

fn register_service_binding_from_parts(
    manifest: &CapsuleManifest,
    service_name: &str,
    endpoint: String,
) -> Result<ServiceBindingRecord> {
    let contract = local_service_binding_contract(manifest, service_name, &endpoint)?;
    open_binding_store()?.register_service_binding(&NewServiceBindingRecord {
        owner_scope: contract.owner_scope,
        service_name: contract.service_name,
        binding_kind: contract.binding_kind,
        transport_kind: contract.transport_kind,
        adapter_kind: contract.adapter_kind,
        endpoint_locator: endpoint,
        tls_mode: contract.tls_mode,
        allowed_callers: contract.allowed_callers,
        target_hint: contract.target_hint,
    })
}

fn load_active_process(process_id: &str) -> Result<ProcessInfo> {
    let process = ProcessManager::new()?
        .read_pid(process_id)
        .with_context(|| format!("failed to read process record '{}'", process_id))?;
    if !process.status.is_active() {
        anyhow::bail!(
            "process '{}' is not active (status={})",
            process_id,
            process.status
        );
    }
    Ok(process)
}

fn load_manifest_from_process(process: &ProcessInfo, process_id: &str) -> Result<CapsuleManifest> {
    let manifest_path = process.manifest_path.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
            "process '{}' does not record a manifest path required for service binding registration",
            process_id
        )
    })?;
    load_manifest(manifest_path)
}
