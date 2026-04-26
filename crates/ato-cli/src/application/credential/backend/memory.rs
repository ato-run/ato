use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use anyhow::Result;

use super::traits::{BackendEntry, CredentialBackend, CredentialKey};

struct CachedEntry {
    value: String,
    cached_at: Instant,
    description: Option<String>,
    allow: Option<Vec<String>>,
    deny: Option<Vec<String>>,
    created_at: String,
}

/// In-process credential cache with optional TTL.
///
/// Namespace-generic: any credential domain (`secrets/*`, `auth/*`) can share
/// the same memory backend.
pub(crate) struct MemoryBackend {
    cache: RwLock<HashMap<(String, String), CachedEntry>>,
    ttl: Option<Duration>,
}

impl MemoryBackend {
    pub(crate) fn new(ttl: Option<Duration>) -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
            ttl,
        }
    }

    fn is_expired(&self, entry: &CachedEntry) -> bool {
        if let Some(ttl) = self.ttl {
            entry.cached_at.elapsed() > ttl
        } else {
            false
        }
    }
}

impl CredentialBackend for MemoryBackend {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn get(&self, key: &CredentialKey) -> Result<Option<String>> {
        let cache = self.cache.read().unwrap();
        let k = (key.namespace.clone(), key.name.clone());
        Ok(cache.get(&k).and_then(|e| {
            if self.is_expired(e) {
                None
            } else {
                Some(e.value.clone())
            }
        }))
    }

    fn set(
        &self,
        key: &CredentialKey,
        value: String,
        description: Option<&str>,
        allow: Option<Vec<String>>,
        deny: Option<Vec<String>>,
    ) -> Result<()> {
        let now_str = chrono::Utc::now().to_rfc3339();
        let mut cache = self.cache.write().unwrap();
        let k = (key.namespace.clone(), key.name.clone());
        cache.insert(
            k,
            CachedEntry {
                value,
                cached_at: Instant::now(),
                description: description.map(|s| s.to_string()),
                allow,
                deny,
                created_at: now_str,
            },
        );
        Ok(())
    }

    fn delete(&self, key: &CredentialKey) -> Result<()> {
        let mut cache = self.cache.write().unwrap();
        cache.remove(&(key.namespace.clone(), key.name.clone()));
        Ok(())
    }

    fn list(&self, namespace: &str) -> Result<Vec<BackendEntry>> {
        let cache = self.cache.read().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let mut entries: Vec<BackendEntry> = cache
            .iter()
            .filter(|((ns, _), e)| ns == namespace && !self.is_expired(e))
            .map(|((_, name), e)| BackendEntry {
                key: name.clone(),
                namespace: namespace.to_string(),
                created_at: e.created_at.clone(),
                updated_at: now.clone(),
                description: e.description.clone(),
                allow: e.allow.clone(),
                deny: e.deny.clone(),
            })
            .collect();
        entries.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(entries)
    }

    fn update_acl(
        &self,
        key: &CredentialKey,
        allow: Option<Vec<String>>,
        deny: Option<Vec<String>>,
    ) -> Result<()> {
        let mut cache = self.cache.write().unwrap();
        let k = (key.namespace.clone(), key.name.clone());
        if let Some(entry) = cache.get_mut(&k) {
            if allow.is_some() {
                entry.allow = allow;
            }
            if deny.is_some() {
                entry.deny = deny;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_set_get() {
        let backend = MemoryBackend::new(None);
        let key = CredentialKey::new("secrets/default", "K");
        backend.set(&key, "v".into(), None, None, None).unwrap();
        assert_eq!(backend.get(&key).unwrap(), Some("v".into()));
    }

    #[test]
    fn namespace_isolation() {
        let backend = MemoryBackend::new(None);
        let a = CredentialKey::new("secrets/default", "K");
        let b = CredentialKey::new("auth/session", "K");
        backend.set(&a, "secret".into(), None, None, None).unwrap();
        backend.set(&b, "auth".into(), None, None, None).unwrap();
        assert_eq!(backend.get(&a).unwrap(), Some("secret".into()));
        assert_eq!(backend.get(&b).unwrap(), Some("auth".into()));
    }

    #[test]
    fn delete_removes_entry() {
        let backend = MemoryBackend::new(None);
        let key = CredentialKey::new("secrets/default", "K");
        backend.set(&key, "v".into(), None, None, None).unwrap();
        backend.delete(&key).unwrap();
        assert_eq!(backend.get(&key).unwrap(), None);
    }

    #[test]
    fn ttl_expires_entry() {
        let backend = MemoryBackend::new(Some(Duration::from_millis(10)));
        let key = CredentialKey::new("secrets/default", "K");
        backend.set(&key, "v".into(), None, None, None).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(backend.get(&key).unwrap(), None);
    }

    #[test]
    fn list_only_requested_namespace() {
        let backend = MemoryBackend::new(None);
        backend
            .set(
                &CredentialKey::new("secrets/default", "A"),
                "1".into(),
                None,
                None,
                None,
            )
            .unwrap();
        backend
            .set(
                &CredentialKey::new("auth/session", "B"),
                "2".into(),
                None,
                None,
                None,
            )
            .unwrap();
        let entries = backend.list("secrets/default").unwrap();
        let keys: Vec<_> = entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, vec!["A"]);
    }
}
