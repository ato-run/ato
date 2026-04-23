use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::backend::{
    AgeFileBackend, BackendEntry, EnvBackend, MemoryBackend, SecretBackend,
    SecretKey, load_identity_bytes,
};
use super::policy::SecretPolicy;

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

/// The main handle for reading/writing secrets.
///
/// Priority chain (highest to lowest):
///   1. EnvBackend       – ATO_SECRET_* environment variables (CI/override)
///   2. MemoryBackend    – in-process session cache
///   3. AgeFileBackend   – ~/.ato/secrets/<ns>.age  (primary persistent)
pub(crate) struct SecretStore {
    home: PathBuf,
    env: EnvBackend,
    memory: MemoryBackend,
    age: Option<AgeFileBackend>,
    /// Configured backend resolution order (from `~/.ato/config.toml [secrets] backends`).
    /// Default order: env → memory → age.
    backend_order: Vec<String>,
}

impl SecretStore {
    /// Open the secret store.
    ///
    /// Loads the age identity non-interactively (session key file or plain-text key).
    /// When the identity is passphrase-protected and no session key is active, the age
    /// backend is disabled; callers should prompt via `ato session start` or
    /// `ato secrets init`.
    pub(crate) fn open() -> Result<Self> {
        let home = dirs::home_dir().context("failed to resolve home directory")?;
        let memory = MemoryBackend::new(None);
        let env = EnvBackend::new();

        let mut age_backend = AgeFileBackend::new(home.clone());
        let loaded = try_load_identity(&mut age_backend);
        let age = if loaded { Some(age_backend) } else { None };

        let backend_order = read_secrets_backend_order(&home).unwrap_or_else(|| {
            vec!["env".into(), "memory".into(), "age".into()]
        });

        Ok(Self { home, env, memory, age, backend_order })
    }

    /// Open with an already-unlocked age backend (used internally by `init` / `session`).
    pub(crate) fn open_with_age(home: PathBuf, age_backend: AgeFileBackend) -> Result<Self> {
        let backend_order = read_secrets_backend_order(&home).unwrap_or_else(|| {
            vec!["env".into(), "memory".into(), "age".into()]
        });
        Ok(Self {
            home,
            env: EnvBackend::new(),
            memory: MemoryBackend::new(None),
            age: Some(age_backend),
            backend_order,
        })
    }

    /// Reference to the age backend, if loaded.
    pub(crate) fn age(&self) -> Option<&AgeFileBackend> {
        self.age.as_ref()
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
        crate::common::env_security::check_user_env_safety(key, value)
            .context("refused to store unsafe secret")?;

        let sk = SecretKey::new(key);
        match &self.age {
            Some(age) => age.set(&sk, value.to_string(), description, allow, deny),
            None => bail!(
                "no age identity loaded — run `ato secrets init` to create one,\n\
                 or `ato session start` to unlock an existing passphrase-protected identity"
            ),
        }
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

        let sk = SecretKey::with_namespace(namespace, key);
        match &self.age {
            Some(age) => age.set(&sk, value.to_string(), description, allow, deny),
            None => bail!(
                "no age identity loaded — run `ato secrets init` to create one,\n\
                 or `ato session start` to unlock an existing passphrase-protected identity"
            ),
        }
    }

    pub(crate) fn get(&self, key: &str) -> Result<Option<String>> {
        let sk = SecretKey::new(key);
        for backend in &self.backend_order {
            let result = match backend.as_str() {
                "env" => self.env.get(&sk)?,
                "memory" => self.memory.get(&sk)?,
                "age" => self.age.as_ref().and_then(|a| a.get(&sk).ok().flatten()),
                "keychain" => None, // keychain no longer supported
                _ => None,
            };
            if let Some(v) = result { return Ok(Some(v)); }
        }
        Ok(None)
    }

    pub(crate) fn get_in_namespace(&self, key: &str, namespace: &str) -> Result<Option<String>> {
        let sk = SecretKey::with_namespace(namespace, key);
        for backend in &self.backend_order {
            let result = match backend.as_str() {
                "env" => self.env.get(&sk)?,
                "memory" => self.memory.get(&sk)?,
                "age" => self.age.as_ref().and_then(|a| a.get(&sk).ok().flatten()),
                _ => None, // legacy doesn't support namespaces
            };
            if let Some(v) = result { return Ok(Some(v)); }
        }
        Ok(None)
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
        let sk = SecretKey::new(key);
        if let Some(age) = &self.age {
            // Remove from all namespaces.
            for ns in age.list_namespaces() {
                let nsk = SecretKey::with_namespace(&ns, key);
                age.delete(&nsk).ok();
            }
            // Also check default namespace explicitly.
            age.delete(&sk).ok();
            return Ok(());
        }
        bail!(
            "no age identity loaded — run `ato secrets init` to create one,\n\
             or `ato session start` to unlock an existing passphrase-protected identity"
        )
    }

