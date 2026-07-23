//! `ato.execution-contract/v1` — the resolved Execution Identity contract.
//!
//! Identity-bearing and non-identity data are separated at the type level:
//!
//! * [`ExecutionContractV1`] (and its facet structs) is the identity-bearing
//!   contract. Every field participates in `execution_id`, deserialization is
//!   fail-closed (`deny_unknown_fields`), and all lists must already be in
//!   canonical (sorted, duplicate-free) order. Absent optional fields have
//!   exactly one canonical spelling — the key is omitted; explicit `null` and
//!   explicit-empty optional collections (`{}` / `[]`) are rejected so that a
//!   consumer canonicalizing the raw JSON can never hash a different form
//!   than the typed layer.
//! * [`ExecutionContractEnvelopeV1`] is the non-identity envelope around a
//!   contract: provenance, diagnostics, evidence, timestamps, and the stored
//!   `execution_id`. It is tolerant of unknown fields by design — nothing in
//!   the envelope besides the embedded contract may influence the id, and
//!   [`ExecutionContractEnvelopeV1::verify`] re-derives the id fail-closed.
//!
//! Canonical form (normative):
//!
//! ```text
//! execution_id = "blake3:" + hex(BLAKE3(UTF8("ato.execution-contract/v1") || 0x00 || JCS(contract)))
//! ```
//!
//! # RFC §4.2 facet mapping (normative)
//!
//! Every row of the RFC's §4.2 identity-bearing facet table maps to an
//! explicit field of [`ExecutionContractV1`]. There are no transitive-only or
//! "pinned by the schema string" rows: each required facet content has a
//! dedicated identity-bearing field whose mutation changes `execution_id`,
//! pinned by a `mutate-*` shared vector. Two field shapes appear:
//!
//! * **typed** — the resolved content is carried by concrete typed fields
//!   whose shape is fixed by v1 (`source.digest`, `target.*`, `launch.argv`,
//!   `guest_surface.*`, `external_state[]`, …).
//! * **opaque sub-contract digest** — a [`ContentDigest`] field commits a
//!   sub-contract whose *payload schema is versioned separately from v1*. The
//!   digest is inside the v1 identity set (mutating it changes
//!   `execution_id`), but the bytes a producer hashes into it are defined by a
//!   sub-contract that MAY gain structure in a later revision without touching
//!   the v1 identity set. This is the RFC §4.5 layering — a digest pins
//!   identity while its payload schema is versioned separately — and matches
//!   the pre-existing `policy.network_digest`, `policy.capability_digest`, and
//!   `policy.filesystem_digest` fields. The opaque `*_digest` facet fields are
//!   `source.projection_digest`, `runtime.dynamic_contract_digest`,
//!   `build_outputs[].projection_digest`, `launch.process_model_digest`,
//!   `launch.environment_policy_digest`, `filesystem.topology_digest`, and the
//!   three `policy.*` digests.
//!
//! | RFC §4.2 facet | Required identity-bearing content | v1 field(s) |
//! |---|---|---|
//! | Source | materialized source digest; source projection rules | `source.digest`; `source.projection_digest` (opaque) |
//! | Target | OS, arch, ABI/libc, observable target features | `target.os` / `target.architecture` / `target.abi` / `target.libc` / `target.observable_features` |
//! | Runtime | resolved runtime artifact; dynamic runtime contract | `runtime.kind` / `runtime.resolved_ref` / `runtime.digest`; `runtime.dynamic_contract_digest` (opaque) |
//! | Dependencies | derivation identity; immutable output identity | `dependencies[].name` / `dependencies[].derivation_digest` / `dependencies[].output_digest` |
//! | Build outputs | immutable output digest; projection | `build_outputs[].name` / `build_outputs[].digest`; `build_outputs[].projection_digest` (opaque) |
//! | Launch | entrypoint, exact argv, cwd; process model | `launch.argv` / `launch.cwd`; `launch.process_model_digest` (opaque) |
//! | Environment | non-secret values; variable requirements, normalization, inheritance policy | `launch.environment[].name` / `launch.environment[].value_digest`; `launch.environment_policy_digest` (opaque) |
//! | Filesystem | immutable layers; mount topology; access modes; writable-boundary contracts | `filesystem.readonly_layers`; `filesystem.topology_digest` (opaque: mount topology + per-mount access modes); `filesystem.writable_paths`; `filesystem.view_digest` (immutable view content) |
//! | Network | ingress, egress, DNS, isolation policy | `policy.network_digest` (opaque) |
//! | Capabilities | filesystem, host, device, sandbox policy | `policy.capability_digest` / `policy.filesystem_digest` (opaque) |
//! | Surface | declared guest bind address, protocol, guest port | `guest_surface.bind_address` / `guest_surface.protocol` / `guest_surface.port` / `guest_surface.features` |
//! | External State schema | binding name, mount/injection target, access mode, schema identity, Snapshot exclusion | `external_state[].name` / `external_state[].target` / `external_state[].access` / `external_state[].schema` / `external_state[].snapshot` |
//!
//! Secret *values* are never identity-bearing (RFC §4.3): secrets are bound by
//! name via `launch.secret_bindings` and their values never enter the
//! contract. `launch.environment_policy_digest` commits the variable
//! requirements / normalization / inheritance *policy*; the resolved
//! non-secret values are committed per-variable by
//! `launch.environment[].value_digest`.
//!
//! An execution whose launch conditions cannot be expressed under this mapping
//! MUST NOT be issued a v1 `execution_id`. Per RFC §4.5, any change to the
//! mapping — adding or removing a field, or moving a facet's commitment — is a
//! semantic change to the identity-bearing field set and requires a new
//! contract version. Defining or evolving the payload schema *behind* an
//! opaque `*_digest` is NOT such a change: the v1 identity set already commits
//! the digest, so the sub-contract can be versioned independently.
//!
//! This contract is `ato.execution-contract/v1` — the sole exact Capsule v1
//! execution identity (RFC §4.1, §13). It supersedes the archived
//! Snapshot-layer-derived `execution_id` model (RFC §16.2): runtime,
//! dependency, and build-output digests remain identity-bearing, Snapshot
//! memory/vmstate/disk layer IDs are excluded, and `execution_id` is finalized
//! before Snapshot capture. The `blake3:` hash prefix is not a schema
//! discriminator (RFC §7.7); the schema string / domain separator
//! `ato.execution-contract/v1` is.
//!
//! The normative spec is
//! `docs/rfcs/accepted/CAPSULE_V1_EXECUTION_MODEL_SPEC.md` (§4.2 facet table,
//! §4.5 canonicalization), whose front-matter names this file as SSOT. It is
//! landing on `nightly` via ato-run/ato#1098 (tracking issue #1086); until it
//! is in this tree, this module and the shared vectors below are the normative
//! definition of the canonical form. When the spec lands, its §4.2 table and
//! this mapping must stay row-for-row identical; a divergence on any row is a
//! §4.5 version bump, not a doc-only fix.
//!
//! Shared cross-language test vectors live in
//! `crates/capsule/tests/fixtures/execution_contract/` and are exercised by
//! `crates/capsule/tests/execution_contract_vectors.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const EXECUTION_CONTRACT_V1_SCHEMA: &str = "ato.execution-contract/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DigestAlgorithm {
    Blake3,
    Sha256,
}

