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
//! * **opaque sub-contract digest** — an [`OpaqueContractDigestV1`] field (a
//!   BLAKE3-only digest, never `sha256`) commits a sub-contract whose *payload
//!   schema is versioned separately from v1*. The digest is inside the v1
//!   identity set (mutating it changes `execution_id`), but the bytes a
//!   producer hashes into it are defined by a sub-contract that MAY gain
//!   structure in a later revision without touching the v1 identity set. This
//!   is the RFC §4.5 layering — a digest pins identity while its payload schema
//!   is versioned separately. The preimage is fixed by v1 under a **typed**
//!   domain ([`OpaqueContractDomainV1`], never a producer-chosen string), so no
//!   opaque digest — including the three `policy.*` digests — has a
//!   producer-defined preimage. The opaque `*_digest` facet fields are
//!   `source.projection_digest`, `runtime.dynamic_contract_digest`,
//!   `build_outputs[].projection_digest`, `launch.process_model_digest`,
//!   `launch.environment_policy_digest`, `launch.environment[].value_digest`,
//!   `filesystem.topology_digest`, and the three `policy.*` digests.
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
//! trailing slash, no backslash, and no Unicode control character — the C0
//! range incl. NUL, DEL, and the C1 range U+0080..=U+009F, i.e.
//! `char::is_control`).
//!
//! The sorted `writable_paths` list uses **segment-wise ordering, and within
//! each segment UTF-8 byte lexicographic order** (normative — Option A,
//! Unicode paths retained). A cross-language implementation **MUST compare
//! UTF-8 bytes, not UTF-16 code units**: the two orders diverge for
//! astral-plane characters (e.g. `U+E000` precedes `U+10000` under UTF-8 /
//! code-point order but follows it under UTF-16). Pinned by the
//! `guest-path-utf8-order` (valid, correctly ordered) and
//! `invalid-guest-path-utf16-order` (the same pair reversed into UTF-16 order,
//! rejected) vectors, and C1-control rejection by `invalid-c1-control-target`
//! (U+0085).
//!
//! `guest_surface.port`, when present, is a nonzero `u16` ([`NonZeroU16`]); a
//! `0` port fails closed. Other non-canonical spellings are pinned by the
//! `invalid-relative-cwd`, `invalid-dotdot-target`,
//! `invalid-trailing-slash-target`, and `invalid-zero-port` vectors.
//!
//! ## Opaque sub-contract digest preimages (normative)
//!
//! Every opaque digest field ([`OpaqueContractDigestV1`]) commits a
//! sub-contract under one fixed preimage rule; only the *payload schema* is
//! versioned separately (RFC §4.5). The rule is
//! `digest = blake3(UTF8(domain) || 0x00 || JCS(payload))`
//! ([`opaque_subcontract_digest`]), where `domain` is the typed
//! [`OpaqueContractDomainV1`] for the facet — never a producer-chosen string.
//! The exact field → domain mapping is:
//!
//! | field | domain | [`OpaqueContractDomainV1`] |
//! |---|---|---|
//! | `source.projection_digest` | `ato.source-projection-contract/v1` | `SourceProjection` |
//! | `runtime.dynamic_contract_digest` | `ato.runtime-dynamic-contract/v1` | `RuntimeDynamic` |
//! | `build_outputs[].projection_digest` | `ato.build-output-projection/v1` | `BuildOutputProjection` |
//! | `launch.process_model_digest` | `ato.process-model-contract/v1` | `ProcessModel` |
//! | `launch.environment_policy_digest` | `ato.environment-policy/v1` | `EnvironmentPolicy` |
//! | `launch.environment[].value_digest` | `ato.environment-value/v1` | `EnvironmentValue` |
//! | `filesystem.topology_digest` | `ato.filesystem-topology/v1` | `FilesystemTopology` |
//! | `policy.network_digest` | `ato.network-policy/v1` | `NetworkPolicy` |
//! | `policy.capability_digest` | `ato.capability-policy/v1` | `CapabilityPolicy` |
//! | `policy.filesystem_digest` | `ato.filesystem-policy/v1` | `FilesystemPolicy` |
//!
//! The three `policy.*` digests are on exactly the same footing as the other
//! opaque facets: their **domain and preimage rule are frozen by v1 now** (no
//! producer-defined preimage), while their payload *schemas* are defined in a
//! later PR. G0-2 stores each payload in `capsule.lock` and re-derives its
//! digest before launch; a later PR may define/extend a payload schema without
//! a v1 identity change, but MUST keep the domain constant and preimage rule
//! above. No in-tree normative preimage spec for the network/capability/
//! filesystem policy payloads exists yet; this module is the SSOT freezing
//! their domain + rule until that schema PR lands.
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

/// The normative domain separator for each opaque sub-contract digest facet.
///
/// Every opaque `*_digest` / `value_digest` field commits a sub-contract under
/// exactly one of these domains. Its digest is fixed by v1 as:
///
/// ```text
/// digest = blake3(UTF8(domain) || 0x00 || JCS(self-describing payload))
/// ```
///
/// The domain is inside the v1 identity set (it is baked into the digest
/// preimage), and the preimage rule and algorithm ([`DigestAlgorithm::Blake3`],
/// carried by [`OpaqueContractDigestV1`]) are frozen. Only the *payload schema*
/// behind the digest is versioned separately from v1 (RFC §4.5 layering): a
/// later PR MAY define or extend a payload without a v1 identity change, but
/// MUST keep the domain and preimage rule constant. G0-2 stores each payload
/// alongside its digest in `capsule.lock` and re-derives the digest before
/// launch.
///
/// The domain is a **typed** value, never a free `&str`: this makes it
/// impossible for a producer to invent a domain string, misspell one, or reuse
/// the wrong domain for a facet. `network`/`capability`/`filesystem` policy
/// digests are on the same footing as the other opaque facets — their domains
/// are frozen here even though their payload schemas land in a later PR (there
/// is no producer-defined preimage: the domain and rule are normative now).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OpaqueContractDomainV1 {
    /// `source.projection_digest` — source projection rules.
    SourceProjection,
    /// `runtime.dynamic_contract_digest` — dynamic runtime contract.
    RuntimeDynamic,
    /// `build_outputs[].projection_digest` — build-output projection.
    BuildOutputProjection,
    /// `launch.process_model_digest` — process model.
    ProcessModel,
    /// `launch.environment_policy_digest` — env requirements/normalization/inheritance policy.
    EnvironmentPolicy,
    /// `launch.environment[].value_digest` — a single resolved non-secret value.
    EnvironmentValue,
    /// `filesystem.topology_digest` — mount topology + per-mount access modes.
    FilesystemTopology,
    /// `policy.network_digest` — ingress/egress/DNS/isolation policy.
    NetworkPolicy,
    /// `policy.capability_digest` — host/device/sandbox capability policy.
    CapabilityPolicy,
    /// `policy.filesystem_digest` — filesystem capability policy.
    FilesystemPolicy,
}

