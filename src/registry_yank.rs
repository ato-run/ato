#![allow(dead_code)]

use anyhow::{bail, Context, Result};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct RegistryYankArgs {
    pub scoped_id: String,
    pub manifest_hash: String,
    pub registry_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryYankResult {
    pub scoped_id: String,
    pub target_manifest_hash: String,
    pub yanked: bool,
}

#[derive(Debug, Deserialize)]
struct RegistryErrorPayload {
    #[serde(default)]
    message: Option<String>,
}

pub fn yank_manifest(args: RegistryYankArgs) -> Result<RegistryYankResult> {
    let scoped = crate::install::parse_capsule_ref(&args.scoped_id)?;
    let base_url = normalize_registry_url(&args.registry_url)?;
    let endpoint = format!("{}/v1/manifest/yank", base_url);
    let payload = serde_json::json!({
        "scoped_id": scoped.scoped_id,
        "target_manifest_hash": args.manifest_hash,
    });

    let request = crate::registry_http::blocking_client_builder(&base_url)
        .build()
        .context("Failed to create registry yank client")?
        .post(&endpoint)
        .json(&payload);
    let request = if let Some(token) = read_ato_token() {
        request.header("authorization", format!("Bearer {}", token))
    } else {
        request
    };

    let response = request
        .send()
        .map_err(|err| anyhow::anyhow!("Failed to yank manifest via {}: {}", endpoint, err))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        let message = parse_error_message(status, &body);
        bail!("Registry yank failed ({}): {}", status.as_u16(), message);
    }

    response
        .json::<RegistryYankResult>()
        .context("Invalid local registry yank response")
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
