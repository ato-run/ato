//! Device-authorization login and the local Ato credential store.
//!
//! The ato API exposes a browser-approved PKCE bridge at
//! /v1/auth/bridge/{init,poll,exchange}. This module owns that protocol so
//! the ato CLI and the object-graph upload tool share one implementation
//! instead of carrying two copies of it.
//!
//! Credentials are stored per API origin in $ATO_HOME/credentials.json
//! (~/.ato/credentials.json by default) and written atomically with
//! owner-only permissions. The token is never printed or logged.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The default Ato API origin that `ato login` authenticates against.
pub const DEFAULT_API_URL: &str = "https://api.ato.run";

const CREDENTIALS_FILE_NAME: &str = "credentials.json";
/// The API accepts at most 200 polls per bridge session.
const MAX_POLL_ATTEMPTS: u64 = 200;

/// What the approval page needs from the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceLoginPrompt {
    /// URL to open in a browser to approve this login.
    pub activate_url: String,
    /// Short code the approval page asks the user to confirm.
    pub user_code: String,
}

/// A credential minted by the device-authorization bridge.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceCredential {
    pub access_token: String,
    pub handle: Option<String>,
    pub user_id: String,
    pub registry_url: String,
    /// Unix seconds since the epoch.
    pub expires_at: u64,
}

impl std::fmt::Debug for DeviceCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeviceCredential")
            .field("access_token", &"<redacted>")
            .field("handle", &self.handle)
            .field("user_id", &self.user_id)
            .field("registry_url", &self.registry_url)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Deserialize)]
struct DeviceInit {
    session_id: String,
    user_code: String,
    poll_interval_sec: u64,
}

#[derive(Deserialize)]
struct DevicePoll {
    code: String,
    auth_code: Option<String>,
    poll_interval_sec: Option<u64>,
}

#[derive(Deserialize)]
struct DeviceExchange {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    handle: Option<String>,
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    registry_url: Option<String>,
}

/// Run the device-authorization flow against `api_url`.
///
/// `on_prompt` is called once, after the bridge session exists and before
/// polling starts, so the caller can show the URL and code (and maybe open a
/// browser). The returned credential is a Bearer token; it is never printed
/// here.
pub fn device_login(
    api_url: &str,
    device_info: &str,
    on_prompt: impl FnOnce(&DeviceLoginPrompt),
) -> Result<DeviceCredential> {
    let parsed = reqwest::Url::parse(api_url).context("invalid device-login API URL")?;
    ensure!(
        parsed.scheme() == "https" || parsed.host_str() == Some("localhost"),
        "device login requires HTTPS (except localhost)"
    );
    ensure!(
        parsed.query().is_none() && parsed.fragment().is_none(),
        "device-login API URL cannot contain a query or fragment"
    );
    let base_url = api_url.trim_end_matches('/');
    let mut entropy = [0_u8; 32];
    getrandom::fill(&mut entropy)
        .map_err(|error| anyhow::anyhow!("generate PKCE verifier entropy: {error}"))?;
    let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(entropy);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("construct device-login HTTP client")?;
    let init_response = client
        .post(format!("{base_url}/v1/auth/bridge/init"))
        .json(&serde_json::json!({
            "code_challenge": challenge,
            "method": "S256",
            "device_info": device_info
        }))
        .send()
        .context("initialize device login")?;
    ensure!(
        init_response.status().is_success(),
        "device login init failed with HTTP {}",
        init_response.status()
    );
    let init: DeviceInit = init_response
        .json()
        .context("decode device login init response")?;
    on_prompt(&DeviceLoginPrompt {
        activate_url: format!(
            "{base_url}/v1/auth/bridge/activate?session_id={}",
            init.session_id
        ),
        user_code: init.user_code.clone(),
    });

    let auth_code = (0..MAX_POLL_ATTEMPTS)
        .find_map(|_| {
            let response = client
                .post(format!("{base_url}/v1/auth/bridge/poll"))
                .json(&serde_json::json!({
                    "session_id": init.session_id,
                    "code_verifier": verifier
                }))
                .send()
                .ok()?;
            let status = response.status();
            let poll: DevicePoll = response.json().ok()?;
            if status.is_success() && poll.code == "SUCCESS" {
                return poll.auth_code;
            }
            std::thread::sleep(Duration::from_secs(
                poll.poll_interval_sec
                    .unwrap_or(init.poll_interval_sec)
                    .max(1),
            ));
            None
        })
        .context("device login was not approved before the polling limit")?;
    let exchange_response = client
        .post(format!("{base_url}/v1/auth/bridge/exchange"))
        .json(&serde_json::json!({
            "session_id": init.session_id,
            "auth_code": auth_code,
            "code_verifier": verifier
        }))
        .send()
        .context("exchange approved device login")?;
    ensure!(
        exchange_response.status().is_success(),
        "device login exchange failed with HTTP {}",
        exchange_response.status()
    );
    let exchange: DeviceExchange = exchange_response
        .json()
        .context("decode device login exchange response")?;
    ensure!(
        exchange.access_token.starts_with("ato_dev_"),
        "device login did not return a device credential"
    );
    let expires_in = exchange.expires_in.unwrap_or(90 * 24 * 60 * 60);
    Ok(DeviceCredential {
        access_token: exchange.access_token,
        handle: exchange.handle,
        user_id: exchange.user_id,
        registry_url: exchange
            .registry_url
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| base_url.to_owned()),
        expires_at: unix_now() + expires_in,
    })
}

