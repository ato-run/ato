//! Device/provider-local installed-state database (SQLite).
//!
//! First minimal slice: the `materialized_objects` and `resource_claims`
//! tables, plus a storage **admission dry-run** built on the recorded claims.
//! Follows the SQLite conventions used by the local CAS index
//! (`adapters/resource/cas/index.rs`): bundled `rusqlite`, WAL journal,
//! `schema_migrations` table.

use std::path::{Path, PathBuf};

use chrono::Utc;
use rusqlite::{Connection, params};

use crate::common::paths::ato_state_dir;
use crate::error::{CapsuleError, Result};

use super::admission::{StorageAdmission, available_space_for_target, evaluate_storage_admission};

const DB_FILE_NAME: &str = "installed_state.sqlite3";
const MIGRATION_0001: &str = "2026-06-05-0001-installed-state";

/// Convert a byte count to SQLite's signed `INTEGER` (i64), failing instead of
/// silently wrapping. A negative-on-overflow value would be read back as `0`
/// via the `max(0)` clamps in the sum queries, which could make a too-large
/// reservation under-count and wrongly admit an install — so storage accounting
/// must never accept an out-of-range value.
fn to_sql_i64_bytes(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        CapsuleError::Runtime(format!("{field} exceeds SQLite INTEGER range: {value}"))
    })
}

/// A materialized object recorded on this device — an artifact / dependency
/// output / runtime tool / model / image, keyed by content hash. Minimal
/// first-slice shape; GC and ref-count maintenance come later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedObject {
    pub object_hash: String,
    pub kind: String,
    pub path: String,
    pub size_bytes: u64,
    pub ref_count: i64,
    pub pinned: bool,
}

/// A storage reservation an installed capsule holds on this device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageClaim {
    pub install_profile_key: String,
    pub reserved_bytes: u64,
}

/// Device/provider-local installed-state database.
///
/// This is the local truth (#508): what has been materialized here and what is
/// claimed. The portable lockfile describes a resolution; this DB records what
/// actually exists on this device. Cross-device summarization is #509.
#[derive(Debug, Clone)]
pub struct InstalledStateDb {
    db_path: PathBuf,
}

impl InstalledStateDb {
    /// Open the default DB under `~/.ato/state/installed_state.sqlite3`.
    pub fn open_default() -> Result<Self> {
        Self::open(ato_state_dir())
    }

    /// Open (creating if needed) the installed-state DB under `state_dir`.
    pub fn open(state_dir: impl AsRef<Path>) -> Result<Self> {
        let state_dir = state_dir.as_ref();
        std::fs::create_dir_all(state_dir).map_err(|e| {
            CapsuleError::Runtime(format!("failed to create {}: {e}", state_dir.display()))
        })?;
        let this = Self {
            db_path: state_dir.join(DB_FILE_NAME),
        };
        this.init_schema()?;
        Ok(this)
    }

    /// Path to the SQLite file.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    fn connect(&self) -> Result<Connection> {
        let conn = Connection::open(&self.db_path).map_err(|e| {
            CapsuleError::Runtime(format!("failed to open {}: {e}", self.db_path.display()))
        })?;
        let rt = |e: rusqlite::Error| CapsuleError::Runtime(e.to_string());
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(rt)?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(rt)?;
        conn.pragma_update(None, "foreign_keys", "ON").map_err(rt)?;
        Ok(conn)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.connect()?;
        let rt = |e: rusqlite::Error| CapsuleError::Runtime(e.to_string());
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS schema_migrations(
              id TEXT PRIMARY KEY,
              applied_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS materialized_objects(
              object_hash  TEXT PRIMARY KEY,
              kind         TEXT NOT NULL,
              path         TEXT NOT NULL,
              size_bytes   INTEGER NOT NULL CHECK(size_bytes >= 0),
              ref_count    INTEGER NOT NULL DEFAULT 0 CHECK(ref_count >= 0),
              pinned       INTEGER NOT NULL DEFAULT 0 CHECK(pinned IN (0, 1)),
              created_at   TEXT NOT NULL,
              last_seen_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS resource_claims(
              id                  INTEGER PRIMARY KEY AUTOINCREMENT,
              install_profile_key TEXT NOT NULL,
              kind                TEXT NOT NULL,
              reserved_bytes      INTEGER CHECK(reserved_bytes IS NULL OR reserved_bytes >= 0),
              detail              TEXT,
              created_at          TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_resource_claims_profile
              ON resource_claims(install_profile_key);
            CREATE INDEX IF NOT EXISTS idx_resource_claims_kind
              ON resource_claims(kind);
            ",
        )
        .map_err(rt)?;
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations(id, applied_at) VALUES (?1, ?2)",
            params![MIGRATION_0001, Utc::now().to_rfc3339()],
        )
        .map_err(rt)?;
        Ok(())
    }

