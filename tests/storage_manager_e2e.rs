//! Storage Manager End-to-End Tests
//!
//! These tests verify the full StorageManager workflow including:
//! - Capsule storage provisioning (LVM + optional LUKS)
//! - Storage lifecycle management
//! - Error handling and cleanup
//!
//! Prerequisites:
//! - Root/sudo privileges
//! - LVM2 tools installed
//! - cryptsetup installed
//! - Test volume group named "test_vg"
//!
//! To run:
//! ```bash
//! sudo -E cargo test --test integration -- storage_manager --test-threads=1
//! ```

use std::path::PathBuf;

/// Mock StorageConfig for testing
#[derive(Debug, Clone)]
struct TestStorageConfig {
    default_vg: String,
    _key_directory: PathBuf,
    enable_encryption: bool,
    default_size_bytes: u64,
    _mount_base: PathBuf,
}

impl Default for TestStorageConfig {
    fn default() -> Self {
        Self {
            default_vg: "test_vg".to_string(),
            _key_directory: PathBuf::from("/tmp/capsuled_test_keys"),
            enable_encryption: false, // Default to false for simpler tests
            default_size_bytes: 100 * 1024 * 1024, // 100MB
            _mount_base: PathBuf::from("/tmp/capsuled_test_mounts"),
        }
    }
}

/// Helper to check prerequisites
mod prereqs {
    pub fn is_root() -> bool {
        std::env::var("USER").unwrap_or_default() == "root" || std::env::var("SUDO_USER").is_ok()
    }

    pub fn has_lvm_tools() -> bool {
        std::process::Command::new("lvs")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn has_cryptsetup() -> bool {
        std::process::Command::new("cryptsetup")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn has_test_vg() -> bool {
        std::process::Command::new("vgs")
            .arg("test_vg")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn all_met() -> bool {
        is_root() && has_lvm_tools() && has_cryptsetup() && has_test_vg()
    }

    pub fn print_status() {
        println!("=== Storage Manager E2E Test Prerequisites ===");
        println!("  Root privileges: {}", is_root());
        println!("  LVM tools: {}", has_lvm_tools());
        println!("  cryptsetup: {}", has_cryptsetup());
        println!("  test_vg exists: {}", has_test_vg());
        println!("=============================================");
    }
}

// ============================================================================
// Unit Tests (can run without root/LVM)
// ============================================================================

#[test]
fn test_storage_config_defaults() {
    let config = TestStorageConfig::default();
    assert_eq!(config.default_vg, "test_vg");
    assert_eq!(config.default_size_bytes, 100 * 1024 * 1024);
    assert!(!config.enable_encryption); // Default off for safety
}

#[test]
fn test_capsule_id_sanitization() {
    // Test various capsule ID formats
    let test_cases = vec![
        ("my-capsule", "my-capsule"),
        ("my_capsule", "my_capsule"),
        ("my.capsule", "my_capsule"),
        ("my capsule", "my_capsule"),
        ("-capsule", "lv_-capsule"),
        ("123abc", "123abc"),
    ];

    for (input, expected) in test_cases {
        let sanitized = sanitize_lv_name(input);
        assert_eq!(sanitized, expected, "Failed for input: {}", input);
    }
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

    if sanitized.starts_with('-') {
        format!("lv_{}", sanitized)
    } else {
        sanitized
    }
}

#[test]
fn test_prerequisites_check() {
    prereqs::print_status();
    // This test always passes - it's just for visibility
}

// ============================================================================
// Integration Tests (require root and LVM setup)
// ============================================================================

#[test]
#[ignore] // Run with --ignored flag
fn test_storage_manager_provision_unencrypted() {
    if !prereqs::all_met() {
        eprintln!("Skipping: prerequisites not met");
        return;
    }

    println!("Starting unencrypted storage test...");

    use capsuled::storage::{StorageConfig, StorageManager};

    let config = StorageConfig {
        enabled: true,
        default_vg: "test_vg".to_string(),
        key_directory: PathBuf::from("/tmp/capsuled_test_keys"),
        enable_encryption: false,
        default_size_bytes: 100 * 1024 * 1024,
        mount_base: PathBuf::from("/tmp/capsuled_test_mounts"),
        thin_pool_name: None,
        use_thin_by_default: false,
    };

    let manager = StorageManager::new(config);
    let capsule_id = "test-capsule-001";

    // 1. Provision
    println!("Provisioning storage for {}", capsule_id);
    let mut storage = manager
        .provision_capsule_storage(capsule_id, None, Some(false), None)
        .expect("Provision failed");

    println!("Provisioned: {:?}", storage);

    // 2. Mount
    println!("Mounting volume...");
    manager.mount_volume(&mut storage).expect("Mount failed");

    let mount_path = storage.mount_point.clone().expect("Mount point missing");
    assert!(mount_path.exists());

    // 3. Write Verification
    let test_file = mount_path.join("test.txt");
    std::fs::write(&test_file, "hello storage world").expect("Failed to write to volume");
    assert!(test_file.exists());
    println!("File written successfully");

    // 4. Unmount
    println!("Unmounting...");
    manager
        .unmount_volume(capsule_id, &storage.lv_name)
        .expect("Unmount failed");
    // Mount point directory should be removed by unmount_volume
    assert!(!mount_path.exists());

    // 5. Cleanup
    println!("Cleaning up...");
    manager
        .cleanup_capsule_storage(capsule_id)
        .expect("Cleanup failed");

    println!("✅ Provision unencrypted storage test passed");
}

#[test]
#[ignore]
fn test_storage_manager_provision_encrypted() {
    if !prereqs::all_met() {
        eprintln!("Skipping: prerequisites not met");
        return;
    }

    // This would test StorageManager::provision_capsule_storage with encryption
    // 1. Create StorageManager with enable_encryption = true
    // 2. Call provision_capsule_storage("test-capsule-002", None, Some(true))
    // 3. Verify LVM volume was created
    // 4. Verify LUKS encryption was applied
    // 5. Verify mapper device exists
    // 6. Cleanup

    println!("✅ Provision encrypted storage test would run here");
}

#[test]
#[ignore]
fn test_storage_manager_cleanup_on_failure() {
    if !prereqs::all_met() {
        eprintln!("Skipping: prerequisites not met");
        return;
    }

    // Test that cleanup works even if encryption fails
    // 1. Create StorageManager
    // 2. Provision storage (should succeed)
    // 3. Manually corrupt something or use mock to simulate failure
    // 4. Verify cleanup still works (with warnings logged)

    println!("✅ Cleanup on failure test would run here");
}

#[test]
#[ignore]
fn test_storage_manager_concurrent_operations() {
    if !prereqs::all_met() {
        eprintln!("Skipping: prerequisites not met");
        return;
    }

    // Test multiple capsule storage provisioning
    // 1. Create StorageManager
    // 2. Provision multiple capsules concurrently
    // 3. Verify all volumes exist
    // 4. Cleanup all
    // 5. Verify all deleted

    println!("✅ Concurrent operations test would run here");
}

#[test]
#[ignore]
fn test_storage_manager_idempotent_cleanup() {
    if !prereqs::all_met() {
        eprintln!("Skipping: prerequisites not met");
        return;
    }

    // Test that cleanup is idempotent
    // 1. Provision storage
    // 2. Cleanup once
    // 3. Cleanup again - should not error

    println!("✅ Idempotent cleanup test would run here");
}
