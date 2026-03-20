//! CAS Client implementations for UARC V1.1.0 compliance.
//!
//! UARC requires source code availability via CAS for L1 Source Policy enforcement.
//! This module provides:
//! - `LocalCasClient`: Local filesystem-based CAS (development/testing)
//! - `HttpCasClient`: Remote HTTP-based CAS (production)

use async_trait::async_trait;
use reqwest::StatusCode;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use tracing::{debug, info};

use crate::error::{CapsuleError, Result};

/// Parse a digest string (e.g., "sha256:abc123...") into (algorithm, hash)
pub fn parse_digest(digest: &str) -> Result<(&str, &str)> {
    let parts: Vec<&str> = digest.splitn(2, ':').collect();
    if parts.len() != 2 {
        return Err(CapsuleError::Config(format!(
            "Invalid digest format (expected 'algorithm:hash'): {}",
            digest
        )));
    }
    Ok((parts[0], parts[1]))
}

/// Abstract trait for CAS client implementations.
///
/// Enables future extension to IPFS, P2P, or other distributed storage backends.
#[async_trait]
pub trait CasClient: Send + Sync {
    /// Fetch a blob by its digest and return the local path.
    ///
    /// The blob is verified against the digest before returning.
    async fn fetch_blob(&self, digest: &str) -> Result<PathBuf>;

    /// Store a blob and return its digest.
    ///
    /// Returns the SHA256 digest of the stored content.
    async fn store_blob(&self, path: &Path) -> Result<String>;

    /// Check if a blob exists without fetching it.
    async fn exists(&self, digest: &str) -> Result<bool>;
}

/// Local filesystem-based CAS client.
///
/// Stores blobs in a directory structure: `{root}/blobs/sha256-{hash}`
#[derive(Debug, Clone)]
pub struct LocalCasClient {
    #[allow(dead_code)] // Will be used for CAS maintenance operations
    root: PathBuf,
    blobs_dir: PathBuf,
}

impl LocalCasClient {
    /// Create a new LocalCasClient with the given root directory.
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let blobs_dir = root.join("blobs");
        std::fs::create_dir_all(&blobs_dir)?;

        info!("Initialized LocalCasClient at {:?}", root);
        Ok(Self { root, blobs_dir })
    }

    /// Get the path where a blob would be stored
    fn blob_path(&self, algorithm: &str, hash: &str) -> PathBuf {
        self.blobs_dir.join(format!("{}-{}", algorithm, hash))
    }

    /// Verify a file matches the expected digest
    fn verify_digest(&self, path: &Path, algorithm: &str, expected_hash: &str) -> Result<()> {
        if algorithm != "sha256" {
            return Err(CapsuleError::Config(format!(
                "Unsupported digest algorithm: {}",
                algorithm
            )));
        }

        let mut file = std::fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 8192];

        loop {
            let bytes_read = file.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer[..bytes_read]);
        }

        let actual_hash = hex::encode(hasher.finalize());
        if actual_hash != expected_hash {
            return Err(CapsuleError::HashMismatch(
                expected_hash.to_string(),
                actual_hash,
            ));
        }

        Ok(())
    }

    /// Enforce immutable permissions for CAS blobs to prevent mutation taint.
    fn enforce_blob_read_only(&self, path: &Path) -> Result<()> {
        let mut permissions = std::fs::metadata(path)?.permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(0o444);
        }
        #[cfg(not(unix))]
        {
            permissions.set_readonly(true);
        }
        std::fs::set_permissions(path, permissions)?;
        Ok(())
    }
}

#[async_trait]
impl CasClient for LocalCasClient {
    async fn fetch_blob(&self, digest: &str) -> Result<PathBuf> {
        let (algorithm, hash) = parse_digest(digest)?;
        let path = self.blob_path(algorithm, hash);

        if !path.exists() {
            return Err(CapsuleError::NotFound(digest.to_string()));
        }

        // Verify integrity
        self.verify_digest(&path, algorithm, hash)?;
        debug!("Fetched blob from local CAS: {}", digest);

        Ok(path)
    }

    async fn store_blob(&self, source_path: &Path) -> Result<String> {
        // Calculate SHA256 hash
        let mut file = std::fs::File::open(source_path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 8192];

        loop {
            let bytes_read = file.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer[..bytes_read]);
        }

        let hash = hex::encode(hasher.finalize());
        let digest = format!("sha256:{}", hash);
        let dest_path = self.blob_path("sha256", &hash);

