use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;

use age::armor::{ArmoredReader, ArmoredWriter, Format};
use age::secrecy::{ExposeSecret, Secret};
use age::x25519;
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::traits::{BackendEntry, CredentialBackend, CredentialKey};
use crate::application::credential::write_secure_file;

const SCHEMA_VERSION: &str = "0.1";

/// JSON structure stored inside each `.age` namespace file.
#[derive(Debug, Serialize, Deserialize)]
struct NamespaceData {
    schema_version: String,
    namespace: String,
    created_at: String,
    updated_at: String,
    #[serde(default)]
    entries: HashMap<String, NamespaceEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NamespaceEntry {
    value: String,
    created_at: String,
    updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    allow: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    deny: Option<Vec<String>>,
}

impl NamespaceData {
    fn new(namespace: &str) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            schema_version: SCHEMA_VERSION.into(),
            namespace: namespace.into(),
            created_at: now.clone(),
            updated_at: now,
            entries: HashMap::new(),
        }
    }
}

/// Primary persistent backend using age file encryption.
///
/// Path layout (relative to `home`):
///   - Identity key:  `.ato/keys/identity.key` (shared across all domains)
///   - Public key:    `.ato/keys/identity.pub`
///   - Namespace file: `.ato/credentials/<domain>/<sub>.age`
///
/// Hierarchical namespace parsing: `"secrets/default"` →
/// domain `"secrets"`, sub `"default"` → `.ato/credentials/secrets/default.age`.
pub(crate) struct AgeFileBackend {
    home: PathBuf,
    identity: Mutex<Option<x25519::Identity>>,
}

impl AgeFileBackend {
    pub(crate) fn new(home: PathBuf) -> Self {
        Self {
            home,
            identity: Mutex::new(None),
        }
    }

    pub(crate) fn keys_dir(&self) -> PathBuf {
        self.home.join(".ato/keys")
    }

    pub(crate) fn credentials_dir(&self) -> PathBuf {
        self.home.join(".ato/credentials")
    }

    pub(crate) fn identity_key_path(&self) -> PathBuf {
        self.keys_dir().join("identity.key")
    }

    pub(crate) fn identity_pub_path(&self) -> PathBuf {
        self.keys_dir().join("identity.pub")
    }

    /// Check whether an identity key file exists on disk.
    pub(crate) fn identity_exists(&self) -> bool {
        self.identity_key_path().exists()
    }

    /// Generate a new X25519 identity and persist it.
    ///
    /// If `passphrase` is `Some`, the identity file is encrypted with that
    /// passphrase (armored age format). With `None` the key is stored as plain
    /// text with `chmod 600`.
    pub(crate) fn init_identity(&self, passphrase: Option<&str>) -> Result<x25519::Identity> {
        let identity = x25519::Identity::generate();
        let identity_secret = identity.to_string();
        let identity_str = identity_secret.expose_secret();
        let public_str = identity.to_public().to_string();

        std::fs::create_dir_all(self.keys_dir()).context("failed to create ~/.ato/keys/")?;

        let key_path = self.identity_key_path();

        if let Some(pp) = passphrase {
            let encryptor = age::Encryptor::with_user_passphrase(Secret::new(pp.to_string()));
            let mut encrypted = vec![];
            {
                let mut armored = ArmoredWriter::wrap_output(&mut encrypted, Format::AsciiArmor)
                    .context("failed to create armored writer")?;
                let mut writer = encryptor
                    .wrap_output(&mut armored)
                    .context("failed to wrap output for passphrase encryption")?;
                writer
                    .write_all(identity_str.as_bytes())
                    .context("failed to write identity")?;
                writer.finish().context("failed to finish encryption")?;
                armored.finish().context("failed to finish armoring")?;
            }
            write_secure_file(&key_path, &encrypted)?;
        } else {
            write_secure_file(&key_path, identity_str.as_bytes())?;
        }

        write_secure_file(&self.identity_pub_path(), public_str.as_bytes())?;

        *self.identity.lock().unwrap() = Some(
            identity_str
                .parse::<x25519::Identity>()
                .expect("round-trip identity parse failed"),
        );
        Ok(identity)
    }

