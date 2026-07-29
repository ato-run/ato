//! `ato.capsule-program/v1` — the Capsule Program (declaration) identity
//! contract (ADR-014).
//!
//! Capsule Program Identity is the exact identity of a canonical, immutable
//! Capsule **declaration**: a pinned program source projection plus normalized
//! authored manifest intent. It does not claim identity of a fully resolved
//! program closure — Execution Identity ([`crate::execution_contract`]) remains
//! the sole exact identity of a resolved, runnable, target-specific launch
//! envelope, and this id is never an execution-compatibility, Snapshot
//! selection, placement, or restore key. When present, its structural
//! integrity and its parent-association claim are mandatory trusted-load
//! checks (ADR-014 §0, §5).
//!
//! Identity-bearing and non-identity data are separated at the type level,
//! mirroring the execution contract:
//!
//! * [`CapsuleProgramContractV1`] (and its facet structs) is the
//!   identity-bearing contract. Every field participates in
//!   `capsule_program_id`, deserialization is fail-closed
//!   (`deny_unknown_fields` on every identity struct), and set-like lists must
//!   already be in canonical (sorted, duplicate-free) order. Absent optional
//!   fields have exactly one canonical spelling — the key is omitted.
//! * [`CapsuleProgramEnvelopeV1`] is the non-identity envelope around a
//!   contract: provenance (`authoring_schema`/`name`/`version`), diagnostics,
//!   timestamps, and the stored `capsule_program_id`. It is tolerant of
//!   unknown fields by design — nothing in the envelope besides the embedded
//!   contract may influence the id, and [`CapsuleProgramEnvelopeV1::verify`]
//!   re-derives the id fail-closed.
//!
//! Canonical form (normative):
//!
//! ```text
//! capsule_program_id =
//!   "blake3:" + hex(BLAKE3(UTF8("ato.capsule-program/v1") || 0x00 || JCS(contract)))
//! ```
//!
//! # Semantic value types (ADR-014 §2.2 Rule 4)
//!
//! No identity-bearing string field is hashed as an uninterpreted `String`.
//! Every string leaf of the IR is one of the semantic newtypes below, each
//! with one canonical grammar validated on parse/`Deserialize` and one
//! canonical serialization. A field that would have to be a bare `String` is,
//! by construction, an unclassified field — it cannot compile, so the ADR's
//! classification matrix cannot silently rot.
//!
//! Base value types: [`SourceRelativePath`] (with its canonical `Root` = `"."`
//! spelling), [`GuestPath`] (reused from the execution contract),
//! [`HttpRequestTarget`], [`TcpProbeTarget`], [`ProbePortReference`],
//! [`GlobPattern`], [`RemoteArtifactRef`], [`Sha256DigestPin`],
//! [`CasContentDigest`], [`GitCommitRevision`], [`WitWorldRef`],
//! [`ContainerUserSpec`], [`TemplatedString`] (reused from the dependency
//! grammar), [`OpaqueCommand`], [`OpaqueAuthoredString`], and
//! [`ProgramIdentifier`]. [`SourceExistingPath`] and
//! [`SourceRelativeFuturePath`] are validation *policies* over
//! [`SourceRelativePath`], not separate value types: existence for the
//! `Existing` policy is checked by the CALLER (adapter/projection) against the
//! projected tree, never in `Deserialize`.
//!
//! Validators enforce CANONICAL SPELLING only — semantics belong to the
//! existing v0.3 normalizer (ADR-014 §2.0.1). They deliberately do not
//! over-reject (e.g. a [`TcpProbeTarget`] of `"5432"` is valid).
//!
//! # The IR — [`ProgramManifestIntentV1`]
//!
//! An independent normalized intent representation (never a projection of
//! [`CapsuleManifest`](crate::types::CapsuleManifest) minus a denylist),
//! designed against the ADR-014 §2.1/§2.2 classification:
//!
//! * **Rule 1** — within an identity-bearing section, every nested field is
//!   identity-bearing by default.
//! * **Rule 2** — the complete non-identity nested exceptions are EXCLUDED
//!   from the IR: `build.outputs.*`, `build.policy.*`,
//!   `exports.cli.<name>.description`, `targets.<label>.model_filename`,
//!   `targets.<label>.model_format`.
//! * **Rule 3** — unsupported nested fields fail closed at ADAPTER time and
//!   have no IR representation: `targets.<label>.engine_path`, an absolute or
//!   out-of-tree `targets.<label>.model`, and `working_dir` on a Wasm target.
//!   `working_dir` is runtime-dependent, so the IR carries
//!   [`NormalizedWorkingDir`] (source-relative for source/web targets, an
//!   absolute [`GuestPath`] for OCI targets).
//! * **Rule 4** — every string leaf uses the matrix's semantic newtype.
//!
//! Canonicalization (ADR-014 §2.3): maps are [`UniqueBTreeMap`] — sorted keys,
//! and a repeated JSON key is rejected on deserialization rather than
//! silently last-wins (`BTreeMap`'s stock behaviour), so one typed value never
//! has two byte-distinct preimages; set-like lists are sorted +
//! deduplicated and validated as strictly increasing; order-sensitive lists
//! (build lifecycle, `targets.preference`, `pack.include`/`exclude`, argv
//! lists, `external.*.providers`, `snapshot.warmup_paths`, `config_schema`)
//! are preserved as authored; absent ≡ explicit default has exactly one
//! canonical spelling — the key is omitted (`Option` + `skip_serializing_if`).
//! Structured targets (`targets.wasm`/`targets.source`/`targets.oci`) are
//! canonicalized by the adapter INTO the same [`NormalizedTargetIntent`] shape
//! as named targets, under the reserved names `"wasm"`/`"source"`/`"oci"`.
//!
//! # Parent link (ADR-014 §5)
//!
//! [`ExecutionContractEnvelopeV1`] carries the additive **non-identity claim**
//! field `capsule_program_id`; [`verify_program_parent`] proves the claim is
//! internally consistent against a [`VerifiedCapsuleProgramId`]. It is a
//! pairwise check only — lock-state interpretation (the four-state matrix,
//! incl. the orphan-claim `ParentEnvelopeMissing` rejection) lives in
//! `capsule_lock/execution.rs`. No derivation proof is provided in Phase 0:
//! nothing proves an execution contract was actually resolved from the named
//! declaration.
//!
//! The normative spec is
//! `docs/rfcs/accepted/ADR-014-capsule-program-identity.md` (Decision §0–§5).
//! Shared test vectors land under
//! `crates/capsule/tests/fixtures/capsule_program_contract/`.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use unicode_normalization::is_nfc;

use crate::contract::program_source_projection::{
    StagedCapsuleSource, VerifiedPinnedSourceMaterialization,
};
use crate::execution_contract::{
    ExecutionContractEnvelopeV1, GuestPath, schema_domained_blake3_id,
};
use crate::types::TemplatedString;

pub const CAPSULE_PROGRAM_V1_SCHEMA: &str = "ato.capsule-program/v1";
pub const CAPSULE_PROGRAM_MANIFEST_INTENT_V1_SCHEMA: &str =
    "ato.capsule-program-manifest-intent/v1";
pub const CAPSULE_PROGRAM_SOURCE_PROJECTION_V1_SCHEMA: &str =
    "ato.capsule-program-source-projection/v1";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CapsuleProgramError {
    #[error("capsule program contract schema must be ato.capsule-program/v1")]
    InvalidSchema,
    #[error("capsule_program_id must be blake3:<64 lowercase hex characters>")]
    InvalidCapsuleProgramId,
    #[error("stored capsule_program_id {stored} does not match the canonical hash {computed}")]
    CapsuleProgramIdMismatch { stored: String, computed: String },
    #[error("failed to canonicalize capsule program contract: {0}")]
    Canonicalization(String),
    #[error("capsule program value '{field}' is not canonical: {reason}")]
    InvalidValue { field: &'static str, reason: String },
    #[error("capsule program identity does not support field '{0}' (fail closed)")]
    UnsupportedField(&'static str),
    #[error("capsule program list '{0}' must be sorted and contain no duplicates")]
    NonCanonicalList(&'static str),
    #[error("program source projection failed: {0}")]
    SourceProjection(String),
    #[error("strict program manifest input rejected: {0}")]
    ManifestInput(String),
    #[error("program manifest load failed: {0}")]
    ManifestLoad(String),
    #[error("input is not a pinned source materialization: {0}")]
    NotPinnedMaterialization(String),
    /// The archive is well formed, but this platform can only give it a
    /// platform-dependent identity — which is not an identity. A1 folds the
    /// owner-executable bit into the tree hash where the filesystem carries it
    /// and treats every file as non-executable where it does not, so the same
    /// archive would mint one `capsule_program_id` on unix and another here.
    /// Its own variant rather than a
    /// [`Self::NotPinnedMaterialization`] string: the input IS a pinned
    /// materialization, and the condition is a property of the host, so a
    /// caller must be able to tell it from a malformed archive without
    /// matching on message text.
    #[error(
        "{archive} cannot be given a portable capsule_program_id on this platform: entry \
         {entry} carries the owner-executable bit, which this platform's filesystem cannot \
         represent — A1 folds that bit into the source-tree digest, so extracting here would \
         mint a different id than unix does for the same archive"
    )]
    NonPortableExecutableBit { archive: String, entry: String },
}

fn invalid(field: &'static str, reason: impl Into<String>) -> CapsuleProgramError {
    CapsuleProgramError::InvalidValue {
        field,
        reason: reason.into(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Identity id + proof-carrying wrapper
// ─────────────────────────────────────────────────────────────────────────────

/// The Capsule Program identity id: `blake3:<64 lowercase hex>`. Mirrors
/// [`ExecutionId`](crate::execution_contract::ExecutionId) exactly — the
/// `blake3:` prefix is not a schema discriminator; the domain separator
/// [`CAPSULE_PROGRAM_V1_SCHEMA`] is.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CapsuleProgramId(String);

impl CapsuleProgramId {
    pub fn new(value: String) -> Result<Self, CapsuleProgramError> {
        let Some(hex) = value.strip_prefix("blake3:") else {
            return Err(CapsuleProgramError::InvalidCapsuleProgramId);
        };
        if hex.len() != 64 || !hex.bytes().all(is_lower_hex_byte) {
            return Err(CapsuleProgramError::InvalidCapsuleProgramId);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CapsuleProgramId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for CapsuleProgramId {
    type Error = CapsuleProgramError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<CapsuleProgramId> for String {
    fn from(value: CapsuleProgramId) -> Self {
        value.0
    }
}

/// A proof-carrying wrapper over a [`CapsuleProgramId`] whose value has been
/// shown to equal the canonical hash of its program contract.
///
/// `VerifiedCapsuleProgramId` proves the program id equals the canonical hash
/// of its declaration contract. It does NOT prove signature, provenance,
/// acceptance, or that any execution was actually resolved from the
/// declaration (Phase 0 provides no derivation proof — ADR-014 §5).
///
/// It cannot be minted from a bare [`CapsuleProgramId`]: it has a private
/// field and no public constructor — no `new`, no `From<CapsuleProgramId>`,
/// no `TryFrom<String>`, no `Deserialize`, and it is never produced directly
/// from [`CapsuleProgramContractV1::compute_capsule_program_id`] (which stays
/// a pure hash). The **only** sanctioned way to obtain one in v1 is
/// [`CapsuleProgramEnvelopeV1::verified_capsule_program_id`], which re-derives
/// the canonical hash via [`CapsuleProgramEnvelopeV1::verify`] and only then
/// wraps the now-proven stored id.
///
/// Because it can only come from that path, an API taking a
/// `&VerifiedCapsuleProgramId` — [`verify_program_parent`] — statically
/// refuses a raw, unproven `CapsuleProgramId`.
///
/// A raw `CapsuleProgramId` cannot be wrapped (there is no public field):
///
/// ```compile_fail
/// use capsule::capsule_program_contract::{CapsuleProgramId, VerifiedCapsuleProgramId};
///
/// let raw = CapsuleProgramId::new(format!("blake3:{}", "0".repeat(64))).unwrap();
/// // The field is private and there is no `new`/`From`/`TryFrom`/`Deserialize`.
/// let _wrapped = VerifiedCapsuleProgramId { capsule_program_id: raw };
/// ```
///
/// nor converted (no conversion impl exists):
///
/// ```compile_fail
/// use capsule::capsule_program_contract::{CapsuleProgramId, VerifiedCapsuleProgramId};
///
/// let raw = CapsuleProgramId::new(format!("blake3:{}", "0".repeat(64))).unwrap();
/// // There is no `From`/`Into` from a raw id: this is a type error.
/// let _wrapped: VerifiedCapsuleProgramId = raw.into();
/// ```
///
/// and a raw `&CapsuleProgramId` cannot stand in for a
/// `&VerifiedCapsuleProgramId` at the parent-link call site:
///
/// ```compile_fail
/// use capsule::capsule_program_contract::{verify_program_parent, CapsuleProgramId};
/// use capsule::execution_contract::ExecutionContractEnvelopeV1;
///
/// let raw = CapsuleProgramId::new(format!("blake3:{}", "0".repeat(64))).unwrap();
/// let execution: ExecutionContractEnvelopeV1 = unimplemented!();
/// // A raw &CapsuleProgramId is not a &VerifiedCapsuleProgramId: type error.
/// let _ = verify_program_parent(&raw, &execution);
/// ```
///
/// including the bare return of the pure hash function:
///
/// ```compile_fail
/// use capsule::capsule_program_contract::{verify_program_parent, CapsuleProgramContractV1};
/// use capsule::execution_contract::ExecutionContractEnvelopeV1;
///
/// let contract: CapsuleProgramContractV1 = unimplemented!();
/// let computed = contract.compute_capsule_program_id().unwrap();
/// let execution: ExecutionContractEnvelopeV1 = unimplemented!();
/// // compute_capsule_program_id() returns a bare CapsuleProgramId, not a
/// // proof: this is a type error.
/// let _ = verify_program_parent(&computed, &execution);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCapsuleProgramId {
    capsule_program_id: CapsuleProgramId,
}

impl VerifiedCapsuleProgramId {
    /// The proven program id: the canonical hash of the declaration contract
    /// it was derived from.
    pub fn as_capsule_program_id(&self) -> &CapsuleProgramId {
        &self.capsule_program_id
    }

    /// Crate-internal, **proof-preserving** construction seam. NOT public and
    /// NOT a second "way to obtain" a `VerifiedCapsuleProgramId`: it exists
    /// only so the one sanctioned path —
    /// [`CapsuleProgramEnvelopeV1::verified_capsule_program_id`] — can build
    /// the wrapper without a public constructor.
    ///
    /// Unlike a bare wrap, this seam **re-derives** the canonical program id
    /// from `contract` and compares it to the caller-supplied id, failing
    /// closed with [`CapsuleProgramError::CapsuleProgramIdMismatch`] on any
    /// disagreement. A caller (or a test using a fake id) therefore cannot
    /// mint a wrapper whose id differs from its contract's hash: the proof is
    /// recomputed here, not trusted.
    ///
    /// Scoped to `crate::contract` (not the whole crate) so the "exactly one
    /// way to obtain a verified id" guarantee cannot widen to any future
    /// capsule-crate module.
    pub(in crate::contract) fn verify_contract_id(
        contract: &CapsuleProgramContractV1,
        capsule_program_id: &CapsuleProgramId,
    ) -> Result<Self, CapsuleProgramError> {
        let computed = contract.compute_capsule_program_id()?;
        if computed != *capsule_program_id {
            return Err(CapsuleProgramError::CapsuleProgramIdMismatch {
                stored: capsule_program_id.to_string(),
                computed: computed.to_string(),
            });
        }
        Ok(Self {
            capsule_program_id: capsule_program_id.clone(),
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Program source contract
// ─────────────────────────────────────────────────────────────────────────────

/// The A1 source-tree digest: ALWAYS sha256, 32 bytes, lowercase hex, spelled
/// `sha256:<64 lowercase hex>` (the verbatim A1 blob string). A bare
/// [`ContentDigest`](crate::execution_contract::ContentDigest) would also
/// admit `blake3` — structurally valid, normatively wrong — so this narrower
/// type enforces the A1 contract (ADR-014 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProgramSourceDigest([u8; 32]);

impl ProgramSourceDigest {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn bytes(self) -> [u8; 32] {
        self.0
    }

    /// Parse the verbatim A1 spelling: `sha256:` + exactly 64 LOWERCASE hex.
    /// A `blake3:` algorithm, uppercase hex, or any other length/shape is
    /// rejected fail-closed.
    pub fn parse(value: &str) -> Result<Self, CapsuleProgramError> {
        let field = "source.digest";
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err(invalid(
                field,
                "must be sha256:<64 lowercase hex characters> (the A1 tree hash is sha256-only)",
            ));
        };
        let bytes = decode_hex_fixed::<32>(field, hex, false)?;
        Ok(Self(bytes))
    }
}

impl fmt::Display for ProgramSourceDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "sha256:{}", hex::encode(self.0))
    }
}

impl TryFrom<String> for ProgramSourceDigest {
    type Error = CapsuleProgramError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<ProgramSourceDigest> for String {
    fn from(value: ProgramSourceDigest) -> Self {
        value.to_string()
    }
}

/// v1 projection rules are fully fixed by ADR-014 §1; there is no per-Capsule
/// payload to hash, so the schema marker is a unit-like type, not a `String`.
/// Serializes as exactly [`CAPSULE_PROGRAM_SOURCE_PROJECTION_V1_SCHEMA`]; any
/// other spelling fails deserialization closed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProgramSourceProjectionSchemaV1;

impl Serialize for ProgramSourceProjectionSchemaV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(CAPSULE_PROGRAM_SOURCE_PROJECTION_V1_SCHEMA)
    }
}

impl<'de> Deserialize<'de> for ProgramSourceProjectionSchemaV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value != CAPSULE_PROGRAM_SOURCE_PROJECTION_V1_SCHEMA {
            return Err(serde::de::Error::custom(format!(
                "projection_schema must be exactly \
                 '{CAPSULE_PROGRAM_SOURCE_PROJECTION_V1_SCHEMA}', got '{value}'"
            )));
        }
        Ok(Self)
    }
}

/// The pinned program source projection facet (ADR-014 §1): the frozen A1
/// tree hash of the projected source tree (control files excluded) plus the
/// projection schema marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramSourceContract {
    pub digest: ProgramSourceDigest,
    pub projection_schema: ProgramSourceProjectionSchemaV1,
}

// ─────────────────────────────────────────────────────────────────────────────
// Semantic value types (ADR-014 §2.2 Rule 4)
// ─────────────────────────────────────────────────────────────────────────────

fn is_lower_hex_byte(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}

/// Decode exactly `N` bytes of hex. `allow_uppercase` lowercase-normalizes
/// mixed-case authoring spellings; the canonical spelling is always lowercase.
fn decode_hex_fixed<const N: usize>(
    field: &'static str,
    encoded: &str,
    allow_uppercase: bool,
) -> Result<[u8; N], CapsuleProgramError> {
    if encoded.len() != N * 2 {
        return Err(invalid(
            field,
            format!("must be exactly {} hex characters", N * 2),
        ));
    }
    let canonical = if allow_uppercase {
        if !encoded.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid(field, "must be hex"));
        }
        encoded.to_ascii_lowercase()
    } else {
        if !encoded.bytes().all(is_lower_hex_byte) {
            return Err(invalid(field, "must be lowercase hex"));
        }
        encoded.to_string()
    };
    let decoded = hex::decode(canonical).map_err(|error| invalid(field, error.to_string()))?;
    decoded
        .try_into()
        .map_err(|_| invalid(field, "wrong decoded length"))
}

macro_rules! string_semantic_type {
    ($type:ident) => {
        impl $type {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl TryFrom<String> for $type {
            type Error = CapsuleProgramError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::parse(&value)
            }
        }

        impl From<$type> for String {
            fn from(value: $type) -> Self {
                value.0
            }
        }
    };
}

/// A source-relative path with a canonical `Root` form (ADR-014 §2.2).
///
/// `"."` is the ONLY canonical spelling of `Root` — the existing v0.3
/// normalizer legitimately produces `"."` for a web static root entrypoint.
/// Non-canonical spellings (`""`, `"./"`, `"./x"`, `"x/."`, `"x/.."`) are
/// rejected fail-closed, never silently normalized. `Relative` is
/// relative-only, UTF-8, NFC, `/`-separated, with no `.`/`..`/empty segments,
/// no leading/trailing `/`, no backslash, and no control character.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum SourceRelativePath {
    /// Canonical spelling: exactly `"."`.
    Root,
    /// A validated relative path such as `"src/app"`.
    Relative(String),
}

