//! Search command implementation
//!
//! Search for published packages in the Store.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read};

const DEFAULT_STORE_API_URL: &str = "https://api.ato.run";
const ENV_STORE_API_URL: &str = "ATO_STORE_API_URL";

/// Store API package summary (from GET /v1/manifest/capsules)
#[derive(Debug, Deserialize)]
struct RawCapsulesResponse {
    capsules: Vec<RawCapsuleSummary>,
    #[serde(default, alias = "nextCursor")]
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawCapsuleSummary {
    id: String,
    slug: String,
    #[serde(default)]
    scoped_id: Option<String>,
    #[serde(default, rename = "scopedId")]
    scoped_id_camel: Option<String>,
    name: String,
    description: String,
    category: String,
    #[serde(rename = "type")]
    capsule_type: String,
    price: u64,
    currency: String,
    publisher: PublisherInfo,
    #[serde(rename = "latestVersion", alias = "latest_version", default)]
    latest_version: Option<String>,
    downloads: u64,
    #[serde(rename = "createdAt", alias = "created_at")]
    created_at: String,
    #[serde(rename = "updatedAt", alias = "updated_at")]
    updated_at: String,
}

#[derive(Debug, Deserialize)]
struct RawCapsuleDetailForManifest {
    #[serde(default)]
    manifest: Option<serde_json::Value>,
    #[serde(default)]
    repository: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawDistributionForManifest {
    #[serde(rename = "artifact_url", alias = "artifactUrl")]
    artifact_url: String,
}

#[derive(Debug, Deserialize)]
struct GitHubRepositoryInfo {
    default_branch: String,
}

#[derive(Debug, Serialize)]
pub struct CapsuleSummary {
    pub id: String,
    pub slug: String,
    pub scoped_id: Option<String>,
    pub name: String,
    pub description: String,
    pub category: String,
    #[serde(rename = "type")]
    pub capsule_type: String,
    pub price: u64,
    pub currency: String,
    pub publisher: PublisherInfo,
    #[serde(rename = "latestVersion", alias = "latest_version", default)]
    pub latest_version: Option<String>,
    pub downloads: u64,
    #[serde(rename = "createdAt", alias = "created_at")]
    pub created_at: String,
    #[serde(rename = "updatedAt", alias = "updated_at")]
    pub updated_at: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PublisherInfo {
    pub handle: String,
    #[serde(rename = "authorDid", alias = "author_did")]
    pub author_did: String,
    pub verified: bool,
}

impl From<RawCapsuleSummary> for CapsuleSummary {
    fn from(raw: RawCapsuleSummary) -> Self {
        let scoped_id = raw.scoped_id.or(raw.scoped_id_camel);
        Self {
            id: raw.id,
            slug: raw.slug,
            scoped_id,
            name: raw.name,
            description: raw.description,
            category: raw.category,
            capsule_type: raw.capsule_type,
            price: raw.price,
            currency: raw.currency,
            publisher: raw.publisher,
            latest_version: raw.latest_version,
            downloads: raw.downloads,
            created_at: raw.created_at,
            updated_at: raw.updated_at,
        }
    }
}

fn normalize_base_url(raw: &str) -> String {
    raw.trim().trim_end_matches('/').to_string()
}

fn default_store_api_url() -> String {
    std::env::var(ENV_STORE_API_URL)
        .ok()
        .map(|value| normalize_base_url(&value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_STORE_API_URL.to_string())
}

/// Search result
#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub capsules: Vec<CapsuleSummary>,
    pub total: usize,
    pub next_cursor: Option<String>,
}

/// Search for packages in the store
pub async fn search_capsules(
    query: Option<&str>,
    category: Option<&str>,
    tags: Option<&[String]>,
    limit: Option<usize>,
    cursor: Option<&str>,
    registry_url: Option<&str>,
    json_output: bool,
) -> Result<SearchResult> {
    // Resolve registry URL
    let registry = if let Some(url) = registry_url {
        normalize_base_url(url)
    } else {
        let info = default_store_api_url();
        if !json_output {
            println!("📡 Using registry: {}", info);
        }
        info
    };

    let client = reqwest::Client::new();

    // Build query parameters
    let mut url = format!("{}/v1/manifest/capsules", registry);
    let mut params = Vec::new();

    if let Some(q) = query {
        params.push(format!("q={}", urlencoding::encode(q)));
    }
    if let Some(c) = category {
        params.push(format!("category={}", urlencoding::encode(c)));
    }
    if let Some(tags) = tags {
        for tag in tags
            .iter()
            .map(|tag| tag.trim())
            .filter(|tag| !tag.is_empty())
        {
            params.push(format!("tag={}", urlencoding::encode(tag)));
        }
    }
    let limit_val = limit.unwrap_or(20).min(50);
    params.push(format!("limit={}", limit_val));
    if let Some(c) = cursor {
        params.push(format!("cursor={}", urlencoding::encode(c)));
    }

    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
    }

