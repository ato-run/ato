use anyhow::{Context, Result};
use keyring::{Entry, Error as KeyringError};
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(test)]
use super::TEST_KEYRING;
use super::{
    read_env_non_empty, AuthManager, Credentials, CANONICAL_CREDENTIALS_DIR,
    CANONICAL_CREDENTIALS_FILE, ENV_ATO_TOKEN, ENV_XDG_CONFIG_HOME, KEYRING_GITHUB_ACCOUNT,
    KEYRING_SERVICE_NAME, KEYRING_SESSION_ACCOUNT, LEGACY_CREDENTIALS_DIR, LEGACY_CREDENTIALS_FILE,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TokenStorageLocation {
    OsKeyring,
    CanonicalFile,
}

impl AuthManager {
    /// Create a new AuthManager with default credentials path
    pub fn new() -> Result<Self> {
        let home = dirs::home_dir().context("Cannot determine home directory")?;
        let credentials_path = canonical_credentials_path(&home);
        let legacy_credentials_path = legacy_credentials_path(&home);
        Ok(Self {
            credentials_path,
            legacy_credentials_path,
            keyring_service: KEYRING_SERVICE_NAME.to_string(),
            keyring_session_account: KEYRING_SESSION_ACCOUNT.to_string(),
            keyring_github_account: KEYRING_GITHUB_ACCOUNT.to_string(),
        })
    }

    #[cfg(test)]
    pub fn with_paths(credentials_path: PathBuf, legacy_credentials_path: PathBuf) -> Self {
        let suffix =
            hex::encode(blake3::hash(credentials_path.to_string_lossy().as_bytes()).as_bytes())
                .chars()
                .take(8)
                .collect::<String>();
        Self {
            credentials_path,
            legacy_credentials_path,
            keyring_service: format!("{}.test", KEYRING_SERVICE_NAME),
            keyring_session_account: format!("{}-{}", KEYRING_SESSION_ACCOUNT, suffix),
            keyring_github_account: format!("{}-{}", KEYRING_GITHUB_ACCOUNT, suffix),
        }
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
                "Not authenticated. Run:\n  ato login\n\nNo usable token found in ATO_TOKEN, OS keyring, {:?}, or {:?}",
                self.credentials_path,
                self.legacy_credentials_path
            );
        }

        Ok(creds)
    }

    /// Delete stored credentials (logout)
    pub fn delete(&self) -> Result<()> {
        self.delete_keyring_token(&self.keyring_session_account)?;
        self.delete_keyring_token(&self.keyring_github_account)?;

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

    pub(super) fn resolve_persisted_token<F>(
        &self,
        env_key: Option<&str>,
        keyring_account: &str,
        selector: F,
    ) -> Result<Option<String>>
    where
        F: Fn(&Credentials) -> Option<&String>,
    {
        if let Some(key) = env_key {
            if let Some(token) = read_env_non_empty(key) {
                return Ok(Some(token));
            }
        }
        if let Some(token) = self.load_keyring_token(keyring_account)? {
            return Ok(Some(token));
        }
        if let Some(creds) = self.load_canonical_credentials()? {
            if let Some(token) = selector(&creds).filter(|value| !value.trim().is_empty()) {
                return Ok(Some(token.clone()));
            }
        }
        if let Some(creds) = self.load_legacy_credentials()? {
            if let Some(token) = selector(&creds).filter(|value| !value.trim().is_empty()) {
                return Ok(Some(token.clone()));
            }
        }
        Ok(None)
    }

    pub(super) fn resolve_session_token(&self) -> Result<Option<String>> {
        self.resolve_persisted_token(
            Some(ENV_ATO_TOKEN),
            &self.keyring_session_account,
            |creds| creds.session_token.as_ref(),
        )
    }

    pub(super) fn resolve_github_token(&self) -> Result<Option<String>> {
        self.resolve_persisted_token(None, &self.keyring_github_account, |creds| {
            creds.github_token.as_ref()
        })
    }

    pub(super) async fn save_keyring_token_async(
        &self,
        account: &str,
        token: String,
    ) -> Result<()> {
        #[cfg(test)]
        if self.is_test_keyring() {
            self.test_keyring_set(account, &token);
            return Ok(());
        }

        let service = self.keyring_service.clone();
        let account = account.to_string();
        tokio::task::spawn_blocking(move || {
            let entry = Entry::new(&service, &account)
                .map_err(|err| anyhow::anyhow!(format_keyring_error_message("save", &err)))?;
            entry
                .set_password(&token)
                .map_err(|err| anyhow::anyhow!(format_keyring_error_message("save", &err)))
        })
        .await
        .map_err(|err| anyhow::anyhow!("Keyring worker failed: {err}"))?
    }

    pub(super) async fn save_session_token_async(&self, token: String) -> Result<()> {
        self.save_keyring_token_async(&self.keyring_session_account, token)
            .await
    }

    pub(super) async fn save_github_token_async(&self, token: String) -> Result<()> {
        self.save_keyring_token_async(&self.keyring_github_account, token)
            .await
    }

    fn keyring_entry(&self, account: &str) -> Result<Entry> {
        Entry::new(&self.keyring_service, account)
            .with_context(|| "Failed to initialize OS keyring entry")
    }

    pub(super) fn load_keyring_token(&self, account: &str) -> Result<Option<String>> {
        #[cfg(test)]
        if self.is_test_keyring() {
            return Ok(self.test_keyring_get(account));
        }
        let entry = match self.keyring_entry(account) {
            Ok(entry) => entry,
            Err(err) if self.is_nonfatal_keyring_access_error(err.as_ref()) => return Ok(None),
            Err(err) => return Err(err),
        };
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(err) if self.is_nonfatal_keyring_error(&err) => Ok(None),
            Err(err) => Err(self.keyring_error(err, "load")),
        }
    }

    fn delete_keyring_token(&self, account: &str) -> Result<()> {
        #[cfg(test)]
        if self.is_test_keyring() {
            self.test_keyring_delete(account);
            return Ok(());
        }
        let entry = match self.keyring_entry(account) {
            Ok(entry) => entry,
            Err(err) if self.is_nonfatal_keyring_access_error(err.as_ref()) => return Ok(()),
            Err(err) => return Err(err),
        };
        match entry.delete_password() {
            Ok(_) | Err(KeyringError::NoEntry) => Ok(()),
            Err(err) if self.is_nonfatal_keyring_error(&err) => Ok(()),
            Err(err) => Err(self.keyring_error(err, "delete")),
        }
    }

    fn keyring_error(&self, err: KeyringError, action: &str) -> anyhow::Error {
        anyhow::anyhow!(format_keyring_error_message(action, &err))
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
        Ok(self
            .load_keyring_token(&self.keyring_session_account)?
            .is_some()
            || self
                .load_keyring_token(&self.keyring_github_account)?
                .is_some())
    }

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

        self.save_session_token_async(token).await?;
        Ok(TokenStorageLocation::OsKeyring)
    }

    #[cfg(test)]
    pub(super) fn is_test_keyring(&self) -> bool {
        self.keyring_service.ends_with(".test")
    }

    #[cfg(test)]
    pub(super) fn test_keyring_get(&self, account: &str) -> Option<String> {
        TEST_KEYRING
            .get_or_init(Default::default)
            .lock()
            .expect("test keyring lock")
            .get(&(self.keyring_service.clone(), account.to_string()))
            .cloned()
    }

    #[cfg(test)]
    pub(super) fn test_keyring_set(&self, account: &str, value: &str) {
        TEST_KEYRING
            .get_or_init(Default::default)
            .lock()
            .expect("test keyring lock")
            .insert(
                (self.keyring_service.clone(), account.to_string()),
                value.to_string(),
            );
    }

    #[cfg(test)]
    pub(super) fn test_keyring_delete(&self, account: &str) {
        TEST_KEYRING
            .get_or_init(Default::default)
            .lock()
            .expect("test keyring lock")
            .remove(&(self.keyring_service.clone(), account.to_string()));
    }

    fn is_nonfatal_keyring_error(&self, err: &KeyringError) -> bool {
        matches!(
            err,
            KeyringError::PlatformFailure(_) | KeyringError::NoStorageAccess(_)
        )
    }

    fn is_nonfatal_keyring_access_error(&self, err: &(dyn std::error::Error + 'static)) -> bool {
        err.downcast_ref::<KeyringError>()
            .map(|inner| self.is_nonfatal_keyring_error(inner))
            .unwrap_or(false)
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

pub(super) fn keyring_user_interaction_not_allowed_message(message: &str) -> bool {
    message
        .to_ascii_lowercase()
        .contains("user interaction is not allowed")
}

fn format_keyring_error_message(action: &str, err: &KeyringError) -> String {
    let err_text = err.to_string();
    let mut message = format!("Failed to {} token in OS keyring ({}).", action, err_text);

    if keyring_user_interaction_not_allowed_message(&err_text) {
        message.push_str(
            " macOS denied Keychain access. This usually means the login keychain is locked or this shell cannot show Keychain prompts (for example: ssh, tmux, sudo, launchd, or a backgrounded GUI session). Unlock the login keychain or allow your terminal app in Keychain Access, then retry.",
        );
    }

    message.push_str(" Use ATO_TOKEN or `ato login --headless` for this environment.");
    message
}