impl SourceRelativePath {
    pub fn parse(value: &str) -> Result<Self, CapsuleProgramError> {
        let field = "source-relative path";
        if value == "." {
            return Ok(Self::Root);
        }
        if value.is_empty() {
            return Err(invalid(
                field,
                "must not be empty ('.' is the only root spelling)",
            ));
        }
        if value.chars().any(|ch| ch == '\\' || ch.is_control()) {
            return Err(invalid(
                field,
                "must not contain a backslash, NUL, or Unicode control character",
            ));
        }
        if !is_nfc(value) {
            return Err(invalid(field, "must be Unicode NFC-normalized"));
        }
        if value.starts_with('/') || value.ends_with('/') {
            return Err(invalid(field, "must not have a leading or trailing slash"));
        }
        for segment in value.split('/') {
            if segment.is_empty() {
                return Err(invalid(field, "must not contain an empty path segment"));
            }
            if segment == "." || segment == ".." {
                return Err(invalid(field, "must not contain a '.' or '..' segment"));
            }
        }
        Ok(Self::Relative(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Root => ".",
            Self::Relative(value) => value,
        }
    }
}

impl fmt::Display for SourceRelativePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl TryFrom<String> for SourceRelativePath {
    type Error = CapsuleProgramError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<SourceRelativePath> for String {
    fn from(value: SourceRelativePath) -> Self {
        value.as_str().to_string()
    }
}

/// Validation policy over [`SourceRelativePath`]: the path MUST exist in the
/// program source projection as a regular file or directory of the expected
/// kind. Existence is checked by the CALLER (adapter/projection) against the
/// projected tree — never in `Deserialize`, which is lexical-only.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceExistingPath(pub SourceRelativePath);

/// Validation policy over [`SourceRelativePath`]: lexical validation only —
/// the target may be produced by a later build step, so existence is never
/// checked.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceRelativeFuturePath(pub SourceRelativePath);

/// An absolute-path HTTP request-target (`"/"`, `"/app"`, …): starts with
/// `/`, no control character, no backslash, no whitespace.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HttpRequestTarget(String);

impl HttpRequestTarget {
    pub fn parse(value: &str) -> Result<Self, CapsuleProgramError> {
        let field = "HTTP request-target";
        if !value.starts_with('/') {
            return Err(invalid(field, "must start with '/' (origin-form)"));
        }
        if value
            .chars()
            .any(|ch| ch == '\\' || ch.is_control() || ch.is_whitespace())
        {
            return Err(invalid(
                field,
                "must not contain whitespace, a backslash, or a control character",
            ));
        }
        Ok(Self(value.to_string()))
    }
}

string_semantic_type!(HttpRequestTarget);

/// A `host:port` / bare-port TCP probe target: non-empty, no whitespace, no
/// control character. Canonical spelling only — `"5432"` and `"db:5432"` are
/// both valid; port semantics belong to the existing normalizer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TcpProbeTarget(String);

impl TcpProbeTarget {
    pub fn parse(value: &str) -> Result<Self, CapsuleProgramError> {
        parse_no_whitespace_no_control("TCP probe target", value)?;
        Ok(Self(value.to_string()))
    }
}

string_semantic_type!(TcpProbeTarget);

/// A placeholder NAME a readiness probe refers to (e.g. `"PORT"`, `"web"`) —
/// NOT a host:port target. Non-empty, no whitespace, no control character.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProbePortReference(String);

impl ProbePortReference {
    pub fn parse(value: &str) -> Result<Self, CapsuleProgramError> {
        parse_no_whitespace_no_control("probe port reference", value)?;
        Ok(Self(value.to_string()))
    }
}

string_semantic_type!(ProbePortReference);

/// An authored glob pattern, hashed as authored: non-empty, no NUL/control
/// character. Glob semantics are never interpreted here.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct GlobPattern(String);

impl GlobPattern {
    pub fn parse(value: &str) -> Result<Self, CapsuleProgramError> {
        let field = "glob pattern";
        if value.is_empty() {
            return Err(invalid(field, "must not be empty"));
        }
        if value.chars().any(char::is_control) {
            return Err(invalid(
                field,
                "must not contain a NUL or control character",
            ));
        }
        Ok(Self(value.to_string()))
    }
}

string_semantic_type!(GlobPattern);

/// A URL / OCI image ref / model-repo ref, hashed as authored: non-empty, no
/// control character, no whitespace.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RemoteArtifactRef(String);

impl RemoteArtifactRef {
    pub fn parse(value: &str) -> Result<Self, CapsuleProgramError> {
        parse_no_whitespace_no_control("remote artifact ref", value)?;
        Ok(Self(value.to_string()))
    }
}

string_semantic_type!(RemoteArtifactRef);

fn parse_no_whitespace_no_control(
    field: &'static str,
    value: &str,
) -> Result<(), CapsuleProgramError> {
    if value.is_empty() {
        return Err(invalid(field, "must not be empty"));
    }
    if value
        .chars()
        .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(invalid(
            field,
            "must not contain whitespace or a control character",
        ));
    }
    Ok(())
}

/// A pinned SHA-256 whose canonical IR spelling is uniformly BARE 64
/// lowercase hex (ADR-014 §2.2 spelling table). Per-field authoring spellings
/// normalize INTO it:
///
/// * [`Sha256DigestPin::parse_flexible`] — bare or `sha256:`-prefixed,
///   mixed-case hex lowercased (`model_sha256`, `model_repo_sha256`,
///   `platforms.<os-arch>.sha256`);
/// * [`Sha256DigestPin::parse_prefixed`] — `sha256:` REQUIRED, matching the
///   existing validator for `targets.source_digest` (the unprefixed spelling
///   stays rejected there).
///
/// `Deserialize` (IR read-back) accepts the canonical bare-lowercase form
/// ONLY — a prefixed or uppercase IR spelling fails closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Sha256DigestPin([u8; 32]);

impl Sha256DigestPin {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn bytes(self) -> [u8; 32] {
        self.0
    }

    /// Authoring spelling for `model_sha256` / `model_repo_sha256` /
    /// `platforms.<os-arch>.sha256`: bare or `sha256:`-prefixed; mixed-case
    /// hex is lowercased. Both spellings produce the SAME IR value.
    pub fn parse_flexible(value: &str) -> Result<Self, CapsuleProgramError> {
        let hex = value.strip_prefix("sha256:").unwrap_or(value);
        Ok(Self(decode_hex_fixed::<32>("sha256 pin", hex, true)?))
    }

    /// Authoring spelling for `targets.source_digest`: the `sha256:` prefix
    /// is REQUIRED (the existing validator rejects the unprefixed spelling,
    /// so the strict layer rejects it too).
    pub fn parse_prefixed(value: &str) -> Result<Self, CapsuleProgramError> {
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err(invalid("sha256 pin", "must start with 'sha256:'"));
        };
        Ok(Self(decode_hex_fixed::<32>("sha256 pin", hex, true)?))
    }

    /// The canonical IR spelling: exactly 64 bare LOWERCASE hex characters.
    fn parse_canonical(value: &str) -> Result<Self, CapsuleProgramError> {
        if value.contains(':') {
            return Err(invalid(
                "sha256 pin",
                "canonical IR spelling is bare 64 lowercase hex (no 'sha256:' prefix)",
            ));
        }
        Ok(Self(decode_hex_fixed::<32>("sha256 pin", value, false)?))
    }
}

impl fmt::Display for Sha256DigestPin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

impl TryFrom<String> for Sha256DigestPin {
    type Error = CapsuleProgramError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse_canonical(&value)
    }
}

impl From<Sha256DigestPin> for String {
    fn from(value: Sha256DigestPin) -> Self {
        value.to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CasDigestAlgorithm {
    Sha256,
    Blake3,
}

impl CasDigestAlgorithm {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sha256 => "sha256",
            Self::Blake3 => "blake3",
        }
    }
}

/// An algorithm-prefixed CAS digest (`sha256:<64 hex>` / `blake3:<64 hex>`)
/// for `targets.wasm.digest` / `targets.oci.digest`. Mixed-case hex is
/// lowercase-normalized on parse; a bare (unprefixed) spelling is rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CasContentDigest {
    algorithm: CasDigestAlgorithm,
    bytes: [u8; 32],
}

impl CasContentDigest {
    pub fn new(algorithm: CasDigestAlgorithm, bytes: [u8; 32]) -> Self {
        Self { algorithm, bytes }
    }

    pub fn algorithm(self) -> CasDigestAlgorithm {
        self.algorithm
    }

    pub fn bytes(self) -> [u8; 32] {
        self.bytes
    }

    pub fn parse(value: &str) -> Result<Self, CapsuleProgramError> {
        let field = "CAS content digest";
        let Some((algorithm, hex)) = value.split_once(':') else {
            return Err(invalid(
                field,
                "must be <algorithm>:<64 hex characters> (bare hex is rejected)",
            ));
        };
        let algorithm = match algorithm {
            "sha256" => CasDigestAlgorithm::Sha256,
            "blake3" => CasDigestAlgorithm::Blake3,
            other => {
                return Err(invalid(
                    field,
                    format!("algorithm must be sha256 or blake3, got '{other}'"),
                ));
            }
        };
        Ok(Self {
            algorithm,
            bytes: decode_hex_fixed::<32>(field, hex, true)?,
        })
    }
}

impl fmt::Display for CasContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}",
            self.algorithm.as_str(),
            hex::encode(self.bytes)
        )
    }
}

impl TryFrom<String> for CasContentDigest {
    type Error = CapsuleProgramError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<CasContentDigest> for String {
    fn from(value: CasContentDigest) -> Self {
        value.to_string()
    }
}

/// An immutable 40-hex git commit (`targets.<label>.model_revision`) — a
/// revision, not a content digest, so it is a distinct type from the 64-hex
/// pins. Mixed-case hex is lowercase-normalized on parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct GitCommitRevision([u8; 20]);

impl GitCommitRevision {
    pub fn new(bytes: [u8; 20]) -> Self {
        Self(bytes)
    }

    pub fn bytes(self) -> [u8; 20] {
        self.0
    }

    pub fn parse(value: &str) -> Result<Self, CapsuleProgramError> {
        Ok(Self(decode_hex_fixed::<20>(
            "git commit revision",
            value,
            true,
        )?))
    }
}

impl fmt::Display for GitCommitRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

impl TryFrom<String> for GitCommitRevision {
    type Error = CapsuleProgramError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<GitCommitRevision> for String {
    fn from(value: GitCommitRevision) -> Self {
        value.to_string()
    }
}

/// A validated WIT world reference (`"wasi:cli/command"`,
/// `"uarc:v1/http-handler"`). Contains `:` and `/`, so it is NOT an
/// identifier: non-empty, ASCII graphic (no space). When `targets.wasm.world`
/// is absent, the adapter default-expands it to
/// [`WitWorldRef::default_cli_command`] before hashing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct WitWorldRef(String);

impl WitWorldRef {
    pub fn parse(value: &str) -> Result<Self, CapsuleProgramError> {
        let field = "WIT world reference";
        if value.is_empty() {
            return Err(invalid(field, "must not be empty"));
        }
        if !value.chars().all(|ch| ch.is_ascii_graphic()) {
            return Err(invalid(field, "must be ASCII graphic (no whitespace)"));
        }
        Ok(Self(value.to_string()))
    }

    /// The default-expanded world for a Wasm target that omits `world`.
    pub fn default_cli_command() -> Self {
        Self("wasi:cli/command".to_string())
    }
}

string_semantic_type!(WitWorldRef);

/// A container user spec: `"uid"`, `"uid:gid"`, or an image-resolvable
/// user/group name (`"1000:1000"` is valid — it fails the identifier
/// grammar, hence the dedicated type). Non-empty, no whitespace, no control
/// character.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContainerUserSpec(String);

impl ContainerUserSpec {
    pub fn parse(value: &str) -> Result<Self, CapsuleProgramError> {
        parse_no_whitespace_no_control("container user spec", value)?;
        Ok(Self(value.to_string()))
    }
}

string_semantic_type!(ContainerUserSpec);

/// An authored command string / argv element, hashed as authored with no path
/// interpretation. NUL only is rejected; an argv element may legitimately be
/// empty.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OpaqueCommand(String);

impl OpaqueCommand {
    pub fn parse(value: &str) -> Result<Self, CapsuleProgramError> {
        if value.contains('\0') {
            return Err(invalid("command", "must not contain NUL"));
        }
        Ok(Self(value.to_string()))
    }
}

string_semantic_type!(OpaqueCommand);

/// An authored free-form value, hashed verbatim. Per ADR-014 r7 this is a
/// FINITE enumeration over the classified fields (env values, runtime/driver
/// hints, version constraints, purposes, timeout strings, reasons), never a
/// catch-all. NUL only is rejected.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OpaqueAuthoredString(String);

impl OpaqueAuthoredString {
    pub fn parse(value: &str) -> Result<Self, CapsuleProgramError> {
        if value.contains('\0') {
            return Err(invalid("authored string", "must not contain NUL"));
        }
        Ok(Self(value.to_string()))
    }
}

string_semantic_type!(OpaqueAuthoredString);

/// A name / label / map key / env-variable name. Deliberately loose (ASCII
/// graphic, no whitespace, non-empty) so real manifests — `"my-app"`,
/// `"NODE_ENV"`, `"linux-x86_64"`, `"service@1"` — are never over-rejected;
/// naming policy belongs to the existing normalizer.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProgramIdentifier(String);

impl ProgramIdentifier {
    pub fn parse(value: &str) -> Result<Self, CapsuleProgramError> {
        let field = "identifier";
        if value.is_empty() {
            return Err(invalid(field, "must not be empty"));
        }
        if !value.chars().all(|ch| ch.is_ascii_graphic()) {
            return Err(invalid(field, "must be ASCII graphic (no whitespace)"));
        }
        Ok(Self(value.to_string()))
    }
}

string_semantic_type!(ProgramIdentifier);

// ─────────────────────────────────────────────────────────────────────────────
// ProgramManifestIntentV1 — the normalized manifest-intent IR
// ─────────────────────────────────────────────────────────────────────────────

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

/// A sorted map whose `Deserialize` fails closed on a repeated key.
///
/// `BTreeMap`'s stock deserializer inserts every entry, so a repeated JSON key
/// silently last-wins: two byte-distinct documents would map to one typed
/// value and therefore claim one `capsule_program_id`. That is an identity
/// preimage ambiguity no non-Rust consumer could detect either (`JSON.parse`
/// is last-wins too), so EVERY identity-bearing map in the IR uses this type —
/// nested ones included — mirroring the execution contract's
/// `present_non_empty_unique_map` (ADR-014 §2.3: maps are sorted with
/// duplicate-key rejection). Pinned by the `invalid-duplicate-*` vectors.
///
/// The serialized form is exactly `BTreeMap`'s (a JSON object, sorted keys),
/// so the canonical bytes and ids recorded by the shared vectors are
/// independent of the wrapper. In-memory construction cannot introduce a
/// duplicate key, so the fail-closed check is only needed on the JSON layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UniqueBTreeMap<K, V>(BTreeMap<K, V>);

impl<K, V> UniqueBTreeMap<K, V> {
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Values are freely mutable; the KEY set is only ever extended through
    /// [`UniqueBTreeMap::insert`] or a deserialization that checked it.
    pub fn values_mut(&mut self) -> std::collections::btree_map::ValuesMut<'_, K, V> {
        self.0.values_mut()
    }
}

impl<K: Ord, V> UniqueBTreeMap<K, V> {
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.0.insert(key, value)
    }
}

