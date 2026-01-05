//! LUKS Encryption Integration Tests
//!
//! These tests verify the LUKS encryption integration:
//! - Encrypted volume creation
//! - Key management (generation, storage, retrieval)
//! - Volume unlock/lock operations
//! - Encrypted mount and unmount
//! - Combined Thin + Encrypted volumes
//!
//! Prerequisites:
//! - Root/sudo privileges
//! - LVM2 tools installed
//! - cryptsetup installed
//! - A volume group named "test_vg"
//! - (Optional) A thin pool named "thin_pool" for thin+encrypted tests
//!
//! To run:
//! ```bash
//! sudo -E cargo test --test storage_encryption_test -- --test-threads=1
//! ```

use std::path::PathBuf;
use std::process::Command;

/// Helper to check prerequisites for encryption tests
mod prereqs {
    use std::process::Command;

    pub fn is_root() -> bool {
        std::env::var("USER").unwrap_or_default() == "root" || std::env::var("SUDO_USER").is_ok()
    }

    pub fn has_lvm_tools() -> bool {
        Command::new("lvs")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn has_cryptsetup() -> bool {
        Command::new("cryptsetup")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn has_test_vg() -> bool {
        Command::new("vgs")
            .arg("test_vg")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn has_thin_pool() -> bool {
        let output = Command::new("lvs")
            .args(["--noheadings", "-o", "lv_attr", "test_vg/thin_pool"])
            .output();

        match output {
            Ok(o) if o.status.success() => {
                let attr = String::from_utf8_lossy(&o.stdout);
                attr.trim().starts_with('t')
            }
            _ => false,
        }
    }

    pub fn all_met() -> bool {
        is_root() && has_lvm_tools() && has_cryptsetup() && has_test_vg()
    }

    pub fn all_with_thin() -> bool {
        all_met() && has_thin_pool()
    }

    pub fn print_status() {
        println!("=== Encryption Test Prerequisites ===");
        println!("  Root privileges: {}", is_root());
        println!("  LVM tools: {}", has_lvm_tools());
        println!("  cryptsetup: {}", has_cryptsetup());
        println!("  test_vg exists: {}", has_test_vg());
        println!("  thin_pool exists: {}", has_thin_pool());
        println!("======================================");
    }
}

/// Check if a device is a LUKS volume
fn is_luks_device(device_path: &str) -> bool {
    Command::new("cryptsetup")
        .args(["isLuks", device_path])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check if a mapper device exists
fn mapper_exists(mapper_name: &str) -> bool {
    std::path::Path::new(&format!("/dev/mapper/{}", mapper_name)).exists()
}

// ============================================================================
// Unit Tests (can run without root/LVM)
// ============================================================================

#[test]
fn test_prerequisites_check() {
    prereqs::print_status();
    // Always passes - for visibility
}

#[test]
fn test_key_generation_size() {
    use capsuled::storage::LuksManager;

    let manager = LuksManager::new(PathBuf::from("/tmp"));

    // Test various key sizes
    let key_32 = manager.generate_key(32);
    assert_eq!(key_32.len(), 32);

    let key_64 = manager.generate_key(64);
    assert_eq!(key_64.len(), 64);

    // Keys should be different (random)
    assert_ne!(key_32, &key_64[..32]);
}

// ============================================================================
// Integration Tests (require root, LVM, cryptsetup)
// ============================================================================

#[test]
#[ignore] // Run with --ignored flag
fn test_encrypted_volume_creation() {
    if !prereqs::all_met() {
        eprintln!("Skipping: prerequisites not met");
        prereqs::print_status();
        return;
    }

    use capsuled::storage::{StorageConfig, StorageManager};

    println!("=== Test: Encrypted Volume Creation ===");

    let config = StorageConfig {
        enabled: true,
        default_vg: "test_vg".to_string(),
        key_directory: PathBuf::from("/tmp/capsuled_encryption_test_keys"),
        enable_encryption: true,              // Enable by default
        default_size_bytes: 50 * 1024 * 1024, // 50MB
        mount_base: PathBuf::from("/tmp/capsuled_encryption_test_mounts"),
        thin_pool_name: None,
        use_thin_by_default: false,
    };

    // Ensure key directory exists
    std::fs::create_dir_all(&config.key_directory).expect("Failed to create key directory");

    let manager = StorageManager::new(config.clone());
    let capsule_id = "encrypted-test-001";

    // Step 1: Provision encrypted volume
    println!("Step 1: Provisioning encrypted volume...");
    let mut storage = manager
        .provision_capsule_storage(capsule_id, None, Some(true), Some(false))
        .expect("Provision failed");

    assert!(storage.encrypted, "Volume should be marked as encrypted");
    assert!(
        storage.device_path.starts_with("/dev/mapper/"),
        "Device path should be mapper device"
    );
    println!("  ✓ Encrypted volume created");
    println!("    Device: {}", storage.device_path);

    // Step 2: Verify underlying volume is LUKS
    println!("Step 2: Verifying LUKS format...");
    let lv_device = format!("/dev/test_vg/{}", storage.lv_name);
    assert!(
        is_luks_device(&lv_device),
        "Underlying LV should be LUKS formatted"
    );
    println!("  ✓ Underlying volume is LUKS");

    // Step 3: Verify mapper device exists
    println!("Step 3: Verifying mapper device...");
    let mapper_name = format!("capsule_{}", storage.lv_name);
    assert!(mapper_exists(&mapper_name), "Mapper device should exist");
    println!("  ✓ Mapper device exists: /dev/mapper/{}", mapper_name);

    // Step 4: Mount and write data
    println!("Step 4: Mounting encrypted volume...");
    manager.mount_volume(&mut storage).expect("Mount failed");

    let mount_path = storage.mount_point.clone().expect("Mount point missing");
    assert!(mount_path.exists(), "Mount point should exist");

    let test_file = mount_path.join("secret_data.txt");
    std::fs::write(&test_file, "Top secret encrypted data!").expect("Failed to write");
    assert!(test_file.exists(), "Test file should exist");
    println!("  ✓ Data written to encrypted volume");

    // Step 5: Unmount
    println!("Step 5: Unmounting...");
    manager
        .unmount_volume(capsule_id, &storage.lv_name)
        .expect("Unmount failed");
    println!("  ✓ Volume unmounted");

    // Step 6: Cleanup
    println!("Step 6: Cleaning up...");
    manager
        .cleanup_capsule_storage(capsule_id)
        .expect("Cleanup failed");

    // Verify mapper is gone
    assert!(
        !mapper_exists(&mapper_name),
        "Mapper should be removed after cleanup"
    );
    println!("  ✓ Encrypted volume cleaned up");

    // Clean up key directory
    let _ = std::fs::remove_dir_all(&config.key_directory);

    println!("=== Test Passed: Encrypted Volume Creation ===");
}

#[test]
#[ignore]
fn test_thin_plus_encrypted_volume() {
    if !prereqs::all_with_thin() {
        eprintln!("Skipping: thin pool prerequisites not met");
        prereqs::print_status();
        return;
    }

    use capsuled::storage::{StorageConfig, StorageManager};

    println!("=== Test: Thin + Encrypted Volume ===");

    let config = StorageConfig {
        enabled: true,
        default_vg: "test_vg".to_string(),
        key_directory: PathBuf::from("/tmp/capsuled_thin_encrypted_keys"),
        enable_encryption: true,
        default_size_bytes: 30 * 1024 * 1024, // 30MB
        mount_base: PathBuf::from("/tmp/capsuled_thin_encrypted_mounts"),
        thin_pool_name: Some("thin_pool".to_string()),
        use_thin_by_default: true,
    };

    std::fs::create_dir_all(&config.key_directory).expect("Failed to create key directory");

    let manager = StorageManager::new(config.clone());
    let capsule_id = "thin-encrypted-001";

    // Provision thin + encrypted volume
    println!("Step 1: Provisioning thin + encrypted volume...");
    let mut storage = manager
        .provision_capsule_storage(capsule_id, None, Some(true), Some(true))
        .expect("Provision failed");

    assert!(storage.encrypted, "Should be encrypted");
    assert!(
        storage.device_path.starts_with("/dev/mapper/"),
        "Should use mapper device"
    );
    println!("  ✓ Thin + encrypted volume created");

    // Verify underlying is thin and LUKS
    let lv_device = format!("/dev/test_vg/{}", storage.lv_name);

    // Check LUKS
    assert!(is_luks_device(&lv_device), "Should be LUKS formatted");
    println!("  ✓ Volume is LUKS encrypted");

    // Check thin via lvs
    let lvs_output = Command::new("lvs")
        .args([
            "--noheadings",
            "-o",
            "lv_attr",
            &format!("test_vg/{}", storage.lv_name),
        ])
        .output()
        .expect("lvs command failed");

    let attr = String::from_utf8_lossy(&lvs_output.stdout);
    // Thin volume attribute starts with 'V'
    assert!(
        attr.trim().starts_with('V'),
        "Should be thin volume (attr: {})",
        attr.trim()
    );
    println!("  ✓ Volume is thin provisioned");

    // Mount and test
    println!("Step 2: Mount and write test...");
    manager.mount_volume(&mut storage).expect("Mount failed");

    let mount_path = storage.mount_point.clone().expect("Mount point missing");
    let test_file = mount_path.join("thin_encrypted_test.txt");
    std::fs::write(&test_file, "Thin + Encrypted = Ultimate Storage!").expect("Write failed");
    println!("  ✓ Data written successfully");

    // Cleanup
    println!("Step 3: Cleanup...");
    manager
        .unmount_volume(capsule_id, &storage.lv_name)
        .expect("Unmount failed");
    manager
        .cleanup_capsule_storage(capsule_id)
        .expect("Cleanup failed");

    let _ = std::fs::remove_dir_all(&config.key_directory);

    println!("=== Test Passed: Thin + Encrypted Volume ===");
}

#[test]
#[ignore]
fn test_encryption_key_persistence() {
    if !prereqs::all_met() {
        eprintln!("Skipping: prerequisites not met");
        return;
    }

    use capsuled::storage::{KeyStorage, LuksManager, StorageConfig, StorageManager};

    println!("=== Test: Encryption Key Persistence ===");

    let key_dir = PathBuf::from("/tmp/capsuled_key_persistence_test");
    std::fs::create_dir_all(&key_dir).expect("Failed to create key directory");

    let config = StorageConfig {
        enabled: true,
        default_vg: "test_vg".to_string(),
        key_directory: key_dir.clone(),
        enable_encryption: true,
        default_size_bytes: 30 * 1024 * 1024,
        mount_base: PathBuf::from("/tmp/capsuled_key_persistence_mounts"),
        thin_pool_name: None,
        use_thin_by_default: false,
    };

    let manager = StorageManager::new(config.clone());
    let luks_manager = LuksManager::new(key_dir.clone());
    let capsule_id = "key-test-001";

    // Create encrypted volume
    println!("Step 1: Creating encrypted volume...");
    let storage = manager
        .provision_capsule_storage(capsule_id, None, Some(true), Some(false))
        .expect("Provision failed");

    // Check key file exists
    let mapper_name = format!("capsule_{}", storage.lv_name);
    let key_path = key_dir.join(&mapper_name);
    assert!(key_path.exists(), "Key file should exist");
    println!("  ✓ Key file created: {:?}", key_path);

    // Read key and verify it can unlock
    let key_data = std::fs::read(&key_path).expect("Failed to read key");
    assert!(key_data.len() >= 32, "Key should be at least 256 bits");
    println!("  ✓ Key is {} bytes", key_data.len());

    // Lock the volume
    println!("Step 2: Locking volume...");
    luks_manager.lock_volume(&mapper_name).expect("Lock failed");
    assert!(!mapper_exists(&mapper_name), "Mapper should be removed");
    println!("  ✓ Volume locked");

    // Re-unlock with saved key
    println!("Step 3: Re-unlocking with saved key...");
    let lv_device = format!("/dev/test_vg/{}", storage.lv_name);
    luks_manager
        .unlock_volume(&lv_device, &mapper_name, KeyStorage::File(key_path.clone()))
        .expect("Unlock failed");

    assert!(
        mapper_exists(&mapper_name),
        "Mapper should exist after re-unlock"
    );
    println!("  ✓ Volume re-unlocked successfully");

    // Cleanup
    println!("Step 4: Cleanup...");
    manager
        .cleanup_capsule_storage(capsule_id)
        .expect("Cleanup failed");

    let _ = std::fs::remove_dir_all(&key_dir);

    println!("=== Test Passed: Encryption Key Persistence ===");
}
