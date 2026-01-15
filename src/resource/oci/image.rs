//! OCI Image Manager
//!
//! High-level API for pulling and preparing OCI container images.
//! Combines registry client, layer extraction, and caching.

use std::path::PathBuf;
use thiserror::Error;
use tracing::{debug, info};

use super::cache::{CacheError, LayerCache};
use super::layer::{LayerError, LayerExtractor};
use super::registry::{ImageRef, ManifestLayer, RegistryClient, RegistryError};

/// Image management errors
#[derive(Error, Debug)]
pub enum ImageError {
    #[error("Registry error: {0}")]
    Registry(#[from] RegistryError),

    #[error("Layer error: {0}")]
    Layer(#[from] LayerError),

    #[error("Cache error: {0}")]
    Cache(#[from] CacheError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type ImageResult<T> = Result<T, ImageError>;

/// Pulled image information
#[derive(Debug, Clone)]
pub struct PulledImage {
    /// Original image reference
    pub image_ref: String,
    /// Path to the extracted rootfs
    pub rootfs_path: PathBuf,
    /// Image digest
    pub digest: String,
    /// Number of layers
    pub layer_count: usize,
    /// Total uncompressed size
    pub total_size: u64,
}

/// OCI Image Manager
pub struct ImageManager {
    cache: LayerCache,
    extractor: LayerExtractor,
    rootfs_dir: PathBuf,
}

impl ImageManager {
    /// Create a new image manager
    pub fn new(cache_dir: PathBuf, rootfs_dir: PathBuf) -> Self {
        Self {
            cache: LayerCache::new(cache_dir.clone()),
            extractor: LayerExtractor::new(cache_dir),
            rootfs_dir,
        }
    }

    /// Pull an image and prepare its rootfs
    pub async fn pull_image(&self, image: &str) -> ImageResult<PulledImage> {
        info!("Pulling image: {}", image);

        // Parse image reference
        let image_ref = ImageRef::parse(image)?;

        // Create registry client and authenticate
        let mut client = RegistryClient::new();
        client.authenticate(&image_ref).await?;

        // Get manifest
        let manifest = client.get_manifest(&image_ref).await?;
        info!("Manifest has {} layers", manifest.layers.len());

        // Download layers (using cache)
        let layer_paths = self
            .download_layers(&client, &image_ref, &manifest.layers)
            .await?;

        // Prepare rootfs directory
        let rootfs_name = self.generate_rootfs_name(&manifest.config.digest);
        let rootfs_path = self.rootfs_dir.join(&rootfs_name);

        // Check if rootfs already exists
        if rootfs_path.exists() {
            info!("Rootfs already exists: {}", rootfs_path.display());
        } else {
            // Extract and merge layers
            self.extractor.merge_layers(&layer_paths, &rootfs_path)?;
        }

        let total_size: u64 = manifest.layers.iter().map(|l| l.size).sum();

        Ok(PulledImage {
            image_ref: image.to_string(),
            rootfs_path,
            digest: manifest.config.digest.clone(),
            layer_count: manifest.layers.len(),
            total_size,
        })
    }

    /// Download layers, using cache when available
    async fn download_layers(
        &self,
        client: &RegistryClient,
        image_ref: &ImageRef,
        layers: &[ManifestLayer],
    ) -> ImageResult<Vec<PathBuf>> {
        let mut layer_paths = Vec::with_capacity(layers.len());

        for (i, layer) in layers.iter().enumerate() {
            info!(
                "Processing layer {}/{}: {} ({} bytes)",
                i + 1,
                layers.len(),
                &layer.digest[..20.min(layer.digest.len())],
                layer.size
            );

            // Check cache
            if let Ok(cached_path) = self.cache.get_layer(&layer.digest) {
                debug!("Layer found in cache: {}", layer.digest);
                layer_paths.push(cached_path);
                continue;
            }

            // Download layer
            let temp_path = std::env::temp_dir()
                .join("nacelle")
                .join("downloads")
                .join(layer.digest.replace("sha256:", ""));

            client
                .download_blob(image_ref, &layer.digest, &temp_path)
                .await?;

            // Read and cache
            let data = tokio::fs::read(&temp_path).await?;
            let image_str = format!("{}:{}", image_ref.repository, image_ref.tag);
            let cached_path = self.cache.put_layer(&layer.digest, &data, &image_str)?;

            // Cleanup temp file
            let _ = tokio::fs::remove_file(&temp_path).await;

            layer_paths.push(cached_path);
        }

        Ok(layer_paths)
    }

    /// Generate a unique rootfs directory name from digest
    fn generate_rootfs_name(&self, digest: &str) -> String {
        let clean = digest.replace("sha256:", "");
        format!("rootfs_{}", &clean[..12.min(clean.len())])
    }

    /// Check if an image's rootfs is already prepared
    pub fn is_image_prepared(&self, _image: &str) -> bool {
        // This would need manifest to get digest, simplified for now
        false
    }

    /// Get rootfs path for an image if it exists
    pub fn get_rootfs(&self, digest: &str) -> Option<PathBuf> {
        let name = self.generate_rootfs_name(digest);
        let path = self.rootfs_dir.join(&name);

        if path.exists() {
            Some(path)
        } else {
            None
        }
    }

    /// Delete a rootfs
    pub fn delete_rootfs(&self, digest: &str) -> ImageResult<()> {
        let name = self.generate_rootfs_name(digest);
        let path = self.rootfs_dir.join(&name);

        if path.exists() {
            std::fs::remove_dir_all(&path)?;
            info!("Deleted rootfs: {}", path.display());
        }

        Ok(())
    }

    /// Get cache statistics
    pub fn cache_stats(&self) -> ImageResult<CacheStats> {
        let size = self.cache.cache_size()?;
        Ok(CacheStats {
            total_size_bytes: size,
        })
    }

    /// Evict old cache entries
    pub fn evict_cache(&self, target_size: u64) -> ImageResult<u64> {
        Ok(self.cache.evict_lru(target_size)?)
    }

    /// Clear all cache
    pub fn clear_cache(&self) -> ImageResult<()> {
        Ok(self.cache.clear()?)
    }
}

/// Cache statistics
#[derive(Debug)]
pub struct CacheStats {
    pub total_size_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_generate_rootfs_name() {
        let temp = TempDir::new().unwrap();
        let manager = ImageManager::new(temp.path().join("cache"), temp.path().join("rootfs"));

        let name = manager.generate_rootfs_name("sha256:abc123def456789");
        assert_eq!(name, "rootfs_abc123def456");
    }

    #[test]
    fn test_get_rootfs_nonexistent() {
        let temp = TempDir::new().unwrap();
        let manager = ImageManager::new(temp.path().join("cache"), temp.path().join("rootfs"));

        assert!(manager.get_rootfs("sha256:nonexistent").is_none());
    }
}