/// The serialized form kept on disk for one API origin.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCredential {
    pub token: String,
    pub handle: Option<String>,
    pub user_id: String,
    pub registry_url: String,
    /// Unix seconds since the epoch.
    pub expires_at: u64,
}

impl std::fmt::Debug for StoredCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoredCredential")
            .field("token", &"<redacted>")
            .field("handle", &self.handle)
            .field("user_id", &self.user_id)
            .field("registry_url", &self.registry_url)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl From<DeviceCredential> for StoredCredential {
    fn from(value: DeviceCredential) -> Self {
        Self {
            token: value.access_token,
            handle: value.handle,
            user_id: value.user_id,
            registry_url: value.registry_url,
            expires_at: value.expires_at,
        }
    }
}

/// The whole credentials file: one credential per API origin.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialStore {
    #[serde(default)]
    pub credentials: BTreeMap<String, StoredCredential>,
}

/// Canonical origin used as the credential key, e.g. https://api.ato.run.
///
/// Trailing slashes, paths, and host case do not create separate entries.
pub fn api_origin(api_url: &str) -> Result<String> {
    let parsed = reqwest::Url::parse(api_url).context("invalid Ato API URL")?;
    Ok(parsed.origin().ascii_serialization())
}

/// The credentials path under an Ato home directory.
pub fn credentials_path(ato_home: &Path) -> PathBuf {
    ato_home.join(CREDENTIALS_FILE_NAME)
}

/// Resolve the credentials path for this host.
pub fn default_credentials_path() -> Result<PathBuf> {
    Ok(credentials_path(&crate::ato_home()?))
}

/// Load the credential stored for `api_url`'s origin, if any.
pub fn load_credential(api_url: &str) -> Result<Option<StoredCredential>> {
    load_credential_at(&default_credentials_path()?, api_url)
}

/// Path-explicit form of [`load_credential`] (used by tests and tools).
pub fn load_credential_at(path: &Path, api_url: &str) -> Result<Option<StoredCredential>> {
    let origin = api_origin(api_url)?;
    let store = read_store(path)?;
    Ok(store.credentials.get(&origin).cloned())
}

/// Store `credential` under `api_url`'s origin, replacing only that origin.
///
/// Returns the path written. The file is written atomically with mode 0600.
pub fn save_credential(api_url: &str, credential: &StoredCredential) -> Result<PathBuf> {
    let path = default_credentials_path()?;
    save_credential_at(&path, api_url, credential)?;
    Ok(path)
}

