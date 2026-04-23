//! Domain-layer facade for auth tokens (session, GitHub), backed by the shared
//! `CredentialBackend` layer introduced in Phase 1.
//!
//! `AuthStore` is the auth counterpart to `SecretStore`:
//!   - **Reads** go through a `BackendChain`
//!     (`env → memory → age → legacy_keychain`).
//!   - **Writes** land in the age file when an identity is available, and in
//!     an in-process memory cache. The legacy OS keyring backend is read-only
//!     — pre-v0.6 users can still authenticate, but new tokens do not leak
//!     into the keychain.
//!   - **Deletes** (logout) are explicitly broadcast to every backend that
//!     might hold a copy, including the legacy keychain, so a full purge is
//!     possible regardless of writability flags.
//!
//! Namespaces:
//!   - `auth/session` / `SESSION_TOKEN` — Store device-flow session token
//!   - `auth/github`  / `GITHUB_TOKEN`  — GitHub Personal Access Token

use std::sync::Arc;

use anyhow::Result;

use crate::application::credential::backend::legacy_keychain::{
    GITHUB_CRED_NAME, GITHUB_NAMESPACE, SESSION_CRED_NAME, SESSION_NAMESPACE,
};
use crate::application::credential::{
    self, backend::CredentialBackend, AgeFileBackend, BackendChain, CredentialKey, EnvBackend,
    LegacyKeychainBackend, MemoryBackend,
};

/// Which backend physically stores a freshly written token.
///
/// Returned by `AuthStore::set_*_token` so the UI can tell the user where
/// their credential actually lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriteLocation {
    /// Wrote to `~/.ato/credentials/auth/<sub>.age`.
    AgeFile,
    /// Age identity not loaded; held only in the in-process memory cache.
    Memory,
}

impl WriteLocation {
    pub(crate) fn display(&self) -> &'static str {
        match self {
            WriteLocation::AgeFile => "age file",
            WriteLocation::Memory => "in-memory cache (age identity not loaded)",
        }
    }
}

pub(crate) struct AuthStore {
    chain: BackendChain,
    age: Option<Arc<AgeFileBackend>>,
    memory: Arc<MemoryBackend>,
    legacy: Arc<LegacyKeychainBackend>,
}

impl AuthStore {
    /// Build an `AuthStore` from already-constructed backends.
    ///
    /// The `order` vector controls read priority for the chain (env / memory
    /// / age / legacy_keychain). `age` is optional — callers that can't
    /// unlock the identity (e.g. passphrase without a running session) still
    /// get working env/legacy fallback reads.
    pub(crate) fn with_backends(
        order: &[String],
        age: Option<Arc<AgeFileBackend>>,
        memory: Arc<MemoryBackend>,
        legacy: Arc<LegacyKeychainBackend>,
    ) -> Self {
        let chain = build_chain(order, age.clone(), memory.clone(), legacy.clone());
        Self {
            chain,
            age,
            memory,
            legacy,
        }
    }

    /// Standard constructor: tries to unlock `<home>/.ato/keys/identity.key`
    /// non-interactively and wires up the given legacy keychain accounts.
    ///
    /// `home` is passed explicitly so tests (and anything else that wants to
    /// redirect the age backend) can operate out of a temp directory without
    /// mutating `$HOME`.
    pub(crate) fn open_for_home(
        home: &std::path::Path,
        keyring_service: impl Into<String>,
        keyring_session_account: impl Into<String>,
        keyring_github_account: impl Into<String>,
    ) -> Result<Self> {
        let mut age_backend = AgeFileBackend::new(home.to_path_buf());
        let age = if try_load_identity_non_interactive(&mut age_backend) {
            Some(Arc::new(age_backend))
        } else {
            None
        };

        let memory = Arc::new(MemoryBackend::new(None));
        let legacy = Arc::new(LegacyKeychainBackend::new(
            keyring_service,
            keyring_session_account,
            keyring_github_account,
        ));

        let order =
            credential::config::read_order(home).unwrap_or_else(credential::config::default_order);

        Ok(Self::with_backends(&order, age, memory, legacy))
    }

    // ── Reads ────────────────────────────────────────────────────────────────

    pub(crate) fn get_session_token(&self) -> Result<Option<String>> {
        self.chain.get(&session_key())
    }

    pub(crate) fn get_github_token(&self) -> Result<Option<String>> {
        self.chain.get(&github_key())
    }

    // ── Writes ───────────────────────────────────────────────────────────────

    pub(crate) fn set_session_token(&self, token: &str) -> Result<WriteLocation> {
        self.set_token(&session_key(), token)
    }