impl OpaqueContractDomainV1 {
    /// The exact normative domain string hashed into the digest preimage.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceProjection => "ato.source-projection-contract/v1",
            Self::RuntimeDynamic => "ato.runtime-dynamic-contract/v1",
            Self::BuildOutputProjection => "ato.build-output-projection/v1",
            Self::ProcessModel => "ato.process-model-contract/v1",
            Self::EnvironmentPolicy => "ato.environment-policy/v1",
            Self::EnvironmentValue => "ato.environment-value/v1",
            Self::FilesystemTopology => "ato.filesystem-topology/v1",
            Self::NetworkPolicy => "ato.network-policy/v1",
            Self::CapabilityPolicy => "ato.capability-policy/v1",
            Self::FilesystemPolicy => "ato.filesystem-policy/v1",
        }
    }
}

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

/// A BLAKE3 opaque sub-contract digest (`blake3:<64 lowercase hex>`).
///
/// Unlike [`ContentDigest`] — which addresses concrete content and may be
/// `blake3` or `sha256` — an opaque sub-contract digest is **always** BLAKE3,
/// because v1 fixes its preimage to
/// `blake3(UTF8(domain) || 0x00 || JCS(payload))` ([`opaque_subcontract_digest`]).
/// The algorithm is therefore not a producer choice: a `sha256:`-spelled value
/// is not a valid opaque digest and fails deserialization closed. Only the
/// *payload schema* behind the digest is versioned separately from v1
/// (RFC §4.5); the algorithm, domain ([`OpaqueContractDomainV1`]), and preimage
/// rule are frozen. Enforcing BLAKE3 at the type level means the shared fixture
/// bytes for every opaque facet can never carry an `execution_id`-affecting
/// algorithm variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct OpaqueContractDigestV1([u8; 32]);

impl OpaqueContractDigestV1 {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn bytes(self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Display for OpaqueContractDigestV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}",
            DigestAlgorithm::Blake3.as_str(),
            hex::encode(self.0)
        )
    }
}

impl TryFrom<String> for OpaqueContractDigestV1 {
    type Error = ExecutionContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        // BLAKE3-only: the `blake3:` prefix is mandatory. A `sha256:` (or any
        // other) spelling is not a valid opaque sub-contract digest.
        let encoded = value
            .strip_prefix("blake3:")
            .ok_or(ExecutionContractError::InvalidOpaqueContractDigest)?;
        if encoded.len() != 64
            || !encoded
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ExecutionContractError::InvalidOpaqueContractDigest);
        }
        let decoded = hex::decode(encoded)
            .map_err(|_| ExecutionContractError::InvalidOpaqueContractDigest)?;
        let bytes = decoded
            .try_into()
            .map_err(|_| ExecutionContractError::InvalidOpaqueContractDigest)?;
        Ok(Self(bytes))
    }
}

impl From<OpaqueContractDigestV1> for String {
    fn from(value: OpaqueContractDigestV1) -> Self {
        value.to_string()
    }
}

impl Serialize for OpaqueContractDigestV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for OpaqueContractDigestV1 {
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

/// A proof-carrying wrapper over an [`ExecutionId`] whose value has been shown to
/// equal the canonical hash of its execution contract.
///
/// `VerifiedExecutionId` proves the execution id equals the canonical hash of its
/// execution contract. It does NOT prove signature, provenance, acceptance, or
/// that it was measured.
///
/// It cannot be minted from a bare [`ExecutionId`]: it has a private field and no
/// public constructor — no `new`, no `From<ExecutionId>`, no `TryFrom<String>`,
/// no `Deserialize`, and it is never produced directly from
/// [`ExecutionContractV1::compute_execution_id`] (which stays a pure hash). The
/// **only** two ways to obtain one are:
///
/// * [`ExecutionContractEnvelopeV1::verified_execution_id`], which re-derives the
///   canonical hash via [`ExecutionContractEnvelopeV1::verify`] and only then
///   wraps the now-proven stored id; and
/// * [`FinalizedExecution::verified_execution_id`](crate::execution_contract_finalize::FinalizedExecution::verified_execution_id),
///   where a completed strict finalization (RFC §4.6) has already proven the
///   measured facets equal the contract, so the issued id is canonical by
///   construction.
///
/// Because it can only come from one of those two paths, an API that takes a
/// `&VerifiedExecutionId` — [`select_snapshots`](super::snapshot_manifest::select_snapshots)
/// and legacy [`migrate`](super::snapshot_manifest::LegacyReadyStateManifestV1::migrate)
/// in [`super::snapshot_manifest`] — statically refuses a raw, unproven
/// `ExecutionId`.
///
/// A raw `ExecutionId` cannot be wrapped (there is no public constructor):
///
/// ```compile_fail
/// use capsule::execution_contract::{ExecutionId, VerifiedExecutionId};
///
/// let raw = ExecutionId::new(format!("blake3:{}", "0".repeat(64))).unwrap();
/// // The field is private and there is no `new`/`From`/`TryFrom`/`Deserialize`.
/// let _wrapped = VerifiedExecutionId { execution_id: raw };
/// ```
///
/// and a raw `&ExecutionId` cannot stand in for a `&VerifiedExecutionId` at a
/// selection call site:
///
/// ```compile_fail
/// use capsule::execution_contract::ExecutionId;
/// use capsule::snapshot_manifest::{select_snapshots, HostRestoreCapabilityV1, SnapshotCandidate};
///
/// let raw = ExecutionId::new(format!("blake3:{}", "0".repeat(64))).unwrap();
/// let host: HostRestoreCapabilityV1 = unimplemented!();
/// let candidates: Vec<SnapshotCandidate> = Vec::new();
/// // A raw &ExecutionId is not a &VerifiedExecutionId: this is a type error.
/// let _ = select_snapshots(&raw, &host, &candidates);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedExecutionId {
    execution_id: ExecutionId,
}

