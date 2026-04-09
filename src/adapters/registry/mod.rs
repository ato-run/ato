//! Distributed Registry Resolution
//!
//! Supports multiple registry discovery methods:
//! - DNS TXT records
//! - Well-known JSON endpoints
//! - DHT/Git (future)

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub(crate) mod binding;
pub(crate) mod http;
pub(crate) mod publish;
pub(crate) mod serve;
pub(crate) mod state;
pub(crate) mod store;
pub(crate) mod url;

/// Registry info discovered from various sources
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryInfo {
    /// Registry API URL
    pub url: String,
    /// Registry name
    pub name: Option<String>,
    /// Registry public key (DID)
    pub public_key: Option<String>,
    /// Discovery source
    pub source: DiscoverySource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiscoverySource {
    /// DNS TXT record
    Dns,
    /// Well-known JSON endpoint
    WellKnown,
    /// Local configuration
    Config,
    /// DHT lookup (future)
    Dht,
}

/// DNS-based registry resolution
pub struct DnsResolver {
    /// DNS TXT record prefix
    prefix: String,
}

impl Default for DnsResolver {
    fn default() -> Self {
        Self {
            prefix: "_capsule-registry".to_string(),
        }
    }
}

impl DnsResolver {
    /// Create a new DNS resolver with custom prefix
    /// Resolve registry URL from a domain's DNS TXT record
    ///
    /// Example: _capsule-registry.example.com TXT "v=1 url=https://registry.example.com"
    pub async fn resolve(&self, domain: &str) -> Result<Option<RegistryInfo>> {
        let lookup_name = format!("{}.{}", self.prefix, domain);

        // Use trust-dns or hickory-dns for resolution
        let resolver = hickory_resolver::TokioAsyncResolver::tokio_from_system_conf()
            .context("Failed to create DNS resolver")?;

        let response = match resolver.txt_lookup(&lookup_name).await {
            Ok(r) => r,
            Err(e) => {
                // No TXT record is not an error, just means no registry configured
                if e.to_string().contains("no records") || e.to_string().contains("NXDomain") {
                    return Ok(None);
                }
                return Err(e.into());
            }
        };

        // Parse TXT records
        for record in response.iter() {
            for txt in record.iter() {
                if let Ok(txt_str) = std::str::from_utf8(txt) {
                    if let Some(info) = self.parse_txt_record(txt_str, domain) {
                        return Ok(Some(info));
                    }
                }
            }
        }

        Ok(None)
    }

    /// Parse a TXT record value
    /// Format: v=1 url=https://registry.example.com [key=did:key:...]
    fn parse_txt_record(&self, txt: &str, domain: &str) -> Option<RegistryInfo> {
        let mut version = None;
        let mut url = None;
        let mut key = None;

        for part in txt.split_whitespace() {
            if let Some((k, v)) = part.split_once('=') {
                match k {
                    "v" => version = Some(v.to_string()),
                    "url" => url = Some(v.to_string()),
                    "key" => key = Some(v.to_string()),
                    _ => {}
                }
            }
        }

        // Require version 1 and url
        if version.as_deref() != Some("1") {
            return None;
        }

        url.map(|u| RegistryInfo {
            url: u,
            name: Some(domain.to_string()),
            public_key: key,
            source: DiscoverySource::Dns,
        })
    }
}

/// Well-known JSON endpoint resolver
pub struct WellKnownResolver {
    client: reqwest::Client,
}

impl Default for WellKnownResolver {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_default(),
        }
    }
}

/// Well-known ato registry JSON format
#[derive(Debug, Deserialize)]
struct WellKnownCapsule {
    url: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    public_key: Option<String>,
    version: String,
}

