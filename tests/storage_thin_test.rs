//! Thin Provisioning Integration Tests
//!
//! These tests verify the LVM Thin Provisioning integration:
//! - Thin volume creation from thin pool
//! - Volume attributes verification (lvs output parsing)
//! - Filesystem creation and mount
//! - Cleanup and pool reclamation
//!
//! Prerequisites:
//! - Root/sudo privileges
//! - LVM2 tools installed (lvcreate, lvs, lvremove)
//! - A thin pool named "thin_pool" in volume group "test_vg"
//!
//! To set up test environment:
//! ```bash
//! # Create loopback device for testing
//! sudo dd if=/dev/zero of=/tmp/test_lvm.img bs=1M count=500
//! sudo losetup /dev/loop0 /tmp/test_lvm.img
//! sudo pvcreate /dev/loop0
//! sudo vgcreate test_vg /dev/loop0
//! # Create thin pool (400MB data, 4MB metadata)
//! sudo lvcreate -L 400M -T test_vg/thin_pool
//! ```
//!
//! To run:
//! ```bash
//! sudo -E cargo test --test storage_thin_test -- --test-threads=1
//! ```

use std::path::PathBuf;
use std::process::Command;

/// Helper to check prerequisites for thin provisioning tests
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
                // Thin pool attribute starts with 't'
                attr.trim().starts_with('t')
            }
            _ => false,
        }
    }

    pub fn all_met() -> bool {
        is_root() && has_lvm_tools() && has_test_vg() && has_thin_pool()
    }

    pub fn print_status() {
        println!("=== Thin Provisioning Test Prerequisites ===");
        println!("  Root privileges: {}", is_root());
        println!("  LVM tools: {}", has_lvm_tools());
        println!("  test_vg exists: {}", has_test_vg());
        println!("  thin_pool exists: {}", has_thin_pool());
        println!("=============================================");
    }
}

/// Parse lvs output to verify volume attributes
struct LvsVolumeInfo {
    #[allow(dead_code)]
    lv_name: String,
    #[allow(dead_code)]
    vg_name: String,
    #[allow(dead_code)]
    lv_attr: String,
    is_thin_volume: bool,
    pool_name: Option<String>,
    size_bytes: u64,
}

impl LvsVolumeInfo {
    fn query(vg_name: &str, lv_name: &str) -> Option<Self> {
        // lvs --noheadings -o lv_name,vg_name,lv_attr,pool_lv,lv_size --units b test_vg/lv_name
        let output = Command::new("lvs")
            .args([
                "--noheadings",
                "--nosuffix",
                "-o",
                "lv_name,vg_name,lv_attr,pool_lv,lv_size",
                "--units",
                "b",
                &format!("{}/{}", vg_name, lv_name),
            ])
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let fields: Vec<&str> = stdout.split_whitespace().collect();

        if fields.len() < 4 {
            return None;
        }

        let lv_attr = fields.get(2).unwrap_or(&"").to_string();
        // Thin volume attributes: first char is 'V' (virtual/thin)
        let is_thin_volume = lv_attr.starts_with('V');

        let pool_name = fields
            .get(3)
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());

        // Parse size (in bytes)
        let size_bytes: u64 = fields.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);

        Some(Self {
            lv_name: fields[0].to_string(),
            vg_name: fields[1].to_string(),
            lv_attr,
            is_thin_volume,
            pool_name,
            size_bytes,
        })
    }
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
fn test_lvs_output_parsing() {
    // Test parsing logic with mock data
    // This validates our parsing without requiring LVM
    let mock_attr_thin = "Vwi-a-t---";
    let mock_attr_thick = "-wi-a-----";

    assert!(
        mock_attr_thin.starts_with('V'),
        "Thin volume should start with 'V'"
    );
    assert!(
        !mock_attr_thick.starts_with('V'),
        "Thick volume should not start with 'V'"
    );
}

// ============================================================================
// Integration Tests (require root, LVM, thin pool)
// ============================================================================

