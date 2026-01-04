use crate::storage::error::{StorageError, StorageResult};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{debug, error, info, warn};

/// Information about an encrypted volume
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EncryptedVolumeInfo {
    /// Name of the LUKS device
    pub device_name: String,
    /// Path to the underlying block device (e.g., /dev/vg_data/lv_encrypted)
    pub device_path: String,
    /// Path to the mapped device (e.g., /dev/mapper/encrypted_vol)
    pub mapper_path: String,
    /// Whether the volume is currently unlocked (open)
    pub is_unlocked: bool,
    /// LUKS version (e.g., "2")
    pub luks_version: String,
}

/// Key storage strategy for LUKS encryption
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KeyStorage {
    /// Key stored in a file at the specified path
    File(PathBuf),
    /// Key provided directly as bytes (in memory)
    Memory(Vec<u8>),
}

/// LUKS (Linux Unified Key Setup) encryption manager
///
/// Provides functionality for creating, unlocking, and managing LUKS encrypted volumes.
/// All operations use standard cryptsetup command-line tools without any CGO dependencies.
pub struct LuksManager {
    /// Directory where key files are stored by default
    key_directory: PathBuf,
}

impl LuksManager {
    /// Creates a new LUKS manager with the specified key storage directory.
    ///
    /// # Arguments
    /// * `key_directory` - Directory path where encryption keys will be stored
    ///
    /// # Example
    /// ```
    /// use capsuled_engine::storage::LuksManager;
    /// use std::path::PathBuf;
    /// let manager = LuksManager::new(PathBuf::from("/etc/capsuled/keys"));
    /// ```
    pub fn new(key_directory: PathBuf) -> Self {
        Self { key_directory }
    }

    /// Creates a new LUKS encrypted volume on the specified device.
    ///
    /// # Arguments
    /// * `device_path` - Path to the block device (e.g., /dev/vg_data/lv_encrypted)
    /// * `key_storage` - Key storage strategy (file or memory)
    ///
    /// # Returns
    /// EncryptedVolumeInfo with the encrypted volume information
    ///
    /// # Errors
    /// Returns StorageError if:
    /// - The device path is invalid
    /// - The device is already encrypted
    /// - The cryptsetup command fails
    /// - Key management fails
    ///
    /// # Example
    /// ```no_run
    /// use capsuled_engine::storage::{LuksManager, KeyStorage};
    /// use std::path::PathBuf;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let manager = LuksManager::new(PathBuf::from("/etc/capsuled/keys"));
    /// let key = vec![0u8; 32]; // 32-byte key
    /// let volume = manager.create_encrypted_volume(
    ///     "/dev/vg_data/lv_encrypted",
    ///     KeyStorage::Memory(key)
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn create_encrypted_volume(
        &self,
        device_path: &str,
        key_storage: KeyStorage,
    ) -> StorageResult<EncryptedVolumeInfo> {
        // Validate device path
        let device = Path::new(device_path);
        if !device.exists() {
            return Err(StorageError::VolumeNotFound(device_path.to_string()));
        }

        info!("Creating LUKS encrypted volume on {}", device_path);

        // Check if device is already a LUKS volume
        if self.is_luks_device(device_path)? {
            return Err(StorageError::VolumeAlreadyExists(format!(
                "Device {} is already a LUKS volume",
                device_path
            )));
        }

        // Prepare key for cryptsetup
        let key_file = match &key_storage {
            KeyStorage::File(path) => {
                if !path.exists() {
                    return Err(StorageError::KeyManagementError(format!(
                        "Key file not found: {}",
                        path.display()
                    )));
                }
                path.clone()
            }
            KeyStorage::Memory(key_data) => {
                // Write key to a temporary file
                let temp_key_file = self
                    .key_directory
                    .join(format!(".tmp_key_{}", uuid::Uuid::new_v4()));
                fs::write(&temp_key_file, key_data).map_err(|e| {
                    StorageError::KeyManagementError(format!(
                        "Failed to write temporary key: {}",
                        e
                    ))
                })?;

                // Set restrictive permissions (600)
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let perms = std::fs::Permissions::from_mode(0o600);
                    fs::set_permissions(&temp_key_file, perms).map_err(|e| {
                        StorageError::KeyManagementError(format!(
                            "Failed to set key permissions: {}",
                            e
                        ))
                    })?;
                }

                temp_key_file
            }
        };

