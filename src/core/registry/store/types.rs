use capsule_core::types::EpochPointer;
use serde::{Deserialize, Serialize};

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