impl<K, V> Default for UniqueBTreeMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

/// Read-only `BTreeMap` surface (`get`/`iter`/`values`/`contains_key`/…);
/// mutation stays on the inherent [`UniqueBTreeMap::insert`].
impl<K, V> std::ops::Deref for UniqueBTreeMap<K, V> {
    type Target = BTreeMap<K, V>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<K: Ord, V> FromIterator<(K, V)> for UniqueBTreeMap<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl<K, V> IntoIterator for UniqueBTreeMap<K, V> {
    type Item = (K, V);
    type IntoIter = std::collections::btree_map::IntoIter<K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a, K, V> IntoIterator for &'a UniqueBTreeMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = std::collections::btree_map::Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<K: Serialize, V: Serialize> Serialize for UniqueBTreeMap<K, V> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de, K, V> Deserialize<'de> for UniqueBTreeMap<K, V>
where
    K: Deserialize<'de> + Ord + fmt::Display,
    V: Deserialize<'de>,
{
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UniqueMapVisitor<K, V>(std::marker::PhantomData<(K, V)>);

        impl<'de, K, V> serde::de::Visitor<'de> for UniqueMapVisitor<K, V>
        where
            K: Deserialize<'de> + Ord + fmt::Display,
            V: Deserialize<'de>,
        {
            type Value = UniqueBTreeMap<K, V>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a map with unique keys")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut access: A,
            ) -> Result<Self::Value, A::Error> {
                let mut map = BTreeMap::new();
                while let Some((key, value)) = access.next_entry::<K, V>()? {
                    // Checked before insert so the rejected key is still owned
                    // here and can name itself in the error.
                    if map.contains_key(&key) {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate identity map key '{key}' \
                             (identity maps must have unique keys)"
                        )));
                    }
                    map.insert(key, value);
                }
                Ok(UniqueBTreeMap(map))
            }
        }

        deserializer.deserialize_map(UniqueMapVisitor(std::marker::PhantomData))
    }
}

// Absent optional IR fields have exactly one canonical spelling: the key is
// omitted — and a flag whose default is dropped on serialization is spelled
// only by its non-default value. The deserializers below reject the
// non-canonical spellings (`null`, `{}`, `[]`, and a flag written as the value
// that is skipped) fail-closed, so an implementation that canonicalizes the raw
// JSON directly (parse -> JCS -> BLAKE3) can never include a key this typed
// layer would have dropped: one document either hashes identically everywhere
// or is rejected everywhere. This mirrors the execution contract's
// `present_not_null` / `present_non_empty_unique_map` /
// `present_non_empty_string_list`, generalized because the IR's value types are
// richer. Serialization is unchanged — only the accepted input set narrows.

/// The one message shape for every non-canonical spelling of absence, naming
/// the offending key: serde attaches no field context to a `deserialize_with`
/// error, so the per-field wrappers supply it.
fn non_canonical_absence(field: &str, spelling: &str) -> String {
    format!(
        "`{field}`: the canonical spelling of absence is an omitted key \
         (explicit {spelling} is non-canonical)"
    )
}

/// `Option` field omitted when `None`: explicit `null` fails closed. The
/// present value is deserialized through `visit_some`, so a malformed value
/// still reports its own error rather than this one.
fn reject_explicit_null<'de, D, T>(
    deserializer: D,
    field: &'static str,
) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct NotNullVisitor<T>(&'static str, std::marker::PhantomData<T>);

    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for NotNullVisitor<T> {
        type Value = Option<T>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "a present value for `{}`", self.0)
        }

        // `null` lands on `visit_none` for a self-describing format and on
        // `visit_unit` for a format that has already committed to a value.
        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Err(E::custom(non_canonical_absence(self.0, "null")))
        }

        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Err(E::custom(non_canonical_absence(self.0, "null")))
        }

        fn visit_some<D: serde::Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<Self::Value, D::Error> {
            T::deserialize(deserializer).map(Some)
        }
    }

    deserializer.deserialize_option(NotNullVisitor(field, std::marker::PhantomData))
}

/// Identity map omitted when empty: explicit `{}` fails closed. Duplicate keys
/// are already rejected by [`UniqueBTreeMap`].
fn reject_explicit_empty_map<'de, D, K, V>(
    deserializer: D,
    field: &'static str,
) -> Result<UniqueBTreeMap<K, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    K: Deserialize<'de> + Ord + fmt::Display,
    V: Deserialize<'de>,
{
    let map = UniqueBTreeMap::<K, V>::deserialize(deserializer)?;
    if map.is_empty() {
        return Err(serde::de::Error::custom(non_canonical_absence(field, "{}")));
    }
    Ok(map)
}

/// Identity list omitted when empty: explicit `[]` fails closed.
fn reject_explicit_empty_list<'de, D, T>(
    deserializer: D,
    field: &'static str,
) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    let list = Vec::<T>::deserialize(deserializer)?;
    if list.is_empty() {
        return Err(serde::de::Error::custom(non_canonical_absence(field, "[]")));
    }
    Ok(list)
}

/// Flag omitted when it equals `skipped`: spelling that value explicitly fails
/// closed, so the key is present only when it carries the other value.
fn reject_skipped_flag<'de, D>(
    deserializer: D,
    field: &'static str,
    skipped: bool,
) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = bool::deserialize(deserializer)?;
    if value == skipped {
        return Err(serde::de::Error::custom(non_canonical_absence(
            field,
            if skipped { "true" } else { "false" },
        )));
    }
    Ok(value)
}

/// One `deserialize_with` wrapper per field NAME per kind, so the rejection
/// names the offending key. A field added to the IR without a name listed here
/// fails to compile at its `deserialize_with` path.
macro_rules! absent_key_deserializers {
    (
        present_not_null { $($null_field:ident),* $(,)? }
        present_non_empty_map { $($map_field:ident),* $(,)? }
        present_non_empty_list { $($list_field:ident),* $(,)? }
        present_true { $($true_field:ident),* $(,)? }
        present_false { $($false_field:ident),* $(,)? }
    ) => {
        mod present_not_null {
            $(
                pub(super) fn $null_field<'de, D, T>(
                    deserializer: D,
                ) -> Result<Option<T>, D::Error>
                where
                    D: serde::Deserializer<'de>,
                    T: serde::Deserialize<'de>,
                {
                    super::reject_explicit_null(deserializer, stringify!($null_field))
                }
            )*
        }

        mod present_non_empty_map {
            $(
                pub(super) fn $map_field<'de, D, K, V>(
                    deserializer: D,
                ) -> Result<super::UniqueBTreeMap<K, V>, D::Error>
                where
                    D: serde::Deserializer<'de>,
                    K: serde::Deserialize<'de> + Ord + std::fmt::Display,
                    V: serde::Deserialize<'de>,
                {
                    super::reject_explicit_empty_map(deserializer, stringify!($map_field))
                }
            )*
        }

        mod present_non_empty_list {
            $(
                pub(super) fn $list_field<'de, D, T>(
                    deserializer: D,
                ) -> Result<Vec<T>, D::Error>
                where
                    D: serde::Deserializer<'de>,
                    T: serde::Deserialize<'de>,
                {
                    super::reject_explicit_empty_list(deserializer, stringify!($list_field))
                }
            )*
        }

        mod present_true {
            $(
                pub(super) fn $true_field<'de, D>(deserializer: D) -> Result<bool, D::Error>
                where
                    D: serde::Deserializer<'de>,
                {
                    super::reject_skipped_flag(deserializer, stringify!($true_field), false)
                }
            )*
        }

        mod present_false {
            $(
                pub(super) fn $false_field<'de, D>(deserializer: D) -> Result<bool, D::Error>
                where
                    D: serde::Deserializer<'de>,
                {
                    super::reject_skipped_flag(deserializer, stringify!($false_field), true)
                }
            )*
        }
    };
}

absent_key_deserializers! {
    present_not_null {
        alias, allow_network, attach, boot_until, build, build_command,
        bytes, capabilities, class, component, content_ready_path, context,
        context_length, contract, cwd, database, default, default_target,
        degraded, delivery, dependencies, description, digest, disk, driver,
        enabled, engine, engine_variant, engine_version, entrypoint, env, exec,
        execution, expect_status, exports, foundation_requirements, fs_writes,
        generator, gid, health_check, http_get, image, ingress, initial_delay_seconds,
        inputs, install_command, interval_seconds, isolation, kill, label,
        language, level, lifecycle, locality, max_restore_seconds, mode,
        model, model_repo, model_repo_sha256, model_revision, model_sha256,
        model_url, mount, network, owner, ownership, pack, package, package_type,
        placeholder, polymorphism, port, prepare, prestart_command, producer,
        profile, profiles, provider, provision, publish, quantization,
        readiness_probe, reproducibility, requirements, run_command, runner_class,
        runtime, runtime_version, schema_id, scope, seal_at, secrets_required,
        service_target, sharing, shell_kind, side_effects, signals, size_bytes, size_mb,
        snapshot, source, source_digest, source_layout, stable_interval_ms,
        stable_successes, startup_timeout, state, stop, storage, store,
        surface, target, targets, tcp_connect, timeout, timeout_seconds,
        toolchain, transparency, upstream_path_prefix, use_thin, user,
        verify, version, vram_min, vram_recommended, working_dir, world,
    }
    present_non_empty_map {
        binaries, bind_env, bindings, cli, config, contracts, credentials,
        dependencies, env, env_inject, external, external_injection, generated_bindings,
        identity_exports, injection_bindings, parameters, paths, platforms,
        routes, runtime_exports, runtime_tools, secrets, services, state,
        targets, tool_dependencies,
    }
    present_non_empty_list {
        aliases, allow_env, allow_from, allowed_binaries, args, artifacts,
        build_env, choices, cmd, command, config_schema, dependencies, depends_on,
        egress_allow, egress_id_allow, engines, env_allowlist, exclude,
        exclude_libs, expose, external_dependencies, host_capabilities,
        implements, include, lockfiles, model_repo_include, needs, outputs,
        package_dependencies, platform, preference, providers, public,
        required_env, runtimes, secrets, server_args, state_bindings, targets,
        tool_artifacts, volumes, warmup_paths,
    }
    present_true {
        allow_emulation, artifacts, chat, dev_mode, encrypted, function_calling,
        gpu, index, listed, model_repo_gated, provenance, publish, read_only,
        recursive, required, root, run_once, secret, use_thin_provisioning,
        vision,
    }
    present_false {
        egress_proxy, required, sanitize_after_restore, strip_prefix,
    }
}

/// The normalized authored manifest intent (ADR-014 §2). Top-level coverage
/// is the complete §2.1 classification: the 32 identity-bearing sections are
/// explicit fields (`seal_at` postdates the ADR's table and is classified the
/// same as `snapshot` — authored seal/restore lifecycle intent, see
/// [`NormalizedSealAtIntent`]); the 9 non-identity sections (`schema_version`, `name`,
/// `version`, `metadata`, `distribution`, `state_owner_scope`,
/// `service_binding_scope`, `routing`, `pool`) have NO field here (they live
/// on the envelope's provenance where applicable); `workspace` is unsupported
/// and fails adapter-time (no field).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramManifestIntentV1 {
    /// Must equal [`CAPSULE_PROGRAM_MANIFEST_INTENT_V1_SCHEMA`].
    pub schema: String,
    pub capsule_type: ProgramIdentifier,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::default_target"
    )]
    pub default_target: Option<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::requirements"
    )]
    pub requirements: Option<NormalizedRequirementsIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::capabilities"
    )]
    pub capabilities: Option<NormalizedCapabilitiesIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::execution"
    )]
    pub execution: Option<NormalizedExecutionIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::storage"
    )]
    pub storage: Option<NormalizedStorageIntent>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::state"
    )]
    pub state: UniqueBTreeMap<ProgramIdentifier, NormalizedStateIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::network"
    )]
    pub network: Option<NormalizedNetworkIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::model"
    )]
    pub model: Option<NormalizedModelIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::transparency"
    )]
    pub transparency: Option<NormalizedTransparencyIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::build"
    )]
    pub build: Option<NormalizedBuildIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::pack"
    )]
    pub pack: Option<NormalizedPackIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::isolation"
    )]
    pub isolation: Option<NormalizedIsolationIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::polymorphism"
    )]
    pub polymorphism: Option<NormalizedPolymorphismIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::targets"
    )]
    pub targets: Option<NormalizedTargetsIntent>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::platforms"
    )]
    pub platforms: UniqueBTreeMap<ProgramIdentifier, NormalizedPlatformArtifactIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::exports"
    )]
    pub exports: Option<NormalizedExportsIntent>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::services"
    )]
    pub services: UniqueBTreeMap<ProgramIdentifier, NormalizedServiceIntent>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::dependencies"
    )]
    pub dependencies: UniqueBTreeMap<ProgramIdentifier, NormalizedDependencyIntent>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::tool_dependencies"
    )]
    pub tool_dependencies: UniqueBTreeMap<ProgramIdentifier, NormalizedToolDependencyIntent>,
    /// Set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::required_env"
    )]
    pub required_env: Vec<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::contracts"
    )]
    pub contracts: UniqueBTreeMap<ProgramIdentifier, NormalizedContractIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::foundation_requirements"
    )]
    pub foundation_requirements: Option<NormalizedFoundationRequirementsIntent>,
    /// Sorted by `name`.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::host_capabilities"
    )]
    pub host_capabilities: Vec<NormalizedHostCapabilityIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::ingress"
    )]
    pub ingress: Option<NormalizedIngressIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::snapshot"
    )]
    pub snapshot: Option<NormalizedSnapshotIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::seal_at"
    )]
    pub seal_at: Option<NormalizedSealAtIntent>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::secrets"
    )]
    pub secrets: UniqueBTreeMap<ProgramIdentifier, NormalizedSecretIntent>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::bindings"
    )]
    pub bindings: UniqueBTreeMap<ProgramIdentifier, NormalizedBindingIntent>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::external"
    )]
    pub external: UniqueBTreeMap<ProgramIdentifier, NormalizedExternalIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::context"
    )]
    pub context: Option<NormalizedContextIntent>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::generated_bindings"
    )]
    pub generated_bindings: UniqueBTreeMap<ProgramIdentifier, NormalizedGeneratedBindingIntent>,
}

/// `[requirements]` — system requirements (declaration semantics, §0).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedRequirementsIntent {
    /// Set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::platform"
    )]
    pub platform: Vec<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::vram_min"
    )]
    pub vram_min: Option<OpaqueAuthoredString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::vram_recommended"
    )]
    pub vram_recommended: Option<OpaqueAuthoredString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::disk"
    )]
    pub disk: Option<OpaqueAuthoredString>,
    /// Set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::dependencies"
    )]
    pub dependencies: Vec<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::capabilities"
    )]
    pub capabilities: Option<NormalizedSecurityCapabilitiesIntent>,
}

/// `[requirements.capabilities]` — declared security posture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedSecurityCapabilitiesIntent {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::network"
    )]
    pub network: Option<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::fs_writes"
    )]
    pub fs_writes: Option<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::side_effects"
    )]
    pub side_effects: Option<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::secrets_required"
    )]
    pub secrets_required: Option<bool>,
}

/// `[capabilities]` — inference model capabilities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedCapabilitiesIntent {
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::chat"
    )]
    pub chat: bool,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::function_calling"
    )]
    pub function_calling: bool,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::vision"
    )]
    pub vision: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::context_length"
    )]
    pub context_length: Option<u32>,
}

/// `execution` — consumed only as the existing normalizer's canonical derived
/// output (§2.0.1; raw `[execution]` authoring is not part of the accepted
/// v0.3 surface).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedExecutionIntent {
    pub runtime: ProgramIdentifier,
    pub entrypoint: NormalizedExecutionEntrypointIntent,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::port"
    )]
    pub port: Option<u16>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::health_check"
    )]
    pub health_check: Option<HttpRequestTarget>,
    /// Omitted when equal to the normalizer default (60).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::startup_timeout"
    )]
    pub startup_timeout: Option<u32>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::env"
    )]
    pub env: UniqueBTreeMap<ProgramIdentifier, OpaqueAuthoredString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::signals"
    )]
    pub signals: Option<NormalizedSignalsIntent>,
}

