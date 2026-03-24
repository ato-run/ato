use anyhow::Result;
use std::net::IpAddr;

use capsule_core::types::CapsuleManifest;

pub const SERVICE_BINDING_KIND_INGRESS: &str = "ingress";
pub const SERVICE_BINDING_KIND_SERVICE: &str = "service";
pub const SERVICE_BINDING_ADAPTER_REVERSE_PROXY: &str = "reverse_proxy";
pub const SERVICE_BINDING_ADAPTER_LOCAL_SERVICE: &str = "local_service";
pub const SERVICE_BINDING_TLS_MODE_DISABLED: &str = "disabled";
pub const SERVICE_BINDING_TLS_MODE_EXPLICIT: &str = "explicit";

#[derive(Debug, Clone)]
pub(super) struct ServiceBindingContract {
    pub owner_scope: String,
    pub service_name: String,
    pub binding_kind: String,
    pub transport_kind: String,
    pub adapter_kind: String,
    pub tls_mode: String,
    pub allowed_callers: Vec<String>,
    pub target_hint: Option<String>,
}

pub fn host_service_binding_scope(manifest: &CapsuleManifest) -> Result<String> {
    manifest.host_service_binding_scope().ok_or_else(|| {
        anyhow::anyhow!(
            "manifest name or service_binding_scope is required before host-side service binding can be registered"
        )
    })
}

pub(super) fn normalize_endpoint_locator(raw: &str) -> Result<String> {
    let parsed = reqwest::Url::parse(raw.trim())?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed.to_string()),
        scheme => anyhow::bail!(
            "host-side service binding endpoint must use http or https scheme (got '{}')",
            scheme
        ),
    }
}

pub(super) fn normalize_local_service_locator(raw: &str) -> Result<String> {
    let normalized = normalize_endpoint_locator(raw)?;
    let parsed = reqwest::Url::parse(&normalized)?;
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("local service binding endpoint is missing a host"))?;
    let is_loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false);
    if !is_loopback {
        anyhow::bail!(
            "local service binding endpoint must use a loopback host such as localhost or 127.0.0.1"
        );
    }
    Ok(normalized)
}

fn service_contract(
    manifest: &CapsuleManifest,
    service_name: &str,
) -> Result<ServiceBindingContract> {
    let service = manifest
        .services
        .as_ref()
        .and_then(|services| services.get(service_name))
        .ok_or_else(|| {
            anyhow::anyhow!("service '{}' is not declared in the manifest", service_name)
        })?;

    Ok(ServiceBindingContract {
        owner_scope: host_service_binding_scope(manifest)?,
        service_name: service_name.to_string(),
        binding_kind: String::new(),
        transport_kind: String::new(),
        adapter_kind: String::new(),
        tls_mode: String::new(),
        allowed_callers: service
            .network
            .as_ref()
            .map(|network| network.allow_from.clone())
            .unwrap_or_default(),
        target_hint: service.target.clone(),
    })
}

pub(super) fn ingress_binding_contract(
    manifest: &CapsuleManifest,
    service_name: &str,
    endpoint_locator: &str,
) -> Result<ServiceBindingContract> {
    let service = manifest
        .services
        .as_ref()
        .and_then(|services| services.get(service_name))
        .ok_or_else(|| {
            anyhow::anyhow!("service '{}' is not declared in the manifest", service_name)
        })?;

    let is_publishable = service_name == "main"
        || service
            .network
            .as_ref()
            .map(|network| network.publish)
            .unwrap_or(false);
    if !is_publishable {
        anyhow::bail!(
            "service '{}' is not marked for host-side publication; set services.{}.network.publish = true or use 'main'",
            service_name,
            service_name
        );
    }

    let transport_kind = if endpoint_locator.starts_with("https://") {
        "https"
    } else {
        "http"
    };
    let tls_mode = if transport_kind == "https" {
        SERVICE_BINDING_TLS_MODE_EXPLICIT
    } else {
        SERVICE_BINDING_TLS_MODE_DISABLED
    };

    let mut contract = service_contract(manifest, service_name)?;
    contract.binding_kind = SERVICE_BINDING_KIND_INGRESS.to_string();
    contract.transport_kind = transport_kind.to_string();
    contract.adapter_kind = SERVICE_BINDING_ADAPTER_REVERSE_PROXY.to_string();
    contract.tls_mode = tls_mode.to_string();
    Ok(contract)
}

pub(super) fn local_service_binding_contract(
    manifest: &CapsuleManifest,
    service_name: &str,
    endpoint_locator: &str,
) -> Result<ServiceBindingContract> {
    let transport_kind = if endpoint_locator.starts_with("https://") {
        "https"
    } else {
        "http"
    };
    let tls_mode = if transport_kind == "https" {
        SERVICE_BINDING_TLS_MODE_EXPLICIT
    } else {
        SERVICE_BINDING_TLS_MODE_DISABLED
    };

    let mut contract = service_contract(manifest, service_name)?;
    contract.binding_kind = SERVICE_BINDING_KIND_SERVICE.to_string();
    contract.transport_kind = transport_kind.to_string();
    contract.adapter_kind = SERVICE_BINDING_ADAPTER_LOCAL_SERVICE.to_string();
    contract.tls_mode = tls_mode.to_string();
    Ok(contract)
}

pub(super) fn auto_bindable_service_names(manifest: &CapsuleManifest) -> Vec<String> {
    let Some(services) = manifest.services.as_ref() else {
        return Vec::new();
    };

    let mut names = services
        .iter()
        .filter_map(|(service_name, service)| {
            let network = service.network.as_ref();
            let should_register = service_name == "main"
                || network.map(|network| network.publish).unwrap_or(false)
                || network
                    .map(|network| !network.allow_from.is_empty())
                    .unwrap_or(false);
            should_register.then(|| service_name.clone())
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

pub(super) fn derive_service_upstream_locator(
    manifest: &CapsuleManifest,
    service_name: &str,
) -> Result<String> {
    derive_service_endpoint_locator(manifest, service_name, None, None)
}

pub(super) fn derive_service_endpoint_locator(
    manifest: &CapsuleManifest,
    service_name: &str,
    default_target_override: Option<&str>,
    port_override: Option<u16>,
) -> Result<String> {
    let service = manifest
        .services
        .as_ref()
        .and_then(|services| services.get(service_name))
        .ok_or_else(|| {
            anyhow::anyhow!("service '{}' is not declared in the manifest", service_name)
        })?;
    let target_label = service
        .target
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .or(default_target_override
            .map(str::trim)
            .filter(|value| !value.is_empty()))
        .unwrap_or(manifest.default_target.trim());
    if target_label.is_empty() {
        anyhow::bail!(
            "service '{}' does not resolve to a target with a listening port",
            service_name
        );
    }
    let port = port_override
        .or_else(|| {
            manifest.targets.as_ref().and_then(|targets| {
                targets
                    .named
                    .get(target_label)
                    .and_then(|target| target.port)
                    .or(targets.port)
            })
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "service '{}' target '{}' does not declare a listening port required for host-side binding",
                service_name,
                target_label
            )
        })?;
    Ok(format!("http://127.0.0.1:{port}/"))
}
