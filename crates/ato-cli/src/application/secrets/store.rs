use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use capsule_core::common::paths::nacelle_home_dir;
use serde::{Deserialize, Serialize};

use super::policy::SecretPolicy;
use crate::application::credential::{
    self, backend::CredentialBackend, AgeFileBackend, BackendChain, BackendEntry, CredentialKey,
    EnvBackend, MemoryBackend,
};

// ── Public types (kept for backward compat) ──────────────────────────────────

/// A stored secret entry with associated metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SecretEntry {
    pub(crate) key: String,
    pub(crate) scope: SecretScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) allow: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) deny: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SecretScope {
    Global,
    Capsule(String),
}

// ── SecretStore ───────────────────────────────────────────────────────────────

/// Domain-layer handle for secrets (namespaces under `secrets/*`).
///
/// Wraps a shared `BackendChain` with the `secrets`-prefixed namespace map.
/// Reads (`get`) go through the chain (default priority: env → memory → age).
/// Writes (`set`/`delete`/`update_acl`) go directly to the persistent age
/// backend.
///
/// External env overrides use `ATO_CRED_SECRETS_<SUB>__<KEY>`. The legacy
/// `ATO_SECRET_*` form was removed in v0.5.
pub(crate) struct SecretStore {
    chain: BackendChain,
    age: Option<Arc<AgeFileBackend>>,
}

impl SecretStore {
    /// Open the secret store.
    ///
    /// Loads the age identity non-interactively (session key file or plain-text key).
    /// When the identity is passphrase-protected and no session key is active, the age
    /// backend is disabled; callers should prompt via `ato session start` or
    /// `ato secrets init`.
    pub(crate) fn open() -> Result<Self> {
        let ato_home = nacelle_home_dir().context("failed to resolve ato home")?;

        let mut age_backend = AgeFileBackend::new(ato_home.clone());
        let loaded = try_load_identity(&mut age_backend);
        let age = if loaded {
            Some(Arc::new(age_backend))
        } else {
            None
        };

        let order = credential::config::read_order(&ato_home)
            .unwrap_or_else(credential::config::default_order);

        Ok(Self {
            chain: build_chain(&order, age.clone()),
            age,
        })
    }

    /// Open with an already-unlocked age backend (used internally by `init` / `session`).
    #[cfg(test)]
    pub(crate) fn open_with_age(home: PathBuf, age_backend: AgeFileBackend) -> Result<Self> {
        let age = Some(Arc::new(age_backend));
        let order =
            credential::config::read_order(&home).unwrap_or_else(credential::config::default_order);
        Ok(Self {
            chain: build_chain(&order, age.clone()),
            age,
        })
    }

    /// Reference to the age backend, if loaded.
    pub(crate) fn age(&self) -> Option<&AgeFileBackend> {
        self.age.as_deref()
    }

    // ── Public API ────────────────────────────────────────────────────────────

    pub(crate) fn set(
        &self,
        key: &str,
        value: &str,
        description: Option<&str>,
        allow: Option<Vec<String>>,
        deny: Option<Vec<String>>,
    ) -> Result<()> {
        self.set_in_namespace(key, "default", value, description, allow, deny)
    }

    pub(crate) fn set_in_namespace(
        &self,
        key: &str,
        namespace: &str,
        value: &str,
        description: Option<&str>,
        allow: Option<Vec<String>>,
        deny: Option<Vec<String>>,
    ) -> Result<()> {
        crate::common::env_security::check_user_env_safety(key, value)
            .context("refused to store unsafe secret")?;

        let ck = CredentialKey::new(secrets_ns(namespace), key);
        match &self.age {
            Some(age) => age.set(&ck, value.to_string(), description, allow, deny),
            None => bail!(
                "no age identity loaded — run `ato secrets init` to create one,\n\
                 or `ato session start` to unlock an existing passphrase-protected identity"
            ),
        }
    }

