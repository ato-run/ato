//! Selfhost JSON-first SQLite Coordinator — storage slice (G3).
//!
//! This crate is the beginning of an independent, resident Coordinator that
//! owns DataResources in SQLite instead of D1. This slice implements the part
//! every surface depends on and that must not be re-invented per transport:
//!
//! - one authority per Resource, with a monotonic head revision;
//! - compare-and-set commits (expected_revision + expected_generation);
//! - exactly-once effects per operation_id: the same operation and payload
//!   returns the stored result, a different payload is a conflict;
//! - a receipt written in the same transaction as the revision;
//! - restart persistence (the database file is the authority).
//!
//! The HTTP surface lives in [`http`]; Ports/Bindings, operation grants and the
//! conformance suite against the hosted D1 implementation are follow-up slices.
//! Nothing here is wired into the CLI or Desktop yet.

pub mod apps;
pub mod handlers;
pub mod http;
pub mod relay;
pub mod service;

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Bumped when the on-disk schema changes in a way that needs a migration.
pub const SCHEMA_VERSION: i64 = 5;

/// Default per-owner quota (64 MiB) and operation-result retention (30 days).
pub const DEFAULT_QUOTA_BYTES: i64 = 64 * 1024 * 1024;
pub const DEFAULT_RETENTION_SECONDS: i64 = 30 * 24 * 60 * 60;

/// One document may not exceed 16 MiB, and a Resource may not accumulate more
/// than 16 active bindings; both bound what one caller can make the Coordinator
/// store.
pub const DEFAULT_MAX_DOCUMENT_BYTES: i64 = 16 * 1024 * 1024;
pub const DEFAULT_MAX_BINDINGS: i64 = 16;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS resources (
  id TEXT PRIMARY KEY,
  owner_id TEXT NOT NULL,
  protocol_ref TEXT NOT NULL,
  display_name TEXT,
  lifecycle TEXT NOT NULL DEFAULT 'independent'
    CHECK (lifecycle IN ('independent', 'instance_bound')),
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'deleted')),
  access_epoch INTEGER NOT NULL DEFAULT 0 CHECK (access_epoch >= 0),
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS document_revisions (
  resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
  revision INTEGER NOT NULL CHECK (revision > 0),
  generation INTEGER NOT NULL DEFAULT 0 CHECK (generation >= 0),
  document_json TEXT NOT NULL,
  content_digest TEXT NOT NULL,
  byte_size INTEGER NOT NULL CHECK (byte_size >= 0),
  created_at INTEGER NOT NULL,
  PRIMARY KEY (resource_id, revision)
);
CREATE TABLE IF NOT EXISTS document_heads (
  resource_id TEXT PRIMARY KEY REFERENCES resources(id) ON DELETE CASCADE,
  head_revision INTEGER NOT NULL,
  generation INTEGER NOT NULL DEFAULT 0 CHECK (generation >= 0),
  updated_at INTEGER NOT NULL,
  FOREIGN KEY (resource_id, head_revision)
    REFERENCES document_revisions(resource_id, revision) ON DELETE RESTRICT
);
CREATE TABLE IF NOT EXISTS grants (
  id TEXT PRIMARY KEY,
  resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
  subject_kind TEXT NOT NULL CHECK (subject_kind IN ('user', 'instance')),
  subject_id TEXT NOT NULL,
  can_read INTEGER NOT NULL DEFAULT 0,
  can_write INTEGER NOT NULL DEFAULT 0,
  can_connect INTEGER NOT NULL DEFAULT 0,
  can_manage INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'revoked')),
  grant_epoch INTEGER NOT NULL DEFAULT 0 CHECK (grant_epoch >= 0),
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS operation_results (
  resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
  operation_id TEXT NOT NULL,
  fingerprint TEXT NOT NULL,
  result_json TEXT NOT NULL,
  receipt_json TEXT NOT NULL,
  revision INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (resource_id, operation_id)
);
CREATE TABLE IF NOT EXISTS outbox (
  resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
  revision INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (resource_id, revision)
);
CREATE TABLE IF NOT EXISTS protocol_operations (
  protocol_ref TEXT NOT NULL,
  operation TEXT NOT NULL,
  semantics TEXT NOT NULL CHECK (semantics IN ('evolution', 'observation')),
  access TEXT NOT NULL CHECK (access IN ('read', 'read_write')),
  implementation_digest TEXT NOT NULL,
  publisher TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  module_source TEXT,
  module_export TEXT,
  expose_mcp INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (protocol_ref, operation)
);
-- A registered operation is immutable: its definition, handler and publisher
-- cannot be swapped after the fact. Exposure (MCP visibility) is the only
-- mutable column.
CREATE TRIGGER IF NOT EXISTS protocol_operation_immutable
BEFORE UPDATE OF protocol_ref, operation, semantics, access, implementation_digest,
  module_source, module_export, publisher ON protocol_operations
BEGIN SELECT RAISE(ABORT, 'protocol_operation_immutable'); END;
CREATE TRIGGER IF NOT EXISTS protocol_operation_claim_guard
BEFORE INSERT ON protocol_operations
WHEN EXISTS (
  SELECT 1 FROM protocol_operations existing
  WHERE existing.protocol_ref = NEW.protocol_ref
    AND (
      existing.publisher <> NEW.publisher
      OR (
        existing.operation = NEW.operation
        AND existing.implementation_digest <> NEW.implementation_digest
      )
    )
)
BEGIN SELECT RAISE(ABORT, 'protocol_operation_publisher_conflict'); END;
CREATE TABLE IF NOT EXISTS bindings (
  id TEXT PRIMARY KEY,
  resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
  port_name TEXT NOT NULL,
  protocol_ref TEXT NOT NULL,
  access TEXT NOT NULL CHECK (access IN ('read', 'read_write', 'operations')),
  operations_json TEXT NOT NULL DEFAULT '[]',
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'revoked')),
  binding_epoch INTEGER NOT NULL DEFAULT 0 CHECK (binding_epoch >= 0),
  created_at INTEGER NOT NULL,
  instance_id TEXT,
  last_used_at INTEGER
);
"#;

