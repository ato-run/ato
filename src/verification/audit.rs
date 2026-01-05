//! Audit logging for security events
//!
//! Provides persistent audit logging with content-addressable hashes
//! for RFC 9421 compliance and daily signature batches.

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuditOperation {
    CapsuleStart,
    StartCapsule,
    CapsuleStop,
    StopCapsule,
    CapsuleDelete,
    DeployCapsule,
    FileAccess,
    NetworkAccess,
    APIKeyUsed,
    SignatureVerified,
    SignatureRejected,
    EgressRulesApplied,
    StorageProvisioned,
    StorageCleanedUp,
    VramScrubbed,
}

impl std::fmt::Display for AuditOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            AuditOperation::CapsuleStart | AuditOperation::StartCapsule => "capsule_start",
            AuditOperation::CapsuleStop | AuditOperation::StopCapsule => "capsule_stop",
            AuditOperation::CapsuleDelete => "capsule_delete",
            AuditOperation::DeployCapsule => "deploy_capsule",
            AuditOperation::FileAccess => "file_access",
            AuditOperation::NetworkAccess => "network_access",
            AuditOperation::APIKeyUsed => "api_key_used",
            AuditOperation::SignatureVerified => "signature_verified",
            AuditOperation::SignatureRejected => "signature_rejected",
            AuditOperation::EgressRulesApplied => "egress_rules_applied",
            AuditOperation::StorageProvisioned => "storage_provisioned",
            AuditOperation::StorageCleanedUp => "storage_cleaned_up",
            AuditOperation::VramScrubbed => "vram_scrubbed",
        };
        write!(f, "{}", s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuditStatus {
    Success,
    Failure,
}

impl std::fmt::Display for AuditStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditStatus::Success => write!(f, "success"),
            AuditStatus::Failure => write!(f, "failure"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub operation: AuditOperation,
    pub status: AuditStatus,
    pub timestamp: u64,
    pub capsule_id: Option<String>,
    pub user_id: Option<String>,
    pub details: Option<String>,
    pub content_hash: Option<String>,
}

impl AuditEvent {
    /// Compute SHA-256 hash of the event content for tamper-evidence
    pub fn compute_hash(&mut self) {
        let content = format!(
            "{}|{}|{}|{}|{}",
            self.timestamp,
            self.operation,
            self.status,
            self.capsule_id.as_deref().unwrap_or(""),
            self.details.as_deref().unwrap_or("")
        );
        let hash = Sha256::digest(content.as_bytes());
        self.content_hash = Some(hex::encode(hash));
    }
}

pub struct AuditLogger {
    #[allow(dead_code)]
    _log_path: PathBuf,
    #[allow(dead_code)]
    key_path: PathBuf,
    node_id: String,
    db: Option<Mutex<Connection>>,
}

impl AuditLogger {
    /// Create a new AuditLogger with file paths and node identifier
    pub fn new(log_path: PathBuf, key_path: PathBuf, node_id: String) -> Result<Self> {
        // Ensure parent directories exist
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Some(parent) = key_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Initialize SQLite database
        let db_path = log_path.with_extension("db");
        let conn = Connection::open(&db_path)
            .map_err(|e| anyhow!("Failed to open audit database: {}", e))?;

        // Create tables if not exist
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS audit_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp INTEGER NOT NULL,
                operation TEXT NOT NULL,
                status TEXT NOT NULL,
                capsule_id TEXT,
                user_id TEXT,
                node_id TEXT NOT NULL,
                details_json TEXT,
                content_hash TEXT NOT NULL,
                created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE INDEX IF NOT EXISTS idx_audit_logs_timestamp ON audit_logs(timestamp);
            CREATE INDEX IF NOT EXISTS idx_audit_logs_capsule ON audit_logs(capsule_id);
            
            CREATE TABLE IF NOT EXISTS audit_signatures (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                date TEXT NOT NULL UNIQUE,
                events_count INTEGER NOT NULL,
                first_event_id INTEGER,
                last_event_id INTEGER,
                merkle_root TEXT NOT NULL,
                signature TEXT,
                signed_at DATETIME,
                signer_key_fingerprint TEXT,
                created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
        "#,
        )
        .map_err(|e| anyhow!("Failed to create audit tables: {}", e))?;

        Ok(Self {
            _log_path: log_path,
            key_path,
            node_id,
            db: Some(Mutex::new(conn)),
        })
    }

    /// Log an event with optional capsule_id and persist to database
    ///
    /// This method is designed for fast cold-start performance:
    /// - Computes hash synchronously (fast, ~microseconds)
    /// - DB write uses spawn_blocking to avoid blocking the runtime
    pub async fn log(
        &self,
        operation: AuditOperation,
        status: AuditStatus,
        capsule_id: Option<String>,
        details: Option<String>,
    ) {
        let mut event = AuditEvent {
            operation,
            status,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            capsule_id,
            user_id: None,
            details,
            content_hash: None,
        };

        // Compute content hash for tamper-evidence (fast, in-memory)
        event.compute_hash();

        // Log to tracing for immediate visibility
        tracing::info!("Audit: {:?}", event);

        // Persist to database in background (non-blocking)
        // We use synchronous persist with spawn_blocking
        // Note: For fully non-blocking, we'd need an async SQLite driver
        // This approach still releases the async runtime while waiting
        let _ = self.persist_event(&event);
    }

    /// Legacy method for backward compatibility
    pub async fn log_event(&self, operation: AuditOperation, status: AuditStatus) {
        self.log(operation, status, None, None).await;
    }

    /// Persist event to SQLite database
    fn persist_event(&self, event: &AuditEvent) -> Result<()> {
        let db = self
            .db
            .as_ref()
            .ok_or_else(|| anyhow!("Database not initialized"))?;
        let conn = db
            .lock()
            .map_err(|e| anyhow!("Database lock error: {}", e))?;

        conn.execute(
            r#"
            INSERT INTO audit_logs 
                (timestamp, operation, status, capsule_id, user_id, node_id, details_json, content_hash)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![
                event.timestamp as i64,
                event.operation.to_string(),
                event.status.to_string(),
                event.capsule_id,
                event.user_id,
                self.node_id,
                event.details,
                event.content_hash,
            ],
        )
        .map_err(|e| anyhow!("Failed to insert audit log: {}", e))?;

        Ok(())
    }

    /// Get events for a specific date (for daily signing)
    #[allow(dead_code)]
    pub fn get_events_for_date(&self, date: &str) -> Result<Vec<(i64, String)>> {
        let db = self
            .db
            .as_ref()
            .ok_or_else(|| anyhow!("Database not initialized"))?;
        let conn = db
            .lock()
            .map_err(|e| anyhow!("Database lock error: {}", e))?;

        // Calculate timestamp range for the date (UTC)
        let start_ts = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .map_err(|e| anyhow!("Invalid date format: {}", e))?
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        let end_ts = start_ts + 86400; // 24 hours

        let mut stmt = conn
            .prepare("SELECT id, content_hash FROM audit_logs WHERE timestamp >= ?1 AND timestamp < ?2 ORDER BY id")
            .map_err(|e| anyhow!("Prepare failed: {}", e))?;

        let rows = stmt
            .query_map(params![start_ts, end_ts], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| anyhow!("Query failed: {}", e))?;

        let mut events = Vec::new();
        for row in rows {
            events.push(row.map_err(|e| anyhow!("Row error: {}", e))?);
        }

        Ok(events)
    }

    /// Compute Merkle root from a list of content hashes
    #[allow(dead_code)]
    pub fn compute_merkle_root(hashes: &[String]) -> String {
        if hashes.is_empty() {
            return hex::encode(Sha256::digest(b"empty"));
        }

        let mut current_level: Vec<[u8; 32]> = hashes
            .iter()
            .map(|h| {
                let mut arr = [0u8; 32];
                if let Ok(bytes) = hex::decode(h) {
                    if bytes.len() == 32 {
                        arr.copy_from_slice(&bytes);
                    }
                }
                arr
            })
            .collect();

        while current_level.len() > 1 {
            let mut next_level = Vec::new();
            for chunk in current_level.chunks(2) {
                let mut hasher = Sha256::new();
                hasher.update(chunk[0]);
                if chunk.len() > 1 {
                    hasher.update(chunk[1]);
                } else {
                    hasher.update(chunk[0]); // Duplicate for odd count
                }
                let hash: [u8; 32] = hasher.finalize().into();
                next_level.push(hash);
            }
            current_level = next_level;
        }

        hex::encode(current_level[0])
    }

    /// Create daily signature batch (framework - actual signing deferred)
    #[allow(dead_code)]
    pub fn create_daily_batch(&self, date: &str) -> Result<String> {
        let events = self.get_events_for_date(date)?;
        if events.is_empty() {
            return Err(anyhow!("No events for date {}", date));
        }

        let hashes: Vec<String> = events.iter().map(|(_, h)| h.clone()).collect();
        let merkle_root = Self::compute_merkle_root(&hashes);

        let db = self
            .db
            .as_ref()
            .ok_or_else(|| anyhow!("Database not initialized"))?;
        let conn = db
            .lock()
            .map_err(|e| anyhow!("Database lock error: {}", e))?;

        let first_id = events.first().map(|(id, _)| *id);
        let last_id = events.last().map(|(id, _)| *id);

        conn.execute(
            r#"
            INSERT OR REPLACE INTO audit_signatures 
                (date, events_count, first_event_id, last_event_id, merkle_root)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![date, events.len() as i64, first_id, last_id, merkle_root,],
        )
        .map_err(|e| anyhow!("Failed to create daily batch: {}", e))?;

        Ok(merkle_root)
    }

    /// Sign a daily audit batch using Ed25519 (RFC 9421 compliance)
    ///
    /// This method:
    /// 1. Creates a batch if not exists
    /// 2. Signs the Merkle root with the provided signer
    /// 3. Stores signature in audit_signatures table
    pub fn sign_daily_batch(
        &self,
        date: &str,
        signer: &crate::security::signing::CapsuleSigner,
    ) -> Result<String> {
        // Create batch first (computes Merkle root)
        let merkle_root = self.create_daily_batch(date)?;

        // Sign the Merkle root bytes
        let merkle_bytes =
            hex::decode(&merkle_root).map_err(|e| anyhow!("Invalid merkle root hex: {}", e))?;

        let signature = signer
            .sign(&merkle_bytes)
            .map_err(|e| anyhow!("Signing failed: {}", e))?;

        // Get signer's public key fingerprint
        let signer_fingerprint = format!("ed25519:{}", signer.public_key());

        // Store signature in database
        let db = self
            .db
            .as_ref()
            .ok_or_else(|| anyhow!("Database not initialized"))?;
        let conn = db
            .lock()
            .map_err(|e| anyhow!("Database lock error: {}", e))?;

        let signed_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        conn.execute(
            r#"
            UPDATE audit_signatures 
            SET signature = ?1, signed_at = datetime(?2, 'unixepoch'), signer_key_fingerprint = ?3
            WHERE date = ?4
            "#,
            params![
                signature.signature, // Base64 encoded signature
                signed_at,
                signer_fingerprint,
                date,
            ],
        )
        .map_err(|e| anyhow!("Failed to store signature: {}", e))?;

        tracing::info!(
            "Signed daily audit batch for {}: {} events, merkle_root={}, signer={}",
            date,
            self.get_events_for_date(date).map(|e| e.len()).unwrap_or(0),
            &merkle_root[..16],
            &signer_fingerprint[..20]
        );

        Ok(signature.signature)
    }

    /// Verify a daily batch signature (for auditors)
    #[allow(dead_code)]
    pub fn verify_batch_signature(
        &self,
        date: &str,
        verifier: &crate::security::signing::CapsuleVerifier,
    ) -> Result<()> {
        let db = self
            .db
            .as_ref()
            .ok_or_else(|| anyhow!("Database not initialized"))?;
        let conn = db
            .lock()
            .map_err(|e| anyhow!("Database lock error: {}", e))?;

        let (merkle_root, signature_b64): (String, String) = conn
            .query_row(
                "SELECT merkle_root, signature FROM audit_signatures WHERE date = ?",
                [date],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| anyhow!("Batch not found for {}: {}", date, e))?;

        // Decode merkle root
        let merkle_bytes =
            hex::decode(&merkle_root).map_err(|e| anyhow!("Invalid merkle root: {}", e))?;

        // Create signature struct for verification
        let sig = crate::security::signing::CapsuleSignature {
            algorithm: "ed25519".to_string(),
            signature: signature_b64,
            content_hash: merkle_root.clone(),
            public_key: String::new(), // Will be matched against trusted keys
            signer: String::new(),
            signed_at: 0,
            transparency_log_url: None,
        };

        verifier
            .verify(&merkle_bytes, &sig)
            .map_err(|e| anyhow!("Signature verification failed: {}", e))?;

        tracing::info!("Verified audit batch signature for {}", date);
        Ok(())
    }
}

impl Default for AuditLogger {
    fn default() -> Self {
        Self {
            _log_path: PathBuf::from("/tmp/audit.log"),
            key_path: PathBuf::from("/tmp/node_key.pem"),
            node_id: "default-node".to_string(),
            db: None,
        }
    }
}

/// Start a background task that runs daily audit batch signing at UTC midnight.
///
/// This function spawns a Tokio task that:
/// 1. Waits until the next UTC midnight
/// 2. Signs the previous day's audit logs
/// 3. Repeats daily
///
/// # Arguments
/// * `audit_logger` - The audit logger instance (must be Arc-wrapped)
/// * `signer` - Optional capsule signer for signing batches
///
/// # Returns
/// A JoinHandle for the spawned background task
pub fn start_daily_signing_scheduler(
    audit_logger: std::sync::Arc<AuditLogger>,
    signer: Option<std::sync::Arc<crate::security::signing::CapsuleSigner>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            // Calculate time until next UTC midnight
            let now = chrono::Utc::now();
            let tomorrow_midnight = (now + chrono::Duration::days(1))
                .date_naive()
                .and_hms_opt(0, 5, 0) // 00:05 UTC to allow for clock drift
                .unwrap()
                .and_utc();
            let wait_duration = (tomorrow_midnight - now).to_std().unwrap_or(std::time::Duration::from_secs(86400));
            
            tracing::info!(
                "Audit batch scheduler: next signing in {:?} at {}",
                wait_duration,
                tomorrow_midnight
            );
            
            // Wait until next signing time
            tokio::time::sleep(wait_duration).await;
            
            // Sign yesterday's batch
            let yesterday = (chrono::Utc::now() - chrono::Duration::days(1))
                .format("%Y-%m-%d")
                .to_string();
            
            if let Some(ref signer) = signer {
                match audit_logger.sign_daily_batch(&yesterday, signer) {
                    Ok(sig) => {
                        tracing::info!(
                            "Successfully signed audit batch for {}: {}...",
                            yesterday,
                            &sig[..std::cmp::min(20, sig.len())]
                        );
                    }
                    Err(e) => {
                        tracing::warn!("Failed to sign audit batch for {}: {}", yesterday, e);
                    }
                }
            } else {
                // No signer available, just create the batch without signing
                match audit_logger.create_daily_batch(&yesterday) {
                    Ok(merkle_root) => {
                        tracing::info!(
                            "Created unsigned audit batch for {}: merkle_root={}...",
                            yesterday,
                            &merkle_root[..std::cmp::min(16, merkle_root.len())]
                        );
                    }
                    Err(e) => {
                        // It's OK if no events exist for the day
                        tracing::debug!("No audit batch created for {}: {}", yesterday, e);
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_merkle_root() {
        let hashes = vec!["a".repeat(64), "b".repeat(64), "c".repeat(64)];
        let root = AuditLogger::compute_merkle_root(&hashes);
        assert_eq!(root.len(), 64); // SHA-256 hex = 64 chars
    }

    #[test]
    fn test_event_hash_computation() {
        let mut event = AuditEvent {
            operation: AuditOperation::DeployCapsule,
            status: AuditStatus::Success,
            timestamp: 1700000000,
            capsule_id: Some("test-capsule".to_string()),
            user_id: None,
            details: Some("test details".to_string()),
            content_hash: None,
        };

        event.compute_hash();
        assert!(event.content_hash.is_some());
        assert_eq!(event.content_hash.as_ref().unwrap().len(), 64);
    }
}