    /// Upsert a materialized object by content hash.
    pub fn record_materialized_object(&self, obj: &MaterializedObject) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO materialized_objects
               (object_hash, kind, path, size_bytes, ref_count, pinned, created_at, last_seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
             ON CONFLICT(object_hash) DO UPDATE SET
               kind=excluded.kind,
               path=excluded.path,
               size_bytes=excluded.size_bytes,
               ref_count=excluded.ref_count,
               pinned=excluded.pinned,
               last_seen_at=excluded.last_seen_at",
            params![
                obj.object_hash,
                obj.kind,
                obj.path,
                to_sql_i64_bytes(obj.size_bytes, "materialized_objects.size_bytes")?,
                obj.ref_count,
                obj.pinned as i64,
                now,
            ],
        )
        .map_err(|e| CapsuleError::Runtime(e.to_string()))?;
        Ok(())
    }

    /// Total size of all recorded materialized objects.
    pub fn total_materialized_bytes(&self) -> Result<u64> {
        let conn = self.connect()?;
        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(size_bytes), 0) FROM materialized_objects",
                [],
                |row| row.get(0),
            )
            .map_err(|e| CapsuleError::Runtime(e.to_string()))?;
        Ok(total.max(0) as u64)
    }

    /// Record a storage reservation for an installed capsule.
    pub fn record_storage_claim(&self, claim: &StorageClaim) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO resource_claims(install_profile_key, kind, reserved_bytes, detail, created_at)
             VALUES (?1, 'storage', ?2, NULL, ?3)",
            params![
                claim.install_profile_key,
                to_sql_i64_bytes(claim.reserved_bytes, "resource_claims.reserved_bytes")?,
                now,
            ],
        )
        .map_err(|e| CapsuleError::Runtime(e.to_string()))?;
        Ok(())
    }

    /// Total storage bytes currently reserved by installed capsules.
    pub fn reserved_storage_bytes(&self) -> Result<u64> {
        let conn = self.connect()?;
        let total: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(reserved_bytes), 0) FROM resource_claims WHERE kind='storage'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| CapsuleError::Runtime(e.to_string()))?;
        Ok(total.max(0) as u64)
    }

    /// Storage **admission dry-run**: would `required_bytes` fit on the volume
    /// backing `target_path`, after the storage already claimed by installed
    /// capsules? Reads only; never writes. `target_path` need not exist yet —
    /// the nearest existing ancestor's volume is probed (the install dir is
    /// created later, during materialization).
    ///
    /// First-slice assumption: a **single local install volume**. All storage
    /// claims are summed regardless of which volume/provider they belong to and
    /// subtracted from `target_path`'s free space. Per-volume / per-provider
    /// scoping (a `volume_key` / `provider_id` on `resource_claims`) is a
    /// follow-up once cloud/external-runner or split-volume state is modeled.
    pub fn check_storage_admission(
        &self,
        required_bytes: u64,
        target_path: impl AsRef<Path>,
    ) -> Result<StorageAdmission> {
        let available = available_space_for_target(target_path)?;
        let reserved = self.reserved_storage_bytes()?;
        Ok(evaluate_storage_admission(
            required_bytes,
            available,
            reserved,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> (tempfile::TempDir, InstalledStateDb) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = InstalledStateDb::open(dir.path().join("state")).expect("open db");
        (dir, db)
    }

    #[test]
    fn open_creates_schema_and_db_file() {
        let (_dir, db) = temp_db();
        assert!(db.db_path().exists(), "db file should be created");
        assert_eq!(db.reserved_storage_bytes().unwrap(), 0);
        assert_eq!(db.total_materialized_bytes().unwrap(), 0);
    }

    #[test]
    fn storage_claims_sum_across_installed_capsules() {
        let (_dir, db) = temp_db();
        db.record_storage_claim(&StorageClaim {
            install_profile_key: "app-a".to_string(),
            reserved_bytes: 40 * 1024 * 1024 * 1024,
        })
        .unwrap();
        db.record_storage_claim(&StorageClaim {
            install_profile_key: "app-b".to_string(),
            reserved_bytes: 50 * 1024 * 1024 * 1024,
        })
        .unwrap();
        assert_eq!(
            db.reserved_storage_bytes().unwrap(),
            90 * 1024 * 1024 * 1024
        );
    }

    #[test]
    fn materialized_object_upsert_is_idempotent_by_hash() {
        let (_dir, db) = temp_db();
        let mut obj = MaterializedObject {
            object_hash: "blake3:deadbeef".to_string(),
            kind: "dependency_output".to_string(),
            path: "/x/y".to_string(),
            size_bytes: 100,
            ref_count: 1,
            pinned: false,
        };
        db.record_materialized_object(&obj).unwrap();
        obj.size_bytes = 250;
        obj.ref_count = 2;
        db.record_materialized_object(&obj).unwrap();
        // Upsert, not duplicate: total reflects the latest size, not 100+250.
        assert_eq!(db.total_materialized_bytes().unwrap(), 250);
    }

    #[test]
    fn admission_rejects_when_requirement_exceeds_available() {
        // u64::MAX cannot fit on any real volume → typed Rejected through the
        // DB path, before any download.
        let (dir, db) = temp_db();
        let decision = db
            .check_storage_admission(u64::MAX, dir.path())
            .expect("dry-run");
        assert!(
            !decision.is_admitted(),
            "an impossible requirement must be rejected: {decision:?}"
        );
        assert!(matches!(decision, StorageAdmission::Rejected { .. }));
    }

    #[test]
    fn admission_admits_a_tiny_requirement_on_a_writable_volume() {
        let (dir, db) = temp_db();
        let decision = db.check_storage_admission(1, dir.path()).expect("dry-run");
        assert!(
            decision.is_admitted(),
            "1 byte should fit on a writable temp volume: {decision:?}"
        );
    }

    #[test]
    fn prior_claims_can_flip_admission_to_rejected() {
        // Reserve essentially all free space, then a modest requirement is
        // rejected even though the bare volume has room.
        let (dir, db) = temp_db();
        let free = available_space_for_target(dir.path()).unwrap();
        // Reserve more than the currently-free space (with a wide margin so the
        // result is stable even if free space wobbles between the two probes).
        db.record_storage_claim(&StorageClaim {
            install_profile_key: "hog".to_string(),
            reserved_bytes: free.saturating_add(100 * 1024 * 1024 * 1024),
        })
        .unwrap();
        let decision = db
            .check_storage_admission(64 * 1024 * 1024, dir.path())
            .expect("dry-run");
        assert!(
            !decision.is_admitted(),
            "with free space fully claimed, a new install must be rejected: {decision:?}"
        );
    }

    #[test]
    fn record_storage_claim_rejects_values_above_i64_max() {
        let (_dir, db) = temp_db();
        let err = db.record_storage_claim(&StorageClaim {
            install_profile_key: "huge".to_string(),
            reserved_bytes: u64::MAX,
        });
        assert!(
            err.is_err(),
            "u64::MAX reserved_bytes must be rejected, not wrapped to a negative i64"
        );
        // Nothing was stored, so accounting stays at zero (no negative wrap).
        assert_eq!(db.reserved_storage_bytes().unwrap(), 0);
    }

    #[test]
    fn record_materialized_object_rejects_size_above_i64_max() {
        let (_dir, db) = temp_db();
        let err = db.record_materialized_object(&MaterializedObject {
            object_hash: "blake3:big".to_string(),
            kind: "blob".to_string(),
            path: "/x".to_string(),
            size_bytes: u64::MAX,
            ref_count: 1,
            pinned: false,
        });
        assert!(
            err.is_err(),
            "u64::MAX size_bytes must be rejected, not wrapped to a negative i64"
        );
        assert_eq!(db.total_materialized_bytes().unwrap(), 0);
    }

    #[test]
    fn oversized_claim_does_not_wrap_and_underreserve() {
        let (_dir, db) = temp_db();
        // A valid claim at the i64 ceiling is counted exactly.
        let big = i64::MAX as u64;
        db.record_storage_claim(&StorageClaim {
            install_profile_key: "near-max".to_string(),
            reserved_bytes: big,
        })
        .unwrap();
        assert_eq!(db.reserved_storage_bytes().unwrap(), big);

        // An out-of-range claim is refused at write time rather than wrapping
        // the reserved sum to a small/zero value that would wrongly admit a new
        // install.
        assert!(
            db.record_storage_claim(&StorageClaim {
                install_profile_key: "overflow".to_string(),
                reserved_bytes: big + 1,
            })
            .is_err()
        );
        assert_eq!(
            db.reserved_storage_bytes().unwrap(),
            big,
            "reserved sum must be unchanged by the rejected oversized claim"
        );
    }
}