    pub(crate) fn get(&self, key: &str) -> Result<Option<String>> {
        let ck = CredentialKey::new(secrets_ns("default"), key);
        self.chain.get(&ck)
    }

    pub(crate) fn get_in_namespace(&self, key: &str, namespace: &str) -> Result<Option<String>> {
        let ck = CredentialKey::new(secrets_ns(namespace), key);
        self.chain.get(&ck)
    }

    /// Load a secret with ACL check for a specific capsule_id.
    pub(crate) fn load(&self, key: &str, capsule_id: Option<&str>) -> Result<Option<String>> {
        if let Some(cid) = capsule_id {
            let entries = self.list()?;
            if let Some(entry) = entries.iter().find(|e| e.key == key) {
                let policy = build_policy(entry)?;
                if policy.check(cid) == super::policy::PolicyResult::Deny {
                    return Ok(None);
                }
            }
        }
        self.get(key)
    }

    pub(crate) fn delete(&self, key: &str) -> Result<()> {
        let age = self.age.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "no age identity loaded — run `ato secrets init` to create one,\n\
                 or `ato session start` to unlock an existing passphrase-protected identity"
            )
        })?;
        // Remove from every secrets sub-namespace.
        for sub in age.list_sub_namespaces("secrets") {
            let ck = CredentialKey::new(format!("secrets/{}", sub), key);
            age.delete(&ck).ok();
        }
        // Also remove from the default namespace explicitly (covers
        // fresh-install case where no sub-ns file exists yet).
        let default_key = CredentialKey::new(secrets_ns("default"), key);
        age.delete(&default_key).ok();
        Ok(())
    }

    pub(crate) fn list(&self) -> Result<Vec<SecretEntry>> {
        let mut entries: Vec<SecretEntry> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        if let Some(age) = &self.age {
            let subs = age.list_sub_namespaces("secrets");
            if subs.is_empty() {
                for be in age.list(&secrets_ns("default")).unwrap_or_default() {
                    if seen.insert(be.key.clone()) {
                        entries.push(backend_entry_to_secret_entry(be));
                    }
                }
            } else {
                for sub in subs {
                    let ns = format!("secrets/{}", sub);
                    for be in age.list(&ns).unwrap_or_default() {
                        if seen.insert(be.key.clone()) {
                            entries.push(backend_entry_to_secret_entry(be));
                        }
                    }
                }
            }
        }

        entries.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(entries)
    }

    pub(crate) fn import_env_file(&self, path: &Path) -> Result<usize> {
        let pairs = read_env_file(path)?;
        let count = pairs.len();
        for (k, v) in pairs {
            self.set(&k, &v, None, None, None)?;
        }
        Ok(count)
    }

    pub(crate) fn update_acl(
        &self,
        key: &str,
        allow: Option<Vec<String>>,
        deny: Option<Vec<String>>,
    ) -> Result<()> {
        let ck = CredentialKey::new(secrets_ns("default"), key);
        match &self.age {
            Some(age) => age.update_acl(&ck, allow, deny),
            None => bail!(
                "no age identity loaded — run `ato secrets init` to create one,\n\
                 or `ato session start` to unlock an existing passphrase-protected identity"
            ),
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Prepend the `secrets/` domain prefix to a user-facing namespace name.
///
/// `"default"` → `"secrets/default"`, `"capsule:foo"` → `"secrets/capsule:foo"`.
pub(crate) fn secrets_ns(user_facing: &str) -> String {
    if user_facing.starts_with("secrets/") {
        user_facing.to_string()
    } else {
        format!("secrets/{}", user_facing)
    }
}

fn build_chain(order: &[String], age: Option<Arc<AgeFileBackend>>) -> BackendChain {
    let mut backends: Vec<Arc<dyn CredentialBackend>> = Vec::new();
    for name in order {
        match name.as_str() {
            "env" => backends.push(Arc::new(EnvBackend::new())),
            "memory" => backends.push(Arc::new(MemoryBackend::new(None))),
            "age" => {
                if let Some(a) = &age {
                    backends.push(a.clone() as Arc<dyn CredentialBackend>);
                }
            }
            // Silently ignore unknown / deprecated names (e.g. legacy "keychain").
            _ => {}
        }
    }
    BackendChain::new(backends)
}

fn try_load_identity(age: &mut AgeFileBackend) -> bool {
    // Check for a session key file exported by `ato session start`.
    if let Ok(session_path) = std::env::var("ATO_SESSION_KEY_FILE") {
        let p = std::path::Path::new(&session_path);
        if p.exists() {
            if let Ok(raw) = std::fs::read(p) {
                if let Ok(id) = credential::load_identity_bytes(&raw, None) {
                    age.install_identity(id);
                    return true;
                }
            }
        }
    }

    if !age.identity_exists() {
        return false;
    }
    // Try plain text (no passphrase).
    age.load_identity_with_passphrase(None).is_ok()
    // Passphrase-protected identities require `ato session start` or interactive init.
}

fn build_policy(entry: &SecretEntry) -> Result<SecretPolicy> {
    if let Some(allow) = &entry.allow {
        return SecretPolicy::allow_list(allow);
    }
    if let Some(deny) = &entry.deny {
        return SecretPolicy::deny_list(deny);
    }
    Ok(SecretPolicy::default())
}

fn backend_entry_to_secret_entry(be: BackendEntry) -> SecretEntry {
    let scope = namespace_to_scope(&be.namespace);
    SecretEntry {
        key: be.key,
        scope,
        description: be.description,
        created_at: be.created_at,
        updated_at: be.updated_at,
        allow: be.allow,
        deny: be.deny,
    }
}

fn namespace_to_scope(ns: &str) -> SecretScope {
    // Strip the `secrets/` domain prefix before mapping to user-facing scope.
    let sub = ns.strip_prefix("secrets/").unwrap_or(ns);
    if sub == "default" {
        SecretScope::Global
    } else if let Some(name) = sub.strip_prefix("capsule:") {
        SecretScope::Capsule(name.to_string())
    } else {
        SecretScope::Capsule(sub.to_string())
    }
}

fn read_env_file(path: &Path) -> Result<Vec<(String, String)>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let mut pairs = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            pairs.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    Ok(pairs)
}

/// Write a file with 0600 permissions. Thin forwarder kept for callers that
/// imported from `secrets::store::write_secure_file`.
pub(crate) fn write_secure_file(path: &Path, contents: &[u8]) -> Result<()> {
    credential::write_secure_file(path, contents)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn init_store(dir: &TempDir) -> SecretStore {
        let home = dir.path().to_path_buf();
        let age = AgeFileBackend::new(home.clone());
        age.init_identity(None).expect("init_identity");
        SecretStore::open_with_age(home, age).expect("open_with_age")
    }

    // ── set / get ─────────────────────────────────────────────────────────────

    #[test]
    fn set_and_get_roundtrip() {
        let dir = TempDir::new().unwrap();
        let store = init_store(&dir);
        store.set("TOKEN", "abc123", None, None, None).unwrap();
        assert_eq!(store.get("TOKEN").unwrap(), Some("abc123".to_string()));
    }

    #[test]
    fn get_missing_key_returns_none() {
        let dir = TempDir::new().unwrap();
        let store = init_store(&dir);
        assert_eq!(store.get("MISSING").unwrap(), None);
    }

    // ── env override has highest priority ─────────────────────────────────────

    #[test]
    fn env_backend_overrides_age_store() {
        let dir = TempDir::new().unwrap();
        let store = init_store(&dir);

        // Store value in age backend.
        store.set("MY_KEY", "age-value", None, None, None).unwrap();

        // New env var form: ATO_CRED_SECRETS_DEFAULT__<KEY>
        std::env::set_var("ATO_CRED_SECRETS_DEFAULT__MY_KEY", "env-value");
        let result = store.get("MY_KEY").unwrap();
        std::env::remove_var("ATO_CRED_SECRETS_DEFAULT__MY_KEY");

        assert_eq!(result, Some("env-value".to_string()));
    }

    #[test]
    fn legacy_ato_secret_env_is_ignored() {
        // Breaking change: ATO_SECRET_* must no longer override.
        let dir = TempDir::new().unwrap();
        let store = init_store(&dir);
        store
            .set("BREAKING_KEY", "age-value", None, None, None)
            .unwrap();

        std::env::set_var("ATO_SECRET_BREAKING_KEY", "legacy-value");
        let got = store.get("BREAKING_KEY").unwrap();
        std::env::remove_var("ATO_SECRET_BREAKING_KEY");

        assert_eq!(got, Some("age-value".to_string()));
    }

    // ── namespace ─────────────────────────────────────────────────────────────

    #[test]
    fn set_in_namespace_isolated_from_default() {
        let dir = TempDir::new().unwrap();
        let store = init_store(&dir);
        store.set("KEY", "default-val", None, None, None).unwrap();
        store
            .set_in_namespace("KEY", "project", "project-val", None, None, None)
            .unwrap();

        assert_eq!(store.get("KEY").unwrap(), Some("default-val".to_string()));
        assert_eq!(
            store.get_in_namespace("KEY", "project").unwrap(),
            Some("project-val".to_string())
        );
    }

    // ── delete ────────────────────────────────────────────────────────────────

    #[test]
    fn delete_removes_from_default_namespace() {
        let dir = TempDir::new().unwrap();
        let store = init_store(&dir);
        store.set("GONE", "value", None, None, None).unwrap();
        store.delete("GONE").unwrap();
        assert_eq!(store.get("GONE").unwrap(), None);
    }

    // ── list ──────────────────────────────────────────────────────────────────

    #[test]
    fn list_returns_stored_entries() {
        let dir = TempDir::new().unwrap();
        let store = init_store(&dir);
        store.set("A", "1", None, None, None).unwrap();
        store.set("B", "2", None, None, None).unwrap();
        let entries = store.list().unwrap();
        let keys: Vec<_> = entries.iter().map(|e| e.key.as_str()).collect();
        assert!(keys.contains(&"A"), "expected A in {:?}", keys);
        assert!(keys.contains(&"B"), "expected B in {:?}", keys);
    }

    // ── ACL check via load() ──────────────────────────────────────────────────

    #[test]
    fn load_allows_capsule_on_allowlist() {
        let dir = TempDir::new().unwrap();
        let store = init_store(&dir);
        store
            .set(
                "SECRET",
                "val",
                None,
                Some(vec!["capsule:allowed".into()]),
                None,
            )
            .unwrap();
        let result = store.load("SECRET", Some("capsule:allowed")).unwrap();
        assert_eq!(result, Some("val".to_string()));
    }

    #[test]
    fn load_denies_capsule_not_on_allowlist() {
        let dir = TempDir::new().unwrap();
        let store = init_store(&dir);
        store
            .set(
                "SECRET",
                "val",
                None,
                Some(vec!["capsule:allowed".into()]),
                None,
            )
            .unwrap();
        let result = store.load("SECRET", Some("capsule:other")).unwrap();
        assert_eq!(result, None);
    }

    // ── import_env_file ───────────────────────────────────────────────────────

    #[test]
    fn import_env_file_stores_all_pairs() {
        let dir = TempDir::new().unwrap();
        let store = init_store(&dir);

        let env_path = dir.path().join("test.env");
        std::fs::write(&env_path, "ALPHA=one\nBETA=two\n# comment\n\n").unwrap();
        let count = store.import_env_file(&env_path).unwrap();
        assert_eq!(count, 2);
        assert_eq!(store.get("ALPHA").unwrap(), Some("one".to_string()));
        assert_eq!(store.get("BETA").unwrap(), Some("two".to_string()));
    }
}
