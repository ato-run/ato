//! Device/provider-local installed-state database (SQLite).
//!
//! First minimal slice: the `materialized_objects` and `resource_claims`
//! tables, plus a storage **admission dry-run** built on the recorded claims.
//! Follows the SQLite conventions used by the local CAS index
//! (`adapters/resource/cas/index.rs`): bundled `rusqlite`, WAL journal,
//! `schema_migrations` table.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use chrono::Utc;
use rusqlite::{Connection, params};

use crate::common::paths::ato_state_dir;
use crate::error::{CapsuleError, Result};

use super::admission::{StorageAdmission, available_space_for_target, evaluate_storage_admission};
use super::port::{
    ConflictPolicy, PortAdmission, PortClaim, evaluate_port_admission, os_port_is_free,
};

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
            CREATE INDEX IF NOT EXISTS idx_resource_claims_kind
              ON resource_claims(kind);
            -- One claim per (installed app, resource kind): reinstall/update
            -- replaces the reservation rather than stacking a new row, so the
            -- reserved sum cannot double-count the same app.
            CREATE UNIQUE INDEX IF NOT EXISTS idx_resource_claims_profile_kind
              ON resource_claims(install_profile_key, kind);
            CREATE TABLE IF NOT EXISTS port_claims(
              id                  INTEGER PRIMARY KEY AUTOINCREMENT,
              install_profile_key TEXT NOT NULL,
              logical_endpoint    TEXT NOT NULL,
              preferred_port      INTEGER NOT NULL CHECK(preferred_port BETWEEN 1 AND 65535),
              last_actual_port    INTEGER CHECK(last_actual_port IS NULL OR last_actual_port BETWEEN 1 AND 65535),
              protocol            TEXT NOT NULL,
              conflict_policy     TEXT NOT NULL CHECK(conflict_policy IN ('remap', 'prompt', 'fail')),
              created_at          TEXT NOT NULL
            );
            -- One claim per (installed app, logical endpoint): re-claiming the
            -- same endpoint replaces the row rather than stacking duplicates.
            CREATE UNIQUE INDEX IF NOT EXISTS idx_port_claims_profile_endpoint
              ON port_claims(install_profile_key, logical_endpoint);
            CREATE INDEX IF NOT EXISTS idx_port_claims_preferred
              ON port_claims(preferred_port);
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

    /// Record (upsert) a storage reservation for an installed capsule.
    ///
    /// Keyed by `(install_profile_key, 'storage')`: reinstalling or updating the
    /// same app **replaces** its reservation instead of adding another row, so
    /// the reserved sum never double-counts a single app.
    pub fn record_storage_claim(&self, claim: &StorageClaim) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO resource_claims(install_profile_key, kind, reserved_bytes, detail, created_at)
             VALUES (?1, 'storage', ?2, NULL, ?3)
             ON CONFLICT(install_profile_key, kind) DO UPDATE SET
               reserved_bytes = excluded.reserved_bytes,
               detail = excluded.detail,
               created_at = excluded.created_at",
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

    // ── Port claims (#508) ──────────────────────────────────────────────────

    /// Record (upsert) a port claim for an installed capsule's logical endpoint.
    ///
    /// Keyed by `(install_profile_key, logical_endpoint)`: re-claiming the same
    /// endpoint replaces the row rather than stacking duplicates. A port claim
    /// is a relaunch ledger entry, **not** exclusive OS ownership.
    pub fn record_port_claim(&self, claim: &PortClaim) -> Result<()> {
        // A preferred port claim names a concrete port (1..=65535). Port 0 means
        // "any port" (auto-assign), which is not a ledger claim.
        if claim.preferred_port == 0 || claim.last_actual_port == Some(0) {
            return Err(CapsuleError::Runtime(format!(
                "port claim ports must be 1..=65535 (preferred={}, last_actual={:?})",
                claim.preferred_port, claim.last_actual_port
            )));
        }
        let now = Utc::now().to_rfc3339();
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO port_claims(
               install_profile_key, logical_endpoint, preferred_port,
               last_actual_port, protocol, conflict_policy, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(install_profile_key, logical_endpoint) DO UPDATE SET
               preferred_port = excluded.preferred_port,
               last_actual_port = excluded.last_actual_port,
               protocol = excluded.protocol,
               conflict_policy = excluded.conflict_policy,
               created_at = excluded.created_at",
            params![
                claim.install_profile_key,
                claim.logical_endpoint,
                claim.preferred_port as i64,
                claim.last_actual_port.map(|p| p as i64),
                claim.protocol,
                claim.conflict_policy.as_str(),
                now,
            ],
        )
        .map_err(|e| CapsuleError::Runtime(e.to_string()))?;
        Ok(())
    }

    /// All recorded port claims across installed capsules.
    pub fn port_claims(&self) -> Result<Vec<PortClaim>> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare(
                "SELECT install_profile_key, logical_endpoint, preferred_port,
                        last_actual_port, protocol, conflict_policy
                 FROM port_claims",
            )
            .map_err(|e| CapsuleError::Runtime(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                let preferred_port: i64 = row.get(2)?;
                let last_actual_port: Option<i64> = row.get(3)?;
                let policy_str: String = row.get(5)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    preferred_port,
                    last_actual_port,
                    row.get::<_, String>(4)?,
                    policy_str,
                ))
            })
            .map_err(|e| CapsuleError::Runtime(e.to_string()))?;

        let mut claims = Vec::new();
        for row in rows {
            let (install_profile_key, logical_endpoint, preferred, last_actual, protocol, policy) =
                row.map_err(|e| CapsuleError::Runtime(e.to_string()))?;
            let conflict_policy = ConflictPolicy::from_str_opt(&policy).ok_or_else(|| {
                CapsuleError::Runtime(format!("invalid conflict_policy in port_claims: {policy}"))
            })?;
            claims.push(PortClaim {
                install_profile_key,
                logical_endpoint,
                preferred_port: u16::try_from(preferred).map_err(|_| {
                    CapsuleError::Runtime(format!("port out of range in port_claims: {preferred}"))
                })?,
                last_actual_port: last_actual
                    .map(u16::try_from)
                    .transpose()
                    .map_err(|_| CapsuleError::Runtime("last_actual_port out of range".into()))?,
                protocol,
                conflict_policy,
            });
        }
        Ok(claims)
    }

    /// Ports already taken (by claims that contend with the requesting
    /// endpoint), for the same `protocol`. The requesting endpoint's **own**
    /// ledger entry — same `(install_profile_key, logical_endpoint, protocol)` —
    /// is excluded; every other claim (including a *different* endpoint of the
    /// **same** app) contends, since two endpoints cannot bind the same port.
    /// Only same-protocol claims contend (TCP and UDP are separate namespaces).
    fn contending_ports(
        &self,
        requesting_app: &str,
        logical_endpoint: &str,
        protocol: &str,
    ) -> Result<HashSet<u16>> {
        let mut taken = HashSet::new();
        for claim in self.port_claims()? {
            if claim.protocol != protocol {
                continue;
            }
            let is_own_endpoint = claim.install_profile_key == requesting_app
                && claim.logical_endpoint == logical_endpoint;
            if is_own_endpoint {
                continue;
            }
            taken.insert(claim.preferred_port);
            if let Some(actual) = claim.last_actual_port {
                taken.insert(actual);
            }
        }
        Ok(taken)
    }

    /// Port **admission** for a relaunch of `(requesting_app, logical_endpoint,
    /// protocol)`: is `preferred` free (not claimed by a contending endpoint and
    /// free on the OS)? If taken, the `policy` decides remap / prompt / fail.
    /// Reads only; never writes.
    pub fn check_port_admission(
        &self,
        requesting_app: &str,
        logical_endpoint: &str,
        protocol: &str,
        preferred: u16,
        policy: ConflictPolicy,
    ) -> Result<PortAdmission> {
        self.check_port_admission_with(
            requesting_app,
            logical_endpoint,
            protocol,
            preferred,
            policy,
            os_port_is_free,
        )
    }

    /// Like [`Self::check_port_admission`] but with an injectable OS-availability
    /// probe, so the decision can be exercised deterministically in tests.
    #[allow(clippy::too_many_arguments)]
    pub fn check_port_admission_with(
        &self,
        requesting_app: &str,
        logical_endpoint: &str,
        protocol: &str,
        preferred: u16,
        policy: ConflictPolicy,
        os_available: impl Fn(u16) -> bool,
    ) -> Result<PortAdmission> {
        let taken = self.contending_ports(requesting_app, logical_endpoint, protocol)?;
        let is_available = |port: u16| !taken.contains(&port) && os_available(port);
        Ok(evaluate_port_admission(preferred, policy, is_available))
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

    #[test]
    fn record_storage_claim_is_idempotent_per_install_profile() {
        let (_dir, db) = temp_db();
        let claim = StorageClaim {
            install_profile_key: "app".to_string(),
            reserved_bytes: 1_000_000_000,
        };
        db.record_storage_claim(&claim).unwrap();
        db.record_storage_claim(&claim).unwrap();
        assert_eq!(
            db.reserved_storage_bytes().unwrap(),
            1_000_000_000,
            "recording the same app's claim twice must not double-count"
        );
    }

    #[test]
    fn successful_reinstall_does_not_double_reserve_storage() {
        let (_dir, db) = temp_db();
        // First install reserves 1GB.
        db.record_storage_claim(&StorageClaim {
            install_profile_key: "app".to_string(),
            reserved_bytes: 1_000_000_000,
        })
        .unwrap();
        // Reinstall/update of the SAME app reserves 2GB → replaces, not adds.
        db.record_storage_claim(&StorageClaim {
            install_profile_key: "app".to_string(),
            reserved_bytes: 2_000_000_000,
        })
        .unwrap();
        assert_eq!(db.reserved_storage_bytes().unwrap(), 2_000_000_000);
        // A different app adds its own reservation.
        db.record_storage_claim(&StorageClaim {
            install_profile_key: "other".to_string(),
            reserved_bytes: 500_000_000,
        })
        .unwrap();
        assert_eq!(db.reserved_storage_bytes().unwrap(), 2_500_000_000);
    }

    fn port_claim_ep(app: &str, endpoint: &str, port: u16, policy: ConflictPolicy) -> PortClaim {
        PortClaim {
            install_profile_key: app.to_string(),
            logical_endpoint: endpoint.to_string(),
            preferred_port: port,
            last_actual_port: None,
            protocol: "tcp".to_string(),
            conflict_policy: policy,
        }
    }

    #[test]
    fn port_claim_upsert_is_per_app_endpoint() {
        let (_dir, db) = temp_db();
        let mut claim = port_claim_ep("app", "http", 3000, ConflictPolicy::Remap);
        db.record_port_claim(&claim).unwrap();
        claim.preferred_port = 3100;
        claim.last_actual_port = Some(49200);
        db.record_port_claim(&claim).unwrap();
        let claims = db.port_claims().unwrap();
        assert_eq!(
            claims.len(),
            1,
            "re-claiming the same endpoint must upsert, not duplicate"
        );
        assert_eq!(claims[0].preferred_port, 3100);
        assert_eq!(claims[0].last_actual_port, Some(49200));
    }

    #[test]
    fn record_port_claim_rejects_port_zero() {
        let (_dir, db) = temp_db();
        assert!(
            db.record_port_claim(&port_claim_ep("app", "http", 0, ConflictPolicy::Remap))
                .is_err(),
            "port 0 is auto-assign, not a concrete claim"
        );
    }

    #[test]
    fn port_admission_admits_uncontended_preferred() {
        let (_dir, db) = temp_db();
        let decision = db
            .check_port_admission_with("app", "http", "tcp", 3000, ConflictPolicy::Fail, |_| true)
            .unwrap();
        assert_eq!(decision, PortAdmission::Admitted { port: 3000 });
    }

    #[test]
    fn port_admission_fail_policy_rejects_when_another_app_holds_the_port() {
        let (_dir, db) = temp_db();
        db.record_port_claim(&port_claim_ep("app-b", "http", 3000, ConflictPolicy::Fail))
            .unwrap();
        // app-a wants the same port; OS reports everything free, so the conflict
        // is purely app-b's claim.
        let decision = db
            .check_port_admission_with("app-a", "http", "tcp", 3000, ConflictPolicy::Fail, |_| true)
            .unwrap();
        assert!(matches!(
            decision,
            PortAdmission::Rejected {
                preferred: 3000,
                policy: ConflictPolicy::Fail
            }
        ));
    }

    #[test]
    fn port_admission_remap_policy_returns_alternative_when_port_taken() {
        let (_dir, db) = temp_db();
        db.record_port_claim(&port_claim_ep("app-b", "http", 3000, ConflictPolicy::Remap))
            .unwrap();
        let decision = db
            .check_port_admission_with("app-a", "http", "tcp", 3000, ConflictPolicy::Remap, |_| {
                true
            })
            .unwrap();
        match decision {
            PortAdmission::Remapped { preferred, port } => {
                assert_eq!(preferred, 3000);
                assert_ne!(port, 3000);
                assert!(port >= 49152, "remap should pick from the dynamic range");
            }
            other => panic!("expected Remapped, got {other:?}"),
        }
    }

    #[test]
    fn port_admission_same_app_same_endpoint_ignores_own_claim() {
        let (_dir, db) = temp_db();
        // The same app re-claiming its OWN endpoint is not a self-conflict.
        db.record_port_claim(&port_claim_ep("app-a", "http", 3000, ConflictPolicy::Fail))
            .unwrap();
        let decision = db
            .check_port_admission_with("app-a", "http", "tcp", 3000, ConflictPolicy::Fail, |_| true)
            .unwrap();
        assert_eq!(decision, PortAdmission::Admitted { port: 3000 });
    }

    #[test]
    fn port_admission_same_app_different_endpoint_conflicts() {
        let (_dir, db) = temp_db();
        // app-a/http already holds 3000; app-a/admin cannot also bind 3000 — a
        // different endpoint of the same app still contends for the port.
        db.record_port_claim(&port_claim_ep("app-a", "http", 3000, ConflictPolicy::Fail))
            .unwrap();
        let decision = db
            .check_port_admission_with("app-a", "admin", "tcp", 3000, ConflictPolicy::Fail, |_| {
                true
            })
            .unwrap();
        assert!(
            matches!(
                decision,
                PortAdmission::Rejected {
                    preferred: 3000,
                    ..
                }
            ),
            "same app, different endpoint must conflict on the same port: {decision:?}"
        );
    }

    #[test]
    fn port_admission_different_protocol_same_port_does_not_conflict() {
        let (_dir, db) = temp_db();
        // A UDP claim on 3000 does not block a TCP request for 3000.
        let udp_claim = PortClaim {
            install_profile_key: "app-b".to_string(),
            logical_endpoint: "udp-svc".to_string(),
            preferred_port: 3000,
            last_actual_port: None,
            protocol: "udp".to_string(),
            conflict_policy: ConflictPolicy::Fail,
        };
        db.record_port_claim(&udp_claim).unwrap();
        let decision = db
            .check_port_admission_with("app-a", "http", "tcp", 3000, ConflictPolicy::Fail, |_| true)
            .unwrap();
        assert_eq!(decision, PortAdmission::Admitted { port: 3000 });
    }
}
