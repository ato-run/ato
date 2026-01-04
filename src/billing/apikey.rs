//! API Key Authentication for Cloud/Billing
//!
//! Features:
//! - API Key generation and revocation
//! - Customer-to-key mapping
//! - Rate limiting per key
//! - Key metadata (created_at, last_used, etc.)
//!
//! Security:
//! - Uses constant-time comparison to prevent timing attacks
//! - SHA-256 hash for key storage (suitable for high-entropy API keys)

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Prefix for API keys
const API_KEY_PREFIX: &str = "gum_";

/// API Key metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    /// The key ID (used for lookup)
    pub id: String,
    /// The full key (hashed for storage)
    pub key_hash: String,
    /// Customer ID this key belongs to
    pub customer_id: String,
    /// Human-readable name for the key
    pub name: String,
    /// Key scopes/permissions
    pub scopes: Vec<String>,
    /// When the key was created (Unix timestamp)
    pub created_at: u64,
    /// When the key was last used (Unix timestamp)
    pub last_used_at: Option<u64>,
    /// When the key expires (Unix timestamp, None = never)
    pub expires_at: Option<u64>,
    /// Whether the key is active
    pub is_active: bool,
    /// Rate limit (requests per minute)
    pub rate_limit: u32,
}

/// Rate limit state for an API key
#[derive(Debug)]
struct RateLimitState {
    /// Request count in current window
    count: u32,
    /// Window start time
    window_start: Instant,
}

/// API Key store trait for pluggable backends
#[async_trait::async_trait]
pub trait ApiKeyStore: Send + Sync {
    /// Get a key by its ID
    async fn get(&self, key_id: &str) -> Option<ApiKey>;

    /// Get a key by its full key (for validation)
    async fn get_by_key(&self, full_key: &str) -> Option<ApiKey>;

    /// Store a new key
    async fn store(&self, key: ApiKey) -> Result<()>;

    /// Update a key
    async fn update(&self, key: ApiKey) -> Result<()>;

    /// Delete a key
    async fn delete(&self, key_id: &str) -> Result<()>;

    /// List all keys for a customer
    async fn list_by_customer(&self, customer_id: &str) -> Vec<ApiKey>;

    /// Update last_used_at timestamp
    async fn touch(&self, key_id: &str) -> Result<()>;
}

/// In-memory API key store (for development/testing)
pub struct InMemoryApiKeyStore {
    keys: RwLock<HashMap<String, ApiKey>>,
}

