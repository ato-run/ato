use anyhow::{Context, Result};
use keyring::{Entry, Error as KeyringError};

use super::traits::{BackendEntry, CredentialBackend, CredentialKey};

/// Namespaces this backend knows about. Anything outside this list is ignored
/// (returns `None` on `get`, no-op on `delete`) so the backend only activates
/// for the legacy auth data it was intended to hold.
pub(crate) const SESSION_NAMESPACE: &str = "auth/session";
pub(crate) const GITHUB_NAMESPACE: &str = "auth/github";

/// Canonical credential names within those namespaces.
pub(crate) const SESSION_CRED_NAME: &str = "SESSION_TOKEN";
pub(crate) const GITHUB_CRED_NAME: &str = "GITHUB_TOKEN";

/// Read-only bridge to the OS keyring holding pre-v0.6 auth tokens.
///
/// Only two credential keys are served:
///
///   - `CredentialKey { namespace: "auth/session",  name: "SESSION_TOKEN" }`
///   - `CredentialKey { namespace: "auth/github",   name: "GITHUB_TOKEN"  }`
///
/// …which are mapped to the historical `(service, account)` tuple used by
/// `AuthManager`. `set` / `update_acl` always fail (the backend is read-only
/// by design — new writes go to the age file). `delete` is allowed so logout
/// can purge any legacy token still sitting in the OS keyring.
///
/// Under `#[cfg(test)]` the backend consults an in-process `TEST_KEYRING`
/// instead of the real OS keyring whenever the service name ends in `.test`.
pub(crate) struct LegacyKeychainBackend {
    service: String,
    session_account: String,
    github_account: String,
}

impl LegacyKeychainBackend {
    pub(crate) fn new(
        service: impl Into<String>,
        session_account: impl Into<String>,
        github_account: impl Into<String>,
    ) -> Self {
        Self {
            service: service.into(),
            session_account: session_account.into(),
            github_account: github_account.into(),
        }
    }

    #[cfg(test)]
    pub(crate) fn service(&self) -> &str {
        &self.service
    }

    #[cfg(test)]
    pub(crate) fn session_account(&self) -> &str {
        &self.session_account
    }

    #[cfg(test)]
    pub(crate) fn github_account(&self) -> &str {
        &self.github_account
    }

    fn account_for(&self, key: &CredentialKey) -> Option<&str> {
        match (key.namespace.as_str(), key.name.as_str()) {
            (SESSION_NAMESPACE, SESSION_CRED_NAME) => Some(&self.session_account),
            (GITHUB_NAMESPACE, GITHUB_CRED_NAME) => Some(&self.github_account),
            _ => None,
        }
    }

    #[cfg(test)]
    fn is_test_service(&self) -> bool {
        self.service.ends_with(".test")
    }
}

impl CredentialBackend for LegacyKeychainBackend {
    fn name(&self) -> &'static str {
        "legacy_keychain"
    }

    fn is_writable(&self) -> bool {
        // New writes must not land here (age file is the default). `AuthStore`
        // bypasses the chain's writable-filter when purging, so logout still
        // clears this backend via explicit `delete`.
        false
    }

    fn get(&self, key: &CredentialKey) -> Result<Option<String>> {
        let Some(account) = self.account_for(key) else {
            return Ok(None);
        };

        #[cfg(test)]
        if self.is_test_service() {
            return Ok(test_support::get(&self.service, account));
        }

        let entry = match Entry::new(&self.service, account) {
            Ok(e) => e,
            Err(err) if is_nonfatal_keyring_error(&err) => return Ok(None),
            Err(err) => return Err(err).context("failed to initialize OS keyring entry"),
        };

        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(err) if is_nonfatal_keyring_error(&err) => Ok(None),
            Err(err) => Err(anyhow::anyhow!(
                "failed to read token from OS keyring: {err}"
            )),
        }
    }

    fn set(
        &self,
        _key: &CredentialKey,
        _value: String,
        _desc: Option<&str>,
        _allow: Option<Vec<String>>,
        _deny: Option<Vec<String>>,
    ) -> Result<()> {
        anyhow::bail!("LegacyKeychainBackend is read-only; new tokens go to the age file")
    }

    fn delete(&self, key: &CredentialKey) -> Result<()> {
        let Some(account) = self.account_for(key) else {
            return Ok(());
        };

        #[cfg(test)]
        if self.is_test_service() {
            test_support::delete(&self.service, account);
            return Ok(());
        }

        let entry = match Entry::new(&self.service, account) {
            Ok(e) => e,
            Err(err) if is_nonfatal_keyring_error(&err) => return Ok(()),
            Err(err) => return Err(err).context("failed to initialize OS keyring entry"),
        };

        match entry.delete_password() {
            Ok(_) | Err(KeyringError::NoEntry) => Ok(()),
            Err(err) if is_nonfatal_keyring_error(&err) => Ok(()),
            Err(err) => Err(anyhow::anyhow!(
                "failed to delete token from OS keyring: {err}"
            )),
        }
    }

    fn list(&self, _namespace: &str) -> Result<Vec<BackendEntry>> {
        // The OS keyring holds only the two hard-coded accounts; reporting
        // them as a list would leak keyring UX into `ato secrets list`.
        Ok(Vec::new())
    }

    fn update_acl(
        &self,
        _key: &CredentialKey,
        _allow: Option<Vec<String>>,
        _deny: Option<Vec<String>>,
    ) -> Result<()> {
        anyhow::bail!("LegacyKeychainBackend does not support ACLs")
    }
}