impl VerifiedExecutionId {
    /// The proven execution id: the canonical hash of the execution contract it
    /// was derived from.
    pub fn as_execution_id(&self) -> &ExecutionId {
        &self.execution_id
    }

    /// Crate-internal, **proof-preserving** construction seam. NOT public and
    /// NOT a third "way to obtain" a `VerifiedExecutionId`: it exists only so the
    /// two sanctioned paths — [`ExecutionContractEnvelopeV1::verified_execution_id`]
    /// and
    /// [`FinalizedExecution::verified_execution_id`](crate::execution_contract_finalize::FinalizedExecution::verified_execution_id)
    /// — can build the wrapper across module boundaries without a public
    /// constructor.
    ///
    /// Unlike a bare wrap, this seam **re-derives** the canonical execution id
    /// from `contract` and compares it to the caller-supplied `execution_id`,
    /// failing closed with [`ExecutionContractError::ExecutionIdMismatch`] on any
    /// disagreement. A caller (or a test using a fake id) therefore cannot mint a
    /// wrapper whose id differs from its contract's hash: the proof is recomputed
    /// here, not trusted.
    ///
    /// Scoped to `crate::contract` (not the whole crate): the only callers are
    /// the two sanctioned minting methods and the in-module tests, all of which
    /// live under this module tree. Narrowing the visibility keeps the "exactly
    /// two ways to obtain a verified id" guarantee from widening to any future
    /// capsule-crate module.
    pub(in crate::contract) fn verify_contract_id(
        contract: &ExecutionContractV1,
        execution_id: &ExecutionId,
    ) -> Result<Self, ExecutionContractError> {
        let computed = contract.compute_execution_id()?;
        if computed != *execution_id {
            return Err(ExecutionContractError::ExecutionIdMismatch {
                stored: execution_id.to_string(),
                computed: computed.to_string(),
            });
        }
        Ok(Self {
            execution_id: execution_id.clone(),
        })
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
/// * no backslash and no Unicode control character (`char::is_control`: the C0
///   range incl. NUL, DEL, and the C1 range U+0080..=U+009F).
///
/// Ordering and equality are **segment-wise** (path components compared
/// component-by-component); **within each segment the comparison is UTF-8 byte
/// lexicographic** (normative — see the module-level "Canonical guest paths"
/// section). This is the canonical sort order the identity lists
/// (`writable_paths`) are validated against. The decomposition into segments is
/// a bijection with the accepted spelling, so segment-wise `Ord` stays
/// consistent with the derived `PartialEq`/`Eq`.
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
        // Reject a backslash and *every* Unicode control character
        // (`char::is_control`: the C0 range incl. NUL, DEL, and the C1 range
        // U+0080..=U+009F). A byte-wise C0/DEL-only check would wrongly accept
        // C1 controls such as U+0085 (NEL), so scan by `char`.
        if value.chars().any(|ch| ch == '\\' || ch.is_control()) {
            return Err(Bad(
                "must not contain a backslash, NUL, or Unicode control character",
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
        //
        // Rust's `str: Ord` compares by UTF-8 bytes, which for valid UTF-8
        // equals Unicode code-point order. This is the normative rule
        // (Option A): a cross-language implementation MUST compare UTF-8 bytes
        // per segment, NOT UTF-16 code units — the two diverge for astral-plane
        // characters (e.g. U+E000 precedes U+10000 by UTF-8/code point but
        // follows it by UTF-16). Pinned by the guest-path-utf8-order /
        // invalid-guest-path-utf16-order vectors.
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
    #[error(
        "opaque sub-contract digest must be blake3:<64 lowercase hex characters> \
         (sha256 is not a valid opaque digest algorithm)"
    )]
    InvalidOpaqueContractDigest,
    #[error("guest path is not canonical: {0}")]
    InvalidGuestPath(&'static str),
    #[error("stored execution_id {stored} does not match the canonical hash {computed}")]
    ExecutionIdMismatch { stored: String, computed: String },
    #[error("failed to canonicalize execution contract: {0}")]
    Canonicalization(String),
    #[error(
        "launch.environment name '{0}' is also declared in launch.secret_bindings; \
         a secret must never be committed as a non-secret environment value"
    )]
    EnvironmentNameIsSecretBinding(String),
    #[error("non-secret environment value payload is not canonical: {0}")]
    InvalidEnvironmentValuePayload(&'static str),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionContractV1 {
    pub schema: String,
    pub source: ResolvedSourceContract,
    pub target: ResolvedTargetContract,
    pub runtime: ResolvedArtifactContract,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list"
    )]
    pub dependencies: Vec<ResolvedDependencyContract>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list"
    )]
    pub build_outputs: Vec<ResolvedBuildOutputContract>,
    pub launch: ResolvedLaunchContract,
    pub filesystem: ResolvedFilesystemContract,
    pub policy: ResolvedPolicyContract,
    pub guest_surface: GuestSurfaceContract,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list"
    )]
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
    pub projection_digest: OpaqueContractDigestV1,
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
    pub dynamic_contract_digest: OpaqueContractDigestV1,
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
    pub projection_digest: OpaqueContractDigestV1,
}

/// Resolved launch facet. `argv` is the exact entrypoint: `argv[0]` names a
/// non-empty program while later elements may be empty strings. `cwd` is the
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
    pub process_model_digest: OpaqueContractDigestV1,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list"
    )]
    pub environment: Vec<EnvironmentVariableContract>,
    pub environment_policy_digest: OpaqueContractDigestV1,
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
    pub value_digest: OpaqueContractDigestV1,
}