    let response: RawCapsulesResponse = crate::registry::http::with_ato_token(client.get(&url))
        .send()
        .await
        .with_context(|| format!("Failed to search capsules: {}", registry))?
        .json()
        .await
        .with_context(|| "Invalid search response")?;

    let capsules: Vec<CapsuleSummary> = response.capsules.into_iter().map(Into::into).collect();
    let total = capsules.len();

    if !json_output {
        if total == 0 {
            println!("🔍 No packages found.");
        } else {
            println!("🔍 Found {} package(s):", total);
        }

        for (index, capsule) in capsules.iter().enumerate() {
            println!();
            println!("{}. {} ({})", index + 1, capsule.name, capsule.slug);
            if !capsule.description.is_empty() {
                println!("   {}", capsule.description);
            }
            println!(
                "   Category: {} | Type: {} | Version: {}",
                capsule.category,
                capsule.capsule_type,
                capsule.latest_version.as_deref().unwrap_or("unknown")
            );
            println!(
                "   Publisher: {}{} | Downloads: {}",
                capsule.publisher.handle,
                if capsule.publisher.verified {
                    " ✓"
                } else {
                    ""
                },
                capsule.downloads
            );
            if capsule.price == 0 {
                println!("   Price: Free");
            } else {
                println!("   Price: {} {}", capsule.price, capsule.currency);
            }
            let scoped_id = capsule
                .scoped_id
                .clone()
                .unwrap_or_else(|| format!("{}/{}", capsule.publisher.handle, capsule.slug));
            println!("   Install: ato install {}", scoped_id);
        }

        if let Some(ref next) = response.next_cursor {
            println!();
            println!("📄 Next cursor: {}", next);
            println!("   Continue: ato search --cursor {}", next);
        }
    }

    Ok(SearchResult {
        capsules,
        total,
        next_cursor: response.next_cursor,
    })
}

pub async fn fetch_capsule_manifest(scoped_id: &str, registry_url: Option<&str>) -> Result<String> {
    let (publisher, slug) = parse_scoped_id(scoped_id)?;
    let registry = if let Some(url) = registry_url {
        normalize_base_url(url)
    } else {
        default_store_api_url()
    };

    let client = reqwest::Client::new();
    let url = format!(
        "{}/v1/capsules/by/{}/{}",
        registry,
        urlencoding::encode(&publisher),
        urlencoding::encode(&slug)
    );

    let detail: RawCapsuleDetailForManifest =
        crate::registry::http::with_ato_token(client.get(&url))
            .send()
            .await
            .with_context(|| format!("Failed to fetch capsule detail: {}", scoped_id))?
            .json()
            .await
            .with_context(|| format!("Invalid capsule detail response: {}", scoped_id))?;

    if let Some(manifest) = detail.manifest.as_ref().and_then(json_to_toml_value) {
        if let Some(table) = manifest.as_table() {
            return toml::to_string_pretty(table)
                .with_context(|| "Failed to serialize manifest TOML");
        }
    }

    if let Some(repository) = detail.repository {
        if let Some(manifest) = fetch_manifest_from_github(&client, &repository).await? {
            return Ok(manifest);
        }
    }

    if let Some(manifest) =
        fetch_manifest_from_distribution_artifact(&client, &registry, &publisher, &slug).await?
    {
        return Ok(manifest);
    }

    anyhow::bail!("capsule.toml is not available from this capsule source")
}

fn parse_scoped_id(scoped_id: &str) -> Result<(String, String)> {
    let mut parts = scoped_id.trim().split('/');
    let publisher = parts.next().unwrap_or_default().trim();
    let slug = parts.next().unwrap_or_default().trim();
    if publisher.is_empty() || slug.is_empty() || parts.next().is_some() {
        anyhow::bail!("invalid scoped id: {}", scoped_id);
    }
    Ok((publisher.to_string(), slug.to_string()))
}

fn parse_github_repo(repository: &str) -> Option<(String, String)> {
    let trimmed = repository.trim().trim_end_matches('/');
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);

