//! Layer 2: Contract — `ato.snapshot-manifest/v1` wire type + pure selection.
//!
//! A Snapshot is an immutable restore optimization subordinate to **exactly one**
//! Execution Identity. This module carries the *pure, backend-free* half of the
//! Capsule v1 Snapshot model (issue #1087):
//!
//! * [`SnapshotManifestV1`] — the versioned wire contract. Its identity anchor is
//!   a **required** [`ExecutionId`] (the `ato.execution-contract/v1` id from
//!   G0-1); a manifest without it, or with a malformed one, fails closed at
//!   deserialize.
//! * [`SnapshotCompatibilityContractV1`] — a typed, versioned restore-requirement
//!   structure (backend/format/VMM/codec/kernel/CPU-template/runner/portability).
//!   Unknown compatibility is rejected at deserialize (`deny_unknown_fields` +
//!   closed enums): *unknown compatibility is not compatibility*.
//! * [`SnapshotManifestV1::snapshot_id`] — the content address of the canonical
//!   manifest payload, derived with the frozen G0-1/A1 rule
//!   ([`super::execution_contract::schema_domained_blake3_id`]) under the
//!   `ato.snapshot-manifest/v1` domain. `snapshot_id` is **not** a field of the
//!   manifest, so it is never part of its own preimage.
//! * [`select_snapshots`] — the pure selection function. Its **first** gate is
//!   exact `execution_id` equality against a *verified* [`ExecutionId`]; only then
//!   proven compatibility; only then ranking. Capsule name, target label, source
//!   commit, `capsule_manifest_hash`, recency, and runner name can never
//!   substitute for exact identity.
//! * [`LegacyReadyStateManifestV1`] — an inspection-only view of a legacy
//!   `ato.ready-state/v1` artifact. It is deserializable for inspection and
//!   explicit migration, but is structurally *not* a v1 selection candidate.
//!   [`LegacyReadyStateManifestV1::migrate`] produces a **new** immutable
//!   [`SnapshotManifestV1`] (and therefore a new `snapshot_id`); it never
//!   reinterprets legacy bytes in place.
//! * [`SnapshotCatalogRecord`] — registry acceptance metadata. Quarantine flips a
//!   status field; it never mutates the manifest bytes or the `snapshot_id`.
//!
//! The manifest **commits to** the captured content by carrying its layer refs
//! (`memory_layer_refs` / `vmstate_layer_refs` / `disk_layer_refs`) so
//! `snapshot_id` is a true content address; the *byte-sealing* that produces
//! those layers, and live restore/runner wiring, remain **out of scope** for
//! #1087 (non-goals) and live in the `snapshot` crate, which composes these
//! types. The `cas_root_digest` and other Artifact-Envelope fields
//! (`ato.snapshot-artifact-envelope/v1`) are likewise out of scope here. The
//! normative spec is
//! `docs/rfcs/accepted/CAPSULE_V1_EXECUTION_MODEL_SPEC.md` §7 and §16.3.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::execution_contract::{
    ContentDigest, ExecutionId, VerifiedExecutionId, schema_domained_blake3_id,
};

/// Schema tag for the Capsule v1 Snapshot manifest wire format.
pub const SNAPSHOT_MANIFEST_V1_SCHEMA: &str = "ato.snapshot-manifest/v1";

/// Schema tag for the versioned Snapshot compatibility contract.
pub const SNAPSHOT_COMPATIBILITY_V1_SCHEMA: &str = "ato.snapshot-compatibility/v1";

/// Schema tag for the versioned Snapshot restore contract (restore-time steps).
pub const SNAPSHOT_RESTORE_CONTRACT_V1_SCHEMA: &str = "ato.snapshot-restore-contract/v1";

/// Schema tag for the versioned structural-sanitization attestation.
pub const SNAPSHOT_SANITIZATION_ATTESTATION_V1_SCHEMA: &str =
    "ato.snapshot-sanitization-attestation/v1";

/// Schema tag for the versioned secret-scan attestation.
pub const SNAPSHOT_SECRET_SCAN_ATTESTATION_V1_SCHEMA: &str =
    "ato.snapshot-secret-scan-attestation/v1";

/// Schema tag for the legacy Ready-State manifest (inspection / migration only).
pub const READY_STATE_V1_SCHEMA: &str = "ato.ready-state/v1";

/// Errors from constructing, validating, deriving, or migrating a Snapshot
/// manifest. Every variant is fail-closed: a manifest that cannot be proven
/// well-formed never yields a `snapshot_id` and is never selectable.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SnapshotManifestError {
    #[error("snapshot manifest schema must be {SNAPSHOT_MANIFEST_V1_SCHEMA}")]
    InvalidSchema,
    #[error("snapshot compatibility schema must be {SNAPSHOT_COMPATIBILITY_V1_SCHEMA}")]
    InvalidCompatibilitySchema,
    #[error("snapshot restore-contract schema must be {SNAPSHOT_RESTORE_CONTRACT_V1_SCHEMA}")]
    InvalidRestoreContractSchema,
    #[error(
        "snapshot sanitization-attestation schema must be \
         {SNAPSHOT_SANITIZATION_ATTESTATION_V1_SCHEMA}"
    )]
    InvalidSanitizationAttestationSchema,
    #[error(
        "snapshot secret-scan-attestation schema must be \
         {SNAPSHOT_SECRET_SCAN_ATTESTATION_V1_SCHEMA}"
    )]
    InvalidSecretScanAttestationSchema,
    #[error("legacy manifest schema must be {READY_STATE_V1_SCHEMA}")]
    InvalidLegacySchema,
    #[error("failed to canonicalize snapshot manifest: {0}")]
    Canonicalization(String),
    #[error("snapshot manifest field '{0}' must be non-empty")]
    EmptyField(&'static str),
    #[error("snapshot_id must be blake3:<64 lowercase hex characters>")]
    InvalidSnapshotId,
    #[error(
        "restore_contract.restore_protocol must equal \
         compatibility_contract.runner_restore_contract (they are the same restore \
         protocol the runner speaks)"
    )]
    RestoreContractMismatch,
}

/// A Snapshot content address: `blake3:<64 lowercase hex>`.
///
/// It is the domain-separated content address of the canonical manifest payload
/// (see [`SnapshotManifestV1::snapshot_id`]) — never a producer-chosen string.
/// Like [`ExecutionId`], it enforces the `blake3:` prefix and lowercase hex so a
/// malformed address fails closed on read.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SnapshotId(String);

