use anyhow::Result;

use super::traits::{BackendEntry, SecretBackend, SecretKey};

const KEYCHAIN_SERVICE: &str = "ato.age";
const PASSPHRASE_ACCOUNT: &str = "identity-passphrase";

/// Keychain backend — passphrase cache only.
///
/// This backend stores exactly one entry: the passphrase used to protect the
/// age identity key.  All actual secrets live in `AgeFileBackend`.
///
/// Silently degraded (is_available = false) in CI and headless environments.
pub(crate) struct KeychainBackend {
    available: bool,
}

impl KeychainBackend {
    pub(crate) fn new() -> Self {
        Self {
            available: probe_keychain(),
        }
    }

    /// Retrieve the cached age identity passphrase, if any.
    pub(crate) fn get_passphrase(&self) -> Option<String> {
        if !self.available {
            return None;
        }
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, PASSPHRASE_ACCOUNT).ok()?;
        match entry.get_password() {
            Ok(p) => Some(p),
            Err(keyring::Error::NoEntry) => None,
            Err(_) => None,
        }
    }

    /// Store the age identity passphrase in the keychain.
    pub(crate) fn set_passphrase(&self, passphrase: &str) -> Result<()> {
        if !self.available {
            return Ok(());
        }
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, PASSPHRASE_ACCOUNT)?;
        entry
            .set_password(passphrase)
            .map_err(|e| anyhow::anyhow!("keychain write failed: {}", e))
    }

    /// Delete the cached passphrase.
    pub(crate) fn delete_passphrase(&self) -> Result<()> {
        if !self.available {
            return Ok(());
        }
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, PASSPHRASE_ACCOUNT)?;
        match entry.delete_password() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(anyhow::anyhow!("keychain delete failed: {}", e)),
        }
    }
}

fn probe_keychain() -> bool {
    #[cfg(test)]
    {
        return false;
    }
    #[cfg(not(test))]
    {
        if crate::application::secrets::storage::is_ci_environment() {
            return false;
        }
        let Ok(entry) = keyring::Entry::new("ato.age.probe", "__probe__") else {
            return false;
        };
        if entry.set_password("probe").is_err() {
            return false;
        }
        let _ = entry.delete_password();
        true
    }
}

/// `SecretBackend` implementation — refuses all regular secret operations.
///
/// The keychain backend only exposes the passphrase helpers above; it should
/// never appear as a regular secret store.
impl SecretBackend for KeychainBackend {
    fn is_available(&self) -> bool {
        self.available
    }

    fn is_writable(&self) -> bool {
        false
    }

    fn get(&self, _key: &SecretKey) -> Result<Option<String>> {
        // Passphrase retrieval is handled via get_passphrase(), not this path.
        Ok(None)
    }

    fn set(&self, _key: &SecretKey, _value: String, _desc: Option<&str>,
           _allow: Option<Vec<String>>, _deny: Option<Vec<String>>) -> Result<()> {
        anyhow::bail!("KeychainBackend only caches the age identity passphrase");
    }

    fn delete(&self, _key: &SecretKey) -> Result<()> {
        anyhow::bail!("KeychainBackend only caches the age identity passphrase");
    }

    fn list(&self, _namespace: &str) -> Result<Vec<BackendEntry>> {
        Ok(vec![])
    }

    fn update_acl(&self, _key: &SecretKey, _allow: Option<Vec<String>>,
                  _deny: Option<Vec<String>>) -> Result<()> {
        anyhow::bail!("KeychainBackend only caches the age identity passphrase");
    }
}