impl DigestAlgorithm {
    fn as_str(self) -> &'static str {
        match self {
            Self::Blake3 => "blake3",
            Self::Sha256 => "sha256",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContentDigest {
    algorithm: DigestAlgorithm,
    bytes: [u8; 32],
}

impl ContentDigest {
    pub fn new(algorithm: DigestAlgorithm, bytes: [u8; 32]) -> Self {
        Self { algorithm, bytes }
    }

    pub fn algorithm(self) -> DigestAlgorithm {
        self.algorithm
    }

    pub fn bytes(self) -> [u8; 32] {
        self.bytes
    }
}

impl fmt::Display for ContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}",
            self.algorithm.as_str(),
            hex::encode(self.bytes)
        )
    }
}

impl TryFrom<String> for ContentDigest {
    type Error = ExecutionContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let (algorithm, encoded) = value
            .split_once(':')
            .ok_or(ExecutionContractError::InvalidContentDigest)?;
        let algorithm = match algorithm {
            "blake3" => DigestAlgorithm::Blake3,
            "sha256" => DigestAlgorithm::Sha256,
            _ => return Err(ExecutionContractError::InvalidContentDigest),
        };
        if encoded.len() != 64
            || !encoded
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ExecutionContractError::InvalidContentDigest);
        }
        let decoded =
            hex::decode(encoded).map_err(|_| ExecutionContractError::InvalidContentDigest)?;
        let bytes = decoded
            .try_into()
            .map_err(|_| ExecutionContractError::InvalidContentDigest)?;
        Ok(Self { algorithm, bytes })
    }
}

impl From<ContentDigest> for String {
    fn from(value: ContentDigest) -> Self {
        value.to_string()
    }
}

