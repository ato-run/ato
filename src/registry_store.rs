use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ed25519_dalek::{Signer, SigningKey};
use rand::rngs::OsRng;
use rand::RngCore;
use rusqlite::types::Type;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use capsule_core::packers::payload as manifest_payload;
use capsule_core::types::identity::public_key_to_did;
use capsule_core::types::{CapsuleManifest, EpochPointer};

const DB_FILE_NAME: &str = "registry.sqlite3";
const SIGNING_KEY_FILE: &str = "signing-key.bin";
const KEY_DIR: &str = "keys";
const ACTIVE_KEY_FILE: &str = "active";
const DEFAULT_LEASE_TTL_SECS: u64 = 900;
const DEFAULT_GC_DEFER_SECS: i64 = 30;
const RETENTION_PINNED_RELEASES: i64 = 5;
// 128-bit random ids already make collisions vanishingly unlikely, but a small retry
// budget keeps the insert path fail-closed if SQLite reports a uniqueness race.
const MAX_STATE_ID_GENERATION_ATTEMPTS: usize = 10;
const MAX_BINDING_ID_GENERATION_ATTEMPTS: usize = 10;
const SCHEMA_MIGRATION_0001: &str = "2026-03-05-0001-manifests-tombstoned";
const SCHEMA_MIGRATION_0002: &str = "2026-03-05-0002-leases-composite";
const SCHEMA_MIGRATION_0003: &str = "2026-03-05-0003-gc-indexes";
const SCHEMA_MIGRATION_0004: &str = "2026-03-05-0004-auto-vacuum-incremental";
const SCHEMA_MIGRATION_0005: &str = "2026-03-05-0005-manifests-yanked";
const SCHEMA_MIGRATION_0006: &str = "2026-03-10-0006-persistent-state-registry";
const SCHEMA_MIGRATION_0007: &str = "2026-03-10-0007-persistent-state-kind-columns";
const SCHEMA_MIGRATION_0008: &str = "2026-03-10-0008-service-binding-registry";
const SCHEMA_MIGRATION_0009: &str = "2026-03-10-0009-service-binding-allowed-callers";

fn manifest_distribution(
    manifest: &CapsuleManifest,
) -> Result<&capsule_core::types::DistributionInfo> {
    manifest.distribution.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "{}: distribution metadata is missing from capsule.toml",
            crate::error_codes::ATO_ERR_INTEGRITY_FAILURE
        )
    })
}

fn generate_random_state_id() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    format!("state-{}", hex::encode(bytes))
}

fn generate_random_binding_id() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    format!("binding-{}", hex::encode(bytes))
}

fn normalize_allowed_callers(allowed_callers: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    for value in allowed_callers {
        let trimmed = value.trim();
        if trimmed.is_empty() || normalized.iter().any(|existing| existing == trimmed) {
            continue;
        }
        normalized.push(trimmed.to_string());
    }
    normalized
}

fn encode_allowed_callers(allowed_callers: &[String]) -> Result<String> {
    Ok(serde_json::to_string(&normalize_allowed_callers(
        allowed_callers,
    ))?)
}

fn decode_allowed_callers(raw: Option<String>) -> rusqlite::Result<Vec<String>> {
    let Some(raw) = raw
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(Vec::new());
    };

    serde_json::from_str::<Vec<String>>(raw)
        .map(|values| normalize_allowed_callers(&values))
        .map_err(|err| rusqlite::Error::FromSqlConversionFailure(8, Type::Text, Box::new(err)))
}