        // Copy to CAS if not already present
        if !dest_path.exists() {
            std::fs::copy(source_path, &dest_path)?;
            self.enforce_blob_read_only(&dest_path)?;
            debug!("Stored blob in local CAS: {}", digest);
        } else {
            // Re-apply hardening in case an existing blob was created before this policy.
            self.enforce_blob_read_only(&dest_path)?;
            debug!("Blob already exists in local CAS: {}", digest);
        }

        Ok(digest)
    }

    async fn exists(&self, digest: &str) -> Result<bool> {
        let (algorithm, hash) = parse_digest(digest)?;
        let path = self.blob_path(algorithm, hash);
        Ok(path.exists())
    }
}

/// HTTP-based CAS client for remote storage.
///
/// Fetches blobs from a remote CAS server (e.g., S3-compatible endpoint).
#[derive(Debug, Clone)]
pub struct HttpCasClient {
    endpoint: String,
    client: reqwest::Client,
    cache_dir: PathBuf,
}

impl HttpCasClient {
    /// Create a new HttpCasClient.
    ///
    /// # Arguments
    /// * `endpoint` - Base URL of the CAS server (e.g., "https://cas.ato.cloud")
    /// * `cache_dir` - Local directory for caching fetched blobs
    pub fn new(endpoint: impl Into<String>, cache_dir: impl AsRef<Path>) -> Result<Self> {
        let cache_dir = cache_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&cache_dir)?;

        let endpoint = endpoint.into();
        info!("Initialized HttpCasClient with endpoint: {}", endpoint);

        Ok(Self {
            endpoint,
            client: reqwest::Client::new(),
            cache_dir,
        })
    }

    /// Get the cache path for a blob
    fn cache_path(&self, algorithm: &str, hash: &str) -> PathBuf {
        self.cache_dir.join(format!("{}-{}", algorithm, hash))
    }
}

#[async_trait]
impl CasClient for HttpCasClient {
    async fn fetch_blob(&self, digest: &str) -> Result<PathBuf> {
        let (algorithm, hash) = parse_digest(digest)?;
        let cache_path = self.cache_path(algorithm, hash);

        // Check cache first
        if cache_path.exists() {
            debug!("Blob found in cache: {}", digest);
            return Ok(cache_path);
        }

        // Fetch from remote
        let url = format!("{}/blobs/{}/{}", self.endpoint, algorithm, hash);
        debug!("Fetching blob from remote: {}", url);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(CapsuleError::Network)?;

        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(CapsuleError::AuthRequired(url));
        }
        if status == StatusCode::NOT_FOUND {
            return Err(CapsuleError::NotFound(digest.to_string()));
        }
        if !status.is_success() {
            return Err(CapsuleError::Network(
                response.error_for_status().unwrap_err(),
            ));
        }

        let bytes = response.bytes().await.map_err(CapsuleError::Network)?;

        // Verify hash
        let actual_hash = hex::encode(Sha256::digest(&bytes));
        if actual_hash != hash {
            return Err(CapsuleError::HashMismatch(hash.to_string(), actual_hash));
        }

        // Write to cache
        std::fs::write(&cache_path, &bytes)?;
        info!("Cached blob from remote CAS: {}", digest);

        Ok(cache_path)
    }

    async fn store_blob(&self, source_path: &Path) -> Result<String> {
        // Calculate hash locally first
        let content = std::fs::read(source_path)?;
        let hash = hex::encode(Sha256::digest(&content));
        let digest = format!("sha256:{}", hash);

        // Upload to remote
        let url = format!("{}/blobs/sha256/{}", self.endpoint, hash);

        let response = self
            .client
            .put(&url)
            .body(content.clone())
            .header("Content-Type", "application/octet-stream")
            .send()
            .await
            .map_err(CapsuleError::Network)?;

        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(CapsuleError::AuthRequired(url));
        }
        if status == StatusCode::NOT_FOUND {
            return Err(CapsuleError::NotFound(url));
        }
        if !status.is_success() {
            return Err(CapsuleError::Network(
                response.error_for_status().unwrap_err(),
            ));
        }

        // Also cache locally
        let cache_path = self.cache_path("sha256", &hash);
        if !cache_path.exists() {
            std::fs::write(&cache_path, &content)?;
        }

        info!("Stored blob in remote CAS: {}", digest);
        Ok(digest)
    }

    async fn exists(&self, digest: &str) -> Result<bool> {
        let (algorithm, hash) = parse_digest(digest)?;

        // Check cache first
        let cache_path = self.cache_path(algorithm, hash);
        if cache_path.exists() {
            return Ok(true);
        }

        // HEAD request to remote
        let url = format!("{}/blobs/{}/{}", self.endpoint, algorithm, hash);
        let response = self
            .client
            .head(&url)
            .send()
            .await
            .map_err(CapsuleError::Network)?;

        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(CapsuleError::AuthRequired(url));
        }
        if status == StatusCode::NOT_FOUND {
            return Ok(false);
        }

        Ok(status.is_success())
    }
}