impl InMemoryApiKeyStore {
    pub fn new() -> Self {
        Self {
            keys: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryApiKeyStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ApiKeyStore for InMemoryApiKeyStore {
    async fn get(&self, key_id: &str) -> Option<ApiKey> {
        self.keys.read().await.get(key_id).cloned()
    }

    async fn get_by_key(&self, full_key: &str) -> Option<ApiKey> {
        let key_hash = hash_key(full_key);
        let key_hash_bytes = key_hash.as_bytes();
        self.keys
            .read()
            .await
            .values()
            // Use constant-time comparison to prevent timing attacks
            .find(|k| {
                let stored_hash_bytes = k.key_hash.as_bytes();
                stored_hash_bytes.len() == key_hash_bytes.len()
                    && bool::from(stored_hash_bytes.ct_eq(key_hash_bytes))
            })
            .cloned()
    }

    async fn store(&self, key: ApiKey) -> Result<()> {
        self.keys.write().await.insert(key.id.clone(), key);
        Ok(())
    }

    async fn update(&self, key: ApiKey) -> Result<()> {
        let mut keys = self.keys.write().await;
        if keys.contains_key(&key.id) {
            keys.insert(key.id.clone(), key);
            Ok(())
        } else {
            Err(anyhow!("Key not found"))
        }
    }

    async fn delete(&self, key_id: &str) -> Result<()> {
        self.keys.write().await.remove(key_id);
        Ok(())
    }

    async fn list_by_customer(&self, customer_id: &str) -> Vec<ApiKey> {
        self.keys
            .read()
            .await
            .values()
            .filter(|k| k.customer_id == customer_id)
            .cloned()
            .collect()
    }

    async fn touch(&self, key_id: &str) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut keys = self.keys.write().await;
        if let Some(key) = keys.get_mut(key_id) {
            key.last_used_at = Some(now);
            Ok(())
        } else {
            Err(anyhow!("Key not found"))
        }
    }
}

/// API Key Manager
pub struct ApiKeyManager {
    store: Arc<dyn ApiKeyStore>,
    rate_limits: RwLock<HashMap<String, RateLimitState>>,
    default_rate_limit: u32,
    rate_limit_window: Duration,
}

impl ApiKeyManager {
    pub fn new(store: Arc<dyn ApiKeyStore>) -> Self {
        Self {
            store,
            rate_limits: RwLock::new(HashMap::new()),
            default_rate_limit: 60, // 60 requests per minute
            rate_limit_window: Duration::from_secs(60),
        }
    }

    pub fn with_rate_limit(mut self, requests_per_minute: u32) -> Self {
        self.default_rate_limit = requests_per_minute;
        self
    }

    /// Generate a new API key for a customer
    pub async fn create_key(
        &self,
        customer_id: &str,
        name: &str,
        scopes: Vec<String>,
        expires_in: Option<Duration>,
    ) -> Result<(String, ApiKey)> {
        // Generate key parts
        let key_id = generate_key_id();
        let key_secret = generate_key_secret();
        let full_key = format!("{}{}{}", API_KEY_PREFIX, key_id, key_secret);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let expires_at = expires_in.map(|d| now + d.as_secs());

        let api_key = ApiKey {
            id: key_id.clone(),
            key_hash: hash_key(&full_key),
            customer_id: customer_id.to_string(),
            name: name.to_string(),
            scopes,
            created_at: now,
            last_used_at: None,
            expires_at,
            is_active: true,
            rate_limit: self.default_rate_limit,
        };

        self.store.store(api_key.clone()).await?;

        info!(
            "Created API key {} for customer {} (name: {})",
            key_id, customer_id, name
        );

        // Return the full key only once - it cannot be retrieved again
        Ok((full_key, api_key))
    }

    /// Validate an API key and check rate limits
    pub async fn validate(&self, full_key: &str) -> Result<ApiKey> {
        // Check key format
        if !full_key.starts_with(API_KEY_PREFIX) {
            return Err(anyhow!("Invalid key format"));
        }

        // Look up key
        let key = self
            .store
            .get_by_key(full_key)
            .await
            .ok_or_else(|| anyhow!("Key not found"))?;

        // Check if active
        if !key.is_active {
            return Err(anyhow!("Key is inactive"));
        }

        // Check expiration
        if let Some(expires_at) = key.expires_at {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            if now > expires_at {
                return Err(anyhow!("Key has expired"));
            }
        }

        // Check rate limit
        if !self.check_rate_limit(&key.id, key.rate_limit).await {
            return Err(anyhow!("Rate limit exceeded"));
        }

        // Update last used
        let _ = self.store.touch(&key.id).await;

        Ok(key)
    }

    /// Check and update rate limit
    async fn check_rate_limit(&self, key_id: &str, limit: u32) -> bool {
        let now = Instant::now();
        let mut rate_limits = self.rate_limits.write().await;

        let state = rate_limits
            .entry(key_id.to_string())
            .or_insert_with(|| RateLimitState {
                count: 0,
                window_start: now,
            });

        // Check if window has expired
        if now.duration_since(state.window_start) > self.rate_limit_window {
            state.count = 1;
            state.window_start = now;
            return true;
        }

        // Check if under limit
        if state.count < limit {
            state.count += 1;
            true
        } else {
            warn!("Rate limit exceeded for key {}", key_id);
            false
        }
    }

    /// Revoke an API key
    pub async fn revoke(&self, key_id: &str) -> Result<()> {
        let mut key = self
            .store
            .get(key_id)
            .await
            .ok_or_else(|| anyhow!("Key not found"))?;

        key.is_active = false;
        self.store.update(key).await?;

        info!("Revoked API key {}", key_id);
        Ok(())
    }

    /// Delete an API key permanently
    pub async fn delete(&self, key_id: &str) -> Result<()> {
        self.store.delete(key_id).await?;
        self.rate_limits.write().await.remove(key_id);

        info!("Deleted API key {}", key_id);
        Ok(())
    }

    /// List all keys for a customer
    pub async fn list_customer_keys(&self, customer_id: &str) -> Vec<ApiKey> {
        self.store.list_by_customer(customer_id).await
    }

    /// Update rate limit for a key
    pub async fn set_rate_limit(&self, key_id: &str, rate_limit: u32) -> Result<()> {
        let mut key = self
            .store
            .get(key_id)
            .await
            .ok_or_else(|| anyhow!("Key not found"))?;

        key.rate_limit = rate_limit;
        self.store.update(key).await?;

        debug!("Updated rate limit for key {} to {}", key_id, rate_limit);
        Ok(())
    }

    /// Get remaining rate limit for a key
    pub async fn get_remaining_rate_limit(&self, key_id: &str) -> Option<u32> {
        let key = self.store.get(key_id).await?;
        let rate_limits = self.rate_limits.read().await;

        if let Some(state) = rate_limits.get(key_id) {
            let now = Instant::now();
            if now.duration_since(state.window_start) > self.rate_limit_window {
                Some(key.rate_limit)
            } else {
                Some(key.rate_limit.saturating_sub(state.count))
            }
        } else {
            Some(key.rate_limit)
        }
    }
}

/// Generate a unique key ID (8 characters)
fn generate_key_id() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let chars: String = (0..8)
        .map(|_| {
            let idx = rng.gen_range(0..36);
            if idx < 10 {
                (b'0' + idx) as char
            } else {
                (b'a' + idx - 10) as char
            }
        })
        .collect();
    chars
}

/// Generate a key secret (24 characters)
fn generate_key_secret() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let chars: String = (0..24)
        .map(|_| {
            let idx = rng.gen_range(0..62);
            if idx < 10 {
                (b'0' + idx) as char
            } else if idx < 36 {
                (b'a' + idx - 10) as char
            } else {
                (b'A' + idx - 36) as char
            }
        })
        .collect();
    chars
}

