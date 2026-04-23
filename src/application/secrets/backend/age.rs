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

use super::traits::{BackendEntry, SecretBackend, SecretKey};
use crate::application::secrets::store::write_secure_file;

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
/// Each namespace is stored as `~/.ato/secrets/<namespace>.age`.
/// The encryption key is an X25519 identity stored at `~/.ato/keys/identity.key`.
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

    pub(crate) fn secrets_dir(&self) -> PathBuf {
        self.home.join(".ato/secrets")
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
    /// passphrase (armored age format).  With `None` the key is stored as plain
    /// text with `chmod 600`.
    pub(crate) fn init_identity(&self, passphrase: Option<&str>) -> Result<x25519::Identity> {
        let identity = x25519::Identity::generate();
        let identity_secret = identity.to_string();
        let identity_str = identity_secret.expose_secret();
        let public_str = identity.to_public().to_string();

        std::fs::create_dir_all(self.keys_dir()).context("failed to create ~/.ato/keys/")?;

        let key_path = self.identity_key_path();

        if let Some(pp) = passphrase {
            // Encrypt the identity string with the passphrase (armored age).
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
            // Plain text, chmod 600.
            write_secure_file(&key_path, identity_str.as_bytes())?;
        }

        // Always write the public key in plain text.
        write_secure_file(&self.identity_pub_path(), public_str.as_bytes())?;

        *self.identity.lock().unwrap() = Some(
            identity_str
                .parse::<x25519::Identity>()
                .expect("round-trip identity parse failed"),
        );
        Ok(identity)
    }

    /// Ensure identity is loaded, using the provided passphrase if the key file
    /// is passphrase-encrypted.  Returns `Err` if no identity exists yet.
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

    fn namespace_path(&self, namespace: &str) -> PathBuf {
        self.secrets_dir()
            .join(format!("{}.age", namespace_to_filename(namespace)))
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

        std::fs::create_dir_all(self.secrets_dir())
            .context("failed to create secrets directory")?;

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

    /// Decrypt ALL namespace files and re-encrypt to a new identity.
    /// Used by `rotate-identity`.
    pub(crate) fn reencrypt_all(&self, new_identity: &x25519::Identity) -> Result<()> {
        let secrets_dir = self.secrets_dir();
        if !secrets_dir.exists() {
            return Ok(());
        }

        for entry in std::fs::read_dir(&secrets_dir).context("failed to read secrets dir")? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("age") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();

            // Read with current identity.
            let data = self.read_namespace(&stem)?;

            // Temporarily swap identity to new one.
            let old_identity = self.identity.lock().unwrap().take();
            *self.identity.lock().unwrap() = Some(clone_identity(new_identity));

            let result = self.write_namespace(&stem, &data);

            // Restore old identity regardless.
            *self.identity.lock().unwrap() = old_identity;
            result?;
        }
        Ok(())
    }

    /// Return all namespace file stems found in secrets_dir.
    pub(crate) fn list_namespaces(&self) -> Vec<String> {
        let secrets_dir = self.secrets_dir();
        if !secrets_dir.exists() {
            return vec![];
        }
        std::fs::read_dir(&secrets_dir)
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

impl SecretBackend for AgeFileBackend {
    fn get(&self, key: &SecretKey) -> Result<Option<String>> {
        let data = self.read_namespace(&key.namespace)?;
        Ok(data.entries.get(&key.name).map(|e| e.value.clone()))
    }

    fn set(
        &self,
        key: &SecretKey,
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

    fn delete(&self, key: &SecretKey) -> Result<()> {
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
        key: &SecretKey,
        allow: Option<Vec<String>>,
        deny: Option<Vec<String>>,
    ) -> Result<()> {
        let mut data = self.read_namespace(&key.namespace)?;
        let entry = data.entries.get_mut(&key.name).with_context(|| {
            format!(
                "secret '{}' not found in namespace '{}'",
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

/// Convert a namespace string to a safe filename (no slashes, etc.).
fn namespace_to_filename(ns: &str) -> String {
    ns.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => c,
            _ => '_',
        })
        .collect()
}

/// Load an age X25519 identity from raw bytes.
///
/// If the bytes look like a passphrase-encrypted armored age file, decrypts
/// using the provided passphrase.  Otherwise treats them as plain text.
pub(crate) fn load_identity_bytes(
    raw: &[u8],
    passphrase: Option<&str>,
) -> Result<x25519::Identity> {
    // Detect format: armored age starts with "-----BEGIN"
    let is_armored = raw.get(..11).map(|h| h == b"-----BEGIN ").unwrap_or(false);
    // Binary age header
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
        // Plain text: scan for AGE-SECRET-KEY-1... line.
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
    // Try for up to 5 seconds.
    file.try_lock_exclusive()
        .or_else(|_| {
            // Retry once after a short sleep.
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

    fn make_key(name: &str) -> SecretKey {
        SecretKey::new(name)
    }

    fn make_ns_key(ns: &str, name: &str) -> SecretKey {
        SecretKey::with_namespace(ns, name)
    }

    // ── identity_exists ───────────────────────────────────────────────────────

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

    // ── init_identity ─────────────────────────────────────────────────────────

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

    // ── roundtrip: set / get ──────────────────────────────────────────────────

    #[test]
    fn roundtrip_set_get_default_namespace() {
        let (_dir, backend) = init_backend();
        let key = make_key("API_KEY");
        backend
            .set(&key, "secret-value".into(), None, None, None)
            .expect("set");
        let got = backend.get(&key).expect("get");
        assert_eq!(got, Some("secret-value".to_string()));
    }

    #[test]
    fn get_returns_none_for_missing_key() {
        let (_dir, backend) = init_backend();
        let key = make_key("NONEXISTENT");
        let got = backend.get(&key).expect("get");
        assert_eq!(got, None);
    }

    #[test]
    fn roundtrip_with_description_and_acl() {
        let (_dir, backend) = init_backend();
        let key = make_key("DB_PASS");
        backend
            .set(
                &key,
                "hunter2".into(),
                Some("database password"),
                Some(vec!["capsule:myapp".into()]),
                None,
            )
            .expect("set");
        let entries = backend.list("default").expect("list");
        let entry = entries.iter().find(|e| e.key == "DB_PASS").expect("entry");
        assert_eq!(entry.description.as_deref(), Some("database password"));
        assert_eq!(
            entry.allow.as_deref(),
            Some(&["capsule:myapp".to_string()][..])
        );
    }

    // ── delete ────────────────────────────────────────────────────────────────

    #[test]
    fn delete_removes_key() {
        let (_dir, backend) = init_backend();
        let key = make_key("TEMP");
        backend
            .set(&key, "x".into(), None, None, None)
            .expect("set");
        backend.delete(&key).expect("delete");
        assert_eq!(backend.get(&key).expect("get"), None);
    }

    #[test]
    fn delete_nonexistent_is_noop() {
        let (_dir, backend) = init_backend();
        let key = make_key("GHOST");
        backend
            .delete(&key)
            .expect("delete nonexistent should be ok");
    }

    // ── list ──────────────────────────────────────────────────────────────────

    #[test]
    fn list_empty_namespace_returns_empty() {
        let (_dir, backend) = init_backend();
        let entries = backend.list("default").expect("list");
        assert!(entries.is_empty());
    }

    #[test]
    fn list_returns_all_keys_sorted() {
        let (_dir, backend) = init_backend();
        for k in &["ZEBRA", "ALPHA", "MIDDLE"] {
            backend
                .set(&make_key(k), "v".into(), None, None, None)
                .expect("set");
        }
        let entries = backend.list("default").expect("list");
        let names: Vec<_> = entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(names, vec!["ALPHA", "MIDDLE", "ZEBRA"]);
    }

    // ── namespace isolation ───────────────────────────────────────────────────

    #[test]
    fn namespace_isolation_default_vs_custom() {
        let (_dir, backend) = init_backend();
        let default_key = make_ns_key("default", "FOO");
        let other_key = make_ns_key("project_a", "FOO");

        backend
            .set(&default_key, "default-value".into(), None, None, None)
            .expect("set default");
        backend
            .set(&other_key, "project-value".into(), None, None, None)
            .expect("set project");

        assert_eq!(
            backend.get(&default_key).expect("get default"),
            Some("default-value".to_string())
        );
        assert_eq!(
            backend.get(&other_key).expect("get project"),
            Some("project-value".to_string())
        );
    }

    #[test]
    fn list_namespaces_returns_created_namespaces() {
        let (_dir, backend) = init_backend();
        backend
            .set(&make_ns_key("ns_a", "K"), "v".into(), None, None, None)
            .expect("set");
        backend
            .set(&make_ns_key("ns_b", "K"), "v".into(), None, None, None)
            .expect("set");
        let ns = backend.list_namespaces();
        assert!(ns.contains(&"ns_a".to_string()), "ns_a not in {:?}", ns);
        assert!(ns.contains(&"ns_b".to_string()), "ns_b not in {:?}", ns);
    }

    // ── schema versioning ─────────────────────────────────────────────────────

    #[test]
    fn namespace_file_has_correct_schema_version() {
        let (_dir, backend) = init_backend();
        backend
            .set(&make_key("X"), "y".into(), None, None, None)
            .expect("set");
        // Read and decrypt to verify schema_version in JSON.
        let identity = {
            let guard = backend.identity.lock().unwrap();
            guard
                .as_ref()
                .map(|id| {
                    id.to_string()
                        .expose_secret()
                        .parse::<x25519::Identity>()
                        .unwrap()
                })
                .unwrap()
        };
        let path = backend.namespace_path("default");
        let ciphertext = std::fs::read(&path).unwrap();
        let decryptor = match age::Decryptor::new(&ciphertext[..]).unwrap() {
            age::Decryptor::Recipients(d) => d,
            _ => panic!("expected recipient encryption"),
        };
        let mut plaintext = Vec::new();
        let mut reader = decryptor
            .decrypt(std::iter::once(&identity as &dyn age::Identity))
            .unwrap();
        std::io::Read::read_to_end(&mut reader, &mut plaintext).unwrap();
        let data: serde_json::Value = serde_json::from_slice(&plaintext).unwrap();
        assert_eq!(data["schema_version"], "0.1");
    }

    // ── load_identity_with_passphrase ─────────────────────────────────────────

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
