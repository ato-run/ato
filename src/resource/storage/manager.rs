//! Directory-Based Storage Manager for Capsule workloads (SPEC V1.1.0)
//!
//! This module provides simple directory-based storage for capsules.
//! LVM/LUKS-based storage has been removed as per SPEC V1.1.0:
//! - Engine should be stateless
//! - Complex block device management is delegated to OS or Coordinator
//!
//! This implementation:
//! - Creates directories for capsule storage (instead of LVM volumes)
//! - Uses host filesystem directly
//! - Is cross-platform compatible (macOS, Linux, Windows)

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::{debug, info, warn};

use crate::storage::error::{StorageError, StorageResult};

/// Information about provisioned capsule storage
#[derive(Debug, Clone)]
pub struct CapsuleStorage {
    /// Capsule ID this storage belongs to
    pub capsule_id: String,
    /// Storage directory path
    pub storage_path: PathBuf,
    /// Size limit in bytes (soft limit, advisory only)
    pub size_limit_bytes: u64,
    /// Mount point (same as storage_path for directory-based storage)
    pub mount_point: Option<PathBuf>,
}

/// Configuration for the StorageManager
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// Whether storage is enabled
    #[serde(default)]
    pub enabled: bool,
    /// Base directory for capsule storage
    pub storage_base: PathBuf,
    /// Default storage size limit in bytes (advisory)
    pub default_size_bytes: u64,
    /// Default volume group name (kept for backward compatibility, but unused)
    #[serde(default = "default_vg")]
    pub default_vg: String,
}

fn default_vg() -> String {
    "unused".to_string()
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            storage_base: PathBuf::from("/var/lib/capsuled/storage"),
            default_size_bytes: 10 * 1024 * 1024 * 1024, // 10GB
            default_vg: default_vg(),
        }
    }
}

/// Directory-based storage manager for capsule workloads
///
/// This is a simple, cross-platform implementation that creates
/// directories for each capsule instead of managing LVM volumes.
pub struct StorageManager {
    config: StorageConfig,
}

impl StorageManager {
    /// Create a new StorageManager with the given configuration
    pub fn new(config: StorageConfig) -> Self {
        // Ensure base directory exists
        if config.enabled {
            if let Err(e) = std::fs::create_dir_all(&config.storage_base) {
                warn!(
                    "Failed to create storage base directory {:?}: {}",
                    config.storage_base, e
                );
            }
        }
        Self { config }
    }

    /// Create a StorageManager with default configuration
    pub fn with_defaults() -> Self {
        Self::new(StorageConfig::default())
    }

    /// Provision storage for a capsule
    ///
    /// Creates a directory for the capsule to use as its working storage.
    ///
    /// # Arguments
    /// * `capsule_id` - Unique identifier for the capsule
    /// * `size_bytes` - Size limit in bytes (advisory, not enforced)
    ///
    /// # Returns
    /// CapsuleStorage with provisioned storage information
    pub fn provision_capsule_storage(
        &self,
        capsule_id: &str,
        size_bytes: Option<u64>,
        _encrypt: Option<bool>,  // Ignored - no encryption in directory mode
        _use_thin: Option<bool>, // Ignored - no thin provisioning in directory mode
    ) -> StorageResult<CapsuleStorage> {
        let size = size_bytes.unwrap_or(self.config.default_size_bytes);
        let dir_name = Self::sanitize_dir_name(capsule_id);
        let storage_path = self.config.storage_base.join(&dir_name);

        info!(
            capsule_id = capsule_id,
            path = %storage_path.display(),
            size_limit_bytes = size,
            "Provisioning directory-based capsule storage"
        );

        // Create directory
        std::fs::create_dir_all(&storage_path).map_err(|e| {
            StorageError::CommandFailed(format!(
                "Failed to create storage directory {:?}: {}",
                storage_path, e
            ))
        })?;

        debug!("Created storage directory: {:?}", storage_path);

        Ok(CapsuleStorage {
            capsule_id: capsule_id.to_string(),
            storage_path: storage_path.clone(),
            size_limit_bytes: size,
            mount_point: Some(storage_path),
        })
    }

    /// Get storage information for a capsule
    pub fn get_capsule_storage(&self, capsule_id: &str) -> StorageResult<Option<CapsuleStorage>> {
        let dir_name = Self::sanitize_dir_name(capsule_id);
        let storage_path = self.config.storage_base.join(&dir_name);

        if storage_path.exists() {
            Ok(Some(CapsuleStorage {
                capsule_id: capsule_id.to_string(),
                storage_path: storage_path.clone(),
                size_limit_bytes: self.config.default_size_bytes,
                mount_point: Some(storage_path),
            }))
        } else {
            Ok(None)
        }
    }

    /// Check if storage exists for a capsule
    pub fn storage_exists(&self, capsule_id: &str) -> StorageResult<bool> {
        let dir_name = Self::sanitize_dir_name(capsule_id);
        let storage_path = self.config.storage_base.join(&dir_name);
        Ok(storage_path.exists())
    }

