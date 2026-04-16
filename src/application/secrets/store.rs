use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};

use super::policy::SecretPolicy;

const GLOBAL_SERVICE: &str = "ato.secrets.global";
const FALLBACK_DIR: &str = ".ato/secrets";
const METADATA_FILE: &str = ".ato/secrets/metadata.json";

/// A stored secret entry with associated metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SecretEntry {
    pub(crate) key: String,
    pub(crate) scope: SecretScope,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    /// ISO-8601 creation timestamp.
    pub(crate) created_at: String,
    /// ISO-8601 last-updated timestamp.
    pub(crate) updated_at: String,
    /// ACL: which capsule IDs may access this secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) allow: Option<Vec<String>>,
    /// ACL: which capsule IDs are explicitly denied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) deny: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SecretScope {
    Global,
    Capsule(String),
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Metadata {
    #[serde(default)]
    secrets: HashMap<String, SecretEntry>,
}

/// The main handle for reading/writing secrets.
///
/// Tries the OS keychain first; falls back to a chmod-600 file when unavailable.
pub(crate) struct SecretStore {
    home: PathBuf,
    use_keyring: bool,
}

impl SecretStore {
    /// Open the store, detecting whether the OS keychain is available.
    pub(crate) fn open() -> Result<Self> {
        let home = dirs::home_dir().context("failed to resolve home directory")?;
        let use_keyring = super::storage::is_keyring_available();
        Ok(Self { home, use_keyring })
    }

    /// Store a secret value globally.
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
        self.write_value(GLOBAL_SERVICE, key, value)?;

