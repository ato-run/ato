use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::credential_store::{AuthStore, WriteLocation};
use super::{
    read_env_non_empty, AuthManager, Credentials, CANONICAL_CREDENTIALS_DIR,
    CANONICAL_CREDENTIALS_FILE, ENV_XDG_CONFIG_HOME, LEGACY_CREDENTIALS_DIR,
    LEGACY_CREDENTIALS_FILE,
};

/// Physical destination of a freshly persisted token.
///
/// Phase 5: OS keyring has been removed entirely. New tokens land in the
/// shared age file (`AgeFile`), the canonical TOML file (headless), or the
/// in-process memory cache as a last-resort fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TokenStorageLocation {
    /// Shared encrypted age file at `~/.ato/credentials/auth/session.age` (or
    /// `auth/github.age`).
    AgeFile,
    /// Canonical TOML metadata file — used in headless mode and when the age
    /// identity isn't unlocked.
    CanonicalFile,
    /// In-process memory cache only. Reached when no age identity is
    /// available; surfaced so the UI can warn the user their token will not
    /// survive the process.
    Memory,
}

impl TokenStorageLocation {
    pub(super) fn display(&self) -> &'static str {
        match self {
            TokenStorageLocation::AgeFile => "age file",
            TokenStorageLocation::CanonicalFile => "credentials file",
            TokenStorageLocation::Memory => "in-memory cache (age identity not loaded)",
        }
    }

    fn from_write_location(loc: WriteLocation) -> Self {
        match loc {
            WriteLocation::AgeFile => TokenStorageLocation::AgeFile,
            WriteLocation::Memory => TokenStorageLocation::Memory,
        }
    }
}

impl AuthManager {
    /// Create a new AuthManager with default credentials path.
    pub fn new() -> Result<Self> {
        let home = dirs::home_dir().context("Cannot determine home directory")?;
        let credentials_path = canonical_credentials_path(&home);
        let legacy_credentials_path = legacy_credentials_path(&home);
        let auth_store = Arc::new(AuthStore::open_for_home(&home)?);
        Ok(Self {
            credentials_path,
            legacy_credentials_path,
            age_home: home,
            auth_store,
        })
    }

    #[cfg(test)]
    pub fn with_paths(credentials_path: PathBuf, legacy_credentials_path: PathBuf) -> Self {
        // Age backends are per-temp-dir so tests don't share encrypted state.
        // Anchor the temp home on the canonical path's great-grandparent when
        // possible (the tests use `<temp>/config/ato/credentials.toml`), else
        // the legacy path's grandparent (`<temp>/home/.ato/...`).
        let age_home = derive_test_age_home(&credentials_path, &legacy_credentials_path);
        let auth_store = Arc::new(
            AuthStore::open_for_home(&age_home).expect("build AuthStore for test AuthManager"),
        );
        Self {
            credentials_path,
            legacy_credentials_path,
            age_home,
            auth_store,
        }
    }

    /// Borrow the eagerly-constructed `AuthStore`.
    ///
    /// Returned by reference so every caller shares the same in-process
    /// `MemoryBackend`; cloning the `Arc` is reserved for places (like
    /// `spawn_blocking`) that need `'static` ownership.
    pub(super) fn auth_store(&self) -> &AuthStore {
        &self.auth_store
    }

    /// Load sanitized credentials metadata from canonical TOML or legacy JSON.
    pub fn load(&self) -> Result<Option<Credentials>> {
        Ok(self.load_any_credentials()?.map(sanitize_credentials))
    }

    /// Save sanitized metadata into canonical TOML without clobbering stored tokens.
    pub fn save(&self, creds: &Credentials) -> Result<()> {
        let mut persistable = self.load_canonical_credentials()?.unwrap_or_default();
        merge_metadata(&mut persistable, creds);
        self.write_canonical_credentials(&persistable)
    }

    /// Load credentials or return an error if not authenticated
    pub fn require(&self) -> Result<Credentials> {
        let mut creds = self.load()?.unwrap_or_default();
        creds.session_token = self.resolve_session_token()?;
        creds.github_token = self.resolve_github_token()?;

        if creds.session_token.is_none() && creds.github_token.is_none() {
            anyhow::bail!(
                "Not authenticated. Run:\n  ato login\n\nNo usable token found in ATO_TOKEN, age file, {:?}, or {:?}",
                self.credentials_path,
                self.legacy_credentials_path
            );
        }

        Ok(creds)
    }