impl SnapshotId {
    pub fn new(value: String) -> Result<Self, SnapshotManifestError> {
        // Distinguish a genuinely empty address (EmptyField) from a malformed
        // but non-empty one (InvalidSnapshotId) — the latter's "must be
        // non-empty" message would be misleading. Mirrors
        // `ExecutionContractError::InvalidExecutionId`.
        if value.is_empty() {
            return Err(SnapshotManifestError::EmptyField("snapshot_id"));
        }
        let Some(hex) = value.strip_prefix("blake3:") else {
            return Err(SnapshotManifestError::InvalidSnapshotId);
        };
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(SnapshotManifestError::InvalidSnapshotId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SnapshotId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for SnapshotId {
    type Error = SnapshotManifestError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SnapshotId> for String {
    fn from(value: SnapshotId) -> Self {
        value.0
    }
}

/// Snapshot backend/format family. A closed enum: an unknown backend spelling
/// fails deserialization (unknown compatibility is not compatibility).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SnapshotBackendKind {
    Firecracker,
    Cloud,
    Qemu,
    Kata,
    /// A backend that captures no VM state (test/dev only). Kept as an explicit
    /// variant rather than a free string so it can never be confused for a real
    /// restore backend at selection time.
    Fake,
}

/// Portability tier of a captured Snapshot. v1 defines exactly **one** provable
/// tier: [`PortabilityTier::ClassPortable`].
///
/// A prior draft also carried a `HostPinned` tier ordered below `ClassPortable`
/// and matched by a `max_portability_tier >= portability_tier` comparison. But
/// v1 defines **no capture-host identity** (RFC §7.4), so a host-pinned tier is
/// unprovable: a merely class-portable host would *falsely* satisfy a
/// host-pinned Snapshot (fail-open), violating the RFC's fail-closed rule. The
/// variant is therefore removed. Because there is a single tier, compatibility
/// is never decided by a tier ranking; it is proven by **exact**
/// `compatibility_class_identity` equality (see
/// [`SnapshotCompatibilityContractV1::is_satisfied_by`]).
///
/// A future host-identity contract MAY reintroduce a *provable* `HostPinned` as
/// a new, versioned tagged variant carrying the capture-host identity a host
/// must present — never as an unprovable ordered tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PortabilityTier {
    /// Restorable on any host that presents the exact matching
    /// `compatibility_class_identity`.
    ClassPortable,
}

/// The versioned Snapshot **compatibility contract**: the restore-only
/// requirements a host MUST prove it satisfies. This replaces and generalizes
/// the legacy `runner_class_id`; `capsule_manifest_hash` is intentionally absent
/// (it is provenance, never a compatibility key).
///
/// `deny_unknown_fields` + closed-enum members make this fail closed: an artifact
/// declaring a compatibility dimension this version does not model is rejected at
/// deserialize rather than silently ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotCompatibilityContractV1 {
    /// Always [`SNAPSHOT_COMPATIBILITY_V1_SCHEMA`]; validated on read.
    pub schema: String,
    /// Snapshot backend family.
    pub backend: SnapshotBackendKind,
    /// Backend state/format version (e.g. Firecracker snapshot format version).
    pub format_version: u32,
    /// VMM identity the state was captured under (e.g. `firecracker-1.7`).
    pub vmm_identity: String,
    /// Machine-state codec contract identity.
    pub state_codec: String,
    /// Guest kernel identity the memory image was captured against.
    pub guest_kernel_identity: String,
    /// CPU template / restore feature-set identity the vmstate requires.
    pub cpu_template: String,
    /// Runner restore-contract identity (the restore protocol the runner speaks).
    pub runner_restore_contract: String,
    /// Portability tier. v1 has exactly one provable tier
    /// ([`PortabilityTier::ClassPortable`]).
    pub portability_tier: PortabilityTier,
    /// The restore-compatibility **class identity** a host MUST present, exactly,
    /// to prove it can restore this Snapshot. A [`ContentDigest`], so a malformed
    /// value fails closed at deserialize. Because v1 has no capture-host identity,
    /// this exact-equality class identity — not an ordered tier — is the sole
    /// portability gate: a host whose class identity differs from, or is absent
    /// against, this value can never satisfy the contract.
    pub compatibility_class_identity: ContentDigest,
}

impl SnapshotCompatibilityContractV1 {
    fn validate(&self) -> Result<(), SnapshotManifestError> {
        if self.schema != SNAPSHOT_COMPATIBILITY_V1_SCHEMA {
            return Err(SnapshotManifestError::InvalidCompatibilitySchema);
        }
        for (field, value) in [
            ("compatibility.vmm_identity", self.vmm_identity.as_str()),
            ("compatibility.state_codec", self.state_codec.as_str()),
            (
                "compatibility.guest_kernel_identity",
                self.guest_kernel_identity.as_str(),
            ),
            ("compatibility.cpu_template", self.cpu_template.as_str()),
            (
                "compatibility.runner_restore_contract",
                self.runner_restore_contract.as_str(),
            ),
        ] {
            if value.trim().is_empty() {
                return Err(SnapshotManifestError::EmptyField(field));
            }
        }
        Ok(())
    }

    /// Whether a restore host that offers `capability` can *prove* it satisfies
    /// every requirement of this contract. Fail-closed: any dimension the host
    /// does not positively match rejects the pair. There is no "unknown ⇒
    /// compatible" branch — and a contract whose own compatibility `schema` is
    /// not the known [`SNAPSHOT_COMPATIBILITY_V1_SCHEMA`] satisfies nothing, so a
    /// wrong/unknown schema version can never field-match its way to
    /// compatibility even if every host dimension would otherwise equal.
    pub fn is_satisfied_by(&self, capability: &HostRestoreCapabilityV1) -> bool {
        self.schema == SNAPSHOT_COMPATIBILITY_V1_SCHEMA
            && capability.backend == self.backend
            && capability
                .supported_format_versions
                .contains(&self.format_version)
            && capability.vmm_identity == self.vmm_identity
            && capability.state_codec == self.state_codec
            && capability.guest_kernel_identity == self.guest_kernel_identity
            && capability
                .cpu_templates
                .iter()
                .any(|t| t == &self.cpu_template)
            && capability.runner_restore_contract == self.runner_restore_contract
            // Portability: EXACT class-identity equality, never an ordered tier.
            // The host must present a class identity, and it must equal the
            // Snapshot's exactly. An absent (`None`) or differing class identity
            // fails closed — there is no tier that lets a non-matching host
            // satisfy the contract.
            //
            // The tier is matched EXHAUSTIVELY with no wildcard arm: adding a
            // future `PortabilityTier` variant (e.g. a provable host-pinned tier)
            // is a compile error here, forcing an explicit matching rule for it
            // rather than silently fail-opening through this class-identity check.
            && match self.portability_tier {
                PortabilityTier::ClassPortable => {
                    capability.compatibility_class_identity
                        == Some(self.compatibility_class_identity)
                }
            }
    }
}

/// What a concrete restore host can prove it is able to restore. Supplied by the
/// caller (a runner) at selection time; the compatibility filter accepts a
/// candidate only when [`SnapshotCompatibilityContractV1::is_satisfied_by`]
/// holds. This is *not* a wire type — it is the runner's live capability probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRestoreCapabilityV1 {
    pub backend: SnapshotBackendKind,
    pub supported_format_versions: Vec<u32>,
    pub vmm_identity: String,
    pub state_codec: String,
    pub guest_kernel_identity: String,
    pub cpu_templates: Vec<String>,
    pub runner_restore_contract: String,
    /// The restore-compatibility class identity this host can prove it belongs
    /// to, if any. `None` means the host presents no class identity and so
    /// satisfies no `ClassPortable` Snapshot (fail-closed). A Snapshot is
    /// restorable here only when this equals its `compatibility_class_identity`
    /// exactly.
    pub compatibility_class_identity: Option<ContentDigest>,
    /// The [`CapturePolicyV1`] values this host can actually honor at restore
    /// (RFC §10.2 resume-vs-start semantics). Fail-closed: a candidate whose
    /// `capture_policy` is not listed here is never selected, because the host
    /// cannot prove it can rehydrate that policy. An empty list satisfies no
    /// Snapshot.
    pub supported_capture_policies: Vec<CapturePolicyV1>,
}

/// Whether the workload was running or idle at capture (RFC §8). A closed enum;
/// the runner reads it at restore to decide whether to **resume** the captured
/// workload or **start** it fresh (RFC §10.2).
///
/// The wire spellings are pinned to the exact RFC §8 names (`running` /
/// `workload_idle`) with explicit per-variant renames rather than a blanket
/// `rename_all`: the closed enum then fails closed on the pre-RFC spelling
/// `idle` and on any unknown value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapturePolicyV1 {
    /// The workload was running at capture; restore resumes it in place.
    #[serde(rename = "running")]
    Running,
    /// The workload was stopped/idle at capture (placeholders revoked); restore
    /// starts it fresh.
    #[serde(rename = "workload_idle")]
    WorkloadIdle,
}

/// The versioned Snapshot **restore contract**: the restore-time protocol and
/// ordered steps a runner performs to rehydrate the captured state (RFC §7.3).
/// `deny_unknown_fields` + a checked schema tag keep it fail-closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreContractV1 {
    /// Always [`SNAPSHOT_RESTORE_CONTRACT_V1_SCHEMA`]; validated on read.
    pub schema: String,
    /// The restore protocol identity the runner must speak to rehydrate this
    /// Snapshot (e.g. `ato-restore/v1`). Required and non-empty.
    pub restore_protocol: String,
    /// Ordered restore steps, applied before the restored session is exposed
    /// (descriptive; may be empty).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<String>,
}

impl RestoreContractV1 {
    fn validate(&self) -> Result<(), SnapshotManifestError> {
        if self.schema != SNAPSHOT_RESTORE_CONTRACT_V1_SCHEMA {
            return Err(SnapshotManifestError::InvalidRestoreContractSchema);
        }
        if self.restore_protocol.trim().is_empty() {
            return Err(SnapshotManifestError::EmptyField(
                "restore_contract.restore_protocol",
            ));
        }
        Ok(())
    }
}

/// Structural-sanitization attestation (RFC §8): which structural cleanup and
/// revocation steps ran before the artifact was sealed. Modeled on the shape of
/// the `snapshot` crate's `SanitizerContract`, but re-declared here as a lean
/// pure-contract type (the `capsule` crate cannot depend on `snapshot`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SanitizationAttestationV1 {
    /// Always [`SNAPSHOT_SANITIZATION_ATTESTATION_V1_SCHEMA`]; validated on read.
    pub schema: String,
    /// Structural cleanup / revocation steps that ran (descriptive; may be
    /// empty, e.g. `"session_id_regenerate"`, `"placeholder_revoke"`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<String>,
}

impl SanitizationAttestationV1 {
    fn validate(&self) -> Result<(), SnapshotManifestError> {
        if self.schema != SNAPSHOT_SANITIZATION_ATTESTATION_V1_SCHEMA {
            return Err(SnapshotManifestError::InvalidSanitizationAttestationSchema);
        }
        Ok(())
    }
}