/// `execution.entrypoint` is runtime-dependent (ADR-014 §2.2): OCI ⇒ a Docker
/// image ref; source/web/wasm ⇒ a source-relative (possibly future) path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum NormalizedExecutionEntrypointIntent {
    SourceRelative(SourceRelativeFuturePath),
    OciImage(RemoteArtifactRef),
}

/// `execution.signals` — omitted entirely when both are the defaults
/// (SIGTERM/SIGKILL).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedSignalsIntent {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::stop"
    )]
    pub stop: Option<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::kill"
    )]
    pub kill: Option<ProgramIdentifier>,
}

/// `[storage]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedStorageIntent {
    /// Sorted by `name` (a named set, not an ordered list).
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::volumes"
    )]
    pub volumes: Vec<NormalizedStorageVolumeIntent>,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::use_thin_provisioning"
    )]
    pub use_thin_provisioning: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedStorageVolumeIntent {
    pub name: ProgramIdentifier,
    pub mount_path: GuestPath,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::read_only"
    )]
    pub read_only: bool,
    /// Omitted when the manifest default (0 = engine default).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::size_bytes"
    )]
    pub size_bytes: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::use_thin"
    )]
    pub use_thin: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::encrypted"
    )]
    pub encrypted: bool,
}

/// `[state.<name>]` — `StateRequirement` has NO mount-path field (state mount
/// paths live on `services.*.state_bindings[].target` and
/// `contracts.*.state.mount`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedStateIntent {
    pub kind: ProgramIdentifier,
    pub durability: ProgramIdentifier,
    pub purpose: OpaqueAuthoredString,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::producer"
    )]
    pub producer: Option<ProgramIdentifier>,
    /// Omitted when the default (`auto`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::attach"
    )]
    pub attach: Option<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::schema_id"
    )]
    pub schema_id: Option<ProgramIdentifier>,
    /// Omitted when the default (`exclusive`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::sharing"
    )]
    pub sharing: Option<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::size_mb"
    )]
    pub size_mb: Option<u32>,
}

/// `[network]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedNetworkIntent {
    /// The authored network posture, present only when it differs from
    /// `types::NETWORK_ENABLED_WHEN_UNDECLARED` — ADR-014 §2.3, "absent ≡
    /// explicit default (one canonical spelling: omitted)". Capsules that do
    /// not author a posture, and capsules that author the current default,
    /// hash exactly as they did before ato#786 added this field.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::enabled"
    )]
    pub enabled: Option<bool>,
    /// Set-like: sorted + deduplicated (ADR-014 §2.3 names this list).
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::egress_allow"
    )]
    pub egress_allow: Vec<RemoteArtifactRef>,
    /// Set-like: sorted by `(rule_type, value)` + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::egress_id_allow"
    )]
    pub egress_id_allow: Vec<NormalizedEgressIdRuleIntent>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedEgressIdRuleIntent {
    pub rule_type: ProgramIdentifier,
    pub value: OpaqueAuthoredString,
}

/// `[model]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedModelIntent {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::source"
    )]
    pub source: Option<RemoteArtifactRef>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::quantization"
    )]
    pub quantization: Option<ProgramIdentifier>,
}

/// `[transparency]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedTransparencyIntent {
    /// Omitted when the default (`loose`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::level"
    )]
    pub level: Option<ProgramIdentifier>,
    /// Allowlist (any-match): sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::allowed_binaries"
    )]
    pub allowed_binaries: Vec<GlobPattern>,
}

/// `[build]` — `build.outputs.*` and `build.policy.*` are Rule-2 exclusions
/// and have NO fields here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedBuildIntent {
    /// Exclusion patterns: authored order preserved (same discipline as
    /// `pack.exclude`).
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::exclude_libs"
    )]
    pub exclude_libs: Vec<GlobPattern>,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::gpu"
    )]
    pub gpu: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::lifecycle"
    )]
    pub lifecycle: Option<NormalizedBuildLifecycleIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::inputs"
    )]
    pub inputs: Option<NormalizedBuildInputsIntent>,
}

/// `[build.lifecycle]` — an ORDER-SENSITIVE pipeline; fields stay positional.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedBuildLifecycleIntent {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::prepare"
    )]
    pub prepare: Option<OpaqueCommand>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::build"
    )]
    pub build: Option<OpaqueCommand>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::package"
    )]
    pub package: Option<OpaqueCommand>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::verify"
    )]
    pub verify: Option<OpaqueCommand>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::publish"
    )]
    pub publish: Option<OpaqueCommand>,
}

/// `[build.inputs]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedBuildInputsIntent {
    /// Existence-checked by the adapter; set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::lockfiles"
    )]
    pub lockfiles: Vec<SourceExistingPath>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::toolchain"
    )]
    pub toolchain: Option<OpaqueAuthoredString>,
    /// Lexical-only; set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::artifacts"
    )]
    pub artifacts: Vec<SourceRelativeFuturePath>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::allow_network"
    )]
    pub allow_network: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::reproducibility"
    )]
    pub reproducibility: Option<OpaqueAuthoredString>,
}

/// `[pack]` — include/exclude are ORDER-SENSITIVE (ADR-014 §2.3): preserved
/// as authored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedPackIntent {
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::include"
    )]
    pub include: Vec<GlobPattern>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::exclude"
    )]
    pub exclude: Vec<GlobPattern>,
}

/// `[isolation]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedIsolationIntent {
    /// Env NAMES; set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::allow_env"
    )]
    pub allow_env: Vec<ProgramIdentifier>,
}

/// `[polymorphism]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedPolymorphismIntent {
    /// Set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::implements"
    )]
    pub implements: Vec<ProgramIdentifier>,
}

/// `[targets]` — `TargetsConfig`'s own global fields plus every target in ONE
/// canonical map. Structured `targets.wasm`/`targets.source`/`targets.oci`
/// are canonicalized by the adapter into [`NormalizedTargetIntent`] entries
/// under the reserved names `"wasm"`/`"source"`/`"oci"`; a named target that
/// collides with a reserved label used by a structured target is an
/// adapter-time error, so the single map can never be ambiguous.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedTargetsIntent {
    /// ORDER-SENSITIVE resolution preference: preserved as authored.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::preference"
    )]
    pub preference: Vec<ProgramIdentifier>,
    /// Authored `sha256:`-prefixed only ([`Sha256DigestPin::parse_prefixed`]).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::source_digest"
    )]
    pub source_digest: Option<Sha256DigestPin>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::port"
    )]
    pub port: Option<u16>,
    /// Omitted when equal to the normalizer default (60).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::startup_timeout"
    )]
    pub startup_timeout: Option<u32>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::env"
    )]
    pub env: UniqueBTreeMap<ProgramIdentifier, OpaqueAuthoredString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::health_check"
    )]
    pub health_check: Option<HttpRequestTarget>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::targets"
    )]
    pub targets: UniqueBTreeMap<ProgramIdentifier, NormalizedTargetIntent>,
}

/// One normalized target (named or canonicalized-structured). Rule-3
/// unsupported fields (`engine_path`, a Wasm `working_dir`, an absolute or
/// out-of-tree `model`) have NO representation — the adapter fails closed;
/// Rule-2 exclusions (`model_filename`, `model_format`) have no field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedTargetIntent {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::runtime"
    )]
    pub runtime: Option<OpaqueAuthoredString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::surface"
    )]
    pub surface: Option<NormalizedSurfaceIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::driver"
    )]
    pub driver: Option<OpaqueAuthoredString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::language"
    )]
    pub language: Option<OpaqueAuthoredString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::runtime_version"
    )]
    pub runtime_version: Option<OpaqueAuthoredString>,
    /// The structured source target's version CONSTRAINT (`"^3.11"`), kept
    /// distinct from the pinned `runtime_version`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::version"
    )]
    pub version: Option<OpaqueAuthoredString>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::runtime_tools"
    )]
    pub runtime_tools: UniqueBTreeMap<ProgramIdentifier, OpaqueAuthoredString>,
    /// Set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::tool_artifacts"
    )]
    pub tool_artifacts: Vec<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::entrypoint"
    )]
    pub entrypoint: Option<SourceRelativeFuturePath>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::component"
    )]
    pub component: Option<SourceRelativeFuturePath>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::image"
    )]
    pub image: Option<RemoteArtifactRef>,
    /// `targets.wasm.digest` / `targets.oci.digest`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::digest"
    )]
    pub digest: Option<CasContentDigest>,
    /// `targets.wasm.world` — default-expanded to `wasi:cli/command` by the
    /// adapter before hashing when authored absent on a Wasm target.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::world"
    )]
    pub world: Option<WitWorldRef>,
    /// `targets.wasm.config` — component config values.
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::config"
    )]
    pub config: UniqueBTreeMap<ProgramIdentifier, OpaqueAuthoredString>,
    /// Argv: ORDER-SENSITIVE, preserved as authored.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::cmd"
    )]
    pub cmd: Vec<OpaqueCommand>,
    /// `targets.source.args` — ORDER-SENSITIVE, preserved as authored.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::args"
    )]
    pub args: Vec<OpaqueCommand>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::env"
    )]
    pub env: UniqueBTreeMap<ProgramIdentifier, OpaqueAuthoredString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::user"
    )]
    pub user: Option<ContainerUserSpec>,
    /// Runtime-class-resolved: source/web ⇒ source-relative; OCI ⇒ absolute
    /// guest path; Wasm ⇒ rejected at adapter time (Rule 3).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::working_dir"
    )]
    pub working_dir: Option<NormalizedWorkingDir>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::port"
    )]
    pub port: Option<u16>,
    /// `targets.source.dependencies` — the declared dependencies file
    /// (requirements.txt / package.json); existence-checked by the adapter.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::dependencies"
    )]
    pub dependencies: Option<SourceExistingPath>,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::dev_mode"
    )]
    pub dev_mode: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::engine"
    )]
    pub engine: Option<RemoteArtifactRef>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::engine_version"
    )]
    pub engine_version: Option<OpaqueAuthoredString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::engine_variant"
    )]
    pub engine_variant: Option<OpaqueAuthoredString>,
    /// In-tree only; absolute/out-of-tree rejected at adapter time (Rule 3).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::model"
    )]
    pub model: Option<SourceExistingPath>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::model_url"
    )]
    pub model_url: Option<RemoteArtifactRef>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::model_sha256"
    )]
    pub model_sha256: Option<Sha256DigestPin>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::model_repo"
    )]
    pub model_repo: Option<RemoteArtifactRef>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::model_revision"
    )]
    pub model_revision: Option<GitCommitRevision>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::model_repo_sha256"
    )]
    pub model_repo_sha256: Option<Sha256DigestPin>,
    /// Allowlist (any-match): sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::model_repo_include"
    )]
    pub model_repo_include: Vec<GlobPattern>,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::model_repo_gated"
    )]
    pub model_repo_gated: bool,
    /// Extra argv: ORDER-SENSITIVE, preserved as authored.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::server_args"
    )]
    pub server_args: Vec<OpaqueCommand>,
    /// Set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::required_env"
    )]
    pub required_env: Vec<ProgramIdentifier>,
    /// Form fields: ORDER-SENSITIVE (display order), preserved as authored.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::config_schema"
    )]
    pub config_schema: Vec<NormalizedConfigFieldIntent>,
    /// Set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::env_allowlist"
    )]
    pub env_allowlist: Vec<ProgramIdentifier>,
    /// Allowlist (any-match): sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::public"
    )]
    pub public: Vec<GlobPattern>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::source_layout"
    )]
    pub source_layout: Option<OpaqueAuthoredString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::package_type"
    )]
    pub package_type: Option<OpaqueAuthoredString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::build_command"
    )]
    pub build_command: Option<OpaqueCommand>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::install_command"
    )]
    pub install_command: Option<NormalizedCommandIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::prestart_command"
    )]
    pub prestart_command: Option<NormalizedCommandIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::run_command"
    )]
    pub run_command: Option<OpaqueCommand>,
    /// Set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::outputs"
    )]
    pub outputs: Vec<SourceRelativeFuturePath>,
    /// Env NAMES; set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::build_env"
    )]
    pub build_env: Vec<ProgramIdentifier>,
    /// Set-like: sorted + deduplicated (startup order derives from the graph,
    /// never from list position).
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::needs"
    )]
    pub needs: Vec<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::readiness_probe"
    )]
    pub readiness_probe: Option<NormalizedReadinessProbeIntent>,
    /// Set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::package_dependencies"
    )]
    pub package_dependencies: Vec<ProgramIdentifier>,
    /// Sorted by `alias`.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::external_dependencies"
    )]
    pub external_dependencies: Vec<NormalizedExternalDependencyIntent>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::external_injection"
    )]
    pub external_injection: UniqueBTreeMap<ProgramIdentifier, NormalizedExternalInjectionIntent>,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::allow_emulation"
    )]
    pub allow_emulation: bool,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::run_once"
    )]
    pub run_once: bool,
}

/// `targets.<label>.working_dir`, resolved by target runtime class (ADR-014
/// §2.2): source/web ⇒ [`SourceRelativePath`] (Root allowed); OCI ⇒ an
/// absolute in-container [`GuestPath`] (`"/app"`). A Wasm `working_dir` is a
/// Rule-3 adapter-time rejection and never reaches this type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum NormalizedWorkingDir {
    SourceRelative(SourceRelativePath),
    Guest(GuestPath),
}

/// `targets.<label>.surface`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedSurfaceIntent {
    pub kind: ProgramIdentifier,
    /// `None` and `Some([])` stay distinct, mirroring the authoring type.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::profiles"
    )]
    pub profiles: Option<Vec<ProgramIdentifier>>,
}

/// One `config_schema` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedConfigFieldIntent {
    pub name: ProgramIdentifier,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::label"
    )]
    pub label: Option<OpaqueAuthoredString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::description"
    )]
    pub description: Option<OpaqueAuthoredString>,
    pub kind: ProgramIdentifier,
    /// Enum-kind choices: ORDER-SENSITIVE (display order), preserved.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::choices"
    )]
    pub choices: Vec<OpaqueAuthoredString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::default"
    )]
    pub default: Option<OpaqueAuthoredString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::placeholder"
    )]
    pub placeholder: Option<OpaqueAuthoredString>,
}

/// A normalized `CommandSpec` (install/prestart lifecycle hooks): the three
/// authored forms stay distinct — they carry different execution semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum NormalizedCommandIntent {
    /// Explicit shell script.
    Shell {
        shell: OpaqueCommand,
        /// Omitted when the default (`posix-sh`).
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present_not_null::shell_kind"
        )]
        shell_kind: Option<ProgramIdentifier>,
    },
    /// Explicit argv command.
    Argv {
        cmd: OpaqueCommand,
        #[serde(
            default,
            skip_serializing_if = "Vec::is_empty",
            deserialize_with = "present_non_empty_list::args"
        )]
        args: Vec<OpaqueCommand>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present_not_null::cwd"
        )]
        cwd: Option<NormalizedWorkingDir>,
        #[serde(
            default,
            skip_serializing_if = "UniqueBTreeMap::is_empty",
            deserialize_with = "present_non_empty_map::env"
        )]
        env: UniqueBTreeMap<ProgramIdentifier, OpaqueAuthoredString>,
    },
    /// Backward-compatible string form (auto-detected at execution time).
    Raw(OpaqueCommand),
}

/// Target-level `readiness_probe` (a DIFFERENT type from the
/// dependency-grammar `ReadyProbe`; the two are never conflated). Timing
/// fields are authored intent (u32) and identity-bearing; each is omitted
/// when equal to its normalizer default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedReadinessProbeIntent {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::http_get"
    )]
    pub http_get: Option<HttpRequestTarget>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::tcp_connect"
    )]
    pub tcp_connect: Option<TcpProbeTarget>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::exec"
    )]
    pub exec: Option<Vec<OpaqueCommand>>,
    /// A placeholder NAME (`"PORT"`, `"web"`), never a host:port target.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::port"
    )]
    pub port: Option<ProbePortReference>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::initial_delay_seconds"
    )]
    pub initial_delay_seconds: Option<u32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::timeout_seconds"
    )]
    pub timeout_seconds: Option<u32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::interval_seconds"
    )]
    pub interval_seconds: Option<u32>,
}

/// One `external_dependencies[]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedExternalDependencyIntent {
    pub alias: ProgramIdentifier,
    pub source: RemoteArtifactRef,
    pub source_type: ProgramIdentifier,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::contract"
    )]
    pub contract: Option<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::injection_bindings"
    )]
    pub injection_bindings: UniqueBTreeMap<ProgramIdentifier, OpaqueAuthoredString>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::parameters"
    )]
    pub parameters: UniqueBTreeMap<ProgramIdentifier, NormalizedParamValueIntent>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::credentials"
    )]
    pub credentials: UniqueBTreeMap<ProgramIdentifier, TemplatedString>,
}

/// A typed parameter value (mirrors the dependency grammar's `ParamValue`;
/// JSON string/int/bool are mutually unambiguous under `untagged`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NormalizedParamValueIntent {
    String(OpaqueAuthoredString),
    Int(i64),
    Bool(bool),
}

/// One `external_injection.<name>` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedExternalInjectionIntent {
    pub injection_type: ProgramIdentifier,
    /// Omitted when the default (`true`).
    #[serde(
        default = "default_true",
        skip_serializing_if = "is_true",
        deserialize_with = "present_false::required"
    )]
    pub required: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::default"
    )]
    pub default: Option<OpaqueAuthoredString>,
}