#[derive(Debug, Clone)]
pub struct RegistryStore {
    data_dir: PathBuf,
    db_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryReleaseRecord {
    pub version: String,
    pub manifest_hash: String,
    pub file_name: String,
    pub sha256: String,
    pub blake3: String,
    pub size_bytes: u64,
    pub signature_status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryVersionResolveRecord {
    pub scoped_id: String,
    pub version: String,
    pub manifest_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yanked_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryPackageRecord {
    pub scoped_id: String,
    pub publisher: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub latest_version: String,
    pub created_at: String,
    pub updated_at: String,
    pub releases: Vec<RegistryReleaseRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryStoreMetadataRecord {
    pub scoped_id: String,
    pub icon_path: Option<String>,
    pub text: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistentStateRecord {
    pub state_id: String,
    pub owner_scope: String,
    pub state_name: String,
    pub kind: String,
    pub backend_kind: String,
    pub backend_locator: String,
    pub producer: String,
    pub purpose: String,
    pub schema_id: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPersistentStateRecord {
    pub owner_scope: String,
    pub state_name: String,
    pub kind: String,
    pub backend_kind: String,
    pub backend_locator: String,
    pub producer: String,
    pub purpose: String,
    pub schema_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceBindingRecord {
    pub binding_id: String,
    pub owner_scope: String,
    pub service_name: String,
    pub binding_kind: String,
    pub transport_kind: String,
    pub adapter_kind: String,
    pub endpoint_locator: String,
    pub tls_mode: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_callers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_hint: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewServiceBindingRecord {
    pub owner_scope: String,
    pub service_name: String,
    pub binding_kind: String,
    pub transport_kind: String,
    pub adapter_kind: String,
    pub endpoint_locator: String,
    pub tls_mode: String,
    pub allowed_callers: Vec<String>,
    pub target_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryDeleteResult {
    pub removed_capsule: bool,
    pub removed_version: Option<String>,
    pub removed_releases: Vec<RegistryReleaseRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryDeleteOutcome {
    CapsuleNotFound,
    VersionNotFound(String),
    Deleted(RegistryDeleteResult),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NegotiateRequest {
    pub scoped_id: String,
    pub target_manifest_hash: String,
    #[serde(default)]
    pub have_chunks: Vec<String>,
    #[serde(default)]
    pub have_chunks_bloom: Option<ChunkBloomFilterRequest>,
    #[serde(default)]
    pub reuse_lease_id: Option<String>,
    #[serde(default)]
    pub max_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkBloomFilterRequest {
    pub m_bits: u64,
    pub k_hashes: u32,
    pub seed: u64,
    pub bitset_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NegotiateResponse {
    pub session_id: String,
    pub required_chunks: Vec<String>,
    pub required_manifests: Vec<String>,
    #[serde(default)]
    pub yanked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub epoch_pointer: Option<EpochPointer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpochResolveRequest {
    pub scoped_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackRequest {
    pub scoped_id: String,
    pub target_manifest_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YankRequest {
    pub scoped_id: String,
    pub target_manifest_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseRefreshRequest {
    pub lease_id: String,
    #[serde(default)]
    pub ttl_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseReleaseRequest {
    pub lease_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpochResolveResponse {
    pub pointer: EpochPointer,
    pub public_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyRotateRequest {
    #[serde(default)]
    pub overlap_hours: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyRevokeRequest {
    pub key_id: String,
    #[serde(default)]
    pub did: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyRotateResponse {
    pub signer_did: String,
    pub key_id: String,
    pub public_key: String,
    pub valid_from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_key_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_valid_to: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyRevokeResponse {
    pub revoked: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_key_rotated_to: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseAcquireResult {
    pub lease_id: String,
    pub expires_at: String,
    pub chunk_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseRefreshResult {
    pub lease_id: String,
    pub expires_at: String,
    pub chunk_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GcTickResult {
    pub expired_leases: usize,
    pub processed: usize,
    pub deleted: usize,
    pub deferred: usize,
    pub failed: usize,
}

#[derive(Debug, Clone)]
struct ParsedBloomFilter {
    m_bits: usize,
    k_hashes: u32,
    seed: u64,
    bitset: Vec<u8>,
}

impl ParsedBloomFilter {
    fn from_request(request: &ChunkBloomFilterRequest) -> Result<Self> {
        let bitset = BASE64
            .decode(request.bitset_base64.as_bytes())
            .context("invalid have_chunks_bloom.bitset_base64")?;
        if request.m_bits == 0 {
            anyhow::bail!("have_chunks_bloom.m_bits must be greater than zero");
        }
        if request.k_hashes == 0 {
            anyhow::bail!("have_chunks_bloom.k_hashes must be greater than zero");
        }
        let available_bits = bitset.len().saturating_mul(8);
        if available_bits == 0 {
            anyhow::bail!("have_chunks_bloom bitset is empty");
        }
        let m_bits = request.m_bits.min(available_bits as u64) as usize;
        Ok(Self {
            m_bits,
            k_hashes: request.k_hashes,
            seed: request.seed,
            bitset,
        })
    }

    fn might_contain(&self, value: &str) -> bool {
        (0..self.k_hashes).all(|round| {
            let bit_index = self.bit_index(value, round);
            let byte_index = bit_index / 8;
            let bit_mask = 1u8 << (bit_index % 8);
            self.bitset
                .get(byte_index)
                .map(|byte| (byte & bit_mask) != 0)
                .unwrap_or(false)
        })
    }

    fn bit_index(&self, value: &str, round: u32) -> usize {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&self.seed.to_le_bytes());
        hasher.update(&round.to_le_bytes());
        hasher.update(value.as_bytes());
        let digest = hasher.finalize();
        let mut raw = [0u8; 8];
        raw.copy_from_slice(&digest.as_bytes()[..8]);
        (u64::from_le_bytes(raw) % self.m_bits as u64) as usize
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SigningIdentity {
    signer_did: String,
    key_id: String,
    public_key: String,
}

impl RegistryStore {
    pub fn open(data_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(data_dir)
            .with_context(|| format!("failed to create {}", data_dir.display()))?;
        let db_path = data_dir.join(DB_FILE_NAME);
        let this = Self {
            data_dir: data_dir.to_path_buf(),
            db_path,
        };
        this.init_schema()?;
        Ok(this)
    }

    pub fn list_registry_packages(&self) -> Result<Vec<RegistryPackageRecord>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT scoped_id, publisher, slug, name, description, latest_version, created_at, updated_at
             FROM registry_packages
             ORDER BY updated_at DESC, scoped_id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(RegistryPackageRecord {
                scoped_id: row.get(0)?,
                publisher: row.get(1)?,
                slug: row.get(2)?,
                name: row.get(3)?,
                description: row.get(4)?,
                latest_version: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
                releases: Vec::new(),
            })
        })?;

        let mut packages = Vec::new();
        for row in rows {
            let mut package = row?;
            package.releases = self.load_registry_releases(&conn, &package.scoped_id)?;
            packages.push(package);
        }
        Ok(packages)
    }

    pub fn find_persistent_state_by_owner_and_locator(
        &self,
        owner_scope: &str,
        backend_locator: &str,
    ) -> Result<Option<PersistentStateRecord>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT state_id, owner_scope, state_name, kind, backend_kind, backend_locator, producer, purpose, schema_id, created_at, updated_at
             FROM persistent_states
             WHERE owner_scope=?1 AND backend_locator=?2",
            params![owner_scope, backend_locator],
            Self::map_persistent_state_row,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn find_persistent_state_by_id(
        &self,
        state_id: &str,
    ) -> Result<Option<PersistentStateRecord>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT state_id, owner_scope, state_name, kind, backend_kind, backend_locator, producer, purpose, schema_id, created_at, updated_at
             FROM persistent_states
             WHERE state_id=?1",
            params![state_id],
            Self::map_persistent_state_row,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_persistent_states(
        &self,
        owner_scope: Option<&str>,
        state_name: Option<&str>,
    ) -> Result<Vec<PersistentStateRecord>> {
        let conn = self.connect()?;
        let sql = match (owner_scope, state_name) {
            (Some(_), Some(_)) => {
                "SELECT state_id, owner_scope, state_name, kind, backend_kind, backend_locator, producer, purpose, schema_id, created_at, updated_at
                 FROM persistent_states
                 WHERE owner_scope=?1 AND state_name=?2
                 ORDER BY updated_at DESC, state_id ASC"
            }
            (Some(_), None) => {
                "SELECT state_id, owner_scope, state_name, kind, backend_kind, backend_locator, producer, purpose, schema_id, created_at, updated_at
                 FROM persistent_states
                 WHERE owner_scope=?1
                 ORDER BY updated_at DESC, state_id ASC"
            }
            (None, Some(_)) => {
                "SELECT state_id, owner_scope, state_name, kind, backend_kind, backend_locator, producer, purpose, schema_id, created_at, updated_at
                 FROM persistent_states
                 WHERE state_name=?1
                 ORDER BY updated_at DESC, state_id ASC"
            }
            (None, None) => {
                "SELECT state_id, owner_scope, state_name, kind, backend_kind, backend_locator, producer, purpose, schema_id, created_at, updated_at
                 FROM persistent_states
                 ORDER BY updated_at DESC, state_id ASC"
            }
        };

        let mut stmt = conn.prepare(sql)?;
        let rows = match (owner_scope, state_name) {
            (Some(owner_scope), Some(state_name)) => stmt.query_map(
                params![owner_scope, state_name],
                Self::map_persistent_state_row,
            )?,
            (Some(owner_scope), None) => {
                stmt.query_map(params![owner_scope], Self::map_persistent_state_row)?
            }
            (None, Some(state_name)) => {
                stmt.query_map(params![state_name], Self::map_persistent_state_row)?
            }
            (None, None) => stmt.query_map([], Self::map_persistent_state_row)?,
        };

        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn register_persistent_state(
        &self,
        record: &NewPersistentStateRecord,
    ) -> Result<PersistentStateRecord> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.connect()?;
        for _ in 0..MAX_STATE_ID_GENERATION_ATTEMPTS {
            let state_id = generate_random_state_id();
            match conn.execute(
                "INSERT INTO persistent_states(
                    state_id, owner_scope, state_name, kind, backend_kind, backend_locator, producer, purpose, schema_id, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
                params![
                    state_id,
                    record.owner_scope,
                    record.state_name,
                    record.kind,
                    record.backend_kind,
                    record.backend_locator,
                    record.producer,
                    record.purpose,
                    record.schema_id,
                    now,
                ],
            ) {
                Ok(_) => {
                    return self
                        .find_persistent_state_by_owner_and_locator(
                            &record.owner_scope,
                            &record.backend_locator,
                        )?
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "failed to retrieve persistent state after registration - database inconsistency detected"
                            )
                        });
                }
                Err(rusqlite::Error::SqliteFailure(err, _))
                    if err.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    if let Some(existing) = self.find_persistent_state_by_owner_and_locator(
                        &record.owner_scope,
                        &record.backend_locator,
                    )? {
                        return Ok(existing);
                    }
                    continue;
                }
                Err(err) => return Err(err.into()),
            }
        }

        anyhow::bail!(
            "failed to allocate a unique persistent state id after {} attempts",
            MAX_STATE_ID_GENERATION_ATTEMPTS
        );
    }

    pub fn find_service_binding_by_identity(
        &self,
        owner_scope: &str,
        service_name: &str,
        binding_kind: &str,
    ) -> Result<Option<ServiceBindingRecord>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT binding_id, owner_scope, service_name, binding_kind, transport_kind, adapter_kind, endpoint_locator, tls_mode, allowed_callers_json, target_hint, created_at, updated_at
             FROM service_bindings
             WHERE owner_scope=?1 AND service_name=?2 AND binding_kind=?3",
            params![owner_scope, service_name, binding_kind],
            Self::map_service_binding_row,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn find_service_binding_by_id(
        &self,
        binding_id: &str,
    ) -> Result<Option<ServiceBindingRecord>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT binding_id, owner_scope, service_name, binding_kind, transport_kind, adapter_kind, endpoint_locator, tls_mode, allowed_callers_json, target_hint, created_at, updated_at
             FROM service_bindings
             WHERE binding_id=?1",
            params![binding_id],
            Self::map_service_binding_row,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn resolve_service_binding(
        &self,
        owner_scope: &str,
        service_name: &str,
        binding_kind: &str,
        caller_service: Option<&str>,
    ) -> Result<Option<ServiceBindingRecord>> {
        let Some(record) =
            self.find_service_binding_by_identity(owner_scope, service_name, binding_kind)?
        else {
            return Ok(None);
        };

        if record.allowed_callers.is_empty() {
            return Ok(Some(record));
        }

        let caller_service = caller_service
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "service binding '{}' requires caller_service because access is restricted to {:?}",
                    record.binding_id,
                    record.allowed_callers
                )
            })?;

        if record
            .allowed_callers
            .iter()
            .any(|value| value == caller_service)
        {
            return Ok(Some(record));
        }

        anyhow::bail!(
            "service '{}' is not allowed to use binding '{}' (allowed callers: {:?})",
            caller_service,
            record.binding_id,
            record.allowed_callers
        );
    }

    pub fn list_service_bindings(
        &self,
        owner_scope: Option<&str>,
        service_name: Option<&str>,
    ) -> Result<Vec<ServiceBindingRecord>> {
        let conn = self.connect()?;
        let sql = match (owner_scope, service_name) {
            (Some(_), Some(_)) => {
                "SELECT binding_id, owner_scope, service_name, binding_kind, transport_kind, adapter_kind, endpoint_locator, tls_mode, allowed_callers_json, target_hint, created_at, updated_at
                 FROM service_bindings
                 WHERE owner_scope=?1 AND service_name=?2
                 ORDER BY updated_at DESC, binding_id ASC"
            }
            (Some(_), None) => {
                "SELECT binding_id, owner_scope, service_name, binding_kind, transport_kind, adapter_kind, endpoint_locator, tls_mode, allowed_callers_json, target_hint, created_at, updated_at
                 FROM service_bindings
                 WHERE owner_scope=?1
                 ORDER BY updated_at DESC, binding_id ASC"
            }
            (None, Some(_)) => {
                "SELECT binding_id, owner_scope, service_name, binding_kind, transport_kind, adapter_kind, endpoint_locator, tls_mode, allowed_callers_json, target_hint, created_at, updated_at
                 FROM service_bindings
                 WHERE service_name=?1
                 ORDER BY updated_at DESC, binding_id ASC"
            }
            (None, None) => {
                "SELECT binding_id, owner_scope, service_name, binding_kind, transport_kind, adapter_kind, endpoint_locator, tls_mode, allowed_callers_json, target_hint, created_at, updated_at
                 FROM service_bindings
                 ORDER BY updated_at DESC, binding_id ASC"
            }
        };

        let mut stmt = conn.prepare(sql)?;
        let rows = match (owner_scope, service_name) {
            (Some(owner_scope), Some(service_name)) => stmt.query_map(
                params![owner_scope, service_name],
                Self::map_service_binding_row,
            )?,
            (Some(owner_scope), None) => {
                stmt.query_map(params![owner_scope], Self::map_service_binding_row)?
            }
            (None, Some(service_name)) => {
                stmt.query_map(params![service_name], Self::map_service_binding_row)?
            }
            (None, None) => stmt.query_map([], Self::map_service_binding_row)?,
        };

        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }
        Ok(records)
    }

    pub fn register_service_binding(
        &self,
        record: &NewServiceBindingRecord,
    ) -> Result<ServiceBindingRecord> {
        let now = chrono::Utc::now().to_rfc3339();
        let conn = self.connect()?;
        let allowed_callers_json = encode_allowed_callers(&record.allowed_callers)?;

        if let Some(existing) = self.find_service_binding_by_identity(
            &record.owner_scope,
            &record.service_name,
            &record.binding_kind,
        )? {
            conn.execute(
                "UPDATE service_bindings
                 SET transport_kind=?1,
                     adapter_kind=?2,
                     endpoint_locator=?3,
                     tls_mode=?4,
                     allowed_callers_json=?5,
                     target_hint=?6,
                     updated_at=?7
                 WHERE binding_id=?8",
                params![
                    record.transport_kind,
                    record.adapter_kind,
                    record.endpoint_locator,
                    record.tls_mode,
                    &allowed_callers_json,
                    record.target_hint,
                    now,
                    existing.binding_id,
                ],
            )?;
            return self
                .find_service_binding_by_id(&existing.binding_id)?
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "failed to retrieve service binding after update - database inconsistency detected"
                    )
                });
        }

        for _ in 0..MAX_BINDING_ID_GENERATION_ATTEMPTS {
            let binding_id = generate_random_binding_id();
            match conn.execute(
                "INSERT INTO service_bindings(
                    binding_id, owner_scope, service_name, binding_kind, transport_kind, adapter_kind, endpoint_locator, tls_mode, allowed_callers_json, target_hint, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
                params![
                    binding_id,
                    record.owner_scope,
                    record.service_name,
                    record.binding_kind,
                    record.transport_kind,
                    record.adapter_kind,
                    record.endpoint_locator,
                    record.tls_mode,
                    &allowed_callers_json,
                    record.target_hint,
                    now,
                ],
            ) {
                Ok(_) => {
                    return self
                        .find_service_binding_by_id(&binding_id)?
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "failed to retrieve service binding after registration - database inconsistency detected"
                            )
                        });
                }
                Err(rusqlite::Error::SqliteFailure(err, _))
                    if err.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    if let Some(existing) = self.find_service_binding_by_identity(
                        &record.owner_scope,
                        &record.service_name,
                        &record.binding_kind,
                    )? {
                        return Ok(existing);
                    }
                    continue;
                }
                Err(err) => return Err(err.into()),
            }
        }

        anyhow::bail!(
            "failed to allocate a unique service binding id after {} attempts",
            MAX_BINDING_ID_GENERATION_ATTEMPTS
        );
    }

    pub fn delete_service_binding_by_identity(
        &self,
        owner_scope: &str,
        service_name: &str,
        binding_kind: &str,
    ) -> Result<Option<ServiceBindingRecord>> {
        let Some(existing) =
            self.find_service_binding_by_identity(owner_scope, service_name, binding_kind)?
        else {
            return Ok(None);
        };

        let conn = self.connect()?;
        conn.execute(
            "DELETE FROM service_bindings WHERE binding_id=?1",
            params![existing.binding_id],
        )?;
        Ok(Some(existing))
    }

    fn map_persistent_state_row(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<PersistentStateRecord> {
        Ok(PersistentStateRecord {
            state_id: row.get(0)?,
            owner_scope: row.get(1)?,
            state_name: row.get(2)?,
            kind: row.get(3)?,
            backend_kind: row.get(4)?,
            backend_locator: row.get(5)?,
            producer: row.get(6)?,
            purpose: row.get(7)?,
            schema_id: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
        })
    }

    fn map_service_binding_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ServiceBindingRecord> {
        Ok(ServiceBindingRecord {
            binding_id: row.get(0)?,
            owner_scope: row.get(1)?,
            service_name: row.get(2)?,
            binding_kind: row.get(3)?,
            transport_kind: row.get(4)?,
            adapter_kind: row.get(5)?,
            endpoint_locator: row.get(6)?,
            tls_mode: row.get(7)?,
            allowed_callers: decode_allowed_callers(row.get(8)?)?,
            target_hint: row.get(9)?,
            created_at: row.get(10)?,
            updated_at: row.get(11)?,
        })
    }

    pub fn find_registry_release(
        &self,
        publisher: &str,
        slug: &str,
        version: &str,
    ) -> Result<Option<RegistryReleaseRecord>> {
        let scoped_id = format!("{}/{}", publisher, slug);
        let conn = self.connect()?;
        conn.query_row(
            "SELECT version, manifest_hash, file_name, sha256, blake3, size_bytes, signature_status, created_at
             FROM registry_releases
             WHERE scoped_id=?1 AND version=?2",
            params![scoped_id, version],
            |row| self.map_registry_release_row(row),
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn resolve_release_version(
        &self,
        publisher: &str,
        slug: &str,
        version: &str,
    ) -> Result<Option<RegistryVersionResolveRecord>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT p.scoped_id, r.version, r.manifest_hash, m.yanked_at
             FROM registry_packages p
             JOIN registry_releases r ON r.scoped_id = p.scoped_id
             JOIN manifests m ON m.manifest_hash = r.manifest_hash
             WHERE p.publisher=?1 AND p.slug=?2 AND r.version=?3",
            params![publisher, slug, version],
            |row| {
                Ok(RegistryVersionResolveRecord {
                    scoped_id: row.get(0)?,
                    version: row.get(1)?,
                    manifest_hash: row.get(2)?,
                    yanked_at: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_store_metadata_entries(&self) -> Result<Vec<RegistryStoreMetadataRecord>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT scoped_id, icon_path, text, updated_at
             FROM registry_store_metadata
             ORDER BY scoped_id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(RegistryStoreMetadataRecord {
                scoped_id: row.get(0)?,
                icon_path: row.get(1)?,
                text: row.get(2)?,
                updated_at: row.get(3)?,
            })
        })?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        Ok(entries)
    }

    pub fn load_store_metadata_entry(
        &self,
        scoped_id: &str,
    ) -> Result<Option<RegistryStoreMetadataRecord>> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT scoped_id, icon_path, text, updated_at
             FROM registry_store_metadata
             WHERE scoped_id=?1",
            params![scoped_id],
            |row| {
                Ok(RegistryStoreMetadataRecord {
                    scoped_id: row.get(0)?,
                    icon_path: row.get(1)?,
                    text: row.get(2)?,
                    updated_at: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn upsert_store_metadata(
        &self,
        scoped_id: &str,
        icon_path: Option<&str>,
        text: Option<&str>,
        updated_at: &str,
    ) -> Result<()> {
        let conn = self.connect()?;
        if icon_path.is_none() && text.is_none() {
            conn.execute(
                "DELETE FROM registry_store_metadata WHERE scoped_id=?1",
                params![scoped_id],
            )?;
            return Ok(());
        }
        conn.execute(
            "INSERT INTO registry_store_metadata(scoped_id, icon_path, text, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(scoped_id) DO UPDATE SET
               icon_path=excluded.icon_path,
               text=excluded.text,
               updated_at=excluded.updated_at",
            params![scoped_id, icon_path, text, updated_at],
        )?;
        Ok(())
    }

    pub fn delete_store_metadata(&self, scoped_id: &str) -> Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "DELETE FROM registry_store_metadata WHERE scoped_id=?1",
            params![scoped_id],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn publish_registry_release(
        &self,
        publisher: &str,
        slug: &str,
        name: &str,
        description: &str,
        version: &str,
        file_name: &str,
        sha256: &str,
        blake3: &str,
        size_bytes: u64,
        capsule_bytes: &[u8],
        issued_at: &str,
    ) -> Result<EpochResolveResponse> {
        let scoped_id = format!("{}/{}", publisher, slug);
        let extracted = extract_manifest_and_payload_from_capsule(capsule_bytes)?;
        let identity = self.ensure_signing_identity()?;
        let (_, signing_key) = self.load_or_create_signing_key()?;

        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let manifest_hash = self.record_verified_manifest_and_epoch_tx(
            &tx,
            &scoped_id,
            &extracted.manifest,
            &extracted.manifest_document,
            &extracted.payload_tar_bytes,
            issued_at,
            &identity,
            &signing_key,
        )?;
        self.upsert_registry_release_tx(
            &tx,
            &scoped_id,
            publisher,
            slug,
            name,
            description,
            version,
            &manifest_hash,
            file_name,
            sha256,
            blake3,
            size_bytes,
            issued_at,
        )?;
        tx.commit()?;

        self.resolve_epoch_pointer(&scoped_id)?
            .context("failed to resolve epoch pointer after insert")
    }

    pub fn delete_registry_capsule(
        &self,
        publisher: &str,
        slug: &str,
        version: Option<&str>,
        now: &str,
    ) -> Result<RegistryDeleteOutcome> {
        let scoped_id = format!("{}/{}", publisher, slug);
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;

        let package_exists: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM registry_packages WHERE scoped_id=?1",
                params![scoped_id],
                |row| row.get(0),
            )
            .optional()?;
        if package_exists.is_none() {
            tx.rollback()?;
            return Ok(RegistryDeleteOutcome::CapsuleNotFound);
        }

        let releases = self.load_registry_releases_tx(&tx, &scoped_id)?;
        let removed_releases = if let Some(version) = version {
            let Some(release) = releases.iter().find(|release| release.version == version) else {
                tx.rollback()?;
                return Ok(RegistryDeleteOutcome::VersionNotFound(version.to_string()));
            };
            tx.execute(
                "DELETE FROM registry_releases WHERE scoped_id=?1 AND version=?2",
                params![scoped_id, version],
            )?;
            vec![release.clone()]
        } else {
            tx.execute(
                "DELETE FROM registry_releases WHERE scoped_id=?1",
                params![scoped_id],
            )?;
            releases
        };

        let mut unique_manifest_hashes = HashSet::new();
        for release in &removed_releases {
            if !unique_manifest_hashes.insert(release.manifest_hash.clone()) {
                continue;
            }
            let remaining_reference: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM registry_releases WHERE manifest_hash=?1 LIMIT 1",
                    params![release.manifest_hash.clone()],
                    |row| row.get(0),
                )
                .optional()?;
            if remaining_reference.is_some() {
                continue;
            }
            if self.tombstone_manifest_tx(&tx, &scoped_id, &release.manifest_hash, now)? {
                self.enqueue_manifest_chunks_for_gc_tx(
                    &tx,
                    &release.manifest_hash,
                    "capsule_delete",
                    now,
                    now,
                )?;
            }
        }

        let remaining_releases = self.load_registry_releases_tx(&tx, &scoped_id)?;
        let removed_capsule = remaining_releases.is_empty();
        if removed_capsule {
            tx.execute(
                "DELETE FROM registry_packages WHERE scoped_id=?1",
                params![scoped_id],
            )?;
        } else {
            let latest_version = latest_version_from_releases(&remaining_releases)
                .context("remaining releases must include latest version")?;
            tx.execute(
                "UPDATE registry_packages
                 SET latest_version=?2, updated_at=?3
                 WHERE scoped_id=?1",
                params![scoped_id, latest_version, now],
            )?;
        }

        tx.commit()?;
        Ok(RegistryDeleteOutcome::Deleted(RegistryDeleteResult {
            removed_capsule,
            removed_version: version.map(ToString::to_string),
            removed_releases,
        }))
    }

    #[cfg(test)]
    pub fn record_manifest_and_epoch(
        &self,
        scoped_id: &str,
        _manifest_toml: &str,
        payload_bytes: &[u8],
        issued_at: &str,
    ) -> Result<EpochResolveResponse> {
        let (manifest, manifest_document) = build_manifest_from_payload(payload_bytes)?;
        self.record_verified_manifest_and_epoch(
            scoped_id,
            &manifest,
            &manifest_document,
            payload_bytes,
            issued_at,
        )
    }

    #[cfg(test)]
    fn record_verified_manifest_and_epoch(
        &self,
        scoped_id: &str,
        manifest: &CapsuleManifest,
        manifest_document: &[u8],
        payload_tar_bytes: &[u8],
        issued_at: &str,
    ) -> Result<EpochResolveResponse> {
        let identity = self.ensure_signing_identity()?;
        let (_, signing_key) = self.load_or_create_signing_key()?;

        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        self.record_verified_manifest_and_epoch_tx(
            &tx,
            scoped_id,
            manifest,
            manifest_document,
            payload_tar_bytes,
            issued_at,
            &identity,
            &signing_key,
        )?;
        tx.commit()?;

        self.resolve_epoch_pointer(scoped_id)?
            .context("failed to resolve epoch pointer after insert")
    }

    fn map_registry_release_row(
        &self,
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<RegistryReleaseRecord> {
        Ok(RegistryReleaseRecord {
            version: row.get(0)?,
            manifest_hash: row.get(1)?,
            file_name: row.get(2)?,
            sha256: row.get(3)?,
            blake3: row.get(4)?,
            size_bytes: row.get::<_, i64>(5)? as u64,
            signature_status: row.get(6)?,
            created_at: row.get(7)?,
        })
    }

    fn load_registry_releases(
        &self,
        conn: &Connection,
        scoped_id: &str,
    ) -> Result<Vec<RegistryReleaseRecord>> {
        let mut stmt = conn.prepare(
            "SELECT version, manifest_hash, file_name, sha256, blake3, size_bytes, signature_status, created_at
             FROM registry_releases
             WHERE scoped_id=?1",
        )?;
        let rows = stmt.query_map(params![scoped_id], |row| self.map_registry_release_row(row))?;
        let mut releases = Vec::new();
        for row in rows {
            releases.push(row?);
        }
        releases.sort_by(|left, right| compare_versions(&right.version, &left.version));
        Ok(releases)
    }

    fn load_registry_releases_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        scoped_id: &str,
    ) -> Result<Vec<RegistryReleaseRecord>> {
        let mut stmt = tx.prepare(
            "SELECT version, manifest_hash, file_name, sha256, blake3, size_bytes, signature_status, created_at
             FROM registry_releases
             WHERE scoped_id=?1",
        )?;
        let rows = stmt.query_map(params![scoped_id], |row| self.map_registry_release_row(row))?;
        let mut releases = Vec::new();
        for row in rows {
            releases.push(row?);
        }
        releases.sort_by(|left, right| compare_versions(&right.version, &left.version));
        Ok(releases)
    }

    #[allow(clippy::too_many_arguments)]
    fn upsert_registry_release_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        scoped_id: &str,
        publisher: &str,
        slug: &str,
        name: &str,
        description: &str,
        version: &str,
        manifest_hash: &str,
        file_name: &str,
        sha256: &str,
        blake3: &str,
        size_bytes: u64,
        now: &str,
    ) -> Result<()> {
        let existing_release: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM registry_releases WHERE scoped_id=?1 AND version=?2",
                params![scoped_id, version],
                |row| row.get(0),
            )
            .optional()?;
        if existing_release.is_some() {
            anyhow::bail!("same version is already published");
        }

        let package_row: Option<(String, String)> = tx
            .query_row(
                "SELECT created_at, latest_version FROM registry_packages WHERE scoped_id=?1",
                params![scoped_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let created_at = package_row
            .as_ref()
            .map(|(created_at, _)| created_at.clone())
            .unwrap_or_else(|| now.to_string());
        let latest_version = package_row
            .as_ref()
            .map(|(_, latest_version)| {
                choose_latest_version(Some(latest_version.as_str()), version)
            })
            .unwrap_or_else(|| version.to_string());

        tx.execute(
            "INSERT INTO registry_packages(
                scoped_id, publisher, slug, name, description, latest_version, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(scoped_id) DO UPDATE SET
               publisher=excluded.publisher,
               slug=excluded.slug,
               name=excluded.name,
               description=excluded.description,
               latest_version=excluded.latest_version,
               updated_at=excluded.updated_at",
            params![
                scoped_id,
                publisher,
                slug,
                name,
                description,
                latest_version,
                created_at,
                now
            ],
        )?;
        tx.execute(
            "INSERT INTO registry_releases(
                scoped_id, version, manifest_hash, file_name, sha256, blake3, size_bytes, signature_status, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'verified', ?8)",
            params![
                scoped_id,
                version,
                manifest_hash,
                file_name,
                normalize_hash(sha256),
                normalize_hash(blake3),
                size_bytes as i64,
                now
            ],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn record_verified_manifest_and_epoch_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        scoped_id: &str,
        manifest: &CapsuleManifest,
        manifest_document: &[u8],
        payload_tar_bytes: &[u8],
        issued_at: &str,
        identity: &SigningIdentity,
        signing_key: &SigningKey,
    ) -> Result<String> {
        let distribution = manifest_distribution(manifest)?;
        let manifest_hash = normalize_manifest_hash(&distribution.manifest_hash);
        validate_chunk_list_against_payload(manifest, payload_tar_bytes)?;
        let chunk_hashes = distribution
            .chunk_list
            .iter()
            .map(|chunk| chunk.chunk_hash.as_str())
            .collect::<Vec<_>>();
        let calculated_merkle_root = compute_merkle_root(&chunk_hashes);
        if normalize_manifest_hash(&calculated_merkle_root)
            != normalize_manifest_hash(&distribution.merkle_root)
        {
            anyhow::bail!(
                "manifest merkle_root mismatch (expected {}, got {})",
                distribution.merkle_root,
                calculated_merkle_root
            );
        }
        let calculated_manifest_hash = compute_manifest_hash_without_signatures(manifest)?;
        if normalize_manifest_hash(&calculated_manifest_hash) != manifest_hash {
            anyhow::bail!(
                "manifest hash mismatch (expected {}, got {})",
                distribution.manifest_hash,
                calculated_manifest_hash
            );
        }

        tx.execute(
            "INSERT OR IGNORE INTO manifests(manifest_hash, manifest_toml, merkle_root, signer_set, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                manifest_hash,
                manifest_document,
                distribution.merkle_root,
                identity.signer_did,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;

        for (ordinal, chunk) in distribution.chunk_list.iter().enumerate() {
            let hash = normalize_blake3_hash(&chunk.chunk_hash);
            let start = chunk.offset as usize;
            let end = start.saturating_add(chunk.length as usize);
            let chunk_data = &payload_tar_bytes[start..end];
            let chunk_path = self.chunk_path(&hash);
            if !chunk_path.exists() {
                if let Some(parent) = chunk_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&chunk_path, chunk_data)?;
            }
            tx.execute(
                "INSERT OR IGNORE INTO chunks(chunk_hash, size_bytes, compression, created_at, tombstoned_at)
                 VALUES (?1, ?2, ?3, ?4, NULL)",
                params![
                    format!("blake3:{hash}"),
                    chunk.length as i64,
                    chunk.compression,
                    chrono::Utc::now().to_rfc3339()
                ],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO manifest_chunks(manifest_hash, ordinal, chunk_hash, offset, length)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    manifest_hash,
                    ordinal as i64,
                    format!("blake3:{hash}"),
                    chunk.offset as i64,
                    chunk.length as i64
                ],
            )?;
        }

        let current_epoch: Option<u64> = tx
            .query_row(
                "SELECT current_epoch FROM capsules WHERE scoped_id=?1",
                params![scoped_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(|v| v as u64);

        let next_epoch = current_epoch.unwrap_or(0) + 1;
        let prev_epoch_hash = current_epoch
            .and_then(|epoch| {
                tx.query_row(
                    "SELECT signed_payload, signature FROM epochs WHERE scoped_id=?1 AND epoch=?2",
                    params![scoped_id, epoch as i64],
                    |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .ok()
                .flatten()
            })
            .map(|(payload, signature)| {
                let mut hasher = blake3::Hasher::new();
                hasher.update(&payload);
                hasher.update(signature.as_bytes());
                format!("blake3:{}", hasher.finalize().to_hex())
            });

        let epoch_unsigned = json!({
            "scoped_id": scoped_id,
            "epoch": next_epoch,
            "manifest_hash": manifest_hash,
            "prev_epoch_hash": prev_epoch_hash,
            "issued_at": issued_at,
            "signer_did": identity.signer_did,
            "key_id": identity.key_id,
        });
        let epoch_payload = serde_jcs::to_vec(&epoch_unsigned)?;
        let signature = signing_key.sign(&epoch_payload);
        let signature_b64 = BASE64.encode(signature.to_bytes());

        tx.execute(
            "INSERT INTO epochs(scoped_id, epoch, manifest_hash, prev_epoch_hash, signed_payload, signer_did, signature, key_id, issued_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                scoped_id,
                next_epoch as i64,
                manifest_hash,
                prev_epoch_hash,
                epoch_payload,
                identity.signer_did,
                signature_b64,
                identity.key_id,
                issued_at
            ],
        )?;
        tx.execute(
            "INSERT INTO capsules(scoped_id, current_epoch, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(scoped_id) DO UPDATE SET current_epoch=excluded.current_epoch, updated_at=excluded.updated_at",
            params![scoped_id, next_epoch as i64, chrono::Utc::now().to_rfc3339()],
        )?;

        Ok(manifest_hash)
    }

    fn tombstone_manifest_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        scoped_id: &str,
        manifest_hash: &str,
        now: &str,
    ) -> Result<bool> {
        let manifest_hash = normalize_manifest_hash(manifest_hash);
        let scoped_known: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM epochs WHERE scoped_id=?1 AND manifest_hash=?2 LIMIT 1",
                params![scoped_id, manifest_hash],
                |row| row.get(0),
            )
            .optional()?;
        if scoped_known.is_none() {
            return Ok(false);
        }

        let changed = tx.execute(
            "UPDATE manifests
             SET tombstoned_at=COALESCE(tombstoned_at, ?2)
             WHERE manifest_hash=?1",
            params![manifest_hash, now],
        )?;
        tx.execute(
            "UPDATE chunks
             SET tombstoned_at=COALESCE(tombstoned_at, ?2)
             WHERE chunk_hash IN (
               SELECT chunk_hash FROM manifest_chunks WHERE manifest_hash=?1
             )",
            params![manifest_hash, now],
        )?;
        Ok(changed > 0)
    }

    fn enqueue_manifest_chunks_for_gc_tx(
        &self,
        tx: &rusqlite::Transaction<'_>,
        manifest_hash: &str,
        reason: &str,
        not_before: &str,
        updated_at: &str,
    ) -> Result<usize> {
        let manifest_hash = normalize_manifest_hash(manifest_hash);
        let reason = if reason.trim().is_empty() {
            "unspecified"
        } else {
            reason.trim()
        };
        let chunk_hashes: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT chunk_hash FROM manifest_chunks
                 WHERE manifest_hash=?1 ORDER BY ordinal ASC",
            )?;
            let rows = stmt.query_map(params![manifest_hash], |row| row.get::<_, String>(0))?;
            let mut values = Vec::new();
            for row in rows {
                values.push(row?);
            }
            values
        };

        for chunk_hash in &chunk_hashes {
            tx.execute(
                "UPDATE chunks
                 SET tombstoned_at=COALESCE(tombstoned_at, ?2)
                 WHERE chunk_hash=?1",
                params![chunk_hash, updated_at],
            )?;
            tx.execute(
                "INSERT INTO gc_queue(chunk_hash, not_before, reason, state, attempts, updated_at)
                 VALUES (?1, ?2, ?3, 'pending', 0, ?4)
                 ON CONFLICT(chunk_hash) DO UPDATE SET
                   not_before=excluded.not_before,
                   reason=excluded.reason,
                   state='pending',
                   updated_at=excluded.updated_at",
                params![chunk_hash, not_before, reason, updated_at],
            )?;
        }
        Ok(chunk_hashes.len())
    }

    pub fn resolve_epoch_pointer(&self, scoped_id: &str) -> Result<Option<EpochResolveResponse>> {
        let conn = self.connect()?;
        let current_epoch: Option<u64> = conn
            .query_row(
                "SELECT current_epoch FROM capsules WHERE scoped_id=?1",
                params![scoped_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(|v| v as u64);
        let Some(epoch) = current_epoch else {
            return Ok(None);
        };

        let row: Option<(String, Option<String>, String, String, String, String)> = conn
            .query_row(
                "SELECT manifest_hash, prev_epoch_hash, issued_at, signer_did, key_id, signature
                 FROM epochs WHERE scoped_id=?1 AND epoch=?2",
                params![scoped_id, epoch as i64],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((manifest_hash, prev_epoch_hash, issued_at, signer_did, key_id, signature)) = row
        else {
            return Ok(None);
        };

        let public_key: Option<String> = conn
            .query_row(
                "SELECT public_key FROM trusted_keys
                 WHERE did=?1
                   AND key_id=?2
                   AND revoked_at IS NULL
                   AND valid_from <= ?3
                   AND (valid_to IS NULL OR valid_to >= ?3)
                 ORDER BY valid_from DESC LIMIT 1",
                params![signer_did, key_id, chrono::Utc::now().to_rfc3339()],
                |row| row.get(0),
            )
            .optional()?;

        let Some(public_key) = public_key else {
            return Ok(None);
        };

        Ok(Some(EpochResolveResponse {
            pointer: EpochPointer {
                scoped_id: scoped_id.to_string(),
                epoch,
                manifest_hash,
                prev_epoch_hash,
                issued_at,
                signer_did,
                key_id,
                signature,
            },
            public_key,
        }))
    }

    pub fn rollback_to_manifest(
        &self,
        scoped_id: &str,
        target_manifest_hash: &str,
    ) -> Result<Option<EpochResolveResponse>> {
        let target_manifest_hash = normalize_manifest_hash(target_manifest_hash);
        let identity = self.ensure_signing_identity()?;
        let (_, signing_key) = self.load_or_create_signing_key()?;
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Exclusive)?;
        let exists_in_history: Option<u64> = tx
            .query_row(
                "SELECT epoch FROM epochs WHERE scoped_id=?1 AND manifest_hash=?2 ORDER BY epoch DESC LIMIT 1",
                params![scoped_id, target_manifest_hash],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(|v| v as u64);
        let Some(_) = exists_in_history else {
            tx.rollback()?;
            return Ok(None);
        };
        let yanked_at: Option<String> = tx
            .query_row(
                "SELECT yanked_at FROM manifests WHERE manifest_hash=?1",
                params![target_manifest_hash],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        if let Some(yanked_at) = yanked_at {
            tx.rollback()?;
            anyhow::bail!(
                "{}: rollback target is yanked (manifest_hash={}, yanked_at={})",
                crate::error_codes::ATO_ERR_INTEGRITY_FAILURE,
                target_manifest_hash,
                yanked_at
            );
        }
        {
            let mut stmt = tx.prepare(
                "SELECT mc.chunk_hash,
                        EXISTS(SELECT 1 FROM chunks c WHERE c.chunk_hash = mc.chunk_hash) AS present
                 FROM manifest_chunks mc
                 WHERE mc.manifest_hash=?1
                 ORDER BY mc.ordinal ASC",
            )?;
            let rows = stmt.query_map(params![target_manifest_hash.clone()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            for row in rows {
                let (chunk_hash, present) = row?;
                if present == 0 {
                    anyhow::bail!(
                        "{}: rollback target references missing chunk row (manifest_hash={}, chunk_hash={})",
                        crate::error_codes::ATO_ERR_INTEGRITY_FAILURE,
                        target_manifest_hash,
                        chunk_hash
                    );
                }
                let normalized = normalize_blake3_hash(&chunk_hash);
                let chunk_path = self.chunk_path(&normalized);
                if !chunk_path.exists() {
                    anyhow::bail!(
                        "{}: rollback target chunk missing on disk (manifest_hash={}, chunk_hash={})",
                        crate::error_codes::ATO_ERR_INTEGRITY_FAILURE,
                        target_manifest_hash,
                        chunk_hash
                    );
                }
                let bytes = std::fs::read(&chunk_path).with_context(|| {
                    format!(
                        "{}: failed to read rollback target chunk {}",
                        crate::error_codes::ATO_ERR_INTEGRITY_FAILURE,
                        chunk_path.display()
                    )
                })?;
                let actual = blake3::hash(&bytes).to_hex().to_string();
                if actual != normalized {
                    anyhow::bail!(
                        "{}: rollback target chunk hash mismatch (chunk_hash={}, got=blake3:{})",
                        crate::error_codes::ATO_ERR_INTEGRITY_FAILURE,
                        chunk_hash,
                        actual
                    );
                }
            }
        }
        crate::process_manager::ProcessManager::new()?
            .cleanup_scoped_processes(scoped_id, true)
            .with_context(|| {
                format!(
                    "Failed to clean up existing processes before rollback for {}",
                    scoped_id
                )
            })?;
        let current_epoch: Option<u64> = tx
            .query_row(
                "SELECT current_epoch FROM capsules WHERE scoped_id=?1",
                params![scoped_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map(|v| v as u64);
        let Some(current_epoch) = current_epoch else {
            tx.rollback()?;
            return Ok(None);
        };
        let previous_epoch_record: Option<(Vec<u8>, String)> = tx
            .query_row(
                "SELECT signed_payload, signature FROM epochs WHERE scoped_id=?1 AND epoch=?2",
                params![scoped_id, current_epoch as i64],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((previous_payload, previous_signature)) = previous_epoch_record else {
            tx.rollback()?;
            return Ok(None);
        };
        let mut previous_hasher = blake3::Hasher::new();
        previous_hasher.update(&previous_payload);
        previous_hasher.update(previous_signature.as_bytes());
        let prev_epoch_hash = Some(format!("blake3:{}", previous_hasher.finalize().to_hex()));
        let next_epoch = current_epoch.saturating_add(1);
        let issued_at = chrono::Utc::now().to_rfc3339();
        let epoch_unsigned = json!({
            "scoped_id": scoped_id,
            "epoch": next_epoch,
            "manifest_hash": target_manifest_hash,
            "prev_epoch_hash": prev_epoch_hash,
            "issued_at": issued_at,
            "signer_did": identity.signer_did,
            "key_id": identity.key_id,
        });
        let epoch_payload = serde_jcs::to_vec(&epoch_unsigned)?;
        let signature = signing_key.sign(&epoch_payload);
        let signature_b64 = BASE64.encode(signature.to_bytes());
        let op_id = new_operation_id("rollback");
        let payload = json!({
            "scoped_id": scoped_id,
            "target_manifest_hash": target_manifest_hash,
            "to_epoch": next_epoch,
        });
        tx.execute(
            "INSERT INTO journal(op_id, op_type, state, payload_json, started_at, finished_at)
             VALUES (?1, 'rollback', 'started', ?2, ?3, NULL)",
            params![
                op_id,
                serde_json::to_string(&payload)?,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        tx.execute(
            "UPDATE manifests SET tombstoned_at=NULL WHERE manifest_hash=?1",
            params![target_manifest_hash.clone()],
        )?;
        tx.execute(
            "UPDATE chunks
             SET tombstoned_at=NULL
             WHERE chunk_hash IN (
               SELECT chunk_hash FROM manifest_chunks WHERE manifest_hash=?1
             )",
            params![target_manifest_hash.clone()],
        )?;
        tx.execute(
            "DELETE FROM gc_queue
             WHERE chunk_hash IN (
               SELECT chunk_hash FROM manifest_chunks WHERE manifest_hash=?1
             )",
            params![target_manifest_hash.clone()],
        )?;
        tx.execute(
            "INSERT INTO epochs(scoped_id, epoch, manifest_hash, prev_epoch_hash, signed_payload, signer_did, signature, key_id, issued_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                scoped_id,
                next_epoch as i64,
                target_manifest_hash,
                prev_epoch_hash,
                epoch_payload,
                identity.signer_did,
                signature_b64,
                identity.key_id,
                issued_at
            ],
        )?;
        tx.execute(
            "UPDATE capsules SET current_epoch=?2, updated_at=?3 WHERE scoped_id=?1",
            params![
                scoped_id,
                next_epoch as i64,
                chrono::Utc::now().to_rfc3339()
            ],
        )?;
        tx.execute(
            "UPDATE journal SET state='completed', finished_at=?2 WHERE op_id=?1",
            params![op_id, chrono::Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
        self.resolve_epoch_pointer(scoped_id)
    }

    pub fn yank_manifest(&self, scoped_id: &str, target_manifest_hash: &str) -> Result<bool> {
        let manifest_hash = normalize_manifest_hash(target_manifest_hash);
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let in_history: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM epochs WHERE scoped_id=?1 AND manifest_hash=?2 LIMIT 1",
                params![scoped_id, manifest_hash],
                |row| row.get(0),
            )
            .optional()?;
        if in_history.is_none() {
            tx.rollback()?;
            return Ok(false);
        }
        tx.execute(
            "UPDATE manifests
             SET yanked_at=COALESCE(yanked_at, ?2)
             WHERE manifest_hash=?1",
            params![manifest_hash, chrono::Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn negotiate(&self, request: &NegotiateRequest) -> Result<NegotiateResponse> {
        let target_manifest_hash = normalize_manifest_hash(&request.target_manifest_hash);
        let conn = self.connect()?;
        let target_known: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM epochs WHERE scoped_id=?1 AND manifest_hash=?2 LIMIT 1",
                params![request.scoped_id, target_manifest_hash],
                |row| row.get(0),
            )
            .optional()?;
        if target_known.is_none() {
            anyhow::bail!(
                "target manifest is not part of scoped capsule history: {}",
                request.target_manifest_hash
            );
        }
        let yanked_at: Option<String> = conn
            .query_row(
                "SELECT yanked_at FROM manifests WHERE manifest_hash=?1",
                params![target_manifest_hash.clone()],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        if let Some(yanked_at) = yanked_at {
            anyhow::bail!(
                "manifest yanked: scoped_id={} manifest_hash={} yanked_at={}",
                request.scoped_id,
                target_manifest_hash,
                yanked_at
            );
        }
        let mut stmt = conn.prepare(
            "SELECT mc.chunk_hash, c.size_bytes
             FROM manifest_chunks mc
             JOIN chunks c ON c.chunk_hash = mc.chunk_hash
             WHERE mc.manifest_hash=?1
             ORDER BY mc.ordinal ASC",
        )?;
        let rows = stmt.query_map(params![target_manifest_hash], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
        })?;
        let have: HashSet<String> = request
            .have_chunks
            .iter()
            .map(|v| normalize_hash(v))
            .collect();
        let bloom = if have.is_empty() {
            request
                .have_chunks_bloom
                .as_ref()
                .map(ParsedBloomFilter::from_request)
                .transpose()?
        } else {
            None
        };
        let max_bytes = request.max_bytes.unwrap_or(u64::MAX);

        let mut required = Vec::new();
        let mut total: u64 = 0;
        for row in rows {
            let (chunk_hash, size) = row?;
            let normalized_chunk = normalize_hash(&chunk_hash);
            let already_have = if !have.is_empty() {
                have.contains(&normalized_chunk)
            } else if let Some(filter) = &bloom {
                filter.might_contain(&normalized_chunk)
            } else {
                false
            };
            if already_have {
                continue;
            }
            if total.saturating_add(size) > max_bytes {
                break;
            }
            required.push(chunk_hash);
            total = total.saturating_add(size);
        }

        let lease = if let Some(reuse_lease_id) = request
            .reuse_lease_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let refreshed = self.refresh_lease(reuse_lease_id, DEFAULT_LEASE_TTL_SECS)?;
            LeaseAcquireResult {
                lease_id: refreshed.lease_id,
                expires_at: refreshed.expires_at,
                chunk_count: refreshed.chunk_count,
            }
        } else {
            self.acquire_manifest_lease(
                &request.scoped_id,
                &target_manifest_hash,
                "registry-negotiate",
                "negotiate",
                DEFAULT_LEASE_TTL_SECS,
            )?
        };

        Ok(NegotiateResponse {
            session_id: new_operation_id("negotiate"),
            required_chunks: required,
            required_manifests: vec![target_manifest_hash],
            yanked: false,
            epoch_pointer: self
                .resolve_epoch_pointer(&request.scoped_id)?
                .map(|v| v.pointer),
            lease_id: Some(lease.lease_id),
            lease_expires_at: Some(lease.expires_at),
        })
    }

    pub fn acquire_manifest_lease(
        &self,
        scoped_id: &str,
        manifest_hash: &str,
        owner: &str,
        purpose: &str,
        ttl_secs: u64,
    ) -> Result<LeaseAcquireResult> {
        let manifest_hash = normalize_manifest_hash(manifest_hash);
        let lease_id = new_operation_id("lease");
        let now = chrono::Utc::now();
        let expires_at = now
            .checked_add_signed(chrono::Duration::seconds(ttl_secs.max(1) as i64))
            .unwrap_or(now)
            .to_rfc3339();
        let created_at = now.to_rfc3339();

        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let target_known: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM epochs WHERE scoped_id=?1 AND manifest_hash=?2 LIMIT 1",
                params![scoped_id, manifest_hash],
                |row| row.get(0),
            )
            .optional()?;
        if target_known.is_none() {
            anyhow::bail!(
                "manifest {} is not part of scoped capsule history {}",
                manifest_hash,
                scoped_id
            );
        }

        let chunk_hashes: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT chunk_hash FROM manifest_chunks
                 WHERE manifest_hash=?1 ORDER BY ordinal ASC",
            )?;
            let rows = stmt.query_map(params![manifest_hash], |row| row.get::<_, String>(0))?;
            let mut hashes = Vec::new();
            for row in rows {
                hashes.push(row?);
            }
            hashes
        };

        for chunk_hash in &chunk_hashes {
            tx.execute(
                "INSERT OR REPLACE INTO leases(lease_id, chunk_hash, owner, expires_at, purpose, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    lease_id,
                    chunk_hash,
                    owner,
                    expires_at,
                    purpose,
                    created_at
                ],
            )?;
        }
        tx.commit()?;

        Ok(LeaseAcquireResult {
            lease_id,
            expires_at,
            chunk_count: chunk_hashes.len(),
        })
    }

    pub fn refresh_lease(&self, lease_id: &str, ttl_secs: u64) -> Result<LeaseRefreshResult> {
        let lease_id = lease_id.trim();
        if lease_id.is_empty() {
            anyhow::bail!("lease_id is required");
        }
        let now = chrono::Utc::now();
        let expires_at = now
            .checked_add_signed(chrono::Duration::seconds(ttl_secs.max(1) as i64))
            .unwrap_or(now)
            .to_rfc3339();

        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let chunk_count: usize = tx.query_row(
            "SELECT COUNT(1) FROM leases WHERE lease_id=?1",
            params![lease_id],
            |row| row.get::<_, i64>(0).map(|v| v as usize),
        )?;
        if chunk_count == 0 {
            anyhow::bail!("lease not found: {}", lease_id);
        }
        tx.execute(
            "UPDATE leases SET expires_at=?2 WHERE lease_id=?1",
            params![lease_id, expires_at],
        )?;
        tx.commit()?;

        Ok(LeaseRefreshResult {
            lease_id: lease_id.to_string(),
            expires_at,
            chunk_count,
        })
    }

    pub fn release_lease(&self, lease_id: &str) -> Result<usize> {
        let lease_id = lease_id.trim();
        if lease_id.is_empty() {
            anyhow::bail!("lease_id is required");
        }
        let conn = self.connect()?;
        let removed = conn.execute("DELETE FROM leases WHERE lease_id=?1", params![lease_id])?;
        Ok(removed)
    }

    pub fn cleanup_expired_leases(&self, now: &str) -> Result<usize> {
        let conn = self.connect()?;
        let removed = conn.execute("DELETE FROM leases WHERE expires_at <= ?1", params![now])?;
        Ok(removed)
    }

    #[cfg(test)]
    pub fn tombstone_manifest(&self, scoped_id: &str, manifest_hash: &str) -> Result<bool> {
        let manifest_hash = normalize_manifest_hash(manifest_hash);
        let now = chrono::Utc::now().to_rfc3339();
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let scoped_known: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM epochs WHERE scoped_id=?1 AND manifest_hash=?2 LIMIT 1",
                params![scoped_id, manifest_hash],
                |row| row.get(0),
            )
            .optional()?;
        if scoped_known.is_none() {
            tx.rollback()?;
            return Ok(false);
        }

        let changed = tx.execute(
            "UPDATE manifests
             SET tombstoned_at=COALESCE(tombstoned_at, ?2)
             WHERE manifest_hash=?1",
            params![manifest_hash, now],
        )?;
        tx.execute(
            "UPDATE chunks
             SET tombstoned_at=COALESCE(tombstoned_at, ?2)
             WHERE chunk_hash IN (
               SELECT chunk_hash FROM manifest_chunks WHERE manifest_hash=?1
             )",
            params![manifest_hash, now],
        )?;
        tx.commit()?;
        Ok(changed > 0)
    }

    #[cfg(test)]
    pub fn enqueue_manifest_chunks_for_gc(
        &self,
        manifest_hash: &str,
        reason: &str,
        not_before: &str,
    ) -> Result<usize> {
        let manifest_hash = normalize_manifest_hash(manifest_hash);
        let reason = if reason.trim().is_empty() {
            "unspecified"
        } else {
            reason.trim()
        };
        let now = chrono::Utc::now().to_rfc3339();
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let chunk_hashes: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT chunk_hash FROM manifest_chunks
                 WHERE manifest_hash=?1 ORDER BY ordinal ASC",
            )?;
            let rows = stmt.query_map(params![manifest_hash], |row| row.get::<_, String>(0))?;
            let mut values = Vec::new();
            for row in rows {
                values.push(row?);
            }
            values
        };

        for chunk_hash in &chunk_hashes {
            tx.execute(
                "UPDATE chunks
                 SET tombstoned_at=COALESCE(tombstoned_at, ?2)
                 WHERE chunk_hash=?1",
                params![chunk_hash, now],
            )?;
            tx.execute(
                "INSERT INTO gc_queue(chunk_hash, not_before, reason, state, attempts, updated_at)
                 VALUES (?1, ?2, ?3, 'pending', 0, ?4)
                 ON CONFLICT(chunk_hash) DO UPDATE SET
                   not_before=excluded.not_before,
                   reason=excluded.reason,
                   state='pending',
                   updated_at=excluded.updated_at",
                params![chunk_hash, not_before, reason, now],
            )?;
        }
        tx.commit()?;
        Ok(chunk_hashes.len())
    }

    pub fn gc_tick(&self, now: &str, max_chunks: usize) -> Result<GcTickResult> {
        let mut result = GcTickResult {
            expired_leases: self.cleanup_expired_leases(now)?,
            ..GcTickResult::default()
        };

        let mut conn = self.connect()?;
        let queue: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT chunk_hash
                 FROM gc_queue
                 WHERE state IN ('pending', 'deferred', 'failed')
                   AND not_before <= ?1
                 ORDER BY not_before ASC
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![now, max_chunks as i64], |row| row.get(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            out
        };

        for chunk_hash in queue {
            result.processed += 1;
            let tombstoned: Option<Option<String>> = conn
                .query_row(
                    "SELECT tombstoned_at FROM chunks WHERE chunk_hash=?1",
                    params![chunk_hash],
                    |row| row.get(0),
                )
                .optional()?;

            if tombstoned.is_none() {
                conn.execute(
                    "UPDATE gc_queue SET state='deleted', updated_at=?2 WHERE chunk_hash=?1",
                    params![chunk_hash, now],
                )?;
                result.deleted += 1;
                continue;
            }

            let has_tombstone = tombstoned.flatten().is_some();
            let active_lease: Option<i64> = conn
                .query_row(
                    "SELECT 1 FROM leases WHERE chunk_hash=?1 AND expires_at > ?2 LIMIT 1",
                    params![chunk_hash, now],
                    |row| row.get(0),
                )
                .optional()?;
            let live_reference: Option<i64> = conn
                .query_row(
                    "SELECT 1
                     FROM manifest_chunks mc
                     JOIN manifests m ON m.manifest_hash = mc.manifest_hash
                     WHERE mc.chunk_hash=?1
                       AND m.tombstoned_at IS NULL
                     LIMIT 1",
                    params![chunk_hash],
                    |row| row.get(0),
                )
                .optional()?;
            let retention_pinned: Option<i64> = conn
                .query_row(
                    "SELECT 1
                     FROM manifest_chunks mc
                     JOIN (
                       SELECT scoped_id, manifest_hash
                       FROM (
                         SELECT scoped_id,
                                manifest_hash,
                                ROW_NUMBER() OVER (
                                  PARTITION BY scoped_id
                                  ORDER BY created_at DESC, version DESC
                                ) AS release_rank
                         FROM registry_releases
                       )
                       WHERE release_rank <= ?2
                     ) pinned ON pinned.manifest_hash = mc.manifest_hash
                     WHERE mc.chunk_hash=?1
                     LIMIT 1",
                    params![chunk_hash, RETENTION_PINNED_RELEASES],
                    |row| row.get(0),
                )
                .optional()?;

            if !has_tombstone
                || active_lease.is_some()
                || live_reference.is_some()
                || retention_pinned.is_some()
            {
                let defer_until = chrono::Utc::now()
                    .checked_add_signed(chrono::Duration::seconds(DEFAULT_GC_DEFER_SECS))
                    .unwrap_or_else(chrono::Utc::now)
                    .to_rfc3339();
                conn.execute(
                    "UPDATE gc_queue
                     SET state='deferred',
                         not_before=?2,
                         attempts=attempts + 1,
                         updated_at=?3
                     WHERE chunk_hash=?1",
                    params![chunk_hash, defer_until, now],
                )?;
                result.deferred += 1;
                continue;
            }

            let normalized = normalize_blake3_hash(&chunk_hash);
            let chunk_path = self.chunk_path(&normalized);
            if chunk_path.exists() {
                if let Err(err) = std::fs::remove_file(&chunk_path) {
                    conn.execute(
                        "UPDATE gc_queue
                         SET state='failed', attempts=attempts + 1, updated_at=?2, reason=?3
                         WHERE chunk_hash=?1",
                        params![chunk_hash, now, format!("unlink_failed:{err}")],
                    )?;
                    result.failed += 1;
                    continue;
                }
            }

            let tx = conn.transaction()?;
            let apply = (|| -> Result<()> {
                tx.execute(
                    "DELETE FROM leases WHERE chunk_hash=?1",
                    params![chunk_hash.clone()],
                )?;
                tx.execute(
                    "DELETE FROM manifest_chunks WHERE chunk_hash=?1",
                    params![chunk_hash.clone()],
                )?;
                tx.execute(
                    "DELETE FROM chunks WHERE chunk_hash=?1",
                    params![chunk_hash.clone()],
                )?;
                tx.execute(
                    "UPDATE gc_queue
                     SET state='deleted', attempts=attempts + 1, updated_at=?2
                     WHERE chunk_hash=?1",
                    params![chunk_hash, now],
                )?;
                Ok(())
            })();

            if let Err(err) = apply {
                tx.rollback()?;
                conn.execute(
                    "UPDATE gc_queue
                     SET state='failed', attempts=attempts + 1, updated_at=?2, reason=?3
                     WHERE chunk_hash=?1",
                    params![chunk_hash, now, format!("db_reflect_failed:{err}")],
                )?;
                result.failed += 1;
                continue;
            }

            tx.commit()?;
            result.deleted += 1;
        }

        Ok(result)
    }

    pub fn checkpoint_wal_truncate(&self) -> Result<()> {
        let conn = self.connect()?;
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    pub fn incremental_vacuum(&self, pages: usize) -> Result<()> {
        let conn = self.connect()?;
        conn.execute_batch(&format!("PRAGMA incremental_vacuum({});", pages.max(1)))?;
        Ok(())
    }

    pub fn rotate_signing_key(&self, overlap_hours: i64) -> Result<KeyRotateResponse> {
        let now = chrono::Utc::now();
        let valid_from = now.to_rfc3339();
        let overlap_hours = overlap_hours.max(0);
        let previous_valid_to = now
            .checked_add_signed(chrono::Duration::hours(overlap_hours))
            .unwrap_or(now)
            .to_rfc3339();

        let (previous_key_id, previous_signing_key) = self.load_or_create_signing_key()?;
        let previous_public = previous_signing_key.verifying_key().to_bytes();
        let previous_did = public_key_to_did(&previous_public);
        let previous_public_b64 = BASE64.encode(previous_public);

        let (key_id, signing_key) = self.create_and_activate_signing_key()?;
        let public_key = signing_key.verifying_key().to_bytes();
        let signer_did = public_key_to_did(&public_key);
        let public_key_b64 = BASE64.encode(public_key);

        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO trusted_keys(did, key_id, public_key, valid_from, valid_to, revoked_at)
             VALUES (?1, ?2, ?3, ?4, NULL, NULL)",
            params![
                previous_did,
                previous_key_id,
                previous_public_b64,
                valid_from.clone()
            ],
        )?;
        tx.execute(
            "INSERT INTO trusted_keys(did, key_id, public_key, valid_from, valid_to, revoked_at)
             VALUES (?1, ?2, ?3, ?4, NULL, NULL)
             ON CONFLICT(did, key_id) DO UPDATE SET
               public_key=excluded.public_key,
               valid_from=excluded.valid_from,
               valid_to=NULL,
               revoked_at=NULL",
            params![signer_did, key_id, public_key_b64, valid_from],
        )?;

        if previous_key_id != key_id {
            tx.execute(
                "UPDATE trusted_keys
                 SET valid_to=CASE
                   WHEN valid_to IS NULL OR valid_to > ?3 THEN ?3
                   ELSE valid_to
                 END
                 WHERE did=?1 AND key_id=?2",
                params![previous_did, previous_key_id, previous_valid_to],
            )?;
        }
        tx.commit()?;

        Ok(KeyRotateResponse {
            signer_did,
            key_id: key_id.clone(),
            public_key: public_key_b64,
            valid_from: now.to_rfc3339(),
            previous_key_id: if previous_key_id == key_id {
                None
            } else {
                Some(previous_key_id)
            },
            previous_valid_to: if overlap_hours > 0 {
                Some(previous_valid_to)
            } else {
                Some(now.to_rfc3339())
            },
        })
    }

    pub fn revoke_key(&self, key_id: &str, did: Option<&str>) -> Result<KeyRevokeResponse> {
        let key_id = key_id.trim();
        if key_id.is_empty() {
            anyhow::bail!("key_id is required");
        }
        let did = did.map(str::trim).filter(|value| !value.is_empty());
        let mut conn = self.connect()?;
        let dids = {
            let mut stmt = conn.prepare(
                "SELECT did FROM trusted_keys
                 WHERE key_id=?1
                 ORDER BY did ASC",
            )?;
            let rows = stmt.query_map(params![key_id], |row| row.get::<_, String>(0))?;
            let mut values = Vec::new();
            for row in rows {
                values.push(row?);
            }
            values
        };
        if dids.is_empty() {
            anyhow::bail!("key_id not found: {}", key_id);
        }
        let target_dids: Vec<String> = if let Some(requested) = did {
            if !dids.iter().any(|candidate| candidate == requested) {
                anyhow::bail!("key_id {} not found for did {}", key_id, requested);
            }
            vec![requested.to_string()]
        } else if dids.len() > 1 {
            let candidates = dids.join(", ");
            anyhow::bail!(
                "key_id {} is shared by multiple did values; specify --did (candidates: {})",
                key_id,
                candidates
            );
        } else {
            vec![dids[0].clone()]
        };

        let now = chrono::Utc::now().to_rfc3339();
        let tx = conn.transaction()?;
        let mut revoked = 0usize;
        for target_did in &target_dids {
            let updated = tx.execute(
                "UPDATE trusted_keys
                 SET revoked_at=COALESCE(revoked_at, ?3),
                     valid_to=COALESCE(valid_to, ?3)
                 WHERE did=?1 AND key_id=?2",
                params![target_did, key_id, now],
            )?;
            revoked += updated;
        }
        tx.commit()?;

        let mut active_key_rotated_to = None;
        if let Some(active_key_id) = self.active_key_id()? {
            if active_key_id == key_id {
                let active_signing_key = self.load_signing_key_by_id(&active_key_id)?;
                let active_did = public_key_to_did(&active_signing_key.verifying_key().to_bytes());
                if target_dids.iter().any(|target| target == &active_did) {
                    let rotated = self.rotate_signing_key(0)?;
                    active_key_rotated_to = Some(rotated.key_id);
                }
            }
        }

        Ok(KeyRevokeResponse {
            revoked,
            active_key_rotated_to,
        })
    }

    pub fn load_manifest_document(&self, manifest_hash: &str) -> Result<Option<Vec<u8>>> {
        let manifest_hash = normalize_manifest_hash(manifest_hash);
        let conn = self.connect()?;
        let row: Option<(Vec<u8>, Option<String>)> = conn
            .query_row(
                "SELECT manifest_toml, yanked_at FROM manifests WHERE manifest_hash=?1",
                params![manifest_hash],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((canonical, yanked_at)) = row else {
            return Ok(None);
        };
        if let Some(yanked_at) = yanked_at {
            anyhow::bail!(
                "manifest yanked: manifest_hash={} yanked_at={}",
                manifest_hash,
                yanked_at
            );
        }
        let expected = normalize_manifest_hash(&manifest_hash);
        let actual = match std::str::from_utf8(&canonical) {
            Ok(manifest_toml) => match toml::from_str::<CapsuleManifest>(manifest_toml) {
                Ok(manifest) => compute_manifest_hash_without_signatures(&manifest)?,
                Err(_) => format!("blake3:{}", blake3::hash(&canonical).to_hex()),
            },
            Err(_) => format!("blake3:{}", blake3::hash(&canonical).to_hex()),
        };
        if normalize_manifest_hash(&actual) != expected {
            anyhow::bail!(
                "manifest hash mismatch in storage (expected blake3:{}, got blake3:{})",
                normalize_blake3_hash(&expected),
                normalize_blake3_hash(&actual)
            );
        }
        Ok(Some(canonical))
    }

    pub fn load_chunk_bytes(&self, chunk_hash: &str) -> Result<Option<Vec<u8>>> {
        let normalized = normalize_blake3_hash(chunk_hash);
        let path = self.chunk_path(&normalized);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(path)?;
        let actual = blake3::hash(&bytes).to_hex().to_string();
        if actual != normalized {
            anyhow::bail!(
                "chunk hash mismatch in storage (expected blake3:{}, got blake3:{})",
                normalized,
                actual
            );
        }
        Ok(Some(bytes))
    }

    fn chunk_path(&self, hash: &str) -> PathBuf {
        let prefix = if hash.len() >= 2 { &hash[0..2] } else { "00" };
        self.data_dir.join("chunks").join(prefix).join(hash)
    }

    fn connect(&self) -> Result<Connection> {
        let conn = Connection::open(&self.db_path)
            .with_context(|| format!("failed to open {}", self.db_path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(conn)
    }

    fn init_schema(&self) -> Result<()> {
        let mut conn = self.connect()?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS manifests(
              manifest_hash TEXT PRIMARY KEY,
              manifest_toml BLOB NOT NULL,
              merkle_root TEXT NOT NULL,
              signer_set TEXT NOT NULL,
              created_at TEXT NOT NULL,
              tombstoned_at TEXT,
              yanked_at TEXT
            );
            CREATE TABLE IF NOT EXISTS chunks(
              chunk_hash TEXT PRIMARY KEY,
              size_bytes INTEGER NOT NULL,
              compression TEXT NOT NULL,
              created_at TEXT NOT NULL,
              tombstoned_at TEXT
            );
            CREATE TABLE IF NOT EXISTS manifest_chunks(
              manifest_hash TEXT NOT NULL,
              ordinal INTEGER NOT NULL,
              chunk_hash TEXT NOT NULL,
              offset INTEGER NOT NULL,
              length INTEGER NOT NULL,
              PRIMARY KEY(manifest_hash, ordinal),
              FOREIGN KEY(manifest_hash) REFERENCES manifests(manifest_hash) ON DELETE CASCADE,
              FOREIGN KEY(chunk_hash) REFERENCES chunks(chunk_hash) ON DELETE RESTRICT
            );
            CREATE TABLE IF NOT EXISTS capsules(
              scoped_id TEXT PRIMARY KEY,
              current_epoch INTEGER NOT NULL,
              updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS epochs(
              scoped_id TEXT NOT NULL,
              epoch INTEGER NOT NULL,
              manifest_hash TEXT NOT NULL,
              prev_epoch_hash TEXT,
              signed_payload BLOB NOT NULL,
              signer_did TEXT NOT NULL,
              signature TEXT NOT NULL,
              key_id TEXT NOT NULL,
              issued_at TEXT NOT NULL,
              PRIMARY KEY(scoped_id, epoch),
              FOREIGN KEY(manifest_hash) REFERENCES manifests(manifest_hash) ON DELETE RESTRICT
            );
            CREATE TABLE IF NOT EXISTS leases(
              lease_id TEXT NOT NULL,
              chunk_hash TEXT NOT NULL,
              owner TEXT NOT NULL,
              expires_at TEXT NOT NULL,
              purpose TEXT NOT NULL,
              created_at TEXT NOT NULL,
              PRIMARY KEY(lease_id, chunk_hash)
            );
            CREATE TABLE IF NOT EXISTS gc_queue(
              chunk_hash TEXT PRIMARY KEY,
              not_before TEXT NOT NULL,
              reason TEXT NOT NULL,
              state TEXT NOT NULL,
              attempts INTEGER NOT NULL DEFAULT 0,
              updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS journal(
              op_id TEXT PRIMARY KEY,
              op_type TEXT NOT NULL,
              state TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              started_at TEXT NOT NULL,
              finished_at TEXT
            );
            CREATE TABLE IF NOT EXISTS trusted_keys(
              did TEXT NOT NULL,
              key_id TEXT NOT NULL,
              public_key TEXT NOT NULL,
              valid_from TEXT NOT NULL,
              valid_to TEXT,
              revoked_at TEXT,
              PRIMARY KEY(did, key_id)
            );
            CREATE TABLE IF NOT EXISTS registry_packages(
              scoped_id TEXT PRIMARY KEY,
              publisher TEXT NOT NULL,
              slug TEXT NOT NULL,
              name TEXT NOT NULL,
              description TEXT NOT NULL,
              latest_version TEXT NOT NULL,
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS registry_releases(
              scoped_id TEXT NOT NULL,
              version TEXT NOT NULL,
              manifest_hash TEXT NOT NULL,
              file_name TEXT NOT NULL,
              sha256 TEXT NOT NULL,
              blake3 TEXT NOT NULL,
              size_bytes INTEGER NOT NULL,
              signature_status TEXT NOT NULL,
              created_at TEXT NOT NULL,
              PRIMARY KEY(scoped_id, version),
              FOREIGN KEY(scoped_id) REFERENCES registry_packages(scoped_id) ON DELETE CASCADE,
              FOREIGN KEY(manifest_hash) REFERENCES manifests(manifest_hash) ON DELETE RESTRICT
            );
            CREATE TABLE IF NOT EXISTS registry_store_metadata(
              scoped_id TEXT PRIMARY KEY,
              icon_path TEXT,
              text TEXT,
              updated_at TEXT NOT NULL,
              FOREIGN KEY(scoped_id) REFERENCES registry_packages(scoped_id) ON DELETE CASCADE
            );
            CREATE TABLE IF NOT EXISTS persistent_states(
              state_id TEXT PRIMARY KEY,
              owner_scope TEXT NOT NULL,
              state_name TEXT NOT NULL,
                            kind TEXT NOT NULL,
                            backend_kind TEXT NOT NULL,
              backend_locator TEXT NOT NULL,
              producer TEXT NOT NULL,
              purpose TEXT NOT NULL,
              schema_id TEXT NOT NULL,
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL,
              UNIQUE(owner_scope, backend_locator)
            );
                        CREATE TABLE IF NOT EXISTS service_bindings(
                            binding_id TEXT PRIMARY KEY,
                            owner_scope TEXT NOT NULL,
                            service_name TEXT NOT NULL,
                            binding_kind TEXT NOT NULL,
                            transport_kind TEXT NOT NULL,
                            adapter_kind TEXT NOT NULL,
                            endpoint_locator TEXT NOT NULL,
                            tls_mode TEXT NOT NULL,
                            allowed_callers_json TEXT NOT NULL DEFAULT '[]',
                            target_hint TEXT,
                            created_at TEXT NOT NULL,
                            updated_at TEXT NOT NULL,
                            UNIQUE(owner_scope, service_name, binding_kind)
                        );
            CREATE TABLE IF NOT EXISTS schema_migrations(
              migration_id TEXT PRIMARY KEY,
              applied_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_epochs_manifest ON epochs(scoped_id, manifest_hash, epoch DESC);
            CREATE INDEX IF NOT EXISTS idx_manifest_chunks_hash ON manifest_chunks(chunk_hash);
            CREATE INDEX IF NOT EXISTS idx_leases_chunk_expires ON leases(chunk_hash, expires_at);
            CREATE INDEX IF NOT EXISTS idx_gc_queue_state_not_before ON gc_queue(state, not_before);
            CREATE INDEX IF NOT EXISTS idx_chunks_tombstoned ON chunks(tombstoned_at);
            CREATE INDEX IF NOT EXISTS idx_registry_packages_publisher_slug ON registry_packages(publisher, slug);
            CREATE INDEX IF NOT EXISTS idx_registry_releases_manifest_hash ON registry_releases(manifest_hash);
            CREATE INDEX IF NOT EXISTS idx_registry_store_metadata_updated_at ON registry_store_metadata(updated_at);
            CREATE INDEX IF NOT EXISTS idx_persistent_states_owner_scope ON persistent_states(owner_scope, state_name);
            CREATE INDEX IF NOT EXISTS idx_service_bindings_owner_scope ON service_bindings(owner_scope, service_name, binding_kind);
            ",
        )?;
        self.apply_schema_migrations(&mut conn)?;
        Ok(())
    }

    fn apply_schema_migrations(&self, conn: &mut Connection) -> Result<()> {
        if !self.is_migration_applied(conn, SCHEMA_MIGRATION_0001)? {
            if !self.column_exists(conn, "manifests", "tombstoned_at")? {
                conn.execute("ALTER TABLE manifests ADD COLUMN tombstoned_at TEXT", [])?;
            }
            self.mark_migration_applied(conn, SCHEMA_MIGRATION_0001)?;
        }

        if !self.is_migration_applied(conn, SCHEMA_MIGRATION_0002)? {
            let rebuild = !self.table_exists(conn, "leases")?
                || !self.column_exists(conn, "leases", "created_at")?
                || !self.leases_has_composite_pk(conn)?;
            if rebuild {
                conn.execute_batch(
                    "
                    CREATE TABLE IF NOT EXISTS leases_new(
                      lease_id TEXT NOT NULL,
                      chunk_hash TEXT NOT NULL,
                      owner TEXT NOT NULL,
                      expires_at TEXT NOT NULL,
                      purpose TEXT NOT NULL,
                      created_at TEXT NOT NULL,
                      PRIMARY KEY(lease_id, chunk_hash)
                    );
                    ",
                )?;
                if self.table_exists(conn, "leases")? {
                    let with_created_at = self.column_exists(conn, "leases", "created_at")?;
                    if with_created_at {
                        conn.execute(
                            "INSERT OR IGNORE INTO leases_new(lease_id, chunk_hash, owner, expires_at, purpose, created_at)
                             SELECT lease_id, chunk_hash, owner, expires_at, purpose, created_at FROM leases",
                            [],
                        )?;
                    } else {
                        conn.execute(
                            "INSERT OR IGNORE INTO leases_new(lease_id, chunk_hash, owner, expires_at, purpose, created_at)
                             SELECT lease_id, chunk_hash, owner, expires_at, purpose, ?1 FROM leases",
                            params![chrono::Utc::now().to_rfc3339()],
                        )?;
                    }
                    conn.execute("DROP TABLE leases", [])?;
                }
                conn.execute("ALTER TABLE leases_new RENAME TO leases", [])?;
            }
            self.mark_migration_applied(conn, SCHEMA_MIGRATION_0002)?;
        }

        if !self.is_migration_applied(conn, SCHEMA_MIGRATION_0003)? {
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_leases_chunk_expires ON leases(chunk_hash, expires_at)",
                [],
            )?;
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_gc_queue_state_not_before ON gc_queue(state, not_before)",
                [],
            )?;
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_chunks_tombstoned ON chunks(tombstoned_at)",
                [],
            )?;
            self.mark_migration_applied(conn, SCHEMA_MIGRATION_0003)?;
        }

        if !self.is_migration_applied(conn, SCHEMA_MIGRATION_0004)? {
            let auto_vacuum_mode: i64 =
                conn.query_row("PRAGMA auto_vacuum", [], |row| row.get(0))?;
            if auto_vacuum_mode != 2 {
                conn.execute_batch("PRAGMA auto_vacuum=INCREMENTAL; VACUUM;")?;
            }
            self.mark_migration_applied(conn, SCHEMA_MIGRATION_0004)?;
        }

        if !self.is_migration_applied(conn, SCHEMA_MIGRATION_0005)? {
            if !self.column_exists(conn, "manifests", "yanked_at")? {
                conn.execute("ALTER TABLE manifests ADD COLUMN yanked_at TEXT", [])?;
            }
            self.mark_migration_applied(conn, SCHEMA_MIGRATION_0005)?;
        }

        if !self.is_migration_applied(conn, SCHEMA_MIGRATION_0006)? {
            conn.execute_batch(
                "
                CREATE TABLE IF NOT EXISTS persistent_states(
                  state_id TEXT PRIMARY KEY,
                  owner_scope TEXT NOT NULL,
                  state_name TEXT NOT NULL,
                                    kind TEXT NOT NULL,
                                    backend_kind TEXT NOT NULL,
                  backend_locator TEXT NOT NULL,
                  producer TEXT NOT NULL,
                  purpose TEXT NOT NULL,
                  schema_id TEXT NOT NULL,
                  created_at TEXT NOT NULL,
                  updated_at TEXT NOT NULL,
                  UNIQUE(owner_scope, backend_locator)
                );
                CREATE INDEX IF NOT EXISTS idx_persistent_states_owner_scope
                  ON persistent_states(owner_scope, state_name);
                ",
            )?;
            self.mark_migration_applied(conn, SCHEMA_MIGRATION_0006)?;
        }

        if !self.is_migration_applied(conn, SCHEMA_MIGRATION_0007)? {
            if !self.column_exists(conn, "persistent_states", "kind")? {
                conn.execute(
                    "ALTER TABLE persistent_states ADD COLUMN kind TEXT NOT NULL DEFAULT 'filesystem'",
                    [],
                )?;
            }
            if !self.column_exists(conn, "persistent_states", "backend_kind")? {
                conn.execute(
                    "ALTER TABLE persistent_states ADD COLUMN backend_kind TEXT NOT NULL DEFAULT 'host_path'",
                    [],
                )?;
            }
            self.mark_migration_applied(conn, SCHEMA_MIGRATION_0007)?;
        }

        if !self.is_migration_applied(conn, SCHEMA_MIGRATION_0008)? {
            conn.execute_batch(
                "
                                CREATE TABLE IF NOT EXISTS service_bindings(
                                    binding_id TEXT PRIMARY KEY,
                                    owner_scope TEXT NOT NULL,
                                    service_name TEXT NOT NULL,
                                    binding_kind TEXT NOT NULL,
                                    transport_kind TEXT NOT NULL,
                                    adapter_kind TEXT NOT NULL,
                                    endpoint_locator TEXT NOT NULL,
                                    tls_mode TEXT NOT NULL,
                                    allowed_callers_json TEXT NOT NULL DEFAULT '[]',
                                    target_hint TEXT,
                                    created_at TEXT NOT NULL,
                                    updated_at TEXT NOT NULL,
                                    UNIQUE(owner_scope, service_name, binding_kind)
                                );
                                CREATE INDEX IF NOT EXISTS idx_service_bindings_owner_scope
                                    ON service_bindings(owner_scope, service_name, binding_kind);
                                ",
            )?;
            self.mark_migration_applied(conn, SCHEMA_MIGRATION_0008)?;
        }

        if !self.is_migration_applied(conn, SCHEMA_MIGRATION_0009)? {
            if !self.column_exists(conn, "service_bindings", "allowed_callers_json")? {
                conn.execute(
                    "ALTER TABLE service_bindings ADD COLUMN allowed_callers_json TEXT NOT NULL DEFAULT '[]'",
                    [],
                )?;
            }
            self.mark_migration_applied(conn, SCHEMA_MIGRATION_0009)?;
        }

        Ok(())
    }

    fn is_migration_applied(&self, conn: &Connection, migration_id: &str) -> Result<bool> {
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM schema_migrations WHERE migration_id=?1",
                params![migration_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(exists.is_some())
    }

    fn mark_migration_applied(&self, conn: &Connection, migration_id: &str) -> Result<()> {
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations(migration_id, applied_at) VALUES (?1, ?2)",
            params![migration_id, chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    fn table_exists(&self, conn: &Connection, table_name: &str) -> Result<bool> {
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                params![table_name],
                |row| row.get(0),
            )
            .optional()?;
        Ok(exists.is_some())
    }

    fn column_exists(
        &self,
        conn: &Connection,
        table_name: &str,
        column_name: &str,
    ) -> Result<bool> {
        let pragma = format!("PRAGMA table_info({})", table_name);
        let mut stmt = conn.prepare(&pragma)?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
        for row in rows {
            if row?.eq_ignore_ascii_case(column_name) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn leases_has_composite_pk(&self, conn: &Connection) -> Result<bool> {
        let mut lease_pk = 0i64;
        let mut chunk_pk = 0i64;
        let mut stmt = conn.prepare("PRAGMA table_info(leases)")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i64>(5)?, // pk ordinal
            ))
        })?;
        for row in rows {
            let (name, pk) = row?;
            if name.eq_ignore_ascii_case("lease_id") {
                lease_pk = pk;
            } else if name.eq_ignore_ascii_case("chunk_hash") {
                chunk_pk = pk;
            }
        }
        Ok(lease_pk > 0 && chunk_pk > 0)
    }

    fn ensure_signing_identity(&self) -> Result<SigningIdentity> {
        let (key_id, signing_key) = self.load_or_create_signing_key()?;
        let public_key = signing_key.verifying_key().to_bytes();
        let did = public_key_to_did(&public_key);
        let public_key_b64 = BASE64.encode(public_key);
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO trusted_keys(did, key_id, public_key, valid_from, valid_to, revoked_at)
             VALUES (?1, ?2, ?3, ?4, NULL, NULL)
             ON CONFLICT(did, key_id) DO UPDATE SET
               public_key=excluded.public_key,
               valid_from=CASE
                 WHEN trusted_keys.valid_from > excluded.valid_from THEN excluded.valid_from
                 ELSE trusted_keys.valid_from
               END,
               revoked_at=NULL",
            params![did, key_id, public_key_b64, chrono::Utc::now().to_rfc3339()],
        )?;
        Ok(SigningIdentity {
            signer_did: did,
            key_id,
            public_key: public_key_b64,
        })
    }

    fn load_or_create_signing_key(&self) -> Result<(String, SigningKey)> {
        if let Some(active_key_id) = self.active_key_id()? {
            let signing_key = self.load_signing_key_by_id(&active_key_id)?;
            return Ok((active_key_id, signing_key));
        }

        if let Some(secret) = self.read_legacy_signing_key_if_exists()? {
            let signing_key = SigningKey::from_bytes(&secret);
            let key_id = short_key_id(&signing_key.verifying_key().to_bytes());
            self.write_signing_key(&key_id, &secret)?;
            self.set_active_key_id(&key_id)?;
            return Ok((key_id, signing_key));
        }

        self.create_and_activate_signing_key()
    }

    fn create_and_activate_signing_key(&self) -> Result<(String, SigningKey)> {
        let mut secret = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut secret);
        let signing_key = SigningKey::from_bytes(&secret);
        let key_id = short_key_id(&signing_key.verifying_key().to_bytes());
        self.write_signing_key(&key_id, &secret)?;
        self.set_active_key_id(&key_id)?;
        Ok((key_id, signing_key))
    }

    fn keys_dir(&self) -> PathBuf {
        self.data_dir.join(KEY_DIR)
    }

    fn active_key_path(&self) -> PathBuf {
        self.keys_dir().join(ACTIVE_KEY_FILE)
    }

    fn signing_key_path(&self, key_id: &str) -> PathBuf {
        self.keys_dir().join(format!("{key_id}.bin"))
    }

    fn active_key_id(&self) -> Result<Option<String>> {
        let path = self.active_key_path();
        if !path.exists() {
            return Ok(None);
        }
        let key_id = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?
            .trim()
            .to_string();
        if key_id.is_empty() {
            return Ok(None);
        }
        Ok(Some(key_id))
    }

    fn set_active_key_id(&self, key_id: &str) -> Result<()> {
        let keys_dir = self.keys_dir();
        std::fs::create_dir_all(&keys_dir)
            .with_context(|| format!("failed to create {}", keys_dir.display()))?;
        let path = self.active_key_path();
        std::fs::write(&path, key_id).with_context(|| format!("failed to write {}", path.display()))
    }

    fn load_signing_key_by_id(&self, key_id: &str) -> Result<SigningKey> {
        let path = self.signing_key_path(key_id);
        let bytes =
            std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
        if bytes.len() != 32 {
            anyhow::bail!("invalid signing key length in {}", path.display());
        }
        let mut secret = [0u8; 32];
        secret.copy_from_slice(&bytes);
        Ok(SigningKey::from_bytes(&secret))
    }

    fn write_signing_key(&self, key_id: &str, secret: &[u8; 32]) -> Result<()> {
        let keys_dir = self.keys_dir();
        std::fs::create_dir_all(&keys_dir)
            .with_context(|| format!("failed to create {}", keys_dir.display()))?;
        let path = self.signing_key_path(key_id);
        std::fs::write(&path, secret)
            .with_context(|| format!("failed to write {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)?.permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(&path, perms)?;
        }
        Ok(())
    }

    fn read_legacy_signing_key_if_exists(&self) -> Result<Option<[u8; 32]>> {
        let path = self.data_dir.join(SIGNING_KEY_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let bytes =
            std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
        if bytes.len() != 32 {
            anyhow::bail!("invalid signing key length in {}", path.display());
        }
        let mut secret = [0u8; 32];
        secret.copy_from_slice(&bytes);
        Ok(Some(secret))
    }
}

pub fn compute_merkle_root(chunk_hashes: &[&str]) -> String {
    if chunk_hashes.is_empty() {
        return format!("blake3:{}", blake3::hash(b"").to_hex());
    }

    let mut level: Vec<[u8; 32]> = chunk_hashes
        .iter()
        .map(|h| {
            let normalized = normalize_blake3_hash(h);
            let mut out = [0u8; 32];
            let decoded = hex::decode(normalized).unwrap_or_else(|_| vec![0u8; 32]);
            if decoded.len() == 32 {
                out.copy_from_slice(&decoded);
            }
            out
        })
        .collect();

    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut i = 0usize;
        while i < level.len() {
            let left = level[i];
            let right = if i + 1 < level.len() {
                level[i + 1]
            } else {
                level[i]
            };
            let mut hasher = blake3::Hasher::new();
            hasher.update(&left);
            hasher.update(&right);
            let digest = hasher.finalize();
            let mut out = [0u8; 32];
            out.copy_from_slice(digest.as_bytes());
            next.push(out);
            i += 2;
        }
        level = next;
    }

    format!("blake3:{}", hex::encode(level[0]))
}

pub fn normalize_blake3_hash(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("blake3:")
        .to_ascii_lowercase()
}

pub fn normalize_manifest_hash(value: &str) -> String {
    format!("blake3:{}", normalize_blake3_hash(value))
}

fn normalize_hash(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("sha256:")
        .trim_start_matches("blake3:")
        .to_ascii_lowercase()
}

fn compare_versions(left: &str, right: &str) -> Ordering {
    match (Version::parse(left), Version::parse(right)) {
        (Ok(left), Ok(right)) => left.cmp(&right),
        _ => left.cmp(right),
    }
}

fn choose_latest_version(current: Option<&str>, candidate: &str) -> String {
    match current {
        Some(current) if compare_versions(current, candidate) == Ordering::Greater => {
            current.to_string()
        }
        _ => candidate.to_string(),
    }
}

fn latest_version_from_releases(releases: &[RegistryReleaseRecord]) -> Option<String> {
    releases
        .iter()
        .max_by(|left, right| compare_versions(&left.version, &right.version))
        .map(|release| release.version.clone())
}

fn short_key_id(public_key: &[u8; 32]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(public_key);
    let digest = hasher.finalize().to_hex().to_string();
    format!("k{}", &digest[..12])
}

fn new_operation_id(op_type: &str) -> String {
    let mut nonce = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut nonce);
    let now = chrono::Utc::now().to_rfc3339();
    let mut hasher = blake3::Hasher::new();
    hasher.update(op_type.as_bytes());
    hasher.update(now.as_bytes());
    hasher.update(&nonce);
    hasher.finalize().to_hex().to_string()
}

#[cfg(test)]
fn build_manifest_from_payload(payload_bytes: &[u8]) -> Result<(CapsuleManifest, Vec<u8>)> {
    let base_manifest = CapsuleManifest::from_toml(
        r#"
schema_version = "1"
name = "registry-artifact"
version = "0.0.0"
type = "app"
default_target = "default"

[targets.default]
runtime = "source"
entrypoint = "main"
"#,
    )?;
    manifest_payload::build_distribution_manifest(&base_manifest, payload_bytes).map_err(Into::into)
}

fn compute_manifest_hash_without_signatures(manifest: &CapsuleManifest) -> Result<String> {
    manifest_payload::compute_manifest_hash_without_signatures(manifest).map_err(Into::into)
}

fn validate_chunk_list_against_payload(
    manifest: &CapsuleManifest,
    payload_tar_bytes: &[u8],
) -> Result<()> {
    let distribution = manifest_distribution(manifest)?;
    let mut next_offset = 0u64;
    for chunk in &distribution.chunk_list {
        if chunk.offset != next_offset {
            anyhow::bail!(
                "manifest chunk offset is not contiguous at {} (expected {})",
                chunk.offset,
                next_offset
            );
        }
        let start = chunk.offset as usize;
        let end = start.saturating_add(chunk.length as usize);
        if end > payload_tar_bytes.len() {
            anyhow::bail!(
                "manifest chunk range {}..{} exceeds payload size {}",
                start,
                end,
                payload_tar_bytes.len()
            );
        }
        let chunk_bytes = &payload_tar_bytes[start..end];
        let actual_hash = format!("blake3:{}", blake3::hash(chunk_bytes).to_hex());
        if normalize_manifest_hash(&actual_hash) != normalize_manifest_hash(&chunk.chunk_hash) {
            anyhow::bail!(
                "manifest chunk hash mismatch at offset {} (expected {}, got {})",
                chunk.offset,
                chunk.chunk_hash,
                actual_hash
            );
        }
        next_offset = chunk.offset.saturating_add(chunk.length);
    }
    if next_offset != payload_tar_bytes.len() as u64 {
        anyhow::bail!(
            "manifest chunk coverage mismatch: covered {}, payload {}",
            next_offset,
            payload_tar_bytes.len()
        );
    }
    Ok(())
}

#[derive(Debug)]
struct ExtractedManifestArtifact {
    manifest: CapsuleManifest,
    manifest_document: Vec<u8>,
    payload_tar_bytes: Vec<u8>,
}

fn extract_manifest_and_payload_from_capsule(bytes: &[u8]) -> Result<ExtractedManifestArtifact> {
    let mut archive = tar::Archive::new(std::io::Cursor::new(bytes));
    let entries = archive
        .entries()
        .context("failed to iterate capsule archive entries")?;
    let mut manifest_toml = None::<String>;
    let mut payload_zst_bytes = None::<Vec<u8>>;
    for entry in entries {
        let mut entry = entry.context("invalid archive entry")?;
        let entry_path = entry
            .path()
            .context("failed to read archive entry path")?
            .to_string_lossy()
            .to_string();
        if entry_path == "capsule.toml" {
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .context("failed to read capsule.toml from archive")?;
            manifest_toml = Some(String::from_utf8(bytes).context("capsule.toml must be UTF-8")?);
        } else if entry_path == "payload.tar.zst" {
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .context("failed to read payload.tar.zst from archive")?;
            payload_zst_bytes = Some(bytes);
        }
    }

    let manifest_toml =
        manifest_toml.ok_or_else(|| anyhow::anyhow!("capsule.toml not found in artifact"))?;
    let payload_zst_bytes = payload_zst_bytes
        .ok_or_else(|| anyhow::anyhow!("payload.tar.zst not found in artifact"))?;

    let manifest: CapsuleManifest =
        toml::from_str(&manifest_toml).context("failed to parse capsule.toml")?;
    let manifest_document = manifest_toml.into_bytes();
    let expected_manifest_hash = compute_manifest_hash_without_signatures(&manifest)?;
    if normalize_manifest_hash(&expected_manifest_hash)
        != normalize_manifest_hash(&manifest_distribution(&manifest)?.manifest_hash)
    {
        anyhow::bail!(
            "capsule.toml hash mismatch (expected {}, got {})",
            expected_manifest_hash,
            manifest_distribution(&manifest)?.manifest_hash
        );
    }

    let mut decoder = zstd::stream::Decoder::new(std::io::Cursor::new(payload_zst_bytes))
        .context("failed to initialize zstd decoder")?;
    let mut payload_tar_bytes = Vec::new();
    decoder
        .read_to_end(&mut payload_tar_bytes)
        .context("failed to decode payload.tar.zst")?;

    validate_chunk_list_against_payload(&manifest, &payload_tar_bytes)?;
    let merkle_root = compute_merkle_root(
        &manifest_distribution(&manifest)?
            .chunk_list
            .iter()
            .map(|chunk| chunk.chunk_hash.as_str())
            .collect::<Vec<_>>(),
    );
    if normalize_manifest_hash(&merkle_root)
        != normalize_manifest_hash(&manifest_distribution(&manifest)?.merkle_root)
    {
        anyhow::bail!(
            "capsule.toml merkle_root mismatch (expected {}, got {})",
            manifest_distribution(&manifest)?.merkle_root,
            merkle_root
        );
    }

    Ok(ExtractedManifestArtifact {
        manifest,
        manifest_document,
        payload_tar_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::io::Write;

    fn manifest(version: &str) -> String {
        format!(
            r#"
schema_version = "0.2"
name = "sample"
version = "{}"
type = "app"
default_target = "cli"
"#,
            version
        )
    }

    fn build_capsule_bytes(manifest: &str) -> Vec<u8> {
        let payload_tar = build_payload_tar().expect("build payload tar");
        let parsed_manifest = CapsuleManifest::from_toml(manifest).expect("parse manifest");
        let (_, manifest_bytes) =
            manifest_payload::build_distribution_manifest(&parsed_manifest, &payload_tar)
                .expect("build manifest");
        let payload_zst =
            zstd::stream::encode_all(Cursor::new(payload_tar), 1).expect("encode payload");

        let mut capsule = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut capsule);

            let mut manifest_header = tar::Header::new_gnu();
            manifest_header.set_size(manifest_bytes.len() as u64);
            manifest_header.set_mode(0o644);
            manifest_header.set_mtime(0);
            manifest_header.set_cksum();
            builder
                .append_data(
                    &mut manifest_header,
                    "capsule.toml",
                    Cursor::new(manifest_bytes),
                )
                .expect("append manifest");

            let mut payload_header = tar::Header::new_gnu();
            payload_header.set_size(payload_zst.len() as u64);
            payload_header.set_mode(0o644);
            payload_header.set_mtime(0);
            payload_header.set_cksum();
            builder
                .append_data(
                    &mut payload_header,
                    "payload.tar.zst",
                    Cursor::new(payload_zst),
                )
                .expect("append payload");

            builder.finish().expect("finish capsule");
        }
        capsule
    }

    fn build_payload_tar() -> Result<Vec<u8>> {
        let mut payload = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut payload);
            let source = b"print('hello from payload')\n";
            let mut header = tar::Header::new_gnu();
            header.set_path("main.py")?;
            header.set_mode(0o644);
            header.set_size(source.len() as u64);
            header.set_mtime(0);
            header.set_cksum();
            builder.append_data(&mut header, "main.py", Cursor::new(source))?;
            builder.finish()?;
        }
        payload.flush().expect("flush payload vec");
        Ok(payload)
    }

    #[test]
    fn rollback_creates_forward_epoch_transition() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(temp.path()).expect("open store");

        let first = store
            .record_manifest_and_epoch(
                "koh0920/sample",
                &manifest("1.0.0"),
                b"payload-v1",
                "2026-03-05T00:00:00Z",
            )
            .expect("record first");
        let second = store
            .record_manifest_and_epoch(
                "koh0920/sample",
                &manifest("1.1.0"),
                b"payload-v2",
                "2026-03-05T00:00:01Z",
            )
            .expect("record second");
        assert_eq!(second.pointer.epoch, first.pointer.epoch + 1);

        let rolled = store
            .rollback_to_manifest("koh0920/sample", &first.pointer.manifest_hash)
            .expect("rollback")
            .expect("rollback response");
        assert_eq!(rolled.pointer.manifest_hash, first.pointer.manifest_hash);
        assert_eq!(rolled.pointer.epoch, second.pointer.epoch + 1);
    }

    #[test]
    fn rollback_fails_when_chunk_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(temp.path()).expect("open store");
        let first = store
            .record_manifest_and_epoch(
                "koh0920/sample",
                &manifest("1.0.0"),
                b"payload-v1",
                "2026-03-05T00:00:00Z",
            )
            .expect("record first");
        let _second = store
            .record_manifest_and_epoch(
                "koh0920/sample",
                &manifest("1.1.0"),
                b"payload-v2",
                "2026-03-05T00:00:01Z",
            )
            .expect("record second");
        let conn = store.connect().expect("connect");
        let chunk_hash: String = conn
            .query_row(
                "SELECT chunk_hash FROM manifest_chunks WHERE manifest_hash=?1 ORDER BY ordinal ASC LIMIT 1",
                params![&first.pointer.manifest_hash],
                |row| row.get(0),
            )
            .expect("chunk hash");
        let chunk_path = store.chunk_path(&normalize_blake3_hash(&chunk_hash));
        std::fs::remove_file(&chunk_path).expect("remove chunk");

        let err = store
            .rollback_to_manifest("koh0920/sample", &first.pointer.manifest_hash)
            .expect_err("rollback must fail");
        assert!(err
            .to_string()
            .contains(crate::error_codes::ATO_ERR_INTEGRITY_FAILURE));
    }

    #[test]
    fn rollback_untombstones_manifest_and_chunks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(temp.path()).expect("open store");
        let first = store
            .record_manifest_and_epoch(
                "koh0920/sample",
                &manifest("1.0.0"),
                b"payload-v1",
                "2026-03-05T00:00:00Z",
            )
            .expect("record first");
        let _second = store
            .record_manifest_and_epoch(
                "koh0920/sample",
                &manifest("1.1.0"),
                b"payload-v2",
                "2026-03-05T00:00:01Z",
            )
            .expect("record second");
        store
            .tombstone_manifest("koh0920/sample", &first.pointer.manifest_hash)
            .expect("tombstone");

        let rolled = store
            .rollback_to_manifest("koh0920/sample", &first.pointer.manifest_hash)
            .expect("rollback")
            .expect("rollback result");
        assert_eq!(rolled.pointer.manifest_hash, first.pointer.manifest_hash);

        let conn = store.connect().expect("connect");
        let manifest_tombstoned: Option<String> = conn
            .query_row(
                "SELECT tombstoned_at FROM manifests WHERE manifest_hash=?1",
                params![&first.pointer.manifest_hash],
                |row| row.get(0),
            )
            .expect("manifest tombstoned");
        assert!(manifest_tombstoned.is_none());

        let still_tombstoned: i64 = conn
            .query_row(
                "SELECT COUNT(1)
                 FROM chunks
                 WHERE chunk_hash IN (
                   SELECT chunk_hash FROM manifest_chunks WHERE manifest_hash=?1
                 )
                 AND tombstoned_at IS NOT NULL",
                params![&first.pointer.manifest_hash],
                |row| row.get(0),
            )
            .expect("chunk tombstoned count");
        assert_eq!(still_tombstoned, 0);
    }

    #[test]
    fn rollback_clears_gc_queue_for_target_chunks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(temp.path()).expect("open store");
        let first = store
            .record_manifest_and_epoch(
                "koh0920/sample",
                &manifest("1.0.0"),
                b"payload-v1",
                "2026-03-05T00:00:00Z",
            )
            .expect("record first");
        let _second = store
            .record_manifest_and_epoch(
                "koh0920/sample",
                &manifest("1.1.0"),
                b"payload-v2",
                "2026-03-05T00:00:01Z",
            )
            .expect("record second");
        store
            .tombstone_manifest("koh0920/sample", &first.pointer.manifest_hash)
            .expect("tombstone");
        store
            .enqueue_manifest_chunks_for_gc(
                &first.pointer.manifest_hash,
                "unit-test",
                &chrono::Utc::now().to_rfc3339(),
            )
            .expect("enqueue");

        let conn = store.connect().expect("connect");
        let queued_before: i64 = conn
            .query_row(
                "SELECT COUNT(1)
                 FROM gc_queue
                 WHERE chunk_hash IN (
                   SELECT chunk_hash FROM manifest_chunks WHERE manifest_hash=?1
                 )",
                params![&first.pointer.manifest_hash],
                |row| row.get(0),
            )
            .expect("queued before");
        assert!(queued_before > 0);

        store
            .rollback_to_manifest("koh0920/sample", &first.pointer.manifest_hash)
            .expect("rollback")
            .expect("rollback result");

        let queued_after: i64 = conn
            .query_row(
                "SELECT COUNT(1)
                 FROM gc_queue
                 WHERE chunk_hash IN (
                   SELECT chunk_hash FROM manifest_chunks WHERE manifest_hash=?1
                 )",
                params![&first.pointer.manifest_hash],
                |row| row.get(0),
            )
            .expect("queued after");
        assert_eq!(queued_after, 0);
    }

    #[test]
    fn rollback_rejects_yanked_manifest() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(temp.path()).expect("open store");
        let first = store
            .record_manifest_and_epoch(
                "koh0920/sample",
                &manifest("1.0.0"),
                b"payload-v1",
                "2026-03-05T00:00:00Z",
            )
            .expect("record first");
        let _second = store
            .record_manifest_and_epoch(
                "koh0920/sample",
                &manifest("1.1.0"),
                b"payload-v2",
                "2026-03-05T00:00:01Z",
            )
            .expect("record second");
        let yanked = store
            .yank_manifest("koh0920/sample", &first.pointer.manifest_hash)
            .expect("yank");
        assert!(yanked);

        let err = store
            .rollback_to_manifest("koh0920/sample", &first.pointer.manifest_hash)
            .expect_err("rollback must fail");
        assert!(err.to_string().contains("yanked"));
    }

    #[test]
    fn negotiate_rejects_unknown_manifest_history() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(temp.path()).expect("open store");
        store
            .record_manifest_and_epoch(
                "koh0920/sample",
                &manifest("1.0.0"),
                b"payload-v1",
                "2026-03-05T00:00:00Z",
            )
            .expect("record first");
        let err = store
            .negotiate(&NegotiateRequest {
                scoped_id: "koh0920/sample".to_string(),
                target_manifest_hash:
                    "blake3:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
                        .to_string(),
                have_chunks: vec![],
                have_chunks_bloom: None,
                reuse_lease_id: None,
                max_bytes: None,
            })
            .expect_err("unknown manifest must fail");
        assert!(err
            .to_string()
            .contains("target manifest is not part of scoped capsule history"));
    }

    #[test]
    fn acquire_manifest_lease_tracks_chunks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(temp.path()).expect("open store");
        let recorded = store
            .record_manifest_and_epoch(
                "koh0920/sample",
                &manifest("1.0.0"),
                b"payload-v1",
                "2026-03-05T00:00:00Z",
            )
            .expect("record");
        let lease = store
            .acquire_manifest_lease(
                "koh0920/sample",
                &recorded.pointer.manifest_hash,
                "test-owner",
                "unit-test",
                900,
            )
            .expect("acquire lease");
        assert!(lease.chunk_count >= 1);

        let conn = store.connect().expect("connect");
        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(1) FROM leases WHERE lease_id=?1",
                params![lease.lease_id],
                |row| row.get(0),
            )
            .expect("lease rows");
        assert_eq!(rows as usize, lease.chunk_count);
    }

    #[test]
    fn gc_tick_cleans_expired_leases_first() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(temp.path()).expect("open store");
        let recorded = store
            .record_manifest_and_epoch(
                "koh0920/sample",
                &manifest("1.0.0"),
                b"payload-v1",
                "2026-03-05T00:00:00Z",
            )
            .expect("record");
        let lease = store
            .acquire_manifest_lease(
                "koh0920/sample",
                &recorded.pointer.manifest_hash,
                "test-owner",
                "unit-test",
                900,
            )
            .expect("acquire lease");
        let conn = store.connect().expect("connect");
        conn.execute(
            "UPDATE leases SET expires_at='1970-01-01T00:00:00Z' WHERE lease_id=?1",
            params![lease.lease_id],
        )
        .expect("expire lease");

        let tick = store
            .gc_tick(&chrono::Utc::now().to_rfc3339(), 8)
            .expect("gc tick");
        assert!(tick.expired_leases >= 1);
    }

    #[test]
    fn gc_tick_keeps_chunks_when_live_manifest_or_lease_exists() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(temp.path()).expect("open store");
        let recorded = store
            .record_manifest_and_epoch(
                "koh0920/sample",
                &manifest("1.0.0"),
                b"payload-v1",
                "2026-03-05T00:00:00Z",
            )
            .expect("record");
        let conn = store.connect().expect("connect");
        let chunk_hash: String = conn
            .query_row(
                "SELECT chunk_hash FROM manifest_chunks WHERE manifest_hash=?1 ORDER BY ordinal ASC LIMIT 1",
                params![recorded.pointer.manifest_hash],
                |row| row.get(0),
            )
            .expect("chunk hash");
        store
            .enqueue_manifest_chunks_for_gc(
                &recorded.pointer.manifest_hash,
                "test-live-ref",
                &chrono::Utc::now().to_rfc3339(),
            )
            .expect("enqueue");

        let tick = store
            .gc_tick(&chrono::Utc::now().to_rfc3339(), 8)
            .expect("gc tick");
        assert!(tick.deferred >= 1);
        assert!(store
            .load_chunk_bytes(&chunk_hash)
            .expect("load chunk")
            .is_some());
    }

    #[test]
    fn gc_tick_keeps_retention_pinned_release_chunks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(temp.path()).expect("open store");
        let publisher = "koh0920";
        let slug = "sample";
        let name = "sample";
        let description = "sample app";

        let mut releases = Vec::new();
        for idx in 0..6 {
            let version = format!("1.0.{}", idx);
            let capsule = build_capsule_bytes(&manifest(&version));
            let record = store
                .publish_registry_release(
                    publisher,
                    slug,
                    name,
                    description,
                    &version,
                    &format!("sample-{}.capsule", version),
                    "sha256:abc",
                    &format!("blake3:{:064x}", idx + 1),
                    capsule.len() as u64,
                    &capsule,
                    &format!("2026-03-05T00:00:0{}Z", idx),
                )
                .expect("publish release");
            releases.push((version, record.pointer.manifest_hash));
        }

        let pinned_manifest_hash = releases[1].1.clone();
        store
            .tombstone_manifest("koh0920/sample", &pinned_manifest_hash)
            .expect("tombstone");
        store
            .enqueue_manifest_chunks_for_gc(
                &pinned_manifest_hash,
                "test-retention",
                &chrono::Utc::now().to_rfc3339(),
            )
            .expect("enqueue");

        let conn = store.connect().expect("connect");
        let chunk_hash: String = conn
            .query_row(
                "SELECT chunk_hash FROM manifest_chunks WHERE manifest_hash=?1 ORDER BY ordinal ASC LIMIT 1",
                params![&pinned_manifest_hash],
                |row| row.get(0),
            )
            .expect("chunk hash");

        let tick = store
            .gc_tick(&chrono::Utc::now().to_rfc3339(), 8)
            .expect("gc tick");
        assert!(tick.deferred >= 1);
        assert!(store
            .load_chunk_bytes(&chunk_hash)
            .expect("load chunk")
            .is_some());
    }

    #[test]
    fn gc_tick_unlinks_and_reflects_db_for_eligible_chunks() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(temp.path()).expect("open store");
        let recorded = store
            .record_manifest_and_epoch(
                "koh0920/sample",
                &manifest("1.0.0"),
                b"payload-v1",
                "2026-03-05T00:00:00Z",
            )
            .expect("record");
        let conn = store.connect().expect("connect");
        let chunk_hash: String = conn
            .query_row(
                "SELECT chunk_hash FROM manifest_chunks WHERE manifest_hash=?1 ORDER BY ordinal ASC LIMIT 1",
                params![recorded.pointer.manifest_hash],
                |row| row.get(0),
            )
            .expect("chunk hash");
        store
            .tombstone_manifest("koh0920/sample", &recorded.pointer.manifest_hash)
            .expect("tombstone manifest");
        store
            .enqueue_manifest_chunks_for_gc(
                &recorded.pointer.manifest_hash,
                "test-delete",
                &chrono::Utc::now().to_rfc3339(),
            )
            .expect("enqueue");

        let tick = store
            .gc_tick(&chrono::Utc::now().to_rfc3339(), 8)
            .expect("gc tick");
        assert!(tick.deleted >= 1);
        assert!(store
            .load_chunk_bytes(&chunk_hash)
            .expect("load chunk")
            .is_none());

        let remaining_chunks: i64 = conn
            .query_row(
                "SELECT COUNT(1) FROM chunks WHERE chunk_hash=?1",
                params![chunk_hash],
                |row| row.get(0),
            )
            .expect("remaining chunks");
        assert_eq!(remaining_chunks, 0);
    }

    #[test]
    fn gc_related_queries_use_indexes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(temp.path()).expect("open store");
        let conn = store.connect().expect("connect");

        let queue_plan = {
            let mut stmt = conn
                .prepare(
                    "EXPLAIN QUERY PLAN
                     SELECT chunk_hash
                     FROM gc_queue
                     WHERE state IN ('pending', 'deferred', 'failed')
                       AND not_before <= ?1
                     ORDER BY not_before ASC
                     LIMIT ?2",
                )
                .expect("prepare queue plan");
            let rows = stmt
                .query_map(params!["2026-03-05T00:00:00Z", 8], |row| {
                    row.get::<_, String>(3)
                })
                .expect("query queue plan");
            let mut lines = Vec::new();
            for row in rows {
                lines.push(row.expect("plan row"));
            }
            lines.join("\n")
        };
        assert!(
            queue_plan.contains("idx_gc_queue_state_not_before")
                || queue_plan.contains("USING INDEX"),
            "unexpected queue plan: {}",
            queue_plan
        );

        let lease_plan = {
            let mut stmt = conn
                .prepare(
                    "EXPLAIN QUERY PLAN
                     SELECT 1 FROM leases WHERE chunk_hash=?1 AND expires_at > ?2 LIMIT 1",
                )
                .expect("prepare lease plan");
            let rows = stmt
                .query_map(params!["blake3:deadbeef", "2026-03-05T00:00:00Z"], |row| {
                    row.get::<_, String>(3)
                })
                .expect("query lease plan");
            let mut lines = Vec::new();
            for row in rows {
                lines.push(row.expect("plan row"));
            }
            lines.join("\n")
        };
        assert!(
            lease_plan.contains("idx_leases_chunk_expires") || lease_plan.contains("USING INDEX"),
            "unexpected lease plan: {}",
            lease_plan
        );
    }

    #[test]
    fn registry_store_fresh_db_creates_persistent_state_columns_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(temp.path()).expect("open store");
        let conn = store.connect().expect("connect");

        let mut stmt = conn
            .prepare("PRAGMA table_info(persistent_states)")
            .expect("prepare table info");
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("query table info")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect columns");

        assert_eq!(
            columns,
            vec![
                "state_id",
                "owner_scope",
                "state_name",
                "kind",
                "backend_kind",
                "backend_locator",
                "producer",
                "purpose",
                "schema_id",
                "created_at",
                "updated_at",
            ]
        );
    }

