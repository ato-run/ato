use anyhow::{Context, Result};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

pub(super) fn resolve_public_base_url(headers: &HeaderMap, fallback: &str) -> String {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| *v == "http" || *v == "https")
        .unwrap_or("http");

    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(header::HOST))
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.split(',').next().unwrap_or(v).trim().to_string());

    if let Some(host) = host {
        return format!("{}://{}", scheme, host);
    }

    fallback.to_string()
}

pub(super) fn normalize_registry_base_url_for_local_run(
    request_base_url: &str,
    listen_url: &str,
) -> String {
    rewrite_wildcard_registry_host(request_base_url).unwrap_or_else(|| {
        rewrite_wildcard_registry_host(listen_url).unwrap_or_else(|| request_base_url.to_string())
    })
}

fn rewrite_wildcard_registry_host(raw: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(raw).ok()?;
    let host = url.host_str()?.to_string();
    let replacement = match host.as_str() {
        "0.0.0.0" => "127.0.0.1",
        "::" | "[::]" => "::1",
        _ => return Some(raw.to_string()),
    };
    url.set_host(Some(replacement)).ok()?;
    Some(url.to_string().trim_end_matches('/').to_string())
}

pub(super) fn get_required_header(headers: &HeaderMap, key: &str) -> Result<String> {
    headers
        .get(key)
        .and_then(|value| value.to_str().ok())
        .map(|v| v.to_string())
        .ok_or_else(|| anyhow::anyhow!("required header '{}' is missing", key))
}

pub(super) fn get_optional_header(headers: &HeaderMap, key: &str) -> Option<String> {
    headers
        .get(key)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub(super) fn parse_required_u32_header(headers: &HeaderMap, key: &str) -> Result<u32> {
    let value = get_required_header(headers, key)?;
    value
        .trim()
        .parse::<u32>()
        .with_context(|| format!("invalid '{}' header value: {}", key, value))
}

pub(super) fn json_error(
    status: StatusCode,
    error: &str,
    message: &str,
) -> axum::response::Response {
    (
        status,
        Json(json!({
            "error": error,
            "message": message
        })),
    )
        .into_response()
}