/// `[platforms.<os>-<arch>]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedPlatformArtifactIntent {
    pub artifact: RemoteArtifactRef,
    /// Authoring: bare SHA-256 as the existing validator accepts it
    /// ([`Sha256DigestPin::parse_flexible`]).
    pub sha256: Sha256DigestPin,
}

/// `[exports]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedExportsIntent {
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::cli"
    )]
    pub cli: UniqueBTreeMap<ProgramIdentifier, NormalizedCliExportIntent>,
    /// Alias → path relative to the materialized tool root.
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::binaries"
    )]
    pub binaries: UniqueBTreeMap<ProgramIdentifier, SourceRelativeFuturePath>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::paths"
    )]
    pub paths: UniqueBTreeMap<ProgramIdentifier, SourceRelativeFuturePath>,
}

/// `exports.cli.<name>` — `description` is a Rule-2 exclusion (display-only)
/// and has NO field here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedCliExportIntent {
    pub kind: ProgramIdentifier,
    pub target: ProgramIdentifier,
    /// Argv: ORDER-SENSITIVE, preserved as authored.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::args"
    )]
    pub args: Vec<OpaqueCommand>,
}

/// `[services.<name>]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedServiceIntent {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::entrypoint"
    )]
    pub entrypoint: Option<OpaqueCommand>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::target"
    )]
    pub target: Option<ProgramIdentifier>,
    /// Set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::depends_on"
    )]
    pub depends_on: Vec<ProgramIdentifier>,
    /// Set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::expose"
    )]
    pub expose: Vec<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::env"
    )]
    pub env: UniqueBTreeMap<ProgramIdentifier, OpaqueAuthoredString>,
    /// Set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::secrets"
    )]
    pub secrets: Vec<ProgramIdentifier>,
    /// Sorted by `(state, target)`.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::state_bindings"
    )]
    pub state_bindings: Vec<NormalizedServiceStateBindingIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::readiness_probe"
    )]
    pub readiness_probe: Option<NormalizedReadinessProbeIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::network"
    )]
    pub network: Option<NormalizedServiceNetworkIntent>,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::run_once"
    )]
    pub run_once: bool,
}

/// One `services.*.state_bindings[]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedServiceStateBindingIntent {
    pub state: ProgramIdentifier,
    pub target: GuestPath,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::service_target"
    )]
    pub service_target: Option<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::owner"
    )]
    pub owner: Option<NormalizedStateOwnerIntent>,
    /// Octal permission string (`"0700"`), hashed as authored.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::mode"
    )]
    pub mode: Option<OpaqueAuthoredString>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedStateOwnerIntent {
    pub uid: u32,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::gid"
    )]
    pub gid: Option<u32>,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::recursive"
    )]
    pub recursive: bool,
}

/// `services.*.network`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedServiceNetworkIntent {
    /// Set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::aliases"
    )]
    pub aliases: Vec<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::publish"
    )]
    pub publish: bool,
    /// Set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::allow_from"
    )]
    pub allow_from: Vec<ProgramIdentifier>,
    /// Omitted when the default (`true`).
    #[serde(
        default = "default_true",
        skip_serializing_if = "is_true",
        deserialize_with = "present_false::egress_proxy"
    )]
    pub egress_proxy: bool,
}

/// `[dependencies.<alias>]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedDependencyIntent {
    pub capsule: RemoteArtifactRef,
    /// `<name>@<major>` contract reference spelling.
    pub contract: ProgramIdentifier,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::parameters"
    )]
    pub parameters: UniqueBTreeMap<ProgramIdentifier, NormalizedParamValueIntent>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::credentials"
    )]
    pub credentials: UniqueBTreeMap<ProgramIdentifier, TemplatedString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::state"
    )]
    pub state: Option<NormalizedDependencyStateIntent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedDependencyStateIntent {
    pub name: ProgramIdentifier,
    /// Omitted when the default (`parent`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::ownership"
    )]
    pub ownership: Option<ProgramIdentifier>,
}

/// `[tool_dependencies.<alias>]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedToolDependencyIntent {
    /// The authored `ref` capsule URL.
    pub capsule_ref: RemoteArtifactRef,
    /// Version CONSTRAINT (`">=16,<17"`) — the lock records the resolution.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::version"
    )]
    pub version: Option<OpaqueAuthoredString>,
    /// Export name → env var name.
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::bind_env"
    )]
    pub bind_env: UniqueBTreeMap<ProgramIdentifier, ProgramIdentifier>,
}

/// `[contracts.<name>]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedContractIntent {
    pub target: ProgramIdentifier,
    pub ready: NormalizedReadyProbeIntent,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::parameters"
    )]
    pub parameters: UniqueBTreeMap<ProgramIdentifier, NormalizedValueSchemaIntent>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::credentials"
    )]
    pub credentials: UniqueBTreeMap<ProgramIdentifier, NormalizedValueSchemaIntent>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::identity_exports"
    )]
    pub identity_exports: UniqueBTreeMap<ProgramIdentifier, TemplatedString>,
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::runtime_exports"
    )]
    pub runtime_exports: UniqueBTreeMap<ProgramIdentifier, NormalizedRuntimeExportIntent>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::state"
    )]
    pub state: Option<NormalizedContractStateIntent>,
}

/// The dependency-grammar `ReadyProbe`, normalized per variant with
/// [`TemplatedString`] leaves (`{{…}}` template syntax validated by the
/// existing grammar, hashed as authored) and opaque timeout strings. A
/// DIFFERENT type from the target-level [`NormalizedReadinessProbeIntent`];
/// the two are never conflated. Externally tagged so every variant's field
/// set is fail-closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum NormalizedReadyProbeIntent {
    Tcp {
        target: TemplatedString,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present_not_null::timeout"
        )]
        timeout: Option<OpaqueAuthoredString>,
    },
    Probe {
        run: TemplatedString,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present_not_null::timeout"
        )]
        timeout: Option<OpaqueAuthoredString>,
    },
    Postgres {
        host: TemplatedString,
        port: TemplatedString,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present_not_null::user"
        )]
        user: Option<TemplatedString>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present_not_null::database"
        )]
        database: Option<TemplatedString>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present_not_null::timeout"
        )]
        timeout: Option<OpaqueAuthoredString>,
    },
    Http {
        url: TemplatedString,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present_not_null::expect_status"
        )]
        expect_status: Option<u16>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present_not_null::timeout"
        )]
        timeout: Option<OpaqueAuthoredString>,
    },
    UnixSocket {
        path: TemplatedString,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present_not_null::timeout"
        )]
        timeout: Option<OpaqueAuthoredString>,
    },
}

/// `contracts.*.parameters.<name>` / `contracts.*.credentials.<name>` (the
/// two authoring schemas are structurally identical).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedValueSchemaIntent {
    pub value_type: ProgramIdentifier,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::required"
    )]
    pub required: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::default"
    )]
    pub default: Option<NormalizedParamValueIntent>,
}

/// `contracts.*.runtime_exports.<name>` — the authored shorthand and detailed
/// spellings canonicalize into this ONE shape (shorthand ⇒ `secret: false`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedRuntimeExportIntent {
    pub value: TemplatedString,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::secret"
    )]
    pub secret: bool,
}

/// `contracts.*.state`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedContractStateIntent {
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::required"
    )]
    pub required: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::version"
    )]
    pub version: Option<OpaqueAuthoredString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::mount"
    )]
    pub mount: Option<GuestPath>,
}

/// `[foundation_requirements]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedFoundationRequirementsIntent {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::profile"
    )]
    pub profile: Option<ProgramIdentifier>,
    /// `name@version-range` constraints; set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::runtimes"
    )]
    pub runtimes: Vec<OpaqueAuthoredString>,
    /// Set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::engines"
    )]
    pub engines: Vec<OpaqueAuthoredString>,
}

/// One `[[host_capabilities]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedHostCapabilityIntent {
    pub name: ProgramIdentifier,
    pub reason: OpaqueAuthoredString,
}

/// `[ingress]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedIngressIntent {
    pub mode: ProgramIdentifier,
    /// Route NAMES key this map (identifier spellings — the alias/name feed
    /// the path prefix, but their authored spelling is a name, not an
    /// origin-form target). `upstream_path_prefix` IS an origin-form
    /// [`HttpRequestTarget`].
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::routes"
    )]
    pub routes: UniqueBTreeMap<ProgramIdentifier, NormalizedIngressRouteIntent>,
    /// Service → env name → authored template value.
    #[serde(
        default,
        skip_serializing_if = "UniqueBTreeMap::is_empty",
        deserialize_with = "present_non_empty_map::env_inject"
    )]
    pub env_inject:
        UniqueBTreeMap<ProgramIdentifier, UniqueBTreeMap<ProgramIdentifier, OpaqueAuthoredString>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedIngressRouteIntent {
    pub target: ProgramIdentifier,
    pub port: u16,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::listed"
    )]
    pub listed: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::alias"
    )]
    pub alias: Option<ProgramIdentifier>,
    /// Omitted when the default (`true`).
    #[serde(
        default = "default_true",
        skip_serializing_if = "is_true",
        deserialize_with = "present_false::strip_prefix"
    )]
    pub strip_prefix: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::upstream_path_prefix"
    )]
    pub upstream_path_prefix: Option<HttpRequestTarget>,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::root"
    )]
    pub root: bool,
}

/// `[snapshot]` — Ready-State sealing/restore intent (declaration semantics,
/// §0: revising it is a new declaration). Every field is omitted when equal
/// to its normalizer default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedSnapshotIntent {
    /// Omitted when the default (`none`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::mode"
    )]
    pub mode: Option<ProgramIdentifier>,
    /// Omitted when the default (`healthcheck`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::boot_until"
    )]
    pub boot_until: Option<ProgramIdentifier>,
    /// Omitted when the default (`true`).
    #[serde(
        default = "default_true",
        skip_serializing_if = "is_true",
        deserialize_with = "present_false::sanitize_after_restore"
    )]
    pub sanitize_after_restore: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::runner_class"
    )]
    pub runner_class: Option<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::max_restore_seconds"
    )]
    pub max_restore_seconds: Option<u32>,
    /// HTTP request-targets, not filesystem paths; ORDER-SENSITIVE (warmup
    /// sequence), preserved as authored.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::warmup_paths"
    )]
    pub warmup_paths: Vec<HttpRequestTarget>,
    /// Omitted when the default (1).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::stable_successes"
    )]
    pub stable_successes: Option<u32>,
    /// Omitted when the default (250).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::stable_interval_ms"
    )]
    pub stable_interval_ms: Option<u64>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::content_ready_path"
    )]
    pub content_ready_path: Option<HttpRequestTarget>,
}

/// `[seal_at]` — authored Snapshot acceptance verification intent (declaration
/// semantics, §0: revising it is a new declaration).
///
/// Same identity category as `snapshot` (§2.1): both are authored seal/restore
/// lifecycle intent, not a runtime observation. Per the §2.2 Rule 4 matrix the
/// argv is [`OpaqueCommand`] (an authored command with no path interpretation —
/// the same row `targets.<label>.cmd` and `readiness_probe.exec` sit on) and the
/// timeout is a plain scalar, so no string in this section is hashed
/// unclassified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedSealAtIntent {
    /// Argv: ORDER-SENSITIVE, preserved as authored (RFC §6.1 — argument
    /// boundaries are exact). Required: an absent or empty `command` is not a
    /// `seal_at` declaration, so `[]` fails closed rather than normalizing to
    /// the absent spelling.
    #[serde(deserialize_with = "present_non_empty_list::command")]
    pub command: Vec<OpaqueCommand>,
    /// Omitted when the author left the bound to the acceptance-loop default.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::timeout_seconds"
    )]
    pub timeout_seconds: Option<u32>,
}

/// `[secrets.<name>]` — a required secret as a ref, never a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedSecretIntent {
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::required"
    )]
    pub required: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::description"
    )]
    pub description: Option<OpaqueAuthoredString>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::env"
    )]
    pub env: Option<ProgramIdentifier>,
    /// Omitted when the default (`env`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::delivery"
    )]
    pub delivery: Option<ProgramIdentifier>,
    /// Omitted when the default (`api_key`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::class"
    )]
    pub class: Option<ProgramIdentifier>,
}

/// `[bindings.<name>]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedBindingIntent {
    pub kind: ProgramIdentifier,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::required"
    )]
    pub required: bool,
    /// Omitted when the default (`user`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::scope"
    )]
    pub scope: Option<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::mount"
    )]
    pub mount: Option<GuestPath>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::mode"
    )]
    pub mode: Option<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::provider"
    )]
    pub provider: Option<ProgramIdentifier>,
}

/// `[external.<name>]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedExternalIntent {
    pub kind: ProgramIdentifier,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::required"
    )]
    pub required: bool,
    /// PREFERENCE order: preserved as authored.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::providers"
    )]
    pub providers: Vec<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::provider"
    )]
    pub provider: Option<ProgramIdentifier>,
    /// Omitted when the default (`parallel`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::provision"
    )]
    pub provision: Option<ProgramIdentifier>,
    /// Omitted when the default (`local_preferred`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::locality"
    )]
    pub locality: Option<ProgramIdentifier>,
    /// Omitted when the default (`demo`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::degraded"
    )]
    pub degraded: Option<ProgramIdentifier>,
}

/// `[context]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedContextIntent {
    /// Omitted when the default (`app_private`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::store"
    )]
    pub store: Option<ProgramIdentifier>,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::artifacts"
    )]
    pub artifacts: bool,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::index"
    )]
    pub index: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::mount"
    )]
    pub mount: Option<GuestPath>,
    #[serde(
        default,
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "present_true::provenance"
    )]
    pub provenance: bool,
}

/// `[generated_bindings.<name>]` — only this SPEC is identity-bearing; the
/// generated value never is (its own doc comment anticipates this).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedGeneratedBindingIntent {
    /// Omitted when the default (`random_base64`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::generator"
    )]
    pub generator: Option<ProgramIdentifier>,
    /// Omitted when the default (32).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::bytes"
    )]
    pub bytes: Option<u32>,
    /// Omitted when the default (`run`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_not_null::scope"
    )]
    pub scope: Option<ProgramIdentifier>,
    /// Set-like: sorted + deduplicated.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "present_non_empty_list::targets"
    )]
    pub targets: Vec<ProgramIdentifier>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Canonical-order validation
// ─────────────────────────────────────────────────────────────────────────────

fn validate_sorted<T: Ord>(field: &'static str, values: &[T]) -> Result<(), CapsuleProgramError> {
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(CapsuleProgramError::NonCanonicalList(field));
    }
    Ok(())
}

fn validate_sorted_by<T>(
    field: &'static str,
    values: &[T],
    compare: impl Fn(&T, &T) -> std::cmp::Ordering,
) -> Result<(), CapsuleProgramError> {
    if values
        .windows(2)
        .any(|pair| compare(&pair[0], &pair[1]) != std::cmp::Ordering::Less)
    {
        return Err(CapsuleProgramError::NonCanonicalList(field));
    }
    Ok(())
}

impl ProgramManifestIntentV1 {
    /// Fail closed on any non-canonical spelling the type system alone cannot
    /// reject: a wrong schema string, or a set-like list that is not strictly
    /// increasing (unsorted or duplicated).
    pub fn validate(&self) -> Result<(), CapsuleProgramError> {
        if self.schema != CAPSULE_PROGRAM_MANIFEST_INTENT_V1_SCHEMA {
            return Err(invalid(
                "manifest_intent.schema",
                format!(
                    "must be '{CAPSULE_PROGRAM_MANIFEST_INTENT_V1_SCHEMA}', got '{}'",
                    self.schema
                ),
            ));
        }
        validate_sorted("required_env", &self.required_env)?;
        validate_sorted_by("host_capabilities", &self.host_capabilities, |a, b| {
            a.name.cmp(&b.name)
        })?;
        if let Some(requirements) = &self.requirements {
            validate_sorted("requirements.platform", &requirements.platform)?;
            validate_sorted("requirements.dependencies", &requirements.dependencies)?;
        }
        if let Some(storage) = &self.storage {
            validate_sorted_by("storage.volumes", &storage.volumes, |a, b| {
                a.name.cmp(&b.name)
            })?;
        }
        if let Some(network) = &self.network {
            validate_sorted("network.egress_allow", &network.egress_allow)?;
            validate_sorted("network.egress_id_allow", &network.egress_id_allow)?;
        }
        if let Some(transparency) = &self.transparency {
            validate_sorted(
                "transparency.allowed_binaries",
                &transparency.allowed_binaries,
            )?;
        }
        if let Some(inputs) = self.build.as_ref().and_then(|build| build.inputs.as_ref()) {
            validate_sorted("build.inputs.lockfiles", &inputs.lockfiles)?;
            validate_sorted("build.inputs.artifacts", &inputs.artifacts)?;
        }
        if let Some(isolation) = &self.isolation {
            validate_sorted("isolation.allow_env", &isolation.allow_env)?;
        }
        if let Some(polymorphism) = &self.polymorphism {
            validate_sorted("polymorphism.implements", &polymorphism.implements)?;
        }
        if let Some(targets) = &self.targets {
            for target in targets.targets.values() {
                target.validate()?;
            }
        }
        for service in self.services.values() {
            validate_sorted("services.*.depends_on", &service.depends_on)?;
            validate_sorted("services.*.expose", &service.expose)?;
            validate_sorted("services.*.secrets", &service.secrets)?;
            validate_sorted_by(
                "services.*.state_bindings",
                &service.state_bindings,
                |a, b| (&a.state, &a.target).cmp(&(&b.state, &b.target)),
            )?;
            if let Some(network) = &service.network {
                validate_sorted("services.*.network.aliases", &network.aliases)?;
                validate_sorted("services.*.network.allow_from", &network.allow_from)?;
            }
        }
        if let Some(foundation) = &self.foundation_requirements {
            validate_sorted("foundation_requirements.runtimes", &foundation.runtimes)?;
            validate_sorted("foundation_requirements.engines", &foundation.engines)?;
        }
        for binding in self.generated_bindings.values() {
            validate_sorted("generated_bindings.*.targets", &binding.targets)?;
        }
        Ok(())
    }
}