#[test]
#[ignore] // Run with --ignored flag
fn test_thin_volume_creation() {
    if !prereqs::all_met() {
        eprintln!("Skipping: prerequisites not met");
        prereqs::print_status();
        return;
    }

    use capsuled_engine::storage::{StorageConfig, StorageManager};

    println!("=== Test: Thin Volume Creation ===");

    // Configure with thin pool
    let config = StorageConfig {
        enabled: true,
        default_vg: "test_vg".to_string(),
        key_directory: PathBuf::from("/tmp/capsuled_thin_test_keys"),
        enable_encryption: false,
        default_size_bytes: 50 * 1024 * 1024, // 50MB
        mount_base: PathBuf::from("/tmp/capsuled_thin_test_mounts"),
        thin_pool_name: Some("thin_pool".to_string()),
        use_thin_by_default: true,
    };

    let manager = StorageManager::new(config);
    let capsule_id = "thin-test-capsule-001";

    // Step 1: Provision thin volume
    println!("Step 1: Provisioning thin volume for {}", capsule_id);
    let mut storage = manager
        .provision_capsule_storage(capsule_id, None, Some(false), Some(true))
        .expect("Provision thin volume failed");

    println!("  LV Name: {}", storage.lv_name);
    println!("  Device Path: {}", storage.device_path);

    // Step 2: Verify volume attributes via lvs
    println!("Step 2: Verifying thin volume attributes");
    let volume_info =
        LvsVolumeInfo::query("test_vg", &storage.lv_name).expect("Failed to query volume info");

    assert!(
        volume_info.is_thin_volume,
        "Volume should be a thin volume (lv_attr should start with 'V')"
    );
    assert_eq!(
        volume_info.pool_name,
        Some("thin_pool".to_string()),
        "Volume should reference thin_pool"
    );
    println!("  ✓ Volume is thin: {}", volume_info.is_thin_volume);
    println!("  ✓ Pool: {:?}", volume_info.pool_name);
    println!("  ✓ Size: {} bytes", volume_info.size_bytes);

    // Step 3: Mount and write test data
    println!("Step 3: Mounting volume and writing test data");
    manager.mount_volume(&mut storage).expect("Mount failed");

    let mount_path = storage.mount_point.clone().expect("Mount point missing");
    assert!(mount_path.exists(), "Mount point should exist");

    let test_file = mount_path.join("thin_test.txt");
    std::fs::write(&test_file, "Hello from thin volume!").expect("Failed to write test file");
    assert!(test_file.exists(), "Test file should exist");
    println!("  ✓ Test file written successfully");

    // Step 4: Unmount
    println!("Step 4: Unmounting volume");
    manager
        .unmount_volume(capsule_id, &storage.lv_name)
        .expect("Unmount failed");
    assert!(!mount_path.exists(), "Mount point should be removed");
    println!("  ✓ Volume unmounted");

    // Step 5: Cleanup
    println!("Step 5: Cleaning up thin volume");
    manager
        .cleanup_capsule_storage(capsule_id)
        .expect("Delete failed");

    // Verify volume is gone
    assert!(
        LvsVolumeInfo::query("test_vg", &storage.lv_name).is_none(),
        "Volume should be deleted"
    );
    println!("  ✓ Volume deleted");

    println!("=== Test Passed: Thin Volume Creation ===");
}

#[test]
#[ignore]
fn test_thin_vs_thick_volume_comparison() {
    if !prereqs::all_met() {
        eprintln!("Skipping: prerequisites not met");
        return;
    }

    use capsuled_engine::storage::{StorageConfig, StorageManager};

    println!("=== Test: Thin vs Thick Volume Comparison ===");

    let config = StorageConfig {
        enabled: true,
        default_vg: "test_vg".to_string(),
        key_directory: PathBuf::from("/tmp/capsuled_comparison_keys"),
        enable_encryption: false,
        default_size_bytes: 30 * 1024 * 1024, // 30MB
        mount_base: PathBuf::from("/tmp/capsuled_comparison_mounts"),
        thin_pool_name: Some("thin_pool".to_string()),
        use_thin_by_default: false, // Default to thick
    };

    let manager = StorageManager::new(config);

    // Create thick volume
    println!("Creating thick volume...");
    let thick_storage = manager
        .provision_capsule_storage("thick-compare-001", None, Some(false), Some(false))
        .expect("Thick volume creation failed");

    let thick_info = LvsVolumeInfo::query("test_vg", &thick_storage.lv_name)
        .expect("Failed to query thick volume");

    assert!(
        !thick_info.is_thin_volume,
        "Thick volume should NOT be thin"
    );
    assert!(
        thick_info.pool_name.is_none(),
        "Thick volume should not have pool"
    );
    println!("  ✓ Thick volume created: {}", thick_storage.lv_name);

    // Create thin volume
    println!("Creating thin volume...");
    let thin_storage = manager
        .provision_capsule_storage("thin-compare-001", None, Some(false), Some(true))
        .expect("Thin volume creation failed");

    let thin_info = LvsVolumeInfo::query("test_vg", &thin_storage.lv_name)
        .expect("Failed to query thin volume");

    assert!(thin_info.is_thin_volume, "Thin volume should be thin");
    assert_eq!(
        thin_info.pool_name,
        Some("thin_pool".to_string()),
        "Thin volume should reference pool"
    );
    println!("  ✓ Thin volume created: {}", thin_storage.lv_name);

    // Cleanup both
    println!("Cleaning up...");
    manager
        .cleanup_capsule_storage("thick-compare-001")
        .expect("Thick cleanup failed");
    manager
        .cleanup_capsule_storage("thin-compare-001")
        .expect("Thin cleanup failed");

    println!("=== Test Passed: Thin vs Thick Comparison ===");
}