/// Secret-scan attestation (RFC §8): scanner identity, policy, coverage, and a
/// redacted verdict. Modeled on the shape of the `snapshot` crate's
/// `NoSecretProof`, but re-declared here as a lean pure-contract type.
///
/// Secret scanning is defense in depth: this attestation records that a scan
/// ran and its (redacted) result — it MUST NOT be read as proof of absence
/// (RFC §8, §17.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretScanAttestationV1 {
    /// Always [`SNAPSHOT_SECRET_SCAN_ATTESTATION_V1_SCHEMA`]; validated on read.
    pub schema: String,
    /// Scanner identity that produced this attestation. Required and non-empty.
    pub scanner_identity: String,
    /// Policy identity applied by the scan. Required and non-empty.
    pub policy_identity: String,
    /// Names of the layers covered by the scan (coverage; may be empty).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scanned_layers: Vec<String>,
    /// Redacted verdict, e.g. `"clean"`. Required and non-empty. Never proof of
    /// absence.
    pub verdict: String,
}

impl SecretScanAttestationV1 {
    fn validate(&self) -> Result<(), SnapshotManifestError> {
        if self.schema != SNAPSHOT_SECRET_SCAN_ATTESTATION_V1_SCHEMA {
            return Err(SnapshotManifestError::InvalidSecretScanAttestationSchema);
        }
        if self.scanner_identity.trim().is_empty() {
            return Err(SnapshotManifestError::EmptyField(
                "secret_scan_attestation.scanner_identity",
            ));
        }
        if self.policy_identity.trim().is_empty() {
            return Err(SnapshotManifestError::EmptyField(
                "secret_scan_attestation.policy_identity",
            ));
        }
        if self.verdict.trim().is_empty() {
            return Err(SnapshotManifestError::EmptyField(
                "secret_scan_attestation.verdict",
            ));
        }
        Ok(())
    }
}

/// The captured-content commitment bundle for a v1 manifest: the layer refs that
/// bind `snapshot_id` to the actual captured bytes, plus the restore/capture and
/// attestation metadata. Passed to [`LegacyReadyStateManifestV1::migrate`], which
/// cannot recover any of it from a legacy artifact (legacy manifests carry no
/// content commitment).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotContentCommitmentV1 {
    /// Content addresses of the captured guest-memory layers.
    pub memory_layer_refs: Vec<ContentDigest>,
    /// Content addresses of the captured VM-state layers.
    pub vmstate_layer_refs: Vec<ContentDigest>,
    /// Content addresses of the captured disk layers.
    pub disk_layer_refs: Vec<ContentDigest>,
    /// Restore-time contract.
    pub restore_contract: RestoreContractV1,
    /// Whether the workload was running or idle at capture.
    pub capture_policy: CapturePolicyV1,
    /// Structural-sanitization attestation.
    pub sanitization_attestation: SanitizationAttestationV1,
    /// Secret-scan attestation.
    pub secret_scan_attestation: SecretScanAttestationV1,
}

/// Optional capture-time provenance. Provenance is descriptive only — it MUST
/// NEVER substitute for identity or compatibility in selection. In particular
/// `capsule_manifest_hash` lives here (and only here) after being removed from
/// compatibility selection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotCaptureProvenance {
    /// The originating capsule manifest hash, retained only as provenance. Never
    /// an identity or compatibility key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capsule_manifest_hash: Option<String>,
    /// Opaque id of the build receipt that produced the artifact, if known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_receipt_id: Option<String>,
}

impl SnapshotCaptureProvenance {
    fn is_empty(&self) -> bool {
        self.capsule_manifest_hash.is_none() && self.build_receipt_id.is_none()
    }
}

/// The `ato.snapshot-manifest/v1` wire contract.
///
/// Identity note: `execution_id` is over the **execution contract**, not the
/// Snapshot bytes/format. Two manifests that differ only in
/// `compatibility_contract` (a Snapshot *format* difference) therefore share an
/// `execution_id` while deriving different `snapshot_id`s — a Snapshot-format
/// change never changes Execution Identity.
///
/// There is deliberately **no `snapshot_id` field**: the address is derived from
/// the canonical payload and so can never be part of its own preimage. An API or
/// registry that wants to surface it uses an outer envelope
/// ([`SnapshotCatalogRecord`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotManifestV1 {
    /// Always [`SNAPSHOT_MANIFEST_V1_SCHEMA`]; validated on read. Because this is
    /// the explicit v1 schema discriminator the legacy schema lacks, a legacy
    /// artifact can never deserialize as a v1 manifest by omission.
    pub schema: String,
    /// REQUIRED. The `ato.execution-contract/v1` id this Snapshot is subordinate
    /// to. Not `Option` — a manifest missing it, or carrying a malformed one,
    /// fails closed at deserialize (`ExecutionId` enforces `blake3:<hex>`).
    pub execution_id: ExecutionId,
    /// Typed, versioned restore requirements. Unknown compatibility is rejected.
    pub compatibility_contract: SnapshotCompatibilityContractV1,
    /// Content addresses of the captured guest-memory layers. REQUIRED and
    /// non-empty: with `execution_id` and `compatibility_contract` alone, two
    /// captures of different bytes would derive the SAME `snapshot_id`. Committing
    /// the layer refs makes `snapshot_id` a true content address of the captured
    /// bytes.
    pub memory_layer_refs: Vec<ContentDigest>,
    /// Content addresses of the captured VM-state layers. REQUIRED and non-empty.
    pub vmstate_layer_refs: Vec<ContentDigest>,
    /// Content addresses of the captured disk layers. REQUIRED and non-empty.
    pub disk_layer_refs: Vec<ContentDigest>,
    /// Restore-time contract (protocol + ordered steps to rehydrate).
    pub restore_contract: RestoreContractV1,
    /// Whether the workload was running or idle at capture (RFC §8/§10.2).
    pub capture_policy: CapturePolicyV1,
    /// Optional capture provenance (descriptive only; never selection input).
    #[serde(default, skip_serializing_if = "SnapshotCaptureProvenance::is_empty")]
    pub capture_provenance: SnapshotCaptureProvenance,
    /// Structural-sanitization attestation (which cleanup/revocation ran).
    pub sanitization_attestation: SanitizationAttestationV1,
    /// Secret-scan attestation (scanner/policy/coverage/verdict; never proof of
    /// absence).
    pub secret_scan_attestation: SecretScanAttestationV1,
}

impl SnapshotManifestV1 {
    /// Deserialize + fully validate a v1 manifest from JSON, fail-closed. This is
    /// the sanctioned entry: it rejects a wrong `schema`, an unknown compatibility
    /// field, a malformed `execution_id`, or a bad compatibility schema. Prefer
    /// this over a bare `serde_json::from_str` so the schema discriminator is
    /// always enforced (the legacy read path never checked `schema`).
    pub fn parse(json: &str) -> Result<Self, SnapshotManifestError> {
        let manifest: Self = serde_json::from_str(json)
            .map_err(|error| SnapshotManifestError::Canonicalization(error.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validate the manifest's own invariants (schema discriminator + nested
    /// compatibility schema). Serde already enforces the required `execution_id`
    /// and rejects unknown compatibility fields at deserialize time; this covers
    /// the string-valued schema discriminators serde cannot.
    pub fn validate(&self) -> Result<(), SnapshotManifestError> {
        if self.schema != SNAPSHOT_MANIFEST_V1_SCHEMA {
            return Err(SnapshotManifestError::InvalidSchema);
        }
        self.compatibility_contract.validate()?;
        // Content commitment: a `snapshot_id` that does not commit to the actual
        // captured bytes is not a content address. Each captured layer class MUST
        // contribute at least one ref, so two captures of different memory /
        // vmstate / disk bytes can never collide on the same id. Missing/empty
        // commitment fails closed (and therefore yields no `snapshot_id`).
        for (field, refs) in [
            ("memory_layer_refs", &self.memory_layer_refs),
            ("vmstate_layer_refs", &self.vmstate_layer_refs),
            ("disk_layer_refs", &self.disk_layer_refs),
        ] {
            if refs.is_empty() {
                return Err(SnapshotManifestError::EmptyField(field));
            }
        }
        self.restore_contract.validate()?;
        // Restore-protocol identity: `restore_contract.restore_protocol` (the
        // protocol the manifest's restore steps are written for) and
        // `compatibility_contract.runner_restore_contract` (the protocol a host
        // must prove it speaks) are the SAME concept. If they disagree, a host
        // that field-matches `runner_restore_contract` would restore under a
        // protocol the manifest never declared. Enforcing equality here makes the
        // host's existing `runner_restore_contract` match transitively prove
        // `restore_protocol` — no separate host field is needed for the protocol.
        if self.restore_contract.restore_protocol
            != self.compatibility_contract.runner_restore_contract
        {
            return Err(SnapshotManifestError::RestoreContractMismatch);
        }
        self.sanitization_attestation.validate()?;
        self.secret_scan_attestation.validate()
    }

    /// The canonical JCS bytes of this manifest — the `snapshot_id` preimage
    /// payload. Because `snapshot_id` is not a field, these bytes never contain
    /// the address they derive.
    fn canonical_bytes(&self) -> Result<Vec<u8>, SnapshotManifestError> {
        self.validate()?;
        serde_jcs::to_vec(self)
            .map_err(|error| SnapshotManifestError::Canonicalization(error.to_string()))
    }

    /// Derive the Snapshot content address under the frozen G0-1/A1 rule:
    ///
    /// ```text
    /// snapshot_id = "blake3:" + hex(
    ///   BLAKE3( UTF8("ato.snapshot-manifest/v1") || 0x00 || JCS(manifest) )
    /// )
    /// ```
    ///
    /// The manifest has no `snapshot_id` field, so it is excluded from its own
    /// preimage by construction. Reuses
    /// [`schema_domained_blake3_id`](super::execution_contract::schema_domained_blake3_id)
    /// — the same helper `execution_id` uses; the rule is not reinvented here.
    pub fn snapshot_id(&self) -> Result<SnapshotId, SnapshotManifestError> {
        let canonical = self.canonical_bytes()?;
        SnapshotId::new(schema_domained_blake3_id(
            SNAPSHOT_MANIFEST_V1_SCHEMA,
            &canonical,
        ))
    }
}

/// Registry acceptance disposition for a captured Snapshot. Acceptance is
/// **registry metadata**, not a property of the immutable artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AcceptanceStatus {
    Accepted,
    Rejected,
    Quarantined,
}

/// A catalog record pinning one immutable `snapshot_id` to its acceptance
/// status. Quarantine mutates *this metadata* only — never the manifest bytes or
/// the id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotCatalogRecord {
    /// The pinned, immutable content address. Never changes for the life of the
    /// record.
    pub snapshot_id: SnapshotId,
    /// Current acceptance disposition.
    pub status: AcceptanceStatus,
}

impl SnapshotCatalogRecord {
    pub fn new(snapshot_id: SnapshotId, status: AcceptanceStatus) -> Self {
        Self {
            snapshot_id,
            status,
        }
    }