/// Canonical schema string for a v1 non-secret environment value payload. It is
/// exactly the [`OpaqueContractDomainV1::EnvironmentValue`] domain, so the
/// payload is self-describing under the identity its digest commits.
pub const ENVIRONMENT_VALUE_PAYLOAD_V1_SCHEMA: &str =
    OpaqueContractDomainV1::EnvironmentValue.as_str();

/// The only value encoding accepted by v1 non-secret environment value payloads.
pub const ENVIRONMENT_VALUE_PAYLOAD_V1_ENCODING: &str = "utf8";

/// A versioned, self-describing non-secret environment value payload (RFC §4.3).
///
/// A committed value is `blake3(UTF8(domain) || 0x00 || JCS(payload))` over the
/// JCS of *this typed payload* under [`OpaqueContractDomainV1::EnvironmentValue`]
/// — never a raw producer-chosen JSON value. `deny_unknown_fields` plus serde's
/// derived duplicate-field rejection make the on-the-wire spelling single-valued,
/// so two producers (or two languages) can neither derive different digests for
/// the same value nor silently drop a duplicated property. The payload schema is
/// pinned (`ato.environment-value/v1`) and the encoding is fixed to `utf8`;
/// [`Self::validate`] rejects any other spelling fail-closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentValuePayloadV1 {
    pub schema: String,
    pub encoding: String,
    pub value: String,
}

impl EnvironmentValuePayloadV1 {
    /// Build a canonical UTF-8 environment value payload.
    pub fn utf8(value: impl Into<String>) -> Self {
        Self {
            schema: ENVIRONMENT_VALUE_PAYLOAD_V1_SCHEMA.to_string(),
            encoding: ENVIRONMENT_VALUE_PAYLOAD_V1_ENCODING.to_string(),
            value: value.into(),
        }
    }

    /// Fail closed on any non-canonical schema/encoding spelling.
    pub fn validate(&self) -> Result<(), ExecutionContractError> {
        if self.schema != ENVIRONMENT_VALUE_PAYLOAD_V1_SCHEMA {
            return Err(ExecutionContractError::InvalidEnvironmentValuePayload(
                "schema must be ato.environment-value/v1",
            ));
        }
        if self.encoding != ENVIRONMENT_VALUE_PAYLOAD_V1_ENCODING {
            return Err(ExecutionContractError::InvalidEnvironmentValuePayload(
                "encoding must be utf8",
            ));
        }
        Ok(())
    }
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
    pub topology_digest: OpaqueContractDigestV1,
    pub readonly_layers: Vec<ContentDigest>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list"
    )]
    pub writable_paths: Vec<GuestPath>,
}

/// Resolved policy facet. All three digests are opaque sub-contract digests
/// ([`OpaqueContractDigestV1`]) under the same normative preimage rule as the
/// other opaque facets — `network_digest` uses domain
/// [`OpaqueContractDomainV1::NetworkPolicy`] (ingress/egress/DNS/isolation),
/// `capability_digest` uses [`OpaqueContractDomainV1::CapabilityPolicy`]
/// (host/device/sandbox capabilities), and `filesystem_digest` uses
/// [`OpaqueContractDomainV1::FilesystemPolicy`]. The preimages are **not**
/// producer-defined: the domain and rule
/// (`blake3(UTF8(domain) || 0x00 || JCS(payload))`) are frozen by v1, while
/// each policy payload schema is versioned separately from v1 (RFC §4.5),
/// exactly like the other opaque facets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedPolicyContract {
    pub network_digest: OpaqueContractDigestV1,
    pub capability_digest: OpaqueContractDigestV1,
    pub filesystem_digest: OpaqueContractDigestV1,
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
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list"
    )]
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
    /// Non-identity parent-association CLAIM naming the Capsule Program
    /// declaration this contract was resolved from (ADR-014 §5). Excluded
    /// from `execution_id` by construction (it is not part of the embedded
    /// contract); verified pairwise via
    /// [`crate::capsule_program_contract::verify_program_parent`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capsule_program_id: Option<crate::contract::capsule_program_contract::CapsuleProgramId>,
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

    /// Obtain a [`VerifiedExecutionId`] from this envelope. Routes through the
    /// proof-preserving [`VerifiedExecutionId::verify_contract_id`] seam, which
    /// re-derives the canonical hash from the embedded contract and fails closed
    /// on any mismatch with the stored `execution_id` before wrapping it. This is
    /// one of the only two ways to obtain a [`VerifiedExecutionId`].
    pub fn verified_execution_id(&self) -> Result<VerifiedExecutionId, ExecutionContractError> {
        VerifiedExecutionId::verify_contract_id(&self.execution_contract, &self.execution_id)
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
        if self.launch.argv.is_empty() || self.launch.argv[0].trim().is_empty() {
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
        // `secret_bindings` is the AUTHORITATIVE secret set (RFC §4.3): a variable
        // may not be both a committed non-secret env value and a secret binding.
        // The name heuristic (`is_sensitive_env_key`) is defense-in-depth, not the
        // boundary — it misses names like `DATABASE_URL` / `AWS_ACCESS_KEY_ID` that
        // only `secret_bindings` captures.
        {
            let secret_bindings: BTreeSet<&str> = self
                .launch
                .secret_bindings
                .iter()
                .map(String::as_str)
                .collect();
            if let Some(name) = self
                .launch
                .environment
                .iter()
                .map(|variable| variable.name.as_str())
                .find(|name| secret_bindings.contains(name))
            {
                return Err(ExecutionContractError::EnvironmentNameIsSecretBinding(
                    name.to_string(),
                ));
            }
        }
        // REQUIRED and non-empty (ADR-015 §6.3): an execution with no immutable
        // layer has no filesystem to identify, so an empty list here is not a
        // capsule with nothing mounted — it is a contract that failed to record
        // what it is.
        if self.filesystem.readonly_layers.is_empty() {
            return Err(ExecutionContractError::UnresolvedField(
                "filesystem.readonly_layers",
            ));
        }
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
        ExecutionId::new(schema_domained_blake3_id(
            EXECUTION_CONTRACT_V1_SCHEMA,
            &canonical,
        ))
    }
}