#[derive(Debug, Error)]
pub enum CoordinatorError {
    #[error("resource_not_found")]
    NotFound,
    #[error("{0}")]
    Conflict(&'static str),
    #[error("{0}")]
    Invalid(&'static str),
    #[error("{0}")]
    Forbidden(&'static str),
    #[error("quota_exceeded")]
    QuotaExceeded,
    #[error("handler_failed")]
    HandlerFailed,
    #[error("storage_error: {0}")]
    Storage(#[from] rusqlite::Error),
    #[error("json_error: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceSnapshot {
    pub resource_id: String,
    pub owner_id: String,
    pub protocol: String,
    pub display_name: Option<String>,
    pub revision: i64,
    pub generation: i64,
    pub content_digest: String,
    pub byte_size: i64,
    pub document: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceSummary {
    pub resource_id: String,
    pub protocol: String,
    pub display_name: Option<String>,
    pub revision: i64,
    pub content_digest: String,
}

#[derive(Debug, Clone)]
pub struct CommitInput {
    pub operation_id: String,
    pub expected_revision: i64,
    pub expected_generation: i64,
    pub document: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommitOutcome {
    pub resource_id: String,
    pub revision: i64,
    pub generation: i64,
    pub content_digest: String,
    pub receipt: Value,
    pub replayed: bool,
    /// The Protocol handler's result, when the operation ran a handler.
    pub result: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Binding {
    pub binding_id: String,
    pub resource_id: String,
    pub port_name: String,
    pub protocol: String,
    pub access: String,
    pub operations: Vec<String>,
    pub status: String,
    pub binding_epoch: i64,
    /// The app instance this binding serves, when the bridge created it.
    pub instance_id: Option<String>,
    /// Last time this binding invoked an operation (Unix seconds), refreshed at
    /// most once a minute so a read-heavy app cannot rewrite the row per call.
    pub last_used_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationInfo {
    pub protocol: String,
    pub operation: String,
    pub semantics: String,
    pub access: String,
    pub publisher: String,
    pub implementation_digest: String,
    pub expose_mcp: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpTool {
    pub protocol: String,
    pub operation: String,
    pub semantics: String,
    pub access: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InvokeOutcome {
    pub revision: i64,
    pub generation: i64,
    pub result: Value,
    pub receipt: Option<Value>,
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quota {
    pub used_bytes: i64,
    pub limit_bytes: i64,
    pub resources: i64,
}

pub struct Coordinator {
    conn: Connection,
    quota_limit_bytes: i64,
    retention_seconds: i64,
    max_document_bytes: i64,
    max_bindings: i64,
}

impl Coordinator {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, CoordinatorError> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self, CoordinatorError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, CoordinatorError> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Two Coordinators may legitimately open the same database file (for
        // example one per served app); wait for the other writer instead of
        // failing immediately with SQLITE_BUSY.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        // Databases created before schema 2 lack the retention column; adding
        // it is idempotent and keeps the file usable without a rebuild.
        let _ = conn.execute(
            "ALTER TABLE operation_results ADD COLUMN expires_at INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute("ALTER TABLE bindings ADD COLUMN instance_id TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE protocol_operations ADD COLUMN module_source TEXT",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE protocol_operations ADD COLUMN module_export TEXT",
            [],
        );
        let _ = conn.execute("ALTER TABLE bindings ADD COLUMN last_used_at INTEGER", []);
        let _ = conn.execute(
            "ALTER TABLE protocol_operations ADD COLUMN expose_mcp INTEGER NOT NULL DEFAULT 0",
            [],
        );
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Self {
            conn,
            quota_limit_bytes: DEFAULT_QUOTA_BYTES,
            retention_seconds: DEFAULT_RETENTION_SECONDS,
            max_document_bytes: DEFAULT_MAX_DOCUMENT_BYTES,
            max_bindings: DEFAULT_MAX_BINDINGS,
        })
    }

    /// Override the per-owner quota (bytes) and the operation-result retention.
    pub fn with_limits(mut self, quota_limit_bytes: i64, retention_seconds: i64) -> Self {
        self.quota_limit_bytes = quota_limit_bytes;
        self.retention_seconds = retention_seconds;
        self
    }

    /// Override the per-document byte cap and the binding cap (tests).
    pub fn with_resource_limits(mut self, max_document_bytes: i64, max_bindings: i64) -> Self {
        self.max_document_bytes = max_document_bytes;
        self.max_bindings = max_bindings;
        self
    }

    /// Bytes currently stored for an owner, the configured limit and the
    /// number of active Resources.
    pub fn quota(&self, owner_id: &str) -> Result<Quota, CoordinatorError> {
        let used_bytes: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(d.byte_size), 0)
             FROM resources r
             JOIN document_heads h ON h.resource_id=r.id
             JOIN document_revisions d ON d.resource_id=r.id AND d.revision=h.head_revision
             WHERE r.owner_id=? AND r.status='active'",
            params![owner_id],
            |row| row.get(0),
        )?;
        let resources: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM resources WHERE owner_id=? AND status='active'",
            params![owner_id],
            |row| row.get(0),
        )?;
        Ok(Quota {
            used_bytes,
            limit_bytes: self.quota_limit_bytes,
            resources,
        })
    }

    /// Delete operation results whose retention expired. Returns the count.
    pub fn gc_expired_results(&mut self) -> Result<usize, CoordinatorError> {
        let deleted = self.conn.execute(
            "DELETE FROM operation_results WHERE expires_at > 0 AND expires_at <= ?",
            params![now()],
        )?;
        Ok(deleted)
    }

    pub fn create_resource(
        &mut self,
        owner_id: &str,
        protocol: &str,
        display_name: Option<&str>,
        document: Value,
    ) -> Result<ResourceSnapshot, CoordinatorError> {
        validate_protocol(protocol)?;
        let (document_json, content_digest, byte_size) = encode_document(&document)?;
        if byte_size > self.max_document_bytes {
            return Err(CoordinatorError::QuotaExceeded);
        }
        let quota = self.quota(owner_id)?;
        if quota.used_bytes + byte_size > self.quota_limit_bytes {
            return Err(CoordinatorError::QuotaExceeded);
        }
        let resource_id = new_id("dres");
        let now = now();
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO resources(id,owner_id,protocol_ref,display_name,lifecycle,status,access_epoch,created_at)
             VALUES (?,?,?,?,'independent','active',0,?)",
            params![resource_id, owner_id, protocol, display_name, now],
        )?;
        tx.execute(
            "INSERT INTO document_revisions(resource_id,revision,generation,document_json,content_digest,byte_size,created_at)
             VALUES (?,1,0,?,?,?,?)",
            params![resource_id, document_json, content_digest, byte_size, now],
        )?;
        tx.execute(
            "INSERT INTO document_heads(resource_id,head_revision,generation,updated_at) VALUES (?,1,0,?)",
            params![resource_id, now],
        )?;
        tx.execute(
            "INSERT INTO grants(id,resource_id,subject_kind,subject_id,can_read,can_write,can_connect,can_manage,status,grant_epoch,created_at)
             VALUES (?,?,'user',?,1,1,1,1,'active',0,?)",
            params![new_id("dgrant"), resource_id, owner_id, now],
        )?;
        tx.commit()?;
        self.read_resource(owner_id, &resource_id)
    }

    pub fn read_resource(
        &self,
        owner_id: &str,
        resource_id: &str,
    ) -> Result<ResourceSnapshot, CoordinatorError> {
        self.conn
            .query_row(
                "SELECT r.id,r.owner_id,r.protocol_ref,r.display_name,h.head_revision,h.generation,
                        d.document_json,d.content_digest,d.byte_size
                 FROM resources r
                 JOIN document_heads h ON h.resource_id=r.id
                 JOIN document_revisions d ON d.resource_id=r.id AND d.revision=h.head_revision
                 WHERE r.id=? AND r.owner_id=? AND r.status='active'",
                params![resource_id, owner_id],
                |row| {
                    let document_json: String = row.get(6)?;
                    Ok(ResourceSnapshot {
                        resource_id: row.get(0)?,
                        owner_id: row.get(1)?,
                        protocol: row.get(2)?,
                        display_name: row.get(3)?,
                        revision: row.get(4)?,
                        generation: row.get(5)?,
                        document: serde_json::from_str(&document_json).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                6,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                        content_digest: row.get(7)?,
                        byte_size: row.get(8)?,
                    })
                },
            )
            .optional()?
            .ok_or(CoordinatorError::NotFound)
    }

    pub fn list_resources(&self, owner_id: &str) -> Result<Vec<ResourceSummary>, CoordinatorError> {
        let mut statement = self.conn.prepare(
            "SELECT r.id,r.protocol_ref,r.display_name,h.head_revision,d.content_digest
             FROM resources r
             JOIN document_heads h ON h.resource_id=r.id
             JOIN document_revisions d ON d.resource_id=r.id AND d.revision=h.head_revision
             WHERE r.owner_id=? AND r.status='active'
             ORDER BY r.created_at, r.id",
        )?;
        let rows = statement.query_map(params![owner_id], |row| {
            Ok(ResourceSummary {
                resource_id: row.get(0)?,
                protocol: row.get(1)?,
                display_name: row.get(2)?,
                revision: row.get(3)?,
                content_digest: row.get(4)?,
            })
        })?;
        let mut resources = Vec::new();
        for row in rows {
            resources.push(row?);
        }
        Ok(resources)
    }

    /// Commit a new document under compare-and-set, exactly once per
    /// operation_id.
    pub fn commit(
        &mut self,
        owner_id: &str,
        resource_id: &str,
        input: CommitInput,
    ) -> Result<CommitOutcome, CoordinatorError> {
        self.commit_with_result(owner_id, resource_id, input, None)
    }

    /// Commit, optionally recording a trusted handler's result in the same
    /// transaction.
    fn commit_with_result(
        &mut self,
        owner_id: &str,
        resource_id: &str,
        input: CommitInput,
        handler_result: Option<Value>,
    ) -> Result<CommitOutcome, CoordinatorError> {
        let fingerprint = fingerprint(&input)?;
        self.commit_with_fingerprint(owner_id, resource_id, input, handler_result, fingerprint)
    }

    fn commit_with_fingerprint(
        &mut self,
        owner_id: &str,
        resource_id: &str,
        input: CommitInput,
        handler_result: Option<Value>,
        fingerprint: String,
    ) -> Result<CommitOutcome, CoordinatorError> {
        if input.operation_id.is_empty() || input.operation_id.len() > 128 {
            return Err(CoordinatorError::Invalid("invalid_operation_id"));
        }
        let (document_json, content_digest, byte_size) = encode_document(&input.document)?;
        let quota = self.quota(owner_id)?;
        // Conservative: the replaced head is not subtracted, so a commit can
        // never push the owner past the limit even transiently.
        if quota.used_bytes + byte_size > self.quota_limit_bytes {
            return Err(CoordinatorError::QuotaExceeded);
        }
        let tx = self.conn.transaction()?;
        let head = tx
            .query_row(
                "SELECT h.head_revision,h.generation
                 FROM resources r JOIN document_heads h ON h.resource_id=r.id
                 WHERE r.id=? AND r.owner_id=? AND r.status='active'",
                params![resource_id, owner_id],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let (head_revision, generation) = head.ok_or(CoordinatorError::NotFound)?;

        let existing = tx
            .query_row(
                "SELECT fingerprint,receipt_json,revision,result_json FROM operation_results
                 WHERE resource_id=? AND operation_id=?",
                params![resource_id, input.operation_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        if let Some((stored_fingerprint, receipt_json, revision, result_json)) = existing {
            if stored_fingerprint != fingerprint {
                return Err(CoordinatorError::Conflict("operation_id_conflict"));
            }
            let receipt: Value = serde_json::from_str(&receipt_json)?;
            let content_digest = receipt
                .get("content_digest")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            tx.commit()?;
            return Ok(CommitOutcome {
                resource_id: resource_id.to_owned(),
                revision,
                generation,
                content_digest,
                receipt,
                replayed: true,
                result: serde_json::from_str(&result_json).ok(),
            });
        }

        if input.expected_revision != head_revision || input.expected_generation != generation {
            return Err(CoordinatorError::Conflict("revision_conflict"));
        }

        let next_revision = head_revision + 1;
        let now = now();
        tx.execute(
            "INSERT INTO document_revisions(resource_id,revision,generation,document_json,content_digest,byte_size,created_at)
             VALUES (?,?,?,?,?,?,?)",
            params![
                resource_id,
                next_revision,
                generation,
                document_json,
                content_digest,
                byte_size,
                now
            ],
        )?;
        tx.execute(
            "UPDATE document_heads SET head_revision=?, generation=?, updated_at=? WHERE resource_id=?",
            params![next_revision, generation, now, resource_id],
        )?;
        let receipt = serde_json::json!({
            "schema": "ato.data-resource-receipt/1",
            "resource_id": resource_id,
            "revision": next_revision,
            "generation": generation,
            "content_digest": content_digest,
            "operation_id": input.operation_id,
        });
        let result = match handler_result.clone() {
            Some(value) => value,
            None => serde_json::json!({
                "revision": next_revision,
                "content_digest": content_digest,
            }),
        };
        tx.execute(
            "INSERT INTO operation_results(resource_id,operation_id,fingerprint,result_json,receipt_json,revision,created_at,expires_at)
             VALUES (?,?,?,?,?,?,?,?)",
            params![
                resource_id,
                input.operation_id,
                fingerprint,
                serde_json::to_string(&result)?,
                serde_json::to_string(&receipt)?,
                next_revision,
                now,
                now + self.retention_seconds
            ],
        )?;
        tx.execute(
            "INSERT INTO outbox(resource_id,revision,created_at) VALUES (?,?,?)",
            params![resource_id, next_revision, now],
        )?;
        tx.commit()?;
        Ok(CommitOutcome {
            resource_id: resource_id.to_owned(),
            revision: next_revision,
            generation,
            content_digest,
            receipt,
            replayed: false,
            result: handler_result,
        })
    }

    /// Register one operation of a Protocol. The first publisher of a
    /// (protocol, operation) owns it; a different publisher, or a different
    /// implementation digest for the same operation, is refused by the DB
    /// trigger.
    pub fn register_operation(
        &mut self,
        protocol: &str,
        operation: &str,
        semantics: &str,
        access: &str,
        implementation_digest: &str,
        publisher: &str,
    ) -> Result<(), CoordinatorError> {
        validate_protocol(protocol)?;
        if operation.is_empty() || operation.len() > 128 {
            return Err(CoordinatorError::Invalid("invalid_operation"));
        }
        if !matches!(semantics, "evolution" | "observation") {
            return Err(CoordinatorError::Invalid("invalid_semantics"));
        }
        if !matches!(access, "read" | "read_write") {
            return Err(CoordinatorError::Invalid("invalid_access"));
        }
        let result = self.conn.execute(
            "INSERT INTO protocol_operations(protocol_ref,operation,semantics,access,implementation_digest,publisher,created_at)
             VALUES (?,?,?,?,?,?,?)",
            params![
                protocol,
                operation,
                semantics,
                access,
                implementation_digest,
                publisher,
                now()
            ],
        );
        match result {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(_, Some(message)))
                if message.contains("protocol_operation_publisher_conflict") =>
            {
                Err(CoordinatorError::Conflict(
                    "protocol_operation_publisher_conflict",
                ))
            }
            Err(error) => Err(CoordinatorError::Storage(error)),
        }
    }

    /// Bind a Resource to a Port. An `operations` binding is operation-only:
    /// it must list at least one registered operation and never grants document
    /// read or replace.
    pub fn bind_resource(
        &mut self,
        resource_id: &str,
        port_name: &str,
        protocol: &str,
        access: &str,
        operations: Vec<String>,
    ) -> Result<Binding, CoordinatorError> {
        self.bind_resource_inner(resource_id, port_name, protocol, access, operations, None)
    }

    /// Bind for an app instance so the served app's bridge can find it.
    pub fn bind_resource_for_instance(
        &mut self,
        resource_id: &str,
        port_name: &str,
        protocol: &str,
        access: &str,
        operations: Vec<String>,
        instance_id: &str,
    ) -> Result<Binding, CoordinatorError> {
        self.bind_resource_inner(
            resource_id,
            port_name,
            protocol,
            access,
            operations,
            Some(instance_id),
        )
    }

    fn bind_resource_inner(
        &mut self,
        resource_id: &str,
        port_name: &str,
        protocol: &str,
        access: &str,
        operations: Vec<String>,
        instance_id: Option<&str>,
    ) -> Result<Binding, CoordinatorError> {
        if port_name.is_empty() || port_name.len() > 64 {
            return Err(CoordinatorError::Invalid("invalid_port"));
        }
        if !matches!(access, "read" | "read_write" | "operations") {
            return Err(CoordinatorError::Invalid("invalid_access"));
        }
        let active: i64 = self.conn.query_row(
            "SELECT count(*) FROM bindings WHERE resource_id=? AND status='active'",
            params![resource_id],
            |row| row.get(0),
        )?;
        if active >= self.max_bindings {
            return Err(CoordinatorError::Invalid("binding_limit_reached"));
        }
        let resource = self.read_resource("local", resource_id)?;
        if resource.protocol != protocol {
            return Err(CoordinatorError::Invalid("protocol_mismatch"));
        }
        if access == "operations" {
            if operations.is_empty() {
                return Err(CoordinatorError::Invalid("operation_list_required"));
            }
            for operation in &operations {
                let registered: Option<i64> = self
                    .conn
                    .query_row(
                        "SELECT 1 FROM protocol_operations WHERE protocol_ref=? AND operation=?",
                        params![protocol, operation],
                        |row| row.get(0),
                    )
                    .optional()?;
                if registered.is_none() {
                    return Err(CoordinatorError::Invalid("operation_not_registered"));
                }
            }
        } else if !operations.is_empty() {
            return Err(CoordinatorError::Invalid(
                "operations_require_operations_access",
            ));
        }
        let binding_id = new_id("dbind");
        self.conn.execute(
            "INSERT INTO bindings(id,resource_id,port_name,protocol_ref,access,operations_json,status,binding_epoch,created_at,instance_id)
             VALUES (?,?,?,?,?,?,'active',0,?,?)",
            params![
                binding_id,
                resource_id,
                port_name,
                protocol,
                access,
                serde_json::to_string(&operations)?,
                now(),
                instance_id
            ],
        )?;
        self.read_binding(&binding_id)
    }

    pub fn read_binding(&self, binding_id: &str) -> Result<Binding, CoordinatorError> {
        self.conn
            .query_row(
                "SELECT id,resource_id,port_name,protocol_ref,access,operations_json,status,binding_epoch,instance_id,last_used_at
                 FROM bindings WHERE id=?",
                params![binding_id],
                |row| {
                    let operations_json: String = row.get(5)?;
                    Ok(Binding {
                        binding_id: row.get(0)?,
                        resource_id: row.get(1)?,
                        port_name: row.get(2)?,
                        protocol: row.get(3)?,
                        access: row.get(4)?,
                        operations: serde_json::from_str(&operations_json).map_err(|error| {
                            rusqlite::Error::FromSqlConversionFailure(
                                5,
                                rusqlite::types::Type::Text,
                                Box::new(error),
                            )
                        })?,
                        status: row.get(6)?,
                        binding_epoch: row.get(7)?,
                        instance_id: row.get(8)?,
                        last_used_at: row.get(9)?,
                    })
                },
            )
            .optional()?
            .ok_or(CoordinatorError::NotFound)
    }

    /// Read the document through a binding. An operation-only binding is
    /// refused: it may only invoke its listed operations.
    pub fn read_binding_document(
        &self,
        binding_id: &str,
    ) -> Result<ResourceSnapshot, CoordinatorError> {
        let binding = self.read_binding(binding_id)?;
        if binding.status != "active" {
            return Err(CoordinatorError::Forbidden("binding_revoked"));
        }
        if binding.access == "operations" {
            return Err(CoordinatorError::Forbidden("operation_only_binding"));
        }
        self.read_resource("local", &binding.resource_id)
    }

    /// Invoke an operation through a binding. The operation must be registered
    /// for the Protocol and, for an operation-only binding, explicitly listed.
    pub fn invoke_binding_operation(
        &mut self,
        binding_id: &str,
        operation: &str,
        input: CommitInput,
    ) -> Result<CommitOutcome, CoordinatorError> {
        let binding = self.read_binding(binding_id)?;
        if binding.status != "active" {
            return Err(CoordinatorError::Forbidden("binding_revoked"));
        }
        let registered: Option<String> = self
            .conn
            .query_row(
                "SELECT access FROM protocol_operations WHERE protocol_ref=? AND operation=?",
                params![binding.protocol, operation],
                |row| row.get(0),
            )
            .optional()?;
        let Some(registered_access) = registered else {
            return Err(CoordinatorError::Forbidden("operation_not_registered"));
        };
        if binding.access == "operations" && !binding.operations.iter().any(|op| op == operation) {
            return Err(CoordinatorError::Forbidden("operation_not_granted"));
        }
        if binding.access == "read" && registered_access == "read_write" {
            return Err(CoordinatorError::Forbidden("binding_read_only"));
        }
        self.commit("local", &binding.resource_id, input)
    }

    /// Revoke a binding. Every later read or invoke through it is refused.
    pub fn revoke_binding(&mut self, binding_id: &str) -> Result<Binding, CoordinatorError> {
        let changed = self.conn.execute(
            "UPDATE bindings SET status='revoked', binding_epoch=binding_epoch+1 WHERE id=? AND status='active'",
            params![binding_id],
        )?;
        if changed == 0 {
            return Err(CoordinatorError::Conflict("binding_conflict"));
        }
        self.read_binding(binding_id)
    }

    /// Register an operation whose implementation is a handler module. The
    /// digest is the SHA-256 of the module source, so a changed handler is a
    /// different implementation.
    #[allow(clippy::too_many_arguments)] // mirrors the registration columns
    pub fn register_handler_operation(
        &mut self,
        protocol: &str,
        operation: &str,
        semantics: &str,
        access: &str,
        module_source: &str,
        module_export: &str,
        publisher: &str,
    ) -> Result<(), CoordinatorError> {
        if module_source.is_empty() {
            return Err(CoordinatorError::Invalid("invalid_module"));
        }
        if module_export.is_empty()
            || !module_export
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '$'))
        {
            return Err(CoordinatorError::Invalid("invalid_export"));
        }
        validate_protocol(protocol)?;
        if operation.is_empty() || operation.len() > 128 {
            return Err(CoordinatorError::Invalid("invalid_operation"));
        }
        if !matches!(semantics, "evolution" | "observation") {
            return Err(CoordinatorError::Invalid("invalid_semantics"));
        }
        if !matches!(access, "read" | "read_write") {
            return Err(CoordinatorError::Invalid("invalid_access"));
        }
        let digest = format!("sha256:{:x}", Sha256::digest(module_source.as_bytes()));
        let result = self.conn.execute(
            "INSERT INTO protocol_operations(protocol_ref,operation,semantics,access,implementation_digest,publisher,created_at,module_source,module_export)
             VALUES (?,?,?,?,?,?,?,?,?)",
            params![
                protocol,
                operation,
                semantics,
                access,
                digest,
                publisher,
                now(),
                module_source,
                module_export
            ],
        );
        match result {
            Ok(_) => Ok(()),
            Err(rusqlite::Error::SqliteFailure(_, Some(message)))
                if message.contains("protocol_operation_publisher_conflict") =>
            {
                Err(CoordinatorError::Conflict(
                    "protocol_operation_publisher_conflict",
                ))
            }
            Err(error) => Err(CoordinatorError::Storage(error)),
        }
    }

    /// Run a registered handler for `operation`, apply its operations and
    /// commit (evolution) or return its result (observation).
    #[allow(clippy::too_many_arguments)] // mirrors the invocation envelope
    pub fn invoke_operation(
        &mut self,
        owner_id: &str,
        resource_id: &str,
        operation: &str,
        arguments: &Value,
        operation_id: &str,
        expected_revision: i64,
        expected_generation: i64,
    ) -> Result<InvokeOutcome, CoordinatorError> {
        let resource = self.read_resource(owner_id, resource_id)?;
        let registration = self
            .conn
            .query_row(
                "SELECT semantics,module_source,module_export FROM protocol_operations
                 WHERE protocol_ref=? AND operation=?",
                params![resource.protocol, operation],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((semantics, Some(module_source), Some(module_export))) = registration else {
            return Err(CoordinatorError::Forbidden("operation_not_registered"));
        };
        let handler_fingerprint = handler_fingerprint(
            operation_id,
            expected_revision,
            expected_generation,
            operation,
            arguments,
        )?;
        // A retried handler invocation returns the stored outcome instead of
        // running the trusted handler a second time.
        let stored = self
            .conn
            .query_row(
                "SELECT fingerprint,receipt_json,revision FROM operation_results
                 WHERE resource_id=? AND operation_id=?",
                params![resource_id, operation_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((stored_fingerprint, receipt_json, revision)) = stored {
            if stored_fingerprint != handler_fingerprint {
                return Err(CoordinatorError::Conflict("operation_id_conflict"));
            }
            let receipt: Value = serde_json::from_str(&receipt_json)?;
            let stored_result = self
                .conn
                .query_row(
                    "SELECT result_json FROM operation_results WHERE resource_id=? AND operation_id=?",
                    params![resource_id, operation_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .and_then(|value| serde_json::from_str::<Value>(&value).ok());
            return Ok(InvokeOutcome {
                revision,
                generation: resource.generation,
                result: stored_result.unwrap_or(Value::Null),
                receipt: Some(receipt),
                replayed: true,
            });
        }
        let state = serde_json::json!({ "document": resource.document });
        let value = crate::handlers::call_export(&module_source, &module_export, &state, arguments)
            .map_err(|error| match error {
                crate::handlers::HandlerError::InvalidModule(code) => {
                    CoordinatorError::Invalid(code)
                }
                crate::handlers::HandlerError::Failed(_) => CoordinatorError::HandlerFailed,
            })?;
        let operations = value
            .get("operations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let result = value.get("result").cloned().unwrap_or(Value::Null);
        if semantics == "observation" {
            if !operations.is_empty() {
                return Err(CoordinatorError::HandlerFailed);
            }
            return Ok(InvokeOutcome {
                revision: resource.revision,
                generation: resource.generation,
                result,
                receipt: None,
                replayed: false,
            });
        }
        if operations.len() != 1
            || operations[0].get("kind").and_then(Value::as_str) != Some("replace")
        {
            return Err(CoordinatorError::HandlerFailed);
        }
        let next_document = operations[0]
            .get("value")
            .cloned()
            .ok_or(CoordinatorError::HandlerFailed)?;
        let outcome = self.commit_with_fingerprint(
            owner_id,
            resource_id,
            CommitInput {
                operation_id: operation_id.to_owned(),
                expected_revision,
                expected_generation,
                document: next_document,
            },
            Some(result),
            handler_fingerprint,
        )?;
        Ok(InvokeOutcome {
            revision: outcome.revision,
            generation: outcome.generation,
            result: outcome.result.unwrap_or(Value::Null),
            receipt: Some(outcome.receipt),
            replayed: outcome.replayed,
        })
    }

    /// Publish or withdraw an operation to MCP clients.
    pub fn set_operation_exposure(
        &mut self,
        protocol: &str,
        operation: &str,
        expose_mcp: bool,
    ) -> Result<(), CoordinatorError> {
        let changed = self.conn.execute(
            "UPDATE protocol_operations SET expose_mcp=? WHERE protocol_ref=? AND operation=?",
            params![expose_mcp as i64, protocol, operation],
        )?;
        if changed == 0 {
            return Err(CoordinatorError::NotFound);
        }
        Ok(())
    }

    /// Operations published to MCP clients, with their declared semantics.
    pub fn mcp_tools(&self) -> Result<Vec<McpTool>, CoordinatorError> {
        // A tool is only useful when the mcp app instance holds a binding that
        // grants it; advertising an ungranted tool invites calls that must be
        // refused.
        let mut statement = self.conn.prepare(
            "SELECT po.protocol_ref,po.operation,po.semantics,po.access
             FROM protocol_operations po
             WHERE po.expose_mcp=1 AND EXISTS (
               SELECT 1 FROM bindings b
               WHERE b.instance_id='mcp' AND b.status='active'
                 AND b.access='operations'
                 AND EXISTS (
                   SELECT 1 FROM json_each(b.operations_json) granted
                   WHERE granted.value=po.operation
                 )
             )
             ORDER BY po.protocol_ref, po.operation",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(McpTool {
                protocol: row.get(0)?,
                operation: row.get(1)?,
                semantics: row.get(2)?,
                access: row.get(3)?,
            })
        })?;
        let mut tools = Vec::new();
        for row in rows {
            tools.push(row?);
        }
        Ok(tools)
    }

    /// Registered Protocol operations, optionally only one protocol's.
    pub fn list_operations(
        &self,
        protocol: Option<&str>,
    ) -> Result<Vec<OperationInfo>, CoordinatorError> {
        let mut statement = self.conn.prepare(
            "SELECT protocol_ref,operation,semantics,access,publisher,implementation_digest,expose_mcp
             FROM protocol_operations
             WHERE (?1 IS NULL OR protocol_ref=?1)
             ORDER BY protocol_ref, operation",
        )?;
        let rows = statement.query_map(params![protocol], |row| {
            Ok(OperationInfo {
                protocol: row.get(0)?,
                operation: row.get(1)?,
                semantics: row.get(2)?,
                access: row.get(3)?,
                publisher: row.get(4)?,
                implementation_digest: row.get(5)?,
                expose_mcp: row.get::<_, i64>(6)? != 0,
            })
        })?;
        let mut operations = Vec::new();
        for row in rows {
            operations.push(row?);
        }
        Ok(operations)
    }

    /// Every binding (any status) for the admin/CLI view, newest first.
    pub fn list_bindings(&self) -> Result<Vec<Binding>, CoordinatorError> {
        let mut statement = self
            .conn
            .prepare("SELECT id FROM bindings ORDER BY created_at DESC, id")?;
        let ids = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut bindings = Vec::new();
        for id in ids {
            bindings.push(self.read_binding(&id)?);
        }
        Ok(bindings)
    }

    /// Active bindings serving one app instance, for the bridge's requirements.
    pub fn bindings_for_instance(
        &self,
        instance_id: &str,
    ) -> Result<Vec<Binding>, CoordinatorError> {
        let mut statement = self.conn.prepare(
            "SELECT id FROM bindings WHERE instance_id=? AND status='active' ORDER BY port_name",
        )?;
        let ids = statement
            .query_map(params![instance_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut bindings = Vec::new();
        for id in ids {
            bindings.push(self.read_binding(&id)?);
        }
        Ok(bindings)
    }

    /// Invoke a registered handler through a binding after the binding's own
    /// checks (active, operation in the grant, read binding cannot write).
    pub fn invoke_binding_handler(
        &mut self,
        binding_id: &str,
        operation: &str,
        arguments: &Value,
        operation_id: &str,
        expected_revision: i64,
        expected_generation: i64,
    ) -> Result<InvokeOutcome, CoordinatorError> {
        let binding = self.read_binding(binding_id)?;
        if binding.status != "active" {
            return Err(CoordinatorError::Forbidden("binding_revoked"));
        }
        let registered: Option<String> = self
            .conn
            .query_row(
                "SELECT access FROM protocol_operations WHERE protocol_ref=? AND operation=?",
                params![binding.protocol, operation],
                |row| row.get(0),
            )
            .optional()?;
        if registered.is_none() {
            return Err(CoordinatorError::Forbidden("operation_not_registered"));
        }
        if binding.access == "operations" && !binding.operations.iter().any(|op| op == operation) {
            return Err(CoordinatorError::Forbidden("operation_not_granted"));
        }
        let outcome = self.invoke_operation(
            "local",
            &binding.resource_id,
            operation,
            arguments,
            operation_id,
            expected_revision,
            expected_generation,
        )?;
        // Debounced: at most one row update a minute per binding, and a failed
        // update must not fail the operation.
        let now = now();
        let _ = self.conn.execute(
            "UPDATE bindings SET last_used_at=? WHERE id=? AND (last_used_at IS NULL OR last_used_at < ?)",
            params![now, binding.binding_id, now - 60],
        );
        Ok(outcome)
    }
}

fn validate_protocol(protocol: &str) -> Result<(), CoordinatorError> {
    let valid = protocol
        .split_once('@')
        .map(|(name, version)| {
            !name.is_empty()
                && !version.is_empty()
                && version.chars().all(|c| c.is_ascii_digit())
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        })
        .unwrap_or(false);
    if valid {
        Ok(())
    } else {
        Err(CoordinatorError::Invalid("unsupported_data_protocol"))
    }
}

/// Canonical JSON: serde_json objects are BTreeMaps, so keys are sorted.
fn encode_document(document: &Value) -> Result<(String, String, i64), CoordinatorError> {
    let json = serde_json::to_string(document)?;
    let digest = format!("sha256:{:x}", Sha256::digest(json.as_bytes()));
    let byte_size = json.len() as i64;
    Ok((json, digest, byte_size))
}

fn handler_fingerprint(
    operation_id: &str,
    expected_revision: i64,
    expected_generation: i64,
    operation: &str,
    arguments: &Value,
) -> Result<String, CoordinatorError> {
    let payload = serde_json::json!({
        "operation_id": operation_id,
        "expected_revision": expected_revision,
        "expected_generation": expected_generation,
        "operation": operation,
        "arguments": arguments,
    });
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_string(&payload)?.as_bytes())
    ))
}

fn fingerprint(input: &CommitInput) -> Result<String, CoordinatorError> {
    let payload = serde_json::json!({
        "operation_id": input.operation_id,
        "expected_revision": input.expected_revision,
        "expected_generation": input.expected_generation,
        "document": input.document,
    });
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_string(&payload)?.as_bytes())
    ))
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn new_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(
        "{prefix}_{:x}{:04x}{:04x}",
        now(),
        std::process::id() as u16,
        counter as u16
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board(title: &str) -> Value {
        serde_json::json!({
            "schemaVersion": 3,
            "columns": [{"id": "todo", "title": "Todo", "order": 1000}],
            "tasks": {"t1": {"id": "t1", "title": title, "dueAt": null}}
        })
    }

    #[test]
    fn creates_and_reads_a_resource() {
        let mut coordinator = Coordinator::open_in_memory().unwrap();
        let created = coordinator
            .create_resource("alice", "todo.board@1", Some("TODO"), board("first"))
            .unwrap();
        assert_eq!(created.revision, 1);
        assert_eq!(created.generation, 0);
        assert!(created.content_digest.starts_with("sha256:"));
        let read = coordinator
            .read_resource("alice", &created.resource_id)
            .unwrap();
        assert_eq!(read, created);
        assert_eq!(coordinator.list_resources("alice").unwrap().len(), 1);
        assert!(matches!(
            coordinator.read_resource("bob", &created.resource_id),
            Err(CoordinatorError::NotFound)
        ));
    }

    #[test]
    fn commits_with_cas_and_writes_a_receipt() {
        let mut coordinator = Coordinator::open_in_memory().unwrap();
        let created = coordinator
            .create_resource("alice", "todo.board@1", None, board("first"))
            .unwrap();
        let outcome = coordinator
            .commit(
                "alice",
                &created.resource_id,
                CommitInput {
                    operation_id: "op-1".into(),
                    expected_revision: 1,
                    expected_generation: 0,
                    document: board("second"),
                },
            )
            .unwrap();
        assert_eq!(outcome.revision, 2);
        assert!(!outcome.replayed);
        assert_eq!(outcome.receipt["revision"], 2);
        assert_eq!(outcome.receipt["resource_id"], created.resource_id);
        let read = coordinator
            .read_resource("alice", &created.resource_id)
            .unwrap();
        assert_eq!(read.revision, 2);
        assert_eq!(read.document, board("second"));
    }

    #[test]
    fn replays_the_same_operation_once() {
        let mut coordinator = Coordinator::open_in_memory().unwrap();
        let created = coordinator
            .create_resource("alice", "todo.board@1", None, board("first"))
            .unwrap();
        let input = CommitInput {
            operation_id: "op-1".into(),
            expected_revision: 1,
            expected_generation: 0,
            document: board("second"),
        };
        let first = coordinator
            .commit("alice", &created.resource_id, input.clone())
            .unwrap();
        let replay = coordinator
            .commit("alice", &created.resource_id, input)
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.revision, first.revision);
        assert_eq!(replay.content_digest, first.content_digest);
        assert_eq!(
            coordinator
                .read_resource("alice", &created.resource_id)
                .unwrap()
                .revision,
            2
        );
    }

    #[test]
    fn refuses_a_reused_operation_id_with_a_different_payload() {
        let mut coordinator = Coordinator::open_in_memory().unwrap();
        let created = coordinator
            .create_resource("alice", "todo.board@1", None, board("first"))
            .unwrap();
        coordinator
            .commit(
                "alice",
                &created.resource_id,
                CommitInput {
                    operation_id: "op-1".into(),
                    expected_revision: 1,
                    expected_generation: 0,
                    document: board("second"),
                },
            )
            .unwrap();
        let conflict = coordinator.commit(
            "alice",
            &created.resource_id,
            CommitInput {
                operation_id: "op-1".into(),
                expected_revision: 2,
                expected_generation: 0,
                document: board("third"),
            },
        );
        assert!(matches!(
            conflict,
            Err(CoordinatorError::Conflict("operation_id_conflict"))
        ));
    }

    #[test]
    fn refuses_a_stale_expected_revision() {
        let mut coordinator = Coordinator::open_in_memory().unwrap();
        let created = coordinator
            .create_resource("alice", "todo.board@1", None, board("first"))
            .unwrap();
        coordinator
            .commit(
                "alice",
                &created.resource_id,
                CommitInput {
                    operation_id: "op-1".into(),
                    expected_revision: 1,
                    expected_generation: 0,
                    document: board("second"),
                },
            )
            .unwrap();
        let stale = coordinator.commit(
            "alice",
            &created.resource_id,
            CommitInput {
                operation_id: "op-2".into(),
                expected_revision: 1,
                expected_generation: 0,
                document: board("third"),
            },
        );
        assert!(matches!(
            stale,
            Err(CoordinatorError::Conflict("revision_conflict"))
        ));
    }

    #[test]
    fn survives_a_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("coordinator.sqlite");
        let resource_id = {
            let mut coordinator = Coordinator::open(&path).unwrap();
            let created = coordinator
                .create_resource("alice", "todo.board@1", None, board("first"))
                .unwrap();
            coordinator
                .commit(
                    "alice",
                    &created.resource_id,
                    CommitInput {
                        operation_id: "op-1".into(),
                        expected_revision: 1,
                        expected_generation: 0,
                        document: board("second"),
                    },
                )
                .unwrap();
            created.resource_id
        };
        let coordinator = Coordinator::open(&path).unwrap();
        let read = coordinator.read_resource("alice", &resource_id).unwrap();
        assert_eq!(read.revision, 2);
        assert_eq!(read.document, board("second"));
    }

    #[test]
    fn refuses_a_create_over_the_quota() {
        let mut coordinator = Coordinator::open_in_memory()
            .unwrap()
            .with_limits(8, DEFAULT_RETENTION_SECONDS);
        let error = coordinator
            .create_resource("alice", "todo.board@1", None, board("first"))
            .expect_err("quota must refuse the create");
        assert!(matches!(error, CoordinatorError::QuotaExceeded));
        assert_eq!(coordinator.quota("alice").unwrap().resources, 0);
    }

    #[test]
    fn reports_quota_usage() {
        let mut coordinator = Coordinator::open_in_memory().unwrap();
        let created = coordinator
            .create_resource("alice", "todo.board@1", None, board("first"))
            .unwrap();
        let quota = coordinator.quota("alice").unwrap();
        assert_eq!(quota.resources, 1);
        assert_eq!(quota.used_bytes, created.byte_size);
        assert_eq!(quota.limit_bytes, DEFAULT_QUOTA_BYTES);
    }

    #[test]
    fn gc_deletes_expired_operation_results() {
        let mut coordinator = Coordinator::open_in_memory()
            .unwrap()
            .with_limits(DEFAULT_QUOTA_BYTES, 0);
        let created = coordinator
            .create_resource("alice", "todo.board@1", None, board("first"))
            .unwrap();
        coordinator
            .commit(
                "alice",
                &created.resource_id,
                CommitInput {
                    operation_id: "op-1".into(),
                    expected_revision: 1,
                    expected_generation: 0,
                    document: board("second"),
                },
            )
            .unwrap();
        assert_eq!(coordinator.gc_expired_results().unwrap(), 1);
        assert_eq!(coordinator.gc_expired_results().unwrap(), 0);
        // The revision itself is untouched; only the replay record expired.
        assert_eq!(
            coordinator
                .read_resource("alice", &created.resource_id)
                .unwrap()
                .revision,
            2
        );
    }

    const HANDLER_MODULE: &str = r#"
function boardOf(state) {
  return state && state.document && typeof state.document === "object" ? state.document : {};
}
export function listDueDates({ state }) {
  const tasks = Object.values(boardOf(state).tasks || {})
    .filter((task) => task && task.dueAt && !task.archived)
    .map((task) => ({
      task_id: String(task.id),
      title: String(task.title || ""),
      due_at: String(task.dueAt),
      completed: task.completed === true,
    }));
  return { operations: [], result: { tasks } };
}
export function createTask({ arguments: args, state }) {
  const board = boardOf(state);
  const tasks = Object.assign({}, board.tasks || {});
  const id = "task-" + (Object.keys(tasks).length + 1);
  tasks[id] = { id, title: String(args.title), dueAt: args.due_at ?? null, completed: false };
  return { operations: [{ kind: "replace", value: { tasks } }], result: { task_id: id } };
}
"#;

    #[test]
    fn invokes_a_registered_handler_end_to_end() {
        let mut coordinator = Coordinator::open_in_memory().unwrap();
        coordinator
            .register_handler_operation(
                "todo.board@1",
                "todo_list_due_dates",
                "observation",
                "read",
                HANDLER_MODULE,
                "listDueDates",
                "alice/app",
            )
            .unwrap();
        coordinator
            .register_handler_operation(
                "todo.board@1",
                "todo_create",
                "evolution",
                "read_write",
                HANDLER_MODULE,
                "createTask",
                "alice/app",
            )
            .unwrap();
        let created = coordinator
            .create_resource(
                "local",
                "todo.board@1",
                None,
                serde_json::json!({"tasks": {"t1": {"id": "t1", "title": "Due", "dueAt": "2026-11-20"}}}),
            )
            .unwrap();
        let binding = coordinator
            .bind_resource(
                &created.resource_id,
                "tasks",
                "todo.board@1",
                "operations",
                vec!["todo_list_due_dates".into(), "todo_create".into()],
            )
            .unwrap();

        let observed = coordinator
            .invoke_binding_handler(
                &binding.binding_id,
                "todo_list_due_dates",
                &serde_json::json!({}),
                "op-due",
                1,
                0,
            )
            .unwrap();
        assert_eq!(observed.revision, 1);
        assert_eq!(observed.result["tasks"][0]["task_id"], "t1");
        assert!(observed.receipt.is_none());

        let created_task = coordinator
            .invoke_binding_handler(
                &binding.binding_id,
                "todo_create",
                &serde_json::json!({"title": "New", "due_at": "2026-12-01"}),
                "op-create",
                1,
                0,
            )
            .unwrap();
        assert_eq!(created_task.revision, 2);
        assert_eq!(created_task.result["task_id"], "task-2");
        let read = coordinator
            .read_resource("local", &created.resource_id)
            .unwrap();
        assert_eq!(read.document["tasks"]["task-2"]["title"], "New");

        // Same operation id + arguments replays the stored outcome.
        let replay = coordinator
            .invoke_binding_handler(
                &binding.binding_id,
                "todo_create",
                &serde_json::json!({"title": "New", "due_at": "2026-12-01"}),
                "op-create",
                1,
                0,
            )
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.revision, 2);
    }

    #[test]
    fn refuses_a_handler_operation_outside_the_grant() {
        let mut coordinator = Coordinator::open_in_memory().unwrap();
        for (operation, semantics, access, export) in [
            ("todo_list_due_dates", "observation", "read", "listDueDates"),
            ("todo_create", "evolution", "read_write", "createTask"),
        ] {
            coordinator
                .register_handler_operation(
                    "todo.board@1",
                    operation,
                    semantics,
                    access,
                    HANDLER_MODULE,
                    export,
                    "alice/app",
                )
                .unwrap();
        }
        let created = coordinator
            .create_resource(
                "local",
                "todo.board@1",
                None,
                serde_json::json!({"tasks": {}}),
            )
            .unwrap();
        let binding = coordinator
            .bind_resource(
                &created.resource_id,
                "tasks",
                "todo.board@1",
                "operations",
                vec!["todo_list_due_dates".into()],
            )
            .unwrap();
        let refused = coordinator.invoke_binding_handler(
            &binding.binding_id,
            "todo_create",
            &serde_json::json!({"title": "Nope"}),
            "op-nope",
            1,
            0,
        );
        assert!(matches!(
            refused,
            Err(CoordinatorError::Forbidden("operation_not_granted"))
        ));
    }

    #[test]
    fn publishes_and_withdraws_operations_for_mcp() {
        let mut coordinator = Coordinator::open_in_memory().unwrap();
        coordinator
            .register_handler_operation(
                "todo.board@1",
                "todo_create",
                "evolution",
                "read_write",
                HANDLER_MODULE,
                "createTask",
                "alice/app",
            )
            .unwrap();
        assert!(coordinator.mcp_tools().unwrap().is_empty());
        let created = coordinator
            .create_resource(
                "local",
                "todo.board@1",
                None,
                serde_json::json!({"tasks": {}}),
            )
            .unwrap();
        coordinator
            .bind_resource_for_instance(
                &created.resource_id,
                "tasks",
                "todo.board@1",
                "operations",
                vec!["todo_create".into()],
                "mcp",
            )
            .unwrap();
        // Exposed but not granted to the MCP instance: not advertised.
        coordinator
            .set_operation_exposure("todo.board@1", "todo_create", true)
            .unwrap();
        let tools = coordinator.mcp_tools().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].operation, "todo_create");
        assert_eq!(tools[0].semantics, "evolution");
        coordinator
            .set_operation_exposure("todo.board@1", "todo_create", false)
            .unwrap();
        assert!(coordinator.mcp_tools().unwrap().is_empty());
        assert!(matches!(
            coordinator.set_operation_exposure("todo.board@1", "missing", true),
            Err(CoordinatorError::NotFound)
        ));
    }

    #[test]
    fn a_registered_operation_cannot_be_swapped() {
        let mut coordinator = Coordinator::open_in_memory().unwrap();
        coordinator
            .register_handler_operation(
                "todo.board@1",
                "todo_create",
                "evolution",
                "read_write",
                HANDLER_MODULE,
                "createTask",
                "alice/app",
            )
            .unwrap();
        let swap_digest = coordinator.conn.execute(
            "UPDATE protocol_operations SET implementation_digest='sha256:other' WHERE protocol_ref='todo.board@1' AND operation='todo_create'",
            [],
        );
        assert!(swap_digest.is_err(), "digest swap must be refused");
        let swap_module = coordinator.conn.execute(
            "UPDATE protocol_operations SET module_source='export function x(){}' WHERE protocol_ref='todo.board@1' AND operation='todo_create'",
            [],
        );
        assert!(swap_module.is_err(), "handler swap must be refused");
        // Exposure is the one mutable column. Without an mcp binding the tool
        // is published but not advertised.
        coordinator
            .set_operation_exposure("todo.board@1", "todo_create", true)
            .unwrap();
        assert!(coordinator.mcp_tools().unwrap().is_empty());
    }

    #[test]
    fn records_when_a_binding_was_last_used() {
        let mut coordinator = Coordinator::open_in_memory().unwrap();
        coordinator
            .register_handler_operation(
                "todo.board@1",
                "todo_create",
                "evolution",
                "read_write",
                HANDLER_MODULE,
                "createTask",
                "alice/app",
            )
            .unwrap();
        let created = coordinator
            .create_resource(
                "local",
                "todo.board@1",
                None,
                serde_json::json!({"tasks": {}}),
            )
            .unwrap();
        let binding = coordinator
            .bind_resource(
                &created.resource_id,
                "tasks",
                "todo.board@1",
                "operations",
                vec!["todo_create".into()],
            )
            .unwrap();
        assert_eq!(binding.last_used_at, None);
        coordinator
            .invoke_binding_handler(
                &binding.binding_id,
                "todo_create",
                &serde_json::json!({"title": "Tracked"}),
                "op-tracked",
                1,
                0,
            )
            .unwrap();
        let used = coordinator.read_binding(&binding.binding_id).unwrap();
        assert!(used.last_used_at.is_some());
    }

    #[test]
    fn lists_every_binding() {
        let mut coordinator = Coordinator::open_in_memory().unwrap();
        let created = coordinator
            .create_resource(
                "local",
                "todo.board@1",
                None,
                serde_json::json!({"tasks": {}}),
            )
            .unwrap();
        coordinator
            .bind_resource(
                &created.resource_id,
                "tasks",
                "todo.board@1",
                "read",
                Vec::new(),
            )
            .unwrap();
        coordinator
            .bind_resource_for_instance(
                &created.resource_id,
                "tasks",
                "todo.board@1",
                "read",
                Vec::new(),
                "app-1",
            )
            .unwrap();
        let bindings = coordinator.list_bindings().unwrap();
        assert_eq!(bindings.len(), 2);
        assert!(bindings.iter().any(|binding| binding.instance_id.is_some()));
    }

    #[test]
    fn lists_registered_operations_per_protocol() {
        let mut coordinator = Coordinator::open_in_memory().unwrap();
        for (protocol, operation) in [
            ("todo.board@1", "todo_create"),
            ("calendar.day@1", "calendar_plan"),
        ] {
            coordinator
                .register_handler_operation(
                    protocol,
                    operation,
                    "evolution",
                    "read_write",
                    HANDLER_MODULE,
                    "createTask",
                    "alice/app",
                )
                .unwrap();
        }
        assert_eq!(coordinator.list_operations(None).unwrap().len(), 2);
        let todo = coordinator.list_operations(Some("todo.board@1")).unwrap();
        assert_eq!(todo.len(), 1);
        assert_eq!(todo[0].operation, "todo_create");
        assert_eq!(todo[0].publisher, "alice/app");
        assert!(!todo[0].expose_mcp);
    }

    #[test]
    fn bounds_one_document_and_one_resource_bindings() {
        let mut coordinator = Coordinator::open_in_memory()
            .unwrap()
            .with_resource_limits(64, 2);
        let too_large = coordinator.create_resource(
            "local",
            "todo.board@1",
            None,
            serde_json::json!({"tasks": {"a": "x".repeat(128)}}),
        );
        assert!(matches!(too_large, Err(CoordinatorError::QuotaExceeded)));

        let created = coordinator
            .create_resource(
                "local",
                "todo.board@1",
                None,
                serde_json::json!({"tasks": {}}),
            )
            .unwrap();
        for port in ["tasks", "tasks-2"] {
            coordinator
                .bind_resource(
                    &created.resource_id,
                    port,
                    "todo.board@1",
                    "read",
                    Vec::new(),
                )
                .unwrap();
        }
        let third = coordinator.bind_resource(
            &created.resource_id,
            "tasks-3",
            "todo.board@1",
            "read",
            Vec::new(),
        );
        assert!(matches!(
            third,
            Err(CoordinatorError::Invalid("binding_limit_reached"))
        ));
    }
}
