#![cfg(feature = "legacy-manifest-tests")]
// Legacy artifact/CAS tests moved to capsule-cli.

//! Unit 6.1: CAS Resolution & Audit Log Persistence E2E Tests
//!
//! These tests verify that:
//! 1. CAS URI resolution works correctly
//! 2. Audit logs are persisted to SQLite with content hashes
//!
//! To run:
//! ```bash
//! cargo test --test audit_cas_e2e -- --test-threads=1
//! ```

use sha2::{Digest, Sha256};
use tempfile::TempDir;

// ============================================================================
// Test 1: CAS URI Resolution
// ============================================================================

#[tokio::test]
async fn test_cas_uri_resolution_success() {
    use nacelle::artifact::manager::{ArtifactConfig, ArtifactManager};

    // Create temp CAS directory structure
    let tmp = TempDir::new().expect("tempdir");
    let cas_root = tmp.path().join("cas");

    // Create a dummy blob with known content
    let blob_content = b"This is test blob content for CAS verification";
    let hash = format!("{:x}", Sha256::digest(blob_content));

    // CAS layout: blobs/<prefix>/<hash>
    let prefix = &hash[0..2];
    let blob_dir = cas_root.join("blobs").join(prefix);
    std::fs::create_dir_all(&blob_dir).expect("create blob dir");

    let blob_path = blob_dir.join(&hash);
    std::fs::write(&blob_path, blob_content).expect("write blob");

    println!("Created CAS blob at: {}", blob_path.display());
    println!("Hash: {}", hash);

    // Configure ArtifactManager with CAS root
    let config = ArtifactConfig {
        registry_url: "file:///dev/null".to_string(),
        cache_path: tmp.path().join("cache"),
        cas_root: Some(cas_root.clone()),
    };

    let manager = ArtifactManager::new(config).await.expect("manager");

    // Resolve CAS URI
    let uri = format!("cas://{}", hash);
    let result = manager.resolve_cas_uri(&uri);

    assert!(
        result.is_ok(),
        "CAS resolution should succeed: {:?}",
        result.err()
    );

    let resolved_path = result.unwrap();
    assert!(resolved_path.exists(), "Resolved path should exist");

    // Verify content matches
    let read_content = std::fs::read(&resolved_path).expect("read blob");
    assert_eq!(read_content, blob_content, "Blob content should match");

    println!(
        "✅ CAS URI resolution verified: {} -> {}",
        uri,
        resolved_path.display()
    );
}

#[tokio::test]
async fn test_cas_uri_resolution_not_found() {
    use nacelle::artifact::manager::{ArtifactConfig, ArtifactError, ArtifactManager};

    let tmp = TempDir::new().expect("tempdir");
    let cas_root = tmp.path().join("cas");
    std::fs::create_dir_all(cas_root.join("blobs")).expect("create blobs dir");

    let config = ArtifactConfig {
        registry_url: "file:///dev/null".to_string(),
        cache_path: tmp.path().join("cache"),
        cas_root: Some(cas_root),
    };

    let manager = ArtifactManager::new(config).await.expect("manager");

    // Try to resolve a non-existent blob
    let fake_hash = "a".repeat(64);
    let uri = format!("cas://{}", fake_hash);
    let result = manager.resolve_cas_uri(&uri);

    assert!(
        result.is_err(),
        "Resolution should fail for non-existent blob"
    );

    match result.unwrap_err() {
        ArtifactError::NotFound(_) => println!("✅ Correctly returned NotFound error"),
        e => panic!("Expected NotFound, got: {:?}", e),
    }
}

#[tokio::test]
async fn test_cas_uri_validation() {
    use nacelle::artifact::manager::{ArtifactConfig, ArtifactError, ArtifactManager};

    let tmp = TempDir::new().expect("tempdir");

    let config = ArtifactConfig {
        registry_url: "file:///dev/null".to_string(),
        cache_path: tmp.path().join("cache"),
        cas_root: Some(tmp.path().to_path_buf()),
    };

    let manager = ArtifactManager::new(config).await.expect("manager");

    // Test invalid URI prefix
    let result = manager.resolve_cas_uri("https://example.com/blob");
    assert!(matches!(result, Err(ArtifactError::InvalidUri(_))));
    println!("✅ Invalid prefix rejected");

    // Test invalid hash length
    let result = manager.resolve_cas_uri("cas://tooshort");
    assert!(matches!(result, Err(ArtifactError::InvalidUri(_))));
    println!("✅ Invalid hash length rejected");

    // Test non-hex characters
    let result = manager.resolve_cas_uri(&format!("cas://{}", "g".repeat(64)));
    assert!(matches!(result, Err(ArtifactError::InvalidUri(_))));
    println!("✅ Non-hex characters rejected");
}

#[tokio::test]
async fn test_cas_root_not_configured() {
    use nacelle::artifact::manager::{ArtifactConfig, ArtifactError, ArtifactManager};

    let tmp = TempDir::new().expect("tempdir");

    let config = ArtifactConfig {
        registry_url: "file:///dev/null".to_string(),
        cache_path: tmp.path().join("cache"),
        cas_root: None, // Not configured
    };

    let manager = ArtifactManager::new(config).await.expect("manager");

    let result = manager.resolve_cas_uri(&format!("cas://{}", "a".repeat(64)));
    assert!(matches!(result, Err(ArtifactError::CasError(_))));
    println!("✅ CAS root not configured error returned");
}