impl Serialize for ContentDigest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ContentDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_from(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ExecutionId(String);

impl ExecutionId {
    pub fn new(value: String) -> Result<Self, ExecutionContractError> {
        let Some(hex) = value.strip_prefix("blake3:") else {
            return Err(ExecutionContractError::InvalidExecutionId);
        };
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ExecutionContractError::InvalidExecutionId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ExecutionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for ExecutionId {
    type Error = ExecutionContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ExecutionId> for String {
    fn from(value: ExecutionId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExecutionContractError {
    #[error("execution contract schema must be ato.execution-contract/v1")]
    InvalidSchema,
    #[error("execution contract field '{0}' must be resolved and non-empty")]
    UnresolvedField(&'static str),
    #[error("execution contract list '{0}' must be sorted and contain no duplicates")]
    NonCanonicalList(&'static str),
    #[error("execution_id must be blake3:<64 lowercase hex characters>")]
    InvalidExecutionId,
    #[error("content digest must use blake3 or sha256 with exactly 64 lowercase hex characters")]
    InvalidContentDigest,
    #[error("observed launch envelope does not exactly match the stored execution contract")]
    ObservedContractMismatch,
    #[error("stored execution_id {stored} does not match the canonical hash {computed}")]
    ExecutionIdMismatch { stored: String, computed: String },
    #[error("failed to canonicalize execution contract: {0}")]
    Canonicalization(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionContractV1 {
    pub schema: String,
    pub source: ResolvedSourceContract,
    pub target: ResolvedTargetContract,
    pub runtime: ResolvedArtifactContract,
    pub dependencies: Vec<ResolvedDependencyContract>,
    pub build_outputs: Vec<ResolvedBuildOutputContract>,
    pub launch: ResolvedLaunchContract,
    pub filesystem: ResolvedFilesystemContract,
    pub policy: ResolvedPolicyContract,
    pub guest_surface: GuestSurfaceContract,
    pub external_state: Vec<ExternalStateContract>,
}

/// Resolved source facet. `digest` is the materialized source projection
/// (RFC §4.6) — the source bytes actually presented to the build.
/// `projection_digest` is the opaque sub-contract digest committing the
/// *source projection rules* (include/exclude, symlink and case policy, …);
/// its payload schema is versioned separately from v1 (see the module-level
/// facet mapping).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedSourceContract {
    pub kind: String,
    pub immutable_ref: String,
    pub digest: ContentDigest,
    pub projection_digest: ContentDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedTargetContract {
    pub os: String,
    pub architecture: String,
    pub abi: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null"
    )]
    pub libc: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "present_non_empty_map"
    )]
    pub observable_features: BTreeMap<String, String>,
}

/// Resolved runtime artifact facet. `digest` is the resolved runtime artifact
/// digest. `dynamic_contract_digest` is the opaque sub-contract digest
/// committing the *dynamic runtime contract* — runtime-provided launch-time
/// behaviour beyond the static artifact bytes (dynamic linking / loader
/// resolution, plugin or module surface, JIT/ABI switches); its payload schema
/// is versioned separately from v1 (see the module-level facet mapping).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedArtifactContract {
    pub kind: String,
    pub resolved_ref: String,
    pub digest: ContentDigest,
    pub dynamic_contract_digest: ContentDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedDependencyContract {
    pub name: String,
    pub derivation_digest: ContentDigest,
    pub output_digest: ContentDigest,
}

/// A single actual immutable build output. `digest` is its immutable output
/// digest; `projection_digest` is the opaque sub-contract digest committing
/// how this output is *projected* into the launch environment (placement,
/// rename, permission projection), with a payload schema versioned separately
/// from v1 (see the module-level facet mapping).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedBuildOutputContract {
    pub name: String,
    pub digest: ContentDigest,
    pub projection_digest: ContentDigest,
}

/// Resolved launch facet. `argv` / `cwd` are the exact entrypoint.
/// `process_model_digest` is the opaque sub-contract digest committing the
/// *process model* (root process, daemonization/PID-1 role, supervised child
/// structure); its payload schema is versioned separately from v1.
/// `environment` carries the resolved non-secret values, and
/// `environment_policy_digest` is the opaque sub-contract digest committing
/// the *variable requirements, normalization, and inheritance policy*. Secret
/// values never enter the contract: secrets are bound by name in
/// `secret_bindings` and their values are excluded from identity (RFC §4.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedLaunchContract {
    pub argv: Vec<String>,
    pub cwd: String,
    pub process_model_digest: ContentDigest,
    pub environment: Vec<EnvironmentVariableContract>,
    pub environment_policy_digest: ContentDigest,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_string_list"
    )]
    pub secret_bindings: Vec<String>,
}

/// One resolved non-secret environment variable. `value_digest` is the
/// content digest of the exact resolved value bytes; secret values never
/// appear here — secrets are bound by name via
/// `ResolvedLaunchContract::secret_bindings` and their values are excluded
/// from identity (RFC §4.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentVariableContract {
    pub name: String,
    pub value_digest: ContentDigest,
}

/// Resolved filesystem facet. `readonly_layers` are the immutable layer
/// digests; `writable_paths` is the writable-boundary contract; `view_digest`
/// commits the composed immutable view *content*. `topology_digest` is the
/// opaque sub-contract digest committing the *mount topology and per-mount
/// access modes* (which layer/output is mounted where, and read-only vs
/// read-write per mount) — content and structure are separate commitments, so
/// a topology or access-mode change is identity-bearing even when the mounted
/// bytes are unchanged. Its payload schema is versioned separately from v1
/// (see the module-level facet mapping).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedFilesystemContract {
    pub view_digest: ContentDigest,
    pub topology_digest: ContentDigest,
    pub readonly_layers: Vec<ContentDigest>,
    pub writable_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedPolicyContract {
    pub network_digest: ContentDigest,
    pub capability_digest: ContentDigest,
    pub filesystem_digest: ContentDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestSurfaceContract {
    pub bind_address: String,
    pub protocol: String,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null"
    )]
    pub port: Option<u16>,
    pub features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalStateContract {
    pub name: String,
    pub target: String,
    pub access: ExternalStateAccess,
    pub schema: String,
    pub snapshot: SnapshotExclusion,
}

/// Complete launch contract independently re-derived from the concrete plan,
/// manifest, target, and materialization that are about to be snapshotted.
/// Partial digest witnesses are intentionally not representable: acceptance
/// requires equality across every identity-bearing facet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedExecutionContractV1 {
    pub contract: ExecutionContractV1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizedExecutionContractV1 {
    contract: ExecutionContractV1,
    execution_id: ExecutionId,
}

impl FinalizedExecutionContractV1 {
    pub fn contract(&self) -> &ExecutionContractV1 {
        &self.contract
    }

    pub fn execution_id(&self) -> &ExecutionId {
        &self.execution_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExternalStateAccess {
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotExclusion {
    Exclude,
}

/// Non-identity envelope around an execution contract.
///
/// Everything here besides `execution_contract` is excluded from the
/// execution identity: provenance, diagnostics, evidence, timestamps, and
/// the stored `execution_id` itself. Unlike the identity-bearing contract,
/// the envelope is deliberately tolerant — unknown fields (runner names,
/// Snapshot/Session IDs, dynamic endpoints, and other operational facts)
/// are ignored on read instead of failing closed, because none of them may
/// influence the id. [`Self::verify`] recomputes the canonical hash from
/// the embedded contract and fails closed on any mismatch with the stored
/// `execution_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionContractEnvelopeV1 {
    pub execution_contract: ExecutionContractV1,
    pub execution_id: ExecutionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub provenance: serde_json::Value,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub diagnostics: serde_json::Value,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub evidence: serde_json::Value,
}

impl ExecutionContractEnvelopeV1 {
    /// Verify that the stored `execution_id` matches the canonical hash of
    /// the embedded identity-bearing contract. A mismatch is terminal for
    /// the reader: the envelope must not be trusted or republished.
    pub fn verify(&self) -> Result<(), ExecutionContractError> {
        let computed = self.execution_contract.compute_execution_id()?;
        if computed != self.execution_id {
            return Err(ExecutionContractError::ExecutionIdMismatch {
                stored: self.execution_id.to_string(),
                computed: computed.to_string(),
            });
        }
        Ok(())
    }
}

impl ExecutionContractV1 {
    pub fn validate(&self) -> Result<(), ExecutionContractError> {
        if self.schema != EXECUTION_CONTRACT_V1_SCHEMA {
            return Err(ExecutionContractError::InvalidSchema);
        }

        for (field, value) in [
            ("source.kind", self.source.kind.as_str()),
            ("source.immutable_ref", self.source.immutable_ref.as_str()),
            ("target.os", self.target.os.as_str()),
            ("target.architecture", self.target.architecture.as_str()),
            ("target.abi", self.target.abi.as_str()),
            ("runtime.kind", self.runtime.kind.as_str()),
            ("runtime.resolved_ref", self.runtime.resolved_ref.as_str()),
            ("launch.cwd", self.launch.cwd.as_str()),
            (
                "guest_surface.bind_address",
                self.guest_surface.bind_address.as_str(),
            ),
            (
                "guest_surface.protocol",
                self.guest_surface.protocol.as_str(),
            ),
        ] {
            if value.trim().is_empty() {
                return Err(ExecutionContractError::UnresolvedField(field));
            }
        }
        for (field, value) in [
            ("source.kind", self.source.kind.as_str()),
            ("target.os", self.target.os.as_str()),
            ("target.architecture", self.target.architecture.as_str()),
            ("target.abi", self.target.abi.as_str()),
            ("runtime.kind", self.runtime.kind.as_str()),
            (
                "guest_surface.protocol",
                self.guest_surface.protocol.as_str(),
            ),
        ] {
            validate_ascii_identifier(field, value)?;
        }
        if self.launch.argv.is_empty()
            || self.launch.argv.iter().any(|value| value.trim().is_empty())
        {
            return Err(ExecutionContractError::UnresolvedField("launch.argv"));
        }
        if let Some(libc) = &self.target.libc {
            validate_ascii_identifier("target.libc", libc)?;
        }
        if self
            .target
            .observable_features
            .iter()
            .any(|(name, value)| !is_ascii_identifier(name) || value.trim().is_empty())
        {
            return Err(ExecutionContractError::UnresolvedField(
                "target.observable_features",
            ));
        }

        validate_named_list(
            "dependencies",
            self.dependencies.iter().map(|item| item.name.as_str()),
        )?;
        validate_named_list(
            "build_outputs",
            self.build_outputs.iter().map(|item| item.name.as_str()),
        )?;
        validate_named_list(
            "launch.environment",
            self.launch
                .environment
                .iter()
                .map(|item| item.name.as_str()),
        )?;
        validate_sorted_identifiers("launch.secret_bindings", &self.launch.secret_bindings)?;
        validate_sorted_digests(
            "filesystem.readonly_layers",
            &self.filesystem.readonly_layers,
        )?;
        validate_sorted_ascii_strings(
            "filesystem.writable_paths",
            &self.filesystem.writable_paths,
        )?;
        validate_sorted_identifiers("guest_surface.features", &self.guest_surface.features)?;
        validate_named_list(
            "external_state",
            self.external_state.iter().map(|item| item.name.as_str()),
        )?;

        for dependency in &self.dependencies {
            ensure_values("dependencies", [&dependency.name])?;
        }
        for output in &self.build_outputs {
            ensure_values("build_outputs", [&output.name])?;
        }
        for variable in &self.launch.environment {
            ensure_values("launch.environment", [&variable.name])?;
        }
        for state in &self.external_state {
            ensure_values(
                "external_state",
                [&state.name, &state.target, &state.schema],
            )?;
        }
        Ok(())
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ExecutionContractError> {
        self.validate()?;
        serde_jcs::to_vec(self)
            .map_err(|error| ExecutionContractError::Canonicalization(error.to_string()))
    }

    pub fn compute_execution_id(&self) -> Result<ExecutionId, ExecutionContractError> {
        let canonical = self.canonical_bytes()?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(EXECUTION_CONTRACT_V1_SCHEMA.as_bytes());
        hasher.update(&[0]);
        hasher.update(&canonical);
        ExecutionId::new(format!("blake3:{}", hasher.finalize().to_hex()))
    }

    /// Verify that the resolved contract exactly equals the contract re-derived
    /// from this build's actual launch plan and materialized bytes.
    ///
    /// A mismatch is terminal: callers must not capture or publish a Snapshot
    /// under the stale execution identity.
    pub fn verify_observation(
        &self,
        observed: &ObservedExecutionContractV1,
    ) -> Result<(), ExecutionContractError> {
        self.validate()?;
        observed.contract.validate()?;
        if self == &observed.contract {
            Ok(())
        } else {
            Err(ExecutionContractError::ObservedContractMismatch)
        }
    }

    pub fn finalize_observation(
        self,
        observed: &ObservedExecutionContractV1,
    ) -> Result<FinalizedExecutionContractV1, ExecutionContractError> {
        self.verify_observation(observed)?;
        let execution_id = self.compute_execution_id()?;
        Ok(FinalizedExecutionContractV1 {
            contract: self,
            execution_id,
        })
    }
}

fn validate_sorted_digests(
    field: &'static str,
    values: &[ContentDigest],
) -> Result<(), ExecutionContractError> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(ExecutionContractError::NonCanonicalList(field));
    }
    Ok(())
}

fn validate_named_list<'a>(
    field: &'static str,
    values: impl Iterator<Item = &'a str>,
) -> Result<(), ExecutionContractError> {
    let values = values.collect::<Vec<_>>();
    if values.iter().any(|value| !is_ascii_identifier(value))
        || values.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(ExecutionContractError::NonCanonicalList(field));
    }
    Ok(())
}

