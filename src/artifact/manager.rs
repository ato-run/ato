use futures::StreamExt;
use reqwest::Client;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

use crate::artifact::cache::{ArtifactCache, CachedRuntime};
use crate::artifact::registry::Registry;

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },
    #[error("Zip extraction failed: {0}")]
    ZipError(#[from] zip::result::ZipError),
    #[error("Runtime not found in registry: {0}")]
    NotFound(String),
    #[error("Invalid registry format: {0}")]
    RegistryError(String),
    #[error("CAS error: {0}")]
    CasError(String),
    #[error("Invalid URI: {0}")]
    InvalidUri(String),
}

#[derive(Clone, Debug)]
pub struct ArtifactConfig {
    pub registry_url: String,
    pub cache_path: PathBuf,
    /// Optional CAS root directory for content-addressable storage
    pub cas_root: Option<PathBuf>,
}

/// Manages runtime artifacts and versions.
#[derive(Debug)]
pub struct ArtifactManager {
    config: ArtifactConfig,
    client: Client,
    cache: ArtifactCache,
}

impl ArtifactManager {
    pub async fn new(config: ArtifactConfig) -> Result<Self, ArtifactError> {
        let cache = ArtifactCache::new(config.cache_path.clone());
        Ok(Self {
            config,
            client: Client::builder()
                .user_agent("gumball-engine/0.1.0")
                .build()?,
            cache,
        })
    }

    /// Resolve a CAS URI (cas://<hash>) to a local file path
    ///
    /// CAS URIs follow the format: cas://<sha256-hash>
    /// The blob is located at: <cas_root>/blobs/<hash[0:2]>/<hash>
    pub fn resolve_cas_uri(&self, uri: &str) -> Result<PathBuf, ArtifactError> {
        const CAS_PREFIX: &str = "cas://";

        if !uri.starts_with(CAS_PREFIX) {
            return Err(ArtifactError::InvalidUri(format!(
                "URI must start with '{}', got: {}",
                CAS_PREFIX, uri
            )));
        }

        let hash = uri.strip_prefix(CAS_PREFIX).unwrap();

        // Validate hash format (64 hex chars for SHA-256)
        if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(ArtifactError::InvalidUri(format!(
                "Invalid CAS hash (expected 64 hex chars): {}",
                hash
            )));
        }

        let cas_root = self
            .config
            .cas_root
            .as_ref()
            .ok_or_else(|| ArtifactError::CasError("CAS root not configured".to_string()))?;

        // CAS storage layout: blobs/<prefix>/<hash>
        let prefix = &hash[0..2];
        let blob_path = cas_root.join("blobs").join(prefix).join(hash);

        if !blob_path.exists() {
            return Err(ArtifactError::NotFound(format!(
                "CAS blob not found: {}",
                blob_path.display()
            )));
        }

