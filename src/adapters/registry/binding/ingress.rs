use anyhow::Result;
use std::path::Path;

use super::contract::{
    derive_service_upstream_locator, normalize_endpoint_locator, SERVICE_BINDING_KIND_INGRESS,
    SERVICE_BINDING_TLS_MODE_EXPLICIT,
};
use super::manifest::load_manifest;
use super::parse_binding_reference;
use super::proxy as ingress_proxy;
use super::store::open_binding_store;

pub fn bootstrap_ingress_tls(
    binding_ref: &str,
    install_system_trust: bool,
    yes: bool,
    json: bool,
) -> Result<()> {
    let binding_id = parse_binding_reference(binding_ref).unwrap_or(binding_ref);
    let record = open_binding_store()?
        .find_service_binding_by_id(binding_id)?
        .ok_or_else(|| {
            anyhow::anyhow!("host-side service binding '{}' was not found", binding_id)
        })?;
    if record.binding_kind != SERVICE_BINDING_KIND_INGRESS {
        anyhow::bail!(
            "TLS bootstrap currently supports only ingress bindings (got '{}')",
            record.binding_kind
        );
    }
    if record.tls_mode != SERVICE_BINDING_TLS_MODE_EXPLICIT {
        anyhow::bail!(
            "binding '{}' does not require explicit TLS bootstrap because tls_mode={}.",
            record.binding_id,
            record.tls_mode
        );
    }

    let endpoint = reqwest::Url::parse(&record.endpoint_locator)?;
    let endpoint_host = endpoint.host_str().ok_or_else(|| {
        anyhow::anyhow!("binding '{}' endpoint is missing a host", record.binding_id)
    })?;
    let tls =
        ingress_proxy::bootstrap_tls(&record.binding_id, endpoint_host, install_system_trust, yes)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&tls)?);
        return Ok(());
    }

    println!("✅ Bootstrapped ingress TLS for {}", record.binding_id);
    println!("   endpoint_host: {}", tls.endpoint_host);
    println!("   cert_path: {}", tls.cert_path.display());
    println!("   key_path: {}", tls.key_path.display());
    println!(
        "   system_trust_installed: {}",
        if tls.system_trust_installed {
            "yes"
        } else {
            "no"
        }
    );
    Ok(())
}

pub fn serve_ingress_binding(
    binding_ref: &str,
    manifest_path: &Path,
    upstream_url: Option<&str>,
) -> Result<()> {
    let binding_id = parse_binding_reference(binding_ref).unwrap_or(binding_ref);
    let record = open_binding_store()?
        .find_service_binding_by_id(binding_id)?
        .ok_or_else(|| {
            anyhow::anyhow!("host-side service binding '{}' was not found", binding_id)
        })?;
    if record.binding_kind != SERVICE_BINDING_KIND_INGRESS {
        anyhow::bail!(
            "host-side proxy serving currently supports only ingress bindings (got '{}')",
            record.binding_kind
        );
    }

    let manifest = load_manifest(manifest_path)?;
    let upstream_locator = match upstream_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => normalize_endpoint_locator(value)?,
        None => derive_service_upstream_locator(&manifest, &record.service_name)?,
    };

    println!(
        "▶️  Serving ingress binding {} on {} -> {}",
        record.binding_id, record.endpoint_locator, upstream_locator
    );
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(ingress_proxy::serve(ingress_proxy::IngressProxyConfig {
        binding_id: record.binding_id,
        endpoint_locator: record.endpoint_locator,
        upstream_locator,
        tls_mode: record.tls_mode,
    }))
}