fn validate_sorted_identifiers(
    field: &'static str,
    values: &[String],
) -> Result<(), ExecutionContractError> {
    let unique = values.iter().collect::<BTreeSet<_>>();
    if unique.len() != values.len()
        || values.iter().any(|value| !is_ascii_identifier(value))
        || values.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(ExecutionContractError::NonCanonicalList(field));
    }
    Ok(())
}

fn validate_sorted_ascii_strings(
    field: &'static str,
    values: &[String],
) -> Result<(), ExecutionContractError> {
    if values.iter().any(|value| {
        value.trim().is_empty()
            || !value
                .bytes()
                .all(|byte| byte == b' ' || byte.is_ascii_graphic())
    }) || values.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(ExecutionContractError::NonCanonicalList(field));
    }
    Ok(())
}

fn validate_ascii_identifier(
    field: &'static str,
    value: &str,
) -> Result<(), ExecutionContractError> {
    if !is_ascii_identifier(value) {
        return Err(ExecutionContractError::UnresolvedField(field));
    }
    Ok(())
}

fn is_ascii_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric()
                || byte == b'_'
                || (index > 0 && b"._:/@+-".contains(&byte))
        })
}

fn ensure_values<const N: usize>(
    field: &'static str,
    values: [&String; N],
) -> Result<(), ExecutionContractError> {
    if values.iter().any(|value| value.trim().is_empty()) {
        return Err(ExecutionContractError::UnresolvedField(field));
    }
    Ok(())
}

