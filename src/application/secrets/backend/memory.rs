use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::Result;

use super::traits::{BackendEntry, SecretBackend, SecretKey};

struct CachedEntry {
    value: String,
    cached_at: Instant,
    description: Option<String>,
    allow: Option<Vec<String>>,
    deny: Option<Vec<String>>,
    created_at: String,
}

/// In-process secret cache with optional TTL.
///
/// Used for session-scoped secrets so that after the identity is unlocked
/// once, subsequent `get()` calls don't need to decrypt from disk again.
pub(crate) struct MemoryBackend {
    cache: Arc<RwLock<HashMap<(String, String), CachedEntry>>>,
    ttl: Option<Duration>,
}

impl MemoryBackend {
    pub(crate) fn new(ttl: Option<Duration>) -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
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

impl SecretBackend for MemoryBackend {
    fn get(&self, key: &SecretKey) -> Result<Option<String>> {
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
        key: &SecretKey,
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

    fn delete(&self, key: &SecretKey) -> Result<()> {
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
        key: &SecretKey,
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