    pub(crate) fn set_github_token(&self, token: &str) -> Result<WriteLocation> {
        self.set_token(&github_key(), token)
    }

    fn set_token(&self, key: &CredentialKey, token: &str) -> Result<WriteLocation> {
        // Always mirror into memory so subsequent reads in the same process
        // are cheap and don't race with the on-disk age write.
        self.memory.set(key, token.to_string(), None, None, None)?;

        match &self.age {
            Some(age) => {
                age.set(key, token.to_string(), None, None, None)?;
                Ok(WriteLocation::AgeFile)
            }
            None => Ok(WriteLocation::Memory),
        }
    }

    // ── Deletes (logout) ─────────────────────────────────────────────────────

    /// Purge one token from **every** backend (memory, age, legacy keychain).
    ///
    /// Errors from individual backends are swallowed: logout is best-effort
    /// and must not leave the user unable to try again. Env is never purged
    /// (the variable is the user's responsibility).
    #[cfg(test)]
    pub(crate) fn delete_session_token(&self) -> Result<()> {
        self.purge(&session_key());
        Ok(())
    }

    /// Purge **both** auth tokens from every backend. Equivalent to calling
    /// the two `delete_*_token` methods in sequence.
    pub(crate) fn delete_all_auth_tokens(&self) -> Result<()> {
        self.purge(&session_key());
        self.purge(&github_key());
        Ok(())
    }

    fn purge(&self, key: &CredentialKey) {
        let _ = self.memory.delete(key);
        if let Some(age) = &self.age {
            let _ = age.delete(key);
        }
        let _ = self.legacy.delete(key);
    }

    // ── Diagnostics ──────────────────────────────────────────────────────────

    /// Human-readable name of the backend that would receive a fresh write.
    /// Matches the `WriteLocation::display()` output for consistency.
    pub(crate) fn primary_write_backend_label(&self) -> &'static str {
        if self.age.is_some() {
            WriteLocation::AgeFile.display()
        } else {
            WriteLocation::Memory.display()
        }
    }

    /// True when the legacy OS keyring currently holds a session token. Used
    /// by `ato status` to warn pre-v0.6 users that their token is still in
    /// the keychain and should be migrated.
    pub(crate) fn legacy_session_token_present(&self) -> Result<bool> {
        Ok(self.legacy.get(&session_key())?.is_some())
    }
}

fn session_key() -> CredentialKey {
    CredentialKey::new(SESSION_NAMESPACE, SESSION_CRED_NAME)
}

fn github_key() -> CredentialKey {
    CredentialKey::new(GITHUB_NAMESPACE, GITHUB_CRED_NAME)
}

fn build_chain(
    order: &[String],
    age: Option<Arc<AgeFileBackend>>,
    memory: Arc<MemoryBackend>,
    legacy: Arc<LegacyKeychainBackend>,
) -> BackendChain {
    let mut backends: Vec<Arc<dyn CredentialBackend>> = Vec::new();
    let mut saw_legacy = false;
    for name in order {
        match name.as_str() {
            "env" => backends.push(Arc::new(EnvBackend::new())),
            "memory" => backends.push(memory.clone() as Arc<dyn CredentialBackend>),
            "age" => {
                if let Some(a) = &age {
                    backends.push(a.clone() as Arc<dyn CredentialBackend>);
                }
            }
            "legacy_keychain" => {
                backends.push(legacy.clone() as Arc<dyn CredentialBackend>);
                saw_legacy = true;
            }
            _ => {}
        }
    }
    // Always keep legacy keychain as the last-resort read fallback so pre-v0.6
    // users can still authenticate, even when they haven't opted into it
    // explicitly via `[credentials] order`.
    if !saw_legacy {
        backends.push(legacy as Arc<dyn CredentialBackend>);
    }
    BackendChain::new(backends)
}

