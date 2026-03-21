use anyhow::Result;

use crate::registry::RegistryResolver;

pub(crate) async fn resolve_registry_url(registry_url: Option<&str>) -> Result<String> {
    if let Some(url) = registry_url {
        return Ok(url.to_string());
    }

    let resolver = RegistryResolver::default();
    Ok(resolver.resolve("localhost").await?.url)
}

pub(crate) async fn resolve_registry_url_with_log(
    registry_url: Option<&str>,
    emit_log: bool,
) -> Result<String> {
    if let Some(url) = registry_url {
        return Ok(url.to_string());
    }

    let resolver = RegistryResolver::default();
    let info = resolver.resolve("localhost").await?;
    if emit_log {
        eprintln!(
            "📡 Using registry: {} ({})",
            info.url,
            format!("{:?}", info.source).to_lowercase()
        );
    }
    Ok(info.url)
}

pub(crate) async fn resolve_normalized_registry_url(
    registry_url: Option<&str>,
    explicit_label: &str,
    resolved_label: &str,
) -> Result<String> {
    if let Some(url) = registry_url {
        return crate::registry::http::normalize_registry_url(url, explicit_label);
    }

    let resolver = RegistryResolver::default();
    let info = resolver.resolve("localhost").await?;
    crate::registry::http::normalize_registry_url(&info.url, resolved_label)
}
