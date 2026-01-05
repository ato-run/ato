//! OCI Image Layer Cache
//!
//! Manages caching of downloaded image layers to avoid redundant downloads.
//! Uses content-addressable storage based on layer digests.

use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;
use tracing::{debug, info, warn};

/// Cache errors
#[derive(Error, Debug)]
pub enum CacheError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("Digest verification failed: expected {expected}, got {actual}")]
    DigestMismatch { expected: String, actual: String },

    #[error("Cache entry not found: {0}")]
    NotFound(String),
}

pub type CacheResult<T> = Result<T, CacheError>;

/// Cache metadata stored alongside each cached layer
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct CacheMetadata {
    pub digest: String,
    pub size: u64,
    pub image_ref: String,
    pub cached_at: chrono::DateTime<chrono::Utc>,
    pub last_accessed: chrono::DateTime<chrono::Utc>,
}

/// Content-addressable layer cache
pub struct LayerCache {
    cache_dir: PathBuf,
    _max_size_bytes: u64,
}

impl LayerCache {
    /// Create a new layer cache
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir,
            _max_size_bytes: 50 * 1024 * 1024 * 1024, // 50GB default
        }
    }

    /// Create a new layer cache with custom max size
    pub fn with_max_size(cache_dir: PathBuf, max_size_bytes: u64) -> Self {
        Self {
            cache_dir,
            _max_size_bytes: max_size_bytes,
        }
    }

    /// Get the path for a cached layer by digest
    pub fn layer_path(&self, digest: &str) -> PathBuf {
        // Digest format: sha256:abc123...
        // Store as: cache_dir/sha256/ab/abc123.../layer.tar.gz
        let clean_digest = digest.replace("sha256:", "");
        let prefix = &clean_digest[..2.min(clean_digest.len())];

        self.cache_dir
            .join("layers")
            .join("sha256")
            .join(prefix)
            .join(&clean_digest)
    }

    /// Check if a layer is cached
    pub fn has_layer(&self, digest: &str) -> bool {
        let path = self.layer_path(digest);
        path.join("layer.tar.gz").exists() || path.join("layer.tar").exists()
    }

    /// Get a cached layer
    pub fn get_layer(&self, digest: &str) -> CacheResult<PathBuf> {
        let base_path = self.layer_path(digest);

        // Try both compressed and uncompressed
        let gz_path = base_path.join("layer.tar.gz");
        if gz_path.exists() {
            self.update_access_time(digest)?;
            return Ok(gz_path);
        }

        let tar_path = base_path.join("layer.tar");
        if tar_path.exists() {
            self.update_access_time(digest)?;
            return Ok(tar_path);
        }

        Err(CacheError::NotFound(digest.to_string()))
    }

    /// Store a layer in the cache
    ///
    /// Uses atomic write pattern (temp file + rename) to prevent corruption
    /// from concurrent writes or interrupted operations.
    pub fn put_layer(&self, digest: &str, data: &[u8], image_ref: &str) -> CacheResult<PathBuf> {
        // Verify digest BEFORE writing anything
        let actual_digest = format!("sha256:{:x}", Sha256::digest(data));
        if actual_digest != digest {
            return Err(CacheError::DigestMismatch {
                expected: digest.to_string(),
                actual: actual_digest,
            });
        }

        let base_path = self.layer_path(digest);

        // Check if layer already exists (avoid redundant writes)
        let is_gzip = data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b;
        let filename = if is_gzip { "layer.tar.gz" } else { "layer.tar" };
        let layer_path = base_path.join(filename);

        if layer_path.exists() {
            debug!("Layer {} already cached, skipping write", digest);
            return Ok(layer_path);
        }

        fs::create_dir_all(&base_path)?;

        // Generate unique temp filename to avoid collisions
        let temp_id = uuid::Uuid::new_v4();
        let temp_layer_path = base_path.join(format!("{}.tmp.{}", filename, temp_id));
        let temp_metadata_path = base_path.join(format!("metadata.json.tmp.{}", temp_id));

        // Write layer data to temp file first
        {
            let mut file = File::create(&temp_layer_path)?;
            file.write_all(data)?;
            file.sync_all()?;
        }

        // Write metadata to temp file
        let metadata = CacheMetadata {
            digest: digest.to_string(),
            size: data.len() as u64,
            image_ref: image_ref.to_string(),
            cached_at: chrono::Utc::now(),
            last_accessed: chrono::Utc::now(),
        };

        let metadata_json = serde_json::to_string_pretty(&metadata).map_err(io::Error::other)?;
        fs::write(&temp_metadata_path, &metadata_json)?;

        // Atomic rename: temp -> final
        // On Unix, rename() is atomic if src and dst are on the same filesystem
        if let Err(e) = fs::rename(&temp_layer_path, &layer_path) {
            // Cleanup temp file on failure
            let _ = fs::remove_file(&temp_layer_path);
            let _ = fs::remove_file(&temp_metadata_path);
            return Err(e.into());
        }

        let metadata_path = base_path.join("metadata.json");
        if let Err(e) = fs::rename(&temp_metadata_path, &metadata_path) {
            // Layer was written successfully, but metadata failed
            // This is recoverable - the layer exists
            warn!("Failed to write metadata for layer {}: {}", digest, e);
        }

        info!("Cached layer {} ({} bytes)", digest, data.len());
        Ok(layer_path)
    }

    /// Update access time for LRU eviction
    fn update_access_time(&self, digest: &str) -> CacheResult<()> {
        let base_path = self.layer_path(digest);
        let metadata_path = base_path.join("metadata.json");

        if metadata_path.exists() {
            let content = fs::read_to_string(&metadata_path)?;
            if let Ok(mut metadata) = serde_json::from_str::<CacheMetadata>(&content) {
                metadata.last_accessed = chrono::Utc::now();
                let updated = serde_json::to_string_pretty(&metadata).map_err(io::Error::other)?;
                fs::write(&metadata_path, updated)?;
            }
        }

        Ok(())
    }

    /// Get total cache size in bytes
    pub fn cache_size(&self) -> CacheResult<u64> {
        let layers_dir = self.cache_dir.join("layers");
        if !layers_dir.exists() {
            return Ok(0);
        }

        Self::dir_size(&layers_dir)
    }

    fn dir_size(path: &Path) -> CacheResult<u64> {
        let mut total = 0;

        if path.is_file() {
            return Ok(fs::metadata(path)?.len());
        }

        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                total += Self::dir_size(&path)?;
            } else {
                total += fs::metadata(&path)?.len();
            }
        }

        Ok(total)
    }

    /// Evict old cache entries using LRU policy
    pub fn evict_lru(&self, target_size: u64) -> CacheResult<u64> {
        let current_size = self.cache_size()?;

        if current_size <= target_size {
            return Ok(0);
        }

        let bytes_to_free = current_size - target_size;
        let mut freed = 0u64;

        // Collect all entries with access times
        let mut entries: Vec<(PathBuf, chrono::DateTime<chrono::Utc>, u64)> = Vec::new();

        let layers_dir = self.cache_dir.join("layers").join("sha256");
        if layers_dir.exists() {
            Self::collect_entries(&layers_dir, &mut entries)?;
        }

        // Sort by last_accessed (oldest first)
        entries.sort_by_key(|(_, accessed, _)| *accessed);

        // Delete until we've freed enough
        for (path, _, size) in entries {
            if freed >= bytes_to_free {
                break;
            }

            if let Err(e) = fs::remove_dir_all(&path) {
                warn!("Failed to evict cache entry {:?}: {}", path, e);
                continue;
            }

            freed += size;
            info!("Evicted cache entry: {:?} ({} bytes)", path, size);
        }

        Ok(freed)
    }

    fn collect_entries(
        dir: &Path,
        entries: &mut Vec<(PathBuf, chrono::DateTime<chrono::Utc>, u64)>,
    ) -> CacheResult<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                // Check if this is a layer directory (has metadata.json)
                let metadata_path = path.join("metadata.json");
                if metadata_path.exists() {
                    let content = fs::read_to_string(&metadata_path)?;
                    if let Ok(metadata) = serde_json::from_str::<CacheMetadata>(&content) {
                        let size = Self::dir_size(&path)?;
                        entries.push((path, metadata.last_accessed, size));
                    }
                } else {
                    // Recurse into subdirectories
                    Self::collect_entries(&path, entries)?;
                }
            }
        }

        Ok(())
    }

    /// Clear the entire cache
    pub fn clear(&self) -> CacheResult<()> {
        let layers_dir = self.cache_dir.join("layers");
        if layers_dir.exists() {
            fs::remove_dir_all(&layers_dir)?;
        }
        info!("Cache cleared");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_cache_layer_path() {
        let temp = TempDir::new().unwrap();
        let cache = LayerCache::new(temp.path().to_path_buf());

        let path = cache.layer_path("sha256:abc123def456");
        assert!(path.to_string_lossy().contains("sha256"));
        assert!(path.to_string_lossy().contains("ab"));
        assert!(path.to_string_lossy().contains("abc123def456"));
    }

    #[test]
    fn test_cache_put_get() {
        let temp = TempDir::new().unwrap();
        let cache = LayerCache::new(temp.path().to_path_buf());

        let data = b"test layer content";
        let digest = format!("sha256:{:x}", Sha256::digest(data));

        // Put layer
        let path = cache.put_layer(&digest, data, "test:latest").unwrap();
        assert!(path.exists());

        // Check has_layer
        assert!(cache.has_layer(&digest));

        // Get layer
        let retrieved = cache.get_layer(&digest).unwrap();
        assert!(retrieved.exists());

        // Verify content
        let content = fs::read(&retrieved).unwrap();
        assert_eq!(content, data);
    }

    #[test]
    fn test_cache_miss() {
        let temp = TempDir::new().unwrap();
        let cache = LayerCache::new(temp.path().to_path_buf());

        assert!(!cache.has_layer("sha256:nonexistent"));
        assert!(cache.get_layer("sha256:nonexistent").is_err());
    }

    #[test]
    fn test_cache_digest_verification() {
        let temp = TempDir::new().unwrap();
        let cache = LayerCache::new(temp.path().to_path_buf());

        let data = b"test layer content";
        let wrong_digest =
            "sha256:0000000000000000000000000000000000000000000000000000000000000000";

        let result = cache.put_layer(wrong_digest, data, "test:latest");
        assert!(matches!(result, Err(CacheError::DigestMismatch { .. })));
    }
}
