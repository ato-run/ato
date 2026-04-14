//! Authentication module for Ato Store
//!
//! Manages authentication credentials for the ato CLI.
//! Stores canonical credentials in `$XDG_CONFIG_HOME/ato/credentials.toml`.

pub(crate) mod consent_store;
mod github;
mod prompt;
mod publisher;
mod storage;
mod store;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::{Mutex, OnceLock};

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
pub(super) const ENV_ATO_TOKEN: &str = "ATO_TOKEN";
pub(super) const ENV_XDG_CONFIG_HOME: &str = "XDG_CONFIG_HOME";
pub(super) const KEYRING_SERVICE_NAME: &str = "run.ato.cli";
pub(super) const KEYRING_SESSION_ACCOUNT: &str = "current_session";
pub(super) const KEYRING_GITHUB_ACCOUNT: &str = "github_token";
pub(super) const GITHUB_APP_INSTALL_TIMEOUT_SECS: u64 = 5 * 60;
pub(super) const GITHUB_APP_INSTALL_POLL_SECS: u64 = 3;
pub(super) const GITHUB_APP_INSTALL_NOTICE_INTERVAL_SECS: u64 = 12;
pub(super) const GITHUB_APP_INSTALL_TROUBLESHOOT_AFTER_SECS: u64 = 45;
pub(super) const LEGACY_CREDENTIALS_DIR: &str = ".ato";
pub(super) const LEGACY_CREDENTIALS_FILE: &str = "credentials.json";
pub(super) const CANONICAL_CREDENTIALS_DIR: &str = "ato";
pub(super) const CANONICAL_CREDENTIALS_FILE: &str = "credentials.toml";

#[cfg(test)]
pub(super) static TEST_KEYRING: OnceLock<Mutex<HashMap<(String, String), String>>> =
    OnceLock::new();

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
    pub(super) keyring_service: String,
    pub(super) keyring_session_account: String,
    pub(super) keyring_github_account: String,
}

pub(super) fn read_env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests;
