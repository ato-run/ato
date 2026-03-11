use crate::config;
use crate::error::{CapsuleError, Result};
use crate::schema_registry::SchemaRegistry;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MagUri {
    pub did_or_domain: String,
    pub schema_hash: Option<String>,
    pub merkle_root: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMagUri {
    pub did: String,
    pub schema_hash: Option<String>,
    pub merkle_root: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct DomainAnchors {
    #[serde(default)]
    anchors: HashMap<String, String>,
}

pub fn parse_mag_uri(uri: &str) -> Result<MagUri> {
    if !uri.starts_with("mag://") {
        return Err(CapsuleError::Config("invalid mag:// scheme".to_string()));
    }

    let rest = &uri[6..];
    let mut parts = rest.split('/');
    let head = parts.next().unwrap_or("");
    if head.is_empty() {
        return Err(CapsuleError::Config("missing mag:// authority".to_string()));
    }

    let mut schema_hash = None;
    let mut did_or_domain = head.to_string();

    if let Some(idx) = head.find("sha256:") {
        let (did_part, hash_part) = head.split_at(idx);
        let did_part = did_part.trim_end_matches(':');
        if did_part.is_empty() || hash_part.is_empty() {
            return Err(CapsuleError::Config(
                "invalid mag:// schema hash".to_string(),
            ));
        }
        did_or_domain = did_part.to_string();
        schema_hash = Some(hash_part.to_string());
    }

    let merkle_root = parts.next().map(|v| v.to_string());
    let path = parts.collect::<Vec<_>>().join("/");
    let path = if path.is_empty() { None } else { Some(path) };

    Ok(MagUri {
        did_or_domain,
        schema_hash,
        merkle_root,
        path,
    })
}

pub fn resolve_mag_uri(uri: &str, registry: &SchemaRegistry) -> Result<ResolvedMagUri> {
    let parsed = parse_mag_uri(uri)?;
    let did = resolve_authority(&parsed.did_or_domain)?;
    let schema_hash = match parsed.schema_hash {
        Some(hash) => Some(registry.resolve_schema_hash(&hash)?),
        None => None,
    };

    Ok(ResolvedMagUri {
        did,
        schema_hash,
        merkle_root: parsed.merkle_root,
        path: parsed.path,
    })
}

fn resolve_authority(authority: &str) -> Result<String> {
    if authority.starts_with("did:") {
        return Ok(authority.to_string());
    }

    if authority.contains('.') {
        let anchors = load_domain_anchors()?;
        if let Some(did) = anchors.anchors.get(authority) {
            return Ok(did.clone());
        }
        return Err(CapsuleError::Config(format!(
            "No domain anchor for {}",
            authority
        )));
    }

    Ok(authority.to_string())
}

fn load_domain_anchors() -> Result<DomainAnchors> {
    let path = config::config_dir()
        .map_err(|e| CapsuleError::Config(format!("Failed to resolve config dir: {}", e)))?
        .join("domain_anchors.json");

    if !path.exists() {
        return Ok(DomainAnchors::default());
    }

    let raw = fs::read_to_string(&path)
        .map_err(|e| CapsuleError::Config(format!("Failed to read domain anchors: {}", e)))?;
    serde_json::from_str(&raw)
        .map_err(|e| CapsuleError::Config(format!("Failed to parse domain anchors: {}", e)))
}
