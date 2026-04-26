# Storage Management Module

This module provides LVM (Logical Volume Manager) and LUKS (Linux Unified Key Setup) encryption functionality for Nacelle engine.

## Features

### LVM Manager (`lvm.rs`)
- Create logical volumes with specified sizes
- Delete logical volumes
- Create snapshots for backup/restore
- List all volumes in a volume group
- Volume name validation

### LUKS Manager (`luks.rs`)
- Create LUKS2 encrypted volumes
- Unlock/lock encrypted volumes
- Key management (file-based or in-memory)
- Generate cryptographically secure keys
- Store keys with secure permissions (600)

## Prerequisites

### System Requirements
- LVM2 tools (`lvcreate`, `lvremove`, `lvs`)
- cryptsetup (for LUKS encryption)
- Root or sudo privileges for volume operations

### Installation (Ubuntu/Debian)
```bash
sudo apt-get install lvm2 cryptsetup
```

## Usage Examples

### LVM Manager

```rust
use nacelle_engine::storage::{LvmManager, StorageResult};

fn main() -> StorageResult<()> {
    // Create LVM manager with default volume group
    let lvm = LvmManager::new("vg_data".to_string());

    // Create a 1GB logical volume
    let volume = lvm.create_volume(
        "my_volume",
        1024 * 1024 * 1024, // 1GB in bytes
        None // Use default VG
    )?;
    println!("Created volume: {}", volume.device_path);

    // List all volumes
    let volumes = lvm.list_volumes(None)?;
    for vol in volumes {
        println!("Volume: {}/{} - {} bytes", 
                 vol.vg_name, vol.lv_name, vol.size_bytes);
    }

    // Create a snapshot
    let snapshot = lvm.create_snapshot(
        "my_volume",
        "my_volume_snap",
        512 * 1024 * 1024, // 512MB COW space
        None
    )?;
    println!("Created snapshot: {}", snapshot.device_path);

    // Delete volume
    lvm.delete_volume("my_volume", None)?;
    
    Ok(())
}
```

### LUKS Manager

```rust
use nacelle_engine::storage::{LuksManager, KeyStorage, StorageResult};
use std::path::PathBuf;

fn main() -> StorageResult<()> {
    // Create LUKS manager
    let luks = LuksManager::new(PathBuf::from("/etc/nacelle/keys"));

    // Generate encryption key
    let key = luks.generate_key(32); // 32-byte key

    // Store key securely
    let key_path = luks.store_key("my_volume", &key)?;
    println!("Key stored at: {}", key_path.display());

    // Create encrypted volume on existing block device
    let encrypted = luks.create_encrypted_volume(
        "/dev/vg_data/lv_encrypted",
        KeyStorage::File(key_path.clone())
    )?;
    println!("Created encrypted volume: {}", encrypted.device_path);

    // Unlock volume
    let unlocked = luks.unlock_volume(
        "/dev/vg_data/lv_encrypted",
        "encrypted_vol",
        KeyStorage::File(key_path)
    )?;
    println!("Unlocked at: {}", unlocked.mapper_path);

    // Use the unlocked volume at /dev/mapper/encrypted_vol
    // ... (format, mount, use)

    // Lock volume when done
    luks.lock_volume("encrypted_vol")?;

    Ok(())
}
```

### Combined LVM + LUKS Example

```rust
use nacelle_engine::storage::{LvmManager, LuksManager, KeyStorage, StorageResult};
use std::path::PathBuf;

fn create_encrypted_volume() -> StorageResult<()> {
    // 1. Create LVM volume
    let lvm = LvmManager::new("vg_data".to_string());
    let volume = lvm.create_volume("encrypted_data", 10 * 1024 * 1024 * 1024, None)?;
    println!("Created LVM volume: {}", volume.device_path);

    // 2. Encrypt the volume with LUKS
    let luks = LuksManager::new(PathBuf::from("/etc/nacelle/keys"));
    let key = luks.generate_key(32);
    let key_path = luks.store_key("encrypted_data", &key)?;
    
    let encrypted = luks.create_encrypted_volume(
        &volume.device_path,
        KeyStorage::File(key_path.clone())
    )?;
    println!("Encrypted volume: {}", encrypted.device_path);

    // 3. Unlock for use
    let unlocked = luks.unlock_volume(
        &encrypted.device_path,
        "encrypted_data_unlocked",
        KeyStorage::File(key_path)
    )?;
    println!("Ready to use at: {}", unlocked.mapper_path);

    Ok(())
}
```

## Error Handling

The storage module uses comprehensive error types:

```rust
use nacelle_engine::storage::{StorageError, StorageResult};

fn handle_errors() -> StorageResult<()> {
    let lvm = LvmManager::new("vg_data".to_string());
    
    match lvm.create_volume("test", 1024 * 1024 * 1024, None) {
        Ok(volume) => println!("Success: {}", volume.device_path),
        Err(StorageError::VolumeAlreadyExists(name)) => {
            eprintln!("Volume {} already exists", name);
        }
        Err(StorageError::InsufficientSpace { required, available }) => {
            eprintln!("Need {} bytes, only {} available", required, available);
        }
        Err(StorageError::PermissionDenied(msg)) => {
            eprintln!("Permission denied: {}", msg);
        }
        Err(e) => eprintln!("Error: {}", e),
    }
    
    Ok(())
}
```

## Integration Tests

Integration tests require:
- Root/sudo privileges
- Actual LVM volume group configured
- cryptsetup installed

To run integration tests:

```bash
# Setup test volume group (example using loop device)
sudo truncate -s 1G /tmp/test_vg.img
sudo losetup -f /tmp/test_vg.img
sudo pvcreate /dev/loop0
sudo vgcreate test_vg /dev/loop0

# Run integration tests
sudo -E cargo test --test storage_integration -- --test-threads=1

# Cleanup
sudo vgremove -f test_vg
sudo pvremove /dev/loop0
sudo losetup -d /dev/loop0
sudo rm /tmp/test_vg.img
```

## Security Considerations

### Key Storage
- Keys are stored with 600 permissions (owner read/write only)
- Temporary key files are removed after use
- Consider using a key management system (KMS) for production

### Root Privileges
- LVM and LUKS operations require root privileges
- The engine should run with appropriate capabilities or as root
- Use sudo with specific command allowlists in production

### Volume Cleanup
- Always lock LUKS volumes before deletion
- Ensure volumes are not mounted before deletion
- Handle errors appropriately to prevent data loss

## Architecture Notes

### CGO-Free Implementation
This module is implemented in pure Rust with no CGO dependencies, using command-line tools:
- LVM: `lvcreate`, `lvremove`, `lvs`
- LUKS: `cryptsetup`

This ensures compatibility with the project's CGO-free requirement.

### Stateless Design
The managers don't maintain state; all information is queried from the system when needed, following the Stateless Master pattern.

## TODO / Future Enhancements

- [ ] Add support for thin provisioning
- [ ] Implement volume resizing
- [ ] Add volume group management
- [ ] Support for key rotation
- [ ] Integration with hardware security modules (HSM)
- [ ] Async/await support for long-running operations
- [ ] Progress callbacks for encryption operations
- [ ] Support for other encryption backends (dm-crypt directly)

## References

- [LVM2 Documentation](https://sourceware.org/lvm2/)
- [LUKS Specification](https://gitlab.com/cryptsetup/cryptsetup)
- [dm-crypt/LUKS Wiki](https://wiki.archlinux.org/title/Dm-crypt)