/// Hash a key for storage (SHA-256)
fn hash_key(key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_and_validate_key() {
        let store = Arc::new(InMemoryApiKeyStore::new());
        let manager = ApiKeyManager::new(store);

        let (full_key, key_info) = manager
            .create_key("cus_123", "test-key", vec!["read".to_string()], None)
            .await
            .unwrap();

        assert!(full_key.starts_with(API_KEY_PREFIX));
        assert_eq!(key_info.customer_id, "cus_123");
        assert_eq!(key_info.name, "test-key");

        // Validate the key
        let validated = manager.validate(&full_key).await.unwrap();
        assert_eq!(validated.id, key_info.id);
    }

    #[tokio::test]
    async fn test_invalid_key_rejected() {
        let store = Arc::new(InMemoryApiKeyStore::new());
        let manager = ApiKeyManager::new(store);

        let result = manager.validate("gum_invalid_key").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_revoked_key_rejected() {
        let store = Arc::new(InMemoryApiKeyStore::new());
        let manager = ApiKeyManager::new(store);

        let (full_key, key_info) = manager
            .create_key("cus_123", "test-key", vec![], None)
            .await
            .unwrap();

        // Revoke the key
        manager.revoke(&key_info.id).await.unwrap();

        // Should fail validation
        let result = manager.validate(&full_key).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("inactive"));
    }

    #[tokio::test]
    async fn test_expired_key_rejected() {
        let store = Arc::new(InMemoryApiKeyStore::new());
        let manager = ApiKeyManager::new(store.clone());

        // Create a valid key first
        let (full_key, key_info) = manager
            .create_key("cus_123", "test-key", vec![], None)
            .await
            .unwrap();

        // Manually expire the key by setting expires_at to the past
        let mut expired_key = key_info.clone();
        expired_key.expires_at = Some(0); // Unix epoch = definitely expired
        store.update(expired_key).await.unwrap();

        // Should fail validation
        let result = manager.validate(&full_key).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("expired"));
    }

    #[tokio::test]
    async fn test_rate_limiting() {
        let store = Arc::new(InMemoryApiKeyStore::new());
        let manager = ApiKeyManager::new(store).with_rate_limit(3);

        let (full_key, _) = manager
            .create_key("cus_123", "test-key", vec![], None)
            .await
            .unwrap();

        // First 3 requests should succeed
        for _ in 0..3 {
            assert!(manager.validate(&full_key).await.is_ok());
        }

        // 4th request should fail
        let result = manager.validate(&full_key).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Rate limit"));
    }

    #[tokio::test]
    async fn test_list_customer_keys() {
        let store = Arc::new(InMemoryApiKeyStore::new());
        let manager = ApiKeyManager::new(store);

        // Create keys for different customers
        manager
            .create_key("cus_1", "key-1", vec![], None)
            .await
            .unwrap();
        manager
            .create_key("cus_1", "key-2", vec![], None)
            .await
            .unwrap();
        manager
            .create_key("cus_2", "key-3", vec![], None)
            .await
            .unwrap();

        let cus1_keys = manager.list_customer_keys("cus_1").await;
        assert_eq!(cus1_keys.len(), 2);

        let cus2_keys = manager.list_customer_keys("cus_2").await;
        assert_eq!(cus2_keys.len(), 1);
    }

    #[tokio::test]
    async fn test_delete_key() {
        let store = Arc::new(InMemoryApiKeyStore::new());
        let manager = ApiKeyManager::new(store);

        let (full_key, key_info) = manager
            .create_key("cus_123", "test-key", vec![], None)
            .await
            .unwrap();

        // Delete the key
        manager.delete(&key_info.id).await.unwrap();

        // Should fail validation
        let result = manager.validate(&full_key).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_key_format() {
        let key_id = generate_key_id();
        assert_eq!(key_id.len(), 8);

        let key_secret = generate_key_secret();
        assert_eq!(key_secret.len(), 24);
    }

    #[test]
    fn test_key_hashing() {
        let key1 = "gum_abc123xyz";
        let key2 = "gum_abc123xyz";
        let key3 = "gum_different";

        assert_eq!(hash_key(key1), hash_key(key2));
        assert_ne!(hash_key(key1), hash_key(key3));
    }
}