impl WellKnownResolver {
    /// Resolve registry from /.well-known/capsule.json
    pub async fn resolve(&self, domain: &str) -> Result<Option<RegistryInfo>> {
        // Try https first, then http for localhost
        let urls =
            if domain.contains("localhost") || domain.starts_with("127.") || domain.contains("::1")
            {
                vec![
                    format!("http://{}/.well-known/capsule.json", domain),
                    format!("https://{}/.well-known/capsule.json", domain),
                ]
            } else {
                vec![
                    format!("https://{}/.well-known/capsule.json", domain),
                    format!("http://{}/.well-known/capsule.json", domain),
                ]
            };

        for url in urls {
            let response = match self.client.get(&url).send().await {
                Ok(r) if r.status().is_success() => r,
                Ok(_) => continue,  // 404 or other non-success
                Err(_) => continue, // Connection failed
            };

            let wk: WellKnownCapsule = response.json().await.context("Invalid JSON")?;

            if wk.version != "1" {
                continue;
            }

            return Ok(Some(RegistryInfo {
                url: wk.url,
                name: wk.name,
                public_key: wk.public_key,
                source: DiscoverySource::WellKnown,
            }));
        }

        Ok(None)
    }
}

/// Composite resolver that tries multiple discovery methods
pub struct RegistryResolver {
    dns: DnsResolver,
    well_known: WellKnownResolver,
    /// Fallback registries from config
    fallbacks: Vec<RegistryInfo>,
}

impl Default for RegistryResolver {
    fn default() -> Self {
        Self {
            dns: DnsResolver::default(),
            well_known: WellKnownResolver::default(),
            fallbacks: vec![RegistryInfo {
                url: "https://api.ato.run".to_string(),
                name: Some("Ato Public Registry".to_string()),
                public_key: None,
                source: DiscoverySource::Config,
            }],
        }
    }
}

impl RegistryResolver {
    /// Add a fallback registry
    #[cfg(test)]
    pub fn with_fallback(mut self, info: RegistryInfo) -> Self {
        self.fallbacks.push(info);
        self
    }

    /// Resolve registry for a domain
    ///
    /// Tries in order:
    /// 1. DNS TXT record (skipped for localhost)
    /// 2. Well-known JSON
    /// 3. Fallback config
    pub async fn resolve(&self, domain: &str) -> Result<RegistryInfo> {
        // For localhost/127.0.0.1, use local dev registry directly
        // Beta mode: DNS discovery is disabled for local development
        if Self::is_localhost(domain) {
            // Try well-known first (http://localhost:8787/.well-known/capsule.json)
            if let Some(info) = self.well_known.resolve("localhost:8787").await? {
                return Ok(info);
            }
        } else {
            // Try DNS first
            if let Some(info) = self.dns.resolve(domain).await? {
                return Ok(info);
            }

            // Try well-known
            if let Some(info) = self.well_known.resolve(domain).await? {
                return Ok(info);
            }
        }

        // Return first fallback
        self.fallbacks
            .first()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("No registry found for domain: {}", domain))
    }

    /// Check if domain is localhost (skip DNS for beta/local development)
    fn is_localhost(domain: &str) -> bool {
        domain == "localhost"
            || domain == "127.0.0.1"
            || domain.starts_with("127.0.0.")
            || domain == "::1"
            || domain == "[::1]"
            || domain.starts_with("[::1]:")
    }

    /// Resolve registry for an app ID (DID)
    ///
    /// If the app_id contains a domain hint, use that.
    /// Otherwise, use the default registry.
    pub async fn resolve_for_app(&self, _app_id: &str) -> Result<RegistryInfo> {
        // Check for domain hint in metadata (future)
        // For now, just return default
        self.fallbacks
            .first()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("No default registry configured"))
    }
}

/// Local registry cache for offline operation
pub struct RegistryCache {
    path: std::path::PathBuf,
}

impl RegistryCache {
    pub fn new() -> Self {
        let path = dirs::cache_dir()
            .unwrap_or_else(|| std::path::PathBuf::from(".").join(".tmp"))
            .join("capsule")
            .join("registry_cache");
        Self { path }
    }

    /// Create a cache with a custom path (used for tests)
    #[cfg(test)]
    pub fn with_path(path: std::path::PathBuf) -> Self {
        Self { path }
    }

    /// Cache a registry info
    #[cfg(test)]
    pub fn put(&self, domain: &str, info: &RegistryInfo) -> Result<()> {
        std::fs::create_dir_all(&self.path)?;
        let file = self.path.join(format!("{}.json", domain.replace('.', "_")));
        let json = serde_json::to_string_pretty(info)?;
        std::fs::write(file, json)?;
        Ok(())
    }