    /// Ensure identity is loaded, using the provided passphrase if the key file
    /// is passphrase-encrypted. Returns `Err` if no identity exists yet.
    pub(crate) fn load_identity_with_passphrase(&self, passphrase: Option<&str>) -> Result<()> {
        {
            let guard = self.identity.lock().unwrap();
            if guard.is_some() {
                return Ok(());
            }
        }

        let key_path = self.identity_key_path();
        if !key_path.exists() {
            bail!(
                "no age identity found at {}\n\
                 Run `ato secrets init` to create one.",
                key_path.display()
            );
        }

        let raw = std::fs::read(&key_path)
            .with_context(|| format!("failed to read {}", key_path.display()))?;

        let identity =
            load_identity_bytes(&raw, passphrase).context("failed to load age identity")?;

        *self.identity.lock().unwrap() = Some(identity);
        Ok(())
    }

    /// Return true when an identity is already loaded in memory.
    #[cfg(test)]
    pub(crate) fn is_identity_loaded(&self) -> bool {
        self.identity.lock().unwrap().is_some()
    }

    /// Return the raw `AGE-SECRET-KEY-1...` string for session key file export.
    ///
    /// Only called by `ato session start`; the string is written to a chmod 600
    /// session file and never logged or displayed.
    pub(crate) fn identity_for_session(&self) -> Option<String> {
        let guard = self.identity.lock().unwrap();
        guard
            .as_ref()
            .map(|id| id.to_string().expose_secret().to_string())
    }

    /// Directly install an already-loaded identity (used by session key file path).
    pub(crate) fn install_identity(&mut self, identity: x25519::Identity) {
        *self.identity.lock().unwrap() = Some(identity);
    }

    fn get_identity(&self) -> Result<x25519::Identity> {
        let guard = self.identity.lock().unwrap();
        guard
            .as_ref()
            .map(|id| {
                id.to_string()
                    .expose_secret()
                    .parse::<x25519::Identity>()
                    .expect("round-trip identity parse failed")
            })
            .ok_or_else(|| {
                anyhow::anyhow!("age identity not loaded – run `ato secrets init` first")
            })
    }

    /// Parse a hierarchical namespace into `(domain, sub)`.
    ///
    /// If no `/` is present, the namespace is placed under the `misc` domain
    /// as a safety fallback (not expected during normal operation).
    fn split_namespace(namespace: &str) -> (String, String) {
        if let Some((d, s)) = namespace.split_once('/') {
            (d.to_string(), s.to_string())
        } else {
            ("misc".to_string(), namespace.to_string())
        }
    }

    fn namespace_path(&self, namespace: &str) -> PathBuf {
        let (domain, sub) = Self::split_namespace(namespace);
        self.credentials_dir()
            .join(sanitize_path_segment(&domain))
            .join(format!("{}.age", sanitize_path_segment(&sub)))
    }

    fn read_namespace(&self, namespace: &str) -> Result<NamespaceData> {
        let path = self.namespace_path(namespace);
        if !path.exists() {
            return Ok(NamespaceData::new(namespace));
        }

        let identity = self.get_identity()?;
        let raw =
            std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;

        let decryptor = match age::Decryptor::new(&raw[..]).context("failed to parse age file")? {
            age::Decryptor::Recipients(d) => d,
            age::Decryptor::Passphrase(_) => {
                bail!("unexpected passphrase-encrypted namespace file")
            }
        };

        let mut reader = decryptor
            .decrypt(std::iter::once(&identity as &dyn age::Identity))
            .context("failed to decrypt namespace file")?;

        let mut plaintext = Vec::new();
        reader
            .read_to_end(&mut plaintext)
            .context("failed to read decrypted data")?;

        serde_json::from_slice(&plaintext)
            .with_context(|| format!("failed to parse namespace JSON for '{}'", namespace))
    }

    fn write_namespace(&self, namespace: &str, data: &NamespaceData) -> Result<()> {
        let identity = self.get_identity()?;
        let recipient: Box<dyn age::Recipient + Send + 'static> = Box::new(identity.to_public());

        let final_path = self.namespace_path(namespace);
        let tmp_path = final_path.with_extension(format!("age.tmp.{}", std::process::id()));

        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory {}", parent.display()))?;
        }

        // Acquire advisory lock on a lock file.
        let lock_path = final_path.with_extension("age.lock");
        let _lock = acquire_lock(&lock_path)?;

        let encryptor = age::Encryptor::with_recipients(vec![recipient])
            .ok_or_else(|| anyhow::anyhow!("failed to build encryptor (empty recipients?)"))?;

        let json = serde_json::to_vec_pretty(data).context("failed to serialize namespace")?;
        let mut encrypted = Vec::new();
        {
            let mut writer = encryptor
                .wrap_output(&mut encrypted)
                .context("failed to wrap encryption output")?;
            writer
                .write_all(&json)
                .context("failed to write plaintext")?;
            writer.finish().context("failed to finish encryption")?;
        }

