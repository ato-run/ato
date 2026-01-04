//! Unified Storage Manager for Capsule workloads
//!
//! Combines LVM volume management and LUKS encryption into a single
//! interface for provisioning and managing capsule storage.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;
use tracing::{debug, error, info, warn};

use crate::storage::error::{StorageError, StorageResult};
use crate::storage::luks::{KeyStorage, LuksManager};
use crate::storage::lvm::LvmManager;

/// Information about provisioned capsule storage
#[derive(Debug, Clone)]
pub struct CapsuleStorage {
    /// Capsule ID this storage belongs to
    pub capsule_id: String,
    /// Volume group name
    pub vg_name: String,
    /// Logical volume name
    pub lv_name: String,
    /// Size in bytes
    pub size_bytes: u64,
    /// Path to the device (raw or encrypted mapper)
    pub device_path: String,
    /// Whether the storage is encrypted
    pub encrypted: bool,
    /// Mount point (if mounted)
    pub mount_point: Option<PathBuf>,
}

/// Configuration for the StorageManager
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// Whether storage is enabled
    #[serde(default)]
    pub enabled: bool,
    /// Default volume group to use
    pub default_vg: String,
    /// Directory for storing encryption keys
    pub key_directory: PathBuf,
    /// Whether to enable encryption by default
    pub enable_encryption: bool,
    /// Default volume size in bytes (for capsules that don't specify)
    pub default_size_bytes: u64,
    /// Mount point base directory
    pub mount_base: PathBuf,
    /// Thin pool name for thin provisioning (must exist in default_vg)
    /// If None, thin provisioning is disabled
    pub thin_pool_name: Option<String>,
    /// Whether to use thin provisioning by default
    #[serde(default)]
    pub use_thin_by_default: bool,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            default_vg: "vg_capsules".to_string(),
            key_directory: PathBuf::from("/etc/capsuled/keys"),
            enable_encryption: true,
            default_size_bytes: 10 * 1024 * 1024 * 1024, // 10GB
            mount_base: PathBuf::from("/var/lib/capsuled/mounts"),
            thin_pool_name: None, // Thin provisioning disabled by default
            use_thin_by_default: false,
        }
    }
}

/// Unified storage manager combining LVM and LUKS operations
pub struct StorageManager {
    lvm: LvmManager,
    luks: LuksManager,
    config: StorageConfig,
}

impl StorageManager {
    /// Create a new StorageManager with the given configuration
    pub fn new(config: StorageConfig) -> Self {
        let lvm = LvmManager::new(config.default_vg.clone());
        let luks = LuksManager::new(config.key_directory.clone());

        Self { lvm, luks, config }
    }

    /// Create a StorageManager with default configuration
    pub fn with_defaults() -> Self {
        Self::new(StorageConfig::default())
    }

    /// Provision storage for a capsule
    ///
    /// This creates an LVM volume (thick or thin) and optionally encrypts it with LUKS.
    ///
    /// # Arguments
    /// * `capsule_id` - Unique identifier for the capsule
    /// * `size_bytes` - Size of storage to provision (uses default if None)
    /// * `encrypt` - Whether to encrypt (uses config default if None)
    /// * `use_thin` - Whether to use thin provisioning (uses config default if None)
    ///
    /// # Returns
    /// CapsuleStorage with provisioned storage information
    pub fn provision_capsule_storage(
        &self,
        capsule_id: &str,
        size_bytes: Option<u64>,
        encrypt: Option<bool>,
        use_thin: Option<bool>,
    ) -> StorageResult<CapsuleStorage> {
        let size = size_bytes.unwrap_or(self.config.default_size_bytes);
        let should_encrypt = encrypt.unwrap_or(self.config.enable_encryption);
        let should_use_thin = use_thin.unwrap_or(self.config.use_thin_by_default);

        // Generate LV name from capsule ID (sanitized)
        let lv_name = Self::sanitize_lv_name(capsule_id);

        info!(
            capsule_id = capsule_id,
            lv_name = %lv_name,
            size_bytes = size,
            encrypted = should_encrypt,
            thin = should_use_thin,
            "Provisioning capsule storage"
        );

        // Step 1: Create LVM volume (thin or thick)
        let volume = if should_use_thin {
            self.create_thin_volume_for_capsule(&lv_name, size)?
        } else {
            self.lvm.create_volume(&lv_name, size, None)?
        };
        debug!(
            "Created LVM volume: {} (thin: {})",
            volume.device_path, volume.is_thin
        );

        // Step 2: Encrypt if required
        let (device_path, encrypted) = if should_encrypt {
            match self.encrypt_volume(&volume.device_path, &lv_name) {
                Ok(mapper_path) => (mapper_path, true),
                Err(e) => {
                    // Cleanup LVM volume on encryption failure
                    error!("Encryption failed, cleaning up LVM volume: {}", e);
                    let _ = self.lvm.delete_volume(&lv_name, None);
                    return Err(e);
                }
            }
        } else {
            (volume.device_path.clone(), false)
        };

        info!(
            capsule_id = capsule_id,
            device_path = %device_path,
            thin = volume.is_thin,
            "Capsule storage provisioned successfully"
        );

        Ok(CapsuleStorage {
            capsule_id: capsule_id.to_string(),
            vg_name: volume.vg_name,
            lv_name,
            size_bytes: size,
            device_path,
            encrypted,
            mount_point: None,
        })
    }

