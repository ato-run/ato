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
use rusqlite::{Connection, OptionalExtension, params};

use crate::common::paths::ato_state_dir;
use crate::error::{CapsuleError, Result};

use super::admission::{StorageAdmission, available_space_for_target, evaluate_storage_admission};
use super::launch_condition::{
    LOCAL_PROVIDER_ID, LaunchConditionClaim, LaunchConditionKind, LaunchConditionSource,
    LaunchConditionStatus, validate_redacted_detail_json,
};
use super::launch_input::{validate_condition_key, validate_locator_id};
use super::port::{
    ConflictPolicy, PortAdmission, PortClaim, evaluate_port_admission, os_port_is_free,
};
use super::relaunch_admission::RelaunchAdmissionInput;

const DB_FILE_NAME: &str = "installed_state.sqlite3";
const MIGRATION_0001: &str = "2026-06-05-0001-installed-state";
const MIGRATION_0002: &str = "2026-06-05-0002-launch-condition-ledger";
const MIGRATION_0003: &str = "2026-06-05-0003-grant-binding-refs";

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
            -- Canonical per-installed-app launch condition ledger (#508). The
            -- device/provider-local source of truth for installed-app relaunch
            -- conditions; resource_claims/port_claims are fast projections of it.
            CREATE TABLE IF NOT EXISTS launch_condition_claims(
              id                  INTEGER PRIMARY KEY AUTOINCREMENT,
              install_profile_key TEXT NOT NULL,
              install_revision_id TEXT NOT NULL DEFAULT '',
              provider_id         TEXT NOT NULL DEFAULT 'local',
              kind                TEXT NOT NULL,
              condition_key       TEXT NOT NULL,
              status              TEXT NOT NULL,
              required            INTEGER NOT NULL DEFAULT 1,
              source              TEXT NOT NULL,
              detail_json         TEXT NOT NULL DEFAULT '{}',
              redacted            INTEGER NOT NULL DEFAULT 1,
              created_at          INTEGER NOT NULL,
              updated_at          INTEGER NOT NULL,
              CHECK (kind IN (
                'storage', 'port', 'env', 'secret', 'state', 'runtime',
                'runtime_tool', 'provider_capability', 'network', 'hardware',
                'policy'
              )),
              CHECK (status IN (
                'satisfied', 'missing', 'stale', 'unavailable',
                'user_grant_required', 'provider_required', 'unknown'
              )),
              CHECK (source IN (
                'manifest', 'lockfile', 'installed_state', 'storage_claim',
                'port_claim', 'secret_store', 'provider_snapshot',
                'runtime_resolution', 'manual'
              )),
              CHECK (required IN (0, 1)),
              CHECK (redacted IN (0, 1))
            );
            -- Identity of a condition: app + revision + provider + kind + key.
            -- install_revision_id/provider_id are non-NULL ('' / 'local') so the
            -- UNIQUE index treats them as concrete (SQLite UNIQUE ignores NULLs).
            CREATE UNIQUE INDEX IF NOT EXISTS idx_launch_condition_claim_unique
              ON launch_condition_claims(
                install_profile_key, install_revision_id, provider_id,
                kind, condition_key
              );
            CREATE INDEX IF NOT EXISTS idx_launch_condition_claims_profile
              ON launch_condition_claims(install_profile_key);
            CREATE INDEX IF NOT EXISTS idx_launch_condition_claims_status
              ON launch_condition_claims(install_profile_key, status);
            CREATE INDEX IF NOT EXISTS idx_launch_condition_claims_kind
              ON launch_condition_claims(install_profile_key, kind);
            -- Existence-only registry of secret grants (#508). Records *that* a
            -- grant exists for a launch condition, identified by the reserved
            -- launch-condition key (e.g. 'secret.OPENAI_API_KEY') — the same
            -- vocabulary as the capsule:// query, not a URI. Stores a redacted
            -- grant id, never the secret value. The relaunch resolver checks
            -- presence here instead of calling a value-returning secret store.
            CREATE TABLE IF NOT EXISTS secret_grant_refs(
              grant_id            TEXT PRIMARY KEY,
              install_profile_key TEXT NOT NULL,
              capsule_location    TEXT NOT NULL DEFAULT '',
              condition_key       TEXT NOT NULL,
              status              TEXT NOT NULL,
              redacted            INTEGER NOT NULL DEFAULT 1,
              created_at          INTEGER NOT NULL,
              updated_at          INTEGER NOT NULL,
              CHECK (status IN ('granted', 'missing', 'revoked', 'stale')),
              CHECK (redacted IN (0, 1))
            );
            CREATE INDEX IF NOT EXISTS idx_secret_grant_refs_profile
              ON secret_grant_refs(install_profile_key);
            -- Existence-only registry of logical state bindings (#508). Records a
            -- logical binding id for a launch condition, identified by its
            -- reserved condition key (e.g. 'state.data') — never a raw host path.
            CREATE TABLE IF NOT EXISTS state_binding_refs(
              binding_id          TEXT PRIMARY KEY,
              install_profile_key TEXT NOT NULL,
              capsule_location    TEXT NOT NULL DEFAULT '',
              condition_key       TEXT NOT NULL,
              state_key           TEXT NOT NULL,
              status              TEXT NOT NULL,
              redacted            INTEGER NOT NULL DEFAULT 1,
              created_at          INTEGER NOT NULL,
              updated_at          INTEGER NOT NULL,
              CHECK (status IN ('bound', 'missing', 'stale')),
              CHECK (redacted IN (0, 1))
            );
            CREATE INDEX IF NOT EXISTS idx_state_binding_refs_profile
              ON state_binding_refs(install_profile_key);
            ",
        )
        .map_err(rt)?;
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations(id, applied_at) VALUES (?1, ?2)",
            params![MIGRATION_0001, now],
        )
        .map_err(rt)?;
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations(id, applied_at) VALUES (?1, ?2)",
            params![MIGRATION_0002, now],
        )
        .map_err(rt)?;
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations(id, applied_at) VALUES (?1, ?2)",
            params![MIGRATION_0003, now],
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

    // ── Launch condition ledger (#508) ──────────────────────────────────────
    //
    // The canonical per-installed-app condition ledger. `resource_claims` and
    // `port_claims` above remain query-optimized projections for admission;
    // `launch_condition_claims` is the full condition model and the SOT for
    // installed-app relaunch.

    /// Validate a claim before writing: detail must be redacted JSON (no
    /// embedded secret values) and a `Secret` condition must be marked redacted.
    fn validate_claim(claim: &LaunchConditionClaim) -> Result<()> {
        validate_redacted_detail_json(claim.kind, &claim.detail_json)?;
        if claim.kind == LaunchConditionKind::Secret && !claim.redacted {
            return Err(CapsuleError::Runtime(
                "secret launch condition must be stored redacted (redacted = true)".to_string(),
            ));
        }
        Ok(())
    }

    fn revision_sentinel(install_revision_id: Option<&str>) -> &str {
        install_revision_id.unwrap_or("")
    }

    fn provider_sentinel(provider_id: Option<&str>) -> &str {
        provider_id.unwrap_or(LOCAL_PROVIDER_ID)
    }

    /// Upsert one launch condition. Keyed by `(install_profile_key,
    /// install_revision_id, provider_id, kind, condition_key)`: re-recording the
    /// same condition replaces it (preserving `created_at`) rather than stacking.
    pub fn record_launch_condition_claim(&self, claim: &LaunchConditionClaim) -> Result<()> {
        let conn = self.connect()?;
        Self::record_launch_condition_claim_conn(&conn, claim)
    }

    fn record_launch_condition_claim_conn(
        conn: &Connection,
        claim: &LaunchConditionClaim,
    ) -> Result<()> {
        Self::validate_claim(claim)?;
        let now = Utc::now().timestamp_millis();
        conn.execute(
            "INSERT INTO launch_condition_claims(
               install_profile_key, install_revision_id, provider_id, kind,
               condition_key, status, required, source, detail_json, redacted,
               created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
             ON CONFLICT(install_profile_key, install_revision_id, provider_id, kind, condition_key)
             DO UPDATE SET
               status = excluded.status,
               required = excluded.required,
               source = excluded.source,
               detail_json = excluded.detail_json,
               redacted = excluded.redacted,
               updated_at = excluded.updated_at",
            params![
                claim.install_profile_key,
                Self::revision_sentinel(claim.install_revision_id.as_deref()),
                Self::provider_sentinel(claim.provider_id.as_deref()),
                claim.kind.as_str(),
                claim.condition_key,
                claim.status.as_str(),
                claim.required as i64,
                claim.source.as_str(),
                claim.detail_json,
                claim.redacted as i64,
                now,
            ],
        )
        .map_err(|e| CapsuleError::Runtime(e.to_string()))?;
        Ok(())
    }

    /// Upsert several launch conditions atomically (all-or-nothing).
    pub fn record_launch_condition_claims(&self, claims: &[LaunchConditionClaim]) -> Result<()> {
        for claim in claims {
            Self::validate_claim(claim)?;
        }
        let mut conn = self.connect()?;
        let tx = conn
            .transaction()
            .map_err(|e| CapsuleError::Runtime(e.to_string()))?;
        for claim in claims {
            Self::record_launch_condition_claim_conn(&tx, claim)?;
        }
        tx.commit()
            .map_err(|e| CapsuleError::Runtime(e.to_string()))?;
        Ok(())
    }

    /// Replace the full condition set for `(install_profile_key,
    /// install_revision_id, provider_id)` atomically: delete the prior rows and
    /// insert `claims` in one transaction, so a partial write can never leave the
    /// SOT in a torn state. All `claims` must match the given identity scope.
    pub fn replace_launch_conditions_for_revision(
        &self,
        install_profile_key: &str,
        install_revision_id: Option<&str>,
        provider_id: Option<&str>,
        claims: &[LaunchConditionClaim],
    ) -> Result<()> {
        let revision = Self::revision_sentinel(install_revision_id);
        let provider = Self::provider_sentinel(provider_id);
        for claim in claims {
            Self::validate_claim(claim)?;
            if claim.install_profile_key != install_profile_key
                || Self::revision_sentinel(claim.install_revision_id.as_deref()) != revision
                || Self::provider_sentinel(claim.provider_id.as_deref()) != provider
            {
                return Err(CapsuleError::Runtime(format!(
                    "launch condition claim '{}' does not match the replacement scope \
                     (app={install_profile_key}, revision={revision}, provider={provider})",
                    claim.condition_key
                )));
            }
        }
        let mut conn = self.connect()?;
        let tx = conn
            .transaction()
            .map_err(|e| CapsuleError::Runtime(e.to_string()))?;
        tx.execute(
            "DELETE FROM launch_condition_claims
             WHERE install_profile_key = ?1 AND install_revision_id = ?2 AND provider_id = ?3",
            params![install_profile_key, revision, provider],
        )
        .map_err(|e| CapsuleError::Runtime(e.to_string()))?;
        for claim in claims {
            Self::record_launch_condition_claim_conn(&tx, claim)?;
        }
        tx.commit()
            .map_err(|e| CapsuleError::Runtime(e.to_string()))?;
        Ok(())
    }

    /// High-level SOT entry point: record the full known launch condition set for
    /// an installed app revision, replacing any prior set for that revision.
    ///
    /// This is **strict** — a failure here means the installed-state SOT could
    /// not be updated, and the caller (install / revision activation) must treat
    /// it as a failure rather than swallow it. (Runtime observation updates such
    /// as `last_actual_port` may be best-effort; recording the condition ledger
    /// is not.)
    pub fn record_installed_launch_ledger(
        &self,
        install_profile_key: &str,
        install_revision_id: Option<&str>,
        provider_id: Option<&str>,
        claims: &[LaunchConditionClaim],
    ) -> Result<()> {
        self.replace_launch_conditions_for_revision(
            install_profile_key,
            install_revision_id,
            provider_id,
            claims,
        )
    }

    /// All launch conditions recorded for an installed app (across revisions and
    /// providers).
    pub fn list_launch_condition_claims(
        &self,
        install_profile_key: &str,
    ) -> Result<Vec<LaunchConditionClaim>> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare(
                "SELECT install_profile_key, install_revision_id, provider_id, kind,
                        condition_key, status, required, source, detail_json, redacted
                 FROM launch_condition_claims
                 WHERE install_profile_key = ?1
                 ORDER BY kind, condition_key",
            )
            .map_err(|e| CapsuleError::Runtime(e.to_string()))?;
        let rows = stmt
            .query_map(params![install_profile_key], Self::map_condition_row)
            .map_err(|e| CapsuleError::Runtime(e.to_string()))?;
        Self::collect_condition_rows(rows)
    }

    /// Launch conditions for a specific `(app, revision, provider)` scope.
    pub fn list_launch_condition_claims_for_revision(
        &self,
        install_profile_key: &str,
        install_revision_id: Option<&str>,
        provider_id: Option<&str>,
    ) -> Result<Vec<LaunchConditionClaim>> {
        let conn = self.connect()?;
        let mut stmt = conn
            .prepare(
                "SELECT install_profile_key, install_revision_id, provider_id, kind,
                        condition_key, status, required, source, detail_json, redacted
                 FROM launch_condition_claims
                 WHERE install_profile_key = ?1 AND install_revision_id = ?2 AND provider_id = ?3
                 ORDER BY kind, condition_key",
            )
            .map_err(|e| CapsuleError::Runtime(e.to_string()))?;
        let rows = stmt
            .query_map(
                params![
                    install_profile_key,
                    Self::revision_sentinel(install_revision_id),
                    Self::provider_sentinel(provider_id),
                ],
                Self::map_condition_row,
            )
            .map_err(|e| CapsuleError::Runtime(e.to_string()))?;
        Self::collect_condition_rows(rows)
    }

    /// Load the relaunch admission input for an installed app revision: the
    /// recorded launch conditions plus the identity, ready for
    /// [`evaluate_relaunch_admission`](super::relaunch_admission::evaluate_relaunch_admission).
    ///
    /// Reads the **revision-specific** ledger only (v1). An empty result is
    /// returned verbatim (empty `claims`) — the evaluator, not this method,
    /// decides what an empty ledger means (a `LedgerMissing` warning, never "no
    /// conditions"). Profile-wide fallback for legacy installs is left to the
    /// caller.
    pub fn load_relaunch_admission_input(
        &self,
        install_profile_key: &str,
        install_revision_id: Option<&str>,
        provider_id: Option<&str>,
    ) -> Result<RelaunchAdmissionInput> {
        let claims = self.list_launch_condition_claims_for_revision(
            install_profile_key,
            install_revision_id,
            provider_id,
        )?;
        Ok(RelaunchAdmissionInput {
            install_profile_key: install_profile_key.to_string(),
            install_revision_id: install_revision_id.map(str::to_string),
            provider_id: provider_id.map(str::to_string),
            claims,
        })
    }

    /// Persist the resolved condition set for a revision after relaunch
    /// resolution. Delegates to [`Self::replace_launch_conditions_for_revision`]
    /// (the same transactional all-or-nothing replace).
    ///
    /// Unlike the **install-time** ledger write (strict — a failure fails the
    /// install), this relaunch-time write-through is intended to be used
    /// **best-effort** by the caller: the in-memory resolved claims are
    /// authoritative for the current launch, so a persistence failure should warn
    /// and continue, not abort the launch. Callers pass only durable resolutions
    /// (see `RelaunchResolution::durable_persist_claims`); transient
    /// host-env-presence resolutions must not be persisted.
    pub fn record_resolved_launch_conditions(
        &self,
        install_profile_key: &str,
        install_revision_id: Option<&str>,
        provider_id: Option<&str>,
        claims: &[LaunchConditionClaim],
    ) -> Result<()> {
        self.replace_launch_conditions_for_revision(
            install_profile_key,
            install_revision_id,
            provider_id,
            claims,
        )
    }

    // ── Secret grant / state binding existence registries (#508) ────────────
    //
    // These record *that* a grant / binding exists (by a redacted logical id),
    // so the relaunch resolver can confirm presence without reading any secret
    // value or raw host path. They never store a value or a path.

    /// Record (upsert) a secret grant reference as `granted`. `grant_id` is a
    /// short logical id (never a secret value); `condition_key` is the reserved
    /// launch-condition key (e.g. `secret.OPENAI_API_KEY`) — the same vocabulary
    /// as the `capsule://` query, **not** a URI. `capsule_location` is an
    /// optional capsule-location label.
    ///
    /// Validation is enforced **here**, at the SOT boundary — callers are not
    /// trusted: a URI/fragment/path-like condition key or a path/token/scheme-like
    /// grant id is rejected, so a raw secret/token can never enter the registry
    /// (and thus can never make a condition spuriously satisfied).
    pub fn record_secret_grant_ref(
        &self,
        install_profile_key: &str,
        capsule_location: Option<&str>,
        condition_key: &str,
        grant_id: &str,
    ) -> Result<()> {
        validate_condition_key(condition_key)?;
        validate_locator_id(grant_id, "grant")?;
        let now = Utc::now().timestamp_millis();
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO secret_grant_refs(
               grant_id, install_profile_key, capsule_location, condition_key,
               status, redacted, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'granted', 1, ?5, ?5)
             ON CONFLICT(grant_id) DO UPDATE SET
               install_profile_key = excluded.install_profile_key,
               capsule_location = excluded.capsule_location,
               condition_key = excluded.condition_key,
               status = 'granted',
               updated_at = excluded.updated_at",
            params![
                grant_id,
                install_profile_key,
                capsule_location.unwrap_or(""),
                condition_key,
                now
            ],
        )
        .map_err(|e| CapsuleError::Runtime(e.to_string()))?;
        Ok(())
    }

    /// Existence-only probe: is there a `granted` secret grant for `grant_id`?
    /// Reads no secret value. An invalid `grant_id` (path/token/scheme-like)
    /// resolves to `Ok(false)` — a malformed id can never satisfy a condition
    /// (conservative-false beats propagating an error into the launch path).
    pub fn secret_grant_ref_exists(&self, grant_id: &str) -> Result<bool> {
        if validate_locator_id(grant_id, "grant").is_err() {
            return Ok(false);
        }
        let conn = self.connect()?;
        let exists = conn
            .query_row(
                "SELECT 1 FROM secret_grant_refs WHERE grant_id = ?1 AND status = 'granted'",
                params![grant_id],
                |_| Ok(()),
            )
            .optional()
            .map_err(|e| CapsuleError::Runtime(e.to_string()))?
            .is_some();
        Ok(exists)
    }

    /// Record (upsert) a logical state binding reference as `bound`. `binding_id`
    /// is a short logical id (never a host path); `condition_key` is the reserved
    /// launch-condition key (e.g. `state.data`). `capsule_location` is an optional
    /// capsule-location label.
    ///
    /// Validation is enforced **here**, at the SOT boundary — a URI/path-like
    /// condition key or a path/scheme-like binding id is rejected, so a raw host
    /// path can never enter the registry.
    pub fn record_state_binding_ref(
        &self,
        install_profile_key: &str,
        capsule_location: Option<&str>,
        condition_key: &str,
        state_key: &str,
        binding_id: &str,
    ) -> Result<()> {
        validate_condition_key(condition_key)?;
        validate_locator_id(binding_id, "binding")?;
        let now = Utc::now().timestamp_millis();
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO state_binding_refs(
               binding_id, install_profile_key, capsule_location, condition_key,
               state_key, status, redacted, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'bound', 1, ?6, ?6)
             ON CONFLICT(binding_id) DO UPDATE SET
               install_profile_key = excluded.install_profile_key,
               capsule_location = excluded.capsule_location,
               condition_key = excluded.condition_key,
               state_key = excluded.state_key,
               status = 'bound',
               updated_at = excluded.updated_at",
            params![
                binding_id,
                install_profile_key,
                capsule_location.unwrap_or(""),
                condition_key,
                state_key,
                now
            ],
        )
        .map_err(|e| CapsuleError::Runtime(e.to_string()))?;
        Ok(())
    }

    /// Existence-only probe: is there a `bound` state binding for `binding_id`?
    /// Reads no host path. An invalid `binding_id` (path/scheme-like) resolves to
    /// `Ok(false)` — a malformed id can never satisfy a condition.
    pub fn state_binding_ref_exists(&self, binding_id: &str) -> Result<bool> {
        if validate_locator_id(binding_id, "binding").is_err() {
            return Ok(false);
        }
        let conn = self.connect()?;
        let exists = conn
            .query_row(
                "SELECT 1 FROM state_binding_refs WHERE binding_id = ?1 AND status = 'bound'",
                params![binding_id],
                |_| Ok(()),
            )
            .optional()
            .map_err(|e| CapsuleError::Runtime(e.to_string()))?
            .is_some();
        Ok(exists)
    }

    /// Map one row into the intermediate tuple of raw column values.
    #[allow(clippy::type_complexity)]
    fn map_condition_row(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<(
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
        String,
        String,
        i64,
    )> {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
            row.get(9)?,
        ))
    }

    #[allow(clippy::type_complexity)]
    fn collect_condition_rows(
        rows: impl Iterator<
            Item = rusqlite::Result<(
                String,
                String,
                String,
                String,
                String,
                String,
                i64,
                String,
                String,
                i64,
            )>,
        >,
    ) -> Result<Vec<LaunchConditionClaim>> {
        let mut claims = Vec::new();
        for row in rows {
            let (
                install_profile_key,
                revision,
                provider,
                kind,
                condition_key,
                status,
                required,
                source,
                detail_json,
                redacted,
            ) = row.map_err(|e| CapsuleError::Runtime(e.to_string()))?;
            let kind = LaunchConditionKind::from_str_opt(&kind).ok_or_else(|| {
                CapsuleError::Runtime(format!("invalid kind in launch_condition_claims: {kind}"))
            })?;
            let status = LaunchConditionStatus::from_str_opt(&status).ok_or_else(|| {
                CapsuleError::Runtime(format!(
                    "invalid status in launch_condition_claims: {status}"
                ))
            })?;
            let source = LaunchConditionSource::from_str_opt(&source).ok_or_else(|| {
                CapsuleError::Runtime(format!(
                    "invalid source in launch_condition_claims: {source}"
                ))
            })?;
            claims.push(LaunchConditionClaim {
                install_profile_key,
                // Sentinels collapse back to None (the default scope).
                install_revision_id: (!revision.is_empty()).then_some(revision),
                provider_id: (provider != LOCAL_PROVIDER_ID).then_some(provider),
                kind,
                condition_key,
                status,
                required: required != 0,
                source,
                detail_json,
                redacted: redacted != 0,
            });
        }
        Ok(claims)
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

    // ── Launch condition ledger (#508) ──────────────────────────────────────

    use super::super::launch_condition::{
        LEDGER_EXTRACTION_STATUS_KEY, launch_condition_extraction_status,
        launch_condition_from_port_claim, launch_condition_from_storage_claim,
    };

    fn condition(
        app: &str,
        kind: LaunchConditionKind,
        condition_key: &str,
        status: LaunchConditionStatus,
    ) -> LaunchConditionClaim {
        LaunchConditionClaim {
            install_profile_key: app.to_string(),
            install_revision_id: None,
            provider_id: None,
            kind,
            condition_key: condition_key.to_string(),
            status,
            required: true,
            source: LaunchConditionSource::Manifest,
            detail_json: "{}".to_string(),
            redacted: true,
        }
    }

    fn table_exists(db: &InstalledStateDb, name: &str) -> bool {
        let conn = db.connect().unwrap();
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            params![name],
            |_| Ok(()),
        )
        .is_ok()
    }

    #[test]
    fn launch_condition_claims_schema_is_created() {
        let (_dir, db) = temp_db();
        assert!(table_exists(&db, "launch_condition_claims"));
        assert!(
            db.list_launch_condition_claims("nobody")
                .unwrap()
                .is_empty(),
            "a fresh ledger has no conditions"
        );
    }

    #[test]
    fn record_launch_condition_claim_upserts_same_key() {
        let (_dir, db) = temp_db();
        let mut c = condition(
            "app",
            LaunchConditionKind::Runtime,
            "deno",
            LaunchConditionStatus::Missing,
        );
        db.record_launch_condition_claim(&c).unwrap();
        c.status = LaunchConditionStatus::Satisfied;
        db.record_launch_condition_claim(&c).unwrap();
        let claims = db.list_launch_condition_claims("app").unwrap();
        assert_eq!(claims.len(), 1, "same key must upsert, not duplicate");
        assert_eq!(claims[0].status, LaunchConditionStatus::Satisfied);
    }

    #[test]
    fn replace_launch_conditions_for_revision_replaces_atomically() {
        let (_dir, db) = temp_db();
        // Initial set: two conditions for rev1.
        let mut a = condition(
            "app",
            LaunchConditionKind::Storage,
            "requirements.disk",
            LaunchConditionStatus::Satisfied,
        );
        a.install_revision_id = Some("rev1".to_string());
        let mut b = condition(
            "app",
            LaunchConditionKind::Port,
            "main.tcp",
            LaunchConditionStatus::Satisfied,
        );
        b.install_revision_id = Some("rev1".to_string());
        db.record_installed_launch_ledger("app", Some("rev1"), None, &[a, b])
            .unwrap();
        assert_eq!(
            db.list_launch_condition_claims_for_revision("app", Some("rev1"), None)
                .unwrap()
                .len(),
            2
        );

        // Replace rev1 with a single different condition → old ones are gone.
        let mut c = condition(
            "app",
            LaunchConditionKind::Env,
            "PORT",
            LaunchConditionStatus::Satisfied,
        );
        c.install_revision_id = Some("rev1".to_string());
        db.record_installed_launch_ledger("app", Some("rev1"), None, &[c])
            .unwrap();
        let after = db
            .list_launch_condition_claims_for_revision("app", Some("rev1"), None)
            .unwrap();
        assert_eq!(after.len(), 1, "replace must delete the prior revision set");
        assert_eq!(after[0].condition_key, "PORT");
    }

    #[test]
    fn replace_rejects_claims_outside_the_scope() {
        let (_dir, db) = temp_db();
        let mut wrong = condition(
            "app",
            LaunchConditionKind::Storage,
            "requirements.disk",
            LaunchConditionStatus::Satisfied,
        );
        wrong.install_revision_id = Some("other-rev".to_string());
        let err = db.replace_launch_conditions_for_revision("app", Some("rev1"), None, &[wrong]);
        assert!(
            err.is_err(),
            "a claim whose revision differs from the replacement scope must be rejected"
        );
    }

    #[test]
    fn list_launch_condition_claims_returns_profile_claims() {
        let (_dir, db) = temp_db();
        db.record_launch_condition_claims(&[
            condition(
                "app",
                LaunchConditionKind::Storage,
                "requirements.disk",
                LaunchConditionStatus::Satisfied,
            ),
            condition(
                "app",
                LaunchConditionKind::ProviderCapability,
                "gpu.nvidia.cuda",
                LaunchConditionStatus::ProviderRequired,
            ),
            condition(
                "other",
                LaunchConditionKind::Runtime,
                "deno",
                LaunchConditionStatus::Satisfied,
            ),
        ])
        .unwrap();
        let app = db.list_launch_condition_claims("app").unwrap();
        assert_eq!(
            app.len(),
            2,
            "only the queried app's conditions are returned"
        );
        assert!(app.iter().all(|c| c.install_profile_key == "app"));
    }

    #[test]
    fn installed_launch_conditions_are_loaded_from_db_not_lockfile() {
        // SOT semantics: the installed app's condition set is reconstructed from
        // the DB ledger alone — no lockfile/manifest is consulted here.
        let (_dir, db) = temp_db();
        let with_rev = |mut c: LaunchConditionClaim| {
            c.install_revision_id = Some("rev1".to_string());
            c
        };
        let claims = vec![
            with_rev(condition(
                "app",
                LaunchConditionKind::Storage,
                "requirements.disk",
                LaunchConditionStatus::Satisfied,
            )),
            with_rev(condition(
                "app",
                LaunchConditionKind::Secret,
                "OPENAI_API_KEY",
                LaunchConditionStatus::UserGrantRequired,
            )),
        ];
        db.record_installed_launch_ledger("app", Some("rev1"), None, &claims)
            .unwrap();

        let loaded = db.list_launch_condition_claims("app").unwrap();
        assert_eq!(loaded.len(), 2);
        let secret = loaded
            .iter()
            .find(|c| c.kind == LaunchConditionKind::Secret)
            .expect("secret condition present in ledger");
        assert_eq!(secret.status, LaunchConditionStatus::UserGrantRequired);
        assert_eq!(secret.condition_key, "OPENAI_API_KEY");
    }

    #[test]
    fn empty_ledger_is_not_used_to_mean_no_conditions() {
        let (_dir, db) = temp_db();
        // A never-installed app has a genuinely empty ledger.
        assert!(
            db.list_launch_condition_claims("ghost").unwrap().is_empty(),
            "a never-installed app records nothing"
        );

        // An installed revision always carries the baseline marker, so its ledger
        // is non-empty even with no extracted requirements — "empty" can only mean
        // "nothing recorded", never "this app has no launch conditions".
        let baseline = launch_condition_extraction_status(
            "app",
            Some("rev1"),
            &[LaunchConditionKind::Storage],
        );
        db.record_installed_launch_ledger("app", Some("rev1"), None, &[baseline])
            .unwrap();
        let loaded = db.list_launch_condition_claims("app").unwrap();
        assert_eq!(loaded.len(), 1);
        let marker = &loaded[0];
        assert_eq!(marker.condition_key, LEDGER_EXTRACTION_STATUS_KEY);
        assert_eq!(marker.kind, LaunchConditionKind::Policy);
        assert!(!marker.required);
        assert!(
            marker.detail_json.contains("\"complete\":false"),
            "the baseline marks the ledger as incomplete: {}",
            marker.detail_json
        );
    }

    #[test]
    fn storage_claim_can_be_projected_to_launch_condition() {
        let (_dir, db) = temp_db();
        let condition = launch_condition_from_storage_claim("app", Some("rev1"), 21474836480);
        db.record_launch_condition_claim(&condition).unwrap();
        let loaded = db.list_launch_condition_claims("app").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].kind, LaunchConditionKind::Storage);
        assert_eq!(loaded[0].condition_key, "requirements.disk");
        assert!(loaded[0].detail_json.contains("21474836480"));
    }

    #[test]
    fn port_claim_can_be_projected_to_launch_condition() {
        let (_dir, db) = temp_db();
        let port = PortClaim {
            install_profile_key: "app".to_string(),
            logical_endpoint: "ato://app/app/main".to_string(),
            preferred_port: 3000,
            last_actual_port: Some(49152),
            protocol: "tcp".to_string(),
            conflict_policy: ConflictPolicy::Remap,
        };
        let condition = launch_condition_from_port_claim(&port);
        db.record_launch_condition_claim(&condition).unwrap();
        let loaded = db.list_launch_condition_claims("app").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].kind, LaunchConditionKind::Port);
        assert!(loaded[0].detail_json.contains("\"preferred_port\":3000"));
    }

    #[test]
    fn secret_launch_condition_rejects_raw_value() {
        let (_dir, db) = temp_db();
        let mut c = condition(
            "app",
            LaunchConditionKind::Secret,
            "OPENAI_API_KEY",
            LaunchConditionStatus::Satisfied,
        );
        c.detail_json = r#"{"value":"sk-abc123"}"#.to_string();
        assert!(
            db.record_launch_condition_claim(&c).is_err(),
            "a secret condition embedding a raw value must be rejected at write time"
        );
        assert!(db.list_launch_condition_claims("app").unwrap().is_empty());
    }

    #[test]
    fn secret_launch_condition_requires_redacted_detail() {
        let (_dir, db) = temp_db();
        let mut c = condition(
            "app",
            LaunchConditionKind::Secret,
            "OPENAI_API_KEY",
            LaunchConditionStatus::UserGrantRequired,
        );
        c.redacted = false;
        assert!(
            db.record_launch_condition_claim(&c).is_err(),
            "a secret condition must be marked redacted"
        );
    }

    #[test]
    fn env_launch_condition_rejects_api_key_value() {
        let (_dir, db) = temp_db();
        let mut c = condition(
            "app",
            LaunchConditionKind::Env,
            "PORT",
            LaunchConditionStatus::Satisfied,
        );
        c.detail_json = r#"{"api_key":"sk-abc"}"#.to_string();
        assert!(db.record_launch_condition_claim(&c).is_err());
    }

    #[test]
    fn same_profile_kind_condition_provider_is_upserted_not_duplicated() {
        let (_dir, db) = temp_db();
        let c = condition(
            "app",
            LaunchConditionKind::Runtime,
            "deno",
            LaunchConditionStatus::Missing,
        );
        db.record_launch_condition_claim(&c).unwrap();
        db.record_launch_condition_claim(&c).unwrap();
        assert_eq!(
            db.list_launch_condition_claims("app").unwrap().len(),
            1,
            "identical identity must upsert, not duplicate"
        );
    }

    #[test]
    fn different_provider_keeps_separate_claims() {
        let (_dir, db) = temp_db();
        let mut local = condition(
            "app",
            LaunchConditionKind::ProviderCapability,
            "gpu.nvidia.cuda",
            LaunchConditionStatus::ProviderRequired,
        );
        local.provider_id = None; // → 'local'
        let mut remote = local.clone();
        remote.provider_id = Some("runner-east".to_string());
        db.record_launch_condition_claim(&local).unwrap();
        db.record_launch_condition_claim(&remote).unwrap();
        assert_eq!(
            db.list_launch_condition_claims("app").unwrap().len(),
            2,
            "the same condition on a different provider is a distinct claim"
        );
    }

    #[test]
    fn different_revision_keeps_separate_claims() {
        let (_dir, db) = temp_db();
        let mut r1 = condition(
            "app",
            LaunchConditionKind::Storage,
            "requirements.disk",
            LaunchConditionStatus::Satisfied,
        );
        r1.install_revision_id = Some("rev1".to_string());
        let mut r2 = r1.clone();
        r2.install_revision_id = Some("rev2".to_string());
        db.record_launch_condition_claim(&r1).unwrap();
        db.record_launch_condition_claim(&r2).unwrap();
        assert_eq!(
            db.list_launch_condition_claims("app").unwrap().len(),
            2,
            "the same condition on a different revision is a distinct claim"
        );
    }

    #[test]
    fn condition_claim_round_trips_through_db() {
        let (_dir, db) = temp_db();
        let mut c = condition(
            "app",
            LaunchConditionKind::Network,
            "egress.api.openai.com",
            LaunchConditionStatus::Satisfied,
        );
        c.install_revision_id = Some("rev1".to_string());
        c.provider_id = Some("runner-east".to_string());
        c.required = false;
        c.source = LaunchConditionSource::ProviderSnapshot;
        c.detail_json = r#"{"host":"api.openai.com","port":443}"#.to_string();
        db.record_launch_condition_claim(&c).unwrap();
        let loaded = db
            .list_launch_condition_claims_for_revision("app", Some("rev1"), Some("runner-east"))
            .unwrap();
        assert_eq!(loaded, vec![c], "stored condition must round-trip exactly");
    }

    #[test]
    fn load_relaunch_admission_input_reads_revision_claims() {
        let (_dir, db) = temp_db();
        let mut c = condition(
            "app",
            LaunchConditionKind::Secret,
            "OPENAI_API_KEY",
            LaunchConditionStatus::UserGrantRequired,
        );
        c.install_revision_id = Some("rev1".to_string());
        db.record_installed_launch_ledger("app", Some("rev1"), None, &[c])
            .unwrap();
        let input = db
            .load_relaunch_admission_input("app", Some("rev1"), None)
            .unwrap();
        assert_eq!(input.install_profile_key, "app");
        assert_eq!(input.install_revision_id.as_deref(), Some("rev1"));
        assert_eq!(input.claims.len(), 1);
        assert_eq!(input.claims[0].condition_key, "OPENAI_API_KEY");
    }

    #[test]
    fn load_relaunch_admission_input_empty_revision_returns_empty_claims_for_evaluator() {
        let (_dir, db) = temp_db();
        // No conditions recorded for this revision → empty claims (the evaluator,
        // not the DB, decides what "empty" means).
        let input = db
            .load_relaunch_admission_input("ghost", Some("rev9"), None)
            .unwrap();
        assert!(input.claims.is_empty());
        assert_eq!(input.install_profile_key, "ghost");
    }

    #[test]
    fn record_secret_grant_ref_accepts_secret_condition_key() {
        let (_dir, db) = temp_db();
        assert!(!db.secret_grant_ref_exists("openai-default").unwrap());
        db.record_secret_grant_ref(
            "app",
            Some("ato.run/koh0920/hello"),
            "secret.OPENAI_API_KEY",
            "openai-default",
        )
        .unwrap();
        assert!(db.secret_grant_ref_exists("openai-default").unwrap());
        assert!(!db.secret_grant_ref_exists("unknown-grant").unwrap());
    }

    #[test]
    fn secret_grant_ref_upserts_same_id() {
        let (_dir, db) = temp_db();
        db.record_secret_grant_ref("app", None, "secret.K", "g1")
            .unwrap();
        db.record_secret_grant_ref("app", None, "secret.K", "g1")
            .unwrap();
        // Upsert, not duplicate; still exists.
        assert!(db.secret_grant_ref_exists("g1").unwrap());
    }

    #[test]
    fn record_state_binding_ref_accepts_state_condition_key() {
        let (_dir, db) = temp_db();
        assert!(!db.state_binding_ref_exists("user-data").unwrap());
        db.record_state_binding_ref(
            "app",
            Some("ato.run/koh0920/hello"),
            "state.data",
            "data",
            "user-data",
        )
        .unwrap();
        assert!(db.state_binding_ref_exists("user-data").unwrap());
        assert!(!db.state_binding_ref_exists("other").unwrap());
    }

    // The registry validates at its own boundary — callers are not trusted. The
    // condition is identified by the reserved condition key, never a URI fragment.

    #[test]
    fn record_secret_grant_ref_rejects_condition_fragment_ref() {
        let (_dir, db) = temp_db();
        // The retired `capsule://…#condition/…` fragment form is not a condition
        // key and must be rejected; so must any foreign-scheme URI.
        assert!(
            db.record_secret_grant_ref(
                "app",
                None,
                "capsule://x#condition/secret/OPENAI_API_KEY",
                "g1"
            )
            .is_err(),
            "a #condition fragment ref is not a valid condition key"
        );
        assert!(
            db.record_secret_grant_ref("app", None, "ato-secret://store/openai", "g1")
                .is_err()
        );
    }

    #[test]
    fn record_secret_grant_ref_rejects_raw_token_id() {
        let (_dir, db) = temp_db();
        assert!(
            db.record_secret_grant_ref("app", None, "secret.K", "sk-abc123def456")
                .is_err(),
            "a raw token-like grant id must be rejected at the DB boundary"
        );
    }

    #[test]
    fn record_state_binding_ref_rejects_condition_fragment_ref() {
        let (_dir, db) = temp_db();
        assert!(
            db.record_state_binding_ref("app", None, "ato-state://app/data", "data", "b1")
                .is_err()
        );
    }

    #[test]
    fn record_state_binding_ref_rejects_raw_host_path_id() {
        let (_dir, db) = temp_db();
        assert!(
            db.record_state_binding_ref("app", None, "state.data", "data", "/Users/koh/data")
                .is_err(),
            "a raw host path binding id must be rejected at the DB boundary"
        );
    }

    #[test]
    fn secret_grant_ref_exists_returns_false_for_invalid_id() {
        let (_dir, db) = temp_db();
        // Invalid ids resolve to false (never error into the launch path).
        assert!(!db.secret_grant_ref_exists("sk-abc123def456").unwrap());
        assert!(!db.secret_grant_ref_exists("ato-secret://x").unwrap());
    }

    #[test]
    fn state_binding_ref_exists_returns_false_for_invalid_id() {
        let (_dir, db) = temp_db();
        assert!(!db.state_binding_ref_exists("/home/koh/data").unwrap());
        assert!(!db.state_binding_ref_exists("file:///x").unwrap());
    }
}
