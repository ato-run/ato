//! Authentication module for Ato Store
//!
//! Manages authentication credentials for the ato CLI.
//! Stores canonical credentials in `$XDG_CONFIG_HOME/ato/credentials.toml`.

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use ed25519_dalek::Signer;
use keyring::{Entry, Error as KeyringError};
use rand::rngs::OsRng;
use rand::RngCore;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::{Mutex, OnceLock};

const DEFAULT_STORE_API_URL: &str = "https://api.ato.run";
const DEFAULT_STORE_SITE_URL: &str = "https://ato.run";
const ENV_STORE_API_URL: &str = "ATO_STORE_API_URL";
const ENV_STORE_SITE_URL: &str = "ATO_STORE_SITE_URL";
const ENV_ATO_TOKEN: &str = "ATO_TOKEN";
const ENV_XDG_CONFIG_HOME: &str = "XDG_CONFIG_HOME";
const KEYRING_SERVICE_NAME: &str = "run.ato.cli";
const KEYRING_SESSION_ACCOUNT: &str = "current_session";
const KEYRING_GITHUB_ACCOUNT: &str = "github_token";
const GITHUB_APP_INSTALL_TIMEOUT_SECS: u64 = 5 * 60;
const GITHUB_APP_INSTALL_POLL_SECS: u64 = 3;
const GITHUB_APP_INSTALL_NOTICE_INTERVAL_SECS: u64 = 12;
const GITHUB_APP_INSTALL_TROUBLESHOOT_AFTER_SECS: u64 = 45;
const LEGACY_CREDENTIALS_DIR: &str = ".ato";
const LEGACY_CREDENTIALS_FILE: &str = "credentials.json";
const CANONICAL_CREDENTIALS_DIR: &str = "ato";
const CANONICAL_CREDENTIALS_FILE: &str = "credentials.toml";

#[cfg(test)]
static TEST_KEYRING: OnceLock<Mutex<HashMap<(String, String), String>>> = OnceLock::new();

/// User credentials stored locally
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Credentials {
    /// GitHub Personal Access Token (legacy fallback)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_token: Option<String>,

    /// Store session token (Device Flow)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_token: Option<String>,

    /// Publisher DID (set after first successful registration)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_did: Option<String>,

    /// Publisher ID (Store)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_id: Option<String>,

    /// Publisher handle (Store)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_handle: Option<String>,

    /// Linked GitHub App installation ID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_app_installation_id: Option<u64>,

    /// Linked GitHub App installation account login
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_app_account_login: Option<String>,

    /// GitHub username (cached from API)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_username: Option<String>,
}

/// Manages authentication credentials
pub struct AuthManager {
    credentials_path: PathBuf,
    legacy_credentials_path: PathBuf,
    keyring_service: String,
    keyring_session_account: String,
    keyring_github_account: String,
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

