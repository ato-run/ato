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
//! | Runtime | resolved runtime artifact; dynamic runtime contract | `runtime.kind` / `runtime.digest`; `runtime.dynamic_contract_digest` (opaque) |
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
//! ## Resolved refs are non-identity provenance (RFC §4.2)
//!
//! The *resolved ref* that names how identity content was obtained —
//! `source.kind`, `source.immutable_ref`, and `runtime.resolved_ref` — is NOT
//! in the identity set. Same source bytes + same projection ⇒ same Execution
//! Identity, and same runtime artifact digest + dynamic contract ⇒ same
//! Execution Identity, so an alias (mirror URL, tag vs commit, `node@lts` vs a
//! pinned digest) must never split ids. These refs live on the non-identity
//! envelope as [`ResolvedRefProvenanceV1`]; mutating them is proved
//! id-preserving by the `*-alias` shared vectors.
//!
//! ## Canonical guest paths and ports
//!
//! `launch.cwd`, `filesystem.writable_paths[]`, and `external_state[].target`
//! are [`GuestPath`]: an absolute path in exactly one canonical spelling (no
//! bare `/` root unless the field opts in, no `.`/`..` segments, no repeated or
//! trailing slash, no backslash/NUL/control chars), with segment-wise ordering
//! for the sorted `writable_paths` list. `guest_surface.port`, when present, is
//! a nonzero `u16` ([`NonZeroU16`]); a `0` port fails closed. Non-canonical
//! spellings are pinned by the `invalid-relative-cwd`, `invalid-dotdot-target`,
//! `invalid-trailing-slash-target`, and `invalid-zero-port` vectors.
//!
//! ## Opaque sub-contract digest preimages (normative)
//!
//! Every opaque `*_digest` field commits a sub-contract under a fixed preimage
//! rule; only the *payload schema* is versioned separately (RFC §4.5). The
//! rule is `digest = blake3(UTF8(domain) || 0x00 || JCS(payload))`
//! ([`opaque_subcontract_digest`]), with these exact domains:
//!
//! | field | domain |
//! |---|---|
//! | `source.projection_digest` | `ato.source-projection-contract/v1` |
//! | `runtime.dynamic_contract_digest` | `ato.runtime-dynamic-contract/v1` |
//! | `build_outputs[].projection_digest` | `ato.build-output-projection/v1` |
//! | `launch.process_model_digest` | `ato.process-model-contract/v1` |
//! | `launch.environment_policy_digest` | `ato.environment-policy/v1` |
//! | `filesystem.topology_digest` | `ato.filesystem-topology/v1` |
//! | `launch.environment[].value_digest` | `ato.environment-value/v1` |
//!
//! The three `policy.*` digests keep their existing producer-defined preimages.
//! G0-2 stores each payload in `ato.lock.json` and re-derives its digest before
//! launch; a later PR may define/extend a payload schema without a v1 identity
//! change, but MUST keep the domain constant and preimage rule above.
//!
//! ## Duplicate keys fail closed (canonicalization safety)
//!
//! A repeated JSON key must never silently collapse to a last-wins value that
//! two byte-distinct inputs could share. Duplicate struct fields (top-level and
//! nested identity objects) are rejected by serde's derived deserializers;
//! `target.observable_features` uses a duplicate-rejecting map visitor rather
//! than `BTreeMap`'s last-wins insert. Pinned by `invalid-duplicate-*` vectors.
//!
//! ## Observation / finalization is deferred to G0-2 (NOT in this surface)
//!
//! This G0-1 surface intentionally exposes NO "observation" or "finalization"
//! type. A finalized execution identity MUST be witnessed against *measured*
//! facet values, and G0-2 will introduce that as a type constructible only from
//! measured facts — a facet-wise observation struct (e.g.
//! `{ source_digest, target, runtime_digest, build_output_digests, … }` built
//! from the concrete plan and materialization), whose fields are compared to
//! the expected contract facet by facet. It MUST NOT be a clone of the expected
//! [`ExecutionContractV1`]: an API that accepts an expected-contract copy as its
//! own "observation" proves nothing (RFC forbids lock-copied observations).
//! Callers in G0-1 use [`ExecutionContractV1::compute_execution_id`] directly.
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

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::num::NonZeroU16;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const EXECUTION_CONTRACT_V1_SCHEMA: &str = "ato.execution-contract/v1";

