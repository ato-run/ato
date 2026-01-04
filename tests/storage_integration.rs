/// Integration tests for storage module
///
/// These tests require:
/// - Root/sudo privileges
/// - LVM2 tools installed
/// - cryptsetup installed
/// - A test volume group named "test_vg"
///
/// To run these tests:
/// ```bash
/// # Setup test volume group (example using loop device)
/// sudo truncate -s 1G /tmp/test_vg.img
/// sudo losetup -f /tmp/test_vg.img
/// sudo pvcreate /dev/loop0
/// sudo vgcreate test_vg /dev/loop0
///
/// # Run tests
/// sudo -E cargo test --test storage_integration -- --test-threads=1
///
/// # Cleanup
/// sudo vgremove -f test_vg
/// sudo pvremove /dev/loop0
/// sudo losetup -d /dev/loop0
/// sudo rm /tmp/test_vg.img
/// ```
///
/// Note: These tests are ignored by default to prevent accidental execution
/// without proper setup. Use `cargo test --test storage_integration -- --ignored`
/// to run them explicitly.
use capsuled_engine::storage::{KeyStorage, LuksManager, LvmManager, StorageResult};
use tempfile::TempDir;

const TEST_VG: &str = "test_vg";

/// Helper to check if we have root privileges
fn is_root() -> bool {
    std::env::var("USER").unwrap_or_default() == "root" || std::env::var("SUDO_USER").is_ok()
}

/// Helper to check if LVM tools are available
fn has_lvm_tools() -> bool {
    std::process::Command::new("lvs")
        .arg("--version")
        .output()
        .is_ok()
}

/// Helper to check if cryptsetup is available
fn has_cryptsetup() -> bool {
    std::process::Command::new("cryptsetup")
        .arg("--version")
        .output()
        .is_ok()
}