// Absent optional identity fields have exactly one canonical spelling: the
// key is omitted. The deserializers below reject the non-canonical spellings
// of absence (`null`, `{}`, `[]`) fail-closed, so an implementation that
// canonicalizes the raw JSON directly (parse → JCS → BLAKE3) can never
// include a key this typed layer would have dropped — the same input either
// hashes identically everywhere or is rejected everywhere.

fn present_not_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some).map_err(|error| {
        serde::de::Error::custom(format!(
            "{error}; absent optional identity fields must omit the key (explicit null is non-canonical)"
        ))
    })
}

fn present_non_empty_map<'de, D>(deserializer: D) -> Result<BTreeMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let map = BTreeMap::deserialize(deserializer)?;
    if map.is_empty() {
        return Err(serde::de::Error::custom(
            "absent optional identity collections must omit the key (explicit {} is non-canonical)",
        ));
    }
    Ok(map)
}

fn present_non_empty_string_list<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let list = Vec::deserialize(deserializer)?;
    if list.is_empty() {
        return Err(serde::de::Error::custom(
            "absent optional identity collections must omit the key (explicit [] is non-canonical)",
        ));
    }
    Ok(list)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;

    fn digest(algorithm: DigestAlgorithm, byte: u8) -> ContentDigest {
        ContentDigest::new(algorithm, [byte; 32])
    }

    fn sample_contract() -> ExecutionContractV1 {
        ExecutionContractV1 {
            schema: EXECUTION_CONTRACT_V1_SCHEMA.to_string(),
            source: ResolvedSourceContract {
                kind: "git".to_string(),
                immutable_ref: "https://example.invalid/repo@012345".to_string(),
                digest: digest(DigestAlgorithm::Sha256, 1),
                projection_digest: digest(DigestAlgorithm::Blake3, 0x0c),
            },
            target: ResolvedTargetContract {
                os: "linux".to_string(),
                architecture: "x86_64".to_string(),
                abi: "gnu".to_string(),
                libc: Some("glibc-2.39".to_string()),
                observable_features: BTreeMap::new(),
            },
            runtime: ResolvedArtifactContract {
                kind: "node".to_string(),
                resolved_ref: "node@22.14.0".to_string(),
                digest: digest(DigestAlgorithm::Sha256, 2),
                dynamic_contract_digest: digest(DigestAlgorithm::Blake3, 0x0d),
            },
            dependencies: vec![ResolvedDependencyContract {
                name: "npm".to_string(),
                derivation_digest: digest(DigestAlgorithm::Blake3, 3),
                output_digest: digest(DigestAlgorithm::Blake3, 4),
            }],
            build_outputs: vec![ResolvedBuildOutputContract {
                name: "app".to_string(),
                digest: digest(DigestAlgorithm::Blake3, 5),
                projection_digest: digest(DigestAlgorithm::Blake3, 0x0e),
            }],
            launch: ResolvedLaunchContract {
                argv: vec!["node".to_string(), "dist/server.js".to_string()],
                cwd: "/workspace".to_string(),
                process_model_digest: digest(DigestAlgorithm::Blake3, 0x0f),
                environment: vec![EnvironmentVariableContract {
                    name: "NODE_ENV".to_string(),
                    value_digest: digest(DigestAlgorithm::Blake3, 6),
                }],
                environment_policy_digest: digest(DigestAlgorithm::Blake3, 0x10),
                secret_bindings: vec!["API_TOKEN".to_string()],
            },
            filesystem: ResolvedFilesystemContract {
                view_digest: digest(DigestAlgorithm::Blake3, 7),
                topology_digest: digest(DigestAlgorithm::Blake3, 0x11),
                readonly_layers: vec![digest(DigestAlgorithm::Blake3, 8)],
                writable_paths: vec!["/tmp".to_string()],
            },
            policy: ResolvedPolicyContract {
                network_digest: digest(DigestAlgorithm::Blake3, 9),
                capability_digest: digest(DigestAlgorithm::Blake3, 10),
                filesystem_digest: digest(DigestAlgorithm::Blake3, 11),
            },
            guest_surface: GuestSurfaceContract {
                bind_address: "0.0.0.0".to_string(),
                protocol: "ato-guest/v1".to_string(),
                port: Some(8080),
                features: vec!["bindings".to_string(), "exec".to_string()],
            },
            external_state: vec![ExternalStateContract {
                name: "data".to_string(),
                target: "/data".to_string(),
                access: ExternalStateAccess::ReadWrite,
                schema: "1".to_string(),
                snapshot: SnapshotExclusion::Exclude,
            }],
        }
    }

    fn matching_observation(contract: &ExecutionContractV1) -> ObservedExecutionContractV1 {
        ObservedExecutionContractV1 {
            contract: contract.clone(),
        }
    }

    #[test]
    fn build_observation_must_match_the_complete_launch_envelope() {
        let contract = sample_contract();
        contract
            .verify_observation(&matching_observation(&contract))
            .expect("matching build observation");

        let mut stale_output = matching_observation(&contract);
        stale_output.contract.build_outputs[0].digest = digest(DigestAlgorithm::Blake3, 0xff);
        assert_eq!(
            contract.verify_observation(&stale_output),
            Err(ExecutionContractError::ObservedContractMismatch)
        );

        for mutate in [
            |observed: &mut ObservedExecutionContractV1| {
                observed.contract.launch.argv.push("--new".to_string());
            },
            |observed: &mut ObservedExecutionContractV1| {
                observed.contract.target.architecture = "aarch64".to_string();
            },
            |observed: &mut ObservedExecutionContractV1| {
                observed.contract.launch.environment[0].value_digest =
                    digest(DigestAlgorithm::Blake3, 0xee);
            },
            |observed: &mut ObservedExecutionContractV1| {
                observed.contract.guest_surface.port = Some(9090);
            },
        ] {
            let mut observed = matching_observation(&contract);
            mutate(&mut observed);
            assert_eq!(
                contract.verify_observation(&observed),
                Err(ExecutionContractError::ObservedContractMismatch)
            );
        }

        let mut stale_derivation = matching_observation(&contract);
        stale_derivation.contract.dependencies[0].derivation_digest =
            digest(DigestAlgorithm::Blake3, 0xdd);
        assert_eq!(
            contract.verify_observation(&stale_derivation),
            Err(ExecutionContractError::ObservedContractMismatch)
        );

        let mut stale_writable_path = matching_observation(&contract);
        stale_writable_path
            .contract
            .filesystem
            .writable_paths
            .push("/var/tmp".to_string());
        assert_eq!(
            contract.verify_observation(&stale_writable_path),
            Err(ExecutionContractError::ObservedContractMismatch)
        );
    }

    #[test]
    fn execution_id_is_domain_separated_jcs_blake3() {
        let contract = sample_contract();
        let canonical = serde_jcs::to_vec(&contract).expect("canonical contract");
        let mut expected_input = EXECUTION_CONTRACT_V1_SCHEMA.as_bytes().to_vec();
        expected_input.push(0);
        expected_input.extend(canonical);

        assert_eq!(
            contract.compute_execution_id().expect("execution id"),
            ExecutionId::new(format!("blake3:{}", blake3::hash(&expected_input).to_hex()))
                .expect("valid id")
        );
        assert_eq!(
            contract.compute_execution_id().expect("execution id"),
            ExecutionId::new(
                "blake3:f33b4afecf228ccab29e8b4b18b101f3f4539d7481e3279873312ae4a666d831"
                    .to_string()
            )
            .expect("shared Rust/TypeScript vector")
        );
    }

    #[test]
    fn content_digest_rejects_placeholders_wrong_lengths_and_uppercase_hex() {
        for invalid in [
            "latest",
            "sha512:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "sha256:unknown",
            "sha256:aa",
            "blake3:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            assert!(
                ContentDigest::try_from(invalid.to_string()).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn content_digest_round_trips_as_a_canonical_string() {
        let expected = digest(DigestAlgorithm::Sha256, 0xab);
        let json = serde_json::to_string(&expected).unwrap();
        assert_eq!(json, format!("\"sha256:{}\"", "ab".repeat(32)));
        assert_eq!(
            serde_json::from_str::<ContentDigest>(&json).unwrap(),
            expected
        );
    }

    #[test]
    fn resolved_target_architecture_changes_execution_id() {
        let x86 = sample_contract();
        let mut arm = x86.clone();
        arm.target.architecture = "aarch64".to_string();

        assert_ne!(
            x86.compute_execution_id().unwrap(),
            arm.compute_execution_id().unwrap()
        );
    }

    #[test]
    fn external_state_contract_changes_execution_id() {
        let first = sample_contract();
        let mut second = first.clone();
        second.external_state[0].schema = "2".to_string();

        assert_ne!(
            first.compute_execution_id().unwrap(),
            second.compute_execution_id().unwrap()
        );
    }

    #[test]
    fn opaque_subcontract_digests_are_identity_bearing() {
        // Each RFC §4.2 facet that v1 commits as an opaque sub-contract digest
        // (source projection rules, dynamic runtime contract, build-output
        // projection, launch process model, environment policy, filesystem
        // mount topology + access modes) must be in the identity set: mutating
        // its digest must change execution_id, and pairwise-distinctly so.
        let baseline = sample_contract();
        let baseline_id = baseline.compute_execution_id().unwrap();

        type Mutation = (&'static str, fn(&mut ExecutionContractV1));
        let mutations: [Mutation; 6] = [
            ("source.projection_digest", |contract| {
                contract.source.projection_digest =
                    ContentDigest::new(DigestAlgorithm::Blake3, [0xac; 32]);
            }),
            ("runtime.dynamic_contract_digest", |contract| {
                contract.runtime.dynamic_contract_digest =
                    ContentDigest::new(DigestAlgorithm::Blake3, [0xad; 32]);
            }),
            ("build_outputs[].projection_digest", |contract| {
                contract.build_outputs[0].projection_digest =
                    ContentDigest::new(DigestAlgorithm::Blake3, [0xae; 32]);
            }),
            ("launch.process_model_digest", |contract| {
                contract.launch.process_model_digest =
                    ContentDigest::new(DigestAlgorithm::Blake3, [0xaf; 32]);
            }),
            ("launch.environment_policy_digest", |contract| {
                contract.launch.environment_policy_digest =
                    ContentDigest::new(DigestAlgorithm::Blake3, [0xb0; 32]);
            }),
            ("filesystem.topology_digest", |contract| {
                contract.filesystem.topology_digest =
                    ContentDigest::new(DigestAlgorithm::Blake3, [0xb1; 32]);
            }),
        ];

        let mut ids = BTreeSet::new();
        ids.insert(baseline_id.as_str().to_string());
        for (field, apply) in mutations {
            let mut mutated = baseline.clone();
            apply(&mut mutated);
            let id = mutated.compute_execution_id().unwrap();
            assert_ne!(id, baseline_id, "{field} must change execution_id");
            assert!(
                ids.insert(id.as_str().to_string()),
                "{field} must produce a distinct execution_id"
            );
        }
    }

    #[test]
    fn unknown_identity_field_fails_closed() {
        let mut value = serde_json::to_value(sample_contract()).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("runner".to_string(), serde_json::json!("local"));

        assert!(serde_json::from_value::<ExecutionContractV1>(value).is_err());
    }

    #[test]
    fn malformed_execution_id_fails_deserialization() {
        assert!(serde_json::from_str::<ExecutionId>("\"blake3:not-a-digest\"").is_err());
    }

    #[test]
    fn uppercase_execution_id_is_rejected_as_noncanonical() {
        assert!(ExecutionId::new(format!("blake3:{}", "A".repeat(64))).is_err());
    }

    #[test]
    fn unresolved_or_empty_identity_fields_fail_closed() {
        let mut value = serde_json::to_value(sample_contract()).unwrap();
        value["runtime"]["digest"] = serde_json::json!("unknown");
        assert!(serde_json::from_value::<ExecutionContractV1>(value).is_err());
    }

    #[test]
    fn canonical_validation_rejects_blank_and_non_ascii_sorted_identifiers() {
        let mut blank = sample_contract();
        blank.dependencies[0].name = " ".to_string();
        assert!(matches!(
            blank.validate(),
            Err(ExecutionContractError::NonCanonicalList("dependencies"))
        ));

        let mut unicode = sample_contract();
        unicode.launch.secret_bindings = vec!["TOKEN".to_string(), "é_TOKEN".to_string()];
        assert!(matches!(
            unicode.validate(),
            Err(ExecutionContractError::NonCanonicalList(
                "launch.secret_bindings"
            ))
        ));
    }

    #[test]
    fn non_canonical_spellings_of_absent_optional_fields_fail_closed() {
        let baseline = serde_json::to_value(sample_contract()).unwrap();
        for (facet, field, non_canonical) in [
            ("target", "libc", serde_json::Value::Null),
            ("guest_surface", "port", serde_json::Value::Null),
            ("target", "observable_features", serde_json::json!({})),
            ("launch", "secret_bindings", serde_json::json!([])),
        ] {
            let mut value = baseline.clone();
            value[facet][field] = non_canonical;
            assert!(
                serde_json::from_value::<ExecutionContractV1>(value).is_err(),
                "{facet}.{field}: non-canonical spelling of absence must fail closed"
            );
        }

        // The canonical spelling of absence — omitting the key — still parses,
        // and the typed defaults serialize back to the omitted form.
        let mut omitted = baseline;
        omitted["target"].as_object_mut().unwrap().remove("libc");
        omitted["guest_surface"]
            .as_object_mut()
            .unwrap()
            .remove("port");
        omitted["launch"]
            .as_object_mut()
            .unwrap()
            .remove("secret_bindings");
        let parsed = serde_json::from_value::<ExecutionContractV1>(omitted).unwrap();
        assert_eq!(parsed.target.libc, None);
        assert_eq!(parsed.guest_surface.port, None);
        assert!(parsed.launch.secret_bindings.is_empty());
        assert!(parsed.target.observable_features.is_empty());

        let reserialized = serde_json::to_value(&parsed).unwrap();
        for (facet, field) in [
            ("target", "libc"),
            ("target", "observable_features"),
            ("guest_surface", "port"),
            ("launch", "secret_bindings"),
        ] {
            assert!(
                reserialized[facet].get(field).is_none(),
                "{facet}.{field}: absent field must serialize as omitted"
            );
        }
    }

    #[test]
    fn envelope_metadata_does_not_change_execution_id() {
        let contract = sample_contract();
        let expected = contract.compute_execution_id().unwrap();
        let mut envelope = ExecutionContractEnvelopeV1 {
            execution_contract: contract,
            execution_id: expected.clone(),
            generated_at: None,
            provenance: serde_json::Value::Null,
            diagnostics: serde_json::Value::Null,
            evidence: serde_json::Value::Null,
        };

        envelope.generated_at = Some("2026-07-21T00:00:00Z".to_string());
        envelope.provenance = serde_json::json!({
            "builder": "runner-a",
            "machine_id": "machine-77",
            "display_url": "https://example.invalid/builds/123",
        });
        envelope.diagnostics = serde_json::json!({"resolver_log": "..."});
        envelope.evidence = serde_json::json!({"probe": "ok"});

        envelope.verify().expect("metadata never affects the id");
        assert_eq!(
            envelope.execution_contract.compute_execution_id().unwrap(),
            expected
        );
    }

    #[test]
    fn envelope_tolerates_unknown_non_identity_fields() {
        let contract = sample_contract();
        let execution_id = contract.compute_execution_id().unwrap();
        let mut value = serde_json::json!({
            "execution_contract": contract,
            "execution_id": execution_id,
        });
        let object = value.as_object_mut().unwrap();
        object.insert("runner_id".to_string(), serde_json::json!("runner-a"));
        object.insert("session_id".to_string(), serde_json::json!("sess-42"));
        object.insert("snapshot_id".to_string(), serde_json::json!("snap-9"));
        object.insert("host_port".to_string(), serde_json::json!(54321));

        let envelope =
            serde_json::from_value::<ExecutionContractEnvelopeV1>(value).expect("tolerant read");
        envelope.verify().expect("unknown envelope fields ignored");
    }

    #[test]
    fn envelope_verification_rejects_execution_id_mismatch() {
        let contract = sample_contract();
        let envelope = ExecutionContractEnvelopeV1 {
            execution_contract: contract,
            execution_id: ExecutionId::new(format!("blake3:{}", "0".repeat(64))).unwrap(),
            generated_at: None,
            provenance: serde_json::Value::Null,
            diagnostics: serde_json::Value::Null,
            evidence: serde_json::Value::Null,
        };

        assert!(matches!(
            envelope.verify(),
            Err(ExecutionContractError::ExecutionIdMismatch { .. })
        ));
    }

    #[test]
    fn canonicalization_is_field_order_and_whitespace_independent() {
        let baseline = sample_contract();
        let expected = baseline.compute_execution_id().unwrap();

        // Reorder top-level and nested keys, and add irrelevant whitespace:
        // typed deserialization plus JCS must erase both.
        let reordered = r#"{
            "external_state": [ { "snapshot": "exclude", "schema": "1",
                "access": "read-write", "target": "/data", "name": "data" } ],
            "guest_surface": { "features": ["bindings", "exec"], "port": 8080,
                "protocol": "ato-guest/v1", "bind_address": "0.0.0.0" },
            "policy": {
                "filesystem_digest": "blake3:0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b",
                "capability_digest": "blake3:0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a",
                "network_digest": "blake3:0909090909090909090909090909090909090909090909090909090909090909" },
            "filesystem": { "writable_paths": ["/tmp"],
                "readonly_layers": ["blake3:0808080808080808080808080808080808080808080808080808080808080808"],
                "topology_digest": "blake3:1111111111111111111111111111111111111111111111111111111111111111",
                "view_digest": "blake3:0707070707070707070707070707070707070707070707070707070707070707" },
            "launch": { "secret_bindings": ["API_TOKEN"],
                "environment_policy_digest": "blake3:1010101010101010101010101010101010101010101010101010101010101010",
                "environment": [ { "value_digest": "blake3:0606060606060606060606060606060606060606060606060606060606060606",
                    "name": "NODE_ENV" } ],
                "process_model_digest": "blake3:0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f",
                "cwd": "/workspace", "argv": ["node", "dist/server.js"] },
            "build_outputs": [ { "projection_digest": "blake3:0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e0e",
                "digest": "blake3:0505050505050505050505050505050505050505050505050505050505050505",
                "name": "app" } ],
            "dependencies": [ { "output_digest": "blake3:0404040404040404040404040404040404040404040404040404040404040404",
                "derivation_digest": "blake3:0303030303030303030303030303030303030303030303030303030303030303",
                "name": "npm" } ],
            "runtime": { "dynamic_contract_digest": "blake3:0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d",
                "digest": "sha256:0202020202020202020202020202020202020202020202020202020202020202",
                "resolved_ref": "node@22.14.0", "kind": "node" },
            "target": { "libc": "glibc-2.39", "abi": "gnu",
                "architecture": "x86_64", "os": "linux" },
            "source": { "projection_digest": "blake3:0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c",
                "digest": "sha256:0101010101010101010101010101010101010101010101010101010101010101",
                "immutable_ref": "https://example.invalid/repo@012345", "kind": "git" },
            "schema": "ato.execution-contract/v1"
        }"#;

        let parsed = serde_json::from_str::<ExecutionContractV1>(reordered).unwrap();
        assert_eq!(parsed, baseline);
        assert_eq!(parsed.compute_execution_id().unwrap(), expected);
        assert_eq!(
            parsed.canonical_bytes().unwrap(),
            baseline.canonical_bytes().unwrap()
        );
    }
}
