//! Authentication module for Ato Store
//!
//! Manages authentication credentials for the ato CLI.
//! Stores canonical credentials in `$XDG_CONFIG_HOME/ato/credentials.toml`.

pub(crate) mod consent_store;
mod credential_store;
mod github;
mod prompt;
mod publisher;
mod storage;
mod store;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

use credential_store::AuthStore;

#[allow(unused_imports)]
pub(crate) use github::login_with_token;
#[allow(unused_imports)]
pub(crate) use store::{
    current_publisher_handle, current_session_token, default_store_registry_url,
    login_with_store_device_flow, logout, require_session_token, share_display_base_url, status,
};

pub(super) const DEFAULT_STORE_API_URL: &str = "https://api.ato.run";
pub(super) const DEFAULT_STORE_SITE_URL: &str = "https://ato.run";
pub(super) const ENV_STORE_API_URL: &str = "ATO_STORE_API_URL";
pub(super) const ENV_STORE_SITE_URL: &str = "ATO_STORE_SITE_URL";
/// Legacy env var for a Store session token. `EnvBackend` reads this as an
/// alias for `ATO_CRED_AUTH_SESSION__SESSION_TOKEN`; kept as a symbolic
/// constant so tests can refer to it without duplicating the string literal.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) const ENV_ATO_TOKEN: &str = "ATO_TOKEN";
pub(super) const ENV_XDG_CONFIG_HOME: &str = "XDG_CONFIG_HOME";
pub(super) const GITHUB_APP_INSTALL_TIMEOUT_SECS: u64 = 5 * 60;
pub(super) const GITHUB_APP_INSTALL_POLL_SECS: u64 = 3;
pub(super) const GITHUB_APP_INSTALL_NOTICE_INTERVAL_SECS: u64 = 12;
pub(super) const GITHUB_APP_INSTALL_TROUBLESHOOT_AFTER_SECS: u64 = 45;
pub(super) const LEGACY_CREDENTIALS_DIR: &str = ".ato";
pub(super) const LEGACY_CREDENTIALS_FILE: &str = "credentials.json";
pub(super) const CANONICAL_CREDENTIALS_DIR: &str = "ato";
pub(super) const CANONICAL_CREDENTIALS_FILE: &str = "credentials.toml";

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
    pub(super) credentials_path: PathBuf,
    pub(super) legacy_credentials_path: PathBuf,
    /// Home directory used by the shared credential layer
    /// (`AuthStore`/`AgeFileBackend`). Defaults to the user's real home in
    /// production; overridable in tests so each temp dir gets its own age
    /// identity and credential layout.
    pub(super) age_home: PathBuf,
    /// Eagerly-constructed `AuthStore`. Cached so the in-process
    /// `MemoryBackend` survives across `resolve_*` / `persist_*` calls — every
    /// call going through a fresh `AuthStore` would otherwise discard its own
    /// memory cache the moment the handle is dropped.
    pub(super) auth_store: Arc<AuthStore>,
}

pub(super) fn read_env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
pub(super) fn shared_env_lock() -> &'static std::sync::Mutex<()> {
    crate::application::credential::test_env_lock()
}

#[cfg(test)]
mod tests;
