use anyhow::Result;

use crate::registry::store::{RegistryStore, ServiceBindingRecord};

use super::contract::SERVICE_BINDING_TLS_MODE_EXPLICIT;
use super::parse_binding_reference;
use super::proxy as ingress_proxy;

pub fn open_binding_store() -> Result<RegistryStore> {
    let store_dir = capsule_core::config::config_dir()?.join("state");
    RegistryStore::open(&store_dir)
}

pub fn list_bindings(
    owner_scope: Option<&str>,
    service_name: Option<&str>,
    json: bool,
) -> Result<()> {
    let store = open_binding_store()?;
    let records = store.list_service_bindings(owner_scope, service_name)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&records)?);
        return Ok(());
    }

    if records.is_empty() {
        println!("No host-side service bindings registered.");
        return Ok(());
    }

    println!(
        "{:<40} {:<20} {:<16} {:<10} {:<8} ENDPOINT",
        "BINDING ID", "OWNER SCOPE", "SERVICE", "KIND", "TLS"
    );
    for record in records {
        println!(
            "{:<40} {:<20} {:<16} {:<10} {:<8} {}",
            record.binding_id,
            record.owner_scope,
            record.service_name,
            record.binding_kind,
            record.tls_mode,
            record.endpoint_locator,
        );
    }
    Ok(())
}

pub fn inspect_binding(binding_ref: &str, json: bool) -> Result<()> {
    let binding_id = parse_binding_reference(binding_ref).unwrap_or(binding_ref);
    let store = open_binding_store()?;
    let record = store
        .find_service_binding_by_id(binding_id)?
        .ok_or_else(|| {
            anyhow::anyhow!("host-side service binding '{}' was not found", binding_id)
        })?;

    if json {
        println!("{}", serde_json::to_string_pretty(&record)?);
        return Ok(());
    }

    println!("Binding ID: {}", record.binding_id);
    println!("Owner Scope: {}", record.owner_scope);
    println!("Service Name: {}", record.service_name);
    println!("Binding Kind: {}", record.binding_kind);
    println!("Transport Kind: {}", record.transport_kind);
    println!("Adapter Kind: {}", record.adapter_kind);
    println!("Endpoint Locator: {}", record.endpoint_locator);
    println!("TLS Mode: {}", record.tls_mode);
    if let Some(tls) = ingress_proxy::load_tls_bootstrap(&record.binding_id)? {
        println!("TLS Bootstrap: ready");
        println!("TLS Cert Path: {}", tls.cert_path.display());
        println!(
            "TLS Trust Installed: {}",
            if tls.system_trust_installed {
                "yes"
            } else {
                "no"
            }
        );
    } else if record.tls_mode == SERVICE_BINDING_TLS_MODE_EXPLICIT {
        println!("TLS Bootstrap: pending");
    }
    if !record.allowed_callers.is_empty() {
        println!("Allowed Callers: {}", record.allowed_callers.join(", "));
    }
    if let Some(target_hint) = record.target_hint.as_deref() {
        println!("Target Hint: {}", target_hint);
    }
    println!("Created At: {}", record.created_at);
    println!("Updated At: {}", record.updated_at);
    Ok(())
}

pub fn resolve_binding(
    owner_scope: &str,
    service_name: &str,
    binding_kind: &str,
    caller_service: Option<&str>,
    json: bool,
) -> Result<()> {
    let record = resolve_binding_record(owner_scope, service_name, binding_kind, caller_service)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&record)?);
        return Ok(());
    }

    println!("Resolved Binding: {}", record.binding_id);
    println!("Owner Scope: {}", record.owner_scope);
    println!("Service Name: {}", record.service_name);
    println!("Binding Kind: {}", record.binding_kind);
    println!("Transport Kind: {}", record.transport_kind);
    println!("Endpoint Locator: {}", record.endpoint_locator);
    if let Some(caller_service) = caller_service.filter(|value| !value.trim().is_empty()) {
        println!("Caller Service: {}", caller_service.trim());
    }
    if !record.allowed_callers.is_empty() {
        println!("Allowed Callers: {}", record.allowed_callers.join(", "));
    }
    Ok(())
}

pub fn resolve_binding_record(
    owner_scope: &str,
    service_name: &str,
    binding_kind: &str,
    caller_service: Option<&str>,
) -> Result<ServiceBindingRecord> {
    let record = open_binding_store()?
        .resolve_service_binding(owner_scope, service_name, binding_kind, caller_service)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "host-side service binding '{}:{}:{}' was not found",
                owner_scope,
                service_name,
                binding_kind
            )
        })?;
    Ok(record)
}