/// The frozen v1 structural-id preimage rule shared by every schema-scoped
/// content address in this crate:
///
/// ```text
/// id = "blake3:" + hex(BLAKE3(UTF8(schema) || 0x00 || JCS(payload)))
/// ```
///
/// The `schema` string is the domain separator (so two payloads that happen to
/// canonicalize to identical bytes under different schemas never collide), the
/// `0x00` byte separates the domain from the payload unambiguously, and
/// `canonical_payload` is the already-JCS-serialized payload. This is the single
/// definition of the rule used by both [`ExecutionContractV1::compute_execution_id`]
/// (domain `ato.execution-contract/v1`) and the `ato.snapshot-manifest/v1`
/// `snapshot_id` derivation (see [`super::snapshot_manifest`]); neither reinvents
/// it. Callers derive `id` *from* a payload that MUST NOT itself contain `id`, so
/// the address is never part of its own preimage.
pub fn schema_domained_blake3_id(schema: &str, canonical_payload: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(schema.as_bytes());
    hasher.update(&[0]);
    hasher.update(canonical_payload);
    format!("blake3:{}", hasher.finalize().to_hex())
}

/// Compute an opaque sub-contract digest under the pinned v1 preimage rule:
///
/// ```text
/// digest = blake3(UTF8(domain) || 0x00 || JCS(payload))
/// ```
///
/// `domain` is a typed [`OpaqueContractDomainV1`] — never a free `&str` — so a
/// producer can neither invent nor misspell a domain. `payload` is the
/// self-describing sub-contract payload whose *schema is versioned separately
/// from v1*. The preimage rule and BLAKE3 algorithm are fixed by v1; the
/// payload schema is not, so a later PR can define the payload without a v1
/// identity change (RFC §4.5). The returned [`OpaqueContractDigestV1`] is what a
/// producer stores in the matching `*_digest` / `value_digest` field.
///
/// Returns [`ExecutionContractError::Canonicalization`] if the payload cannot
/// be JCS-serialized (fail-closed: an unserializable payload never yields a
/// digest).
pub fn opaque_subcontract_digest<T>(
    domain: OpaqueContractDomainV1,
    payload: &T,
) -> Result<OpaqueContractDigestV1, ExecutionContractError>
where
    T: Serialize,
{
    let canonical = serde_jcs::to_vec(payload)
        .map_err(|error| ExecutionContractError::Canonicalization(error.to_string()))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain.as_str().as_bytes());
    hasher.update(&[0]);
    hasher.update(&canonical);
    Ok(OpaqueContractDigestV1::new(*hasher.finalize().as_bytes()))
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
    present_non_empty_list(deserializer)
}

/// The same rule for any element type: an ABSENT optional collection omits its
/// key, so an explicit `[]` is a second spelling of the same value.
///
/// Two spellings of one value would canonicalize to two documents and therefore
/// two `execution_id`s for one execution — which is the one thing the canonical
/// form exists to prevent (ADR-015 §6.3).
fn present_non_empty_list<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    let list = Vec::deserialize(deserializer)?;
    if list.is_empty() {
        return Err(serde::de::Error::custom(
            "absent optional identity collections must omit the key (explicit [] is non-canonical)",
        ));
    }
    Ok(list)
}

/// A minimal, valid [`ExecutionContractV1`] for cross-module tests, seeded so
/// distinct `seed` values derive distinct canonical `execution_id`s. The
/// snapshot-manifest selection tests use it to mint a real
/// [`VerifiedExecutionId`] through the proof-preserving
/// [`VerifiedExecutionId::verify_contract_id`] seam — computing the id from a
/// real contract instead of wrapping a synthetic id.
#[cfg(test)]
pub(in crate::contract) fn test_execution_contract(seed: u8) -> ExecutionContractV1 {
    let content = |byte: u8| ContentDigest::new(DigestAlgorithm::Blake3, [byte; 32]);
    let opaque = |byte: u8| OpaqueContractDigestV1::new([byte; 32]);
    let path = |value: &str| GuestPath::parse(value).expect("canonical guest path");
    ExecutionContractV1 {
        schema: EXECUTION_CONTRACT_V1_SCHEMA.to_string(),
        source: ResolvedSourceContract {
            digest: content(seed),
            projection_digest: opaque(seed ^ 0x11),
        },
        target: ResolvedTargetContract {
            os: "linux".to_string(),
            architecture: "x86_64".to_string(),
            abi: "gnu".to_string(),
            libc: Some("glibc-2.39".to_string()),
            observable_features: std::collections::BTreeMap::new(),
        },
        runtime: ResolvedArtifactContract {
            kind: "node".to_string(),
            digest: content(seed ^ 0x22),
            dynamic_contract_digest: opaque(seed ^ 0x33),
        },
        dependencies: vec![ResolvedDependencyContract {
            name: "npm".to_string(),
            derivation_digest: content(seed ^ 0x44),
            output_digest: content(seed ^ 0x55),
        }],
        build_outputs: vec![ResolvedBuildOutputContract {
            name: "app".to_string(),
            digest: content(seed ^ 0x66),
            projection_digest: opaque(seed ^ 0x77),
        }],
        launch: ResolvedLaunchContract {
            argv: vec!["node".to_string(), "dist/server.js".to_string()],
            cwd: path("/workspace"),
            process_model_digest: opaque(seed ^ 0x88),
            environment: vec![EnvironmentVariableContract {
                name: "NODE_ENV".to_string(),
                value_digest: opaque(seed ^ 0x99),
            }],
            environment_policy_digest: opaque(seed ^ 0xaa),
            secret_bindings: vec!["API_TOKEN".to_string()],
        },
        filesystem: ResolvedFilesystemContract {
            view_digest: content(seed ^ 0xbb),
            topology_digest: opaque(seed ^ 0xcc),
            readonly_layers: vec![content(seed ^ 0xdd)],
            writable_paths: vec![path("/tmp")],
        },
        policy: ResolvedPolicyContract {
            network_digest: opaque(seed ^ 0xee),
            capability_digest: opaque(seed ^ 0xf0),
            filesystem_digest: opaque(seed ^ 0x0f),
        },
        guest_surface: GuestSurfaceContract {
            bind_address: "0.0.0.0".to_string(),
            protocol: "ato-guest/v1".to_string(),
            port: Some(std::num::NonZeroU16::new(8080).unwrap()),
            features: vec!["bindings".to_string(), "exec".to_string()],
        },
        external_state: vec![ExternalStateContract {
            name: "data".to_string(),
            target: path("/data"),
            access: ExternalStateAccess::ReadWrite,
            schema: "1".to_string(),
            snapshot: SnapshotExclusion::Exclude,
        }],
    }
}

