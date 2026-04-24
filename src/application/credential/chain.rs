use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{bail, Result};

use super::backend::{BackendEntry, CredentialBackend, CredentialKey};

/// Ordered composition of `CredentialBackend`s.
///
/// `get` tries backends in order and returns the first hit. `set` writes to
/// the first writable+available backend. `delete_first` and `delete_all`
/// differ in logout semantics (Phase 2 for `auth`).
///
/// `list` merges entries from all backends in order, de-duplicated by
/// (namespace, key) with the first occurrence winning — matching `get`
/// priority.
///
/// Phase 1 only exercises `get` from `SecretStore`; the remaining methods
/// are used by Phase 2 (`AuthStore`).
#[allow(dead_code)]
pub(crate) struct BackendChain {
    backends: Vec<Arc<dyn CredentialBackend>>,
}

#[allow(dead_code)]
impl BackendChain {
    pub(crate) fn new(backends: Vec<Arc<dyn CredentialBackend>>) -> Self {
        Self { backends }
    }

    /// Fetch a credential from the first backend that has it.
    pub(crate) fn get(&self, key: &CredentialKey) -> Result<Option<String>> {
        for b in &self.backends {
            if !b.is_available() {
                continue;
            }
            match b.get(key) {
                Ok(Some(v)) => return Ok(Some(v)),
                Ok(None) => continue,
                Err(_) => continue,
            }
        }
        Ok(None)
    }

    /// Write to the first writable+available backend.
    pub(crate) fn set(
        &self,
        key: &CredentialKey,
        value: String,
        description: Option<&str>,
        allow: Option<Vec<String>>,
        deny: Option<Vec<String>>,
    ) -> Result<()> {
        for b in &self.backends {
            if !b.is_writable() || !b.is_available() {
                continue;
            }
            return b.set(key, value, description, allow, deny);
        }
        bail!("no writable credential backend available")
    }

    /// Delete from the first writable backend that holds the key.
    pub(crate) fn delete_first(&self, key: &CredentialKey) -> Result<()> {
        for b in &self.backends {
            if !b.is_writable() || !b.is_available() {
                continue;
            }
            if matches!(b.get(key), Ok(Some(_))) {
                return b.delete(key);
            }
        }
        // Not found anywhere — still attempt deletion on the first writable
        // backend as a no-op semantic (matches previous secrets behavior).
        for b in &self.backends {
            if b.is_writable() && b.is_available() {
                return b.delete(key);
            }
        }
        Ok(())
    }