    /// Quarantine an accepted-but-now-invalid Snapshot. Flips only the status
    /// field; the `snapshot_id` (and, by construction, the artifact bytes it
    /// addresses) are untouched. A quarantined Snapshot is never selected and is
    /// never silently repaired under the same id.
    pub fn quarantine(&mut self) {
        self.status = AcceptanceStatus::Quarantined;
    }

    pub fn is_accepted(&self) -> bool {
        matches!(self.status, AcceptanceStatus::Accepted)
    }
}

/// Opaque ranking signals for a candidate. These influence order **only after**
/// the identity + compatibility filters have run. Nothing here is ever an
/// eligibility input — a candidate with an ideal ranking but the wrong
/// `execution_id` is never selected.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SnapshotRankingSignals {
    /// Estimated restore cost (lower is preferred). E.g. cold bytes to fault in.
    pub restore_cost: u64,
    /// Whether the candidate's hotset is already resident (preferred).
    pub hotset_resident: bool,
    /// Capture recency as an epoch-seconds tie-breaker (newer preferred). Recency
    /// is a *tie-breaker only* — it can never substitute for exact identity.
    pub created_at_epoch: u64,
}

/// A selection candidate: an immutable v1 manifest plus its registry acceptance
/// status and ranking signals. The manifest is the *only* identity/compatibility
/// input; status gates eligibility; ranking orders survivors.
///
/// Invariant: `manifest` MUST be a [`SnapshotManifestV1::parse`]-validated (or
/// [`LegacyReadyStateManifestV1::migrate`]-produced) manifest. This type exposes
/// no validated constructor, so a direct struct literal *can* technically wrap an
/// unvalidated manifest; [`select_snapshots`] therefore re-checks
/// [`SnapshotManifestV1::validate`] defensively and never selects a malformed
/// candidate regardless of how it was constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotCandidate {
    pub manifest: SnapshotManifestV1,
    pub status: AcceptanceStatus,
    pub ranking: SnapshotRankingSignals,
}

/// Select the restore candidates for a **verified** Execution Identity on a
/// given restore host, in the hard order mandated by RFC §7.5:
///
/// 1. **exact `execution_id` equality** — the first and non-negotiable gate.
///    `requested` is a [`VerifiedExecutionId`] — a proof-carrying wrapper that can
///    only be obtained from a [`verify`](super::execution_contract::ExecutionContractEnvelopeV1::verify)'d
///    execution-contract envelope or a completed strict finalization — never a
///    raw [`ExecutionId`], lock, `lock_id`, capsule name, or target label. The
///    exact match compares `requested.as_execution_id()` against the candidate's
///    manifest id; a candidate whose manifest carries a different id is dropped
///    here.
/// 2. **acceptance** — only `Accepted` candidates survive (quarantined/rejected
///    Snapshots are never restored).
/// 3. **proven compatibility** — the host must prove it satisfies the candidate's
///    compatibility contract ([`SnapshotCompatibilityContractV1::is_satisfied_by`])
///    **and** positively support the candidate's [`CapturePolicyV1`] (its
///    `supported_capture_policies` must contain it). Unknown compatibility and an
///    unsupported capture policy both fail closed.
/// 4. **ranking** — surviving candidates are ordered by
///    [`SnapshotRankingSignals`] (lower restore cost, then hotset-resident, then
///    newer). Ranking runs **only** on the post-identity, post-compatibility set;
///    it can never promote a candidate past the identity/compatibility gates.
///
/// Independently of the four gates, any candidate whose manifest is not
/// well-formed ([`SnapshotManifestV1::validate`] fails) is dropped defensively
/// before the compatibility gate: a malformed candidate must never be selectable,
/// even if it was constructed by bypassing [`SnapshotManifestV1::parse`].
pub fn select_snapshots<'a>(
    requested: &VerifiedExecutionId,
    host: &HostRestoreCapabilityV1,
    candidates: &'a [SnapshotCandidate],
) -> Vec<&'a SnapshotCandidate> {
    let mut eligible: Vec<&SnapshotCandidate> = candidates
        .iter()
        // Gate 1: exact execution_id equality — FIRST, before anything else. The
        // requested id is a proof-carrying VerifiedExecutionId; the filter reads
        // the proven id via `as_execution_id()`.
        .filter(|candidate| &candidate.manifest.execution_id == requested.as_execution_id())
        // Gate 2: acceptance metadata (quarantined/rejected never restore).
        .filter(|candidate| matches!(candidate.status, AcceptanceStatus::Accepted))
        // Defensive well-formedness gate: a malformed candidate manifest must
        // never be selectable, even if constructed by bypassing `parse`. This
        // also fails closed on a wrong/unknown compatibility schema.
        .filter(|candidate| candidate.manifest.validate().is_ok())
        // Gate 3: proven compatibility (fail-closed).
        .filter(|candidate| {
            candidate
                .manifest
                .compatibility_contract
                .is_satisfied_by(host)
        })
        // Gate 3 (capture policy): the host must positively support the
        // candidate's capture policy, or it cannot honor the resume-vs-start
        // restore semantics that policy dictates (RFC §10.2). Fail-closed — a
        // policy absent from `supported_capture_policies` is never selected.
        .filter(|candidate| {
            host.supported_capture_policies
                .contains(&candidate.manifest.capture_policy)
        })
        .collect();

    // Gate 4: ranking — ONLY over the already identity+compat-filtered set.
    eligible.sort_by_key(|candidate| rank_key(candidate));
    eligible
}

/// Total-order ranking key (all-ascending; `cmp` picks the best first).
///
/// * `restore_cost` ascending — cheaper restore first;
/// * `!hotset_resident` ascending — `false` (resident) sorts before `true`;
/// * `Reverse(created_at_epoch)` — newer first, as a final tie-breaker only.
fn rank_key(candidate: &SnapshotCandidate) -> (u64, bool, std::cmp::Reverse<u64>) {
    (
        candidate.ranking.restore_cost,
        !candidate.ranking.hotset_resident,
        std::cmp::Reverse(candidate.ranking.created_at_epoch),
    )
}

/// An inspection-only view of a legacy `ato.ready-state/v1` artifact.
///
/// This exists so legacy artifacts remain **deserializable for inspection and
/// explicit migration** — it is intentionally tolerant (`execution_id` optional
/// and opaque, unknown fields ignored), mirroring the real legacy manifest. It is
/// **not** a [`SnapshotCandidate`] and cannot be passed to [`select_snapshots`],
/// which structurally enforces "legacy artifacts are never v1-selectable".
///
/// A legacy `execution_id`, when present, is an *opaque, unverified* string that
/// was never bound to a verified execution contract; migration therefore never
/// trusts it as identity (see [`Self::migrate`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyReadyStateManifestV1 {
    /// Expected to be [`READY_STATE_V1_SCHEMA`]; checked by [`Self::inspect`].
    pub schema: String,
    /// The legacy originating capsule manifest hash (opaque provenance).
    #[serde(default)]
    pub capsule_manifest_hash: Option<String>,
    /// The legacy optional, opaque `execution_id` string. Present or not, it is
    /// never eligible for v1 exact lookup and is never trusted as identity.
    #[serde(default)]
    pub execution_id: Option<String>,
}