        // Execute cryptsetup luksFormat command
        let output = Command::new("cryptsetup")
            .arg("luksFormat")
            .arg("--type")
            .arg("luks2")
            .arg("--key-file")
            .arg(&key_file)
            .arg("--batch-mode") // Don't ask for confirmation
            .arg(device_path)
            .output()
            .map_err(|e| {
                StorageError::EncryptionError(format!("Failed to execute cryptsetup: {}", e))
            })?;

        // Clean up temporary key file if created
        if matches!(key_storage, KeyStorage::Memory(_)) {
            let _ = fs::remove_file(&key_file);
        }

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("cryptsetup luksFormat failed: {}", stderr);

            if stderr.contains("Permission denied") {
                return Err(StorageError::PermissionDenied(
                    "cryptsetup requires root privileges".to_string(),
                ));
            }

            return Err(StorageError::EncryptionError(format!(
                "luksFormat command failed: {}",
                stderr
            )));
        }

        debug!("LUKS encrypted volume created successfully");

        // Extract device name from path
        let device_name = device
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        Ok(EncryptedVolumeInfo {
            device_name: device_name.clone(),
            device_path: device_path.to_string(),
            mapper_path: format!("/dev/mapper/{}", device_name),
            is_unlocked: false,
            luks_version: "2".to_string(),
        })
    }

    /// Unlocks (opens) a LUKS encrypted volume.
    ///
    /// # Arguments
    /// * `device_path` - Path to the encrypted block device
    /// * `mapper_name` - Name for the mapped device (will appear in /dev/mapper/)
    /// * `key_storage` - Key storage strategy
    ///
    /// # Returns
    /// EncryptedVolumeInfo with the unlocked volume information
    ///
    /// # Errors
    /// Returns StorageError if:
    /// - The device is not a LUKS volume
    /// - The key is incorrect
    /// - The device is already unlocked
    /// - The cryptsetup command fails
    ///
    /// # Example
    /// ```no_run
    /// use capsuled_engine::storage::{LuksManager, KeyStorage};
    /// use std::path::PathBuf;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let manager = LuksManager::new(PathBuf::from("/etc/capsuled/keys"));
    /// let key = vec![0u8; 32];
    /// let volume = manager.unlock_volume(
    ///     "/dev/vg_data/lv_encrypted",
    ///     "encrypted_vol",
    ///     KeyStorage::Memory(key)
    /// )?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn unlock_volume(
        &self,
        device_path: &str,
        mapper_name: &str,
        key_storage: KeyStorage,
    ) -> StorageResult<EncryptedVolumeInfo> {
        // Validate device exists and is LUKS
        if !Path::new(device_path).exists() {
            return Err(StorageError::VolumeNotFound(device_path.to_string()));
        }

        if !self.is_luks_device(device_path)? {
            return Err(StorageError::EncryptionError(format!(
                "Device {} is not a LUKS volume",
                device_path
            )));
        }

        // Check if already unlocked
        let mapper_path = format!("/dev/mapper/{}", mapper_name);
        if Path::new(&mapper_path).exists() {
            warn!("Volume already unlocked: {}", mapper_path);
            return self.get_volume_info(device_path, mapper_name);
        }

        info!("Unlocking LUKS volume: {} -> {}", device_path, mapper_name);

        // Prepare key
        let key_file = match &key_storage {
            KeyStorage::File(path) => path.clone(),
            KeyStorage::Memory(key_data) => {
                let temp_key_file = self
                    .key_directory
                    .join(format!(".tmp_key_{}", uuid::Uuid::new_v4()));
                fs::write(&temp_key_file, key_data).map_err(|e| {
                    StorageError::KeyManagementError(format!(
                        "Failed to write temporary key: {}",
                        e
                    ))
                })?;

                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let perms = std::fs::Permissions::from_mode(0o600);
                    fs::set_permissions(&temp_key_file, perms).map_err(|e| {
                        StorageError::KeyManagementError(format!(
                            "Failed to set key permissions: {}",
                            e
                        ))
                    })?;
                }

                temp_key_file
            }
        };

        // Execute cryptsetup luksOpen command
        let output = Command::new("cryptsetup")
            .arg("luksOpen")
            .arg("--key-file")
            .arg(&key_file)
            .arg(device_path)
            .arg(mapper_name)
            .output()
            .map_err(|e| {
                StorageError::EncryptionError(format!("Failed to execute cryptsetup: {}", e))
            })?;

        // Clean up temporary key file
        if matches!(key_storage, KeyStorage::Memory(_)) {
            let _ = fs::remove_file(&key_file);
        }

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("cryptsetup luksOpen failed: {}", stderr);

            if stderr.contains("No key available") || stderr.contains("incorrect passphrase") {
                return Err(StorageError::KeyManagementError(
                    "Incorrect key or passphrase".to_string(),
                ));
            }

            if stderr.contains("Permission denied") {
                return Err(StorageError::PermissionDenied(
                    "cryptsetup requires root privileges".to_string(),
                ));
            }

            return Err(StorageError::EncryptionError(format!(
                "luksOpen command failed: {}",
                stderr
            )));
        }

        debug!("LUKS volume unlocked successfully");

        self.get_volume_info(device_path, mapper_name)
    }

    /// Locks (closes) a LUKS encrypted volume.
    ///
    /// # Arguments
    /// * `mapper_name` - Name of the mapped device (from /dev/mapper/)
    ///
    /// # Errors
    /// Returns StorageError if:
    /// - The mapped device is not found
    /// - The device is busy
    /// - The cryptsetup command fails
    ///
    /// # Example
    /// ```no_run
    /// use capsuled_engine::storage::LuksManager;
    /// use std::path::PathBuf;
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let manager = LuksManager::new(PathBuf::from("/etc/capsuled/keys"));
    /// manager.lock_volume("encrypted_vol")?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn lock_volume(&self, mapper_name: &str) -> StorageResult<()> {
        let mapper_path = format!("/dev/mapper/{}", mapper_name);

        if !Path::new(&mapper_path).exists() {
            return Err(StorageError::VolumeNotFound(mapper_path));
        }

        info!("Locking LUKS volume: {}", mapper_name);

        // Execute cryptsetup luksClose command
        let output = Command::new("cryptsetup")
            .arg("luksClose")
            .arg(mapper_name)
            .output()
            .map_err(|e| {
                StorageError::EncryptionError(format!("Failed to execute cryptsetup: {}", e))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("cryptsetup luksClose failed: {}", stderr);

            if stderr.contains("busy") || stderr.contains("in use") {
                return Err(StorageError::VolumeBusy(mapper_name.to_string()));
            }

            if stderr.contains("Permission denied") {
                return Err(StorageError::PermissionDenied(
                    "cryptsetup requires root privileges".to_string(),
                ));
            }

            return Err(StorageError::EncryptionError(format!(
                "luksClose command failed: {}",
                stderr
            )));
        }

        debug!("LUKS volume locked successfully");
        Ok(())
    }

    /// Checks if a device is a LUKS encrypted volume.
    ///
    /// # Arguments
    /// * `device_path` - Path to the block device
    ///
    /// # Returns
    /// true if the device is a LUKS volume, false otherwise
    fn is_luks_device(&self, device_path: &str) -> StorageResult<bool> {
        let output = Command::new("cryptsetup")
            .arg("isLuks")
            .arg(device_path)
            .output()
            .map_err(|e| {
                StorageError::CommandFailed(format!("Failed to execute cryptsetup isLuks: {}", e))
            })?;

        // isLuks returns 0 if it's a LUKS device, non-zero otherwise
        Ok(output.status.success())
    }

    /// Gets information about an encrypted volume.
    ///
    /// # Arguments
    /// * `device_path` - Path to the encrypted block device
    /// * `mapper_name` - Name of the mapped device
    ///
    /// # Returns
    /// EncryptedVolumeInfo with volume information
    fn get_volume_info(
        &self,
        device_path: &str,
        mapper_name: &str,
    ) -> StorageResult<EncryptedVolumeInfo> {
        let mapper_path = format!("/dev/mapper/{}", mapper_name);
        let is_unlocked = Path::new(&mapper_path).exists();

        // Get LUKS version
        let output = Command::new("cryptsetup")
            .arg("luksDump")
            .arg(device_path)
            .output()
            .map_err(|e| {
                StorageError::CommandFailed(format!("Failed to execute cryptsetup luksDump: {}", e))
            })?;

        let mut luks_version = "2".to_string();
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(line) = stdout.lines().find(|l| l.contains("Version:")) {
                if let Some(version) = line.split(':').nth(1) {
                    luks_version = version.trim().to_string();
                }
            }
        }

        let device_name = Path::new(device_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        Ok(EncryptedVolumeInfo {
            device_name,
            device_path: device_path.to_string(),
            mapper_path,
            is_unlocked,
            luks_version,
        })
    }

    /// Generates a random encryption key.
    ///
    /// # Arguments
    /// * `size_bytes` - Size of the key in bytes (typically 32 or 64)
    ///
    /// # Returns
    /// Vector of random bytes
    ///
    /// # Example
    /// ```
    /// use capsuled_engine::storage::LuksManager;
    /// use std::path::PathBuf;
    /// let manager = LuksManager::new(PathBuf::from("/etc/capsuled/keys"));
    /// let key = manager.generate_key(32);
    /// assert_eq!(key.len(), 32);
    /// ```
    pub fn generate_key(&self, size_bytes: usize) -> Vec<u8> {
        use std::fs::File;
        use std::io::Read;

        // Read from /dev/urandom for cryptographically secure random data
        let mut key = vec![0u8; size_bytes];
        if let Ok(mut urandom) = File::open("/dev/urandom") {
            let _ = urandom.read_exact(&mut key);
        }
        key
    }

    /// Stores a key to a file with secure permissions.
    ///
    /// # Arguments
    /// * `key_name` - Name for the key file (without extension)
    /// * `key_data` - The key bytes to store
    ///
    /// # Returns
    /// Path to the stored key file
    ///
    /// # Errors
    /// Returns StorageError if file operations fail
    pub fn store_key(&self, key_name: &str, key_data: &[u8]) -> StorageResult<PathBuf> {
        // Ensure key directory exists
        if !self.key_directory.exists() {
            fs::create_dir_all(&self.key_directory).map_err(|e| {
                StorageError::KeyManagementError(format!("Failed to create key directory: {}", e))
            })?;
        }

        let key_path = self.key_directory.join(format!("{}.key", key_name));

        // Write key file
        fs::write(&key_path, key_data).map_err(|e| {
            StorageError::KeyManagementError(format!("Failed to write key file: {}", e))
        })?;

        // Set restrictive permissions (600 - owner read/write only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            fs::set_permissions(&key_path, perms).map_err(|e| {
                StorageError::KeyManagementError(format!("Failed to set key permissions: {}", e))
            })?;
        }

        info!("Stored encryption key: {}", key_path.display());
        Ok(key_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_luks_manager() {
        let key_dir = PathBuf::from("/tmp/test_keys");
        let manager = LuksManager::new(key_dir.clone());
        assert_eq!(manager.key_directory, key_dir);
    }

    #[test]
    fn test_generate_key() {
        let manager = LuksManager::new(PathBuf::from("/tmp/test_keys"));

        let key_32 = manager.generate_key(32);
        assert_eq!(key_32.len(), 32);

        let key_64 = manager.generate_key(64);
        assert_eq!(key_64.len(), 64);

        // Verify keys are different (not all zeros)
        assert!(key_32.iter().any(|&b| b != 0));
        assert!(key_64.iter().any(|&b| b != 0));
    }

    #[test]
    fn test_encrypted_volume_info_creation() {
        let info = EncryptedVolumeInfo {
            device_name: "lv_encrypted".to_string(),
            device_path: "/dev/vg_data/lv_encrypted".to_string(),
            mapper_path: "/dev/mapper/encrypted_vol".to_string(),
            is_unlocked: true,
            luks_version: "2".to_string(),
        };

        assert_eq!(info.device_name, "lv_encrypted");
        assert_eq!(info.device_path, "/dev/vg_data/lv_encrypted");
        assert_eq!(info.mapper_path, "/dev/mapper/encrypted_vol");
        assert!(info.is_unlocked);
        assert_eq!(info.luks_version, "2");
    }

    #[test]
    fn test_key_storage_variants() {
        let file_key = KeyStorage::File(PathBuf::from("/tmp/key.bin"));
        let memory_key = KeyStorage::Memory(vec![1, 2, 3, 4]);

        match file_key {
            KeyStorage::File(path) => assert_eq!(path, PathBuf::from("/tmp/key.bin")),
            _ => panic!("Expected File variant"),
        }

        match memory_key {
            KeyStorage::Memory(data) => assert_eq!(data, vec![1, 2, 3, 4]),
            _ => panic!("Expected Memory variant"),
        }
    }

    // Note: Integration tests that actually execute cryptsetup commands
    // should be in a separate test module with proper setup/teardown
    // and should only run with root privileges
}
