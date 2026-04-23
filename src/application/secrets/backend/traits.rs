use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Identifies a secret by namespace + key name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SecretKey {
    pub(crate) namespace: String,
    pub(crate) name: String,
}

impl SecretKey {
    pub(crate) fn new(name: impl Into<String>) -> Self {
        Self {
            namespace: "default".into(),
            name: name.into(),
        }
    }

    pub(crate) fn with_namespace(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            name: name.into(),
        }
    }
}

/// An entry returned by `SecretBackend::list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct BackendEntry {
    pub(crate) key: String,
    pub(crate) namespace: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) allow: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) deny: Option<Vec<String>>,
}

/// Core backend trait.
pub(crate) trait SecretBackend: Send + Sync {
    fn get(&self, key: &SecretKey) -> Result<Option<String>>;

    fn set(
        &self,
        key: &SecretKey,
        value: String,
        description: Option<&str>,
        allow: Option<Vec<String>>,
        deny: Option<Vec<String>>,
    ) -> Result<()>;

    fn delete(&self, key: &SecretKey) -> Result<()>;

    fn list(&self, namespace: &str) -> Result<Vec<BackendEntry>>;

    fn update_acl(
        &self,
        key: &SecretKey,
        allow: Option<Vec<String>>,
        deny: Option<Vec<String>>,
    ) -> Result<()>;
}