    /// Get cached registry info
    #[cfg(test)]
    pub fn get(&self, domain: &str) -> Option<RegistryInfo> {
        let file = self.path.join(format!("{}.json", domain.replace('.', "_")));
        let content = std::fs::read_to_string(file).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Clear cache
    pub fn clear(&self) -> Result<()> {
        if self.path.exists() {
            std::fs::remove_dir_all(&self.path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_temp_cache() -> (RegistryCache, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let cache = RegistryCache::with_path(tmp.path().join("registry_cache"));
        (cache, tmp)
    }

    #[test]
    fn test_parse_txt_record() {
        let resolver = DnsResolver::default();

        // Valid record
        let info = resolver
            .parse_txt_record(
                "v=1 url=https://registry.example.com key=did:key:z6Mk",
                "example.com",
            )
            .unwrap();
        assert_eq!(info.url, "https://registry.example.com");
        assert_eq!(info.public_key, Some("did:key:z6Mk".to_string()));

        // Missing version
        assert!(resolver
            .parse_txt_record("url=https://registry.example.com", "example.com")
            .is_none());

        // Wrong version
        assert!(resolver
            .parse_txt_record("v=2 url=https://registry.example.com", "example.com")
            .is_none());
    }

    #[test]
    fn test_registry_cache() {
        let (cache, _tmp) = make_temp_cache();
        let info = RegistryInfo {
            url: "https://test.example.com".to_string(),
            name: Some("Test".to_string()),
            public_key: None,
            source: DiscoverySource::Config,
        };

        cache.put("test.example.com", &info).unwrap();
        let cached = cache.get("test.example.com").unwrap();
        assert_eq!(cached.url, info.url);
    }

    #[test]
    fn test_parse_txt_record_without_key() {
        let resolver = DnsResolver::default();
        let info = resolver
            .parse_txt_record("v=1 url=https://registry.example.com", "example.com")
            .unwrap();
        assert_eq!(info.url, "https://registry.example.com");
        assert!(info.public_key.is_none());
        assert!(matches!(info.source, DiscoverySource::Dns));
    }

    #[test]
    fn test_parse_txt_record_extra_fields() {
        let resolver = DnsResolver::default();
        let info = resolver
            .parse_txt_record(
                "v=1 url=https://registry.example.com key=did:key:z6Mk extra=ignored",
                "test.com",
            )
            .unwrap();
        assert_eq!(info.url, "https://registry.example.com");
        assert_eq!(info.name, Some("test.com".to_string()));
    }

    #[test]
    fn test_registry_resolver_default() {
        let resolver = RegistryResolver::default();
        assert!(!resolver.fallbacks.is_empty());
        assert_eq!(resolver.fallbacks[0].url, "https://api.ato.run");
    }

    #[test]
    fn test_registry_resolver_with_fallback() {
        let resolver = RegistryResolver::default().with_fallback(RegistryInfo {
            url: "https://custom.example.com".to_string(),
            name: Some("Custom".to_string()),
            public_key: None,
            source: DiscoverySource::Config,
        });
        assert_eq!(resolver.fallbacks.len(), 2);
    }

    #[test]
    fn test_discovery_source_serialization() {
        let info = RegistryInfo {
            url: "https://example.com".to_string(),
            name: None,
            public_key: None,
            source: DiscoverySource::WellKnown,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("\"wellknown\""));
    }

    #[test]
    fn test_cache_clear() {
        let (cache, _tmp) = make_temp_cache();
        let info = RegistryInfo {
            url: "https://clear-test.example.com".to_string(),
            name: None,
            public_key: None,
            source: DiscoverySource::Config,
        };
        cache.put("clear-test.example.com", &info).unwrap();
        assert!(cache.get("clear-test.example.com").is_some());

        cache.clear().unwrap();
        assert!(cache.get("clear-test.example.com").is_none());
    }

    #[test]
    fn test_cache_domain_encoding() {
        let (cache, _tmp) = make_temp_cache();
        let info = RegistryInfo {
            url: "https://sub.domain.example.com".to_string(),
            name: None,
            public_key: None,
            source: DiscoverySource::Config,
        };
        // Domain with dots should be stored with underscores
        cache.put("sub.domain.example.com", &info).unwrap();
        let cached = cache.get("sub.domain.example.com").unwrap();
        assert_eq!(cached.url, info.url);
    }
}