    #[test]
    fn persistent_state_registry_round_trips() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(temp.path()).expect("open store");
        let record = store
            .register_persistent_state(&NewPersistentStateRecord {
                owner_scope: "demo-app".to_string(),
                state_name: "data".to_string(),
                kind: "filesystem".to_string(),
                backend_kind: "host_path".to_string(),
                backend_locator: "/var/lib/ato/persistent/demo-app/data".to_string(),
                producer: "demo-app".to_string(),
                purpose: "primary-data".to_string(),
                schema_id:
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                        .to_string(),
            })
            .expect("register");
        assert!(record.state_id.starts_with("state-"));

        let fetched = store
            .find_persistent_state_by_owner_and_locator(
                "demo-app",
                "/var/lib/ato/persistent/demo-app/data",
            )
            .expect("lookup")
            .expect("record");
        assert_eq!(fetched, record);

        let by_id = store
            .find_persistent_state_by_id(&record.state_id)
            .expect("lookup by id")
            .expect("record by id");
        assert_eq!(by_id, record);

        let listed = store
            .list_persistent_states(Some("demo-app"), Some("data"))
            .expect("list states");
        assert_eq!(listed, vec![record]);
    }

    #[test]
    fn service_binding_registry_round_trips_and_updates() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(temp.path()).expect("open store");
        let record = store
            .register_service_binding(&NewServiceBindingRecord {
                owner_scope: "demo-app".to_string(),
                service_name: "main".to_string(),
                binding_kind: "ingress".to_string(),
                transport_kind: "https".to_string(),
                adapter_kind: "reverse_proxy".to_string(),
                endpoint_locator: "https://demo.local/".to_string(),
                tls_mode: "explicit".to_string(),
                allowed_callers: vec!["web".to_string(), "worker".to_string()],
                target_hint: Some("app".to_string()),
            })
            .expect("register binding");
        assert!(record.binding_id.starts_with("binding-"));
        assert_eq!(record.allowed_callers, vec!["web", "worker"]);

        let fetched = store
            .find_service_binding_by_identity("demo-app", "main", "ingress")
            .expect("lookup binding")
            .expect("binding record");
        assert_eq!(fetched, record);

        let by_id = store
            .find_service_binding_by_id(&record.binding_id)
            .expect("lookup binding by id")
            .expect("binding record by id");
        assert_eq!(by_id, record);

        let updated = store
            .register_service_binding(&NewServiceBindingRecord {
                owner_scope: "demo-app".to_string(),
                service_name: "main".to_string(),
                binding_kind: "ingress".to_string(),
                transport_kind: "http".to_string(),
                adapter_kind: "reverse_proxy".to_string(),
                endpoint_locator: "http://127.0.0.1:4310/".to_string(),
                tls_mode: "disabled".to_string(),
                allowed_callers: vec!["worker".to_string()],
                target_hint: Some("app".to_string()),
            })
            .expect("update binding");
        assert_eq!(updated.binding_id, record.binding_id);
        assert_eq!(updated.endpoint_locator, "http://127.0.0.1:4310/");
        assert_eq!(updated.tls_mode, "disabled");
        assert_eq!(updated.allowed_callers, vec!["worker"]);

        let listed = store
            .list_service_bindings(Some("demo-app"), Some("main"))
            .expect("list bindings");
        assert_eq!(listed, vec![updated]);
    }

    #[test]
    fn service_binding_resolution_enforces_allowed_callers() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(temp.path()).expect("open store");
        let record = store
            .register_service_binding(&NewServiceBindingRecord {
                owner_scope: "demo-app".to_string(),
                service_name: "api".to_string(),
                binding_kind: "service".to_string(),
                transport_kind: "http".to_string(),
                adapter_kind: "reverse_proxy".to_string(),
                endpoint_locator: "http://127.0.0.1:4310/".to_string(),
                tls_mode: "disabled".to_string(),
                allowed_callers: vec!["web".to_string()],
                target_hint: Some("app".to_string()),
            })
            .expect("register binding");

        let resolved = store
            .resolve_service_binding("demo-app", "api", "service", Some("web"))
            .expect("resolve binding")
            .expect("resolved record");
        assert_eq!(resolved, record);

        let missing_caller = store
            .resolve_service_binding("demo-app", "api", "service", None)
            .expect_err("caller is required for restricted bindings");
        assert!(missing_caller
            .to_string()
            .contains("requires caller_service"));

        let denied = store
            .resolve_service_binding("demo-app", "api", "service", Some("worker"))
            .expect_err("unauthorized caller must fail");
        assert!(denied.to_string().contains("not allowed"));
    }

    #[test]
    fn delete_service_binding_by_identity_removes_record() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(temp.path()).expect("open store");
        let record = store
            .register_service_binding(&NewServiceBindingRecord {
                owner_scope: "demo-app".to_string(),
                service_name: "api".to_string(),
                binding_kind: "service".to_string(),
                transport_kind: "http".to_string(),
                adapter_kind: "local_service".to_string(),
                endpoint_locator: "http://127.0.0.1:4310/".to_string(),
                tls_mode: "disabled".to_string(),
                allowed_callers: vec!["web".to_string()],
                target_hint: Some("app".to_string()),
            })
            .expect("register binding");

        let deleted = store
            .delete_service_binding_by_identity("demo-app", "api", "service")
            .expect("delete binding")
            .expect("deleted record");
        assert_eq!(deleted, record);

        let remaining = store
            .find_service_binding_by_identity("demo-app", "api", "service")
            .expect("lookup binding after delete");
        assert!(remaining.is_none());
    }

    #[test]
    fn revoke_key_requires_did_when_key_id_collides() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(temp.path()).expect("open store");
        let identity = store.ensure_signing_identity().expect("identity");
        let conn = store.connect().expect("connect");
        conn.execute(
            "INSERT OR REPLACE INTO trusted_keys(did, key_id, public_key, valid_from, valid_to, revoked_at)
             VALUES (?1, ?2, ?3, ?4, NULL, NULL)",
            params![
                "did:key:zcollision",
                identity.key_id,
                BASE64.encode([7u8; 32]),
                chrono::Utc::now().to_rfc3339()
            ],
        )
        .expect("insert collision key");

        let err = store
            .revoke_key(&identity.key_id, None)
            .expect_err("collision must require did");
        assert!(err.to_string().contains("specify --did"));
        assert!(err.to_string().contains("did:key:zcollision"));
    }

    #[test]
    fn negotiate_reuses_lease_id_on_retry() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = RegistryStore::open(temp.path()).expect("open store");
        let recorded = store
            .record_manifest_and_epoch(
                "koh0920/sample",
                &manifest("1.0.0"),
                b"payload-v1",
                "2026-03-05T00:00:00Z",
            )
            .expect("record");

        let first = store
            .negotiate(&NegotiateRequest {
                scoped_id: "koh0920/sample".to_string(),
                target_manifest_hash: recorded.pointer.manifest_hash.clone(),
                have_chunks: vec![],
                have_chunks_bloom: Some(ChunkBloomFilterRequest {
                    m_bits: 8,
                    k_hashes: 1,
                    seed: 7,
                    bitset_base64: BASE64.encode([0xffu8]),
                }),
                reuse_lease_id: None,
                max_bytes: None,
            })
            .expect("first negotiate");
        assert!(first.required_chunks.is_empty());
        let lease_id = first.lease_id.clone().expect("lease_id");

        let second = store
            .negotiate(&NegotiateRequest {
                scoped_id: "koh0920/sample".to_string(),
                target_manifest_hash: recorded.pointer.manifest_hash.clone(),
                have_chunks: vec![],
                have_chunks_bloom: None,
                reuse_lease_id: Some(lease_id.clone()),
                max_bytes: None,
            })
            .expect("second negotiate");
        assert_eq!(second.lease_id.as_deref(), Some(lease_id.as_str()));
        assert!(!second.required_chunks.is_empty());
    }
}