impl LegacyReadyStateManifestV1 {
    /// Deserialize a legacy manifest for inspection, checking only its schema
    /// tag. Tolerant of unknown fields by design (inspection must never fail on a
    /// forward-compatible legacy artifact).
    pub fn inspect(json: &str) -> Result<Self, SnapshotManifestError> {
        let manifest: Self = serde_json::from_str(json)
            .map_err(|error| SnapshotManifestError::Canonicalization(error.to_string()))?;
        if manifest.schema != READY_STATE_V1_SCHEMA {
            return Err(SnapshotManifestError::InvalidLegacySchema);
        }
        Ok(manifest)
    }

    /// **Explicit** migration to a v1 manifest. This is the only bridge from
    /// legacy to v1, and it is deliberately not automatic:
    ///
    /// * The caller supplies a [`VerifiedExecutionId`] — a proof-carrying wrapper
    ///   obtained from a verified execution-contract envelope or a completed
    ///   strict finalization — because the legacy opaque `execution_id` string is
    ///   untrusted and may be absent. The type makes this non-negotiable: a legacy
    ///   artifact without a trustworthy identity cannot be migrated without the
    ///   caller independently proving the identity.
    /// * The caller supplies a full v1 [`SnapshotCompatibilityContractV1`],
    ///   because the legacy `runner_class_id` is not a v1 compatibility contract.
    /// * The caller supplies the [`SnapshotContentCommitmentV1`] (layer refs +
    ///   restore/capture + attestations): a legacy artifact carries no content
    ///   commitment, and a v1 manifest without one is not a content address and
    ///   fails [`SnapshotManifestV1::validate`].
    /// * The result is a **new immutable manifest** and therefore a **new
    ///   `snapshot_id`** — the legacy bytes are never reinterpreted in place. The
    ///   legacy `capsule_manifest_hash` is carried across as provenance only.
    ///
    /// The legacy `schema` is re-checked first: the tolerant public fields let a
    /// caller hand in a struct whose `schema` is not [`READY_STATE_V1_SCHEMA`]
    /// (e.g. `ato.snapshot-manifest/v1`), and migrating such a struct would be
    /// treating a non-legacy artifact as legacy — so it is rejected with
    /// [`SnapshotManifestError::InvalidLegacySchema`].
    pub fn migrate(
        &self,
        verified_execution_id: &VerifiedExecutionId,
        compatibility_contract: SnapshotCompatibilityContractV1,
        content: SnapshotContentCommitmentV1,
    ) -> Result<SnapshotManifestV1, SnapshotManifestError> {
        if self.schema != READY_STATE_V1_SCHEMA {
            return Err(SnapshotManifestError::InvalidLegacySchema);
        }
        let migrated = SnapshotManifestV1 {
            schema: SNAPSHOT_MANIFEST_V1_SCHEMA.to_string(),
            execution_id: verified_execution_id.as_execution_id().clone(),
            compatibility_contract,
            memory_layer_refs: content.memory_layer_refs,
            vmstate_layer_refs: content.vmstate_layer_refs,
            disk_layer_refs: content.disk_layer_refs,
            restore_contract: content.restore_contract,
            capture_policy: content.capture_policy,
            capture_provenance: SnapshotCaptureProvenance {
                capsule_manifest_hash: self.capsule_manifest_hash.clone(),
                build_receipt_id: None,
            },
            sanitization_attestation: content.sanitization_attestation,
            secret_scan_attestation: content.secret_scan_attestation,
        };
        migrated.validate()?;
        Ok(migrated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::contract::execution_contract::test_execution_contract;

    // Two distinct identity anchors, each the REAL canonical hash of a seeded
    // execution contract (seeds 1 and 2). The selection/migration tests mint a
    // `VerifiedExecutionId` through the proof-preserving `verify_contract_id`
    // seam — no synthetic-id wrapping — and put the same real id into a
    // candidate manifest's `execution_id`.
    const SEED_A: u8 = 1;
    const SEED_B: u8 = 2;

    /// The real canonical `execution_id` of the seeded execution contract.
    fn exec_id(seed: u8) -> ExecutionId {
        test_execution_contract(seed)
            .compute_execution_id()
            .expect("valid execution id")
    }

    /// Mint a [`VerifiedExecutionId`] via the proof-preserving seam: compute the
    /// id from the real seeded contract and route it through `verify_contract_id`
    /// (which recomputes and compares). No fake id is ever wrapped.
    fn verified(seed: u8) -> VerifiedExecutionId {
        let contract = test_execution_contract(seed);
        let id = contract.compute_execution_id().expect("valid execution id");
        VerifiedExecutionId::verify_contract_id(&contract, &id).expect("proof-preserving mint")
    }

    /// A valid [`ContentDigest`] seeded by a single lowercase-hex fill character.
    fn digest(fill: char) -> ContentDigest {
        assert!(fill.is_ascii_hexdigit() && !fill.is_ascii_uppercase());
        ContentDigest::try_from(format!("blake3:{}", fill.to_string().repeat(64)))
            .expect("valid content digest")
    }

    // The Snapshot's restore-compatibility class identity, shared by
    // `sample_compat` and `matching_host` so a matching host satisfies it.
    fn class_identity() -> ContentDigest {
        digest('c')
    }

    fn sample_compat() -> SnapshotCompatibilityContractV1 {
        SnapshotCompatibilityContractV1 {
            schema: SNAPSHOT_COMPATIBILITY_V1_SCHEMA.to_string(),
            backend: SnapshotBackendKind::Firecracker,
            format_version: 2,
            vmm_identity: "firecracker-1.7".to_string(),
            state_codec: "fc-state/v2".to_string(),
            guest_kernel_identity: "vmlinux-6.1-ato".to_string(),
            cpu_template: "T2CL".to_string(),
            runner_restore_contract: "ato-restore/v1".to_string(),
            portability_tier: PortabilityTier::ClassPortable,
            compatibility_class_identity: class_identity(),
        }
    }

    fn sample_content() -> SnapshotContentCommitmentV1 {
        SnapshotContentCommitmentV1 {
            memory_layer_refs: vec![digest('1')],
            vmstate_layer_refs: vec![digest('2')],
            disk_layer_refs: vec![digest('3')],
            restore_contract: RestoreContractV1 {
                schema: SNAPSHOT_RESTORE_CONTRACT_V1_SCHEMA.to_string(),
                restore_protocol: "ato-restore/v1".to_string(),
                steps: vec!["network_reconnect".to_string()],
            },
            capture_policy: CapturePolicyV1::WorkloadIdle,
            sanitization_attestation: SanitizationAttestationV1 {
                schema: SNAPSHOT_SANITIZATION_ATTESTATION_V1_SCHEMA.to_string(),
                steps: vec!["session_id_regenerate".to_string()],
            },
            secret_scan_attestation: SecretScanAttestationV1 {
                schema: SNAPSHOT_SECRET_SCAN_ATTESTATION_V1_SCHEMA.to_string(),
                scanner_identity: "ato-secret-scan/1.0".to_string(),
                policy_identity: "default/v1".to_string(),
                scanned_layers: vec!["memory".to_string(), "vmstate".to_string()],
                verdict: "clean".to_string(),
            },
        }
    }

    fn sample_manifest(seed: u8) -> SnapshotManifestV1 {
        let content = sample_content();
        SnapshotManifestV1 {
            schema: SNAPSHOT_MANIFEST_V1_SCHEMA.to_string(),
            execution_id: exec_id(seed),
            compatibility_contract: sample_compat(),
            memory_layer_refs: content.memory_layer_refs,
            vmstate_layer_refs: content.vmstate_layer_refs,
            disk_layer_refs: content.disk_layer_refs,
            restore_contract: content.restore_contract,
            capture_policy: content.capture_policy,
            capture_provenance: SnapshotCaptureProvenance::default(),
            sanitization_attestation: content.sanitization_attestation,
            secret_scan_attestation: content.secret_scan_attestation,
        }
    }

    fn matching_host() -> HostRestoreCapabilityV1 {
        HostRestoreCapabilityV1 {
            backend: SnapshotBackendKind::Firecracker,
            supported_format_versions: vec![1, 2, 3],
            vmm_identity: "firecracker-1.7".to_string(),
            state_codec: "fc-state/v2".to_string(),
            guest_kernel_identity: "vmlinux-6.1-ato".to_string(),
            cpu_templates: vec!["T2CL".to_string(), "T2A".to_string()],
            runner_restore_contract: "ato-restore/v1".to_string(),
            compatibility_class_identity: Some(class_identity()),
            supported_capture_policies: vec![
                CapturePolicyV1::Running,
                CapturePolicyV1::WorkloadIdle,
            ],
        }
    }

    fn candidate(
        seed: u8,
        status: AcceptanceStatus,
        ranking: SnapshotRankingSignals,
    ) -> SnapshotCandidate {
        SnapshotCandidate {
            manifest: sample_manifest(seed),
            status,
            ranking,
        }
    }

    // --- Acceptance: missing / malformed execution_id rejected at deserialize ---

    #[test]
    fn snapshot_manifest_missing_execution_id_is_rejected() {
        // Drop `execution_id` from an otherwise-valid manifest: required
        // non-Option field ⇒ serde error.
        let mut value = serde_json::to_value(sample_manifest(SEED_A)).unwrap();
        value.as_object_mut().unwrap().remove("execution_id");
        assert!(SnapshotManifestV1::parse(&value.to_string()).is_err());
    }

    #[test]
    fn snapshot_manifest_malformed_execution_id_is_rejected() {
        // Present but not a blake3:<64 hex> id ⇒ ExecutionId::try_from fails closed.
        let mut value = serde_json::to_value(sample_manifest(SEED_A)).unwrap();
        value["execution_id"] = serde_json::json!("not-a-real-id");
        assert!(SnapshotManifestV1::parse(&value.to_string()).is_err());
    }

    #[test]
    fn snapshot_manifest_wrong_schema_is_rejected() {
        let mut manifest = sample_manifest(SEED_A);
        manifest.schema = "ato.ready-state/v1".to_string();
        let json = serde_json::to_string(&manifest).unwrap();
        assert!(matches!(
            SnapshotManifestV1::parse(&json),
            Err(SnapshotManifestError::InvalidSchema)
        ));
    }

    // --- Acceptance: unknown compatibility rejected (fail-closed) ---

    #[test]
    fn unknown_compatibility_field_is_rejected() {
        // A compatibility dimension this version does not model must fail closed,
        // not be silently ignored. Injected into an otherwise-valid manifest so
        // the ONLY defect is the unknown compatibility field.
        let mut value = serde_json::to_value(sample_manifest(SEED_A)).unwrap();
        value["compatibility_contract"]
            .as_object_mut()
            .unwrap()
            .insert("gpu_model".to_string(), serde_json::json!("a100"));
        assert!(SnapshotManifestV1::parse(&value.to_string()).is_err());
    }

    #[test]
    fn unknown_compatibility_backend_is_rejected() {
        let mut value = serde_json::to_value(sample_manifest(SEED_A)).unwrap();
        value["compatibility_contract"]
            .as_object_mut()
            .unwrap()
            .insert(
                "backend".to_string(),
                serde_json::json!("some-unknown-backend"),
            );
        assert!(SnapshotManifestV1::parse(&value.to_string()).is_err());
    }

    #[test]
    fn wrong_compatibility_schema_is_rejected() {
        let mut manifest = sample_manifest(SEED_A);
        manifest.compatibility_contract.schema = "ato.snapshot-compatibility/v2".to_string();
        assert!(matches!(
            manifest.validate(),
            Err(SnapshotManifestError::InvalidCompatibilitySchema)
        ));
    }

    // --- Acceptance: exact lookup cannot be substituted ---

    #[test]
    fn wrong_execution_id_is_never_selected_even_when_best_ranked() {
        let host = matching_host();
        // A candidate with a DIFFERENT execution_id but the most attractive
        // ranking (cheapest, hotset-resident, newest) and Accepted status.
        let tempting = candidate(
            SEED_B,
            AcceptanceStatus::Accepted,
            SnapshotRankingSignals {
                restore_cost: 0,
                hotset_resident: true,
                created_at_epoch: u64::MAX,
            },
        );
        // The only correct-identity candidate is worse-ranked.
        let correct = candidate(
            SEED_A,
            AcceptanceStatus::Accepted,
            SnapshotRankingSignals {
                restore_cost: 9_999,
                hotset_resident: false,
                created_at_epoch: 1,
            },
        );
        let pool = [tempting, correct.clone()];
        let selected = select_snapshots(&verified(SEED_A), &host, &pool);
        assert_eq!(selected, vec![&correct]);
    }

    #[test]
    fn name_target_commit_recency_runner_cannot_substitute_for_exact_lookup() {
        // None of these signals live in the selection inputs at all: the only
        // identity input is `manifest.execution_id`. A candidate for a different
        // Execution Identity is dropped regardless of provenance overlap.
        let host = matching_host();
        let mut other = sample_manifest(SEED_B);
        // Same capsule provenance (name/commit proxy) as the requested identity —
        // must NOT rescue it.
        other.capture_provenance.capsule_manifest_hash = Some("blake3:deadbeef".to_string());
        let cand = SnapshotCandidate {
            manifest: other,
            status: AcceptanceStatus::Accepted,
            ranking: SnapshotRankingSignals::default(),
        };
        let pool = [cand];
        let selected = select_snapshots(&verified(SEED_A), &host, &pool);
        assert!(selected.is_empty());
    }

    // --- Acceptance: unknown compatibility rejected at SELECTION (host cannot prove) ---

    #[test]
    fn host_that_cannot_prove_compatibility_selects_nothing() {
        let mut host = matching_host();
        host.supported_format_versions = vec![1, 3]; // not 2
        let cand = candidate(SEED_A, AcceptanceStatus::Accepted, Default::default());
        assert!(select_snapshots(&verified(SEED_A), &host, &[cand]).is_empty());

        let mut host = matching_host();
        host.cpu_templates = vec!["T2A".to_string()]; // no T2CL
        let cand = candidate(SEED_A, AcceptanceStatus::Accepted, Default::default());
        assert!(select_snapshots(&verified(SEED_A), &host, &[cand]).is_empty());

        let mut host = matching_host();
        host.compatibility_class_identity = Some(digest('d')); // different class
        let cand = candidate(SEED_A, AcceptanceStatus::Accepted, Default::default());
        assert!(select_snapshots(&verified(SEED_A), &host, &[cand]).is_empty());

        let mut host = matching_host();
        host.compatibility_class_identity = None; // absent class fails closed
        let cand = candidate(SEED_A, AcceptanceStatus::Accepted, Default::default());
        assert!(select_snapshots(&verified(SEED_A), &host, &[cand]).is_empty());
    }

    // --- Acceptance: a malformed candidate is never selectable (defensive gate) ---

    #[test]
    fn candidate_with_wrong_compat_schema_is_never_selected() {
        // Model a candidate constructed by bypassing `parse` (direct struct
        // literal) whose compatibility `schema` is an unknown version. Its
        // execution_id matches the request and every *host dimension* would
        // field-equal (schema is not a host field), so without the hardening it
        // could slip through. Both the validate() gate and the is_satisfied_by
        // schema check must independently reject it.
        let host = matching_host();
        let mut malformed = sample_manifest(SEED_A);
        malformed.compatibility_contract.schema = "ato.snapshot-compatibility/v2".to_string();
        // Sanity: the manifest is indeed malformed and every host dimension
        // field-equals (so only the schema hardening keeps it out).
        assert!(malformed.validate().is_err());
        let cand = SnapshotCandidate {
            manifest: malformed,
            status: AcceptanceStatus::Accepted,
            ranking: SnapshotRankingSignals::default(),
        };
        assert!(select_snapshots(&verified(SEED_A), &host, std::slice::from_ref(&cand)).is_empty());
        // The compatibility check alone also fails closed on the unknown schema.
        assert!(!cand.manifest.compatibility_contract.is_satisfied_by(&host));
    }

    #[test]
    fn candidate_with_empty_compat_identity_field_is_never_selected() {
        // A malformed manifest with an empty identity field, matching id and
        // Accepted status, is dropped by the defensive well-formedness gate.
        let host = matching_host();
        let mut malformed = sample_manifest(SEED_A);
        malformed.compatibility_contract.vmm_identity = String::new();
        assert!(malformed.validate().is_err());
        let cand = SnapshotCandidate {
            manifest: malformed,
            status: AcceptanceStatus::Accepted,
            ranking: SnapshotRankingSignals::default(),
        };
        assert!(select_snapshots(&verified(SEED_A), &host, std::slice::from_ref(&cand)).is_empty());
    }

    // --- Acceptance: SnapshotId::new distinguishes empty from malformed ---

    #[test]
    fn snapshot_id_new_reports_empty_and_malformed_distinctly() {
        // Genuinely empty ⇒ EmptyField ("must be non-empty").
        assert!(matches!(
            SnapshotId::new(String::new()),
            Err(SnapshotManifestError::EmptyField("snapshot_id"))
        ));
        // Non-empty but malformed (missing prefix / wrong length / non-hex /
        // uppercase) ⇒ InvalidSnapshotId, never the misleading EmptyField.
        let no_prefix = "1".repeat(64);
        let too_short = "blake3:abc123".to_string();
        let uppercase = format!("blake3:{}", "A".repeat(64));
        let non_hex = format!("blake3:{}", "g".repeat(64));
        for malformed in [no_prefix, too_short, uppercase, non_hex] {
            assert!(
                matches!(
                    SnapshotId::new(malformed.clone()),
                    Err(SnapshotManifestError::InvalidSnapshotId)
                ),
                "expected InvalidSnapshotId for {malformed:?}"
            );
        }
        // A well-formed address still constructs.
        assert!(SnapshotId::new(format!("blake3:{}", "a".repeat(64))).is_ok());
    }

    // --- Acceptance: ranking happens ONLY after identity + compat filtering ---

    #[test]
    fn ranking_orders_only_the_identity_and_compat_filtered_set() {
        let host = matching_host();
        let cheap = candidate(
            SEED_A,
            AcceptanceStatus::Accepted,
            SnapshotRankingSignals {
                restore_cost: 10,
                hotset_resident: true,
                created_at_epoch: 5,
            },
        );
        let expensive = candidate(
            SEED_A,
            AcceptanceStatus::Accepted,
            SnapshotRankingSignals {
                restore_cost: 100,
                hotset_resident: false,
                created_at_epoch: 9,
            },
        );
        // Wrong identity — must be filtered out BEFORE ranking, even though its
        // restore_cost (0) would rank it first.
        let wrong = candidate(
            SEED_B,
            AcceptanceStatus::Accepted,
            SnapshotRankingSignals {
                restore_cost: 0,
                hotset_resident: true,
                created_at_epoch: 100,
            },
        );
        let pool = [expensive.clone(), wrong, cheap.clone()];
        let selected = select_snapshots(&verified(SEED_A), &host, &pool);
        assert_eq!(selected, vec![&cheap, &expensive]);
    }

    #[test]
    fn quarantined_and_rejected_candidates_are_never_selected() {
        let host = matching_host();
        let quarantined = candidate(SEED_A, AcceptanceStatus::Quarantined, Default::default());
        let rejected = candidate(SEED_A, AcceptanceStatus::Rejected, Default::default());
        assert!(select_snapshots(&verified(SEED_A), &host, &[quarantined, rejected]).is_empty());
    }

    // --- Acceptance: quarantine mutates neither bytes nor id ---

    #[test]
    fn quarantine_does_not_mutate_snapshot_id() {
        let manifest = sample_manifest(SEED_A);
        let id = manifest.snapshot_id().expect("id");
        let mut record = SnapshotCatalogRecord::new(id.clone(), AcceptanceStatus::Accepted);
        assert!(record.is_accepted());
        record.quarantine();
        assert_eq!(record.status, AcceptanceStatus::Quarantined);
        // The pinned id is unchanged; the manifest bytes it addresses are
        // untouched (re-deriving from the same manifest yields the same id).
        assert_eq!(record.snapshot_id, id);
        assert_eq!(manifest.snapshot_id().expect("id"), id);
    }

    // --- Acceptance: snapshot_id derivation (JCS + domain BLAKE3, no self-ref) ---

    #[test]
    fn snapshot_id_uses_domain_separated_jcs_blake3_without_self_reference() {
        let manifest = sample_manifest(SEED_A);
        // Recompute the preimage by hand from the manifest's own JCS bytes (which
        // contain no `snapshot_id` field) and the schema domain.
        let canonical = serde_jcs::to_vec(&manifest).unwrap();
        let expected = schema_domained_blake3_id(SNAPSHOT_MANIFEST_V1_SCHEMA, &canonical);
        assert_eq!(manifest.snapshot_id().unwrap().as_str(), expected);
        // Deterministic + stable across recomputation.
        assert_eq!(
            manifest.snapshot_id().unwrap(),
            manifest.snapshot_id().unwrap()
        );
        // The serialized manifest genuinely has no snapshot_id field to feed back
        // into the preimage.
        let value: serde_json::Value = serde_json::from_slice(&canonical).unwrap();
        assert!(value.get("snapshot_id").is_none());
    }

    // --- Acceptance: snapshot-format change does NOT change execution_id ---

    #[test]
    fn snapshot_format_change_changes_snapshot_id_not_execution_id() {
        let base = sample_manifest(SEED_A);
        let mut reformatted = base.clone();
        // A pure Snapshot *format* change: different backend format version.
        reformatted.compatibility_contract.format_version = 99;

        // execution_id (Execution Identity) is unchanged...
        assert_eq!(base.execution_id, reformatted.execution_id);
        // ...but the content-addressed snapshot_id differs.
        assert_ne!(
            base.snapshot_id().unwrap(),
            reformatted.snapshot_id().unwrap()
        );
    }

    // --- Acceptance: legacy inspectable but not v1-selectable without migration ---

    #[test]
    fn legacy_manifest_is_inspectable() {
        // A legacy artifact WITHOUT execution_id still round-trips for inspection,
        // and tolerates unknown/forward fields.
        let json = serde_json::json!({
            "schema": READY_STATE_V1_SCHEMA,
            "capsule_manifest_hash": "blake3:abc123",
            "runner_class_id": "some-legacy-class",
            "has_vsock": true,
        })
        .to_string();
        let legacy = LegacyReadyStateManifestV1::inspect(&json).expect("inspectable");
        assert_eq!(legacy.execution_id, None);
        assert_eq!(
            legacy.capsule_manifest_hash.as_deref(),
            Some("blake3:abc123")
        );
        // There is no API path that turns a LegacyReadyStateManifestV1 into a
        // SnapshotCandidate — it is structurally not v1-selectable.
    }

    #[test]
    fn legacy_inspect_rejects_non_legacy_schema() {
        let json = serde_json::json!({ "schema": SNAPSHOT_MANIFEST_V1_SCHEMA }).to_string();
        assert!(matches!(
            LegacyReadyStateManifestV1::inspect(&json),
            Err(SnapshotManifestError::InvalidLegacySchema)
        ));
    }

    #[test]
    fn explicit_migration_creates_new_manifest_and_new_snapshot_id() {
        // Legacy artifact carrying an OPAQUE, untrusted execution_id string.
        let legacy = LegacyReadyStateManifestV1 {
            schema: READY_STATE_V1_SCHEMA.to_string(),
            capsule_manifest_hash: Some("blake3:provenance".to_string()),
            execution_id: Some("legacy-opaque-not-trusted".to_string()),
        };
        // Migration requires a VerifiedExecutionId supplied by the caller (the
        // type refuses a raw id) — the legacy opaque string is never trusted as
        // identity.
        let anchor = verified(SEED_A);
        let migrated = legacy
            .migrate(&anchor, sample_compat(), sample_content())
            .expect("migration");

        // New immutable v1 manifest bound to the verified identity...
        assert_eq!(migrated.schema, SNAPSHOT_MANIFEST_V1_SCHEMA);
        assert_eq!(migrated.execution_id, *anchor.as_execution_id());
        // ...provenance carried across, never as identity.
        assert_eq!(
            migrated.capture_provenance.capsule_manifest_hash.as_deref(),
            Some("blake3:provenance")
        );
        // A brand-new snapshot_id is produced (the manifest is well-formed and
        // derives an id); nothing was reinterpreted in place.
        let new_id = migrated.snapshot_id().expect("new id");
        assert!(new_id.as_str().starts_with("blake3:"));
        // The migrated v1 manifest IS now selectable for its verified identity.
        let host = matching_host();
        let cand = SnapshotCandidate {
            manifest: migrated,
            status: AcceptanceStatus::Accepted,
            ranking: SnapshotRankingSignals::default(),
        };
        assert_eq!(
            select_snapshots(&anchor, &host, std::slice::from_ref(&cand)),
            vec![&cand]
        );
    }

    #[test]
    fn migrate_rejects_a_non_legacy_schema() {
        // The tolerant public fields let a caller construct a struct whose schema
        // is NOT the legacy tag (here, the v1 manifest schema). migrate() must
        // re-check and refuse to treat a non-legacy artifact as legacy.
        let not_legacy = LegacyReadyStateManifestV1 {
            schema: SNAPSHOT_MANIFEST_V1_SCHEMA.to_string(),
            capsule_manifest_hash: None,
            execution_id: None,
        };
        assert!(matches!(
            not_legacy.migrate(&verified(SEED_A), sample_compat(), sample_content()),
            Err(SnapshotManifestError::InvalidLegacySchema)
        ));
    }

    // --- Acceptance (Blocker 1): snapshot_id commits the captured content ---

    #[test]
    fn different_content_layers_produce_different_snapshot_id() {
        // Same execution_id + compatibility, but different memory / vmstate /
        // disk layer bytes MUST derive different snapshot_ids: snapshot_id is a
        // content address of the captured bytes, not just of identity+format.
        let base = sample_manifest(SEED_A);

        let mut memory = base.clone();
        memory.memory_layer_refs = vec![digest('9')];
        let mut vmstate = base.clone();
        vmstate.vmstate_layer_refs = vec![digest('9')];
        let mut disk = base.clone();
        disk.disk_layer_refs = vec![digest('9')];

        // Identity and compatibility are untouched across all three variants.
        for variant in [&memory, &vmstate, &disk] {
            assert_eq!(variant.execution_id, base.execution_id);
            assert_eq!(variant.compatibility_contract, base.compatibility_contract);
        }

        let ids = [
            base.snapshot_id().unwrap(),
            memory.snapshot_id().unwrap(),
            vmstate.snapshot_id().unwrap(),
            disk.snapshot_id().unwrap(),
        ];
        // All four content closures are distinct ⇒ all four ids are distinct.
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                assert_ne!(ids[i], ids[j], "content difference must change snapshot_id");
            }
        }
    }