        write_secure_file(&tmp_path, &encrypted)?;
        std::fs::rename(&tmp_path, &final_path).with_context(|| {
            format!(
                "failed to rename {} → {}",
                tmp_path.display(),
                final_path.display()
            )
        })?;

        Ok(())
    }

    /// Decrypt ALL namespace files under `<credentials>/<domain>/` and re-encrypt
    /// to a new identity. Used by `rotate-identity`.
    pub(crate) fn reencrypt_all(&self, new_identity: &x25519::Identity) -> Result<()> {
        let credentials_dir = self.credentials_dir();
        if !credentials_dir.exists() {
            return Ok(());
        }

        for domain_entry in
            std::fs::read_dir(&credentials_dir).context("failed to read credentials dir")?
        {
            let domain_entry = domain_entry?;
            let domain_path = domain_entry.path();
            if !domain_path.is_dir() {
                continue;
            }
            let domain = domain_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            if domain.is_empty() {
                continue;
            }

            for entry in std::fs::read_dir(&domain_path)
                .with_context(|| format!("failed to read {}", domain_path.display()))?
            {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("age") {
                    continue;
                }
                let sub = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                if sub.is_empty() {
                    continue;
                }
                let full_ns = format!("{}/{}", domain, sub);

                let data = self.read_namespace(&full_ns)?;

                let old_identity = self.identity.lock().unwrap().take();
                *self.identity.lock().unwrap() = Some(clone_identity(new_identity));

                let result = self.write_namespace(&full_ns, &data);

                *self.identity.lock().unwrap() = old_identity;
                result?;
            }
        }
        Ok(())
    }

    /// Return all sub-namespace names found under `<credentials>/<domain>/`.
    ///
    /// E.g. for domain `"secrets"`, might return `["default", "capsule_myapp"]`.
    /// Callers compose the full namespace as `format!("{domain}/{sub}")`.
    pub(crate) fn list_sub_namespaces(&self, domain: &str) -> Vec<String> {
        let domain_dir = self.credentials_dir().join(sanitize_path_segment(domain));
        if !domain_dir.exists() {
            return vec![];
        }
        std::fs::read_dir(&domain_dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let p = e.path();
                if p.extension()?.to_str()? == "age" {
                    Some(p.file_stem()?.to_str()?.to_string())
                } else {
                    None
                }
            })
            .collect()
    }
}

impl CredentialBackend for AgeFileBackend {
    fn name(&self) -> &'static str {
        "age"
    }

    fn is_available(&self) -> bool {
        self.identity.lock().unwrap().is_some()
    }

    fn get(&self, key: &CredentialKey) -> Result<Option<String>> {
        let data = self.read_namespace(&key.namespace)?;
        Ok(data.entries.get(&key.name).map(|e| e.value.clone()))
    }

    fn set(
        &self,
        key: &CredentialKey,
        value: String,
        description: Option<&str>,
        allow: Option<Vec<String>>,
        deny: Option<Vec<String>>,
    ) -> Result<()> {
        let mut data = self.read_namespace(&key.namespace)?;
        let now = chrono::Utc::now().to_rfc3339();
        let entry = data
            .entries
            .entry(key.name.clone())
            .or_insert_with(|| NamespaceEntry {
                value: String::new(),
                created_at: now.clone(),
                updated_at: now.clone(),
                description: None,
                allow: None,
                deny: None,
            });
        entry.value = value;
        entry.updated_at = now;
        if let Some(d) = description {
            entry.description = Some(d.to_string());
        }
        if allow.is_some() {
            entry.allow = allow;
        }
        if deny.is_some() {
            entry.deny = deny;
        }
        data.updated_at = chrono::Utc::now().to_rfc3339();
        self.write_namespace(&key.namespace, &data)
    }

    fn delete(&self, key: &CredentialKey) -> Result<()> {
        let mut data = self.read_namespace(&key.namespace)?;
        data.entries.remove(&key.name);
        data.updated_at = chrono::Utc::now().to_rfc3339();
        self.write_namespace(&key.namespace, &data)
    }

    fn list(&self, namespace: &str) -> Result<Vec<BackendEntry>> {
        let data = self.read_namespace(namespace)?;
        let mut entries: Vec<BackendEntry> = data
            .entries
            .into_iter()
            .map(|(name, e)| BackendEntry {
                key: name,
                namespace: namespace.to_string(),
                created_at: e.created_at,
                updated_at: e.updated_at,
                description: e.description,
                allow: e.allow,
                deny: e.deny,
            })
            .collect();
        entries.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(entries)
    }

    fn update_acl(
        &self,
        key: &CredentialKey,
        allow: Option<Vec<String>>,
        deny: Option<Vec<String>>,
    ) -> Result<()> {
        let mut data = self.read_namespace(&key.namespace)?;
        let entry = data.entries.get_mut(&key.name).with_context(|| {
            format!(
                "credential '{}' not found in namespace '{}'",
                key.name, key.namespace
            )
        })?;
        if allow.is_some() {
            entry.allow = allow;
        }
        if deny.is_some() {
            entry.deny = deny;
        }
        entry.updated_at = chrono::Utc::now().to_rfc3339();
        data.updated_at = chrono::Utc::now().to_rfc3339();
        self.write_namespace(&key.namespace, &data)
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// Convert one path segment (domain or sub) to a safe filename.
fn sanitize_path_segment(seg: &str) -> String {
    seg.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => c,
            _ => '_',
        })
        .collect()
}