// Opaque sub-contract digest domain separators (normative).
//
// Each opaque `*_digest` facet field commits a sub-contract whose *payload
// schema is versioned separately from v1*. Its digest is fixed by v1 as:
//
// ```text
// digest = blake3(UTF8("<domain>") || 0x00 || JCS(self-describing payload))
// ```
//
// The domain string below is the exact preimage prefix for that field. A
// later PR MAY define or extend the payload schema behind a digest without a
// v1 identity change (RFC §4.5 layering), but MUST keep this preimage rule and
// domain constant. G0-2 stores each payload alongside its digest in
// `ato.lock.json` and re-derives the digest before launch. See the
// module-level "opaque sub-contract digest preimages" section.
pub const SOURCE_PROJECTION_CONTRACT_V1_DOMAIN: &str = "ato.source-projection-contract/v1";
pub const RUNTIME_DYNAMIC_CONTRACT_V1_DOMAIN: &str = "ato.runtime-dynamic-contract/v1";
pub const BUILD_OUTPUT_PROJECTION_V1_DOMAIN: &str = "ato.build-output-projection/v1";
pub const PROCESS_MODEL_CONTRACT_V1_DOMAIN: &str = "ato.process-model-contract/v1";
pub const ENVIRONMENT_POLICY_V1_DOMAIN: &str = "ato.environment-policy/v1";
pub const FILESYSTEM_TOPOLOGY_V1_DOMAIN: &str = "ato.filesystem-topology/v1";

/// Domain separator for a single resolved non-secret environment *value*
/// digest (`launch.environment[].value_digest`). The preimage relation is
/// pinned as a v1 contract:
///
/// ```text
/// value_digest = blake3(UTF8("ato.environment-value/v1") || 0x00 || JCS(payload))
/// ```
///
/// where `payload` is a self-describing object carrying the normalized,
/// non-secret variable value. Secret values never enter the contract at all
/// (RFC §4.3). G0-2 stores the payload in `ato.lock.json` and verifies the
/// digest before launch.
pub const ENVIRONMENT_VALUE_V1_DOMAIN: &str = "ato.environment-value/v1";

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

/// A canonical absolute guest path.
///
/// Guest paths appear in identity-bearing fields (`launch.cwd`,
/// `filesystem.writable_paths[]`, `external_state[].target`). A path is
/// accepted only in exactly one canonical spelling so that semantically equal
/// paths can never derive different `execution_id`s across implementations:
///
/// * absolute — a leading `/`;
/// * no bare `/` root, unless the field explicitly opts in via
///   [`GuestPath::parse_allowing_root`];
/// * no `.` or `..` segments;
/// * no repeated `/` (empty segment) and no trailing `/`;
/// * no backslash, NUL, or other control characters.
///
/// Ordering and equality are **segment-wise** (path components compared
/// component-by-component), which is the canonical sort order the identity
/// lists (`writable_paths`) are validated against — independent of raw byte
/// order. The decomposition into segments is a bijection with the accepted
/// spelling, so segment-wise `Ord` stays consistent with the derived
/// `PartialEq`/`Eq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestPath(String);

impl GuestPath {
    /// Parse a canonical guest path, rejecting the bare `/` root.
    pub fn parse(value: &str) -> Result<Self, ExecutionContractError> {
        Self::parse_inner(value, false)
    }