    #[test]
    fn identical_content_closure_produces_identical_snapshot_id() {
        // Two manifests with the same full content closure derive the SAME id
        // deterministically (content addressing is stable).
        let a = sample_manifest(SEED_A);
        let b = sample_manifest(SEED_A);
        assert_eq!(a, b);
        assert_eq!(a.snapshot_id().unwrap(), b.snapshot_id().unwrap());
    }

    #[test]
    fn missing_content_commitment_fails_validate_and_derivation() {
        // A manifest whose captured-content commitment is missing/empty is not a
        // content address: validate() (and therefore snapshot_id()) fails closed.
        for drop in [
            ("memory_layer_refs", 0u8),
            ("vmstate_layer_refs", 1),
            ("disk_layer_refs", 2),
        ] {
            let mut manifest = sample_manifest(SEED_A);
            match drop.1 {
                0 => manifest.memory_layer_refs.clear(),
                1 => manifest.vmstate_layer_refs.clear(),
                _ => manifest.disk_layer_refs.clear(),
            }
            assert_eq!(
                manifest.validate(),
                Err(SnapshotManifestError::EmptyField(drop.0)),
                "{}",
                drop.0
            );
            // No id is derivable from an un-committed manifest.
            assert!(manifest.snapshot_id().is_err(), "{}", drop.0);
        }
    }