/// Load an age X25519 identity from raw bytes.
///
/// If the bytes look like a passphrase-encrypted armored age file, decrypts
/// using the provided passphrase. Otherwise treats them as plain text.
pub(crate) fn load_identity_bytes(
    raw: &[u8],
    passphrase: Option<&str>,
) -> Result<x25519::Identity> {
    let is_armored = raw.get(..11).map(|h| h == b"-----BEGIN ").unwrap_or(false);
    let is_age_binary = raw
        .get(..14)
        .map(|h| h == b"age-encryption")
        .unwrap_or(false);

    if is_armored || is_age_binary {
        let pp = passphrase.ok_or_else(|| {
            anyhow::anyhow!("identity.key is passphrase-protected but no passphrase was provided")
        })?;

        let plaintext = if is_armored {
            let armored = ArmoredReader::new(raw);
            decrypt_passphrase(armored, pp)?
        } else {
            decrypt_passphrase(raw, pp)?
        };

        let text = String::from_utf8(plaintext).context("identity key is not valid UTF-8")?;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with("AGE-SECRET-KEY-") {
                return x25519::Identity::from_str(line)
                    .map_err(|e| anyhow::anyhow!("invalid age identity: {}", e));
            }
        }
        bail!("no AGE-SECRET-KEY found in decrypted identity file");
    } else {
        let text = std::str::from_utf8(raw).context("identity.key is not valid UTF-8")?;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with("AGE-SECRET-KEY-") {
                return x25519::Identity::from_str(line)
                    .map_err(|e| anyhow::anyhow!("invalid age identity: {}", e));
            }
        }
        bail!("no AGE-SECRET-KEY-1... line found in {}", "identity.key");
    }
}

fn decrypt_passphrase<R: Read>(input: R, passphrase: &str) -> Result<Vec<u8>> {
    let decryptor = match age::Decryptor::new(input).context("failed to parse age file")? {
        age::Decryptor::Passphrase(d) => d,
        age::Decryptor::Recipients(_) => {
            bail!("expected passphrase-encrypted identity, got recipient-encrypted")
        }
    };
    let mut reader = decryptor
        .decrypt(&Secret::new(passphrase.to_string()), None)
        .context("wrong passphrase for identity.key")?;
    let mut out = Vec::new();
    reader.read_to_end(&mut out)?;
    Ok(out)
}

fn acquire_lock(lock_path: &Path) -> Result<File> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)
        .with_context(|| format!("failed to open lock file {}", lock_path.display()))?;
    file.try_lock_exclusive()
        .or_else(|_| {
            std::thread::sleep(std::time::Duration::from_secs(5));
            file.try_lock_exclusive()
        })
        .with_context(|| {
            format!(
                "failed to acquire exclusive lock on {} – another ato process may be running",
                lock_path.display()
            )
        })?;
    Ok(file)
}