    /// Parse a canonical guest path, accepting the bare `/` root. Used only by
    /// fields that explicitly permit the filesystem root.
    pub fn parse_allowing_root(value: &str) -> Result<Self, ExecutionContractError> {
        Self::parse_inner(value, true)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn parse_inner(value: &str, allow_root: bool) -> Result<Self, ExecutionContractError> {
        use ExecutionContractError::InvalidGuestPath as Bad;
        if !value.starts_with('/') {
            return Err(Bad("must be absolute (a leading '/')"));
        }
        if value
            .bytes()
            .any(|byte| byte == b'\\' || byte < 0x20 || byte == 0x7f)
        {
            return Err(Bad(
                "must not contain a backslash, NUL, or control character",
            ));
        }
        if value == "/" {
            return if allow_root {
                Ok(Self(value.to_string()))
            } else {
                Err(Bad("bare '/' root is not permitted for this field"))
            };
        }
        if value.ends_with('/') {
            return Err(Bad("must not have a trailing slash"));
        }
        // `value` starts with '/', so the first split segment is always the
        // empty string before the leading slash; every later segment is a real
        // path component.
        for segment in value.split('/').skip(1) {
            if segment.is_empty() {
                return Err(Bad("must not contain a repeated slash"));
            }
            if segment == "." || segment == ".." {
                return Err(Bad("must not contain a '.' or '..' segment"));
            }
        }
        Ok(Self(value.to_string()))
    }
}

impl fmt::Display for GuestPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl PartialOrd for GuestPath {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for GuestPath {
    fn cmp(&self, other: &Self) -> Ordering {
        // Segment-wise: compare path components lexicographically. Both paths
        // start with '/', so the leading empty segment compares equal first.
        self.0.split('/').cmp(other.0.split('/'))
    }
}

impl Serialize for GuestPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for GuestPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
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
    #[error("guest path is not canonical: {0}")]
    InvalidGuestPath(&'static str),
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
/// *source projection rules* (include/exclude, symlink and case policy, …); its
/// preimage domain is [`SOURCE_PROJECTION_CONTRACT_V1_DOMAIN`] and its payload
/// schema is versioned separately from v1 (see the module-level facet mapping).
///
/// The *resolved ref* that names how these bytes were obtained (`kind`,
/// `immutable_ref` — VCS kind, tag/commit alias, mirror URL) is deliberately
/// NOT here: two aliases that resolve to the same source bytes and the same
/// projection are the SAME Execution Identity (RFC §4.2), so the ref lives in
/// the non-identity [`ResolvedRefProvenanceV1`] on the envelope, never in the
/// identity set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedSourceContract {
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
        deserialize_with = "present_non_empty_unique_map"
    )]
    pub observable_features: BTreeMap<String, String>,
}

/// Resolved runtime artifact facet. `kind` is the runtime family discriminator
/// and `digest` is the resolved runtime artifact digest.
/// `dynamic_contract_digest` is the opaque sub-contract digest committing the
/// *dynamic runtime contract* — runtime-provided launch-time behaviour beyond
/// the static artifact bytes (dynamic linking / loader resolution, plugin or
/// module surface, JIT/ABI switches); its preimage domain is
/// [`RUNTIME_DYNAMIC_CONTRACT_V1_DOMAIN`] and its payload schema is versioned
/// separately from v1 (see the module-level facet mapping).
///
/// The *resolved ref* that names how the artifact was selected
/// (`resolved_ref` — e.g. `node@22.14.0` vs a mirror digest URL) is NOT here:
/// two refs resolving to the same runtime artifact digest and dynamic contract
/// are the SAME Execution Identity, so the ref lives in the non-identity
/// [`ResolvedRefProvenanceV1`] on the envelope, never in the identity set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedArtifactContract {
    pub kind: String,
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
/// rename, permission projection). Its preimage domain is
/// [`BUILD_OUTPUT_PROJECTION_V1_DOMAIN`] and its payload schema is versioned
/// separately from v1 (see the module-level facet mapping).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedBuildOutputContract {
    pub name: String,
    pub digest: ContentDigest,
    pub projection_digest: ContentDigest,
}

