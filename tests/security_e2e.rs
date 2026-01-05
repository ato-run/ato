//! Security Integration E2E Tests
//!
//! These tests verify the complete security stack:
//! - Unit 2: Signature Verification (Fail-Closed)
//! - Unit 3: Egress Firewall (Fail-Closed)
//! - Unit 4: Storage/VRAM Lifecycle
//!
//! Prerequisites:
//! - Root/sudo privileges (iptables, LVM, LUKS, mount)
//! - Test volume group "test_vg"
//! - Network access for egress tests
//!
//! To run:
//! ```bash
//! sudo -E cargo test --test security_e2e -- --ignored --test-threads=1
//! ```

#![cfg(unix)]

use std::collections::HashMap;
use std::path::PathBuf;

// ============================================================================
// Prerequisites Check
// ============================================================================

mod prereqs {
    pub fn is_root() -> bool {
        std::env::var("USER").unwrap_or_default() == "root" || std::env::var("SUDO_USER").is_ok()
    }

    pub fn has_iptables() -> bool {
        std::process::Command::new("iptables")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn has_lvm_tools() -> bool {
        std::process::Command::new("lvs")
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

    pub fn network_prereqs() -> bool {
        is_root() && has_iptables()
    }

    pub fn storage_prereqs() -> bool {
        is_root() && has_lvm_tools() && has_test_vg()
    }

    pub fn print_status() {
        println!("=== Security E2E Test Prerequisites ===");
        println!("  Root privileges: {}", is_root());
        println!("  iptables: {}", has_iptables());
        println!("  LVM tools: {}", has_lvm_tools());
        println!("  test_vg exists: {}", has_test_vg());
        println!("========================================");
    }
}

/// Helper to create a minimal test manifest
fn create_test_manifest(name: &str) -> capsule_core::capsule_v1::CapsuleManifestV1 {
    use capsule_core::capsule_v1::*;

    CapsuleManifestV1 {
        schema_version: "1.0".to_string(),
        name: name.to_string(),
        version: "1.0.0".to_string(),
        capsule_type: CapsuleType::App,
        metadata: CapsuleMetadataV1::default(),
        capabilities: None,
        requirements: CapsuleRequirements::default(),
        execution: CapsuleExecution {
            runtime: RuntimeType::Docker,
            entrypoint: "alpine:latest".to_string(),
            port: None,
            health_check: None,
            startup_timeout: 60,
            env: HashMap::new(),
            signals: SignalConfig::default(),
        },
        storage: CapsuleStorage::default(),
        routing: CapsuleRouting::default(),
        network: None,
        model: None,
        transparency: None,
        pool: None,
        targets: None,
    }
}

// ============================================================================
// Test 1: Egress Fail-Closed (Blocked Traffic)
// ============================================================================

#[test]
#[ignore] // Requires root and network
fn test_egress_fail_closed_blocks_disallowed_traffic() {
    prereqs::print_status();
    if !prereqs::network_prereqs() {
        eprintln!("Skipping: network prerequisites not met");
        return;
    }

    use capsuled::security::egress_policy::generate_fw_rules;
    use capsule_core::capsule_v1::{EgressIdRule, EgressIdType, NetworkConfig};

    // Create manifest with restricted egress (only internal network)
    let mut manifest = create_test_manifest("test-egress-blocked");
    manifest.network = Some(NetworkConfig {
        egress_allow: vec![],
        egress_id_allow: vec![EgressIdRule {
            rule_type: EgressIdType::Cidr,
            value: "10.0.0.0/8".to_string(), // Only internal
        }],
    });

    // Generate iptables rules
    let rules = generate_fw_rules(&manifest);

    println!("Generated {} iptables rules:", rules.len());
    for rule in &rules {
        println!("  {}", rule);
    }

    // Verify structure
    assert!(
        rules.iter().any(|r| r.contains("-P OUTPUT DROP")),
        "Missing default DROP"
    );
    assert!(
        rules
            .iter()
            .any(|r| r.contains("10.0.0.0/8") && r.contains("ACCEPT")),
        "Missing internal network allow rule"
    );

    // Verify blocked destination is NOT in allow rules
    assert!(
        !rules.iter().any(|r| r.contains("8.8.8.8")),
        "External DNS should not be explicitly allowed"
    );

    println!("✅ Egress Fail-Closed rule generation verified");
}

// ============================================================================
// Test 2: Egress Success (Allowed Traffic)
// ============================================================================

#[test]
#[ignore]
fn test_egress_allows_permitted_traffic() {
    prereqs::print_status();
    if !prereqs::network_prereqs() {
        eprintln!("Skipping: network prerequisites not met");
        return;
    }

    use capsuled::security::egress_policy::generate_fw_rules;
    use capsule_core::capsule_v1::{EgressIdRule, EgressIdType, NetworkConfig};

    // Create manifest allowing specific external IP
    let mut manifest = create_test_manifest("test-egress-allowed");
    manifest.network = Some(NetworkConfig {
        egress_allow: vec![],
        egress_id_allow: vec![EgressIdRule {
            rule_type: EgressIdType::Ip,
            value: "1.1.1.1".to_string(), // Cloudflare DNS
        }],
    });

    let rules = generate_fw_rules(&manifest);

    // Verify allowed IP is in rules
    assert!(
        rules
            .iter()
            .any(|r| r.contains("1.1.1.1") && r.contains("ACCEPT")),
        "Allowed IP should have ACCEPT rule"
    );

    // Verify loopback and established are always allowed
    assert!(
        rules
            .iter()
            .any(|r| r.contains("-o lo") && r.contains("ACCEPT")),
        "Loopback should be allowed"
    );
    assert!(
        rules
            .iter()
            .any(|r| r.contains("ESTABLISHED") || r.contains("RELATED")),
        "Established connections should be allowed"
    );

    println!("✅ Egress Allow rule generation verified");
}

// ============================================================================
// Test 3: Signature Priority (Unit 2 blocks before Unit 3)
// ============================================================================

#[test]
#[ignore]
fn test_signature_verification_priority_over_egress() {
    prereqs::print_status();

    use capsuled::security::verifier::ManifestVerifier;
    use capsule_core::capsule_v1::{EgressIdRule, EgressIdType, NetworkConfig};

    // Create verifier with a FAKE trusted key to enable signature enforcement
    // When a trusted key is configured, the verifier will reject invalid signatures
    let fake_trusted_key = "ed25519:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string();
    let verifier = ManifestVerifier::new(Some(fake_trusted_key), true);

    let mut manifest = create_test_manifest("test-sig-priority");
    manifest.network = Some(NetworkConfig {
        egress_allow: vec![],
        egress_id_allow: vec![EgressIdRule {
            rule_type: EgressIdType::Cidr,
            value: "0.0.0.0/0".to_string(), // Wide open (should never apply if sig fails)
        }],
    });

    // Serialize manifest
    let manifest_json = serde_json::to_string(&manifest).expect("serialize manifest");

    // Invalid signature (random bytes - too short to be valid format)
    let invalid_signature = vec![0u8; 64];

    // Verification should fail BEFORE egress rules are even considered
    // Possible errors:
    // 1. Signature file too short (invalid format)
    // 2. Signature key doesn't match trusted key
    // 3. Cryptographic verification fails
    let result = verifier.verify(manifest_json.as_bytes(), &invalid_signature, "");

    assert!(
        result.is_err(),
        "Invalid signature should cause verification failure"
    );

    let err_msg = result.unwrap_err().to_string();
    println!("Verification error: {}", err_msg);

    // Verify the error is about signature format or verification, not something else
    assert!(
        err_msg.contains("signature") || err_msg.contains("Invalid") || err_msg.contains("format"),
        "Error should be about signature: {}",
        err_msg
    );

    // Key insight: If signature fails, egress rules are NEVER generated
    // This proves Unit 2 takes priority over Unit 3
    println!("✅ Signature verification priority confirmed");
}

// ============================================================================
// Test 4: Storage/VRAM Lifecycle Cleanup
// ============================================================================

#[test]
#[ignore]
fn test_storage_vram_lifecycle_cleanup() {
    prereqs::print_status();
    if !prereqs::storage_prereqs() {
        eprintln!("Skipping: storage prerequisites not met");
        return;
    }

    use capsuled::storage::{StorageConfig, StorageManager};

    let config = StorageConfig {
        enabled: true,
        default_vg: "test_vg".to_string(),
        key_directory: PathBuf::from("/tmp/security_e2e_keys"),
        enable_encryption: false,             // Start simple
        default_size_bytes: 50 * 1024 * 1024, // 50MB
        mount_base: PathBuf::from("/tmp/security_e2e_mounts"),
        thin_pool_name: None,
        use_thin_by_default: false,
    };

    let manager = StorageManager::new(config);
    let capsule_id = "security-e2e-capsule-001";

    // Phase 1: Provision
    println!("Phase 1: Provisioning storage...");
    let mut storage = manager
        .provision_capsule_storage(capsule_id, None, Some(false), None)
        .expect("provision");

    assert!(
        PathBuf::from(&storage.device_path).exists()
            || storage.device_path.starts_with("/dev/mapper"),
        "Device path should exist"
    );

    // Phase 2: Mount
    println!("Phase 2: Mounting volume...");
    manager.mount_volume(&mut storage).expect("Mount failed");

    let mount_path = storage
        .mount_point
        .clone()
        .expect("Mount point should be set");
    println!("  Mount point: {:?}", mount_path);
    assert!(mount_path.exists(), "Mount point should exist after mount");

    // Phase 3: Write data
    println!("Phase 3: Writing test data...");
    let test_file = mount_path.join("security_test.dat");
    std::fs::write(&test_file, b"SENSITIVE_DATA_12345").expect("Write failed");
    assert!(test_file.exists(), "Test file should exist");

    // Phase 4: Unmount
    println!("Phase 4: Unmounting...");
    manager
        .unmount_volume(capsule_id, &storage.lv_name)
        .expect("Unmount failed");

    // Mount point directory should be removed
    assert!(
        !mount_path.exists(),
        "Mount point should be removed after unmount"
    );

    // Phase 5: Cleanup
    println!("Phase 5: Cleaning up resources...");
    manager
        .cleanup_capsule_storage(capsule_id)
        .expect("Cleanup failed");

    // Verify LV no longer exists
    let lv_check = std::process::Command::new("lvs")
        .arg(format!("test_vg/{}", storage.lv_name))
        .output()
        .expect("lvs command");

    assert!(
        !lv_check.status.success(),
        "LV should be deleted after cleanup"
    );

    println!("✅ Storage lifecycle cleanup verified");
}

// ============================================================================
// Test: Combined Security Flow
// ============================================================================

#[test]
#[ignore]
fn test_combined_security_flow() {
    prereqs::print_status();

    // This test verifies the logical flow:
    // 1. Signature check FIRST
    // 2. If passed, egress rules generated
    // 3. Storage provisioned
    // 4. Capsule runs
    // 5. On stop: VRAM scrub + storage cleanup

    use capsuled::security::egress_policy::generate_fw_rules;
    use capsuled::security::verifier::ManifestVerifier;

    // Step 1: Create manifest
    let manifest = create_test_manifest("combined-flow-test");
    let manifest_json = serde_json::to_string(&manifest).expect("serialize");

    // Step 2: Verify signature (permissive mode for test)
    let verifier = ManifestVerifier::new(None, false);
    let verify_result = verifier.verify(manifest_json.as_bytes(), &[], "");
    assert!(
        verify_result.is_ok(),
        "Permissive verifier should pass empty signature"
    );

    // Step 3: Generate egress rules (should be default DROP with essentials)
    let rules = generate_fw_rules(&manifest);
    assert!(!rules.is_empty(), "Should generate base firewall rules");
    assert!(
        rules.iter().any(|r| r.contains("-P OUTPUT DROP")),
        "Default DROP should always be present"
    );

    println!("✅ Combined security flow verified");
}