    /// Delete stored credentials (logout).
    ///
    /// Purges every backend that might hold a token: memory cache and the age
    /// file. Also deletes the canonical TOML credentials file. The legacy
    /// `credentials.json` is intentionally left alone.
    pub fn delete(&self) -> Result<()> {
        self.auth_store().delete_all_auth_tokens()?;

        if self.credentials_path.exists() {
            fs::remove_file(&self.credentials_path).with_context(|| {
                format!(
                    "Failed to delete credentials at {:?}",
                    self.credentials_path
                )
            })?;
        }
        Ok(())
    }

    /// Get the path where credentials are stored
    pub fn credentials_path(&self) -> &PathBuf {
        &self.credentials_path
    }

    pub fn legacy_credentials_path(&self) -> &PathBuf {
        &self.legacy_credentials_path
    }

    pub(super) fn resolve_session_token(&self) -> Result<Option<String>> {
        // `ATO_TOKEN` is handled inside `EnvBackend` as a legacy alias for
        // `ATO_CRED_AUTH_SESSION__SESSION_TOKEN`, so reading through the
        // AuthStore chain honors both forms without a dedicated check here.
        if let Some(token) = self.auth_store().get_session_token()? {
            return Ok(Some(token));
        }

        if let Some(token) = self
            .load_canonical_credentials()?
            .and_then(|c| nonempty(c.session_token))
        {
            return Ok(Some(token));
        }
        if let Some(token) = self
            .load_legacy_credentials()?
            .and_then(|c| nonempty(c.session_token))
        {
            return Ok(Some(token));
        }
        Ok(None)
    }

    pub(super) fn resolve_github_token(&self) -> Result<Option<String>> {
        if let Some(token) = self.auth_store().get_github_token()? {
            return Ok(Some(token));
        }

        if let Some(token) = self
            .load_canonical_credentials()?
            .and_then(|c| nonempty(c.github_token))
        {
            return Ok(Some(token));
        }
        if let Some(token) = self
            .load_legacy_credentials()?
            .and_then(|c| nonempty(c.github_token))
        {
            return Ok(Some(token));
        }
        Ok(None)
    }

    pub(super) async fn save_session_token_async(&self, token: String) -> Result<WriteLocation> {
        let store = self.auth_store.clone();
        tokio::task::spawn_blocking(move || store.set_session_token(&token))
            .await
            .map_err(|err| anyhow::anyhow!("credential worker failed: {err}"))?
    }

    pub(super) async fn save_github_token_async(&self, token: String) -> Result<WriteLocation> {
        let store = self.auth_store.clone();
        tokio::task::spawn_blocking(move || store.set_github_token(&token))
            .await
            .map_err(|err| anyhow::anyhow!("credential worker failed: {err}"))?
    }

    pub(super) fn write_canonical_credentials(&self, creds: &Credentials) -> Result<()> {
        let contents = toml::to_string_pretty(creds).context("Failed to serialize credentials")?;
        write_secure_credentials_file(&self.credentials_path, &contents)
    }

    pub(super) fn load_canonical_credentials(&self) -> Result<Option<Credentials>> {
        read_toml_credentials_file(&self.credentials_path)
    }

    fn load_legacy_credentials(&self) -> Result<Option<Credentials>> {
        read_legacy_json_credentials_file(&self.legacy_credentials_path)
    }

    fn load_any_credentials(&self) -> Result<Option<Credentials>> {
        if let Some(creds) = self.load_canonical_credentials()? {
            return Ok(Some(creds));
        }
        self.load_legacy_credentials()
    }

    pub(super) fn has_persisted_local_state(&self) -> Result<bool> {
        if self.credentials_path.exists() || self.legacy_credentials_path.exists() {
            return Ok(true);
        }
        let store = self.auth_store();
        Ok(store.get_session_token()?.is_some() || store.get_github_token()?.is_some())
    }

