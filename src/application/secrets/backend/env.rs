use anyhow::Result;

use super::traits::{BackendEntry, SecretBackend, SecretKey};

/// Read-only backend that reads `ATO_SECRET_<KEY>` environment variables.
///
/// Intended for CI environments where secrets are injected via env vars.
/// Takes the highest priority in the chain.
pub(crate) struct EnvBackend;

impl EnvBackend {
    pub(crate) fn new() -> Self {
        Self
    }

    fn env_key_for(name: &str) -> String {
        format!("ATO_SECRET_{}", name)
    }
}

impl SecretBackend for EnvBackend {
    fn get(&self, key: &SecretKey) -> Result<Option<String>> {
        Ok(std::env::var(Self::env_key_for(&key.name)).ok())
    }

    fn set(
        &self,
        _key: &SecretKey,
        _value: String,
        _desc: Option<&str>,
        _allow: Option<Vec<String>>,
        _deny: Option<Vec<String>>,
    ) -> Result<()> {
        anyhow::bail!("EnvBackend is read-only");
    }

    fn delete(&self, _key: &SecretKey) -> Result<()> {
        anyhow::bail!("EnvBackend is read-only");
    }

    fn list(&self, namespace: &str) -> Result<Vec<BackendEntry>> {
        const PREFIX: &str = "ATO_SECRET_";
        let now = chrono::Utc::now().to_rfc3339();
        let entries = std::env::vars()
            .filter_map(|(k, v)| {
                k.strip_prefix(PREFIX)
                    .map(|name| BackendEntry {
                        key: name.to_string(),
                        namespace: namespace.to_string(),
                        created_at: now.clone(),
                        updated_at: now.clone(),
                        description: Some("from env".into()),
                        allow: None,
                        deny: None,
                    })
                    .filter(|_| !v.is_empty())
            })
            .collect();
        Ok(entries)
    }

    fn update_acl(
        &self,
        _key: &SecretKey,
        _allow: Option<Vec<String>>,
        _deny: Option<Vec<String>>,
    ) -> Result<()> {
        anyhow::bail!("EnvBackend is read-only");
    }
}