        info!("Resolved CAS URI {} -> {}", uri, blob_path.display());
        Ok(blob_path)
    }

    /// Check if a URI is a CAS URI
    pub fn is_cas_uri(uri: &str) -> bool {
        uri.starts_with("cas://")
    }

    pub async fn ensure_runtime(
        &self,
        name: &str,
        version: &str,
        _progress_tx: Option<tokio::sync::mpsc::Sender<String>>,
    ) -> Result<PathBuf, ArtifactError> {
        let target_os = if cfg!(target_os = "macos") {
            if cfg!(target_arch = "aarch64") {
                "mac-arm64"
            } else {
                "mac-x64"
            }
        } else {
            "linux-x64" // Fallback
        };

        // Check cache
        if self.cache.exists(name, version, target_os).await {
            let path = self.cache.get_runtime_path(name, version, target_os);
            let marker = path.join(".binary_path");
            if let Ok(relative_path) = fs::read_to_string(&marker).await {
                let binary = path.join(relative_path.trim());
                if binary.exists() {
                    info!("Runtime {}@{} already installed", name, version);
                    return Ok(binary);
                }
            }
        }

        // Fetch registry
        info!("Fetching registry from {}", self.config.registry_url);
        let registry = self.fetch_registry().await.unwrap_or_else(|_| {
            warn!("Failed to fetch registry, using hardcoded fallback");
            self.get_fallback_registry()
        });

        let runtime_def = registry
            .runtimes
            .get(name)
            .ok_or_else(|| ArtifactError::NotFound(format!("Runtime {} not found", name)))?;

        let version_def = runtime_def.versions.get(version).ok_or_else(|| {
            ArtifactError::NotFound(format!("Version {} not found for {}", version, name))
        })?;

        let artifact_info = version_def.get(target_os).ok_or_else(|| {
            ArtifactError::NotFound(format!(
                "Platform {} not supported for {}@{}",
                target_os, name, version
            ))
        })?;

        let install_dir = self.cache.get_runtime_path(name, version, target_os);

        // Download and install
        self.download_and_install(
            &artifact_info.url,
            &artifact_info.sha256,
            &install_dir,
            &artifact_info.binary_path,
        )
        .await?;

        Ok(install_dir.join(&artifact_info.binary_path))
    }

    pub async fn list_cached(&self) -> Vec<CachedRuntime> {
        self.cache.list_cached().await
    }

    pub async fn clear_cache(&self, name: &str) -> Result<(), ArtifactError> {
        self.cache
            .clear_cache(name)
            .await
            .map_err(ArtifactError::Io)
    }

    async fn fetch_registry(&self) -> Result<Registry, ArtifactError> {
        if self.config.registry_url.starts_with("file://") {
            let path = self.config.registry_url.strip_prefix("file://").unwrap();
            let content = fs::read_to_string(path).await?;
            let registry: Registry = serde_json::from_str(&content).map_err(|e| {
                ArtifactError::RegistryError(format!("Failed to parse registry JSON: {}", e))
            })?;
            Ok(registry)
        } else {
            let resp = self.client.get(&self.config.registry_url).send().await?;
            let registry: Registry = resp.json().await?;
            Ok(registry)
        }
    }

    fn get_fallback_registry(&self) -> Registry {
        // Return empty registry or minimal fallback if needed, but prefer erroring if registry is missing
        // to ensure we are using the real one.
        // For now, let's keep it empty to force proper configuration.
        Registry {
            runtimes: std::collections::HashMap::new(),
        }
    }

    async fn download_and_install(
        &self,
        url: &str,
        expected_sha256: &str,
        install_dir: &Path,
        binary_rel_path: &str,
    ) -> Result<(), ArtifactError> {
        info!("Downloading {} to {:?}", url, install_dir);

        let response = self.client.get(url).send().await?;
        let mut stream = response.bytes_stream();
        let mut hasher = Sha256::new();

        let temp_dir = std::env::temp_dir().join("gumball_downloads");
        fs::create_dir_all(&temp_dir).await?;
        let temp_file_path = temp_dir.join(format!("download_{}.zip", uuid::Uuid::new_v4()));
        let mut file = fs::File::create(&temp_file_path).await?;

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result?;
            file.write_all(&chunk).await?;
            hasher.update(&chunk);
        }

        file.flush().await?;

        let hash_result = format!("{:x}", hasher.finalize());
        if expected_sha256 != "SKIP_VERIFY" && hash_result != expected_sha256 {
            return Err(ArtifactError::HashMismatch {
                expected: expected_sha256.to_string(),
                actual: hash_result,
            });
        }

        let file_std = std::fs::File::open(&temp_file_path)?;
        let mut archive = zip::ZipArchive::new(file_std)?;

        fs::create_dir_all(install_dir).await?;
        archive.extract(install_dir)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let bin_path = install_dir.join(binary_rel_path);
            if bin_path.exists() {
                let mut perms = std::fs::metadata(&bin_path)?.permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&bin_path, perms)?;
            }
        }

        fs::write(install_dir.join(".binary_path"), binary_rel_path).await?;
        fs::remove_file(temp_file_path).await?;

        Ok(())
    }
}