impl NormalizedTargetIntent {
    fn validate(&self) -> Result<(), CapsuleProgramError> {
        validate_sorted("targets.*.tool_artifacts", &self.tool_artifacts)?;
        validate_sorted("targets.*.model_repo_include", &self.model_repo_include)?;
        validate_sorted("targets.*.required_env", &self.required_env)?;
        validate_sorted("targets.*.env_allowlist", &self.env_allowlist)?;
        validate_sorted("targets.*.public", &self.public)?;
        validate_sorted("targets.*.outputs", &self.outputs)?;
        validate_sorted("targets.*.build_env", &self.build_env)?;
        validate_sorted("targets.*.needs", &self.needs)?;
        validate_sorted("targets.*.package_dependencies", &self.package_dependencies)?;
        validate_sorted_by(
            "targets.*.external_dependencies",
            &self.external_dependencies,
            |a, b| a.alias.cmp(&b.alias),
        )?;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Contract, envelope, parent link
// ─────────────────────────────────────────────────────────────────────────────

/// The identity-bearing Capsule Program declaration contract. Every field
/// participates in `capsule_program_id`; deserialization is fail-closed
/// (`deny_unknown_fields`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapsuleProgramContractV1 {
    /// Must equal [`CAPSULE_PROGRAM_V1_SCHEMA`].
    pub schema: String,
    pub source: ProgramSourceContract,
    pub manifest_intent: ProgramManifestIntentV1,
}

impl CapsuleProgramContractV1 {
    pub fn validate(&self) -> Result<(), CapsuleProgramError> {
        if self.schema != CAPSULE_PROGRAM_V1_SCHEMA {
            return Err(CapsuleProgramError::InvalidSchema);
        }
        self.manifest_intent.validate()
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CapsuleProgramError> {
        self.validate()?;
        serde_jcs::to_vec(self)
            .map_err(|error| CapsuleProgramError::Canonicalization(error.to_string()))
    }

    /// The frozen v1 rule shared with `execution_id`
    /// ([`schema_domained_blake3_id`]):
    ///
    /// ```text
    /// capsule_program_id =
    ///   "blake3:" + hex(BLAKE3(UTF8("ato.capsule-program/v1") || 0x00 || JCS(self)))
    /// ```
    pub fn compute_capsule_program_id(&self) -> Result<CapsuleProgramId, CapsuleProgramError> {
        let canonical = self.canonical_bytes()?;
        CapsuleProgramId::new(schema_domained_blake3_id(
            CAPSULE_PROGRAM_V1_SCHEMA,
            &canonical,
        ))
    }
}

/// Non-identity envelope around a Capsule Program contract.
///
/// Everything here besides `program_contract` is excluded from the program
/// identity: `provenance` (which carries `authoring_schema`/`name`/`version`
/// per the ADR-014 §2.1 non-identity classification), diagnostics,
/// timestamps, and the stored `capsule_program_id` itself. Unlike the
/// identity-bearing contract, the envelope is deliberately tolerant — unknown
/// fields are ignored on read instead of failing closed, because none of them
/// may influence the id. [`Self::verify`] recomputes the canonical hash from
/// the embedded contract and fails closed on any mismatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapsuleProgramEnvelopeV1 {
    pub program_contract: CapsuleProgramContractV1,
    pub capsule_program_id: CapsuleProgramId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub provenance: serde_json::Value,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub diagnostics: serde_json::Value,
}

impl CapsuleProgramEnvelopeV1 {
    /// Verify that the stored `capsule_program_id` matches the canonical hash
    /// of the embedded identity-bearing contract. A mismatch is terminal for
    /// the reader: the envelope must not be trusted or republished.
    pub fn verify(&self) -> Result<(), CapsuleProgramError> {
        let computed = self.program_contract.compute_capsule_program_id()?;
        if computed != self.capsule_program_id {
            return Err(CapsuleProgramError::CapsuleProgramIdMismatch {
                stored: self.capsule_program_id.to_string(),
                computed: computed.to_string(),
            });
        }
        Ok(())
    }

    /// Obtain a [`VerifiedCapsuleProgramId`] from this envelope. Routes
    /// through the proof-preserving
    /// [`VerifiedCapsuleProgramId::verify_contract_id`] seam, which re-derives
    /// the canonical hash from the embedded contract and fails closed on any
    /// mismatch with the stored id before wrapping it. This is the ONLY way
    /// to obtain a [`VerifiedCapsuleProgramId`] in v1.
    pub fn verified_capsule_program_id(
        &self,
    ) -> Result<VerifiedCapsuleProgramId, CapsuleProgramError> {
        VerifiedCapsuleProgramId::verify_contract_id(
            &self.program_contract,
            &self.capsule_program_id,
        )
    }
}

/// Parent-link verification failure (ADR-014 §5).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CapsuleProgramLinkError {
    #[error("capsule program envelope failed verification: {0}")]
    ProgramEnvelope(#[from] CapsuleProgramError),
    #[error(
        "execution envelope claims a capsule_program_id but no program identity envelope \
         is present to verify it against (orphan claim, fail closed)"
    )]
    ParentEnvelopeMissing,
    #[error(
        "execution envelope does not claim a parent capsule_program_id \
         (the claim is mandatory when both envelopes are present)"
    )]
    ParentMissing,
    #[error(
        "execution envelope claims parent {claimed} but the verified capsule program \
         identity is {verified}"
    )]
    ParentMismatch {
        claimed: CapsuleProgramId,
        verified: CapsuleProgramId,
    },
}