    pub(crate) fn list(&self) -> Result<Vec<SecretEntry>> {
        let mut entries: Vec<SecretEntry> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        if let Some(age) = &self.age {
            for ns in age.list_namespaces() {
                for be in age.list(&ns).unwrap_or_default() {
                    if seen.insert(be.key.clone()) {
                        entries.push(backend_entry_to_secret_entry(be));
                    }
                }
            }
            if age.list_namespaces().is_empty() {
                for be in age.list("default").unwrap_or_default() {
                    if seen.insert(be.key.clone()) {
                        entries.push(backend_entry_to_secret_entry(be));
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
        let sk = SecretKey::new(key);
        match &self.age {
            Some(age) => age.update_acl(&sk, allow, deny),
            None => bail!(
                "no age identity loaded — run `ato secrets init` to create one,\n\
                 or `ato session start` to unlock an existing passphrase-protected identity"
            ),
        }
    }

}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn try_load_identity(age: &mut AgeFileBackend) -> bool {
    // Check for a session key file exported by `ato session start`.
    if let Ok(session_path) = std::env::var("ATO_SESSION_KEY_FILE") {
        let p = std::path::Path::new(&session_path);
        if p.exists() {
            if let Ok(raw) = std::fs::read(p) {
                if let Ok(id) = load_identity_bytes(&raw, None) {
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

/// Read `~/.ato/config.toml` and return the `[secrets] backends` list if present.
///
/// Allowed backend names: `"env"`, `"memory"`, `"age"`.
/// If the key is absent or the file doesn't exist, returns `None` (use default order).
fn read_secrets_backend_order(home: &Path) -> Option<Vec<String>> {
    let config_path = home.join(".ato").join("config.toml");
    let raw = std::fs::read_to_string(config_path).ok()?;
    let doc: toml::Value = raw.parse().ok()?;
    let backends = doc
        .get("secrets")?
        .get("backends")?
        .as_array()?
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_ascii_lowercase()))
        .collect::<Vec<_>>();
    if backends.is_empty() { None } else { Some(backends) }
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
    if ns == "default" {
        SecretScope::Global
    } else if let Some(name) = ns.strip_prefix("capsule:") {
        SecretScope::Capsule(name.to_string())
    } else {
        SecretScope::Capsule(ns.to_string())
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

/// Write a file with 0600 permissions, using an atomic temp-rename.
pub(crate) fn write_secure_file(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("secrets path must have a parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create directory {}", parent.display()))?;

    let tmp_path = path.with_extension("tmp");

    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&tmp_path)
            .with_context(|| format!("failed to open {}", tmp_path.display()))?;
        file.write_all(contents)
            .with_context(|| format!("failed to write {}", tmp_path.display()))?;
        file.flush()
            .with_context(|| format!("failed to flush {}", tmp_path.display()))?;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to chmod {}", tmp_path.display()))?;
    }

    #[cfg(not(unix))]
    {
        let mut file = std::fs::File::create(&tmp_path)
            .with_context(|| format!("failed to open {}", tmp_path.display()))?;
        file.write_all(contents)
            .with_context(|| format!("failed to write {}", tmp_path.display()))?;
        file.flush()
            .with_context(|| format!("failed to flush {}", tmp_path.display()))?;
    }

    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "failed to rename {} → {}",
            tmp_path.display(),
            path.display()
        )
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use crate::application::secrets::backend::AgeFileBackend;

    fn init_store(dir: &TempDir) -> SecretStore {
        let home = dir.path().to_path_buf();
        let mut age = AgeFileBackend::new(home.clone());
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

        // Inject env override.
        std::env::set_var("ATO_SECRET_MY_KEY", "env-value");
        let result = store.get("MY_KEY").unwrap();
        std::env::remove_var("ATO_SECRET_MY_KEY");

        assert_eq!(result, Some("env-value".to_string()));
    }

    // ── namespace ─────────────────────────────────────────────────────────────

    #[test]
    fn set_in_namespace_isolated_from_default() {
        let dir = TempDir::new().unwrap();
        let store = init_store(&dir);
        store.set("KEY", "default-val", None, None, None).unwrap();
        store.set_in_namespace("KEY", "project", "project-val", None, None, None).unwrap();

        assert_eq!(store.get("KEY").unwrap(), Some("default-val".to_string()));
        assert_eq!(store.get_in_namespace("KEY", "project").unwrap(), Some("project-val".to_string()));
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
            .set("SECRET", "val", None, Some(vec!["capsule:allowed".into()]), None)
            .unwrap();
        let result = store.load("SECRET", Some("capsule:allowed")).unwrap();
        assert_eq!(result, Some("val".to_string()));
    }

    #[test]
    fn load_denies_capsule_not_on_allowlist() {
        let dir = TempDir::new().unwrap();
        let store = init_store(&dir);
        store
            .set("SECRET", "val", None, Some(vec!["capsule:allowed".into()]), None)
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
