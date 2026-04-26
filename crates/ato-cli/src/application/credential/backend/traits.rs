use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Identifies a credential by hierarchical namespace + key name.
///
/// `namespace` uses `/` as hierarchy separator. Top-level segment denotes
/// the storage domain (e.g. `"secrets"`, `"auth"`); deeper segments are the
/// domain-specific sub-namespace.
///
/// Examples:
///   - `CredentialKey { namespace: "secrets/default",  name: "API_KEY" }`
///   - `CredentialKey { namespace: "secrets/capsule:myapp", name: "DB_PASS" }`
///   - `CredentialKey { namespace: "auth/session",   name: "SESSION_TOKEN" }`
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CredentialKey {
    pub(crate) namespace: String,
    pub(crate) name: String,
}

impl CredentialKey {
    pub(crate) fn new(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            name: name.into(),
        }
    }
}

/// An entry returned by `CredentialBackend::list`.
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

/// Core backend trait shared by `secrets` and `auth` domains.
pub(crate) trait CredentialBackend: Send + Sync {
    /// Short identifier used in config files / diagnostics (e.g. `"env"`, `"age"`).
    #[allow(dead_code)] // Used by Phase 2 diagnostics (`ato cred doctor`).
    fn name(&self) -> &'static str;

    /// Whether this backend can currently satisfy reads (e.g. identity loaded).
    fn is_available(&self) -> bool {
        true
    }

    /// Whether this backend supports `set`.
    fn is_writable(&self) -> bool {
        true
    }

    fn get(&self, key: &CredentialKey) -> Result<Option<String>>;

    fn set(
        &self,
        key: &CredentialKey,
        value: String,
        description: Option<&str>,
        allow: Option<Vec<String>>,
        deny: Option<Vec<String>>,
    ) -> Result<()>;

    fn delete(&self, key: &CredentialKey) -> Result<()>;

    fn list(&self, namespace: &str) -> Result<Vec<BackendEntry>>;

    fn update_acl(
        &self,
        key: &CredentialKey,
        allow: Option<Vec<String>>,
        deny: Option<Vec<String>>,
    ) -> Result<()>;
}