/// Prove the execution envelope's parent-association CLAIM is internally
/// consistent with an already-verified Capsule Program identity.
///
/// Pairwise check only: a `None` claim is
/// [`CapsuleProgramLinkError::ParentMissing`] (the claim is mandatory when
/// both envelopes are present — ADR-014 §5); the lock-state interpretation
/// (incl. the orphan-claim
/// [`CapsuleProgramLinkError::ParentEnvelopeMissing`] rejection and
/// true-legacy acceptance) lives in `capsule_lock/execution.rs`, which mints
/// the [`VerifiedCapsuleProgramId`] exactly once. Taking
/// `&VerifiedCapsuleProgramId` statically refuses a raw, unproven id, so the
/// claim can only ever be compared against a hash-checked declaration.
pub fn verify_program_parent(
    verified: &VerifiedCapsuleProgramId,
    execution: &ExecutionContractEnvelopeV1,
) -> Result<(), CapsuleProgramLinkError> {
    let Some(claimed) = execution.capsule_program_id.as_ref() else {
        return Err(CapsuleProgramLinkError::ParentMissing);
    };
    if claimed != verified.as_capsule_program_id() {
        return Err(CapsuleProgramLinkError::ParentMismatch {
            claimed: claimed.clone(),
            verified: verified.as_capsule_program_id().clone(),
        });
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Derivation entrypoint
// ─────────────────────────────────────────────────────────────────────────────

/// Derive the Capsule Program Contract from one pinned capsule root
/// (ADR-014 §2.0 — the ONLY public derivation path).
///
/// The input is a
/// [`VerifiedPinnedSourceMaterialization`](crate::program_source_projection::VerifiedPinnedSourceMaterialization),
/// not a bare path: ADR-014 §1 admits only a pinned source materialization
/// (immutable archive / `source_materialize` output), and a local working tree
/// is inadmissible in Phase 0 — admitting one needs its own follow-up ADR.
///
/// The manifest is read from `<root>/capsule.toml` inside this function; there
/// is deliberately no variant taking manifest text and a source root as
/// independent inputs, so a producer can never pair source A with manifest B.
///
/// Ordering follows ADR-014 §1: A1v2 admissibility runs over the ORIGINAL tree
/// FIRST, in full, before any manifest byte is parsed — the strict adapter's
/// `SourceExistingPath` checks may only assume "A1 already rejected every
/// in-tree symlink" because A1 has already run. Everything after that gate
/// reads from ONE process-private staging copy
/// ([`StagedCapsuleSource`](crate::program_source_projection::StagedCapsuleSource)):
/// control files are resolved in it, the manifest is loaded from it, existence
/// checks resolve against it, and the digest is taken over it with exactly the
/// resolved control files removed. Manifest intent and source digest therefore
/// provably come from one tree state that no outside process can mutate
/// mid-derivation.
///
/// Both the ordinary v0.3 normalizer (`load_manifest`, strict validation) and
/// the strict identity gate (`parse_program_manifest_v03_input`, run inside the
/// adapter) must accept the manifest.
pub fn derive_capsule_program_contract(
    pinned: &VerifiedPinnedSourceMaterialization,
) -> Result<CapsuleProgramContractV1, CapsuleProgramError> {
    // Steps 1-3: admissibility over the original tree, staging copy, control
    // files resolved inside the copy.
    let staged = StagedCapsuleSource::stage(pinned)?;

    // The manifest comes from the staging copy, so the bytes parsed here are
    // the bytes the digest below was taken over. Both layers are handed an
    // absolute staging path and build their messages from it — `load_manifest`
    // embeds the manifest path, the strict adapter resolves every
    // `SourceExistingPath` against the root — so both are relativized: the
    // staging copy is process-private, and so is the pinned root behind it
    // whenever the proof was archive-minted.
    let loaded = crate::contract::manifest::load_manifest(staged.manifest_path())
        .map_err(|error| staged.relativize(CapsuleProgramError::ManifestLoad(error.to_string())))?;
    let manifest_intent = crate::contract::program_manifest_input::program_intent_from_v03(
        &loaded.model,
        &loaded.raw_text,
        staged.root(),
    )
    .map_err(|error| staged.relativize(error))?;

    // Steps 4-6: exclude exactly the resolved control files, then hash.
    let source = staged.into_projected()?.source_contract()?;

    let contract = CapsuleProgramContractV1 {
        schema: CAPSULE_PROGRAM_V1_SCHEMA.to_string(),
        source,
        manifest_intent,
    };
    contract.validate()?;
    Ok(contract)
}

// ─────────────────────────────────────────────────────────────────────────────
// Test helpers
// ─────────────────────────────────────────────────────────────────────────────

/// A minimal, valid [`CapsuleProgramContractV1`] for cross-module tests,
/// seeded so distinct `seed` values derive distinct canonical
/// `capsule_program_id`s. Mirrors `test_execution_contract`.
#[cfg(test)]
pub(in crate::contract) fn test_capsule_program_contract(seed: u8) -> CapsuleProgramContractV1 {
    let identifier = |value: &str| ProgramIdentifier::parse(value).expect("valid identifier");
    let mut state = UniqueBTreeMap::new();
    state.insert(
        identifier("scratch"),
        NormalizedStateIntent {
            kind: identifier("filesystem"),
            durability: identifier("ephemeral"),
            purpose: OpaqueAuthoredString::parse("run scratch").expect("valid authored string"),
            producer: None,
            attach: None,
            schema_id: None,
            sharing: None,
            size_mb: None,
        },
    );
    CapsuleProgramContractV1 {
        schema: CAPSULE_PROGRAM_V1_SCHEMA.to_string(),
        source: ProgramSourceContract {
            digest: ProgramSourceDigest::new([seed; 32]),
            projection_schema: ProgramSourceProjectionSchemaV1,
        },
        manifest_intent: ProgramManifestIntentV1 {
            schema: CAPSULE_PROGRAM_MANIFEST_INTENT_V1_SCHEMA.to_string(),
            capsule_type: identifier("web-app"),
            default_target: None,
            requirements: None,
            capabilities: None,
            execution: None,
            storage: None,
            state,
            network: None,
            model: None,
            transparency: None,
            build: None,
            pack: None,
            isolation: None,
            polymorphism: None,
            targets: None,
            platforms: UniqueBTreeMap::new(),
            exports: None,
            services: UniqueBTreeMap::new(),
            dependencies: UniqueBTreeMap::new(),
            tool_dependencies: UniqueBTreeMap::new(),
            required_env: Vec::new(),
            contracts: UniqueBTreeMap::new(),
            foundation_requirements: None,
            host_capabilities: Vec::new(),
            ingress: None,
            snapshot: None,
            seal_at: None,
            secrets: UniqueBTreeMap::new(),
            bindings: UniqueBTreeMap::new(),
            external: UniqueBTreeMap::new(),
            context: None,
            generated_bindings: UniqueBTreeMap::new(),
        },
    }
}

/// A minimal, valid [`CapsuleProgramEnvelopeV1`] whose stored id is the
/// canonical hash of [`test_capsule_program_contract`]`(seed)`.
#[cfg(test)]
pub(in crate::contract) fn test_capsule_program_envelope(seed: u8) -> CapsuleProgramEnvelopeV1 {
    let program_contract = test_capsule_program_contract(seed);
    let capsule_program_id = program_contract
        .compute_capsule_program_id()
        .expect("canonical id");
    CapsuleProgramEnvelopeV1 {
        program_contract,
        capsule_program_id,
        generated_at: None,
        provenance: serde_json::Value::Null,
        diagnostics: serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution_contract::test_execution_contract;

    fn fake_id() -> CapsuleProgramId {
        CapsuleProgramId::new(format!("blake3:{}", "0".repeat(64))).expect("valid id shape")
    }

    // ── derivation entrypoint (end to end) ───────────────────────────────

    const DERIVE_MANIFEST: &str = r#"
schema_version = "0.3"
name = "derive-fixture"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:1"
port = 8080
"#;

    /// The test-only mint: this test builds the tree it then attests to, so the
    /// pinnedness obligation is discharged here rather than asserted about
    /// someone else's directory. No caller outside this crate's unit tests can
    /// reach it — the public mint is `from_source_archive`.
    fn pinned(root: &std::path::Path) -> VerifiedPinnedSourceMaterialization {
        VerifiedPinnedSourceMaterialization::for_test(root).expect("pinned materialization")
    }

    /// The EARNED mint, over a tree frozen by the real materializer. Used by the
    /// error-text tests because it is the case the `for_test` mint cannot model:
    /// the pinned root is the extraction directory this crate owns, so an
    /// absolute path in a message is a process-private path the caller can
    /// neither act on nor be trusted with. The returned `TempDir` keeps the
    /// archive alive.
    fn archive_pinned(
        tree: &std::path::Path,
    ) -> (tempfile::TempDir, VerifiedPinnedSourceMaterialization) {
        let archive_dir = tempfile::tempdir().expect("archive dir");
        let archive = archive_dir.path().join("source.tar.zst");
        crate::foundation::blob::materialize_source_archive(tree, &archive)
            .expect("materialize source archive");
        let pinned = VerifiedPinnedSourceMaterialization::from_source_archive(&archive)
            .expect("archive extracts to a pinned materialization");
        (archive_dir, pinned)
    }

    #[test]
    fn derive_entrypoint_is_deterministic_and_lock_file_immune() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(root.path().join("capsule.toml"), DERIVE_MANIFEST).expect("manifest");
        std::fs::write(root.path().join("app.py"), b"print('hi')\n").expect("source");

        let first = derive_capsule_program_contract(&pinned(root.path())).expect("derive");
        let second = derive_capsule_program_contract(&pinned(root.path())).expect("derive again");
        assert_eq!(first, second);
        let baseline = first.compute_capsule_program_id().expect("id");

        // A canonical lock at the root never reaches the preimage — even one
        // carrying a program_identity-shaped body (self-reference immunity
        // through the full entrypoint, not just the projection layer).
        std::fs::write(
            root.path().join("capsule.lock"),
            format!(
                "{{\"schema_version\":1,\"program_identity\":{{\"capsule_program_id\":\"{baseline}\"}}}}"
            ),
        )
        .expect("lock");
        let with_lock =
            derive_capsule_program_contract(&pinned(root.path())).expect("derive with lock");
        assert_eq!(
            with_lock.compute_capsule_program_id().expect("id"),
            baseline
        );

        // Source bytes DO reach the preimage.
        std::fs::write(root.path().join("app.py"), b"print('bye')\n").expect("mutate source");
        let mutated =
            derive_capsule_program_contract(&pinned(root.path())).expect("derive mutated");
        assert_ne!(mutated.compute_capsule_program_id().expect("id"), baseline);
    }

    #[test]
    fn derive_entrypoint_rejects_lock_coexistence_and_missing_manifest() {
        let root = tempfile::tempdir().expect("tempdir");
        // The manifest is verified as a control file inside the staging copy
        // (ADR-014 §1 step 2), which runs before any manifest byte is parsed.
        let error = derive_capsule_program_contract(&pinned(root.path())).unwrap_err();
        let CapsuleProgramError::SourceProjection(message) = &error else {
            panic!("expected SourceProjection, got {error:?}");
        };
        assert!(message.contains("does not exist"), "{message}");

        std::fs::write(root.path().join("capsule.toml"), DERIVE_MANIFEST).expect("manifest");
        std::fs::write(root.path().join("capsule.lock"), b"{}").expect("canonical lock");
        std::fs::write(root.path().join("ato.lock.json"), b"{}").expect("alias lock");
        assert!(matches!(
            derive_capsule_program_contract(&pinned(root.path())),
            Err(CapsuleProgramError::SourceProjection(_))
        ));
    }

    /// A malformed manifest fails at load time. `load_manifest` builds its own
    /// message from the absolute path it was handed — the staging copy — so the
    /// entrypoint relativizes it: the manifest is named `capsule.toml`, and
    /// neither the staging copy nor the pinned root behind it appears.
    ///
    /// Derived from an ARCHIVE-minted proof, because that is the case where
    /// even "attribute it back to the pinned root" would still be disclosing a
    /// process-private directory.
    #[test]
    fn derive_entrypoint_reports_manifest_load_errors_relative_to_the_root() {
        let tree = tempfile::tempdir().expect("tempdir");
        std::fs::write(tree.path().join("capsule.toml"), b"not = [valid").expect("manifest");
        let (_archive_dir, proof) = archive_pinned(tree.path());
        let private_root = proof.root().display().to_string();

        let error = derive_capsule_program_contract(&proof).unwrap_err();
        let CapsuleProgramError::ManifestLoad(message) = &error else {
            panic!("expected ManifestLoad, got {error:?}");
        };
        assert!(
            !message.contains(&private_root),
            "the extraction root must not reach the message: {message}"
        );
        assert!(
            !message.contains(&tree.path().display().to_string()),
            "{message}"
        );
        // Non-vacuous: the message still names the file that failed to parse.
        assert!(message.contains("capsule.toml"), "{message}");
    }

    /// A `SourceExistingPath` rejection from the strict adapter. The adapter
    /// resolves every such path against the staging root, so its rejection is
    /// relativized by the same seam — while still naming the offending path in
    /// the spelling the author wrote.
    #[test]
    fn derive_entrypoint_reports_source_existing_path_rejections_relative_to_the_root() {
        let model_manifest = |path: &str| {
            format!(
                r#"
schema_version = "0.3"
name = "gate-fixture"
version = "0.1.0"
type = "app"
default_target = "chat"

[targets.chat]
runtime = "native-inference"
engine = "llama.cpp"
engine_version = "b9754"
model = "{path}"
"#
            )
        };

        // (a) the model names a control file the projection excludes.
        let tree = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tree.path().join("capsule.toml"),
            model_manifest("capsule.toml"),
        )
        .expect("manifest");
        let (_archive_dir, proof) = archive_pinned(tree.path());
        let private_root = proof.root().display().to_string();

        let error = derive_capsule_program_contract(&proof).unwrap_err();
        let CapsuleProgramError::InvalidValue { field, reason } = &error else {
            panic!("expected InvalidValue, got {error:?}");
        };
        assert_eq!(*field, "targets.*.model");
        assert!(!reason.contains(&private_root), "{reason}");
        // Non-vacuous: both the authored path and the control file it collided
        // with are named, relative to the root.
        assert!(reason.contains("'capsule.toml'"), "{reason}");
        assert!(reason.contains("control file"), "{reason}");

        // (b) the model names a path that is not in the tree at all.
        let tree = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tree.path().join("capsule.toml"),
            model_manifest("models/absent.gguf"),
        )
        .expect("manifest");
        let (_archive_dir, proof) = archive_pinned(tree.path());
        let private_root = proof.root().display().to_string();

        let error = derive_capsule_program_contract(&proof).unwrap_err();
        let CapsuleProgramError::InvalidValue { field, reason } = &error else {
            panic!("expected InvalidValue, got {error:?}");
        };
        assert_eq!(*field, "targets.*.model");
        assert!(!reason.contains(&private_root), "{reason}");
        assert!(reason.contains("'models/absent.gguf'"), "{reason}");
    }

    /// The projection layer's own rejection, through the entrypoint and from an
    /// archive-minted proof: no absolute path, and the manifest still named.
    #[test]
    fn derive_entrypoint_reports_missing_manifest_relative_to_the_root() {
        let tree = tempfile::tempdir().expect("tempdir");
        std::fs::write(tree.path().join("app.py"), b"print('hi')\n").expect("source");
        let (_archive_dir, proof) = archive_pinned(tree.path());
        let private_root = proof.root().display().to_string();

        let error = derive_capsule_program_contract(&proof).unwrap_err();
        let CapsuleProgramError::SourceProjection(message) = &error else {
            panic!("expected SourceProjection, got {error:?}");
        };
        assert!(!message.contains(&private_root), "{message}");
        assert!(
            message.contains("required manifest capsule.toml does not exist"),
            "{message}"
        );
    }

    /// A1v2 admissibility runs over the ORIGINAL tree before the manifest is
    /// parsed (ADR-014 §1 step 1): an unsafe absolute symlink is rejected even
    /// though the manifest itself is perfectly valid.
    #[cfg(unix)]
    #[test]
    fn derive_entrypoint_gates_admissibility_before_parsing_the_manifest() {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::write(root.path().join("capsule.toml"), DERIVE_MANIFEST).expect("manifest");
        std::os::unix::fs::symlink("/capsule.toml", root.path().join("link.toml"))
            .expect("symlink");

        let error = derive_capsule_program_contract(&pinned(root.path())).unwrap_err();
        let CapsuleProgramError::SourceProjection(message) = &error else {
            panic!("expected SourceProjection, got {error:?}");
        };
        assert!(message.contains("A1v2 admissibility"), "{message}");
    }

    // ── id + envelope ────────────────────────────────────────────────────

    #[test]
    fn capsule_program_id_is_domain_separated_jcs_blake3() {
        let contract = test_capsule_program_contract(7);
        let canonical = serde_jcs::to_vec(&contract).expect("canonical contract");
        let mut expected_input = CAPSULE_PROGRAM_V1_SCHEMA.as_bytes().to_vec();
        expected_input.push(0);
        expected_input.extend(canonical);

        assert_eq!(
            contract.compute_capsule_program_id().expect("id"),
            CapsuleProgramId::new(format!("blake3:{}", blake3::hash(&expected_input).to_hex()))
                .expect("valid id")
        );
    }

    #[test]
    fn compute_is_deterministic_and_seed_sensitive() {
        let first = test_capsule_program_contract(7);
        assert_eq!(
            first.compute_capsule_program_id().unwrap(),
            first.clone().compute_capsule_program_id().unwrap()
        );
        assert_ne!(
            first.compute_capsule_program_id().unwrap(),
            test_capsule_program_contract(8)
                .compute_capsule_program_id()
                .unwrap()
        );
    }

    #[test]
    fn malformed_capsule_program_ids_are_rejected() {
        for invalid in [
            format!("sha256:{}", "0".repeat(64)),
            format!("blake3:{}", "A".repeat(64)),
            format!("blake3:{}", "0".repeat(63)),
            "blake3:not-a-digest".to_string(),
            "0".repeat(64),
        ] {
            assert!(CapsuleProgramId::new(invalid.clone()).is_err(), "{invalid}");
        }
    }

    #[test]
    fn wrong_contract_schema_fails_closed() {
        let mut contract = test_capsule_program_contract(1);
        contract.schema = "ato.capsule-program/v2".to_string();
        assert_eq!(
            contract.compute_capsule_program_id(),
            Err(CapsuleProgramError::InvalidSchema)
        );

        let mut contract = test_capsule_program_contract(1);
        contract.manifest_intent.schema = "ato.capsule-program-manifest-intent/v2".to_string();
        assert!(matches!(
            contract.compute_capsule_program_id(),
            Err(CapsuleProgramError::InvalidValue { .. })
        ));
    }

    #[test]
    fn envelope_verifies_and_rejects_id_mismatch() {
        let envelope = test_capsule_program_envelope(7);
        envelope.verify().expect("matching envelope verifies");

        let mut tampered = envelope.clone();
        tampered.capsule_program_id = fake_id();
        assert!(matches!(
            tampered.verify(),
            Err(CapsuleProgramError::CapsuleProgramIdMismatch { .. })
        ));

        // Tampering with the CONTRACT while keeping the stored id also fails.
        let mut tampered = envelope;
        tampered.program_contract.source.digest = ProgramSourceDigest::new([0xEE; 32]);
        assert!(matches!(
            tampered.verify(),
            Err(CapsuleProgramError::CapsuleProgramIdMismatch { .. })
        ));
    }

    #[test]
    fn envelope_metadata_never_changes_the_id() {
        let mut envelope = test_capsule_program_envelope(7);
        let expected = envelope.capsule_program_id.clone();

        envelope.generated_at = Some("2026-07-24T00:00:00Z".to_string());
        envelope.provenance = serde_json::json!({
            "authoring_schema": "0.3",
            "name": "my-app",
            "version": "1.2.3",
        });
        envelope.diagnostics = serde_json::json!({"adapter_log": "..."});

        envelope.verify().expect("metadata never affects the id");
        assert_eq!(
            envelope
                .program_contract
                .compute_capsule_program_id()
                .unwrap(),
            expected
        );
    }

    #[test]
    fn envelope_tolerates_unknown_non_identity_fields() {
        let envelope = test_capsule_program_envelope(3);
        let mut value = serde_json::to_value(&envelope).unwrap();
        let object = value.as_object_mut().unwrap();
        object.insert("registry_row".to_string(), serde_json::json!("row-77"));
        object.insert("publisher".to_string(), serde_json::json!("dev-a"));

        let parsed =
            serde_json::from_value::<CapsuleProgramEnvelopeV1>(value).expect("tolerant read");
        parsed.verify().expect("unknown envelope fields ignored");
    }

    #[test]
    fn verified_id_is_minted_only_from_a_matching_envelope() {
        let envelope = test_capsule_program_envelope(5);
        let verified = envelope
            .verified_capsule_program_id()
            .expect("matching envelope yields a verified id");
        assert_eq!(
            *verified.as_capsule_program_id(),
            envelope.capsule_program_id
        );

        let mut tampered = envelope;
        tampered.capsule_program_id = fake_id();
        assert!(matches!(
            tampered.verified_capsule_program_id(),
            Err(CapsuleProgramError::CapsuleProgramIdMismatch { .. })
        ));
    }

    #[test]
    fn verify_contract_id_recomputes_and_rejects_a_mismatch() {
        let contract = test_capsule_program_contract(9);
        let real_id = contract.compute_capsule_program_id().unwrap();

        let verified = VerifiedCapsuleProgramId::verify_contract_id(&contract, &real_id)
            .expect("matching id is proof-preserving");
        assert_eq!(*verified.as_capsule_program_id(), real_id);

        assert!(matches!(
            VerifiedCapsuleProgramId::verify_contract_id(&contract, &fake_id()),
            Err(CapsuleProgramError::CapsuleProgramIdMismatch { .. })
        ));
    }

    #[test]
    fn unknown_identity_field_fails_closed() {
        let baseline = serde_json::to_value(test_capsule_program_contract(2)).unwrap();

        let mut top = baseline.clone();
        top.as_object_mut()
            .unwrap()
            .insert("runner".to_string(), serde_json::json!("local"));
        assert!(serde_json::from_value::<CapsuleProgramContractV1>(top).is_err());

        let mut nested = baseline;
        nested["manifest_intent"]
            .as_object_mut()
            .unwrap()
            .insert("name".to_string(), serde_json::json!("my-app"));
        assert!(
            serde_json::from_value::<CapsuleProgramContractV1>(nested).is_err(),
            "non-identity manifest fields must not deserialize into the intent"
        );
    }

    #[test]
    fn canonicalization_is_field_order_and_whitespace_independent() {
        let baseline = test_capsule_program_contract(7);
        let expected = baseline.compute_capsule_program_id().unwrap();

        let reordered = format!(
            r#"{{
                "manifest_intent": {{
                    "state": {{ "scratch": {{
                        "purpose": "run scratch",
                        "durability": "ephemeral",
                        "kind": "filesystem" }} }},
                    "capsule_type": "web-app",
                    "schema": "ato.capsule-program-manifest-intent/v1" }},
                "source": {{
                    "projection_schema": "ato.capsule-program-source-projection/v1",
                    "digest": "sha256:{digest}" }},
                "schema": "ato.capsule-program/v1"
            }}"#,
            digest = "07".repeat(32)
        );

        let parsed = serde_json::from_str::<CapsuleProgramContractV1>(&reordered).unwrap();
        assert_eq!(parsed, baseline);
        assert_eq!(parsed.compute_capsule_program_id().unwrap(), expected);
        assert_eq!(
            parsed.canonical_bytes().unwrap(),
            baseline.canonical_bytes().unwrap()
        );
    }

    #[test]
    fn source_digest_and_manifest_intent_mutations_change_the_id() {
        let baseline = test_capsule_program_contract(7);
        let baseline_id = baseline.compute_capsule_program_id().unwrap();

        let mut source_changed = baseline.clone();
        source_changed.source.digest = ProgramSourceDigest::new([0x42; 32]);
        assert_ne!(
            source_changed.compute_capsule_program_id().unwrap(),
            baseline_id
        );

        let mut intent_changed = baseline.clone();
        intent_changed.manifest_intent.capsule_type = ProgramIdentifier::parse("tool").unwrap();
        assert_ne!(
            intent_changed.compute_capsule_program_id().unwrap(),
            baseline_id
        );

        let mut state_changed = baseline;
        state_changed
            .manifest_intent
            .state
            .values_mut()
            .next()
            .unwrap()
            .purpose = OpaqueAuthoredString::parse("different purpose").unwrap();
        assert_ne!(
            state_changed.compute_capsule_program_id().unwrap(),
            baseline_id
        );
    }

    #[test]
    fn unsorted_set_like_lists_fail_validation() {
        let mut contract = test_capsule_program_contract(1);
        contract.manifest_intent.required_env = vec![
            ProgramIdentifier::parse("ZZZ").unwrap(),
            ProgramIdentifier::parse("AAA").unwrap(),
        ];
        assert_eq!(
            contract.validate(),
            Err(CapsuleProgramError::NonCanonicalList("required_env"))
        );

        let mut contract = test_capsule_program_contract(1);
        contract.manifest_intent.required_env = vec![
            ProgramIdentifier::parse("AAA").unwrap(),
            ProgramIdentifier::parse("AAA").unwrap(),
        ];
        assert_eq!(
            contract.validate(),
            Err(CapsuleProgramError::NonCanonicalList("required_env"))
        );
    }

    // ── duplicate identity map keys ──────────────────────────────────────

    /// A contract document whose `manifest_intent` carries `body` in addition
    /// to the two mandatory intent fields. Duplicate-key vectors must be fed
    /// as raw TEXT: `serde_json::Value` is itself last-wins and would collapse
    /// the duplicate before the typed layer ever saw it.
    fn contract_json(body: &str) -> String {
        format!(
            r#"{{
                "schema": "ato.capsule-program/v1",
                "source": {{
                    "digest": "sha256:{digest}",
                    "projection_schema": "ato.capsule-program-source-projection/v1"
                }},
                "manifest_intent": {{
                    "schema": "ato.capsule-program-manifest-intent/v1",
                    "capsule_type": "web-app"{body}
                }}
            }}"#,
            digest = "07".repeat(32)
        )
    }

    #[test]
    fn duplicate_top_level_identity_map_key_fails_closed() {
        let state = |first: &str, second: &str| {
            format!(
                r#", "state": {{
                    "{first}": {{ "kind": "filesystem", "durability": "ephemeral",
                                  "purpose": "run scratch" }},
                    "{second}": {{ "kind": "database", "durability": "persistent",
                                   "purpose": "app data" }}
                }}"#
            )
        };

        let unique = serde_json::from_str::<CapsuleProgramContractV1>(&contract_json(&state(
            "scratch", "data",
        )))
        .expect("distinct keys parse");
        assert_eq!(unique.manifest_intent.state.len(), 2);

        let error = serde_json::from_str::<CapsuleProgramContractV1>(&contract_json(&state(
            "scratch", "scratch",
        )))
        .expect_err("a repeated identity map key must not last-wins");
        assert!(
            error
                .to_string()
                .contains("duplicate identity map key 'scratch'"),
            "error must name the duplicated key, got: {error}"
        );
    }

    #[test]
    fn duplicate_nested_identity_map_key_fails_closed() {
        // targets.<label>.env — one level below a map that is itself keyed.
        let target_env = |first: &str, second: &str| {
            format!(
                r#", "targets": {{ "targets": {{ "app": {{
                    "env": {{ "{first}": "8080", "{second}": "9090" }} }} }} }}"#
            )
        };
        serde_json::from_str::<CapsuleProgramContractV1>(&contract_json(&target_env(
            "PORT", "HOST",
        )))
        .expect("distinct nested keys parse");
        let error = serde_json::from_str::<CapsuleProgramContractV1>(&contract_json(&target_env(
            "PORT", "PORT",
        )))
        .expect_err("a repeated nested map key must not last-wins");
        assert!(
            error
                .to_string()
                .contains("duplicate identity map key 'PORT'"),
            "error must name the duplicated nested key, got: {error}"
        );

        // ingress.env_inject — a map of maps: the INNER map is checked too.
        let env_inject = |first: &str, second: &str| {
            format!(
                r#", "ingress": {{ "mode": "path", "env_inject": {{
                    "web": {{ "{first}": "a", "{second}": "b" }} }} }}"#
            )
        };
        serde_json::from_str::<CapsuleProgramContractV1>(&contract_json(&env_inject("A", "B")))
            .expect("distinct inner keys parse");
        let error =
            serde_json::from_str::<CapsuleProgramContractV1>(&contract_json(&env_inject("A", "A")))
                .expect_err("a repeated doubly-nested map key must not last-wins");
        assert!(
            error.to_string().contains("duplicate identity map key 'A'"),
            "error must name the duplicated inner key, got: {error}"
        );
    }

    // ── non-canonical spellings of absence ───────────────────────────────

    /// The recorded shared-vector baseline (`contract/vectors/baseline.json`),
    /// verbatim, with the `capsule_program_id` recorded for it BEFORE the
    /// absence-spelling tightening. Absence is spelled by omission here, so
    /// the tightening narrows only what is REJECTED: this document, its
    /// serialized form, and its id are untouched.
    const RECORDED_BASELINE_CONTRACT: &str = r#"{
      "manifest_intent": {
        "capsule_type": "web-app",
        "schema": "ato.capsule-program-manifest-intent/v1",
        "state": {
          "scratch": {
            "durability": "ephemeral",
            "kind": "filesystem",
            "purpose": "run scratch"
          }
        }
      },
      "schema": "ato.capsule-program/v1",
      "source": {
        "digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
        "projection_schema": "ato.capsule-program-source-projection/v1"
      }
    }"#;

    const RECORDED_BASELINE_ID: &str =
        "blake3:eaf5c32fa4a5c4fe83b6c1bad10d556ca82cde5b948f40a26323ebe7b9b81c4f";

    #[test]
    fn omitted_absence_round_trips_to_the_recorded_id() {
        let document: serde_json::Value =
            serde_json::from_str(RECORDED_BASELINE_CONTRACT).expect("fixture text is JSON");
        let contract = serde_json::from_value::<CapsuleProgramContractV1>(document.clone())
            .expect("the canonical spelling of absence — an omitted key — still parses");

        assert_eq!(
            contract.compute_capsule_program_id().unwrap().to_string(),
            RECORDED_BASELINE_ID,
            "tightening deserialization must not move a recorded id"
        );
        assert_eq!(
            serde_json::to_value(&contract).unwrap(),
            document,
            "the omitted spelling must survive a round trip unchanged"
        );
    }

    #[test]
    fn non_canonical_spellings_of_absence_fail_closed() {
        // `field` is the key the message must name; each body is a spelling
        // the typed layer would otherwise DROP on the way back out, letting a
        // consumer that canonicalizes the raw JSON hash a document this layer
        // never represents.
        for (field, spelling, body) in [
            ("default_target", "null", r#", "default_target": null"#),
            ("state", "{}", r#", "state": {}"#),
            ("required_env", "[]", r#", "required_env": []"#),
            // Nested: one and two levels below the top-level intent.
            (
                "producer",
                "null",
                r#", "state": { "scratch": { "kind": "filesystem",
                    "durability": "ephemeral", "purpose": "run scratch",
                    "producer": null } }"#,
            ),
            ("env", "{}", r#", "targets": { "env": {} }"#),
            (
                "egress_allow",
                "[]",
                r#", "network": { "egress_allow": [] }"#,
            ),
            // A flag omitted when false: writing the skipped value explicitly
            // is the same ambiguity in a different shape.
            ("chat", "false", r#", "capabilities": { "chat": false }"#),
        ] {
            let error = serde_json::from_str::<CapsuleProgramContractV1>(&contract_json(body))
                .expect_err("a non-canonical spelling of absence must fail closed");
            assert!(
                error.to_string().contains(&format!("`{field}`")),
                "explicit {spelling} must be rejected naming `{field}`, got: {error}"
            );
        }
    }

    #[test]
    fn present_identity_values_still_parse_after_the_tightening() {
        for body in [
            r#", "default_target": "linux-x86_64""#,
            r#", "required_env": ["PORT"]"#,
            r#", "targets": { "env": { "PORT": "8080" } }"#,
            r#", "capabilities": { "chat": true }"#,
            r#", "state": { "scratch": { "kind": "filesystem",
                "durability": "ephemeral", "purpose": "run scratch",
                "producer": "builder" } }"#,
        ] {
            serde_json::from_str::<CapsuleProgramContractV1>(&contract_json(body))
                .unwrap_or_else(|error| panic!("present value must parse: {body}: {error}"));
        }
    }

    #[test]
    fn unique_map_serializes_exactly_as_a_btree_map() {
        let entry = |name: &str, value: &str| {
            (
                ProgramIdentifier::parse(name).unwrap(),
                OpaqueAuthoredString::parse(value).unwrap(),
            )
        };
        let entries = [entry("b", "2"), entry("a", "1")];
        let unique: UniqueBTreeMap<ProgramIdentifier, OpaqueAuthoredString> =
            entries.iter().cloned().collect();
        let plain: BTreeMap<ProgramIdentifier, OpaqueAuthoredString> =
            entries.iter().cloned().collect();

        assert_eq!(
            serde_jcs::to_vec(&unique).unwrap(),
            serde_jcs::to_vec(&plain).unwrap(),
            "the wrapper must not change the canonical preimage"
        );
        assert_eq!(
            serde_json::to_string(&unique).unwrap(),
            r#"{"a":"1","b":"2"}"#
        );
        assert_eq!(
            serde_json::from_str::<UniqueBTreeMap<ProgramIdentifier, OpaqueAuthoredString>>(
                r#"{"a":"1","b":"2"}"#
            )
            .unwrap(),
            unique,
            "a duplicate-free map still round-trips"
        );
    }

    // ── program source contract ──────────────────────────────────────────

    #[test]
    fn program_source_digest_is_sha256_only_lowercase() {
        let canonical = format!("sha256:{}", "ab".repeat(32));
        let parsed = ProgramSourceDigest::parse(&canonical).expect("canonical digest");
        assert_eq!(parsed.to_string(), canonical);
        assert_eq!(
            serde_json::from_str::<ProgramSourceDigest>(&format!("\"{canonical}\"")).unwrap(),
            parsed
        );

        for invalid in [
            format!("blake3:{}", "ab".repeat(32)),
            format!("sha256:{}", "AB".repeat(32)),
            format!("sha256:{}", "ab".repeat(31)),
            "ab".repeat(32),
            "sha256:zz".to_string(),
        ] {
            assert!(ProgramSourceDigest::parse(&invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn projection_schema_serializes_as_exactly_the_schema_string() {
        let json = serde_json::to_string(&ProgramSourceProjectionSchemaV1).unwrap();
        assert_eq!(
            json,
            format!("\"{CAPSULE_PROGRAM_SOURCE_PROJECTION_V1_SCHEMA}\"")
        );
        assert!(serde_json::from_str::<ProgramSourceProjectionSchemaV1>(&json).is_ok());
        assert!(
            serde_json::from_str::<ProgramSourceProjectionSchemaV1>(
                "\"ato.capsule-program-source-projection/v2\""
            )
            .is_err()
        );
    }

    // ── semantic type grammars ───────────────────────────────────────────

    #[test]
    fn source_relative_path_root_is_the_only_root_spelling() {
        assert_eq!(
            SourceRelativePath::parse(".").unwrap(),
            SourceRelativePath::Root
        );
        assert_eq!(SourceRelativePath::Root.as_str(), ".");
        assert_eq!(
            serde_json::to_string(&SourceRelativePath::Root).unwrap(),
            "\".\""
        );
        assert_eq!(
            serde_json::from_str::<SourceRelativePath>("\".\"").unwrap(),
            SourceRelativePath::Root
        );
    }

    #[test]
    fn source_relative_path_accepts_only_the_canonical_spelling() {
        for good in [
            "index.html",
            "src/app",
            "src/app/main.py",
            "ユーザー/データ",
        ] {
            let parsed = SourceRelativePath::parse(good).expect(good);
            assert_eq!(parsed.as_str(), good);
        }
        for bad in [
            "", "./", "./x", "x/.", "x/..", "..", "../x", "/x", "x/", "a//b", "a\\b", "a\u{7}b",
            "a\u{85}b",
        ] {
            assert!(SourceRelativePath::parse(bad).is_err(), "{bad:?}");
        }
        // Non-NFC input is rejected, never silently normalized.
        assert!(SourceRelativePath::parse("cafe\u{301}").is_err());
        assert!(SourceRelativePath::parse("caf\u{e9}").is_ok());
    }

    #[test]
    fn source_path_policy_wrappers_are_transparent() {
        let existing: SourceExistingPath =
            serde_json::from_str("\"requirements.txt\"").expect("lexical parse");
        assert_eq!(
            serde_json::to_string(&existing).unwrap(),
            "\"requirements.txt\""
        );
        let future: SourceRelativeFuturePath =
            serde_json::from_str("\"dist/server.js\"").expect("lexical parse");
        assert_eq!(
            serde_json::to_string(&future).unwrap(),
            "\"dist/server.js\""
        );
        // The Existing policy stays lexical at the serde layer.
        assert!(serde_json::from_str::<SourceExistingPath>("\"x/..\"").is_err());
    }

    #[test]
    fn http_request_target_grammar() {
        for good in ["/", "/app", "/api/v1?x=1", "/健康"] {
            assert!(HttpRequestTarget::parse(good).is_ok(), "{good}");
        }
        for bad in ["", "app", "health", "/a b", "/a\tb", "/a\\b", "/a\u{7}b"] {
            assert!(HttpRequestTarget::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn tcp_probe_target_grammar() {
        for good in ["5432", "db:5432", "127.0.0.1:80"] {
            assert!(TcpProbeTarget::parse(good).is_ok(), "{good}");
        }
        for bad in ["", "db :5432", "db\t5432", "db\u{7}"] {
            assert!(TcpProbeTarget::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn probe_port_reference_grammar() {
        for good in ["PORT", "web", "http-1"] {
            assert!(ProbePortReference::parse(good).is_ok(), "{good}");
        }
        for bad in ["", "P T", "p\u{7}"] {
            assert!(ProbePortReference::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn glob_pattern_grammar() {
        for good in ["lib/**/*.so", "*.safetensors", "a b/*.txt"] {
            assert!(GlobPattern::parse(good).is_ok(), "{good}");
        }
        for bad in ["", "a\0b", "a\u{7}b"] {
            assert!(GlobPattern::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn remote_artifact_ref_grammar() {
        for good in [
            "ghcr.io/org/image:tag",
            "https://example.invalid/model.gguf",
            "hf:org/model",
            "org/model",
        ] {
            assert!(RemoteArtifactRef::parse(good).is_ok(), "{good}");
        }
        for bad in ["", "a b", "a\u{7}b", "a\nb"] {
            assert!(RemoteArtifactRef::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn sha256_digest_pin_spelling_table() {
        let bare = "ab".repeat(32);
        let prefixed = format!("sha256:{bare}");
        let uppercase = "AB".repeat(32);

        // Flexible authoring: bare, prefixed, and uppercase all normalize to
        // the SAME canonical IR value.
        let canonical = Sha256DigestPin::parse_flexible(&bare).expect("bare");
        assert_eq!(
            Sha256DigestPin::parse_flexible(&prefixed).expect("prefixed"),
            canonical
        );
        assert_eq!(
            Sha256DigestPin::parse_flexible(&uppercase).expect("uppercase"),
            canonical
        );
        assert_eq!(canonical.to_string(), bare);

        // Prefixed-only authoring (targets.source_digest) rejects the bare
        // spelling — the existing validator already rejects it.
        assert!(Sha256DigestPin::parse_prefixed(&prefixed).is_ok());
        assert!(Sha256DigestPin::parse_prefixed(&bare).is_err());

        // IR read-back accepts ONLY the canonical bare-lowercase spelling.
        assert_eq!(
            serde_json::from_str::<Sha256DigestPin>(&format!("\"{bare}\"")).unwrap(),
            canonical
        );
        assert!(serde_json::from_str::<Sha256DigestPin>(&format!("\"{prefixed}\"")).is_err());
        assert!(serde_json::from_str::<Sha256DigestPin>(&format!("\"{uppercase}\"")).is_err());
        assert!(Sha256DigestPin::parse_flexible(&"ab".repeat(31)).is_err());
    }

    #[test]
    fn cas_content_digest_grammar() {
        let sha = format!("sha256:{}", "0c".repeat(32));
        let blake = format!("blake3:{}", "0c".repeat(32));
        assert_eq!(CasContentDigest::parse(&sha).unwrap().to_string(), sha);
        assert_eq!(CasContentDigest::parse(&blake).unwrap().to_string(), blake);
        // Mixed-case hex is lowercase-normalized.
        assert_eq!(
            CasContentDigest::parse(&format!("sha256:{}", "0C".repeat(32)))
                .unwrap()
                .to_string(),
            sha
        );
        for bad in [
            "0c".repeat(32),
            format!("sha512:{}", "0c".repeat(32)),
            format!("sha256:{}", "0c".repeat(31)),
            "sha256:xyz".to_string(),
        ] {
            assert!(CasContentDigest::parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn git_commit_revision_is_40_hex_only() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            GitCommitRevision::parse(commit).unwrap().to_string(),
            commit
        );
        assert_eq!(
            GitCommitRevision::parse(&commit.to_ascii_uppercase())
                .unwrap()
                .to_string(),
            commit
        );
        for bad in [
            "main",
            "0123456",
            &"ab".repeat(32), // 64 hex = a digest, not a commit
        ] {
            assert!(GitCommitRevision::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn wit_world_ref_grammar_and_default() {
        for good in ["wasi:cli/command", "uarc:v1/http-handler"] {
            assert!(WitWorldRef::parse(good).is_ok(), "{good}");
        }
        for bad in ["", "wasi cli", "wasi\tcli", "世界"] {
            assert!(WitWorldRef::parse(bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            WitWorldRef::default_cli_command().as_str(),
            "wasi:cli/command"
        );
    }

    #[test]
    fn container_user_spec_grammar() {
        for good in ["1000", "1000:1000", "postgres", "app:app"] {
            assert!(ContainerUserSpec::parse(good).is_ok(), "{good}");
        }
        for bad in ["", "user name", "u\u{7}"] {
            assert!(ContainerUserSpec::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn opaque_command_and_authored_string_reject_only_nul() {
        assert!(OpaqueCommand::parse("").is_ok());
        assert!(OpaqueCommand::parse("npm run build && echo done").is_ok());
        assert!(OpaqueCommand::parse("a\0b").is_err());

        assert!(OpaqueAuthoredString::parse("").is_ok());
        assert!(OpaqueAuthoredString::parse("free form\nvalue").is_ok());
        assert!(OpaqueAuthoredString::parse("a\0b").is_err());
    }

    #[test]
    fn program_identifier_grammar() {
        for good in ["my-app", "NODE_ENV", "linux-x86_64", "service@1", "web"] {
            assert!(ProgramIdentifier::parse(good).is_ok(), "{good}");
        }
        for bad in ["", "a b", "a\tb", "日本語"] {
            assert!(ProgramIdentifier::parse(bad).is_err(), "{bad:?}");
        }
    }

    // ── parent link ──────────────────────────────────────────────────────

    fn execution_envelope_claiming(
        seed: u8,
        claim: Option<CapsuleProgramId>,
    ) -> ExecutionContractEnvelopeV1 {
        let execution_contract = test_execution_contract(seed);
        let execution_id = execution_contract.compute_execution_id().expect("id");
        ExecutionContractEnvelopeV1 {
            execution_contract,
            execution_id,
            capsule_program_id: claim,
            resolved_refs: Default::default(),
            generated_at: None,
            provenance: serde_json::Value::Null,
            diagnostics: serde_json::Value::Null,
            evidence: serde_json::Value::Null,
        }
    }

    #[test]
    fn verify_program_parent_requires_a_claim() {
        let verified = test_capsule_program_envelope(7)
            .verified_capsule_program_id()
            .expect("verified id");
        let execution = execution_envelope_claiming(1, None);
        assert_eq!(
            verify_program_parent(&verified, &execution),
            Err(CapsuleProgramLinkError::ParentMissing)
        );
    }

    #[test]
    fn verify_program_parent_accepts_a_matching_claim() {
        let envelope = test_capsule_program_envelope(7);
        let verified = envelope.verified_capsule_program_id().expect("verified id");
        let execution = execution_envelope_claiming(1, Some(envelope.capsule_program_id.clone()));
        verify_program_parent(&verified, &execution).expect("matching claim verifies");
    }

    #[test]
    fn verify_program_parent_rejects_a_mismatched_claim() {
        let envelope = test_capsule_program_envelope(7);
        let verified = envelope.verified_capsule_program_id().expect("verified id");
        let other = test_capsule_program_envelope(8).capsule_program_id;
        let execution = execution_envelope_claiming(1, Some(other.clone()));
        assert_eq!(
            verify_program_parent(&verified, &execution),
            Err(CapsuleProgramLinkError::ParentMismatch {
                claimed: other,
                verified: envelope.capsule_program_id,
            })
        );
    }

    #[test]
    fn absent_parent_claim_serializes_as_omitted() {
        // The claim is a non-identity envelope field: when absent, the
        // envelope's serialized bytes are identical to a pre-ADR-014 envelope.
        let envelope = execution_envelope_claiming(1, None);
        let value = serde_json::to_value(&envelope).unwrap();
        assert!(value.get("capsule_program_id").is_none());

        let claiming = execution_envelope_claiming(
            1,
            Some(test_capsule_program_envelope(7).capsule_program_id),
        );
        // Adding the claim never moves execution_id.
        assert_eq!(envelope.execution_id, claiming.execution_id);
        claiming.verify().expect("claim does not affect verify");
    }
}
