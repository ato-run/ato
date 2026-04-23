use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use super::publisher::PublisherMeResponse;
use super::shared_env_lock as env_lock;
use super::storage::{keyring_user_interaction_not_allowed_message, TokenStorageLocation};
use super::store::{hydrate_publisher_identity_with, is_local_store_api_base_url};
use super::{
    current_session_token, require_session_token, AuthManager, Credentials, ENV_ATO_TOKEN,
};

const ENV_CRED_AUTH_SESSION_TOKEN: &str = "ATO_CRED_AUTH_SESSION__SESSION_TOKEN";

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
async fn persist_session_token_interactive_falls_back_to_memory_without_identity() {
    // Phase 2: interactive logins now default to the shared age file. With
    // no identity initialized under the test's age_home, `AuthStore` falls
    // back to its in-process memory cache and returns `Memory` — neither the
    // canonical credentials file nor the legacy OS keyring should be touched.
    let _serial = env_lock().lock().unwrap();
    let _token_guard = EnvVarGuard::set(ENV_ATO_TOKEN, None);
    let _cred_guard = EnvVarGuard::set(ENV_CRED_AUTH_SESSION_TOKEN, None);
    let temp_dir = TempDir::new().unwrap();
    let (manager, canonical_path, _) = test_manager(&temp_dir);

    let storage = manager
        .persist_session_token("interactive-token".to_string(), false)
        .await
        .unwrap();

    assert_eq!(storage, TokenStorageLocation::Memory);
    assert!(!canonical_path.exists());
    assert_eq!(
        manager
            .load_keyring_token(&manager.keyring_session_account)
            .unwrap(),
        None
    );
    // Subsequent reads resolve the value from the in-process memory cache
    // that AuthStore keeps alive via its `Arc` backends.
    assert_eq!(
        manager.resolve_session_token().unwrap().as_deref(),
        Some("interactive-token")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn persist_session_token_interactive_writes_to_age_when_identity_loaded() {
    // With an age identity initialized at the test's age_home, interactive
    // logins should land in the age file and subsequent reads resolve
    // through the chain without any keyring hit.
    //
    // NOTE: `AuthManager` caches its `AuthStore` eagerly (so the in-process
    // memory backend survives across calls). The identity must therefore be
    // initialized BEFORE constructing the manager — otherwise the cached
    // store sees `age_exists = false` and downgrades to the memory backend.
    let _serial = env_lock().lock().unwrap();
    let _token_guard = EnvVarGuard::set(ENV_ATO_TOKEN, None);
    let _cred_guard = EnvVarGuard::set(ENV_CRED_AUTH_SESSION_TOKEN, None);
    let temp_dir = TempDir::new().unwrap();
    // `test_manager` places files at `<tempdir>/{config,home}/...`, which
    // `derive_test_age_home` resolves to `<tempdir>` itself.
    let age = crate::application::credential::AgeFileBackend::new(temp_dir.path().to_path_buf());
    age.init_identity(None).unwrap();

    let (manager, canonical_path, _) = test_manager(&temp_dir);

    let storage = manager
        .persist_session_token("interactive-token".to_string(), false)
        .await
        .unwrap();

    assert_eq!(storage, TokenStorageLocation::AgeFile);
    assert!(!canonical_path.exists());
    assert_eq!(
        manager
            .load_keyring_token(&manager.keyring_session_account)
            .unwrap(),
        None
    );
    assert_eq!(
        manager.resolve_session_token().unwrap().as_deref(),
        Some("interactive-token")
    );
    assert!(manager
        .age_home
        .join(".ato/credentials/auth/session.age")
        .exists());
}