/// Path-explicit form of [`save_credential`] (used by tests and tools).
pub fn save_credential_at(path: &Path, api_url: &str, credential: &StoredCredential) -> Result<()> {
    let origin = api_origin(api_url)?;
    let mut store = read_store(path)?;
    store.credentials.insert(origin, credential.clone());
    write_store(path, &store)
}

/// Resolve the Bearer token for `api_url`.
///
/// An explicit token (the --auth-token flag or ATO_API_TOKEN) wins; it is
/// returned without touching the credentials file. Otherwise the credential
/// stored for the same API origin is used.
pub fn resolve_token(explicit: Option<String>, api_url: &str) -> Result<String> {
    resolve_token_at(&default_credentials_path()?, explicit, api_url)
}

fn resolve_token_at(path: &Path, explicit: Option<String>, api_url: &str) -> Result<String> {
    if let Some(token) = explicit {
        return Ok(token);
    }
    if let Some(credential) = load_credential_at(path, api_url)? {
        return Ok(credential.token);
    }
    bail!(
        "no Ato credential for {}; run `ato login --api-url {} first`",
        api_origin(api_url)?,
        api_url.trim_end_matches('/')
    )
}

fn read_store(path: &Path) -> Result<CredentialStore> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("read Ato credentials {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(CredentialStore::default())
        }
        Err(error) => {
            Err(error).with_context(|| format!("read Ato credentials {}", path.display()))
        }
    }
}