/// Resolved launch facet. `argv` is the exact entrypoint and `cwd` is the
/// canonical guest working directory ([`GuestPath`]).
/// `process_model_digest` is the opaque sub-contract digest committing the
/// *process model* (root process, daemonization/PID-1 role, supervised child
/// structure); its preimage domain is [`PROCESS_MODEL_CONTRACT_V1_DOMAIN`] and
/// its payload schema is versioned separately from v1. `environment` carries
/// the resolved non-secret values (each committed under
/// [`ENVIRONMENT_VALUE_V1_DOMAIN`]), and `environment_policy_digest` is the
/// opaque sub-contract digest committing the *variable requirements,
/// normalization, and inheritance policy* (domain
/// [`ENVIRONMENT_POLICY_V1_DOMAIN`]). Secret values never enter the contract:
/// secrets are bound by name in `secret_bindings` and their values are
/// excluded from identity (RFC §4.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedLaunchContract {
    pub argv: Vec<String>,
    pub cwd: GuestPath,
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
/// content digest of the exact resolved value under the
/// [`ENVIRONMENT_VALUE_V1_DOMAIN`] preimage rule (`blake3(domain || 0x00 ||
/// JCS(payload))`, where `payload` is the self-describing normalized value);
/// secret values never appear here — secrets are bound by name via
/// `ResolvedLaunchContract::secret_bindings` and their values are excluded
/// from identity (RFC §4.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentVariableContract {
    pub name: String,
    pub value_digest: ContentDigest,
}

/// Resolved filesystem facet. `readonly_layers` are the immutable layer
/// digests; `writable_paths` is the writable-boundary contract as canonical
/// guest paths ([`GuestPath`]); `view_digest` commits the composed immutable
/// view *content*. `topology_digest` is the opaque sub-contract digest
/// committing the *mount topology and per-mount access modes* (which
/// layer/output is mounted where, and read-only vs read-write per mount) —
/// content and structure are separate commitments, so a topology or
/// access-mode change is identity-bearing even when the mounted bytes are
/// unchanged. Its preimage domain is [`FILESYSTEM_TOPOLOGY_V1_DOMAIN`] and its
/// payload schema is versioned separately from v1 (see the module-level facet
/// mapping).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedFilesystemContract {
    pub view_digest: ContentDigest,
    pub topology_digest: ContentDigest,
    pub readonly_layers: Vec<ContentDigest>,
    pub writable_paths: Vec<GuestPath>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedPolicyContract {
    pub network_digest: ContentDigest,
    pub capability_digest: ContentDigest,
    pub filesystem_digest: ContentDigest,
}

/// Declared guest-facing surface. `port` is an optional guest port; when
/// present it is a nonzero `u16` (a `0` port is never a valid declared
/// surface, so it is rejected fail-closed via [`NonZeroU16`]).
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
    pub port: Option<NonZeroU16>,
    pub features: Vec<String>,
}

/// External state binding. `target` is the canonical guest mount/injection
/// path ([`GuestPath`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalStateContract {
    pub name: String,
    pub target: GuestPath,
    pub access: ExternalStateAccess,
    pub schema: String,
    pub snapshot: SnapshotExclusion,
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

/// Non-identity provenance for the *resolved refs* that name how the
/// identity-bearing content was obtained.
///
/// `source_kind` / `source_immutable_ref` (VCS kind, tag/commit/mirror alias)
/// and `runtime_resolved_ref` (e.g. `node@22.14.0` vs a mirror digest URL) are
/// **aliases**: two refs that resolve to the same source bytes + projection and
/// the same runtime artifact digest + dynamic contract are the SAME Execution
/// Identity (RFC §4.2). They therefore live here on the non-identity envelope,
/// never in [`ExecutionContractV1`], so an alias can never split an id.
/// Excluded from `execution_id` by construction (it is not part of the
/// embedded contract).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedRefProvenanceV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_immutable_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_resolved_ref: Option<String>,
}