    /// Create a thin volume for a capsule using the configured thin pool
    fn create_thin_volume_for_capsule(
        &self,
        lv_name: &str,
        size_bytes: u64,
    ) -> StorageResult<crate::storage::lvm::VolumeInfo> {
        let pool_name = self.config.thin_pool_name.as_ref().ok_or_else(|| {
            StorageError::CommandFailed(
                "Thin provisioning requested but no thin_pool_name configured".to_string(),
            )
        })?;

        info!(
            lv_name = %lv_name,
            pool_name = %pool_name,
            size_bytes = size_bytes,
            "Creating thin volume from pool"
        );

        self.lvm
            .create_thin_volume(lv_name, size_bytes, pool_name, None)
    }

    /// Mount a provisioned volume
    ///
    /// This formats the volume (if needed) and mounts it to the target path.
    pub fn mount_volume(&self, storage: &mut CapsuleStorage) -> StorageResult<()> {
        let mount_point = self
            .config
            .mount_base
            .join(&storage.capsule_id)
            .join(&storage.lv_name);

        // Ensure mount point exists
        if !mount_point.exists() {
            std::fs::create_dir_all(&mount_point).map_err(|e| {
                StorageError::CommandFailed(format!("Failed to create mount point: {}", e))
            })?;
        }

        let device_path = &storage.device_path;

        // Check if formatted
        if !self.is_formatted(device_path)? {
            info!("Formatting volume {} as ext4", device_path);
            let output = Command::new("mkfs.ext4")
                .arg("-F") // Force
                .arg(device_path)
                .output()
                .map_err(|e| {
                    StorageError::CommandFailed(format!("Failed to execute mkfs.ext4: {}", e))
                })?;

            if !output.status.success() {
                return Err(StorageError::CommandFailed(format!(
                    "mkfs.ext4 failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                )));
            }
        }

        // Mount
        info!("Mounting {} to {}", device_path, mount_point.display());
        let output = Command::new("mount")
            .arg(device_path)
            .arg(&mount_point)
            .output()
            .map_err(|e| StorageError::CommandFailed(format!("Failed to execute mount: {}", e)))?;

        if !output.status.success() {
            return Err(StorageError::CommandFailed(format!(
                "mount failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        storage.mount_point = Some(mount_point);
        Ok(())
    }

    /// Unmount a volume
    pub fn unmount_volume(&self, capsule_id: &str, lv_name: &str) -> StorageResult<()> {
        // Construct path identically to mount_volume
        // Note: Ideally we track active mounts, but stateless constraints imply deriving it.
        // We use sanitize_lv_name just in case caller passes raw name, but we expect sanitized or original?
        // Let's assume passed valid lv_name.
        // Better: We should probably list mounts or try to unmount the expected path.
        let mount_point = self.config.mount_base.join(capsule_id).join(lv_name);

        if mount_point.exists() {
            // Check if mounted? Or just try unmount.
            info!("Unmounting {}", mount_point.display());
            let output = Command::new("umount")
                .arg(&mount_point)
                .output()
                .map_err(|e| {
                    StorageError::CommandFailed(format!("Failed to execute umount: {}", e))
                })?;

            // Ignore "not mounted" errors if we are just cleaning up
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.contains("not mounted") {
                    return Err(StorageError::CommandFailed(format!(
                        "umount failed: {}",
                        stderr
                    )));
                }
            }

            // Remove mount point dir
            let _ = std::fs::remove_dir(&mount_point);
        }
        Ok(())
    }

    fn is_formatted(&self, device_path: &str) -> StorageResult<bool> {
        // Use blkid to check for filesystem
        let output = Command::new("blkid")
            .arg(device_path)
            .output()
            .map_err(|e| StorageError::CommandFailed(format!("Failed to execute blkid: {}", e)))?;

        // If blkid returns successfully and output contains TYPE=, it's formatted.
        // If it returns exit code 2, it's unformatted/unknown.
        Ok(output.status.success())
    }

    ///
    /// This locks any LUKS encryption and deletes the LVM volume.
    ///
    /// # Arguments
    /// * `capsule_id` - Unique identifier for the capsule
    pub fn cleanup_capsule_storage(&self, capsule_id: &str) -> StorageResult<()> {
        let lv_name = Self::sanitize_lv_name(capsule_id);
        let mapper_name = format!("capsule_{}", lv_name);

        info!(capsule_id = capsule_id, "Cleaning up capsule storage");

        // Step 1: Lock LUKS volume if it exists
        let mapper_path = format!("/dev/mapper/{}", mapper_name);
        if std::path::Path::new(&mapper_path).exists() {
            debug!("Locking LUKS volume: {}", mapper_name);
            match self.luks.lock_volume(&mapper_name) {
                Ok(_) => debug!("LUKS volume locked"),
                Err(e) => warn!("Failed to lock LUKS volume (may already be locked): {}", e),
            }
        }

        // Step 2: Delete LVM volume
        debug!("Deleting LVM volume: {}", lv_name);
        match self.lvm.delete_volume(&lv_name, None) {
            Ok(_) => {
                info!(
                    capsule_id = capsule_id,
                    "Capsule storage cleaned up successfully"
                );
                Ok(())
            }
            Err(e) => {
                // Volume might not exist (already cleaned up)
                if matches!(e, StorageError::VolumeNotFound(_)) {
                    warn!("Volume not found during cleanup (may already be deleted)");
                    Ok(())
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Check if storage exists for a capsule
    pub fn storage_exists(&self, capsule_id: &str) -> StorageResult<bool> {
        let lv_name = Self::sanitize_lv_name(capsule_id);
        let volumes = self.lvm.list_volumes(None)?;
        Ok(volumes.iter().any(|v| v.lv_name == lv_name))
    }

    /// Get storage information for a capsule
    pub fn get_capsule_storage(&self, capsule_id: &str) -> StorageResult<Option<CapsuleStorage>> {
        let lv_name = Self::sanitize_lv_name(capsule_id);
        let volumes = self.lvm.list_volumes(None)?;

        if let Some(volume) = volumes.iter().find(|v| v.lv_name == lv_name) {
            let mapper_name = format!("capsule_{}", lv_name);
            let mapper_path = format!("/dev/mapper/{}", mapper_name);
            let encrypted = std::path::Path::new(&mapper_path).exists();

            let device_path = if encrypted {
                mapper_path
            } else {
                volume.device_path.clone()
            };

            Ok(Some(CapsuleStorage {
                capsule_id: capsule_id.to_string(),
                vg_name: volume.vg_name.clone(),
                lv_name: volume.lv_name.clone(),
                size_bytes: volume.size_bytes,
                device_path,
                encrypted,
                mount_point: None,
            }))
        } else {
            Ok(None)
        }
    }

    /// Encrypt a volume with LUKS
    fn encrypt_volume(&self, device_path: &str, lv_name: &str) -> StorageResult<String> {
        let mapper_name = format!("capsule_{}", lv_name);

        // Generate encryption key
        let key = self.luks.generate_key(64); // 512-bit key

        // Store key securely
        let key_path = self.luks.store_key(&mapper_name, &key)?;
        debug!("Stored encryption key: {:?}", key_path);

        // Create encrypted volume
        self.luks
            .create_encrypted_volume(device_path, KeyStorage::File(key_path.clone()))?;

        // Unlock the volume
        let info =
            self.luks
                .unlock_volume(device_path, &mapper_name, KeyStorage::File(key_path))?;

        Ok(info.mapper_path)
    }

    /// Sanitize capsule ID to a valid LVM volume name
    fn sanitize_lv_name(capsule_id: &str) -> String {
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

        // Ensure it doesn't start with a hyphen
        if sanitized.starts_with('-') {
            format!("lv_{}", sanitized)
        } else {
            sanitized
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_lv_name() {
        assert_eq!(StorageManager::sanitize_lv_name("my-capsule"), "my-capsule");
        assert_eq!(StorageManager::sanitize_lv_name("my_capsule"), "my_capsule");
        assert_eq!(StorageManager::sanitize_lv_name("my capsule"), "my_capsule");
        assert_eq!(StorageManager::sanitize_lv_name("my.capsule"), "my_capsule");
        assert_eq!(StorageManager::sanitize_lv_name("-capsule"), "lv_-capsule");
    }

    #[test]
    fn test_storage_config_default() {
        let config = StorageConfig::default();
        assert_eq!(config.default_vg, "vg_capsules");
        assert!(config.enable_encryption);
        assert_eq!(config.default_size_bytes, 10 * 1024 * 1024 * 1024);
    }

    #[test]
    fn test_capsule_storage_struct() {
        let storage = CapsuleStorage {
            capsule_id: "test-capsule".to_string(),
            vg_name: "vg_capsules".to_string(),
            lv_name: "test-capsule".to_string(),
            size_bytes: 1024 * 1024 * 1024,
            device_path: "/dev/mapper/capsule_test-capsule".to_string(),
            encrypted: true,
            mount_point: None,
        };

        assert_eq!(storage.capsule_id, "test-capsule");
        assert!(storage.encrypted);
        assert!(storage.mount_point.is_none());
    }
}