/// Helper to check if test volume group exists
fn has_test_vg() -> bool {
    std::process::Command::new("vgs")
        .arg(TEST_VG)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
#[ignore] // Requires root and LVM setup
fn test_lvm_create_and_delete_volume() -> StorageResult<()> {
    // Check prerequisites
    if !is_root() {
        eprintln!("Skipping test: requires root privileges");
        return Ok(());
    }
    if !has_lvm_tools() {
        eprintln!("Skipping test: LVM tools not available");
        return Ok(());
    }
    if !has_test_vg() {
        eprintln!("Skipping test: test volume group '{}' not found", TEST_VG);
        return Ok(());
    }

    let lvm = LvmManager::new(TEST_VG.to_string());
    let volume_name = "test_volume_integration";
    let size_bytes = 100 * 1024 * 1024; // 100MB

    // Create volume
    let volume = lvm.create_volume(volume_name, size_bytes, None)?;
    assert_eq!(volume.vg_name, TEST_VG);
    assert_eq!(volume.lv_name, volume_name);
    assert!(volume.size_bytes >= size_bytes); // LVM may round up

    // Verify volume exists
    let volumes = lvm.list_volumes(None)?;
    assert!(volumes.iter().any(|v| v.lv_name == volume_name));

    // Delete volume
    lvm.delete_volume(volume_name, None)?;

    // Verify volume is deleted
    let volumes = lvm.list_volumes(None)?;
    assert!(!volumes.iter().any(|v| v.lv_name == volume_name));

    Ok(())
}

#[test]
#[ignore] // Requires root and LVM setup
fn test_lvm_create_snapshot() -> StorageResult<()> {
    if !is_root() || !has_lvm_tools() || !has_test_vg() {
        eprintln!("Skipping test: prerequisites not met");
        return Ok(());
    }

    let lvm = LvmManager::new(TEST_VG.to_string());
    let volume_name = "test_volume_snapshot";
    let snapshot_name = "test_volume_snapshot_snap";
    let size_bytes = 100 * 1024 * 1024;

    // Create source volume
    let _volume = lvm.create_volume(volume_name, size_bytes, None)?;

    // Create snapshot
    let snapshot = lvm.create_snapshot(
        volume_name,
        snapshot_name,
        50 * 1024 * 1024, // 50MB COW space
        None,
    )?;
    assert_eq!(snapshot.lv_name, snapshot_name);

    // Verify snapshot exists
    let volumes = lvm.list_volumes(None)?;
    assert!(volumes.iter().any(|v| v.lv_name == snapshot_name));

    // Cleanup
    lvm.delete_volume(snapshot_name, None)?;
    lvm.delete_volume(volume_name, None)?;

    Ok(())
}

#[test]
#[ignore] // Requires root and cryptsetup
fn test_luks_create_and_unlock_volume() -> StorageResult<()> {
    if !is_root() || !has_cryptsetup() || !has_lvm_tools() || !has_test_vg() {
        eprintln!("Skipping test: prerequisites not met");
        return Ok(());
    }

    let lvm = LvmManager::new(TEST_VG.to_string());
    let volume_name = "test_luks_volume";
    let mapper_name = "test_luks_unlocked";
    let size_bytes = 100 * 1024 * 1024;

    // Create LVM volume first
    let volume = lvm.create_volume(volume_name, size_bytes, None)?;

    // Setup LUKS manager with temporary key directory
    let temp_dir = TempDir::new().unwrap();
    let luks = LuksManager::new(temp_dir.path().to_path_buf());

    // Generate and store key
    let key = luks.generate_key(32);
    let key_path = luks.store_key(volume_name, &key)?;

    // Create encrypted volume
    let encrypted =
        luks.create_encrypted_volume(&volume.device_path, KeyStorage::File(key_path.clone()))?;
    assert_eq!(encrypted.device_path, volume.device_path);
    assert!(!encrypted.is_unlocked);

    // Unlock volume
    let unlocked =
        luks.unlock_volume(&volume.device_path, mapper_name, KeyStorage::File(key_path))?;
    assert!(unlocked.is_unlocked);
    assert_eq!(unlocked.mapper_path, format!("/dev/mapper/{}", mapper_name));

    // Lock volume
    luks.lock_volume(mapper_name)?;

    // Cleanup
    lvm.delete_volume(volume_name, None)?;

    Ok(())
}

#[test]
#[ignore] // Requires root, LVM, and cryptsetup
fn test_luks_with_memory_key() -> StorageResult<()> {
    if !is_root() || !has_cryptsetup() || !has_lvm_tools() || !has_test_vg() {
        eprintln!("Skipping test: prerequisites not met");
        return Ok(());
    }

    let lvm = LvmManager::new(TEST_VG.to_string());
    let volume_name = "test_luks_memory";
    let mapper_name = "test_luks_memory_unlocked";
    let size_bytes = 100 * 1024 * 1024;

    // Create LVM volume
    let volume = lvm.create_volume(volume_name, size_bytes, None)?;

    // Setup LUKS with temporary key directory
    let temp_dir = TempDir::new().unwrap();
    let luks = LuksManager::new(temp_dir.path().to_path_buf());

    // Use memory-based key (more secure, no key file on disk)
    let key = luks.generate_key(32);

    // Create encrypted volume with in-memory key
    let _encrypted =
        luks.create_encrypted_volume(&volume.device_path, KeyStorage::Memory(key.clone()))?;

    // Unlock with same in-memory key
    let unlocked = luks.unlock_volume(&volume.device_path, mapper_name, KeyStorage::Memory(key))?;
    assert!(unlocked.is_unlocked);

    // Lock and cleanup
    luks.lock_volume(mapper_name)?;
    lvm.delete_volume(volume_name, None)?;

    Ok(())
}

#[test]
#[ignore] // Requires root, LVM, and cryptsetup
fn test_full_encrypted_volume_lifecycle() -> StorageResult<()> {
    if !is_root() || !has_cryptsetup() || !has_lvm_tools() || !has_test_vg() {
        eprintln!("Skipping test: prerequisites not met");
        return Ok(());
    }

    let lvm = LvmManager::new(TEST_VG.to_string());
    let temp_dir = TempDir::new().unwrap();
    let luks = LuksManager::new(temp_dir.path().to_path_buf());

    let volume_name = "test_full_lifecycle";
    let mapper_name = "test_full_lifecycle_unlocked";
    let size_bytes = 100 * 1024 * 1024;

    // 1. Create LVM volume
    let volume = lvm.create_volume(volume_name, size_bytes, None)?;
    println!("Created LVM volume: {}", volume.device_path);

    // 2. Encrypt with LUKS
    let key = luks.generate_key(32);
    let _encrypted =
        luks.create_encrypted_volume(&volume.device_path, KeyStorage::Memory(key.clone()))?;
    println!("Encrypted volume with LUKS2");

    // 3. Unlock
    let unlocked = luks.unlock_volume(&volume.device_path, mapper_name, KeyStorage::Memory(key))?;
    println!("Unlocked at: {}", unlocked.mapper_path);

    // At this point, you could:
    // - Format the filesystem: mkfs.ext4 /dev/mapper/test_full_lifecycle_unlocked
    // - Mount it: mount /dev/mapper/test_full_lifecycle_unlocked /mnt
    // - Use it for storage
    // - Unmount: umount /mnt

    // 4. Lock
    luks.lock_volume(mapper_name)?;
    println!("Locked volume");

    // 5. Delete LVM volume
    lvm.delete_volume(volume_name, None)?;
    println!("Deleted LVM volume");

    Ok(())
}

#[test]
fn test_prerequisites_check() {
    println!("=== Storage Integration Test Prerequisites ===");
    println!("Root privileges: {}", is_root());
    println!("LVM tools available: {}", has_lvm_tools());
    println!("cryptsetup available: {}", has_cryptsetup());
    println!("Test VG '{}' exists: {}", TEST_VG, has_test_vg());
    println!("==============================================");

    if is_root() && has_lvm_tools() && has_cryptsetup() && has_test_vg() {
        println!("✅ All prerequisites met! You can run integration tests.");
    } else {
        println!(
            "❌ Some prerequisites not met. See comments in test file for setup instructions."
        );
    }
}