/// Create a CAS client from configuration.
///
/// Reads from environment variables:
/// - `ATO_CAS_TYPE`: "local" or "http" (default: "local")
/// - `ATO_CAS_ENDPOINT`: HTTP endpoint for remote CAS
/// - `ATO_CAS_ROOT`: Root directory for local CAS (default: ~/.ato/cas)
#[allow(dead_code)] // Will be used when CAS integration is enabled
pub fn create_cas_client_from_env() -> Result<Box<dyn CasClient>> {
    let cas_type = std::env::var("ATO_CAS_TYPE").unwrap_or_else(|_| "local".to_string());

    match cas_type.as_str() {
        "http" => {
            let endpoint = std::env::var("ATO_CAS_ENDPOINT")
                .unwrap_or_else(|_| "https://cas.ato.cloud".to_string());
            let cache_dir = std::env::var("ATO_CAS_CACHE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    crate::common::paths::nacelle_home_dir_or_workspace_tmp().join("cas-cache")
                });
            Ok(Box::new(HttpCasClient::new(endpoint, cache_dir)?))
        }
        "local" => {
            let root = std::env::var("ATO_CAS_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    crate::common::paths::nacelle_home_dir_or_workspace_tmp().join("cas")
                });
            Ok(Box::new(LocalCasClient::new(root)?))
        }
        other => {
            // Unknown CAS type defaults to local
            tracing::warn!("Unknown CAS type '{}', defaulting to local", other);
            let root = std::env::var("ATO_CAS_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    crate::common::paths::nacelle_home_dir_or_workspace_tmp().join("cas")
                });
            Ok(Box::new(LocalCasClient::new(root)?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_local_cas_store_and_fetch() {
        let temp_dir = TempDir::new().unwrap();
        let cas = LocalCasClient::new(temp_dir.path()).unwrap();

        // Create a test file
        let test_file = temp_dir.path().join("test.txt");
        std::fs::write(&test_file, "Hello, CAS!").unwrap();

        // Store it
        let digest = cas.store_blob(&test_file).await.unwrap();
        assert!(digest.starts_with("sha256:"));

        // Fetch it back
        let fetched_path = cas.fetch_blob(&digest).await.unwrap();
        let content = std::fs::read_to_string(fetched_path).unwrap();
        assert_eq!(content, "Hello, CAS!");
    }

    #[tokio::test]
    async fn test_local_cas_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let cas = LocalCasClient::new(temp_dir.path()).unwrap();

        let result = cas
            .fetch_blob("sha256:0000000000000000000000000000000000000000000000000000000000000000")
            .await;
        assert!(matches!(result, Err(CapsuleError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_local_cas_store_sets_read_only_permissions() {
        let temp_dir = TempDir::new().unwrap();
        let cas = LocalCasClient::new(temp_dir.path()).unwrap();

        let test_file = temp_dir.path().join("test-readonly.txt");
        std::fs::write(&test_file, "readonly").unwrap();

        let digest = cas.store_blob(&test_file).await.unwrap();
        let stored = cas.fetch_blob(&digest).await.unwrap();
        let metadata = std::fs::metadata(stored).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(metadata.permissions().mode() & 0o777, 0o444);
        }
        #[cfg(not(unix))]
        {
            assert!(metadata.permissions().readonly());
        }
    }

    #[tokio::test]
    async fn test_local_cas_store_rejects_in_place_mutation() {
        let temp_dir = TempDir::new().unwrap();
        let cas = LocalCasClient::new(temp_dir.path()).unwrap();

        let test_file = temp_dir.path().join("test-taint.txt");
        std::fs::write(&test_file, "immutable").unwrap();

        let digest = cas.store_blob(&test_file).await.unwrap();
        let stored = cas.fetch_blob(&digest).await.unwrap();

        let err = std::fs::OpenOptions::new()
            .write(true)
            .open(&stored)
            .expect_err("CAS blob must not be writable");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn test_parse_digest() {
        let (algo, hash) = parse_digest("sha256:abc123").unwrap();
        assert_eq!(algo, "sha256");
        assert_eq!(hash, "abc123");

        assert!(parse_digest("invalid").is_err());
    }
}