    // --- Acceptance (Blocker 2): ClassPortable matched by EXACT class identity ---

    #[test]
    fn class_portable_requires_exact_matching_class_identity() {
        let cand = candidate(SEED_A, AcceptanceStatus::Accepted, Default::default());

        // Exact class-identity match ⇒ selected.
        let host = matching_host();
        assert_eq!(
            select_snapshots(&verified(SEED_A), &host, std::slice::from_ref(&cand)),
            vec![&cand]
        );
        assert!(cand.manifest.compatibility_contract.is_satisfied_by(&host));

        // A DIFFERENT class identity ⇒ nothing (no tier lets it through).
        let mut different = matching_host();
        different.compatibility_class_identity = Some(digest('d'));
        assert!(
            select_snapshots(&verified(SEED_A), &different, std::slice::from_ref(&cand)).is_empty()
        );
        assert!(
            !cand
                .manifest
                .compatibility_contract
                .is_satisfied_by(&different)
        );

        // An ABSENT class identity ⇒ nothing (fail-closed on unknown class).
        let mut absent = matching_host();
        absent.compatibility_class_identity = None;
        assert!(
            select_snapshots(&verified(SEED_A), &absent, std::slice::from_ref(&cand)).is_empty()
        );
        assert!(
            !cand
                .manifest
                .compatibility_contract
                .is_satisfied_by(&absent)
        );
    }