    let mut parts = without_scheme.split('/');
    let host = parts.next()?.to_ascii_lowercase();
    if host != "github.com" && host != "www.github.com" {
        return None;
    }
    let owner = parts.next()?.trim();
    let repo = parts.next()?.trim().trim_end_matches(".git");
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

async fn fetch_manifest_from_github(
    client: &reqwest::Client,
    repository: &str,
) -> Result<Option<String>> {
    let Some((owner, repo)) = parse_github_repo(repository) else {
        return Ok(None);
    };

    let api_url = format!("https://api.github.com/repos/{}/{}", owner, repo);
    let repo_info = match client
        .get(&api_url)
        .header(reqwest::header::USER_AGENT, "ato-cli")
        .send()
        .await
    {
        Ok(response) => response.json::<GitHubRepositoryInfo>().await.ok(),
        Err(_) => None,
    };

    let mut branches = Vec::new();
    if let Some(info) = repo_info {
        branches.push(info.default_branch);
    }
    branches.push("main".to_string());
    branches.push("master".to_string());
    branches.sort();
    branches.dedup();

    for branch in branches {
        let raw_url = format!(
            "https://raw.githubusercontent.com/{}/{}/{}/capsule.toml",
            owner, repo, branch
        );
        let response = match client
            .get(&raw_url)
            .header(reqwest::header::USER_AGENT, "ato-cli")
            .send()
            .await
        {
            Ok(res) => res,
            Err(_) => continue,
        };
        if !response.status().is_success() {
            continue;
        }
        let body = match response.text().await {
            Ok(text) => text,
            Err(_) => continue,
        };
        if body.trim().is_empty() {
            continue;
        }
        return Ok(Some(body));
    }

    Ok(None)
}

async fn fetch_manifest_from_distribution_artifact(
    client: &reqwest::Client,
    registry: &str,
    publisher: &str,
    slug: &str,
) -> Result<Option<String>> {
    let distribution_url = format!(
        "{}/v1/capsules/by/{}/{}/distributions",
        registry,
        urlencoding::encode(publisher),
        urlencoding::encode(slug)
    );
    let distribution_response =
        match crate::registry::http::with_ato_token(client.get(&distribution_url))
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => return Ok(None),
        };
    if !distribution_response.status().is_success() {
        return Ok(None);
    }
    let distribution = match distribution_response
        .json::<RawDistributionForManifest>()
        .await
    {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };

    let artifact_response = match client.get(&distribution.artifact_url).send().await {
        Ok(response) => response,
        Err(_) => return Ok(None),
    };
    if !artifact_response.status().is_success() {
        return Ok(None);
    }
    let bytes = match artifact_response.bytes().await {
        Ok(bytes) => bytes,
        Err(_) => return Ok(None),
    };
    let manifest = match extract_manifest_from_capsule_archive(bytes.as_ref()) {
        Ok(manifest) => manifest,
        Err(_) => return Ok(None),
    };
    Ok(Some(manifest))
}

fn extract_manifest_from_capsule_archive(bytes: &[u8]) -> Result<String> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive
        .entries()
        .context("Failed to read .capsule archive entries")?;

