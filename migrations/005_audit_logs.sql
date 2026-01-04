-- Audit log table for security event persistence
-- RFC 9421 compliance: content-addressable logs with daily signatures

CREATE TABLE IF NOT EXISTS audit_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp INTEGER NOT NULL,           -- Unix epoch seconds
    operation TEXT NOT NULL,              -- deploy_capsule, capsule_start, capsule_stop, etc.
    status TEXT NOT NULL,                 -- success, failure
    capsule_id TEXT,                      -- Associated capsule (nullable for system events)
    user_id TEXT,                         -- User/principal ID if available
    node_id TEXT NOT NULL,                -- Engine node identifier
    details_json TEXT,                    -- Additional structured details
    content_hash TEXT NOT NULL,           -- SHA-256 of (timestamp|operation|status|capsule_id|details)
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_audit_logs_timestamp ON audit_logs(timestamp);
CREATE INDEX IF NOT EXISTS idx_audit_logs_capsule ON audit_logs(capsule_id);
CREATE INDEX IF NOT EXISTS idx_audit_logs_operation ON audit_logs(operation);

-- Daily signature batches for tamper-evidence
-- Each day's logs are aggregated into a Merkle tree and signed
CREATE TABLE IF NOT EXISTS audit_signatures (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    date TEXT NOT NULL UNIQUE,            -- YYYY-MM-DD (UTC)
    events_count INTEGER NOT NULL,        -- Number of events in batch
    first_event_id INTEGER,               -- Reference to first audit_logs.id
    last_event_id INTEGER,                -- Reference to last audit_logs.id
    merkle_root TEXT NOT NULL,            -- SHA-256 Merkle root of content_hashes
    signature TEXT,                       -- Ed25519 signature of merkle_root (base64)
    signed_at DATETIME,                   -- When signature was created
    signer_key_fingerprint TEXT,          -- ed25519:<base64> fingerprint
    created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_audit_signatures_date ON audit_signatures(date);