    #[test]
    fn select_snapshots_matches_exactly_via_the_verified_wrapper() {
        // The exact-identity gate reads the proven id out of the wrapper: the
        // matching identity selects, a different verified identity selects
        // nothing (no substitute for exact equality).
        let host = matching_host();
        let cand = candidate(SEED_A, AcceptanceStatus::Accepted, Default::default());
        assert_eq!(
            select_snapshots(&verified(SEED_A), &host, std::slice::from_ref(&cand)),
            vec![&cand]
        );
        assert!(select_snapshots(&verified(SEED_B), &host, std::slice::from_ref(&cand)).is_empty());
    }

    #[test]
    fn compatibility_is_satisfied_by_matching_host() {
        assert!(sample_compat().is_satisfied_by(&matching_host()));
    }

    #[test]
    fn snapshot_id_and_execution_id_round_trip_through_json() {
        let manifest = sample_manifest(SEED_A);
        let json = serde_json::to_string(&manifest).unwrap();
        let parsed = SnapshotManifestV1::parse(&json).expect("round-trip");
        assert_eq!(parsed, manifest);
        assert_eq!(
            parsed.snapshot_id().unwrap(),
            manifest.snapshot_id().unwrap()
        );
    }

    // --- Acceptance (Blocker): capture-policy proven at SELECTION ---

    #[test]
    fn candidate_with_unsupported_capture_policy_is_never_selected() {
        // The candidate is captured `WorkloadIdle` (the sample default) and both
        // identity and every compatibility dimension match, but the host lists
        // only `Running` in `supported_capture_policies`. The host cannot prove it
        // can honor the workload_idle restore semantics, so the candidate is
        // dropped fail-closed at selection — the capture policy is the ONLY thing
        // keeping it out.
        let mut host = matching_host();
        host.supported_capture_policies = vec![CapturePolicyV1::Running];
        let cand = candidate(SEED_A, AcceptanceStatus::Accepted, Default::default());
        // Sanity: identity + compatibility both hold; only the capture policy differs.
        assert!(cand.manifest.compatibility_contract.is_satisfied_by(&host));
        assert_eq!(cand.manifest.capture_policy, CapturePolicyV1::WorkloadIdle);
        assert!(select_snapshots(&verified(SEED_A), &host, std::slice::from_ref(&cand)).is_empty());
        // An empty support list satisfies no Snapshot either.
        let mut none = matching_host();
        none.supported_capture_policies = vec![];
        assert!(select_snapshots(&verified(SEED_A), &none, std::slice::from_ref(&cand)).is_empty());
    }

    #[test]
    fn candidate_with_supported_capture_policy_and_matching_protocol_is_selected() {
        // Host supports the candidate's capture policy AND shares the restore
        // protocol (`runner_restore_contract` == `restore_protocol`), so the
        // candidate survives every gate and is selected.
        let host = matching_host();
        let cand = candidate(SEED_A, AcceptanceStatus::Accepted, Default::default());
        assert!(
            host.supported_capture_policies
                .contains(&cand.manifest.capture_policy)
        );
        assert_eq!(
            cand.manifest.restore_contract.restore_protocol,
            cand.manifest.compatibility_contract.runner_restore_contract
        );
        assert_eq!(
            select_snapshots(&verified(SEED_A), &host, std::slice::from_ref(&cand)),
            vec![&cand]
        );
    }

    // --- Acceptance (Blocker): restore_protocol identity enforced in validate() ---

    #[test]
    fn restore_protocol_must_equal_runner_restore_contract() {
        // `restore_contract.restore_protocol` and
        // `compatibility_contract.runner_restore_contract` are the same concept.
        // A manifest whose two disagree is rejected fail-closed at validate()
        // with `RestoreContractMismatch`, yields no `snapshot_id`, and is
        // therefore also unselectable via the defensive validate() gate — even
        // though identity + every host dimension would otherwise match.
        let mut manifest = sample_manifest(SEED_A);
        manifest.restore_contract.restore_protocol = "ato-restore/v2".to_string();
        // (compatibility_contract.runner_restore_contract is still "ato-restore/v1")
        assert_eq!(
            manifest.validate(),
            Err(SnapshotManifestError::RestoreContractMismatch)
        );
        // A mismatched manifest is not a content address.
        assert!(manifest.snapshot_id().is_err());
        let host = matching_host();
        let cand = SnapshotCandidate {
            manifest,
            status: AcceptanceStatus::Accepted,
            ranking: SnapshotRankingSignals::default(),
        };
        assert!(select_snapshots(&verified(SEED_A), &host, std::slice::from_ref(&cand)).is_empty());
    }

    // --- Acceptance (Major): capture-policy wire values match the RFC ---

    #[test]
    fn capture_policy_wire_values_match_the_rfc() {
        // RFC §8 pins the two policy spellings to exactly "running" and
        // "workload_idle".
        assert_eq!(
            serde_json::to_value(CapturePolicyV1::Running).unwrap(),
            serde_json::json!("running")
        );
        assert_eq!(
            serde_json::to_value(CapturePolicyV1::WorkloadIdle).unwrap(),
            serde_json::json!("workload_idle")
        );
        assert_eq!(
            serde_json::from_value::<CapturePolicyV1>(serde_json::json!("running")).unwrap(),
            CapturePolicyV1::Running
        );
        assert_eq!(
            serde_json::from_value::<CapturePolicyV1>(serde_json::json!("workload_idle")).unwrap(),
            CapturePolicyV1::WorkloadIdle
        );
        // The pre-RFC spelling "idle" and any unknown value fail closed (a closed
        // enum with explicit renames accepts neither).
        assert!(serde_json::from_value::<CapturePolicyV1>(serde_json::json!("idle")).is_err());
        assert!(
            serde_json::from_value::<CapturePolicyV1>(serde_json::json!("workload-idle")).is_err()
        );
        assert!(serde_json::from_value::<CapturePolicyV1>(serde_json::json!("suspended")).is_err());
    }
}
