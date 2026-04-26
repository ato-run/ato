use crate::common::hash::sha256_hex;
use crate::config;
use crate::error::{CapsuleError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// Minimal JSON Schema stubs for Foundation-standard aliases (std.*).
///
/// These schemas are bundled at compile time so that conformant ato implementations
/// can resolve `std.*` aliases without a network round-trip to the Foundation registry.
/// The JCS-canonicalized SHA-256 of each schema body becomes the canonical alias hash.
///
/// Schema versioning is independent of URL spec versioning (see §T-2 of the spec).
const STD_SCHEMA_SOURCES: &[(&str, &str)] = &[
    (
        "std.todo.v1",
        r#"{"$id":"ato:std.todo.v1","$schema":"https://json-schema.org/draft/2020-12/schema","properties":{"done":{"type":"boolean"},"due":{"format":"date","type":"string"},"title":{"type":"string"}},"required":["title"],"type":"object"}"#,
    ),
    (
        "std.note.v1",
        r#"{"$id":"ato:std.note.v1","$schema":"https://json-schema.org/draft/2020-12/schema","properties":{"content":{"type":"string"},"tags":{"items":{"type":"string"},"type":"array"}},"required":["content"],"type":"object"}"#,
    ),
    (
        "std.task.v1",
        r#"{"$id":"ato:std.task.v1","$schema":"https://json-schema.org/draft/2020-12/schema","properties":{"assignee":{"type":"string"},"status":{"enum":["pending","in-progress","done"],"type":"string"},"title":{"type":"string"}},"required":["status","title"],"type":"object"}"#,
    ),
    (
        "std.event.v1",
        r#"{"$id":"ato:std.event.v1","$schema":"https://json-schema.org/draft/2020-12/schema","properties":{"at":{"format":"date-time","type":"string"},"kind":{"type":"string"},"payload":{"type":"object"}},"required":["at","kind"],"type":"object"}"#,
    ),
];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SchemaRegistry {
    pub aliases: HashMap<String, String>,
}

impl SchemaRegistry {
    /// Load the user's registry from disk and merge in bundled Foundation `std.*` aliases.
    ///
    /// User-defined aliases take precedence — a user may override a `std.*` alias by writing
    /// an explicit entry in `~/.ato/schema_registry.json`.
    pub fn load() -> Result<Self> {
        let mut registry = Self::load_bundled()?;

        let path = registry_path()?;
        if path.exists() {
            let raw = fs::read_to_string(&path)
                .map_err(|e| CapsuleError::Config(format!("Failed to read registry: {}", e)))?;
            let user: SchemaRegistry = serde_json::from_str(&raw)
                .map_err(|e| CapsuleError::Config(format!("Failed to parse registry: {}", e)))?;
            // User aliases win over bundled defaults.
            for (alias, hash) in user.aliases {
                registry.aliases.insert(alias, hash);
            }
        }

        Ok(registry)
    }

    /// Build a registry pre-populated with bundled Foundation `std.*` aliases.
    ///
    /// Hashes are computed at runtime from the embedded schema JSON so that the source JSON
    /// remains human-readable and auditable without a separate hash-generation step.
    pub fn load_bundled() -> Result<Self> {
        let mut aliases = HashMap::new();
        for (alias, schema_json) in STD_SCHEMA_SOURCES {
            match Self::hash_schema_bytes(schema_json.as_bytes()) {
                Ok(hash) => {
                    aliases.insert(alias.to_string(), hash);
                }
                Err(e) => {
                    // Bundled schemas should never be malformed; treat as a bug.
                    return Err(CapsuleError::Config(format!(
                        "Failed to hash bundled schema '{}': {}",
                        alias, e
                    )));
                }
            }
        }
        Ok(Self { aliases })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_aliases_are_present() {
        let registry = SchemaRegistry::load_bundled().unwrap();
        for (alias, _) in STD_SCHEMA_SOURCES {
            assert!(
                registry.aliases.contains_key(*alias),
                "bundled alias missing: {}",
                alias
            );
        }
    }

    #[test]
    fn bundled_alias_hashes_are_valid_sha256() {
        let registry = SchemaRegistry::load_bundled().unwrap();
        for (alias, _) in STD_SCHEMA_SOURCES {
            let hash = registry.aliases.get(*alias).unwrap();
            assert!(
                hash.starts_with("sha256:"),
                "alias {} hash should start with sha256:",
                alias
            );
            assert_eq!(hash.len(), 7 + 64, "alias {} hash has wrong length", alias);
        }
    }

    #[test]
    fn resolve_std_todo_alias() {
        let registry = SchemaRegistry::load_bundled().unwrap();
        let hash = registry.resolve_schema_hash("std.todo.v1").unwrap();
        assert!(hash.starts_with("sha256:"));
    }

    #[test]
    fn user_alias_overrides_bundled() {
        let mut registry = SchemaRegistry::load_bundled().unwrap();
        let custom_hash = format!("sha256:{}", "a".repeat(64));
        registry.register_alias("std.todo.v1", &custom_hash);
        let resolved = registry.resolve_schema_hash("std.todo.v1").unwrap();
        assert_eq!(resolved, custom_hash);
    }
}