    /// Remove the credential from every writable backend that holds it.
    ///
    /// Used by Phase 2 `auth logout` to ensure a session token is purged
    /// from every layer (memory cache + age file + legacy keychain).
    #[allow(dead_code)]
    pub(crate) fn delete_all(&self, key: &CredentialKey) -> Result<()> {
        let mut last_err: Option<anyhow::Error> = None;
        for b in &self.backends {
            if !b.is_writable() || !b.is_available() {
                continue;
            }
            if let Err(e) = b.delete(key) {
                last_err = Some(e);
            }
        }
        match last_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Merge `list` across backends, deduplicating by entry `key`.
    pub(crate) fn list(&self, namespace: &str) -> Result<Vec<BackendEntry>> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<BackendEntry> = Vec::new();
        for b in &self.backends {
            if !b.is_available() {
                continue;
            }
            let entries = b.list(namespace).unwrap_or_default();
            for e in entries {
                if seen.insert(e.key.clone()) {
                    out.push(e);
                }
            }
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    /// Update ACL on the first writable backend that holds the key.
    pub(crate) fn update_acl(
        &self,
        key: &CredentialKey,
        allow: Option<Vec<String>>,
        deny: Option<Vec<String>>,
    ) -> Result<()> {
        for b in &self.backends {
            if !b.is_writable() || !b.is_available() {
                continue;
            }
            if matches!(b.get(key), Ok(Some(_))) {
                return b.update_acl(key, allow, deny);
            }
        }
        bail!(
            "credential '{}' not found in any writable backend (namespace '{}')",
            key.name,
            key.namespace
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::credential::backend::{EnvBackend, MemoryBackend};

    fn env_var(ns: &str, name: &str) -> String {
        let ns_up: String = ns
            .chars()
            .map(|c| match c {
                'a'..='z' => c.to_ascii_uppercase(),
                'A'..='Z' | '0'..='9' | '_' => c,
                _ => '_',
            })
            .collect();
        format!("ATO_CRED_{}__{}", ns_up, name)
    }

    #[test]
    fn get_priority_env_over_memory() {
        let env: Arc<dyn CredentialBackend> = Arc::new(EnvBackend::new());
        let mem: Arc<dyn CredentialBackend> = Arc::new(MemoryBackend::new(None));
        let chain = BackendChain::new(vec![env, mem.clone()]);

        let key = CredentialKey::new("secrets/default", "CHAIN_PRIO_TEST");

        // memory has "mem-val"
        mem.set(&key, "mem-val".into(), None, None, None).unwrap();

        // without env var set, memory wins
        assert_eq!(chain.get(&key).unwrap(), Some("mem-val".into()));

        // env overrides
        let v = env_var("secrets/default", "CHAIN_PRIO_TEST");
        std::env::set_var(&v, "env-val");
        let got = chain.get(&key).unwrap();
        std::env::remove_var(&v);
        assert_eq!(got, Some("env-val".into()));
    }

    #[test]
    fn set_skips_readonly_backend() {
        let env: Arc<dyn CredentialBackend> = Arc::new(EnvBackend::new());
        let mem: Arc<dyn CredentialBackend> = Arc::new(MemoryBackend::new(None));
        let chain = BackendChain::new(vec![env, mem.clone()]);

        let key = CredentialKey::new("secrets/default", "SET_SKIP_TEST");
        chain.set(&key, "v".into(), None, None, None).unwrap();
        // Should have landed in memory.
        assert_eq!(mem.get(&key).unwrap(), Some("v".into()));
    }

    #[test]
    fn delete_all_purges_every_writable_backend() {
        let mem_a: Arc<dyn CredentialBackend> = Arc::new(MemoryBackend::new(None));
        let mem_b: Arc<dyn CredentialBackend> = Arc::new(MemoryBackend::new(None));
        let chain = BackendChain::new(vec![mem_a.clone(), mem_b.clone()]);
        let key = CredentialKey::new("auth/session", "TOKEN");
        mem_a.set(&key, "a".into(), None, None, None).unwrap();
        mem_b.set(&key, "b".into(), None, None, None).unwrap();
        chain.delete_all(&key).unwrap();
        assert_eq!(mem_a.get(&key).unwrap(), None);
        assert_eq!(mem_b.get(&key).unwrap(), None);
    }

    #[test]
    fn delete_first_only_touches_one_backend() {
        let mem_a: Arc<dyn CredentialBackend> = Arc::new(MemoryBackend::new(None));
        let mem_b: Arc<dyn CredentialBackend> = Arc::new(MemoryBackend::new(None));
        let chain = BackendChain::new(vec![mem_a.clone(), mem_b.clone()]);
        let key = CredentialKey::new("secrets/default", "K");
        mem_a.set(&key, "a".into(), None, None, None).unwrap();
        mem_b.set(&key, "b".into(), None, None, None).unwrap();
        chain.delete_first(&key).unwrap();
        assert_eq!(mem_a.get(&key).unwrap(), None);
        assert_eq!(mem_b.get(&key).unwrap(), Some("b".into()));
    }

    #[test]
    fn list_merges_and_dedupes() {
        let mem_a: Arc<dyn CredentialBackend> = Arc::new(MemoryBackend::new(None));
        let mem_b: Arc<dyn CredentialBackend> = Arc::new(MemoryBackend::new(None));
        let ns = "secrets/default";
        mem_a
            .set(
                &CredentialKey::new(ns, "A"),
                "from-a".into(),
                None,
                None,
                None,
            )
            .unwrap();
        mem_b
            .set(
                &CredentialKey::new(ns, "A"),
                "from-b".into(),
                None,
                None,
                None,
            )
            .unwrap();
        mem_b
            .set(
                &CredentialKey::new(ns, "B"),
                "only-b".into(),
                None,
                None,
                None,
            )
            .unwrap();
        let chain = BackendChain::new(vec![mem_a, mem_b]);
        let entries = chain.list(ns).unwrap();
        let keys: Vec<_> = entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, vec!["A", "B"]);
    }
}