fn try_load_identity_non_interactive(age: &mut AgeFileBackend) -> bool {
    if let Ok(session_path) = std::env::var("ATO_SESSION_KEY_FILE") {
        let p = std::path::Path::new(&session_path);
        if p.exists() {
            if let Ok(raw) = std::fs::read(p) {
                if let Ok(id) = credential::load_identity_bytes(&raw, None) {
                    age.install_identity(id);
                    return true;
                }
            }
        }
    }

    if !age.identity_exists() {
        return false;
    }
    age.load_identity_with_passphrase(None).is_ok()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::auth::shared_env_lock as env_lock;
    use crate::application::credential::backend::legacy_keychain::test_support;
    use tempfile::TempDir;

    /// RAII guard that restores an env var on drop — survives panics so a
    /// failing assertion can't leave state that contaminates other tests.
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

    fn test_store(tag: &str, with_age: bool) -> (TempDir, AuthStore) {
        let dir = TempDir::new().unwrap();
        let age = if with_age {
            let a = AgeFileBackend::new(dir.path().to_path_buf());
            a.init_identity(None).unwrap();
            Some(Arc::new(a))
        } else {
            None
        };
        let memory = Arc::new(MemoryBackend::new(None));
        let legacy = Arc::new(LegacyKeychainBackend::new(
            "run.ato.cli.test",
            format!("current_session-authstore-{tag}"),
            format!("github_token-authstore-{tag}"),
        ));
        let order = credential::config::default_order();
        let store = AuthStore::with_backends(&order, age, memory, legacy);
        (dir, store)
    }

    #[test]
    fn get_returns_none_when_nothing_stored() {
        let (_dir, store) = test_store("empty", true);
        assert!(store.get_session_token().unwrap().is_none());
        assert!(store.get_github_token().unwrap().is_none());
    }

    #[test]
    fn set_then_get_roundtrips_via_age() {
        let (_dir, store) = test_store("roundtrip", true);
        let loc = store.set_session_token("abc").unwrap();
        assert_eq!(loc, WriteLocation::AgeFile);
        assert_eq!(store.get_session_token().unwrap(), Some("abc".into()));
    }

    #[test]
    fn set_without_age_falls_back_to_memory() {
        let (_dir, store) = test_store("nomem", false);
        let loc = store.set_session_token("abc").unwrap();
        assert_eq!(loc, WriteLocation::Memory);
        assert_eq!(store.get_session_token().unwrap(), Some("abc".into()));
    }

    #[test]
    fn legacy_keychain_provides_read_fallback() {
        let (_dir, store) = test_store("legacy-read", true);
        // Seed the legacy OS-keyring slot directly.
        test_support::set(
            "run.ato.cli.test",
            "current_session-authstore-legacy-read",
            "legacy-token",
        );
        assert_eq!(
            store.get_session_token().unwrap(),
            Some("legacy-token".into())
        );
        // Cleanup so later tests aren't affected.
        test_support::delete("run.ato.cli.test", "current_session-authstore-legacy-read");
    }

    #[test]
    fn memory_beats_legacy_keychain_on_read() {
        let (_dir, store) = test_store("priority", true);
        test_support::set(
            "run.ato.cli.test",
            "current_session-authstore-priority",
            "legacy-val",
        );
        store.set_session_token("fresh-val").unwrap();
        assert_eq!(store.get_session_token().unwrap(), Some("fresh-val".into()));
        test_support::delete("run.ato.cli.test", "current_session-authstore-priority");
    }

    #[test]
    fn delete_session_token_purges_every_backend() {
        let (_dir, store) = test_store("purge", true);
        store.set_session_token("abc").unwrap();
        test_support::set(
            "run.ato.cli.test",
            "current_session-authstore-purge",
            "legacy",
        );
        store.delete_session_token().unwrap();
        assert!(store.get_session_token().unwrap().is_none());
        assert_eq!(
            test_support::get("run.ato.cli.test", "current_session-authstore-purge"),
            None
        );
    }

    #[test]
    fn delete_all_clears_both_auth_tokens() {
        let (_dir, store) = test_store("deleteall", true);
        store.set_session_token("s").unwrap();
        store.set_github_token("g").unwrap();
        store.delete_all_auth_tokens().unwrap();
        assert!(store.get_session_token().unwrap().is_none());
        assert!(store.get_github_token().unwrap().is_none());
    }

    #[test]
    fn env_override_wins() {
        let _serial = env_lock().lock().unwrap();
        let (_dir, store) = test_store("env-override", true);
        store.set_session_token("age-val").unwrap();
        let _guard = EnvVarGuard::set(
            "ATO_CRED_AUTH_SESSION__SESSION_TOKEN",
            Some("env-val"),
        );
        let got = store.get_session_token().unwrap();
        assert_eq!(got, Some("env-val".into()));
    }

    #[test]
    fn primary_write_backend_label_reports_age_when_loaded() {
        let (_dir, store) = test_store("label-age", true);
        assert_eq!(store.primary_write_backend_label(), "age file");
    }

    #[test]
    fn primary_write_backend_label_reports_memory_when_not_loaded() {
        let (_dir, store) = test_store("label-mem", false);
        assert!(store.primary_write_backend_label().contains("in-memory"));
    }
}