    fn resolve_persisted_token<F>(
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

    fn resolve_session_token(&self) -> Result<Option<String>> {
        self.resolve_persisted_token(
            Some(ENV_ATO_TOKEN),
            &self.keyring_session_account,
            |creds| creds.session_token.as_ref(),
        )
    }

    fn resolve_github_token(&self) -> Result<Option<String>> {
        self.resolve_persisted_token(None, &self.keyring_github_account, |creds| {
            creds.github_token.as_ref()
        })
    }

    async fn save_keyring_token_async(&self, account: &str, token: String) -> Result<()> {
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

    async fn save_session_token_async(&self, token: String) -> Result<()> {
        self.save_keyring_token_async(&self.keyring_session_account, token)
            .await
    }

    async fn save_github_token_async(&self, token: String) -> Result<()> {
        self.save_keyring_token_async(&self.keyring_github_account, token)
            .await
    }

    fn keyring_entry(&self, account: &str) -> Result<Entry> {
        Entry::new(&self.keyring_service, account)
            .with_context(|| "Failed to initialize OS keyring entry")
    }

    fn load_keyring_token(&self, account: &str) -> Result<Option<String>> {
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

    fn write_canonical_credentials(&self, creds: &Credentials) -> Result<()> {
        let contents = toml::to_string_pretty(creds).context("Failed to serialize credentials")?;
        write_secure_credentials_file(&self.credentials_path, &contents)
    }

    fn load_canonical_credentials(&self) -> Result<Option<Credentials>> {
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

    fn has_persisted_local_state(&self) -> Result<bool> {
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

    async fn persist_session_token(
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
    fn is_test_keyring(&self) -> bool {
        self.keyring_service.ends_with(".test")
    }

    #[cfg(test)]
    fn test_keyring_get(&self, account: &str) -> Option<String> {
        TEST_KEYRING
            .get_or_init(Default::default)
            .lock()
            .expect("test keyring lock")
            .get(&(self.keyring_service.clone(), account.to_string()))
            .cloned()
    }

    #[cfg(test)]
    fn test_keyring_set(&self, account: &str, value: &str) {
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
    fn test_keyring_delete(&self, account: &str) {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TokenStorageLocation {
    OsKeyring,
    CanonicalFile,
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

fn merge_metadata(target: &mut Credentials, incoming: &Credentials) {
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

/// GitHub user information
#[derive(Debug, Deserialize)]
struct GitHubUser {
    login: String,
}

#[derive(Debug, Deserialize)]
struct BridgeInitResponse {
    session_id: String,
    user_code: String,
    expires_in: u64,
    #[serde(default)]
    poll_interval_sec: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct BridgePollResponse {
    code: String,
    #[serde(default)]
    poll_interval_sec: Option<u64>,
    #[serde(default)]
    auth_code: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BridgeExchangeResponse {
    access_token: String,
    #[serde(default)]
    handle: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RetryAfterResponse {
    retry_after: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct StoreSessionUser {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StoreSessionResponse {
    #[serde(default)]
    user: Option<StoreSessionUser>,
}

#[derive(Debug, Deserialize)]
struct StoreErrorResponse {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PublisherMeResponse {
    id: String,
    handle: String,
    author_did: String,
}

#[derive(Debug, Deserialize)]
struct PublisherRegisterResponse {
    id: String,
    handle: String,
    author_did: String,
}

#[derive(Debug, Deserialize)]
struct GitHubInstallationsResponse {
    installations: Vec<GitHubAppInstallation>,
}

#[derive(Debug, Deserialize, Clone)]
struct GitHubAppInstallation {
    installation_id: u64,
    account_login: String,
    status: String,
}

#[derive(Debug, Deserialize)]
struct GitHubAppInstallUrlResponse {
    install_url: String,
    #[serde(default)]
    callback_url: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct GitHubAppCallbackResponse {
    installation_id: u64,
    account_login: String,
}

fn read_env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn current_session_token() -> Option<String> {
    let auth = AuthManager::new().ok()?;
    auth.resolve_session_token().ok().flatten()
}

pub fn require_session_token() -> Result<String> {
    let auth = AuthManager::new()?;
    let Some(token) = auth.resolve_session_token()? else {
        anyhow::bail!(
            "Authentication required. Run `ato login` again, or set `ATO_TOKEN` for this shell."
        );
    };
    Ok(token)
}

pub fn current_publisher_handle() -> Result<Option<String>> {
    let manager = AuthManager::new()?;
    Ok(
        hydrate_publisher_identity_with(&manager, fetch_publisher_me_blocking)?
            .and_then(|creds| cached_publisher_handle(&creds)),
    )
}

pub fn default_store_registry_url() -> String {
    store_api_base_url()
}

fn trim_trailing_slash(value: &str) -> String {
    value.trim_end_matches('/').to_string()
}

fn to_base64_url(bytes: &[u8]) -> String {
    BASE64_STANDARD
        .encode(bytes)
        .replace('+', "-")
        .replace('/', "_")
        .trim_end_matches('=')
        .to_string()
}

fn generate_pkce_verifier() -> String {
    let mut bytes = [0u8; 64];
    OsRng.fill_bytes(&mut bytes);
    to_base64_url(&bytes)
}

fn generate_pkce_challenge_s256(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    to_base64_url(&hasher.finalize())
}

fn store_api_base_url() -> String {
    trim_trailing_slash(
        &read_env_non_empty(ENV_STORE_API_URL).unwrap_or_else(|| DEFAULT_STORE_API_URL.to_string()),
    )
}

fn store_site_base_url() -> String {
    trim_trailing_slash(
        &read_env_non_empty(ENV_STORE_SITE_URL)
            .unwrap_or_else(|| DEFAULT_STORE_SITE_URL.to_string()),
    )
}

fn is_local_store_api_base_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn keyring_user_interaction_not_allowed_message(message: &str) -> bool {
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

fn try_open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .context("Failed to launch browser with `open`")?;
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .context("Failed to launch browser with `xdg-open`")?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .context("Failed to launch browser with `start`")?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Ok(())
}

fn fetch_store_session_user(session_token: &str) -> Result<Option<StoreSessionUser>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .context("Failed to create HTTP client")?;

    let response = client
        .get(format!("{}/api/auth/session", store_api_base_url()))
        .header("Accept", "application/json")
        .header(
            "Cookie",
            format!(
                "better-auth.session_token={}; __Secure-better-auth.session_token={}",
                session_token, session_token
            ),
        )
        .send()
        .context("Failed to fetch Store session")?;

    if response.status() == StatusCode::UNAUTHORIZED || response.status() == StatusCode::FORBIDDEN {
        return Ok(None);
    }

    if !response.status().is_success() {
        anyhow::bail!("Store session lookup failed (HTTP {})", response.status());
    }

    let body = response
        .json::<StoreSessionResponse>()
        .context("Failed to parse Store session response")?;

    Ok(body.user)
}

fn store_session_cookie_header(session_token: &str) -> String {
    format!(
        "better-auth.session_token={}; __Secure-better-auth.session_token={}",
        session_token, session_token
    )
}

fn cached_publisher_handle(creds: &Credentials) -> Option<String> {
    creds.publisher_handle.as_ref().and_then(|handle| {
        let trimmed = handle.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn merge_publisher_identity(creds: &mut Credentials, me: &PublisherMeResponse) {
    if !me.id.trim().is_empty() {
        creds.publisher_id = Some(me.id.clone());
    }
    if !me.handle.trim().is_empty() {
        creds.publisher_handle = Some(me.handle.clone());
    }
    if !me.author_did.trim().is_empty() {
        creds.publisher_did = Some(me.author_did.clone());
    }
}

fn hydrate_publisher_identity_with<F>(
    manager: &AuthManager,
    fetcher: F,
) -> Result<Option<Credentials>>
where
    F: FnOnce(&str) -> Result<Option<PublisherMeResponse>>,
{
    let mut creds = manager.load()?.unwrap_or_default();
    if cached_publisher_handle(&creds).is_some() {
        return Ok(Some(creds));
    }

    let Some(session_token) = manager.resolve_session_token()? else {
        return Ok(None);
    };

    let Some(me) = fetcher(&session_token)? else {
        return Ok(None);
    };

    merge_publisher_identity(&mut creds, &me);
    manager.save(&creds)?;
    Ok(Some(creds))
}

fn fetch_publisher_me_blocking(session_token: &str) -> Result<Option<PublisherMeResponse>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .context("Failed to create HTTP client")?;

    let response = client
        .get(format!("{}/v1/publishers/me", store_api_base_url()))
        .header("Accept", "application/json")
        .header("Cookie", store_session_cookie_header(session_token))
        .send()
        .context("Failed to fetch publisher profile")?;

    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if response.status() == StatusCode::UNAUTHORIZED || response.status() == StatusCode::FORBIDDEN {
        anyhow::bail!("Store session is not authorized for publisher lookup");
    }
    if !response.status().is_success() {
        anyhow::bail!("Publisher lookup failed (HTTP {})", response.status());
    }

    let body = response
        .json::<PublisherMeResponse>()
        .context("Failed to parse publisher profile response")?;

    Ok(Some(body))
}

fn parse_store_error_text(body: &str) -> String {
    if let Ok(parsed) = serde_json::from_str::<StoreErrorResponse>(body) {
        match (parsed.error, parsed.message) {
            (Some(error), Some(message)) if !message.is_empty() => {
                return format!("{}: {}", error, message);
            }
            (Some(error), _) => return error,
            (_, Some(message)) if !message.is_empty() => return message,
            _ => {}
        }
    }
    body.trim().to_string()
}

fn normalize_handle_candidate(input: &str) -> String {
    let lowered = input.trim().to_ascii_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut prev_dash = false;
    for ch in lowered.chars() {
        let ok = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        if ok {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let mut normalized = out.trim_matches('-').to_string();
    if normalized.len() < 3 {
        normalized.push_str("-pub");
    }
    if normalized.len() > 63 {
        normalized.truncate(63);
        normalized = normalized.trim_end_matches('-').to_string();
    }
    if normalized.len() < 3 {
        normalized = "ato-publisher".to_string();
    }
    normalized
}

fn is_valid_handle(value: &str) -> bool {
    if value.len() < 3 || value.len() > 63 {
        return false;
    }
    let bytes = value.as_bytes();
    if bytes.first() == Some(&b'-') || bytes.last() == Some(&b'-') {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn prompt_line(prompt: &str) -> Result<String> {
    print!("{}", prompt);
    io::stdout().flush().context("Failed to flush stdout")?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("Failed to read from stdin")?;
    Ok(input.trim().to_string())
}

fn prompt_yes_no(prompt: &str, default_yes: bool) -> Result<bool> {
    let suffix = if default_yes { "[Y/n]" } else { "[y/N]" };
    let answer = prompt_line(&format!("{} {}: ", prompt, suffix))?;
    if answer.is_empty() {
        return Ok(default_yes);
    }
    let normalized = answer.to_ascii_lowercase();
    if ["y", "yes"].contains(&normalized.as_str()) {
        return Ok(true);
    }
    if ["n", "no"].contains(&normalized.as_str()) {
        return Ok(false);
    }
    Ok(default_yes)
}

fn publisher_signing_key_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Cannot determine home directory")?;
    Ok(home
        .join(".ato")
        .join("keys")
        .join("publisher-signing-key.json"))
}

fn ensure_publisher_signing_key() -> Result<capsule_core::types::signing::StoredKey> {
    let key_path = publisher_signing_key_path()?;
    if key_path.exists() {
        return capsule_core::types::signing::StoredKey::read(&key_path)
            .with_context(|| format!("Failed to read {}", key_path.display()));
    }

    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let generated = capsule_core::types::signing::StoredKey::generate();
    generated
        .write(&key_path)
        .with_context(|| format!("Failed to write {}", key_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&key_path)?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&key_path, perms)?;
    }

    Ok(generated)
}

#[derive(Debug, Clone)]
struct PublisherOnboardingInfo {
    publisher_id: String,
    publisher_handle: String,
    publisher_did: String,
    installation: Option<GitHubAppInstallation>,
}

async fn fetch_publisher_me(
    client: &reqwest::Client,
    api_base: &str,
    session_token: &str,
) -> Result<Option<PublisherMeResponse>> {
    let response = client
        .get(format!("{}/v1/publishers/me", api_base))
        .header("Accept", "application/json")
        .header("Cookie", store_session_cookie_header(session_token))
        .send()
        .await
        .context("Failed to fetch publisher profile")?;

    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if response.status() == StatusCode::UNAUTHORIZED || response.status() == StatusCode::FORBIDDEN {
        anyhow::bail!("Store session is not authorized for publisher lookup");
    }
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!(
            "Publisher lookup failed ({}): {}",
            status,
            parse_store_error_text(&body)
        );
    }

    let payload = response
        .json::<PublisherMeResponse>()
        .await
        .context("Failed to parse publisher profile response")?;
    Ok(Some(payload))
}

async fn register_publisher_with_prompt(
    client: &reqwest::Client,
    api_base: &str,
    session_token: &str,
    github_username: Option<&str>,
) -> Result<PublisherRegisterResponse> {
    let signing_key = ensure_publisher_signing_key()?;
    let did = signing_key.did()?;

    let default_handle = normalize_handle_candidate(
        github_username
            .filter(|v| !v.trim().is_empty())
            .unwrap_or("ato-publisher"),
    );

    println!();
    println!("🧩 Publisher setup is required for publishing.");

    for _ in 0..5 {
        let entered = prompt_line(&format!("👤 Publisher handle [{}]: ", default_handle))?;
        let handle = if entered.is_empty() {
            default_handle.clone()
        } else {
            normalize_handle_candidate(&entered)
        };

        if !is_valid_handle(&handle) {
            eprintln!("⚠️  Invalid handle. Use 3-63 chars, lowercase letters/digits/hyphen.");
            continue;
        }

        let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let signature = signing_key
            .to_signing_key()?
            .sign(timestamp.as_bytes())
            .to_bytes();
        let signature_b64 = BASE64_STANDARD.encode(signature);

        let payload = serde_json::json!({
            "handle": handle,
            "author_did": did,
            "did_proof": {
                "did": did,
                "timestamp": timestamp,
                "signature": signature_b64,
            }
        });

        let response = client
            .post(format!("{}/v1/publishers/register", api_base))
            .header("Accept", "application/json")
            .header("Cookie", store_session_cookie_header(session_token))
            .json(&payload)
            .send()
            .await
            .context("Failed to register publisher")?;

        if response.status().is_success() {
            return response
                .json::<PublisherRegisterResponse>()
                .await
                .context("Failed to parse publisher register response");
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let err_text = parse_store_error_text(&body);

        if status == StatusCode::CONFLICT && err_text.contains("handle_taken") {
            eprintln!("⚠️  Handle is already taken. Choose another one.");
            continue;
        }
        if status == StatusCode::CONFLICT && err_text.contains("already_registered") {
            if let Some(me) = fetch_publisher_me(client, api_base, session_token).await? {
                return Ok(PublisherRegisterResponse {
                    id: me.id,
                    handle: me.handle,
                    author_did: me.author_did,
                });
            }
        }

        anyhow::bail!("Publisher registration failed ({}): {}", status, err_text);
    }

    anyhow::bail!("Publisher setup aborted: failed to select a valid/available handle")
}

async fn fetch_github_app_installations(
    client: &reqwest::Client,
    api_base: &str,
    session_token: &str,
) -> Result<Vec<GitHubAppInstallation>> {
    let response = client
        .get(format!("{}/v1/sources/github/app/installations", api_base))
        .header("Accept", "application/json")
        .header("Cookie", store_session_cookie_header(session_token))
        .send()
        .await
        .context("Failed to fetch GitHub App installations")?;

    if response.status() == StatusCode::UNAUTHORIZED || response.status() == StatusCode::FORBIDDEN {
        anyhow::bail!("GitHub App installation lookup is unauthorized for current session");
    }
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!(
            "Failed to fetch GitHub App installations ({}): {}",
            status,
            parse_store_error_text(&body)
        );
    }

    let payload = response
        .json::<GitHubInstallationsResponse>()
        .await
        .context("Failed to parse GitHub App installations response")?;
    Ok(payload.installations)
}

fn choose_active_installation(
    installations: &[GitHubAppInstallation],
) -> Option<GitHubAppInstallation> {
    installations
        .iter()
        .find(|i| i.status.eq_ignore_ascii_case("active"))
        .cloned()
        .or_else(|| installations.first().cloned())
}

async fn fetch_github_app_install_url(
    client: &reqwest::Client,
    api_base: &str,
    session_token: &str,
) -> Result<GitHubAppInstallUrlResponse> {
    let response = client
        .get(format!("{}/v1/sources/github/app/install-url", api_base))
        .header("Accept", "application/json")
        .header("Cookie", store_session_cookie_header(session_token))
        .send()
        .await
        .context("Failed to request GitHub App install URL")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!(
            "Failed to request GitHub App install URL ({}): {}",
            status,
            parse_store_error_text(&body)
        );
    }

    let payload = response
        .json::<GitHubAppInstallUrlResponse>()
        .await
        .context("Failed to parse GitHub App install URL response")?;
    Ok(payload)
}

async fn link_github_app_installation_manually(
    client: &reqwest::Client,
    api_base: &str,
    session_token: &str,
    installation_id: u64,
    state: Option<&str>,
) -> Result<GitHubAppInstallation> {
    let mut request = client
        .get(format!("{}/v1/sources/github/app/callback", api_base))
        .header("Accept", "application/json")
        .header("Cookie", store_session_cookie_header(session_token))
        .query(&[
            ("installation_id", installation_id.to_string()),
            ("setup_action", "install".to_string()),
        ]);
    if let Some(non_empty_state) = state.filter(|value| !value.trim().is_empty()) {
        request = request.query(&[("state", non_empty_state.to_string())]);
    }

    let response = request
        .send()
        .await
        .context("Failed to call GitHub App callback endpoint")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!(
            "Manual callback failed ({}): {}",
            status,
            parse_store_error_text(&body)
        );
    }

    let payload = response
        .json::<GitHubAppCallbackResponse>()
        .await
        .context("Failed to parse GitHub App callback response")?;

    let installations = fetch_github_app_installations(client, api_base, session_token).await?;
    if let Some(found) = installations
        .into_iter()
        .find(|item| item.installation_id == payload.installation_id)
    {
        return Ok(found);
    }

    Ok(GitHubAppInstallation {
        installation_id: payload.installation_id,
        account_login: payload.account_login,
        status: "active".to_string(),
    })
}

async fn ensure_github_app_installation_with_tui(
    client: &reqwest::Client,
    api_base: &str,
    session_token: &str,
) -> Result<GitHubAppInstallation> {
    let existing = fetch_github_app_installations(client, api_base, session_token).await?;
    if let Some(active) = choose_active_installation(&existing) {
        return Ok(active);
    }

    let install = fetch_github_app_install_url(client, api_base, session_token).await?;
    println!();
    println!("🔌 GitHub App installation is required.");
    println!("   URL: {}", install.install_url);
    if let Some(callback_url) = install.callback_url.as_deref() {
        println!("   Callback: {}", callback_url);
    }
    if let Some(expires_in) = install.expires_in {
        println!("   Link expires in: {}s", expires_in);
    }
    if let Some(state) = install.state.as_deref() {
        println!("   State: {}", state);
    }

    if let Err(error) = try_open_browser(&install.install_url) {
        eprintln!("⚠️  Could not open browser automatically: {}", error);
        eprintln!("   Open the URL manually to continue.");
    }

    if !prompt_yes_no("GitHub App install page opened. Start linking now?", true)? {
        anyhow::bail!("GitHub App installation was cancelled");
    }

    println!("⏳ Waiting for GitHub App installation to be linked...");
    let started = Instant::now();
    let mut last_notice = Instant::now();
    let mut troubleshooting_printed = false;
    loop {
        if started.elapsed() >= Duration::from_secs(GITHUB_APP_INSTALL_TIMEOUT_SECS) {
            let mut hint = String::from(
                "Timed out waiting for GitHub App installation to be linked.\n\
                 Re-check that installation completed in GitHub and run `ato login` again.",
            );
            if let Some(callback_url) = install.callback_url.as_deref() {
                hint.push_str(&format!("\nExpected callback endpoint: {}", callback_url));
            }
            println!();
            println!("⚠️  {}", hint);
            println!(
                "   You can link manually by entering installation_id (from GitHub installation URL)."
            );
            let manual_input =
                prompt_line("   installation_id (blank to cancel and retry later): ")?;
            if manual_input.trim().is_empty() {
                anyhow::bail!(
                    "Timed out waiting for GitHub App installation. Complete linking and run `ato login` again."
                );
            }
            let installation_id = manual_input
                .trim()
                .parse::<u64>()
                .with_context(|| format!("Invalid installation_id: {}", manual_input.trim()))?;
            let linked = link_github_app_installation_manually(
                client,
                api_base,
                session_token,
                installation_id,
                install.state.as_deref(),
            )
            .await?;
            println!(
                "   ✔ Linked installation {} ({})",
                linked.installation_id, linked.account_login
            );
            return Ok(linked);
        }

        let installations = fetch_github_app_installations(client, api_base, session_token).await?;
        if let Some(active) = choose_active_installation(&installations) {
            return Ok(active);
        }

        let elapsed = started.elapsed().as_secs();
        if !troubleshooting_printed && elapsed >= GITHUB_APP_INSTALL_TROUBLESHOOT_AFTER_SECS {
            println!(
                "   • still waiting ({}s). If you already installed, callback may not have reached Store.",
                elapsed
            );
            println!("   • Ensure the final GitHub install step completed for the target account.");
            println!(
                "   • If this repeats, verify GitHub App setup URL points to /v1/sources/github/app/callback."
            );
            troubleshooting_printed = true;
            last_notice = Instant::now();
        } else if last_notice.elapsed()
            >= Duration::from_secs(GITHUB_APP_INSTALL_NOTICE_INTERVAL_SECS)
        {
            println!("   • waiting for installation link... ({}s)", elapsed);
            last_notice = Instant::now();
        }

        tokio::time::sleep(Duration::from_secs(GITHUB_APP_INSTALL_POLL_SECS)).await;
    }
}

async fn run_publisher_onboarding_flow(
    session_token: &str,
    github_username: Option<&str>,
) -> Result<PublisherOnboardingInfo> {
    let api_base = store_api_base_url();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("Failed to create HTTP client")?;

    let publisher =
        if let Some(existing) = fetch_publisher_me(&client, &api_base, session_token).await? {
            existing
        } else {
            let created =
                register_publisher_with_prompt(&client, &api_base, session_token, github_username)
                    .await?;
            PublisherMeResponse {
                id: created.id,
                handle: created.handle,
                author_did: created.author_did,
            }
        };

    let installation =
        Some(ensure_github_app_installation_with_tui(&client, &api_base, session_token).await?);

    Ok(PublisherOnboardingInfo {
        publisher_id: publisher.id,
        publisher_handle: publisher.handle,
        publisher_did: publisher.author_did,
        installation,
    })
}

/// Verify a GitHub token by calling the GitHub API
async fn verify_github_token(token: &str) -> Result<String> {
    let client = reqwest::Client::new();

    let response = client
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {}", token))
        .header("User-Agent", "ato-cli")
        .send()
        .await
        .context("Failed to connect to GitHub API")?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().await.unwrap_or_default();

        anyhow::bail!("Invalid GitHub token (HTTP {}): {}", status, error_text);
    }

    let user: GitHubUser = response
        .json()
        .await
        .context("Failed to parse GitHub user response")?;

    Ok(user.login)
}

/// Login with a GitHub Personal Access Token
pub async fn login_with_token(token: String) -> Result<()> {
    println!("🔐 Verifying GitHub token...");

    let username = verify_github_token(&token).await?;

    let manager = AuthManager::new()?;
    manager.save_github_token_async(token).await?;
    let mut creds = manager.load()?.unwrap_or_default();
    creds.github_username = Some(username.clone());
    manager.save(&creds)?;

    println!("✅ Authenticated as @{}", username);
    println!("   GitHub token storage: OS keyring");
    if manager.credentials_path().exists() {
        println!("   Metadata file: {:?}", manager.credentials_path());
    }

    Ok(())
}

/// Login with Store Device Flow
#[allow(clippy::needless_return)]
pub async fn login_with_store_device_flow(headless: bool) -> Result<()> {
    let api_base = store_api_base_url();
    let site_base = store_site_base_url();
    let client = reqwest::Client::new();
    let code_verifier = generate_pkce_verifier();
    let code_challenge = generate_pkce_challenge_s256(&code_verifier);

    let start_response = client
        .post(format!("{}/v1/auth/bridge/init", api_base))
        .json(&serde_json::json!({
            "code_challenge": code_challenge,
            "method": "S256",
            "device_info": format!("ato-cli/{}", env!("CARGO_PKG_VERSION")),
        }))
        .send()
        .await
        .with_context(|| "Failed to start Store bridge authentication")?;

    if !start_response.status().is_success() {
        let status = start_response.status();
        let body = start_response.text().await.unwrap_or_default();
        let mut message = format!("Bridge auth init failed ({}): {}", status, body);
        if status.is_server_error() && is_local_store_api_base_url(&api_base) {
            message.push_str(
                "\nLocal ato-store may be missing DB migrations. Run `pnpm -C apps/ato-store db:migrate` and restart `pnpm -C apps/ato-store dev`.",
            );
        }
        anyhow::bail!(message);
    }

    let start: BridgeInitResponse = start_response
        .json()
        .await
        .context("Invalid bridge auth init response")?;

    let session_id = start.session_id.clone();
    let activate_url = format!(
        "{}/v1/auth/bridge/activate?session_id={}",
        api_base, session_id
    );

    let login_url = format!(
        "{}/auth?next={}",
        site_base,
        urlencoding::encode(&activate_url)
    );

    if headless {
        println!("🧩 Headless login mode");
        println!("   Open this URL on another authenticated browser session:");
        println!("   {}", login_url);
        println!("🔑 Verification code: {}", start.user_code);
        println!("⏳ Waiting for remote approval...");
    } else {
        println!("🌐 Opening browser for Ato sign-in...");
        println!("   URL: {}", login_url);
        println!("🔑 Verification code: {}", start.user_code);

        if let Err(error) = try_open_browser(&login_url) {
            eprintln!("⚠️  Could not open browser automatically: {}", error);
            eprintln!("   Open the URL manually to continue sign-in.");
        }

        println!("⏳ Waiting for browser authentication...");
    }

    let poll_timeout_secs = start.expires_in.min(300);
    let mut poll_interval_secs = start.poll_interval_sec.unwrap_or(2).max(1);
    let started_at = Instant::now();

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                let _ = client
                    .post(format!("{}/v1/auth/bridge/cancel", api_base))
                    .json(&serde_json::json!({
                        "session_id": &session_id,
                        "reason": "cli_interrupted",
                    }))
                    .send()
                    .await;
                anyhow::bail!("Authentication cancelled by user (Ctrl+C)");
            }
            _ = tokio::time::sleep(Duration::from_secs(poll_interval_secs)) => {}
        }

        if started_at.elapsed() >= Duration::from_secs(poll_timeout_secs) {
            anyhow::bail!(
                "Authentication timed out after {} seconds. Run `ato login` again.",
                poll_timeout_secs
            );
        }

        let poll_response = client
            .post(format!("{}/v1/auth/bridge/poll", api_base))
            .json(&serde_json::json!({
                "session_id": &session_id,
                "code_verifier": &code_verifier,
            }))
            .send()
            .await
            .with_context(|| "Failed to poll bridge authentication state")?;

        if poll_response.status() == StatusCode::TOO_MANY_REQUESTS {
            let body =
                poll_response
                    .json::<RetryAfterResponse>()
                    .await
                    .unwrap_or(RetryAfterResponse {
                        retry_after: Some(poll_interval_secs),
                    });
            let retry_after = body.retry_after.unwrap_or(poll_interval_secs).max(1);
            tokio::time::sleep(Duration::from_secs(retry_after)).await;
            continue;
        }

        if poll_response.status() == StatusCode::CONFLICT {
            anyhow::bail!("Authentication denied or cancelled. Run `ato login` again.");
        }

        if poll_response.status() == StatusCode::GONE {
            anyhow::bail!("Authentication expired. Run `ato login` again.");
        }

        if poll_response.status() == StatusCode::BAD_REQUEST {
            let body = poll_response.text().await.unwrap_or_default();
            anyhow::bail!("Authentication failed: {}", body);
        }

        if !poll_response.status().is_success() {
            let status = poll_response.status();
            let body = poll_response.text().await.unwrap_or_default();
            anyhow::bail!("Bridge auth poll failed ({}): {}", status, body);
        }

        let poll: BridgePollResponse = poll_response
            .json()
            .await
            .context("Invalid bridge auth poll response")?;

        match poll.code.as_str() {
            "PENDING" => {
                poll_interval_secs = poll.poll_interval_sec.unwrap_or(poll_interval_secs).max(1);
            }
            "SUCCESS" => {
                let auth_code = poll
                    .auth_code
                    .context("Bridge auth approved but no auth code was returned")?;

                let exchange_response = client
                    .post(format!("{}/v1/auth/bridge/exchange", api_base))
                    .json(&serde_json::json!({
                        "session_id": &session_id,
                        "auth_code": auth_code,
                        "code_verifier": &code_verifier,
                    }))
                    .send()
                    .await
                    .context("Failed to exchange bridge auth code")?;

                if !exchange_response.status().is_success() {
                    let status = exchange_response.status();
                    let body = exchange_response.text().await.unwrap_or_default();
                    anyhow::bail!("Bridge auth exchange failed ({}): {}", status, body);
                }

                let exchange: BridgeExchangeResponse = exchange_response
                    .json()
                    .await
                    .context("Invalid bridge auth exchange response")?;

                let session_token = exchange.access_token;

                let manager = AuthManager::new()?;
                let storage = manager
                    .persist_session_token(session_token.clone(), headless)
                    .await?;
                let mut creds = manager.load()?.unwrap_or_default();
                creds.publisher_handle = exchange.handle.clone();
                if headless {
                    let mut persisted = manager.load_canonical_credentials()?.unwrap_or_default();
                    persisted.session_token = Some(session_token.clone());
                    merge_metadata(&mut persisted, &creds);
                    manager.write_canonical_credentials(&persisted)?;
                }

                let session_token_for_setup = session_token.clone();
                println!("🧪 Running publisher onboarding...");
                let onboarding = run_publisher_onboarding_flow(
                    &session_token_for_setup,
                    creds.github_username.as_deref(),
                )
                .await?;
                creds.publisher_id = Some(onboarding.publisher_id);
                creds.publisher_handle = Some(onboarding.publisher_handle);
                creds.publisher_did = Some(onboarding.publisher_did);
                if let Some(installation) = onboarding.installation {
                    creds.github_app_installation_id = Some(installation.installation_id);
                    creds.github_app_account_login = Some(installation.account_login);
                }
                if headless {
                    let mut persisted = manager.load_canonical_credentials()?.unwrap_or_default();
                    persisted.session_token = Some(session_token.clone());
                    merge_metadata(&mut persisted, &creds);
                    manager.write_canonical_credentials(&persisted)?;
                }

                println!("✅ Login completed successfully");
                if let Some(handle) = creds.publisher_handle.as_deref() {
                    println!("   Publisher: {}", handle);
                }
                if let Some(id) = creds.github_app_installation_id {
                    println!("   GitHub App Installation: {}", id);
                }
                match storage {
                    TokenStorageLocation::OsKeyring => {
                        println!("   Store session saved to: OS keyring");
                    }
                    TokenStorageLocation::CanonicalFile => {
                        println!(
                            "   Store session saved to: {:?}",
                            manager.credentials_path()
                        );
                    }
                }
                if headless {
                    println!("   Metadata file: {:?}", manager.credentials_path());
                }
                return Ok(());
            }
            other => {
                anyhow::bail!("Unexpected authentication status: {}", other);
            }
        }
    }
}

/// Logout (delete stored credentials)
#[allow(clippy::needless_return)]
pub fn logout() -> Result<()> {
    let manager = AuthManager::new()?;

    if !manager.has_persisted_local_state()? {
        println!("ℹ️  Not currently logged in");
        return Ok(());
    }

    manager.delete()?;
    println!("✅ Logged out successfully");
    println!(
        "   Removed session tokens from: OS keyring and {:?}",
        manager.credentials_path()
    );
    if manager.legacy_credentials_path().exists() {
        println!(
            "   Legacy metadata file was left untouched: {:?}",
            manager.legacy_credentials_path()
        );
    }

    Ok(())
}

/// Show current authentication status
pub fn status() -> Result<()> {
    let manager = AuthManager::new()?;

    match manager.require() {
        Ok(creds) => {
            println!("✅ Authenticated");
            if let Some(session_token) = &creds.session_token {
                println!("   Store session: configured");
                match fetch_store_session_user(session_token) {
                    Ok(Some(user)) => {
                        println!("   User ID: {}", user.id);
                        if let Some(name) = user.name {
                            println!("   Name: {}", name);
                        }
                        if let Some(email) = user.email {
                            println!("   Email: {}", email);
                        }
                    }
                    Ok(None) => {
                        println!("   User: session expired or unavailable");
                    }
                    Err(err) => {
                        println!("   User: failed to fetch ({})", err);
                    }
                }
            }
            if creds.github_token.is_some() {
                println!("   GitHub token: configured");
            }
            if let Some(username) = &creds.github_username {
                println!("   GitHub: @{}", username);
            }
            if let Some(did) = &creds.publisher_did {
                println!("   Publisher DID: {}", did);
            }
            if let Some(handle) = &creds.publisher_handle {
                println!("   Publisher Handle: {}", handle);
            }
            if let Some(id) = creds.github_app_installation_id {
                println!("   GitHub App Installation ID: {}", id);
            }
            if let Some(login) = &creds.github_app_account_login {
                println!("   GitHub App Account: {}", login);
            }
            if manager
                .load_keyring_token(&manager.keyring_session_account)?
                .is_some()
            {
                println!("   Session storage: OS keyring");
            }
            if manager.credentials_path().exists() {
                println!("   Credential file: {:?}", manager.credentials_path());
            } else if manager.legacy_credentials_path().exists() {
                println!(
                    "   Legacy credential file: {:?}",
                    manager.legacy_credentials_path()
                );
            }
        }
        Err(_) => {
            println!("❌ Not authenticated");
            println!("   Run: ato login");
            println!();
            println!("   Headless/CI/agent fallback:");
            println!("   Set ATO_TOKEN or run `ato login --headless`");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let previous = std::env::var(key).ok();
            match value {
                Some(next) => std::env::set_var(key, next),
                None => std::env::remove_var(key),
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn test_manager(temp_dir: &TempDir) -> (AuthManager, PathBuf, PathBuf) {
        let canonical = temp_dir
            .path()
            .join("config")
            .join("ato")
            .join("credentials.toml");
        let legacy = temp_dir
            .path()
            .join("home")
            .join(".ato")
            .join("credentials.json");
        (
            AuthManager::with_paths(canonical.clone(), legacy.clone()),
            canonical,
            legacy,
        )
    }

    #[test]
    fn test_credentials_roundtrip_uses_canonical_toml() {
        let temp_dir = TempDir::new().unwrap();
        let (manager, creds_path, _) = test_manager(&temp_dir);

        let original = Credentials {
            github_token: Some("ghp_test123".to_string()),
            session_token: Some("sess_test_123".to_string()),
            publisher_did: Some("did:key:z6Mk...".to_string()),
            publisher_id: Some("01testpublisherid".to_string()),
            publisher_handle: Some("testuser".to_string()),
            github_app_installation_id: Some(12345),
            github_app_account_login: Some("koh0920".to_string()),
            github_username: Some("testuser".to_string()),
        };

        manager.save(&original).unwrap();
        let raw = fs::read_to_string(&creds_path).unwrap();
        assert!(raw.contains("publisher_did = \"did:key:z6Mk...\""));
        assert!(!raw.contains("sess_test_123"));
        let loaded = manager.load().unwrap().unwrap();

        assert_eq!(loaded.github_token, None);
        assert_eq!(loaded.session_token, None);
        assert_eq!(original.publisher_did, loaded.publisher_did);
        assert_eq!(original.publisher_id, loaded.publisher_id);
        assert_eq!(original.publisher_handle, loaded.publisher_handle);
        assert_eq!(
            original.github_app_installation_id,
            loaded.github_app_installation_id
        );
        assert_eq!(
            original.github_app_account_login,
            loaded.github_app_account_login
        );
        assert_eq!(original.github_username, loaded.github_username);
    }

    #[test]
    fn test_legacy_credentials_json_compatibility() {
        let _guard = env_lock().lock().unwrap();
        let _token_guard = EnvVarGuard::set(ENV_ATO_TOKEN, None);
        let temp_dir = TempDir::new().unwrap();
        let (manager, _, legacy_path) = test_manager(&temp_dir);

        fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        fs::write(
            &legacy_path,
            r#"{
  "github_token": "ghp_legacy123",
  "session_token": "legacy-session-token",
  "publisher_did": "did:key:z6MkLegacy",
  "github_username": "legacy-user"
}"#,
        )
        .unwrap();

        let loaded = manager.load().unwrap().unwrap();

        assert_eq!(loaded.github_token, None);
        assert_eq!(loaded.session_token, None);
        assert_eq!(loaded.publisher_did.as_deref(), Some("did:key:z6MkLegacy"));
        assert_eq!(loaded.publisher_id, None);
        assert_eq!(loaded.publisher_handle, None);
        assert_eq!(loaded.github_app_installation_id, None);
        assert_eq!(loaded.github_app_account_login, None);
        assert_eq!(loaded.github_username.as_deref(), Some("legacy-user"));
        assert_eq!(
            manager.resolve_session_token().unwrap().as_deref(),
            Some("legacy-session-token")
        );
    }

    #[test]
    fn test_require_fails_when_not_authenticated() {
        let _guard = env_lock().lock().unwrap();
        let _token_guard = EnvVarGuard::set(ENV_ATO_TOKEN, None);
        let temp_dir = TempDir::new().unwrap();
        let (manager, _, _) = test_manager(&temp_dir);
        let result = manager.require();

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Not authenticated"));
    }

    #[test]
    fn test_require_fails_when_no_tokens() {
        let _guard = env_lock().lock().unwrap();
        let _token_guard = EnvVarGuard::set(ENV_ATO_TOKEN, None);
        let temp_dir = TempDir::new().unwrap();
        let (manager, _, _) = test_manager(&temp_dir);
        manager
            .save(&Credentials {
                github_token: None,
                session_token: None,
                publisher_did: Some("did:key:z6Mk...".to_string()),
                publisher_id: None,
                publisher_handle: None,
                github_app_installation_id: None,
                github_app_account_login: None,
                github_username: Some("testuser".to_string()),
            })
            .unwrap();

        let result = manager.require();
        assert!(result.is_err());
    }

    #[test]
    fn test_delete_credentials_keeps_legacy_file() {
        let temp_dir = TempDir::new().unwrap();
        let (manager, creds_path, legacy_path) = test_manager(&temp_dir);

        let creds = Credentials {
            github_token: Some("ghp_test123".to_string()),
            session_token: Some("sess_test_123".to_string()),
            publisher_did: None,
            publisher_id: None,
            publisher_handle: None,
            github_app_installation_id: None,
            github_app_account_login: None,
            github_username: Some("testuser".to_string()),
        };

        manager.write_canonical_credentials(&creds).unwrap();
        fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        fs::write(&legacy_path, r#"{"publisher_handle":"legacy-user"}"#).unwrap();
        manager.test_keyring_set(&manager.keyring_session_account, "keyring-token");
        assert!(creds_path.exists());
        assert!(legacy_path.exists());

        manager.delete().unwrap();
        assert!(!creds_path.exists());
        assert!(legacy_path.exists());
        assert_eq!(
            manager
                .load_keyring_token(&manager.keyring_session_account)
                .unwrap(),
            None
        );
    }

    #[test]
    fn hydrate_publisher_identity_uses_cached_handle_without_fetch() {
        let temp_dir = TempDir::new().unwrap();
        let (manager, _, _) = test_manager(&temp_dir);
        manager
            .save(&Credentials {
                github_token: None,
                session_token: None,
                publisher_did: Some("did:key:z6MkCached".to_string()),
                publisher_id: Some("publisher-cached".to_string()),
                publisher_handle: Some("cached-handle".to_string()),
                github_app_installation_id: None,
                github_app_account_login: None,
                github_username: None,
            })
            .unwrap();

        let hydrated = hydrate_publisher_identity_with(&manager, |_| {
            anyhow::bail!("fetcher should not be called when handle is cached")
        })
        .unwrap()
        .expect("cached credentials");

        assert_eq!(hydrated.publisher_handle.as_deref(), Some("cached-handle"));
        assert_eq!(hydrated.publisher_id.as_deref(), Some("publisher-cached"));
    }

    #[test]
    fn hydrate_publisher_identity_fetches_and_persists_missing_handle() {
        let _guard = env_lock().lock().unwrap();
        let temp_dir = TempDir::new().unwrap();
        let (manager, _, _) = test_manager(&temp_dir);
        manager
            .save(&Credentials {
                github_token: None,
                session_token: None,
                publisher_did: None,
                publisher_id: None,
                publisher_handle: None,
                github_app_installation_id: None,
                github_app_account_login: None,
                github_username: Some("dock-user".to_string()),
            })
            .unwrap();
        let _token_guard = EnvVarGuard::set(ENV_ATO_TOKEN, Some("session-token-123"));

        let hydrated = hydrate_publisher_identity_with(&manager, |token| {
            assert_eq!(token, "session-token-123");
            Ok(Some(PublisherMeResponse {
                id: "publisher-123".to_string(),
                handle: "dock-user".to_string(),
                author_did: "did:key:z6MkDockUser".to_string(),
            }))
        })
        .unwrap()
        .expect("hydrated credentials");

        assert_eq!(hydrated.publisher_handle.as_deref(), Some("dock-user"));
        assert_eq!(hydrated.publisher_id.as_deref(), Some("publisher-123"));
        assert_eq!(
            hydrated.publisher_did.as_deref(),
            Some("did:key:z6MkDockUser")
        );

        let persisted = manager.load().unwrap().unwrap();
        assert_eq!(persisted.publisher_handle.as_deref(), Some("dock-user"));
        assert_eq!(persisted.publisher_id.as_deref(), Some("publisher-123"));
        assert_eq!(
            persisted.publisher_did.as_deref(),
            Some("did:key:z6MkDockUser")
        );
    }

    #[test]
    fn current_session_token_reads_env_override() {
        let _guard = env_lock().lock().unwrap();
        let _token_guard = EnvVarGuard::set(ENV_ATO_TOKEN, Some("session-token-123"));
        assert_eq!(
            current_session_token().as_deref(),
            Some("session-token-123")
        );
    }

    #[test]
    fn require_session_token_reads_env_override() {
        let _guard = env_lock().lock().unwrap();
        let _token_guard = EnvVarGuard::set(ENV_ATO_TOKEN, Some("session-token-123"));
        assert_eq!(
            require_session_token().expect("session token"),
            "session-token-123"
        );
    }

    #[test]
    fn is_local_store_api_base_url_detects_loopback_hosts() {
        assert!(is_local_store_api_base_url("http://localhost:8787"));
        assert!(is_local_store_api_base_url("http://127.0.0.1:8787"));
        assert!(!is_local_store_api_base_url("https://api.ato.run"));
    }

    #[test]
    fn keyring_user_interaction_not_allowed_message_detects_macos_error() {
        assert!(keyring_user_interaction_not_allowed_message(
            "Platform secure storage failure: User interaction is not allowed."
        ));
        assert!(!keyring_user_interaction_not_allowed_message(
            "Platform secure storage failure: Item not found."
        ));
    }

    #[test]
    fn save_preserves_existing_canonical_tokens() {
        let temp_dir = TempDir::new().unwrap();
        let (manager, _, _) = test_manager(&temp_dir);
        manager
            .write_canonical_credentials(&Credentials {
                session_token: Some("file-session".to_string()),
                github_token: Some("file-github".to_string()),
                publisher_handle: Some("before".to_string()),
                ..Credentials::default()
            })
            .unwrap();

        manager
            .save(&Credentials {
                publisher_handle: Some("after".to_string()),
                ..Credentials::default()
            })
            .unwrap();

        let persisted = manager.load_canonical_credentials().unwrap().unwrap();
        assert_eq!(persisted.session_token.as_deref(), Some("file-session"));
        assert_eq!(persisted.github_token.as_deref(), Some("file-github"));
        assert_eq!(persisted.publisher_handle.as_deref(), Some("after"));
    }

    #[test]
    fn save_does_not_migrate_legacy_tokens_into_canonical_file() {
        let temp_dir = TempDir::new().unwrap();
        let (manager, canonical_path, legacy_path) = test_manager(&temp_dir);
        fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        fs::write(
            &legacy_path,
            r#"{"session_token":"legacy-session","publisher_handle":"legacy-user"}"#,
        )
        .unwrap();

        manager
            .save(&Credentials {
                publisher_handle: Some("new-user".to_string()),
                ..Credentials::default()
            })
            .unwrap();

        assert!(canonical_path.exists());
        let persisted = manager.load_canonical_credentials().unwrap().unwrap();
        assert_eq!(persisted.session_token, None);
        assert_eq!(persisted.publisher_handle.as_deref(), Some("new-user"));
    }

    #[test]
    fn canonical_file_wins_over_legacy_for_session_resolution() {
        let _guard = env_lock().lock().unwrap();
        let _token_guard = EnvVarGuard::set(ENV_ATO_TOKEN, None);
        let temp_dir = TempDir::new().unwrap();
        let (manager, _, legacy_path) = test_manager(&temp_dir);

        fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        fs::write(&legacy_path, r#"{"session_token":"legacy-token"}"#).unwrap();
        assert_eq!(
            manager.resolve_session_token().unwrap().as_deref(),
            Some("legacy-token")
        );

        manager
            .write_canonical_credentials(&Credentials {
                session_token: Some("canonical-token".to_string()),
                ..Credentials::default()
            })
            .unwrap();
        assert_eq!(
            manager.resolve_session_token().unwrap().as_deref(),
            Some("canonical-token")
        );
    }

    #[test]
    fn require_uses_canonical_file_token_when_keyring_is_unavailable() {
        let _guard = env_lock().lock().unwrap();
        let _token_guard = EnvVarGuard::set(ENV_ATO_TOKEN, None);
        let temp_dir = TempDir::new().unwrap();
        let (manager, _, _) = test_manager(&temp_dir);
        manager
            .write_canonical_credentials(&Credentials {
                session_token: Some("file-session".to_string()),
                publisher_handle: Some("dock-user".to_string()),
                ..Credentials::default()
            })
            .unwrap();

        let creds = manager.require().unwrap();
        assert_eq!(creds.session_token.as_deref(), Some("file-session"));
        assert_eq!(creds.publisher_handle.as_deref(), Some("dock-user"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn persist_session_token_headless_uses_canonical_file_with_0600() {
        let temp_dir = TempDir::new().unwrap();
        let (manager, canonical_path, _) = test_manager(&temp_dir);

        let storage = manager
            .persist_session_token("headless-token".to_string(), true)
            .await
            .unwrap();

        assert_eq!(storage, TokenStorageLocation::CanonicalFile);
        assert!(canonical_path.exists());
        let persisted = manager.load_canonical_credentials().unwrap().unwrap();
        assert_eq!(persisted.session_token.as_deref(), Some("headless-token"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = fs::metadata(&canonical_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn persist_session_token_interactive_uses_keyring_without_creating_file() {
        let temp_dir = TempDir::new().unwrap();
        let (manager, canonical_path, _) = test_manager(&temp_dir);

        let storage = manager
            .persist_session_token("interactive-token".to_string(), false)
            .await
            .unwrap();

        assert_eq!(storage, TokenStorageLocation::OsKeyring);
        assert!(!canonical_path.exists());
        assert_eq!(
            manager
                .load_keyring_token(&manager.keyring_session_account)
                .unwrap()
                .as_deref(),
            Some("interactive-token")
        );
    }
}