fn is_nonfatal_keyring_error(err: &KeyringError) -> bool {
    matches!(
        err,
        KeyringError::PlatformFailure(_) | KeyringError::NoStorageAccess(_)
    )
}

/// In-process keyring used under `#[cfg(test)]` and by auth tests that
/// previously owned the `TEST_KEYRING` static. Exposed `pub(crate)` so both
/// the backend and the auth-layer test helpers can share a single store.
#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static STORE: OnceLock<Mutex<HashMap<(String, String), String>>> = OnceLock::new();

    fn store() -> &'static Mutex<HashMap<(String, String), String>> {
        STORE.get_or_init(Default::default)
    }

    pub(crate) fn get(service: &str, account: &str) -> Option<String> {
        store()
            .lock()
            .expect("test keyring lock")
            .get(&(service.to_string(), account.to_string()))
            .cloned()
    }

    pub(crate) fn set(service: &str, account: &str, value: &str) {
        store().lock().expect("test keyring lock").insert(
            (service.to_string(), account.to_string()),
            value.to_string(),
        );
    }

    pub(crate) fn delete(service: &str, account: &str) {
        store()
            .lock()
            .expect("test keyring lock")
            .remove(&(service.to_string(), account.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_backend(suffix: &str) -> LegacyKeychainBackend {
        LegacyKeychainBackend::new(
            "run.ato.cli.test",
            format!("current_session-legacy-kc-{suffix}"),
            format!("github_token-legacy-kc-{suffix}"),
        )
    }

    #[test]
    fn get_returns_none_for_unknown_namespace() {
        let b = mk_backend("unknown-ns");
        let k = CredentialKey::new("secrets/default", "FOO");
        assert_eq!(b.get(&k).unwrap(), None);
    }

    #[test]
    fn get_returns_none_when_absent() {
        let b = mk_backend("absent");
        let k = CredentialKey::new(SESSION_NAMESPACE, SESSION_CRED_NAME);
        assert_eq!(b.get(&k).unwrap(), None);
    }

    #[test]
    fn get_returns_seeded_value_via_test_support() {
        let b = mk_backend("seeded");
        test_support::set(b.service(), b.session_account(), "seed-token");
        let k = CredentialKey::new(SESSION_NAMESPACE, SESSION_CRED_NAME);
        assert_eq!(b.get(&k).unwrap(), Some("seed-token".into()));
        // Cleanup so later tests in this suite start clean.
        test_support::delete(b.service(), b.session_account());
    }

    #[test]
    fn delete_clears_test_keyring_entry() {
        let b = mk_backend("deletes");
        test_support::set(b.service(), b.github_account(), "gh-token");
        let k = CredentialKey::new(GITHUB_NAMESPACE, GITHUB_CRED_NAME);
        b.delete(&k).unwrap();
        assert_eq!(test_support::get(b.service(), b.github_account()), None);
    }

    #[test]
    fn set_is_rejected() {
        let b = mk_backend("setrejects");
        let k = CredentialKey::new(SESSION_NAMESPACE, SESSION_CRED_NAME);
        assert!(b.set(&k, "x".into(), None, None, None).is_err());
    }

    #[test]
    fn is_writable_is_false() {
        let b = mk_backend("rw");
        assert!(!b.is_writable());
    }
}