    /// Cleanup storage for a capsule
    ///
    /// Removes the capsule's storage directory and all its contents.
    pub fn cleanup_capsule_storage(&self, capsule_id: &str) -> StorageResult<()> {
        let dir_name = Self::sanitize_dir_name(capsule_id);
        let storage_path = self.config.storage_base.join(&dir_name);

        info!(capsule_id = capsule_id, "Cleaning up capsule storage");

        if storage_path.exists() {
            std::fs::remove_dir_all(&storage_path).map_err(|e| {
                StorageError::CommandFailed(format!(
                    "Failed to remove storage directory {:?}: {}",
                    storage_path, e
                ))
            })?;
            info!(
                capsule_id = capsule_id,
                "Capsule storage cleaned up successfully"
            );
        } else {
            debug!(
                capsule_id = capsule_id,
                "Storage directory doesn't exist, nothing to clean"
            );
        }

        Ok(())
    }

    /// Get the storage path for a capsule (for OCI bundle creation, etc.)
    pub fn get_storage_path(&self, capsule_id: &str) -> PathBuf {
        let dir_name = Self::sanitize_dir_name(capsule_id);
        self.config.storage_base.join(&dir_name)
    }

    /// Calculate actual directory size (sum of all files)
    pub fn get_used_bytes(&self, capsule_id: &str) -> StorageResult<u64> {
        let storage_path = self.get_storage_path(capsule_id);
        if !storage_path.exists() {
            return Ok(0);
        }

        Self::calculate_dir_size(&storage_path)
    }

    /// Recursively calculate directory size
    fn calculate_dir_size(path: &PathBuf) -> StorageResult<u64> {
        let mut total = 0u64;

        let entries = std::fs::read_dir(path).map_err(|e| {
            StorageError::CommandFailed(format!("Failed to read directory {:?}: {}", path, e))
        })?;

        for entry in entries {
            let entry = entry.map_err(|e| {
                StorageError::CommandFailed(format!("Failed to read directory entry: {}", e))
            })?;

            let metadata = entry.metadata().map_err(|e| {
                StorageError::CommandFailed(format!("Failed to read metadata: {}", e))
            })?;

            if metadata.is_dir() {
                total += Self::calculate_dir_size(&entry.path())?;
            } else {
                total += metadata.len();
            }
        }

        Ok(total)
    }

    /// Sanitize capsule ID to a valid directory name
    fn sanitize_dir_name(capsule_id: &str) -> String {
        let sanitized: String = capsule_id
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();

        // Ensure it doesn't start with a dot or hyphen
        if sanitized.starts_with('.') || sanitized.starts_with('-') {
            format!("capsule_{}", sanitized)
        } else if sanitized.is_empty() {
            "capsule_unnamed".to_string()
        } else {
            sanitized
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_sanitize_dir_name() {
        assert_eq!(
            StorageManager::sanitize_dir_name("my-capsule"),
            "my-capsule"
        );
        assert_eq!(
            StorageManager::sanitize_dir_name("my_capsule"),
            "my_capsule"
        );
        assert_eq!(
            StorageManager::sanitize_dir_name("my capsule"),
            "my_capsule"
        );
        assert_eq!(
            StorageManager::sanitize_dir_name("my.capsule"),
            "my_capsule"
        );
        assert_eq!(
            StorageManager::sanitize_dir_name("-capsule"),
            "capsule_-capsule"
        );
        // ".hidden" becomes "_hidden" after sanitization, then gets prefix because starts with underscore? No - starts with '.'
        // Actually the sanitization replaces '.' with '_', so ".hidden" -> "_hidden"
        // Then "_hidden" does NOT start with '.' or '-', so no prefix is added
        assert_eq!(StorageManager::sanitize_dir_name(".hidden"), "_hidden");
        assert_eq!(StorageManager::sanitize_dir_name(""), "capsule_unnamed");
    }

    #[test]
    fn test_storage_config_default() {
        let config = StorageConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.default_size_bytes, 10 * 1024 * 1024 * 1024);
    }

    #[test]
    fn test_provision_and_cleanup_storage() {
        let temp_dir = TempDir::new().unwrap();
        let config = StorageConfig {
            enabled: true,
            storage_base: temp_dir.path().to_path_buf(),
            default_size_bytes: 1024 * 1024,
            default_vg: "unused".to_string(),
        };

        let manager = StorageManager::new(config);

        // Provision
        let storage = manager
            .provision_capsule_storage("test-capsule", None, None, None)
            .unwrap();
        assert_eq!(storage.capsule_id, "test-capsule");
        assert!(storage.storage_path.exists());

        // Check exists
        assert!(manager.storage_exists("test-capsule").unwrap());

        // Get storage
        let retrieved = manager.get_capsule_storage("test-capsule").unwrap();
        assert!(retrieved.is_some());

        // Cleanup
        manager.cleanup_capsule_storage("test-capsule").unwrap();
        assert!(!manager.storage_exists("test-capsule").unwrap());
    }

    #[test]
    fn test_calculate_dir_size() {
        let temp_dir = TempDir::new().unwrap();
        let config = StorageConfig {
            enabled: true,
            storage_base: temp_dir.path().to_path_buf(),
            default_size_bytes: 1024 * 1024,
            default_vg: "unused".to_string(),
        };

        let manager = StorageManager::new(config);

        // Provision storage
        let storage = manager
            .provision_capsule_storage("size-test", None, None, None)
            .unwrap();

        // Create a test file
        let test_file = storage.storage_path.join("test.txt");
        std::fs::write(&test_file, "Hello, World!").unwrap();

        // Check size
        let used = manager.get_used_bytes("size-test").unwrap();
        assert_eq!(used, 13); // "Hello, World!" is 13 bytes
    }
}