    /// Persist a fresh session token.
    ///
    /// - **Headless**: writes the token into the canonical credentials TOML
    ///   so shell redirection and CI can round-trip the value through a
    ///   persistent file. This path never touches the age backend.
    /// - **Interactive**: hands the token to `AuthStore`, which prefers the
    ///   age file but falls back to the in-process memory cache when the
    ///   identity is unavailable (caller should hint at `ato secrets init`).
    pub(super) async fn persist_session_token(
        &self,
        token: String,
        headless: bool,
    ) -> Result<TokenStorageLocation> {
        if headless {
            let mut creds = self.load_canonical_credentials()?.unwrap_or_default();
            creds.session_token = Some(token);
            self.write_canonical_credentials(&creds)?;
            return Ok(TokenStorageLocation::CanonicalFile);
        }

        let location = self.save_session_token_async(token).await?;
        Ok(TokenStorageLocation::from_write_location(location))
    }
}

fn canonical_credentials_path(home: &Path) -> PathBuf {
    if let Some(config_home) = read_env_non_empty(ENV_XDG_CONFIG_HOME) {
        return PathBuf::from(config_home)
            .join(CANONICAL_CREDENTIALS_DIR)
            .join(CANONICAL_CREDENTIALS_FILE);
    }
    home.join(".config")
        .join(CANONICAL_CREDENTIALS_DIR)
        .join(CANONICAL_CREDENTIALS_FILE)
}

fn legacy_credentials_path(home: &Path) -> PathBuf {
    home.join(LEGACY_CREDENTIALS_DIR)
        .join(LEGACY_CREDENTIALS_FILE)
}

#[cfg(test)]
fn derive_test_age_home(canonical: &Path, legacy: &Path) -> PathBuf {
    // In tests we need a dedicated "home" for the age backend so it writes
    // under `<tempdir>/.ato/credentials/auth/...`. Both paths share the
    // tempdir root (e.g. `<tempdir>/config/ato/...` and
    // `<tempdir>/home/.ato/...`), so walk up until we find a segment literally
    // named "config" or "home" and return its parent.
    for p in canonical.ancestors().chain(legacy.ancestors()) {
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if (name == "config" || name == "home") && p.parent().is_some() {
            return p.parent().unwrap().to_path_buf();
        }
    }
    // Last-resort: co-locate under the canonical file's directory so age
    // data lives alongside the test's credentials file (and never escapes to
    // a real filesystem root like `/`).
    canonical
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| legacy.to_path_buf())
}

fn sanitize_credentials(mut creds: Credentials) -> Credentials {
    creds.session_token = None;
    creds.github_token = None;
    creds
}

pub(super) fn merge_metadata(target: &mut Credentials, incoming: &Credentials) {
    target.publisher_did = incoming.publisher_did.clone();
    target.publisher_id = incoming.publisher_id.clone();
    target.publisher_handle = incoming.publisher_handle.clone();
    target.github_app_installation_id = incoming.github_app_installation_id;
    target.github_app_account_login = incoming.github_app_account_login.clone();
    target.github_username = incoming.github_username.clone();
}

fn nonempty(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn read_toml_credentials_file(path: &Path) -> Result<Option<Credentials>> {
    if !path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(path)
        .with_context(|| format!("Failed to read credentials from {:?}", path))?;
    let creds: Credentials = toml::from_str(&contents)
        .with_context(|| format!("Failed to parse credentials from {:?}", path))?;
    Ok(Some(creds))
}

fn read_legacy_json_credentials_file(path: &Path) -> Result<Option<Credentials>> {
    if !path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(path)
        .with_context(|| format!("Failed to read legacy credentials from {:?}", path))?;
    let creds: Credentials = serde_json::from_str(&contents)
        .with_context(|| format!("Failed to parse legacy credentials from {:?}", path))?;
    Ok(Some(creds))
}

#[allow(clippy::needless_return)]
fn write_secure_credentials_file(path: &Path, contents: &str) -> Result<()> {
    let parent = path
        .parent()
        .context("Credentials path must have a parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create credentials directory {:?}", parent))?;
    set_dir_permissions_if_supported(parent)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("Failed to open credentials file {:?}", path))?;
        use std::io::Write as _;
        file.write_all(contents.as_bytes())
            .with_context(|| format!("Failed to write credentials to {:?}", path))?;
        file.flush()
            .with_context(|| format!("Failed to flush credentials to {:?}", path))?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("Failed to secure credentials file {:?}", path))?;
        return Ok(());
    }

    #[cfg(not(unix))]
    {
        fs::write(path, contents)
            .with_context(|| format!("Failed to write credentials to {:?}", path))?;
        Ok(())
    }
}

fn set_dir_permissions_if_supported(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("Failed to secure credentials directory {:?}", path))?;
    }

    Ok(())
}