#[cfg(test)]
mod tests {

    /// Every optional identity collection has exactly ONE spelling for absence:
    /// the key is omitted. `[]` is rejected on the way in and never produced on
    /// the way out.
    ///
    /// Before ADR-015 §6.3 these six serialized `[]` when empty, which is a
    /// second spelling of the same value — two canonical documents, two
    /// `execution_id`s, one execution. The rule was applied to code and vectors
    /// together, before anything minted in production, because afterwards it
    /// would be a contract version.
    #[test]
    fn every_optional_collection_omits_its_key_when_empty() {
        let mut contract = super::test_execution_contract(1);
        contract.dependencies.clear();
        contract.build_outputs.clear();
        contract.launch.environment.clear();
        contract.launch.secret_bindings.clear();
        contract.filesystem.writable_paths.clear();
        contract.guest_surface.features.clear();
        contract.external_state.clear();
        contract.target.observable_features.clear();

        let json = serde_json::to_value(&contract).expect("serialize");
        for key in ["dependencies", "build_outputs", "external_state"] {
            assert!(json.get(key).is_none(), "{key} must be omitted when empty");
        }
        for (parent, key) in [
            ("launch", "environment"),
            ("launch", "secret_bindings"),
            ("filesystem", "writable_paths"),
            ("guest_surface", "features"),
            ("target", "observable_features"),
        ] {
            assert!(
                json[parent].get(key).is_none(),
                "{parent}.{key} must be omitted when empty"
            );
        }

        // And an explicit `[]` on the way back in is refused rather than
        // normalized — normalizing it is precisely how two languages derive two
        // ids for one execution.
        for pointer in [
            "/dependencies",
            "/build_outputs",
            "/external_state",
            "/launch/environment",
            "/launch/secret_bindings",
            "/filesystem/writable_paths",
            "/guest_surface/features",
        ] {
            let mut spelled = json.clone();
            let (parent, key) = pointer.rsplit_once('/').expect("pointer");
            let target = if parent.is_empty() {
                &mut spelled
            } else {
                spelled.pointer_mut(parent).expect("parent")
            };
            target[key] = serde_json::json!([]);
            assert!(
                serde_json::from_value::<ExecutionContractV1>(spelled).is_err(),
                "an explicit [] at {pointer} must be refused"
            );
        }
    }

    /// `filesystem.readonly_layers` is REQUIRED and non-empty: an execution with
    /// no immutable layer has no filesystem to identify, so an empty list is a
    /// contract that failed to record what it is — not a capsule with nothing
    /// mounted.
    #[test]
    fn readonly_layers_must_not_be_empty() {
        let mut contract = super::test_execution_contract(1);
        contract.filesystem.readonly_layers.clear();
        assert!(matches!(
            contract.validate(),
            Err(ExecutionContractError::UnresolvedField(
                "filesystem.readonly_layers"
            ))
        ));
    }

    /// The rule MOVES the identity for a contract with an empty collection —
    /// which is why it had to land before anything minted in production.
    #[test]
    fn omitting_an_empty_collection_changes_the_execution_id() {
        let populated = super::test_execution_contract(1);
        let mut emptied = populated.clone();
        emptied.external_state.clear();
        assert_ne!(
            populated.compute_execution_id().unwrap(),
            emptied.compute_execution_id().unwrap()
        );
        // The emptied contract still canonicalizes — it simply says less.
        let bytes = String::from_utf8(emptied.canonical_bytes().unwrap()).unwrap();
        assert!(
            !bytes.contains("external_state"),
            "the key is gone, not empty: {bytes}"
        );
    }
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;

    /// Baseline `execution_id` for [`sample_contract`]. Kept in lockstep with
    /// the shared fixture baseline (`manifest.json` / the vector generator).
    const BASELINE_EXECUTION_ID: &str =
        "blake3:fe216ebfbc65450f1cca14ca3ff41de81c42467fa672e7a7471d3392b058f464";

    fn digest(algorithm: DigestAlgorithm, byte: u8) -> ContentDigest {
        ContentDigest::new(algorithm, [byte; 32])
    }

    fn opaque_digest(byte: u8) -> OpaqueContractDigestV1 {
        OpaqueContractDigestV1::new([byte; 32])
    }

    fn guest_path(value: &str) -> GuestPath {
        GuestPath::parse(value).expect("canonical guest path")
    }