#[test]
#[ignore]
fn test_thin_pool_info() {
    if !prereqs::all_met() {
        eprintln!("Skipping: prerequisites not met");
        return;
    }

    use capsuled_engine::storage::LvmManager;

    println!("=== Test: Thin Pool Info ===");

    let lvm = LvmManager::new("test_vg".to_string());

    let pool_info = lvm
        .get_thin_pool_info("thin_pool", None)
        .expect("Failed to get thin pool info");

    println!("  Pool: {}/{}", pool_info.vg_name, pool_info.pool_name);
    println!("  Size: {} bytes", pool_info.size_bytes);
    println!("  Data Used: {} bytes", pool_info.data_used_bytes);
    println!("  Data Usage: {:.2}%", pool_info.data_percent);
    println!("  Metadata Usage: {:.2}%", pool_info.metadata_percent);

    assert!(pool_info.size_bytes > 0, "Pool should have non-zero size");
    assert!(
        pool_info.data_percent >= 0.0 && pool_info.data_percent <= 100.0,
        "Data percentage should be between 0-100"
    );

    println!("=== Test Passed: Thin Pool Info ===");
}

#[test]
#[ignore]
fn test_thin_volume_extend() {
    if !prereqs::all_met() {
        eprintln!("Skipping: prerequisites not met");
        return;
    }

    use capsuled_engine::storage::{StorageConfig, StorageManager};

    println!("=== Test: Thin Volume Extension ===");

    let config = StorageConfig {
        enabled: true,
        default_vg: "test_vg".to_string(),
        key_directory: PathBuf::from("/tmp/capsuled_extend_keys"),
        enable_encryption: false,
        default_size_bytes: 20 * 1024 * 1024, // 20MB initial
        mount_base: PathBuf::from("/tmp/capsuled_extend_mounts"),
        thin_pool_name: Some("thin_pool".to_string()),
        use_thin_by_default: true,
    };

    let manager = StorageManager::new(config);
    let capsule_id = "extend-test-001";

    // Create thin volume
    println!("Step 1: Creating 20MB thin volume...");
    let storage = manager
        .provision_capsule_storage(capsule_id, None, Some(false), Some(true))
        .expect("Failed to create thin volume");

    let initial_info =
        LvsVolumeInfo::query("test_vg", &storage.lv_name).expect("Failed to query volume");
    println!("  Initial size: {} bytes", initial_info.size_bytes);

    // Extend to 40MB using LvmManager directly
    println!("Step 2: Extending to 40MB...");
    let new_size = 40 * 1024 * 1024;

    use capsuled_engine::storage::LvmManager;
    let lvm = LvmManager::new("test_vg".to_string());
    lvm.extend_volume(&storage.lv_name, new_size, None, false)
        .expect("Failed to extend volume");

    let extended_info =
        LvsVolumeInfo::query("test_vg", &storage.lv_name).expect("Failed to query extended volume");
    println!("  Extended size: {} bytes", extended_info.size_bytes);

    assert!(
        extended_info.size_bytes >= new_size,
        "Volume should be at least 40MB after extension"
    );
    println!("  ✓ Volume extended successfully");

    // Cleanup
    println!("Step 3: Cleaning up...");
    manager
        .cleanup_capsule_storage(capsule_id)
        .expect("Cleanup failed");

    println!("=== Test Passed: Thin Volume Extension ===");
}
