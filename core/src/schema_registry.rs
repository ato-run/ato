use crate::config;
use crate::error::{CapsuleError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SchemaRegistry {
    pub aliases: HashMap<String, String>,
}

impl SchemaRegistry {
    pub fn load() -> Result<Self> {
        let path = registry_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(&path)
            .map_err(|e| CapsuleError::Config(format!("Failed to read registry: {}", e)))?;
        serde_json::from_str(&raw)
            .map_err(|e| CapsuleError::Config(format!("Failed to parse registry: {}", e)))
    }

    pub fn save(&self) -> Result<()> {
        let path = registry_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                CapsuleError::Config(format!("Failed to create registry dir: {}", e))
            })?;
        }
        let raw = serde_json::to_string_pretty(self)
            .map_err(|e| CapsuleError::Config(format!("Failed to serialize registry: {}", e)))?;
        fs::write(&path, raw)
            .map_err(|e| CapsuleError::Config(format!("Failed to write registry: {}", e)))?;
        Ok(())
    }

    pub fn resolve_alias(&self, alias: &str) -> Option<String> {
        self.aliases.get(alias).cloned()
    }

    pub fn resolve_schema_hash(&self, schema_id: &str) -> Result<String> {
        if schema_id.starts_with("sha256:") {
            validate_sha256_hash(schema_id)?;
            return Ok(schema_id.to_string());
        }

        if let Some(hash) = self.resolve_alias(schema_id) {
            validate_sha256_hash(&hash)?;
            return Ok(hash);
        }

        Err(CapsuleError::Config(format!(
            "Unknown schema alias: {}",
            schema_id
        )))
    }

    pub fn register_alias(&mut self, alias: &str, schema_hash: &str) {
        self.aliases
            .insert(alias.to_string(), schema_hash.to_string());
    }

    pub fn hash_schema_value(value: &serde_json::Value) -> Result<String> {
        let canonical = serde_jcs::to_vec(value)
            .map_err(|e| CapsuleError::Config(format!("Failed to canonicalize schema: {}", e)))?;
        Ok(format!("sha256:{}", sha256_hex(&canonical)))
    }

    pub fn hash_schema_bytes(schema: &[u8]) -> Result<String> {
        let value: serde_json::Value = serde_json::from_slice(schema)
            .map_err(|e| CapsuleError::Config(format!("Invalid schema JSON: {}", e)))?;
        Self::hash_schema_value(&value)
    }
}

fn registry_path() -> Result<PathBuf> {
    Ok(config::config_dir()?.join("schema_registry.json"))
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    let digest = hasher.finalize();
    hex::encode(digest)
}

fn validate_sha256_hash(schema_hash: &str) -> Result<()> {
    let Some(hash) = schema_hash.strip_prefix("sha256:") else {
        return Err(CapsuleError::Config(format!(
            "Schema hash must start with sha256:, got {}",
            schema_hash
        )));
    };
    if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CapsuleError::Config(format!(
            "Schema hash has invalid format: {}",
            schema_hash
        )));
    }
    Ok(())
}