    for entry in entries {
        let mut entry = entry.context("Invalid .capsule entry")?;
        let entry_path = entry
            .path()
            .context("Failed to read archive entry path")?
            .to_string_lossy()
            .to_string();
        if entry_path == "capsule.toml" {
            let mut manifest = String::new();
            entry
                .read_to_string(&mut manifest)
                .context("Failed to read capsule.toml from artifact")?;
            return Ok(manifest);
        }
    }

    anyhow::bail!("Invalid artifact: capsule.toml not found in .capsule archive")
}

fn json_to_toml_value(value: &serde_json::Value) -> Option<toml::Value> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::Bool(v) => Some(toml::Value::Boolean(*v)),
        serde_json::Value::Number(v) => {
            if let Some(i) = v.as_i64() {
                Some(toml::Value::Integer(i))
            } else {
                v.as_f64().map(toml::Value::Float)
            }
        }
        serde_json::Value::String(v) => Some(toml::Value::String(v.clone())),
        serde_json::Value::Array(values) => values
            .iter()
            .map(json_to_toml_value)
            .collect::<Option<Vec<_>>>()
            .map(toml::Value::Array),
        serde_json::Value::Object(map) => {
            let mut table = toml::map::Map::new();
            for (key, value) in map {
                if let Some(converted) = json_to_toml_value(value) {
                    table.insert(key.clone(), converted);
                }
            }
            Some(toml::Value::Table(table))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RawCapsulesResponse;
    use super::*;

    #[test]
    fn parses_capsules_response_when_latest_version_is_null() {
        let raw = r#"{
            "capsules": [{
                "id": "01TEST",
                "slug": "sample-capsule",
                "name": "sample-capsule",
                "description": "sample",
                "category": "tools",
                "type": "app",
                "price": 0,
                "currency": "usd",
                "publisher": {
                    "handle": "koh0920",
                    "authorDid": "did:key:z6Mk...",
                    "verified": true
                },
                "latestVersion": null,
                "downloads": 2,
                "createdAt": "2026-02-14 05:55:45",
                "updatedAt": "2026-02-23T05:51:55.877Z"
            }],
            "next_cursor": null
        }"#;

        let parsed: RawCapsulesResponse = serde_json::from_str(raw).expect("should parse");
        assert_eq!(parsed.capsules.len(), 1);
        assert!(parsed.capsules[0].latest_version.is_none());
    }

    #[test]
    fn parses_capsules_response_with_both_scoped_keys() {
        let raw = r#"{
            "capsules": [{
                "id": "01TEST",
                "slug": "sample-capsule",
                "scoped_id": "koh0920/sample-capsule",
                "scopedId": "koh0920/sample-capsule",
                "name": "sample-capsule",
                "description": "sample",
                "category": "tools",
                "type": "app",
                "price": 0,
                "currency": "usd",
                "publisher": {
                    "handle": "koh0920",
                    "authorDid": "did:key:z6Mk...",
                    "verified": true
                },
                "latestVersion": "1.0.0",
                "downloads": 2,
                "createdAt": "2026-02-14 05:55:45",
                "updatedAt": "2026-02-23T05:51:55.877Z"
            }],
            "next_cursor": null
        }"#;

        let parsed: RawCapsulesResponse = serde_json::from_str(raw).expect("should parse");
        assert_eq!(parsed.capsules.len(), 1);
    }

    #[test]
    fn extract_manifest_from_capsule_archive_succeeds() {
        let manifest = r#"schema_version = "0.2"
name = "sample"
version = "1.0.0"
type = "app"
default_target = "cli"
"#;
        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            let mut header = tar::Header::new_gnu();
            header.set_path("capsule.toml").expect("path");
            header.set_mode(0o644);
            header.set_size(manifest.len() as u64);
            header.set_cksum();
            builder
                .append_data(&mut header, "capsule.toml", manifest.as_bytes())
                .expect("append");
            builder.finish().expect("finish");
        }

        let extracted = extract_manifest_from_capsule_archive(&bytes).expect("extract");
        assert!(extracted.contains("name = \"sample\""));
    }
}