        let now = chrono::Utc::now().to_rfc3339();
        let mut meta = self.load_metadata()?;
        let entry = meta
            .secrets
            .entry(key.to_string())
            .or_insert_with(|| SecretEntry {
                key: key.to_string(),
                scope: SecretScope::Global,
                description: None,
                created_at: now.clone(),
                updated_at: now.clone(),
                allow: None,
                deny: None,
            });
        entry.updated_at = now;
        if let Some(d) = description {
            entry.description = Some(d.to_string());
        }
        if let Some(a) = allow {
            entry.allow = Some(a);
        }
        if let Some(d) = deny {
            entry.deny = Some(d);
        }
        self.save_metadata(&meta)
    }

    /// Retrieve a secret value by key.
    pub(crate) fn get(&self, key: &str) -> Result<Option<String>> {
        self.read_value(GLOBAL_SERVICE, key)
    }

    /// Load a secret with ACL check for a specific capsule_id.
    pub(crate) fn load(&self, key: &str, capsule_id: Option<&str>) -> Result<Option<String>> {
        let meta = self.load_metadata()?;
        if let Some(entry) = meta.secrets.get(key) {
            if let Some(cid) = capsule_id {
                let policy = build_policy(entry)?;
                if policy.check(cid) == super::policy::PolicyResult::Deny {
                    return Ok(None);
                }
            }
        }
        self.read_value(GLOBAL_SERVICE, key)
    }

    /// Delete a secret.
    pub(crate) fn delete(&self, key: &str) -> Result<()> {
        // Remove from keychain / fallback file
        if self.use_keyring {
            let entry = Entry::new(GLOBAL_SERVICE, key)?;
            match entry.delete_password() {
                Ok(()) => {}
                Err(keyring::Error::NoEntry) => {}
                Err(e) => bail!("keychain delete failed: {}", e),
            }
        }
        // Also remove from fallback file if present
        let fallback = self.fallback_path(GLOBAL_SERVICE);
        if fallback.exists() {
            let mut pairs = read_env_file(&fallback).unwrap_or_default();
            pairs.retain(|(k, _)| k != key);
            write_env_file(&fallback, &pairs)?;
        }

        // Remove from metadata
        let mut meta = self.load_metadata()?;
        meta.secrets.remove(key);
        self.save_metadata(&meta)
    }

    /// List all secret entries (metadata only, no values).
    pub(crate) fn list(&self) -> Result<Vec<SecretEntry>> {
        let meta = self.load_metadata()?;
        let mut entries: Vec<_> = meta.secrets.into_values().collect();
        entries.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(entries)
    }

    /// Import key=value pairs, storing each as a global secret.
    pub(crate) fn import_env_file(&self, path: &Path) -> Result<usize> {
        let pairs = read_env_file(path)?;
        let count = pairs.len();
        for (key, value) in pairs {
            self.set(&key, &value, None, None, None)?;
        }
        Ok(count)
    }

    /// Update ACL for an existing key.
    pub(crate) fn update_acl(
        &self,
        key: &str,
        allow: Option<Vec<String>>,
        deny: Option<Vec<String>>,
    ) -> Result<()> {
        let mut meta = self.load_metadata()?;
        let entry = meta
            .secrets
            .get_mut(key)
            .with_context(|| format!("secret '{}' not found", key))?;
        if let Some(a) = allow {
            entry.allow = Some(a);
        }
        if let Some(d) = deny {
            entry.deny = Some(d);
        }
        entry.updated_at = chrono::Utc::now().to_rfc3339();
        self.save_metadata(&meta)
    }

    // ── private helpers ──────────────────────────────────────────────────────

    fn write_value(&self, service: &str, key: &str, value: &str) -> Result<()> {
        if self.use_keyring {
            let entry = Entry::new(service, key)?;
            entry
                .set_password(value)
                .with_context(|| format!("keychain write failed for '{}'", key))?;
        } else {
            let path = self.fallback_path(service);
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

    fn read_value(&self, service: &str, key: &str) -> Result<Option<String>> {
        if self.use_keyring {
            let entry = Entry::new(service, key)?;
            match entry.get_password() {
                Ok(v) => return Ok(Some(v)),
                Err(keyring::Error::NoEntry) => {}
                Err(e) if is_nonfatal_keyring_error(&e) => {}
                Err(e) => bail!("keychain read failed for '{}': {}", key, e),
            }
        }
        // Fallback: try the local file
        let path = self.fallback_path(service);
        if path.exists() {
            let pairs = read_env_file(&path).unwrap_or_default();
            if let Some((_, v)) = pairs.into_iter().find(|(k, _)| k == key) {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    fn fallback_path(&self, service: &str) -> PathBuf {
        let safe_service = service
            .chars()
            .map(|c| if matches!(c, '.' | '/') { '_' } else { c })
            .collect::<String>();
        self.home
            .join(FALLBACK_DIR)
            .join(format!("{}.env", safe_service))
    }

    fn metadata_path(&self) -> PathBuf {
        self.home.join(METADATA_FILE)
    }

    fn load_metadata(&self) -> Result<Metadata> {
        let path = self.metadata_path();
        if !path.exists() {
            return Ok(Metadata::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read secrets metadata from {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse secrets metadata from {}", path.display()))
    }

    fn save_metadata(&self, meta: &Metadata) -> Result<()> {
        let path = self.metadata_path();
        let rendered = serde_json::to_string_pretty(meta)?;
        write_secure_file(&path, rendered.as_bytes())
    }
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

/// Write a file with 0600 permissions (owner-read/write only), using an atomic temp-rename.
///
/// # Platform notes
/// On Unix the file is created with `O_CREAT | mode 0600` and an explicit `chmod` to
/// prevent any window where another process could read a world-readable temp file.
///
/// On Windows (`#[cfg(not(unix))]`) no equivalent ACL restriction is applied — the
/// file inherits the default NTFS ACL from its parent directory.  This tool is
/// primarily targeted at macOS/Linux; Windows support is best-effort.
pub(crate) fn write_secure_file(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("secrets path must have a parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create directory {}", parent.display()))?;

    // Write to a temp file then atomically rename
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