fn write_store(path: &Path, store: &CredentialStore) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("create Ato credentials directory {}", parent.display()))?;
    let bytes = serde_json::to_vec_pretty(store).context("encode Ato credentials")?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(CREDENTIALS_FILE_NAME);
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.new",
        std::process::id(),
        unix_now_nanos()
    ));
    let result = write_private_file(&temporary, &bytes).and_then(|()| {
        fs::rename(&temporary, path)
            .with_context(|| format!("replace Ato credentials {}", path.display()))
    });
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("create Ato credentials {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write Ato credentials {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("flush Ato credentials {}", path.display()))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn unix_now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credential(token: &str, handle: Option<&str>) -> StoredCredential {
        StoredCredential {
            token: token.to_owned(),
            handle: handle.map(ToOwned::to_owned),
            user_id: "user_1".to_owned(),
            registry_url: "https://api.ato.run".to_owned(),
            expires_at: 1_800_000_000,
        }
    }

    #[test]
    fn debug_output_never_contains_the_token() {
        let stored = credential("ato_dev_secret", Some("koh"));
        assert!(!format!("{stored:?}").contains("ato_dev_secret"));
        let device = DeviceCredential {
            access_token: "ato_dev_secret".to_owned(),
            handle: Some("koh".to_owned()),
            user_id: "user_1".to_owned(),
            registry_url: "https://api.ato.run".to_owned(),
            expires_at: 1_800_000_000,
        };
        assert!(!format!("{device:?}").contains("ato_dev_secret"));
    }

    #[test]
    fn credentials_round_trip_through_the_store() {
        let directory = tempfile::tempdir().unwrap();
        let path = credentials_path(directory.path());
        let saved = credential("ato_dev_one", Some("koh"));
        save_credential_at(&path, "https://api.ato.run", &saved).unwrap();
        assert_eq!(
            load_credential_at(&path, "https://api.ato.run").unwrap(),
            Some(saved)
        );
        assert_eq!(
            load_credential_at(&path, "https://staging.api.ato.run").unwrap(),
            None
        );
    }

    #[test]
    fn credentials_file_uses_the_documented_shape() {
        let directory = tempfile::tempdir().unwrap();
        let path = credentials_path(directory.path());
        save_credential_at(
            &path,
            "https://api.ato.run",
            &credential("ato_dev_one", Some("koh")),
        )
        .unwrap();
        let raw: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let entry = &raw["credentials"]["https://api.ato.run"];
        assert_eq!(entry["token"], "ato_dev_one");
        assert_eq!(entry["handle"], "koh");
        assert_eq!(entry["user_id"], "user_1");
        assert_eq!(entry["registry_url"], "https://api.ato.run");
        assert_eq!(entry["expires_at"], 1_800_000_000);
    }

    #[test]
    fn saving_an_origin_replaces_only_that_origin() {
        let directory = tempfile::tempdir().unwrap();
        let path = credentials_path(directory.path());
        save_credential_at(
            &path,
            "https://api.ato.run",
            &credential("ato_dev_one", Some("koh")),
        )
        .unwrap();
        save_credential_at(
            &path,
            "https://staging.api.ato.run",
            &credential("ato_dev_staging", None),
        )
        .unwrap();
        save_credential_at(
            &path,
            "https://api.ato.run/",
            &credential("ato_dev_two", Some("koh")),
        )
        .unwrap();

        assert_eq!(
            load_credential_at(&path, "https://api.ato.run")
                .unwrap()
                .unwrap()
                .token,
            "ato_dev_two"
        );
        assert_eq!(
            load_credential_at(&path, "https://staging.api.ato.run")
                .unwrap()
                .unwrap()
                .token,
            "ato_dev_staging"
        );
        let raw: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["credentials"].as_object().unwrap().len(), 2);
    }

    #[test]
    fn origins_ignore_trailing_slashes_paths_and_host_case() {
        assert_eq!(
            api_origin("https://API.ato.run/").unwrap(),
            "https://api.ato.run"
        );
        assert_eq!(
            api_origin("https://api.ato.run/v1/auth").unwrap(),
            "https://api.ato.run"
        );
        assert_eq!(
            api_origin("http://localhost:8787/").unwrap(),
            "http://localhost:8787"
        );
    }

    #[test]
    fn a_missing_credentials_file_reads_as_empty() {
        let directory = tempfile::tempdir().unwrap();
        let path = credentials_path(directory.path());
        assert!(
            load_credential_at(&path, "https://api.ato.run")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_malformed_credentials_file_is_never_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = credentials_path(directory.path());
        fs::write(&path, b"{\"credentials\":").unwrap();
        let error = save_credential_at(
            &path,
            "https://api.ato.run",
            &credential("ato_dev_one", Some("koh")),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("read Ato credentials"),
            "{error:#}"
        );
        assert_eq!(fs::read(&path).unwrap(), b"{\"credentials\":");
    }

    #[cfg(unix)]
    #[test]
    fn the_credentials_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = credentials_path(directory.path());
        save_credential_at(
            &path,
            "https://api.ato.run",
            &credential("ato_dev_one", Some("koh")),
        )
        .unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode is {:o}", mode & 0o777);
    }

    #[test]
    fn resolve_token_prefers_an_explicit_token_without_reading_the_store() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("unreadable.json");
        fs::write(&path, b"not json").unwrap();
        assert_eq!(
            resolve_token_at(&path, Some("explicit".to_owned()), "https://api.ato.run").unwrap(),
            "explicit"
        );
    }

    #[test]
    fn resolve_token_falls_back_to_the_stored_credential() {
        let directory = tempfile::tempdir().unwrap();
        let path = credentials_path(directory.path());
        save_credential_at(
            &path,
            "https://staging.api.ato.run",
            &credential("ato_dev_stored", Some("koh")),
        )
        .unwrap();
        assert_eq!(
            resolve_token_at(&path, None, "https://staging.api.ato.run/").unwrap(),
            "ato_dev_stored"
        );
    }

    #[test]
    fn resolve_token_without_a_credential_points_at_ato_login() {
        let directory = tempfile::tempdir().unwrap();
        let path = credentials_path(directory.path());
        save_credential_at(
            &path,
            "https://staging.api.ato.run",
            &credential("ato_dev_stored", Some("koh")),
        )
        .unwrap();
        let error = resolve_token_at(&path, None, "https://api.ato.run").unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("run `ato login --api-url https://api.ato.run first`"),
            "{message}"
        );
        assert!(!message.contains("ato_dev_stored"), "{message}");
    }
}