impl ResolvedRefProvenanceV1 {
    fn is_empty(&self) -> bool {
        self.source_kind.is_none()
            && self.source_immutable_ref.is_none()
            && self.runtime_resolved_ref.is_none()
    }
}

/// Non-identity envelope around an execution contract.
///
/// Everything here besides `execution_contract` is excluded from the
/// execution identity: the resolved-ref provenance, free-form provenance,
/// diagnostics, evidence, timestamps, and the stored `execution_id` itself.
/// Unlike the identity-bearing contract, the envelope is deliberately tolerant
/// — unknown fields (runner names, Snapshot/Session IDs, dynamic endpoints, and
/// other operational facts) are ignored on read instead of failing closed,
/// because none of them may influence the id. [`Self::verify`] recomputes the
/// canonical hash from the embedded contract and fails closed on any mismatch
/// with the stored `execution_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionContractEnvelopeV1 {
    pub execution_contract: ExecutionContractV1,
    pub execution_id: ExecutionId,
    #[serde(default, skip_serializing_if = "ResolvedRefProvenanceV1::is_empty")]
    pub resolved_refs: ResolvedRefProvenanceV1,
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
            ("target.os", self.target.os.as_str()),
            ("target.architecture", self.target.architecture.as_str()),
            ("target.abi", self.target.abi.as_str()),
            ("runtime.kind", self.runtime.kind.as_str()),
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
        // Each element is already a canonical `GuestPath` (validated on
        // deserialization); the list only needs to be sorted (segment-wise)
        // and duplicate-free.
        validate_sorted_guest_paths("filesystem.writable_paths", &self.filesystem.writable_paths)?;
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
            // `state.target` is a canonical `GuestPath`; only the free-form
            // name and schema still need a non-empty check.
            ensure_values("external_state", [&state.name, &state.schema])?;
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
}

