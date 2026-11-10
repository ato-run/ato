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

pub mod http;

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Bumped when the on-disk schema changes in a way that needs a migration.
pub const SCHEMA_VERSION: i64 = 1;

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
  PRIMARY KEY (resource_id, operation_id)
);
CREATE TABLE IF NOT EXISTS outbox (
  resource_id TEXT NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
  revision INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (resource_id, revision)
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
}

pub struct Coordinator {
    conn: Connection,
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
        conn.execute_batch(SCHEMA)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Self { conn })
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
        if input.operation_id.is_empty() || input.operation_id.len() > 128 {
            return Err(CoordinatorError::Invalid("invalid_operation_id"));
        }
        let (document_json, content_digest, byte_size) = encode_document(&input.document)?;
        let fingerprint = fingerprint(&input)?;
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
                "SELECT fingerprint,receipt_json,revision FROM operation_results
                 WHERE resource_id=? AND operation_id=?",
                params![resource_id, input.operation_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        if let Some((stored_fingerprint, receipt_json, revision)) = existing {
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
        let result = serde_json::json!({
            "revision": next_revision,
            "content_digest": content_digest,
        });
        tx.execute(
            "INSERT INTO operation_results(resource_id,operation_id,fingerprint,result_json,receipt_json,revision,created_at)
             VALUES (?,?,?,?,?,?,?)",
            params![
                resource_id,
                input.operation_id,
                fingerprint,
                serde_json::to_string(&result)?,
                serde_json::to_string(&receipt)?,
                next_revision,
                now
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
        })
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
}
