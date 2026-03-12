#![allow(dead_code)]

use anyhow::{bail, Context, Result};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct RegistryDeleteArgs {
    pub scoped_id: String,
    pub registry_url: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryDeleteResult {
    pub deleted: bool,
    pub scoped_id: String,
    pub removed_capsule: bool,
    pub removed_versions: usize,
    pub removed_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RegistryErrorPayload {
    #[serde(default)]
    message: Option<String>,
}

pub fn delete_capsule(args: RegistryDeleteArgs) -> Result<RegistryDeleteResult> {
    let scoped = crate::install::parse_capsule_ref(&args.scoped_id)?;
    let base_url = normalize_registry_url(&args.registry_url)?;
    let endpoint = build_delete_endpoint(
        &base_url,
        &scoped.publisher,
        &scoped.slug,
        args.version.as_deref(),
    );

    let request = crate::registry_http::blocking_client_builder(&base_url)
        .build()
        .context("Failed to create registry delete client")?
        .delete(&endpoint);
    let request = if let Some(token) = read_ato_token() {
        request.header("authorization", format!("Bearer {}", token))
    } else {
        request
    };

    let response = request
        .send()
        .map_err(|err| anyhow::anyhow!("Failed to delete capsule via {}: {}", endpoint, err))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        let message = parse_error_message(status, &body);
        bail!("Registry delete failed ({}): {}", status.as_u16(), message);
    }

    response
        .json::<RegistryDeleteResult>()
        .context("Invalid local registry delete response")
}

fn build_delete_endpoint(
    base_url: &str,
    publisher: &str,
    slug: &str,
    version: Option<&str>,
) -> String {
    let mut endpoint = format!(
        "{}/v1/local/capsules/by/{}/{}?confirmed=true",
        base_url,
        urlencoding::encode(publisher),
        urlencoding::encode(slug)
    );
    if let Some(version) = version.map(str::trim).filter(|value| !value.is_empty()) {
        endpoint.push_str("&version=");
        endpoint.push_str(&urlencoding::encode(version));
    }
    endpoint
}

fn normalize_registry_url(raw: &str) -> Result<String> {
    crate::registry_http::normalize_registry_url(raw, "--registry")
}

fn parse_error_message(status: StatusCode, body: &str) -> String {
    let parsed = serde_json::from_str::<RegistryErrorPayload>(body).ok();
    if let Some(message) = parsed
        .and_then(|payload| payload.message)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return message;
    }
    let raw = body.trim();
    if raw.is_empty() {
        return format!("HTTP {}", status.as_u16());
    }
    raw.to_string()
}

fn read_ato_token() -> Option<String> {
    crate::auth::current_session_token()
}