/// Compute an opaque sub-contract digest under the pinned v1 preimage rule:
///
/// ```text
/// digest = blake3(UTF8(domain) || 0x00 || JCS(payload))
/// ```
///
/// `domain` MUST be one of the normative opaque-digest domain constants
/// (`ato.source-projection-contract/v1`, `ato.runtime-dynamic-contract/v1`,
/// `ato.build-output-projection/v1`, `ato.process-model-contract/v1`,
/// `ato.environment-policy/v1`, `ato.filesystem-topology/v1`) or
/// [`ENVIRONMENT_VALUE_V1_DOMAIN`]. `payload` is the self-describing
/// sub-contract payload whose *schema is versioned separately from v1*. The
/// preimage rule is fixed by v1; the payload schema is not, so a later PR can
/// define the payload without a v1 identity change (RFC §4.5). The returned
/// digest is what a producer stores in the matching `*_digest` /
/// `value_digest` field.
pub fn opaque_subcontract_digest<T>(
    domain: &str,
    payload: &T,
) -> Result<ContentDigest, ExecutionContractError>
where
    T: Serialize,
{
    let canonical = serde_jcs::to_vec(payload)
        .map_err(|error| ExecutionContractError::Canonicalization(error.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain.as_bytes());
    hasher.update(&[0]);
    hasher.update(&canonical);
    Ok(ContentDigest::new(
        DigestAlgorithm::Blake3,
        *hasher.finalize().as_bytes(),
    ))
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

fn validate_sorted_guest_paths(
    field: &'static str,
    values: &[GuestPath],
) -> Result<(), ExecutionContractError> {
    // Segment-wise `Ord`: strictly increasing rejects both unsorted and
    // duplicate entries in one pass.
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
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

// `BTreeMap::deserialize` is last-wins on duplicate keys, which would let two
// byte-distinct JSON inputs (one with a repeated observable-feature key) map to
// the same typed value and hash — a canonicalization hole. This visitor
// instead fails closed on the first duplicate insertion, and still rejects the
// non-canonical explicit-empty spelling of absence. (Duplicate keys in the
// *struct* facets — top-level and nested identity objects — are already
// rejected by serde's derived struct deserializers via `duplicate field`,
// pinned by the invalid-duplicate-top-level-field / -nested-field vectors.)
fn present_non_empty_unique_map<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct UniqueMapVisitor;

    impl<'de> serde::de::Visitor<'de> for UniqueMapVisitor {
        type Value = BTreeMap<String, String>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a non-empty map of observable target features with unique keys")
        }

        fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut map = BTreeMap::new();
            while let Some((key, value)) = access.next_entry::<String, String>()? {
                if map.insert(key.clone(), value).is_some() {
                    return Err(serde::de::Error::custom(format!(
                        "duplicate observable feature key '{key}' \
                         (identity maps must have unique keys)"
                    )));
                }
            }
            if map.is_empty() {
                return Err(serde::de::Error::custom(
                    "absent optional identity collections must omit the key \
                     (explicit {} is non-canonical)",
                ));
            }
            Ok(map)
        }
    }

    deserializer.deserialize_map(UniqueMapVisitor)
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

    /// Baseline `execution_id` for [`sample_contract`]. Kept in lockstep with
    /// the shared fixture baseline (`manifest.json` / the vector generator).
    const BASELINE_EXECUTION_ID: &str =
        "blake3:fe216ebfbc65450f1cca14ca3ff41de81c42467fa672e7a7471d3392b058f464";

    fn digest(algorithm: DigestAlgorithm, byte: u8) -> ContentDigest {
        ContentDigest::new(algorithm, [byte; 32])
    }

    fn guest_path(value: &str) -> GuestPath {
        GuestPath::parse(value).expect("canonical guest path")
    }

    fn sample_contract() -> ExecutionContractV1 {
        ExecutionContractV1 {
            schema: EXECUTION_CONTRACT_V1_SCHEMA.to_string(),
            source: ResolvedSourceContract {
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
                cwd: guest_path("/workspace"),
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
                writable_paths: vec![guest_path("/tmp")],
            },
            policy: ResolvedPolicyContract {
                network_digest: digest(DigestAlgorithm::Blake3, 9),
                capability_digest: digest(DigestAlgorithm::Blake3, 10),
                filesystem_digest: digest(DigestAlgorithm::Blake3, 11),
            },
            guest_surface: GuestSurfaceContract {
                bind_address: "0.0.0.0".to_string(),
                protocol: "ato-guest/v1".to_string(),
                port: Some(NonZeroU16::new(8080).unwrap()),
                features: vec!["bindings".to_string(), "exec".to_string()],
            },
            external_state: vec![ExternalStateContract {
                name: "data".to_string(),
                target: guest_path("/data"),
                access: ExternalStateAccess::ReadWrite,
                schema: "1".to_string(),
                snapshot: SnapshotExclusion::Exclude,
            }],
        }
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
        // Pinned baseline id — MUST equal the shared fixture baseline
        // (`tests/fixtures/execution_contract/manifest.json`), which the
        // deterministic vector generator recomputes. Regenerate both together.
        assert_eq!(
            contract.compute_execution_id().expect("execution id"),
            ExecutionId::new(BASELINE_EXECUTION_ID.to_string())
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
            resolved_refs: ResolvedRefProvenanceV1::default(),
            generated_at: None,
            provenance: serde_json::Value::Null,
            diagnostics: serde_json::Value::Null,
            evidence: serde_json::Value::Null,
        };

        // Resolved-ref provenance (aliases) is non-identity: it must not move
        // the id.
        envelope.resolved_refs = ResolvedRefProvenanceV1 {
            source_kind: Some("archive".to_string()),
            source_immutable_ref: Some("https://mirror.invalid/repo@012345".to_string()),
            runtime_resolved_ref: Some("node@lts".to_string()),
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
            resolved_refs: ResolvedRefProvenanceV1::default(),
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
                "kind": "node" },
            "target": { "libc": "glibc-2.39", "abi": "gnu",
                "architecture": "x86_64", "os": "linux" },
            "source": { "projection_digest": "blake3:0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c",
                "digest": "sha256:0101010101010101010101010101010101010101010101010101010101010101" },
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

    #[test]
    fn guest_path_accepts_only_the_canonical_spelling() {
        for good in ["/workspace", "/data/ユーザー", "/a/b/c", "/tmp"] {
            assert!(GuestPath::parse(good).is_ok(), "{good}");
        }
        for (bad, _why) in [
            ("workspace", "relative"),
            ("", "empty"),
            ("/", "bare root"),
            ("/data/", "trailing slash"),
            ("/data//x", "repeated slash"),
            ("/data/../data", ".. segment"),
            ("/data/./x", ". segment"),
            ("/data\\x", "backslash"),
            ("/data\u{7}x", "control char"),
        ] {
            assert!(GuestPath::parse(bad).is_err(), "{bad}");
        }
        // Only a field that explicitly opts in may accept the bare root.
        assert!(GuestPath::parse("/").is_err());
        assert!(GuestPath::parse_allowing_root("/").is_ok());
        assert!(GuestPath::parse_allowing_root("/data/../data").is_err());
    }

    #[test]
    fn guest_path_ordering_is_segment_wise() {
        // Segment-wise: "/a" < "/a/b" < "/a.b" because the first component "a"
        // sorts before "a.b", and a prefix sorts before its extension.
        let a = guest_path("/a");
        let a_b = guest_path("/a/b");
        let a_dot_b = guest_path("/a.b");
        assert!(a < a_b);
        assert!(a_b < a_dot_b);
    }

    #[test]
    fn zero_guest_port_is_rejected() {
        let mut value = serde_json::to_value(sample_contract()).unwrap();
        value["guest_surface"]["port"] = serde_json::json!(0);
        assert!(serde_json::from_value::<ExecutionContractV1>(value).is_err());
    }

    #[test]
    fn duplicate_observable_feature_key_fails_closed() {
        // serde_json::Value would silently collapse the duplicate, so feed raw
        // bytes with a repeated key straight to the deserializer.
        let raw = r#"{
            "os": "linux", "architecture": "x86_64", "abi": "gnu",
            "observable_features": { "avx512": "1", "avx512": "0" }
        }"#;
        assert!(serde_json::from_str::<ResolvedTargetContract>(raw).is_err());
    }

    #[test]
    fn opaque_subcontract_digest_matches_the_pinned_preimage() {
        // Prove the preimage shape for one domain: the environment value digest.
        let payload = serde_json::json!({ "value": "production", "encoding": "utf-8" });
        let got = opaque_subcontract_digest(ENVIRONMENT_VALUE_V1_DOMAIN, &payload)
            .expect("digest computes");

        let canonical = serde_jcs::to_vec(&payload).unwrap();
        let mut preimage = ENVIRONMENT_VALUE_V1_DOMAIN.as_bytes().to_vec();
        preimage.push(0);
        preimage.extend(canonical);
        let expected =
            ContentDigest::new(DigestAlgorithm::Blake3, *blake3::hash(&preimage).as_bytes());

        assert_eq!(got, expected);
        assert_eq!(got.algorithm(), DigestAlgorithm::Blake3);
        // Domain separation: the same payload under a different domain differs.
        let other = opaque_subcontract_digest(FILESYSTEM_TOPOLOGY_V1_DOMAIN, &payload).unwrap();
        assert_ne!(got, other);
    }
}
