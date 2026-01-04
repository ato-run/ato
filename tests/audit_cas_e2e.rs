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
    use capsuled_engine::artifact::manager::{ArtifactConfig, ArtifactManager};

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
    use capsuled_engine::artifact::manager::{ArtifactConfig, ArtifactError, ArtifactManager};

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
    use capsuled_engine::artifact::manager::{ArtifactConfig, ArtifactError, ArtifactManager};

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
    use capsuled_engine::artifact::manager::{ArtifactConfig, ArtifactError, ArtifactManager};

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

// ============================================================================
// Test 2: Audit Log Persistence
// ============================================================================

#[tokio::test]
async fn test_audit_log_persistence() {
    use capsuled_engine::security::audit::{AuditLogger, AuditOperation, AuditStatus};

    let tmp = TempDir::new().expect("tempdir");
    let log_path = tmp.path().join("audit.log");
    let key_path = tmp.path().join("node_key.pem");

    let logger = AuditLogger::new(log_path.clone(), key_path, "test-node-001".to_string())
        .expect("create logger");

    // Log several events
    let events = [
        (
            AuditOperation::DeployCapsule,
            AuditStatus::Success,
            Some("test-capsule-001".to_string()),
        ),
        (
            AuditOperation::CapsuleStart,
            AuditStatus::Success,
            Some("test-capsule-001".to_string()),
        ),
        (
            AuditOperation::EgressRulesApplied,
            AuditStatus::Success,
            Some("test-capsule-001".to_string()),
        ),
        (
            AuditOperation::CapsuleStop,
            AuditStatus::Success,
            Some("test-capsule-001".to_string()),
        ),
        (
            AuditOperation::SignatureRejected,
            AuditStatus::Failure,
            Some("bad-capsule".to_string()),
        ),
    ];

    for (op, status, capsule_id) in events.iter() {
        logger
            .log(op.clone(), status.clone(), capsule_id.clone(), None)
            .await;
    }

    // Verify database was created
    let db_path = log_path.with_extension("db");
    assert!(
        db_path.exists(),
        "Audit database should be created at {:?}",
        db_path
    );

    // Query the database directly
    let conn = rusqlite::Connection::open(&db_path).expect("open db");

    // Count events
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM audit_logs", [], |row| row.get(0))
        .expect("count query");

    assert_eq!(count, 5, "Should have 5 audit events");

    // Verify each event has required fields
    let mut stmt = conn
        .prepare("SELECT operation, status, capsule_id, node_id, content_hash FROM audit_logs ORDER BY id")
        .expect("prepare");

    #[allow(clippy::type_complexity)]
    let rows: Vec<(String, String, Option<String>, String, Option<String>)> = stmt
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .expect("query")
        .map(|r| r.unwrap())
        .collect();

    assert_eq!(rows.len(), 5);

    // Verify first event
    assert_eq!(rows[0].0, "deploy_capsule");
    assert_eq!(rows[0].1, "success");
    assert_eq!(rows[0].2, Some("test-capsule-001".to_string()));
    assert_eq!(rows[0].3, "test-node-001");

    // Verify content_hash is SHA-256 (64 hex chars)
    let hash = rows[0].4.as_ref().expect("hash should exist");
    assert_eq!(hash.len(), 64, "Hash should be 64 hex characters");
    assert!(
        hash.chars().all(|c| c.is_ascii_hexdigit()),
        "Hash should be hex"
    );

    // Verify failure event
    assert_eq!(rows[4].0, "signature_rejected");
    assert_eq!(rows[4].1, "failure");
    assert_eq!(rows[4].2, Some("bad-capsule".to_string()));

    println!(
        "✅ Audit log persistence verified: {} events with content hashes",
        count
    );
}

#[tokio::test]
async fn test_audit_content_hash_uniqueness() {
    use capsuled_engine::security::audit::{AuditLogger, AuditOperation, AuditStatus};

    let tmp = TempDir::new().expect("tempdir");
    let log_path = tmp.path().join("audit_unique.log");
    let key_path = tmp.path().join("node_key.pem");

    let logger = AuditLogger::new(log_path.clone(), key_path, "node-hash-test".to_string())
        .expect("create logger");

    // Log events with different details - should have different hashes
    logger
        .log(
            AuditOperation::DeployCapsule,
            AuditStatus::Success,
            Some("capsule-a".to_string()),
            Some("details-1".to_string()),
        )
        .await;

    logger
        .log(
            AuditOperation::DeployCapsule,
            AuditStatus::Success,
            Some("capsule-b".to_string()),
            Some("details-2".to_string()),
        )
        .await;

    // Query hashes
    let db_path = log_path.with_extension("db");
    let conn = rusqlite::Connection::open(&db_path).expect("open db");

    let hashes: Vec<String> = conn
        .prepare("SELECT content_hash FROM audit_logs")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .map(|r| r.unwrap())
        .collect();

    assert_eq!(hashes.len(), 2);
    assert_ne!(
        hashes[0], hashes[1],
        "Different events should have different hashes"
    );

    println!("✅ Content hash uniqueness verified");
}

#[tokio::test]
async fn test_audit_merkle_root_computation() {
    use capsuled_engine::security::audit::AuditLogger;

    // Test Merkle root computation
    let hashes = vec![
        "a".repeat(64),
        "b".repeat(64),
        "c".repeat(64),
        "d".repeat(64),
    ];

    let root = AuditLogger::compute_merkle_root(&hashes);

    assert_eq!(root.len(), 64, "Merkle root should be 64 hex chars");
    assert!(
        root.chars().all(|c| c.is_ascii_hexdigit()),
        "Root should be hex"
    );

    // Verify determinism
    let root2 = AuditLogger::compute_merkle_root(&hashes);
    assert_eq!(root, root2, "Merkle root should be deterministic");

    // Verify different input gives different root
    let hashes2 = vec!["x".repeat(64)];
    let root3 = AuditLogger::compute_merkle_root(&hashes2);
    assert_ne!(root, root3, "Different inputs should give different roots");

    println!("✅ Merkle root computation verified");
}

