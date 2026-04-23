use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::backend::{
    AgeFileBackend, BackendEntry, EnvBackend, KeychainBackend, MemoryBackend, SecretBackend,
    SecretKey,
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
///   4. Legacy keychain  – OS keychain + chmod-600 fallback file (migration path)
pub(crate) struct SecretStore {
    home: PathBuf,
    env: EnvBackend,
    memory: MemoryBackend,
    age: Option<AgeFileBackend>,
    keychain: KeychainBackend,
    /// Legacy: use OS keychain as fallback when age identity is absent.
    legacy_use_keyring: bool,
}

impl SecretStore {
    /// Open the secret store.
    ///
    /// Loads the age identity non-interactively (tries keychain for passphrase
    /// if the identity is passphrase-protected).  Falls back to the legacy
    /// keychain backend when no age identity is found.
    pub(crate) fn open() -> Result<Self> {
        let home = dirs::home_dir().context("failed to resolve home directory")?;
        let keychain = KeychainBackend::new();
        let memory = MemoryBackend::new(None);
        let env = EnvBackend::new();

        let mut age_backend = AgeFileBackend::new(home.clone());
        let loaded = try_load_identity(&mut age_backend, &keychain);
        let age = if loaded { Some(age_backend) } else { None };

        let legacy_use_keyring = super::storage::is_keyring_available();

        Ok(Self {
            home,
            env,
            memory,
            age,
            keychain,
            legacy_use_keyring,
        })
    }

    /// Open with an already-unlocked age backend (used internally by `init` / `session`).
    pub(crate) fn open_with_age(home: PathBuf, age_backend: AgeFileBackend) -> Result<Self> {
        let keychain = KeychainBackend::new();
        let legacy_use_keyring = super::storage::is_keyring_available();
        Ok(Self {
            home,
            env: EnvBackend::new(),
            memory: MemoryBackend::new(None),
            age: Some(age_backend),
            keychain,
            legacy_use_keyring,
        })
    }

    /// Reference to the age backend, if loaded.
    pub(crate) fn age(&self) -> Option<&AgeFileBackend> {
        self.age.as_ref()
    }

    /// Reference to the keychain backend.
    pub(crate) fn keychain(&self) -> &KeychainBackend {
        &self.keychain
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

        if let Some(age) = &self.age {
            return age.set(&sk, value.to_string(), description, allow, deny);
        }

        // Legacy path.
        self.legacy_write(key, value)?;
        self.legacy_update_metadata(key, description, allow, deny, false)
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

        if let Some(age) = &self.age {
            return age.set(&sk, value.to_string(), description, allow, deny);
        }

        // Fall back to default namespace on legacy path.
        self.legacy_write(key, value)?;
        self.legacy_update_metadata(key, description, allow, deny, false)
    }

    pub(crate) fn get(&self, key: &str) -> Result<Option<String>> {
        let sk = SecretKey::new(key);
        if let Some(v) = self.env.get(&sk)? { return Ok(Some(v)); }
        if let Some(v) = self.memory.get(&sk)? { return Ok(Some(v)); }
        if let Some(age) = &self.age {
            if let Some(v) = age.get(&sk)? { return Ok(Some(v)); }
        }
        self.legacy_read(key)
    }

    pub(crate) fn get_in_namespace(&self, key: &str, namespace: &str) -> Result<Option<String>> {
        let sk = SecretKey::with_namespace(namespace, key);
        if let Some(v) = self.env.get(&sk)? { return Ok(Some(v)); }
        if let Some(v) = self.memory.get(&sk)? { return Ok(Some(v)); }
        if let Some(age) = &self.age {
            if let Some(v) = age.get(&sk)? { return Ok(Some(v)); }
        }
        // Legacy doesn't support namespaces, skip.
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
        }
        self.legacy_delete(key)
    }

    pub(crate) fn list(&self) -> Result<Vec<SecretEntry>> {
        let mut entries: Vec<SecretEntry> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        // Age backend: all namespaces.
        if let Some(age) = &self.age {
            for ns in age.list_namespaces() {
                let backend_entries = age.list(&ns).unwrap_or_default();
                for be in backend_entries {
                    if seen.insert(be.key.clone()) {
                        entries.push(backend_entry_to_secret_entry(be));
                    }
                }
            }
            // Also list default if no namespaces found on disk (may be new store).
            if age.list_namespaces().is_empty() {
                for be in age.list("default").unwrap_or_default() {
                    if seen.insert(be.key.clone()) {
                        entries.push(backend_entry_to_secret_entry(be));
                    }
                }
            }
        }

        // Legacy: only add entries not already seen.
        for entry in self.legacy_list()? {
            if seen.insert(entry.key.clone()) {
                entries.push(entry);
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
        if let Some(age) = &self.age {
            return age.update_acl(&sk, allow, deny);
        }
        self.legacy_update_metadata(key, None, allow, deny, true)
    }

    // ── Legacy helpers (keychain + env file) ─────────────────────────────────

    fn legacy_write(&self, key: &str, value: &str) -> Result<()> {
        const GLOBAL_SERVICE: &str = "ato.secrets.global";
        if self.legacy_use_keyring {
            let entry = keyring::Entry::new(GLOBAL_SERVICE, key)?;
            entry
                .set_password(value)
                .with_context(|| format!("keychain write failed for '{}'", key))?;
        } else {
            let path = self.legacy_fallback_path(GLOBAL_SERVICE);
            let mut pairs = read_env_file(&path).unwrap_or_default();
            if let Some(pos) = pairs.iter().position(|(k, _)| k == key) {
                pairs[pos].1 = value.to_string();
            } else {
                pairs.push((key.to_string(), value.to_string()));
            }
            write_env_file(&path, &pairs)?;
        }
        Ok(())
    }

    fn legacy_read(&self, key: &str) -> Result<Option<String>> {
        const GLOBAL_SERVICE: &str = "ato.secrets.global";
        if self.legacy_use_keyring {
            let entry = keyring::Entry::new(GLOBAL_SERVICE, key)?;
            match entry.get_password() {
                Ok(v) => return Ok(Some(v)),
                Err(keyring::Error::NoEntry) => {}
                Err(e) if is_nonfatal_keyring_error(&e) => {}
                Err(e) => bail!("keychain read failed for '{}': {}", key, e),
            }
        }
        let path = self.legacy_fallback_path(GLOBAL_SERVICE);
        if path.exists() {
            let pairs = read_env_file(&path).unwrap_or_default();
            if let Some((_, v)) = pairs.into_iter().find(|(k, _)| k == key) {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    fn legacy_delete(&self, key: &str) -> Result<()> {
        const GLOBAL_SERVICE: &str = "ato.secrets.global";
        if self.legacy_use_keyring {
            let entry = keyring::Entry::new(GLOBAL_SERVICE, key)?;
            match entry.delete_password() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(e) => bail!("keychain delete failed: {}", e),
            }
        }
        let fallback = self.legacy_fallback_path(GLOBAL_SERVICE);
        if fallback.exists() {
            let mut pairs = read_env_file(&fallback).unwrap_or_default();
            pairs.retain(|(k, _)| k != key);
            write_env_file(&fallback, &pairs)?;
        }
        // Remove from metadata too.
        let mut meta = self.legacy_load_metadata()?;
        meta.secrets.remove(key);
        self.legacy_save_metadata(&meta)
    }

    fn legacy_list(&self) -> Result<Vec<SecretEntry>> {
        let meta = self.legacy_load_metadata()?;
        let mut entries: Vec<_> = meta.secrets.into_values().collect();
        entries.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(entries)
    }

    fn legacy_update_metadata(
        &self,
        key: &str,
        description: Option<&str>,
        allow: Option<Vec<String>>,
        deny: Option<Vec<String>>,
        require_existing: bool,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        let mut meta = self.legacy_load_metadata()?;
        if require_existing {
            let entry = meta
                .secrets
                .get_mut(key)
                .with_context(|| format!("secret '{}' not found", key))?;
            if let Some(a) = allow { entry.allow = Some(a); }
            if let Some(d) = deny  { entry.deny  = Some(d); }
            entry.updated_at = now;
        } else {
            let entry = meta.secrets.entry(key.to_string()).or_insert_with(|| SecretEntry {
                key: key.to_string(),
                scope: SecretScope::Global,
                description: None,
                created_at: now.clone(),
                updated_at: now.clone(),
                allow: None,
                deny: None,
            });
            entry.updated_at = now;
            if let Some(d) = description { entry.description = Some(d.to_string()); }
            if allow.is_some() { entry.allow = allow; }
            if deny.is_some()  { entry.deny  = deny; }
        }
        self.legacy_save_metadata(&meta)
    }

    fn legacy_fallback_path(&self, service: &str) -> PathBuf {
        let safe = service
            .chars()
            .map(|c| if matches!(c, '.' | '/') { '_' } else { c })
            .collect::<String>();
        self.home.join(".ato/secrets").join(format!("{}.env", safe))
    }

    fn legacy_metadata_path(&self) -> PathBuf {
        self.home.join(".ato/secrets/metadata.json")
    }

    fn legacy_load_metadata(&self) -> Result<LegacyMetadata> {
        let path = self.legacy_metadata_path();
        if !path.exists() {
            return Ok(LegacyMetadata::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read secrets metadata from {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse secrets metadata from {}", path.display()))
    }

    fn legacy_save_metadata(&self, meta: &LegacyMetadata) -> Result<()> {
        let path = self.legacy_metadata_path();
        let rendered = serde_json::to_string_pretty(meta)?;
        write_secure_file(&path, rendered.as_bytes())
    }

    /// Delete a legacy keychain entry (for migration).
    pub(crate) fn legacy_delete_key(&self, key: &str) -> Result<()> {
        self.legacy_delete(key)
    }

    /// List legacy keychain entries (for migrate-from-keychain).
    pub(crate) fn legacy_list_all_keys(&self) -> Vec<String> {
        self.legacy_load_metadata()
            .map(|m| m.secrets.into_keys().collect())
            .unwrap_or_default()
    }

    /// Read a legacy keychain entry directly (for migration).
    pub(crate) fn legacy_get(&self, key: &str) -> Result<Option<String>> {
        self.legacy_read(key)
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

#[derive(Debug, Default, Serialize, Deserialize)]
struct LegacyMetadata {
    #[serde(default)]
    secrets: std::collections::HashMap<String, SecretEntry>,
}

fn try_load_identity(age: &mut AgeFileBackend, keychain: &KeychainBackend) -> bool {
    if !age.identity_exists() {
        return false;
    }
    // Try plain text first (no passphrase).
    if age.load_identity_with_passphrase(None).is_ok() {
        return true;
    }
    // Try cached passphrase from keychain.
    if let Some(pp) = keychain.get_passphrase() {
        if age.load_identity_with_passphrase(Some(&pp)).is_ok() {
            return true;
        }
    }
    // Can't load non-interactively – age backend disabled.
    false
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

fn is_nonfatal_keyring_error(e: &keyring::Error) -> bool {
    matches!(
        e,
        keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_)
    )
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

fn write_env_file(path: &Path, pairs: &[(String, String)]) -> Result<()> {
    let rendered = pairs
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("\n");
    write_secure_file(path, format!("{}\n", rendered).as_bytes())
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