fn clone_identity(id: &x25519::Identity) -> x25519::Identity {
    id.to_string()
        .expose_secret()
        .parse::<x25519::Identity>()
        .expect("round-trip identity parse failed")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tmp_backend() -> (TempDir, AgeFileBackend) {
        let dir = TempDir::new().expect("tempdir");
        let backend = AgeFileBackend::new(dir.path().to_path_buf());
        (dir, backend)
    }

    fn init_backend() -> (TempDir, AgeFileBackend) {
        let (dir, backend) = tmp_backend();
        backend.init_identity(None).expect("init_identity");
        (dir, backend)
    }

    fn secrets_key(name: &str) -> CredentialKey {
        CredentialKey::new("secrets/default", name)
    }

    fn ns_key(ns: &str, name: &str) -> CredentialKey {
        CredentialKey::new(ns, name)
    }

    #[test]
    fn identity_not_exists_before_init() {
        let (_dir, backend) = tmp_backend();
        assert!(!backend.identity_exists());
    }

    #[test]
    fn identity_exists_after_init() {
        let (_dir, backend) = init_backend();
        assert!(backend.identity_exists());
    }

    #[test]
    fn init_identity_creates_key_and_pub_files() {
        let (dir, backend) = tmp_backend();
        backend.init_identity(None).expect("init_identity");
        assert!(dir.path().join(".ato/keys/identity.key").exists());
        assert!(dir.path().join(".ato/keys/identity.pub").exists());
    }

    #[test]
    fn init_identity_returns_loaded_identity() {
        let (_dir, backend) = tmp_backend();
        backend.init_identity(None).expect("init_identity");
        assert!(backend.is_identity_loaded());
    }

    #[test]
    fn roundtrip_set_get_default_namespace() {
        let (_dir, backend) = init_backend();
        let key = secrets_key("API_KEY");
        backend
            .set(&key, "secret-value".into(), None, None, None)
            .expect("set");
        let got = backend.get(&key).expect("get");
        assert_eq!(got, Some("secret-value".to_string()));
    }

    #[test]
    fn set_writes_under_credentials_secrets_dir() {
        let (dir, backend) = init_backend();
        let key = secrets_key("TEST");
        backend
            .set(&key, "v".into(), None, None, None)
            .expect("set");
        let path = dir.path().join(".ato/credentials/secrets/default.age");
        assert!(path.exists(), "expected file at {}", path.display());
    }

    #[test]
    fn auth_namespace_separated_from_secrets() {
        let (dir, backend) = init_backend();
        let s_key = secrets_key("FOO");
        let a_key = ns_key("auth/session", "SESSION_TOKEN");
        backend
            .set(&s_key, "secret-foo".into(), None, None, None)
            .unwrap();
        backend
            .set(&a_key, "auth-token".into(), None, None, None)
            .unwrap();

        assert!(dir
            .path()
            .join(".ato/credentials/secrets/default.age")
            .exists());
        assert!(dir
            .path()
            .join(".ato/credentials/auth/session.age")
            .exists());
        assert_eq!(backend.get(&s_key).unwrap(), Some("secret-foo".into()));
        assert_eq!(backend.get(&a_key).unwrap(), Some("auth-token".into()));
    }

    #[test]
    fn get_returns_none_for_missing_key() {
        let (_dir, backend) = init_backend();
        let key = secrets_key("NONEXISTENT");
        assert_eq!(backend.get(&key).expect("get"), None);
    }

    #[test]
    fn delete_removes_key() {
        let (_dir, backend) = init_backend();
        let key = secrets_key("TEMP");
        backend.set(&key, "x".into(), None, None, None).unwrap();
        backend.delete(&key).unwrap();
        assert_eq!(backend.get(&key).unwrap(), None);
    }

    #[test]
    fn list_returns_all_keys_sorted() {
        let (_dir, backend) = init_backend();
        for k in &["ZEBRA", "ALPHA", "MIDDLE"] {
            backend
                .set(&secrets_key(k), "v".into(), None, None, None)
                .unwrap();
        }
        let entries = backend.list("secrets/default").unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(names, vec!["ALPHA", "MIDDLE", "ZEBRA"]);
    }

    #[test]
    fn list_sub_namespaces_enumerates_domain() {
        let (_dir, backend) = init_backend();
        backend
            .set(
                &ns_key("secrets/default", "K"),
                "v".into(),
                None,
                None,
                None,
            )
            .unwrap();
        backend
            .set(
                &ns_key("secrets/project_a", "K"),
                "v".into(),
                None,
                None,
                None,
            )
            .unwrap();
        backend
            .set(&ns_key("auth/session", "K"), "v".into(), None, None, None)
            .unwrap();
        let mut subs = backend.list_sub_namespaces("secrets");
        subs.sort();
        assert_eq!(subs, vec!["default", "project_a"]);
        let auth_subs = backend.list_sub_namespaces("auth");
        assert_eq!(auth_subs, vec!["session"]);
    }

    #[test]
    fn load_identity_plain_text_succeeds() {
        let (dir, _backend) = init_backend();
        let fresh = AgeFileBackend::new(dir.path().to_path_buf());
        assert!(fresh.load_identity_with_passphrase(None).is_ok());
        assert!(fresh.is_identity_loaded());
    }

    #[test]
    fn load_identity_nonexistent_returns_err() {
        let (_dir, backend) = tmp_backend();
        assert!(backend.load_identity_with_passphrase(None).is_err());
    }
}