    fn sample_contract() -> ExecutionContractV1 {
        ExecutionContractV1 {
            schema: EXECUTION_CONTRACT_V1_SCHEMA.to_string(),
            source: ResolvedSourceContract {
                digest: digest(DigestAlgorithm::Sha256, 1),
                projection_digest: opaque_digest(0x0c),
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
                dynamic_contract_digest: opaque_digest(0x0d),
            },
            dependencies: vec![ResolvedDependencyContract {
                name: "npm".to_string(),
                derivation_digest: digest(DigestAlgorithm::Blake3, 3),
                output_digest: digest(DigestAlgorithm::Blake3, 4),
            }],
            build_outputs: vec![ResolvedBuildOutputContract {
                name: "app".to_string(),
                digest: digest(DigestAlgorithm::Blake3, 5),
                projection_digest: opaque_digest(0x0e),
            }],
            launch: ResolvedLaunchContract {
                argv: vec!["node".to_string(), "dist/server.js".to_string()],
                cwd: guest_path("/workspace"),
                process_model_digest: opaque_digest(0x0f),
                environment: vec![EnvironmentVariableContract {
                    name: "NODE_ENV".to_string(),
                    value_digest: opaque_digest(6),
                }],
                environment_policy_digest: opaque_digest(0x10),
                secret_bindings: vec!["API_TOKEN".to_string()],
            },
            filesystem: ResolvedFilesystemContract {
                view_digest: digest(DigestAlgorithm::Blake3, 7),
                topology_digest: opaque_digest(0x11),
                readonly_layers: vec![digest(DigestAlgorithm::Blake3, 8)],
                writable_paths: vec![guest_path("/tmp")],
            },
            policy: ResolvedPolicyContract {
                network_digest: opaque_digest(9),
                capability_digest: opaque_digest(10),
                filesystem_digest: opaque_digest(11),
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
        // projection, launch process model, environment policy, environment
        // value, filesystem mount topology + access modes, and the three
        // network/capability/filesystem policies) must be in the identity set:
        // mutating its digest must change execution_id, and pairwise-distinctly
        // so.
        let baseline = sample_contract();
        let baseline_id = baseline.compute_execution_id().unwrap();

        type Mutation = (&'static str, fn(&mut ExecutionContractV1));
        let mutations: [Mutation; 10] = [
            ("source.projection_digest", |contract| {
                contract.source.projection_digest = OpaqueContractDigestV1::new([0xac; 32]);
            }),
            ("runtime.dynamic_contract_digest", |contract| {
                contract.runtime.dynamic_contract_digest = OpaqueContractDigestV1::new([0xad; 32]);
            }),
            ("build_outputs[].projection_digest", |contract| {
                contract.build_outputs[0].projection_digest =
                    OpaqueContractDigestV1::new([0xae; 32]);
            }),
            ("launch.process_model_digest", |contract| {
                contract.launch.process_model_digest = OpaqueContractDigestV1::new([0xaf; 32]);
            }),
            ("launch.environment_policy_digest", |contract| {
                contract.launch.environment_policy_digest = OpaqueContractDigestV1::new([0xb0; 32]);
            }),
            ("launch.environment[].value_digest", |contract| {
                contract.launch.environment[0].value_digest =
                    OpaqueContractDigestV1::new([0xb2; 32]);
            }),
            ("filesystem.topology_digest", |contract| {
                contract.filesystem.topology_digest = OpaqueContractDigestV1::new([0xb1; 32]);
            }),
            ("policy.network_digest", |contract| {
                contract.policy.network_digest = OpaqueContractDigestV1::new([0xb3; 32]);
            }),
            ("policy.capability_digest", |contract| {
                contract.policy.capability_digest = OpaqueContractDigestV1::new([0xb4; 32]);
            }),
            ("policy.filesystem_digest", |contract| {
                contract.policy.filesystem_digest = OpaqueContractDigestV1::new([0xb5; 32]);
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
            capsule_program_id: None,
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
            capsule_program_id: None,
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
    fn verified_execution_id_is_obtainable_from_a_valid_envelope() {
        let contract = sample_contract();
        let execution_id = contract.compute_execution_id().unwrap();
        let envelope = ExecutionContractEnvelopeV1 {
            execution_contract: contract,
            execution_id: execution_id.clone(),
            capsule_program_id: None,
            resolved_refs: ResolvedRefProvenanceV1::default(),
            generated_at: None,
            provenance: serde_json::Value::Null,
            diagnostics: serde_json::Value::Null,
            evidence: serde_json::Value::Null,
        };

        let verified = envelope
            .verified_execution_id()
            .expect("a matching envelope yields a verified id");
        assert_eq!(*verified.as_execution_id(), execution_id);
    }

    #[test]
    fn verified_execution_id_is_refused_from_a_tampered_envelope() {
        // A stored id that is not the canonical hash of the contract must never
        // yield a VerifiedExecutionId — verify() fails closed first.
        let contract = sample_contract();
        let envelope = ExecutionContractEnvelopeV1 {
            execution_contract: contract,
            execution_id: ExecutionId::new(format!("blake3:{}", "0".repeat(64))).unwrap(),
            capsule_program_id: None,
            resolved_refs: ResolvedRefProvenanceV1::default(),
            generated_at: None,
            provenance: serde_json::Value::Null,
            diagnostics: serde_json::Value::Null,
            evidence: serde_json::Value::Null,
        };

        assert!(matches!(
            envelope.verified_execution_id(),
            Err(ExecutionContractError::ExecutionIdMismatch { .. })
        ));
    }

    #[test]
    fn verify_contract_id_recomputes_and_rejects_a_mismatch() {
        // The proof-preserving seam must recompute the id from the contract and
        // reject any supplied id that is not the canonical hash — a fake id can
        // never mint a VerifiedExecutionId.
        let contract = sample_contract();
        let real_id = contract.compute_execution_id().unwrap();

        // The correct id wraps.
        let verified = VerifiedExecutionId::verify_contract_id(&contract, &real_id)
            .expect("matching id is proof-preserving");
        assert_eq!(*verified.as_execution_id(), real_id);

        // A different (fake) id fails closed.
        let fake_id = ExecutionId::new(format!("blake3:{}", "0".repeat(64))).unwrap();
        assert_ne!(fake_id, real_id);
        assert!(matches!(
            VerifiedExecutionId::verify_contract_id(&contract, &fake_id),
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
        for good in [
            "/workspace",
            "/data/ユーザー",
            "/a/b/c",
            "/tmp",
            "/\u{e000}",  // private-use BMP char (not a control char)
            "/\u{10000}", // astral-plane char (not a control char)
        ] {
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
            ("/data\u{7}x", "C0 control char"),
            ("/data\u{7f}x", "DEL control char"),
            ("/data\u{85}x", "C1 control char (U+0085 NEL)"),
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
    fn guest_path_ordering_is_utf8_byte_order_not_utf16() {
        // U+E000 (private-use, BMP) vs U+10000 (astral). Under UTF-8 / code
        // point order, U+E000 < U+10000 (first bytes 0xEE < 0xF0). Under UTF-16
        // code-unit order they invert (U+10000's lead surrogate 0xD800 < the
        // single unit 0xE000). The normative rule is UTF-8 byte order, so the
        // private-use char MUST sort first — the same order Rust's `str: Ord`
        // and the shared guest-path-utf8-order vector encode.
        let private_use = guest_path("/\u{e000}");
        let astral = guest_path("/\u{10000}");
        assert!(private_use < astral);
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
        let got = opaque_subcontract_digest(OpaqueContractDomainV1::EnvironmentValue, &payload)
            .expect("digest computes");

        let canonical = serde_jcs::to_vec(&payload).unwrap();
        let mut preimage = OpaqueContractDomainV1::EnvironmentValue
            .as_str()
            .as_bytes()
            .to_vec();
        preimage.push(0);
        preimage.extend(canonical);
        let expected = OpaqueContractDigestV1::new(*blake3::hash(&preimage).as_bytes());

        assert_eq!(got, expected);
        // The wire form is BLAKE3-only.
        assert_eq!(
            got.to_string(),
            format!("blake3:{}", hex::encode(got.bytes()))
        );
        // Domain separation: the same payload under a different domain differs.
        let other = opaque_subcontract_digest(OpaqueContractDomainV1::FilesystemTopology, &payload)
            .unwrap();
        assert_ne!(got, other);
    }

    #[test]
    fn opaque_contract_digest_rejects_sha256_and_wrong_shapes() {
        // A sha256-spelled value is not a valid opaque sub-contract digest:
        // the algorithm is fixed to blake3 at the type level.
        for invalid in [
            format!("sha256:{}", "ab".repeat(32)),
            format!("blake3:{}", "A".repeat(64)), // uppercase hex is non-canonical
            format!("blake3:{}", "aa".repeat(31)), // too short
            "blake3:not-hex".to_string(),
            "aa".repeat(32), // missing algorithm prefix
        ] {
            assert!(
                OpaqueContractDigestV1::try_from(invalid.clone()).is_err(),
                "{invalid}"
            );
        }
        // The canonical blake3 spelling round-trips.
        let ok = format!("blake3:{}", "0c".repeat(32));
        let parsed =
            OpaqueContractDigestV1::try_from(ok.clone()).expect("valid blake3 opaque digest");
        assert_eq!(parsed.to_string(), ok);

        // Fail-closed at the struct layer too: a sha256-spelled opaque field
        // must reject on deserialization (representative field: policy.network_digest).
        let mut value = serde_json::to_value(sample_contract()).unwrap();
        value["policy"]["network_digest"] =
            serde_json::json!(format!("sha256:{}", "ab".repeat(32)));
        assert!(serde_json::from_value::<ExecutionContractV1>(value).is_err());
    }

    #[test]
    fn env_name_that_is_a_secret_binding_fails_validation_even_when_heuristic_misses() {
        // Blocker 3 layer 1: secret_bindings is the authoritative secret set. A
        // name the heuristic does NOT flag (DATABASE_URL, AWS_ACCESS_KEY_ID) that
        // is both a committed env value AND a secret binding must fail validation.
        for name in ["DATABASE_URL", "AWS_ACCESS_KEY_ID"] {
            let mut contract = sample_contract();
            contract.launch.environment = vec![EnvironmentVariableContract {
                name: name.to_string(),
                value_digest: opaque_digest(6),
            }];
            contract.launch.secret_bindings = vec![name.to_string()];
            assert_eq!(
                contract.validate(),
                Err(ExecutionContractError::EnvironmentNameIsSecretBinding(
                    name.to_string()
                )),
                "{name}"
            );
        }
    }

    #[test]
    fn environment_value_payload_rejects_unknown_and_duplicate_and_non_canonical() {
        // Major 2: the typed env value payload is single-valued on the wire.
        // Unknown field ⇒ rejected (deny_unknown_fields).
        assert!(
            serde_json::from_str::<EnvironmentValuePayloadV1>(
                r#"{"schema":"ato.environment-value/v1","encoding":"utf8","value":"x","extra":1}"#,
            )
            .is_err()
        );
        // Duplicate property ⇒ rejected (serde's derived duplicate-field guard).
        assert!(
            serde_json::from_str::<EnvironmentValuePayloadV1>(
                r#"{"schema":"ato.environment-value/v1","encoding":"utf8","value":"x","value":"y"}"#,
            )
            .is_err()
        );
        // Canonical payload round-trips and validates.
        let payload = EnvironmentValuePayloadV1::utf8("x");
        let parsed: EnvironmentValuePayloadV1 =
            serde_json::from_str(&serde_json::to_string(&payload).unwrap()).unwrap();
        assert_eq!(parsed, payload);
        payload.validate().expect("canonical payload validates");
        // Wrong schema / encoding are rejected fail-closed.
        assert!(
            EnvironmentValuePayloadV1 {
                schema: "ato.environment-value/v2".to_string(),
                encoding: "utf8".to_string(),
                value: "x".to_string(),
            }
            .validate()
            .is_err()
        );
        assert!(
            EnvironmentValuePayloadV1 {
                schema: ENVIRONMENT_VALUE_PAYLOAD_V1_SCHEMA.to_string(),
                encoding: "base64".to_string(),
                value: "x".to_string(),
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn environment_value_digest_is_taken_over_the_typed_payload() {
        // Major 2: the committed digest is blake3(domain || 0 || JCS(typed
        // payload)) — exactly the opaque sub-contract digest of the typed payload.
        let payload = EnvironmentValuePayloadV1::utf8("production");
        let via_helper =
            crate::execution_contract_finalize::environment_value_digest(&payload).unwrap();
        let via_opaque =
            opaque_subcontract_digest(OpaqueContractDomainV1::EnvironmentValue, &payload).unwrap();
        assert_eq!(via_helper, via_opaque);
        // A different value yields a different digest.
        let other = EnvironmentValuePayloadV1::utf8("staging");
        assert_ne!(
            via_helper,
            crate::execution_contract_finalize::environment_value_digest(&other).unwrap()
        );
    }
}
