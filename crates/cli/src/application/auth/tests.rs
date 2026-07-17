use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use super::publisher::PublisherMeResponse;
use super::shared_env_lock as env_lock;
use super::storage::TokenStorageLocation;
use super::store::{
    EXTERNAL_BROWSER_LOGIN_HARDENING_EVIDENCE, EXTERNAL_BROWSER_LOGIN_HARDENING_LANDED,
    browser_launch_failed_event, compute_poll_timing, desktop_login_gate_messages,
    hydrate_publisher_identity_with, is_local_store_api_base_url,
    login_with_store_device_flow_desktop, sanitize_bridge_failure,
};
use super::{
    AuthManager, Credentials, ENV_ATO_TOKEN, current_session_token, require_session_token,
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
            Some(next) => unsafe { std::env::set_var(key, next) },
            None => unsafe { std::env::remove_var(key) },
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => unsafe { std::env::set_var(self.key, value) },
            None => unsafe { std::env::remove_var(self.key) },
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
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Not authenticated")
    );
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
    assert!(creds_path.exists());
    assert!(legacy_path.exists());

    manager.delete().unwrap();
    assert!(!creds_path.exists());
    assert!(legacy_path.exists());
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
fn require_uses_canonical_file_token_when_no_age_identity() {
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
#[allow(clippy::await_holding_lock)]
async fn persist_session_token_interactive_falls_back_to_memory_without_identity() {
    // Phase 2: interactive logins now default to the shared age file. With
    // no identity initialized under the test's age_home, `AuthStore` falls
    // back to its in-process memory cache and returns `Memory`. The canonical
    // credentials file must not be touched.
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
    // Subsequent reads resolve the value from the in-process memory cache
    // that AuthStore keeps alive via its `Arc` backends.
    assert_eq!(
        manager.resolve_session_token().unwrap().as_deref(),
        Some("interactive-token")
    );
}

#[tokio::test(flavor = "current_thread")]
#[allow(clippy::await_holding_lock)]
async fn persist_session_token_interactive_writes_to_age_when_identity_loaded() {
    // With an age identity initialized at the test's age_home, interactive
    // logins should land in the age file and subsequent reads resolve
    // through the chain.
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
        manager.resolve_session_token().unwrap().as_deref(),
        Some("interactive-token")
    );
    assert!(
        manager
            .age_home
            .join("credentials/auth/session.age")
            .exists()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn desktop_device_flow_fails_closed_pending_bridge_hardening() {
    // ato#1077's explicit ordering constraint: the external-browser Desktop
    // login path must not proceed until ato-api#275 (Phase 1b auth_bridge
    // hardening) has actually landed and been deployed. The gate must trip
    // before any network/filesystem work — this test asserts that with no
    // env setup, no age identity, and no reachable Store API, the call still
    // fails fast with the expected reason (rather than hanging on a network
    // call or silently proceeding).
    let result = login_with_store_device_flow_desktop().await;
    let err = result.expect_err(
        "desktop external-browser login must fail closed while the bridge-hardening gate is shut",
    );
    let message = err.to_string();
    assert!(
        message.contains("ato-api#275"),
        "error should name the blocking dependency (ato-api#275), got: {message}"
    );
}

#[test]
fn desktop_login_gate_user_message_has_no_internal_jargon() {
    // Round-2 review finding (Blocker): the fail-closed gate's message was
    // being forwarded verbatim into the Dock's user-facing toast, exposing a
    // multi-sentence internal paragraph naming GitHub issue numbers to real
    // end users with no next step. The developer-facing half may still name
    // internal tracking issues (that's what reaches stderr/logs), but the
    // user-facing half must read like a product message and must point at
    // the one login path that actually still works (`ato login` from a
    // terminal, which is not gated by this constant).
    let (dev_detail, user_message) = desktop_login_gate_messages();

    assert!(
        dev_detail.contains("ato-api#275"),
        "developer detail should still name the blocking dependency for logs/tests"
    );

    for jargon in ["ato-api#275", "ato#1077", "RFC", "release-ordering"] {
        assert!(
            !user_message.contains(jargon),
            "user-facing message must not leak internal jargon ({jargon:?}), got: {user_message}"
        );
    }
    assert!(
        user_message.contains("ato login"),
        "user-facing message should point at the still-working terminal fallback, got: {user_message}"
    );
}

#[test]
fn desktop_login_gate_user_message_does_not_imply_terminal_fallback_is_safe() {
    // Round-3 review finding (Major): the round-2 wording ("...to sign in
    // instead") read as if `ato login` in a terminal were a safe substitute
    // for the very OS-browser exposure this gate exists to block. It is not:
    // plain `ato login`'s existing non-headless branch
    // (`bridge_authenticate_ephemeral`, reached from
    // `login_with_store_device_flow`) already opens a real OS browser
    // against the same unhardened `/v1/auth/bridge/*` endpoints today on
    // `main` — identical residual exposure to what this gate blocks for
    // Desktop. The message must keep pointing at the one login path that
    // still works (round-2 requirement, re-checked above) without framing it
    // as an equally-safe replacement.
    let (_, user_message) = desktop_login_gate_messages();

    assert!(
        !user_message.to_lowercase().contains("instead"),
        "user-facing message must not frame the terminal fallback as a safe substitute \
         (avoid \"...instead\" phrasing), got: {user_message}"
    );
    assert!(
        user_message.contains("ato login"),
        "user-facing message must still name the one login path that currently works, got: {user_message}"
    );
}

#[test]
fn hardening_flag_requires_recorded_evidence_when_enabled() {
    // Round-4 review finding (Major, cross-repo-dependency): converts the
    // flip of `EXTERNAL_BROWSER_LOGIN_HARDENING_LANDED` from a
    // discipline-based obligation ("flip this only once ato-api#275 has
    // merged AND deployed", a comment a reviewer just has to trust) into a
    // CI-enforced one. A future contributor who flips the flag to `true`
    // without also replacing `EXTERNAL_BROWSER_LOGIN_HARDENING_EVIDENCE`'s
    // sentinel value in the same commit fails this test, and therefore CI.
    const PENDING_SENTINEL: &str =
        "PENDING: ato-api#275 not yet merged to ato-api's main and deployed to ato-api production";

    if EXTERNAL_BROWSER_LOGIN_HARDENING_LANDED {
        assert_ne!(
            EXTERNAL_BROWSER_LOGIN_HARDENING_EVIDENCE, PENDING_SENTINEL,
            "flipping EXTERNAL_BROWSER_LOGIN_HARDENING_LANDED to true requires recording \
             real ato-api#275 merge+deploy evidence in \
             EXTERNAL_BROWSER_LOGIN_HARDENING_EVIDENCE in the same commit"
        );
    } else {
        // While the gate is still shut, the evidence constant must still be
        // the sentinel — if someone updates one without the other (in
        // either direction), that is itself a sign the two drifted apart.
        assert_eq!(
            EXTERNAL_BROWSER_LOGIN_HARDENING_EVIDENCE, PENDING_SENTINEL,
            "EXTERNAL_BROWSER_LOGIN_HARDENING_EVIDENCE was edited without flipping \
             EXTERNAL_BROWSER_LOGIN_HARDENING_LANDED to true — keep the two in sync"
        );
    }
}

#[test]
fn compute_poll_timing_caps_timeout_at_300_and_floors_interval_at_1() {
    // Round-4 review finding (Major, test-coverage): this arithmetic is
    // shared verbatim between `bridge_authenticate_ephemeral` (plain `ato
    // login`, reachable today) and `login_with_store_device_flow_desktop`
    // (unreachable while the fail-closed gate is shut) precisely so a
    // regression here cannot silently diverge between the two — and so it
    // has direct test coverage regardless of the gate's boolean value.
    let (timeout, interval) = compute_poll_timing(9_999, Some(0));
    assert_eq!(timeout, 300, "poll timeout must be capped at 300 seconds");
    assert_eq!(interval, 1, "poll interval must be floored at 1 second");
}

#[test]
fn compute_poll_timing_defaults_interval_to_2_when_absent_and_keeps_timeout_under_cap() {
    let (timeout, interval) = compute_poll_timing(120, None);
    assert_eq!(timeout, 120);
    assert_eq!(interval, 2, "default poll interval must be 2 seconds");
}

#[test]
fn browser_launch_failed_event_carries_login_url_and_sanitized_message() {
    // Round-4 review finding (Major, test-coverage): the
    // `desktop_browser_launch_failed` NDJSON payload was previously
    // constructed only inline at a call site unreachable while
    // `EXTERNAL_BROWSER_LOGIN_HARDENING_LANDED` is `false`, so its exact
    // schema had never actually run under any test. It is now a pure,
    // directly-tested function.
    let error = anyhow::anyhow!("no handler registered");
    let (message, event) = browser_launch_failed_event("https://ato.run/auth?next=abc", &error);

    assert!(
        message.contains("no handler registered"),
        "message should surface the underlying open-browser error, got: {message}"
    );
    assert_eq!(event["type"], "desktop_browser_launch_failed");
    assert_eq!(event["login_url"], "https://ato.run/auth?next=abc");
    assert_eq!(event["message"], message);
}

#[test]
fn sanitize_bridge_failure_keeps_raw_detail_out_of_the_user_message() {
    // Round-4 review finding (Major, information-disclosure-ux):
    // `login_with_store_device_flow_desktop`'s poll/exchange/init failure
    // branches used to push a raw ato-api HTTP status + response body
    // straight into the `desktop_login_failed` event's `message` field,
    // which `on_login_completion` (ato-desktop) forwards verbatim into a
    // Dock toast. `sanitize_bridge_failure` is the boundary that keeps the
    // raw text out of `message` (only `detail`, which ato-desktop must
    // never show to a user, carries it).
    let raw = "Bridge auth exchange failed (500): {\"secret_debug_token\":\"abc123\"}".to_string();
    let (message, detail) = sanitize_bridge_failure("Sign-in failed to complete.", raw.clone());

    assert_eq!(
        detail, raw,
        "detail must preserve the full raw diagnostic for logs"
    );
    assert!(
        !message.contains("secret_debug_token") && !message.contains("abc123"),
        "user-facing message must not contain any part of the raw response body, got: {message}"
    );
    assert!(message.contains("Sign-in failed to complete."));
    assert!(
        message.contains("ato login"),
        "user-facing message should point the user at a working fallback, got: {message}"
    );
}