// ============================================================================
// Test 3: Combined CAS + Audit Flow
// ============================================================================

#[tokio::test]
async fn test_combined_cas_audit_flow() {
    use capsuled_engine::artifact::manager::{ArtifactConfig, ArtifactManager};
    use capsuled_engine::security::audit::{AuditLogger, AuditOperation, AuditStatus};

    let tmp = TempDir::new().expect("tempdir");

    // Setup CAS
    let cas_root = tmp.path().join("cas");
    let blob_content = b"artifact-blob-for-combined-test";
    let hash = format!("{:x}", Sha256::digest(blob_content));
    let prefix = &hash[0..2];
    let blob_dir = cas_root.join("blobs").join(prefix);
    std::fs::create_dir_all(&blob_dir).expect("create blob dir");
    std::fs::write(blob_dir.join(&hash), blob_content).expect("write blob");

    // Setup Audit Logger
    let log_path = tmp.path().join("combined.log");
    let logger = AuditLogger::new(
        log_path.clone(),
        tmp.path().join("key.pem"),
        "combined-test-node".to_string(),
    )
    .expect("logger");

    // Setup Artifact Manager
    let config = ArtifactConfig {
        registry_url: "file:///dev/null".to_string(),
        cache_path: tmp.path().join("cache"),
        cas_root: Some(cas_root),
    };
    let manager = ArtifactManager::new(config).await.expect("manager");

    // Simulate: Resolve CAS artifact and log the action
    let uri = format!("cas://{}", hash);
    let resolved = manager.resolve_cas_uri(&uri).expect("resolve");

    // Log artifact resolution as audit event
    logger
        .log(
            AuditOperation::DeployCapsule,
            AuditStatus::Success,
            Some("cas-capsule".to_string()),
            Some(format!(
                "Resolved artifact: {} -> {}",
                uri,
                resolved.display()
            )),
        )
        .await;

    // Verify audit log captured the event
    let db_path = log_path.with_extension("db");
    let conn = rusqlite::Connection::open(&db_path).expect("open db");

    let details: String = conn
        .query_row(
            "SELECT details_json FROM audit_logs WHERE capsule_id = ?",
            ["cas-capsule"],
            |row| row.get(0),
        )
        .expect("query");

    assert!(details.contains(&hash), "Audit log should contain CAS hash");
    assert!(
        details.contains("Resolved artifact"),
        "Audit log should mention resolution"
    );

    println!("✅ Combined CAS + Audit flow verified");
}

// ============================================================================
// Test 4: Audit Batch Signing (RFC 9421)
// ============================================================================

#[tokio::test]
async fn test_audit_batch_signing() {
    use capsuled_engine::security::audit::{AuditLogger, AuditOperation, AuditStatus};
    use capsuled_engine::security::signing::CapsuleSigner;

    let tmp = TempDir::new().expect("tempdir");
    let log_path = tmp.path().join("signed_audit.log");
    let key_path = tmp.path().join("node_key.pem");

    let logger = AuditLogger::new(log_path.clone(), key_path, "signing-test-node".to_string())
        .expect("create logger");

    // Log events for today
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    logger
        .log(
            AuditOperation::DeployCapsule,
            AuditStatus::Success,
            Some("signed-capsule".to_string()),
            Some("test deploy".to_string()),
        )
        .await;

    logger
        .log(
            AuditOperation::CapsuleStart,
            AuditStatus::Success,
            Some("signed-capsule".to_string()),
            Some("test start".to_string()),
        )
        .await;

    logger
        .log(
            AuditOperation::CapsuleStop,
            AuditStatus::Success,
            Some("signed-capsule".to_string()),
            Some("test stop".to_string()),
        )
        .await;

    // Create signer
    let signer = CapsuleSigner::new("audit-batch-signer");

    // Sign the daily batch
    let signature = logger
        .sign_daily_batch(&today, &signer)
        .expect("signing should succeed");

    assert!(!signature.is_empty(), "Signature should not be empty");

    // Verify signature was stored in database
    let db_path = log_path.with_extension("db");
    let conn = rusqlite::Connection::open(&db_path).expect("open db");

    let (stored_merkle, stored_sig, stored_fingerprint): (String, String, String) = conn
        .query_row(
            "SELECT merkle_root, signature, signer_key_fingerprint FROM audit_signatures WHERE date = ?",
            [&today],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("query signature");

    assert!(!stored_merkle.is_empty(), "Merkle root should be stored");
    assert_eq!(
        stored_sig, signature,
        "Stored signature should match returned"
    );
    assert!(
        stored_fingerprint.starts_with("ed25519:"),
        "Fingerprint should have ed25519 prefix"
    );

    // Verify events count
    let events_count: i64 = conn
        .query_row(
            "SELECT events_count FROM audit_signatures WHERE date = ?",
            [&today],
            |row| row.get(0),
        )
        .expect("query count");

    assert_eq!(events_count, 3, "Should have 3 events in batch");

    println!(
        "✅ Audit batch signing verified: {} events, signature stored",
        events_count
    );
    println!("   Merkle root: {}...", &stored_merkle[..16]);
    println!("   Signer: {}", stored_fingerprint);
}
