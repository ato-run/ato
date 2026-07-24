//! External State exclusion boundary, schema gate, and receipt boundary
//! (issue #1090, Gate-0 style: pure, deterministic, no live wiring).
//!
//! External State is mutable or principal-specific state attached to a Session —
//! user data, persistent app data, secret/API-key values, OAuth tokens, Ato
//! identity, concrete database/service bindings (RFC
//! `docs/rfcs/accepted/CAPSULE_V1_EXECUTION_MODEL_SPEC.md` §9.1). Capsule v1 makes
//! it a **structurally separate runtime attachment** whose *schema contract* is
//! identity-bearing while its concrete *instance* and *values* are not.
//!
//! This module holds the three pure pieces #1090 owns (the RFC names it directly
//! at §"References" → `crates/snapshot/src/external_state.rs`):
//!
//! 1. **Live-workload requirement analysis** (§8.3): whether a `running` capture
//!    is ineligible because the live workload requires **restore-time bindings** —
//!    either declared External State *or* declared restore-time secret bindings
//!    (secret VALUES are External State; production secrets must never be attached
//!    before capture, and there is no secret-bearing running fallback). This is the
//!    analysis the sanctioned
//!    [`crate::acceptance::VerifiedRunningSnapshotEligibility`] production
//!    constructor runs — fail closed, never a caller-supplied bool.
//! 2. **Snapshot exclusion boundary** (§9.2, §17.4): each `snapshot = "exclude"`
//!    binding is backed by a **separate** volume, so its bytes MUST be absent
//!    from every shared Snapshot layer (memory / vmstate / disk). The
//!    proof-carrying, identity-bound [`VerifiedExcludedStateBoundary`] asserts the
//!    STRUCTURAL guarantees (§17.4 is *structurally set up* here, not byte-proven —
//!    see the type doc).
//! 3. **Schema gate + attach + receipt boundary** (§9.2, §9.3): an incompatible
//!    state schema fails **before** the read-write attach, and the Session
//!    Receipt records only an *opaque* state reference + generation + non-secret
//!    compatibility evidence — never content, secret values, owner, or instance
//!    id.
//!
//! **Identity split (already frozen in G0-1, reused here).** The identity-bearing
//! External State contract ([`ExternalStateContract`]: binding name,
//! mount/injection target, access mode, schema identity, Snapshot-exclusion rule)
//! lives inside [`ExecutionContractV1`] and is JCS-hashed into `execution_id`
//! (RFC §4.2). The concrete *instance* (owner id, volume/binding instance id,
//! generation, data bytes, secret values, identity assertions) is excluded from
//! identity by construction (RFC §4.3) — it appears only in
//! [`ExternalStateInstance`], which deliberately carries **no** data-byte or
//! secret field, so nothing here can leak instance values into an id, a Snapshot,
//! a lockfile, or a Receipt.

use std::collections::{BTreeMap, BTreeSet};

use capsule::execution_contract::{
    ContentDigest, ExecutionContractEnvelopeV1, ExecutionContractV1, ExecutionId,
    ExternalStateAccess, ExternalStateContract, GuestPath,
};
use capsule::snapshot_manifest::SnapshotManifestV1;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Schema id of the Session Receipt's External-State record wire format.
pub const SESSION_STATE_RECEIPT_V1_SCHEMA: &str = "ato.session.external-state-receipt/v1";

// ---------------------------------------------------------------------------
// 1. Live-workload restore-time-binding requirement analysis (RFC §8.3)
// ---------------------------------------------------------------------------

/// Whether a `running` capture of this Capsule is ineligible because its **live
/// workload requires restore-time bindings** — either declared External State or
/// declared restore-time **secret bindings**.
///
/// RFC §8.3 fixes the two capture policies: `running` = "the workload requires
/// **no** External State to be live"; `workload_idle` = "the workload requires
/// External State **or restore-time bindings**". §17.3 restates the test
/// obligation: "`running` captures contain no required External State." §18
/// confirms the consequence: applications that need real External State "use
/// `workload_idle` or cold launch", not a running capture.
///
/// **Fail-closed reduction.** Two independent facets make a `running` capture
/// ineligible:
///
/// * any declared **External State binding** — state the live workload consumes;
///   access mode does not weaken this (a `read-only` binding is still a required
///   live attachment); and
/// * any declared **restore-time secret binding**
///   ([`ResolvedLaunchContract::secret_bindings`](capsule::execution_contract::ResolvedLaunchContract)).
///   A secret *value* is External State (RFC §9.1): production secrets MUST NOT be
///   attached before capture (§8.4), and there is **no** secret-bearing running
///   fallback (§8.3). A Capsule that declares secret bindings therefore requires
///   restore-time delivery and is ineligible for a `running` capture, even if it
///   declares no `external_state[]` volume.
///
/// `workload_idle` (the eligible policy for such Capsules) is an independent
/// lifecycle follow-up (#1093) and out of scope here; until it lands, such a build
/// MUST fail closed as ineligible and MUST NOT fall back to a secret-bearing
/// running capture (RFC §8.3).
#[must_use]
pub fn requires_restore_time_bindings_for_live_workload(contract: &ExecutionContractV1) -> bool {
    !contract.external_state.is_empty() || !contract.launch.secret_bindings.is_empty()
}

// ---------------------------------------------------------------------------
// 2. Snapshot exclusion boundary (RFC §9.2, §17.4)
// ---------------------------------------------------------------------------

/// The capture-time topology backing a Capsule's `snapshot = "exclude"` External
/// State: each excluded binding name mapped to the content address of the
/// **separate** writable volume that holds its bytes (RFC §9.2 "`/data` is a
/// separate writable boundary, not part of shared Snapshot layers"; §8.5
/// "excluded state paths are backed by separate volumes").
///
/// This is the structural capture fact a [`VerifiedExcludedStateBoundary`] is
/// built from. It is deliberately NOT itself an exclusion proof: an exclusion
/// guarantee is only ever the identity-bound boundary minted from a *verified*
/// contract plus this topology.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExcludedStateCaptureTopology {
    /// binding name → separate state-volume content address.
    state_volumes: BTreeMap<String, ContentDigest>,
}

impl ExcludedStateCaptureTopology {
    /// Build a topology from `(binding_name, separate_volume_address)` pairs — the
    /// separate volume that backs each `snapshot = "exclude"` binding at capture.
    #[must_use]
    pub fn new(volumes: impl IntoIterator<Item = (String, ContentDigest)>) -> Self {
        Self {
            state_volumes: volumes.into_iter().collect(),
        }
    }
}

/// Why a [`VerifiedExcludedStateBoundary`] could not be minted from a verified
/// contract + capture topology. Every variant fails closed: no boundary is
/// produced, so no exclusion claim can rest on a malformed capture.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExcludedStateBoundaryError {
    /// The Execution Contract envelope failed verification (its stored
    /// `execution_id` is not the canonical hash of the embedded contract), so no
    /// identity-bound boundary can be minted from it.
    #[error(
        "execution contract failed verification; cannot bind an External-State exclusion boundary"
    )]
    UnverifiedContract,
    /// A declared `snapshot = "exclude"` binding has no separate state volume in
    /// the capture topology: its bytes are not shown to be backed by a separate
    /// volume, so exclusion cannot be structurally set up.
    #[error(
        "excluded External State binding `{0}` has no separate state volume in the capture topology"
    )]
    MissingStateVolume(String),
    /// Two excluded bindings map to the SAME volume address — they are not the
    /// SEPARATE volumes §9.2 requires.
    #[error(
        "excluded External State bindings share one state volume address {0} — \
         each excluded binding must be a separate volume"
    )]
    SharedStateVolume(String),
    /// The capture topology names volumes for bindings the identity contract does
    /// not declare as External State — the boundary must cover exactly the declared
    /// excluded set, no more.
    #[error(
        "capture topology names {0} state volume(s) for bindings not declared as External State"
    )]
    UnknownStateVolume(usize),
}

/// A **proof-carrying, identity-bound** exclusion boundary: the set of content
/// addresses of the separate External-State volumes that MUST be excluded from
/// every shared Snapshot layer, bound to the verified Execution Identity it was
/// minted for.
///
/// **Why proof-carrying.** There is deliberately **no** public `new`/`Default`
/// that takes a caller-chosen digest set (its fields are private and it is not
/// `Deserialize`): an arbitrary or empty caller set bound to nothing was the hole
/// this type closes. The **only** constructor is
/// [`VerifiedExcludedStateBoundary::from_verified_capture`], which verifies the
/// Execution Contract, requires each declared `snapshot = "exclude"` binding to be
/// backed by its own separate state volume, and binds the boundary to the verified
/// `execution_id`. A boundary minted for contract A therefore cannot be applied to
/// a Snapshot manifest of Identity B.
///
/// **What is proven here — be honest.** This pure slice sets up §17.4 exclusion
/// *structurally*, it does not byte-prove it. What holds:
///
/// * production state is not attached pre-capture (enforced upstream by the
///   `running`-eligibility gate; External State / secret bindings make a running
///   capture ineligible);
/// * each excluded binding is backed by a **separate** volume (structural check
///   here); and
/// * the boundary is **bound to the verified Execution Identity** and refuses to
///   apply to any other manifest.
///
/// [`Self::ensure_absent_from_shared_layers`] additionally checks the excluded
/// volume addresses are not referenced verbatim as shared-layer refs. That detects
/// the state volume being listed as a shared layer; it does **not** prove state
/// bytes were never *copied into* a memory/vmstate/disk layer. The byte-level
/// CAS-closure disjointness proof (and trusted-backend attestation minting) is a
/// deliberate follow-up once live capture wiring lands; the proof TYPE shape is
/// defined now.
///
/// There is no public arbitrary-digest / `Default` constructor: the type is not
/// `Default` and its fields are private, so a caller cannot fabricate a boundary
/// over a chosen (or empty) digest set bound to nothing.
///
/// ```compile_fail
/// use snapshot::external_state::VerifiedExcludedStateBoundary;
/// // No `Default` derive and no public `new` taking a caller-chosen digest set:
/// // the only constructor is `from_verified_capture`.
/// let _ = VerifiedExcludedStateBoundary::default();
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedExcludedStateBoundary {
    /// The verified Execution Identity this boundary is bound to. Private, so a
    /// boundary can never be re-pointed at a foreign manifest.
    execution_id: ExecutionId,
    /// The separate excluded-state volume addresses to keep out of shared layers.
    volume_addresses: BTreeSet<ContentDigest>,
}

impl VerifiedExcludedStateBoundary {
    /// Mint an identity-bound exclusion boundary from a **verified** Execution
    /// Contract envelope and its capture topology — the only way to construct one.
    ///
    /// Steps, all fail-closed:
    ///
    /// 1. **Verify the contract** — [`ExecutionContractEnvelopeV1::verified_execution_id`]
    ///    recomputes the canonical hash and matches it against the stored id; a
    ///    disagreement yields [`ExcludedStateBoundaryError::UnverifiedContract`].
    /// 2. **Require a separate volume per excluded binding** — every
    ///    `snapshot = "exclude"` External State binding (v1 has the single
    ///    `exclude` variant, so all declared bindings are excluded) must have its
    ///    own distinct state-volume address in `topology`; a missing entry is
    ///    [`ExcludedStateBoundaryError::MissingStateVolume`], two bindings sharing
    ///    one address is [`ExcludedStateBoundaryError::SharedStateVolume`].
    /// 3. **Reject extraneous volumes** — a topology naming volumes for bindings the
    ///    contract does not declare is [`ExcludedStateBoundaryError::UnknownStateVolume`].
    /// 4. **Bind** the boundary to the verified `execution_id` from that same
    ///    contract.
    pub fn from_verified_capture(
        envelope: &ExecutionContractEnvelopeV1,
        topology: &ExcludedStateCaptureTopology,
    ) -> Result<Self, ExcludedStateBoundaryError> {
        let verified = envelope
            .verified_execution_id()
            .map_err(|_| ExcludedStateBoundaryError::UnverifiedContract)?;

        let mut volume_addresses = BTreeSet::new();
        for binding in &envelope.execution_contract.external_state {
            let address = topology.state_volumes.get(&binding.name).ok_or_else(|| {
                ExcludedStateBoundaryError::MissingStateVolume(binding.name.clone())
            })?;
            if !volume_addresses.insert(*address) {
                return Err(ExcludedStateBoundaryError::SharedStateVolume(
                    address.to_string(),
                ));
            }
        }
        // Every declared excluded binding resolved to a distinct key above, so the
        // topology has at least as many entries as bindings; any surplus names a
        // volume for a binding the contract never declared.
        let declared = envelope.execution_contract.external_state.len();
        if topology.state_volumes.len() > declared {
            return Err(ExcludedStateBoundaryError::UnknownStateVolume(
                topology.state_volumes.len() - declared,
            ));
        }

        Ok(Self {
            execution_id: verified.as_execution_id().clone(),
            volume_addresses,
        })
    }

    /// The verified Execution Identity this boundary is bound to.
    #[must_use]
    pub fn execution_id(&self) -> &ExecutionId {
        &self.execution_id
    }

    /// Whether the boundary excludes no addresses (a Capsule with no External
    /// State). Even an empty boundary is identity-bound: it still refuses to apply
    /// to a foreign manifest.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.volume_addresses.is_empty()
    }

    /// Whether `address` is one of the excluded External-State volume addresses.
    #[must_use]
    pub fn contains(&self, address: &ContentDigest) -> bool {
        self.volume_addresses.contains(address)
    }

    /// Assert that (a) `manifest` belongs to the SAME verified Execution Identity
    /// this boundary was bound to, and (b) no excluded External-State volume address
    /// appears in any shared Snapshot layer (memory / vmstate / disk).
    ///
    /// Fail-closed on the first breach. This is the STRUCTURAL check that §17.4 is
    /// set up (state volume separate and not referenced verbatim as a shared layer,
    /// boundary bound to its own identity) — **not** a byte-level disjointness proof
    /// (see the type doc): it cannot prove state bytes copied into a layer are
    /// absent.
    pub fn ensure_absent_from_shared_layers(
        &self,
        manifest: &SnapshotManifestV1,
    ) -> Result<(), ExclusionViolation> {
        // Identity binding: a boundary for Identity A can never be applied to a
        // manifest of Identity B.
        if manifest.execution_id != self.execution_id {
            return Err(ExclusionViolation::ExecutionIdentityMismatch {
                boundary: self.execution_id.to_string(),
                manifest: manifest.execution_id.to_string(),
            });
        }
        // An empty boundary can never be violated; skip the scan.
        if self.volume_addresses.is_empty() {
            return Ok(());
        }
        for (layer, refs) in [
            ("memory", &manifest.memory_layer_refs),
            ("vmstate", &manifest.vmstate_layer_refs),
            ("disk", &manifest.disk_layer_refs),
        ] {
            for address in refs {
                if self.volume_addresses.contains(address) {
                    return Err(ExclusionViolation::StateBytesInSharedLayer {
                        layer,
                        address: address.to_string(),
                    });
                }
            }
        }
        Ok(())
    }
}

/// A fail-closed exclusion violation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExclusionViolation {
    /// The boundary was applied to a manifest of a DIFFERENT Execution Identity
    /// than it was bound to — refused fail-closed (RFC §9.2 identity binding).
    #[error(
        "External State exclusion boundary bound to Execution Identity {boundary} cannot be \
         applied to a Snapshot manifest of Identity {manifest}"
    )]
    ExecutionIdentityMismatch { boundary: String, manifest: String },
    /// An excluded External-State volume address appears in a shared Snapshot
    /// layer — the `snapshot = "exclude"` boundary was breached and the candidate
    /// MUST be rejected.
    #[error(
        "excluded External State volume {address} appears in shared Snapshot layer `{layer}` \
         — snapshot=exclude bytes must be absent from every shared layer"
    )]
    StateBytesInSharedLayer {
        layer: &'static str,
        address: String,
    },
}

// ---------------------------------------------------------------------------
// 3. Opaque state reference (RFC §9.3)
// ---------------------------------------------------------------------------

/// Why a string is not a valid opaque External-State reference.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OpaqueStateRefError {
    /// The reference is not of the form `opaque:<handle>` with a non-empty
    /// handle. A raw path, a bare id, or an empty string is rejected so a Receipt
    /// can never accidentally carry a non-opaque (potentially content-bearing)
    /// reference.
    #[error("external state reference is not a non-empty `opaque:<handle>` reference")]
    NotOpaque,
    /// The handle exceeds the maximum opaque-handle length. A ref is a short,
    /// trusted-resolver-minted id, never a payload; an over-long value is refused.
    #[error("external state reference handle exceeds the maximum length")]
    TooLong,
    /// The handle is not the canonical, non-secret spelling: it must begin with a
    /// lowercase-ASCII alphanumeric and otherwise contain only `[a-z0-9._:-]`. This
    /// rejects control characters, whitespace, and upper-case "shouting" tokens
    /// like `SECRET-...` or `owner-...` in caps, so a secret- or authorization-
    /// shaped value can never masquerade as an opaque ref.
    #[error(
        "external state reference handle is not canonical: it must be a lowercase ASCII \
         [a-z0-9] first character followed by [a-z0-9._:-], with no control characters"
    )]
    NonCanonical,
}

/// An **opaque** reference to a concrete External State instance (RFC §9.3:
/// `state_ref = opaque:user-state-ref`).
///
/// It names *which* state without carrying any of its content or secret values.
/// It is a **trusted-resolver-minted, non-authorization, non-secret** id: it is
/// validated to a canonical `opaque:<handle>` spelling — bounded length, lowercase
/// ASCII `[a-z0-9]` + `[-._:]`, no control characters — on construction and
/// deserialization, so a non-opaque, over-long, secret-shaped, or control-char
/// value can never enter a Receipt. It is a non-identity value (RFC §4.3): it
/// never influences `execution_id`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OpaqueStateRef(String);

impl OpaqueStateRef {
    /// Prefix every opaque reference carries.
    const PREFIX: &'static str = "opaque:";
    /// The maximum accepted handle (post-prefix) length. A ref is a short id, not
    /// a payload.
    const MAX_HANDLE_LEN: usize = 128;

    /// Validate and wrap a `opaque:<handle>` reference.
    pub fn new(value: impl Into<String>) -> Result<Self, OpaqueStateRefError> {
        let value = value.into();
        let Some(handle) = value.strip_prefix(Self::PREFIX) else {
            return Err(OpaqueStateRefError::NotOpaque);
        };
        if handle.is_empty() {
            return Err(OpaqueStateRefError::NotOpaque);
        }
        if handle.len() > Self::MAX_HANDLE_LEN {
            return Err(OpaqueStateRefError::TooLong);
        }
        if !Self::is_canonical_handle(handle) {
            return Err(OpaqueStateRefError::NonCanonical);
        }
        Ok(Self(value))
    }

    /// A canonical handle is a trusted-resolver-minted, non-secret id: a lowercase
    /// ASCII `[a-z0-9]` first character followed by `[a-z0-9._:-]`. Rejects control
    /// characters, whitespace, and any upper-case token so a secret- or owner-
    /// shaped value cannot ride through as a ref.
    fn is_canonical_handle(handle: &str) -> bool {
        let mut chars = handle.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
            return false;
        }
        chars.all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_' | '.' | ':')
        })
    }

    /// A validation-only synthetic opaque reference for `binding` (RFC §8.4):
    /// disposable acceptance and build may attach only ephemeral synthetic
    /// bindings, never a real state ref. Used solely by
    /// [`SyntheticValidationStateInstance`].
    #[must_use]
    pub fn synthetic(binding: &str) -> Self {
        Self(format!("{}synthetic:{binding}", Self::PREFIX))
    }

    /// The canonical `opaque:<handle>` string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for OpaqueStateRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for OpaqueStateRef {
    type Error = OpaqueStateRefError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<OpaqueStateRef> for String {
    fn from(value: OpaqueStateRef) -> Self {
        value.0
    }
}

impl Serialize for OpaqueStateRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for OpaqueStateRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_from(value).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// 4. Concrete instance + schema-gated attach (RFC §9.2)
// ---------------------------------------------------------------------------

/// A concrete **production** External State instance presented at attach time.
///
/// Everything here **except** the schema identity is a non-identity infrastructure
/// fact (RFC §4.3): the owner id, the volume/binding instance id, the generation,
/// and the opaque ref never change `execution_id` and never enter shared
/// Snapshots. Critically, this type carries **no** data-byte or secret-value
/// field: the raw state bytes and secret values live only in the separate volume
/// the runner attaches, never in a value passed through this pure layer — so they
/// cannot leak into a lockfile, Snapshot, or Receipt via this path.
///
/// It is a DISTINCT type from [`SyntheticValidationStateInstance`]: only a
/// production instance is accepted by the production attach path
/// ([`plan_production_attach`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalStateInstance {
    /// Opaque handle naming which concrete state this is.
    pub state_ref: OpaqueStateRef,
    /// Monotonic generation marker of the concrete state (non-identity).
    pub generation: String,
    /// The concrete volume's schema identity, gated against the contract's.
    pub schema: String,
    /// The owning principal id (non-identity; never enters a Receipt).
    pub owner_id: String,
    /// The volume/binding instance id (non-identity; never enters a Receipt).
    pub volume_id: String,
}

/// A **synthetic, validation-only** External State instance (RFC §8.4).
///
/// It is a DISTINCT type from the production [`ExternalStateInstance`] so the
/// production attach path ([`plan_production_attach`]) cannot, at the type level,
/// be handed a synthetic instance — synthetic identity/binding is explicitly
/// validation-only. It is minted only by [`SyntheticValidationStateInstance::synthetic_for`]
/// and accepted only by [`plan_validation_attach`]. It carries no owner, volume,
/// data, or secret field at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntheticValidationStateInstance {
    state_ref: OpaqueStateRef,
    generation: String,
    schema: String,
}

impl SyntheticValidationStateInstance {
    /// A validation-only **synthetic ephemeral** instance conforming to
    /// `contract`'s declared schema (RFC §8.3 / §8.4 / §9.2: "build and acceptance
    /// use an empty or synthetic ephemeral volume").
    ///
    /// It connects **no** production owner, user state, secret, or Ato identity:
    /// the ref is a synthetic opaque handle and the generation is the literal
    /// `synthetic`. Because it conforms to the declared schema, it passes
    /// [`plan_validation_attach`] for disposable validation without ever touching
    /// real External State. (For a `running` capture the eligibility proof already
    /// requires an **empty** External State + secret-binding contract, so no attach
    /// happens on that path at all; this helper serves disposable validation and
    /// the future `workload_idle` lane.)
    #[must_use]
    pub fn synthetic_for(contract: &ExternalStateContract) -> Self {
        Self {
            state_ref: OpaqueStateRef::synthetic(&contract.name),
            generation: "synthetic".to_string(),
            schema: contract.schema.clone(),
        }
    }

    /// The synthetic volume's declared schema identity.
    #[must_use]
    pub fn schema(&self) -> &str {
        &self.schema
    }

    /// The synthetic opaque handle.
    #[must_use]
    pub fn state_ref(&self) -> &OpaqueStateRef {
        &self.state_ref
    }

    /// The synthetic generation marker (always `synthetic`).
    #[must_use]
    pub fn generation(&self) -> &str {
        &self.generation
    }
}

/// Why a concrete instance may not be attached to its contract binding.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExternalStateAttachError {
    /// The instance's schema identity is not compatible with the contract's
    /// identity-bearing schema. v1 does not migrate across schema identities, so
    /// this fails closed **before** any read-write attach (RFC §9.2:
    /// "incompatible schema fails before read-write attach").
    #[error(
        "External State schema incompatible: contract `{binding}` expects schema `{expected}`, \
         instance is schema `{found}` — refusing to attach"
    )]
    SchemaIncompatible {
        binding: String,
        expected: String,
        found: String,
    },
}

/// A sanctioned proof that a **compatible** External State instance was attached
/// to its contract binding.
///
/// It is minted **only** by [`plan_production_attach`] / [`plan_validation_attach`],
/// and only *after* the schema gate has passed — so its mere existence proves the
/// incompatible-schema path fails **before** any attachment is produced (RFC §9.2).
/// It carries only non-secret, non-content facts: the binding name and target and
/// access mode (from the identity-bearing contract), the matched schema identity
/// (compatibility evidence), and the opaque ref + generation of the concrete
/// instance. It never carries owner id, volume id, data bytes, or secret values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalStateAttachment {
    binding_name: String,
    target: GuestPath,
    access: ExternalStateAccess,
    schema_identity: String,
    state_ref: OpaqueStateRef,
    generation: String,
}

impl ExternalStateAttachment {
    /// The identity-bearing binding name.
    #[must_use]
    pub fn binding_name(&self) -> &str {
        &self.binding_name
    }

    /// The identity-bearing mount/injection target.
    #[must_use]
    pub fn target(&self) -> &GuestPath {
        &self.target
    }

    /// The identity-bearing access mode.
    #[must_use]
    pub fn access(&self) -> ExternalStateAccess {
        self.access
    }

    /// The matched schema identity (non-secret compatibility evidence).
    #[must_use]
    pub fn schema_identity(&self) -> &str {
        &self.schema_identity
    }

    /// The opaque reference to the attached concrete state.
    #[must_use]
    pub fn state_ref(&self) -> &OpaqueStateRef {
        &self.state_ref
    }

    /// The attached state's generation (non-identity).
    #[must_use]
    pub fn generation(&self) -> &str {
        &self.generation
    }

    /// Produce the Session Receipt's External-State record for this attachment —
    /// the **only** sanctioned way state facts reach a Receipt. It copies solely
    /// the opaque ref, the generation, and the non-secret compatibility evidence;
    /// there is structurally no owner/volume/content/secret field to copy (RFC
    /// §9.3, §12, §14).
    #[must_use]
    pub fn session_receipt(&self) -> SessionStateReceiptV1 {
        SessionStateReceiptV1 {
            schema: SESSION_STATE_RECEIPT_V1_SCHEMA.to_string(),
            binding_name: self.binding_name.clone(),
            target: self.target.clone(),
            access: self.access,
            schema_identity: self.schema_identity.clone(),
            state_ref: self.state_ref.clone(),
            state_generation: self.generation.clone(),
        }
    }
}

/// Plan the attach of a **production** instance to its contract binding, running
/// the **schema gate before attach** (RFC §9.2). Fail closed with
/// [`ExternalStateAttachError::SchemaIncompatible`] when the instance's schema
/// identity does not match the contract's identity-bearing schema — before any
/// [`ExternalStateAttachment`] is produced, so no read-write attach can proceed on
/// an incompatible schema. On success, mints the sanctioned attachment.
///
/// This path takes an [`ExternalStateInstance`] only: a
/// [`SyntheticValidationStateInstance`] is a distinct type and cannot be passed
/// here, so a validation-only instance can never drive a production attach.
///
/// ```compile_fail
/// use capsule::execution_contract::ExternalStateContract;
/// use snapshot::external_state::{plan_production_attach, SyntheticValidationStateInstance};
///
/// fn demo(contract: &ExternalStateContract, synthetic: &SyntheticValidationStateInstance) {
///     // `plan_production_attach` takes `&ExternalStateInstance`; a synthetic
///     // validation instance is a distinct type, so this fails to compile.
///     let _ = plan_production_attach(contract, synthetic);
/// }
/// ```
pub fn plan_production_attach(
    contract: &ExternalStateContract,
    instance: &ExternalStateInstance,
) -> Result<ExternalStateAttachment, ExternalStateAttachError> {
    plan_attach_inner(
        contract,
        &instance.schema,
        &instance.state_ref,
        &instance.generation,
    )
}

/// Plan the attach of a **synthetic validation** instance to its contract binding,
/// running the same schema gate before attach (RFC §9.2 / §8.4). This is the only
/// path that accepts a [`SyntheticValidationStateInstance`]; production attaches go
/// through [`plan_production_attach`].
pub fn plan_validation_attach(
    contract: &ExternalStateContract,
    instance: &SyntheticValidationStateInstance,
) -> Result<ExternalStateAttachment, ExternalStateAttachError> {
    plan_attach_inner(
        contract,
        &instance.schema,
        &instance.state_ref,
        &instance.generation,
    )
}

fn plan_attach_inner(
    contract: &ExternalStateContract,
    instance_schema: &str,
    state_ref: &OpaqueStateRef,
    generation: &str,
) -> Result<ExternalStateAttachment, ExternalStateAttachError> {
    if contract.schema != instance_schema {
        return Err(ExternalStateAttachError::SchemaIncompatible {
            binding: contract.name.clone(),
            expected: contract.schema.clone(),
            found: instance_schema.to_string(),
        });
    }
    Ok(ExternalStateAttachment {
        binding_name: contract.name.clone(),
        target: contract.target.clone(),
        access: contract.access,
        schema_identity: contract.schema.clone(),
        state_ref: state_ref.clone(),
        generation: generation.to_string(),
    })
}

// ---------------------------------------------------------------------------
// 5. Session Receipt External-State record (RFC §9.3, §12)
// ---------------------------------------------------------------------------

/// Why a deserialized [`SessionStateReceiptV1`] failed its consumer-boundary
/// integrity check.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SessionStateReceiptError {
    /// The receipt JSON is not valid for the v1 wire form — e.g. it carries an
    /// unknown field (`deny_unknown_fields` rejects an owner/secret/volume field),
    /// a non-opaque `state_ref`, or a malformed `target`.
    #[error("session state receipt is not a valid v1 wire record: {0}")]
    Malformed(String),
    /// The receipt's `schema` is not [`SESSION_STATE_RECEIPT_V1_SCHEMA`].
    #[error("session state receipt schema is not the supported v1 schema")]
    UnsupportedSchema,
}

/// The Session Receipt's record of one attached External State binding.
///
/// Records **only** an opaque state reference, the state generation, and
/// non-secret compatibility evidence (binding name, target, access mode, and the
/// matched schema identity). It never carries content, data bytes, secret values,
/// identity assertions, the owner id, or the volume instance id — there is
/// structurally no field for any of those, and `deny_unknown_fields` refuses a
/// wire record that tries to smuggle one in (RFC §9.3, §12 "Receipts MUST redact
/// secret values and identity assertions", §14). The generation is a recorded fact
/// that does not change `execution_id` (RFC §9.3).
///
/// Its fields are **private**: a receipt is minted only via
/// [`ExternalStateAttachment::session_receipt`] (fail-closed by construction) and,
/// for a receipt read off the wire, parsed via [`SessionStateReceiptV1::parse`],
/// which enforces the schema discriminator on top of serde's fail-closed decode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionStateReceiptV1 {
    /// Always [`SESSION_STATE_RECEIPT_V1_SCHEMA`].
    schema: String,
    /// The identity-bearing binding name.
    binding_name: String,
    /// The identity-bearing mount/injection target.
    target: GuestPath,
    /// The identity-bearing access mode.
    access: ExternalStateAccess,
    /// Non-secret compatibility evidence: the schema identity the instance was
    /// gated against.
    schema_identity: String,
    /// Opaque handle — names which state, carries none of its content or secrets.
    state_ref: OpaqueStateRef,
    /// Non-identity generation marker (RFC §9.3: "state generation changes do not
    /// change `execution_id`").
    state_generation: String,
}

impl SessionStateReceiptV1 {
    /// Parse + validate a receipt from JSON, fail-closed. `deny_unknown_fields`
    /// already rejects a receipt carrying an `owner_id` / `secret` / `volume_id`
    /// field, a non-opaque `state_ref`, or a malformed `target` at deserialize;
    /// this additionally enforces the schema discriminator. This is the sanctioned
    /// entry for a receipt a consumer did not itself mint.
    pub fn parse(json: &str) -> Result<Self, SessionStateReceiptError> {
        let receipt: Self = serde_json::from_str(json)
            .map_err(|error| SessionStateReceiptError::Malformed(error.to_string()))?;
        receipt.validate()?;
        Ok(receipt)
    }

    /// Enforce the schema discriminator. The typed fields (opaque-validated
    /// `state_ref`, canonical `GuestPath` `target`, enum `access`) already fail
    /// closed at deserialize; this covers the string-valued `schema` serde cannot.
    pub fn validate(&self) -> Result<(), SessionStateReceiptError> {
        if self.schema != SESSION_STATE_RECEIPT_V1_SCHEMA {
            return Err(SessionStateReceiptError::UnsupportedSchema);
        }
        Ok(())
    }

    /// The schema discriminator (always [`SESSION_STATE_RECEIPT_V1_SCHEMA`] once
    /// validated).
    #[must_use]
    pub fn schema(&self) -> &str {
        &self.schema
    }

    /// The identity-bearing binding name.
    #[must_use]
    pub fn binding_name(&self) -> &str {
        &self.binding_name
    }

    /// The identity-bearing mount/injection target.
    #[must_use]
    pub fn target(&self) -> &GuestPath {
        &self.target
    }

    /// The identity-bearing access mode.
    #[must_use]
    pub fn access(&self) -> ExternalStateAccess {
        self.access
    }

    /// Non-secret compatibility evidence: the matched schema identity.
    #[must_use]
    pub fn schema_identity(&self) -> &str {
        &self.schema_identity
    }

    /// The opaque state reference (names which state, carries no content/secret).
    #[must_use]
    pub fn state_ref(&self) -> &OpaqueStateRef {
        &self.state_ref
    }

    /// The non-identity generation marker.
    #[must_use]
    pub fn state_generation(&self) -> &str {
        &self.state_generation
    }
}

#[cfg(test)]
mod tests {
    use capsule::execution_contract::DigestAlgorithm;

    use super::*;

    fn digest(byte: u8) -> ContentDigest {
        ContentDigest::new(DigestAlgorithm::Blake3, [byte; 32])
    }

    fn guest_path(value: &str) -> GuestPath {
        GuestPath::parse(value).expect("canonical guest path")
    }

    fn state_contract(schema: &str, access: ExternalStateAccess) -> ExternalStateContract {
        ExternalStateContract {
            name: "data".to_string(),
            target: guest_path("/data"),
            access,
            schema: schema.to_string(),
            snapshot: capsule::execution_contract::SnapshotExclusion::Exclude,
        }
    }

    fn instance(schema: &str) -> ExternalStateInstance {
        ExternalStateInstance {
            state_ref: OpaqueStateRef::new("opaque:user-state-ref").unwrap(),
            generation: "gen_456".to_string(),
            schema: schema.to_string(),
            owner_id: "user-123".to_string(),
            volume_id: "vol-789".to_string(),
        }
    }

    // --- Opaque ref: canonical, bounded, non-secret `opaque:<handle>` only ---
    #[test]
    fn opaque_state_ref_enforces_canonical_non_secret_handle() {
        assert!(OpaqueStateRef::new("opaque:user-state-ref").is_ok());
        // Not opaque / empty handle.
        assert_eq!(
            OpaqueStateRef::new("/data/user").unwrap_err(),
            OpaqueStateRefError::NotOpaque
        );
        assert_eq!(
            OpaqueStateRef::new("opaque:").unwrap_err(),
            OpaqueStateRefError::NotOpaque
        );
        assert_eq!(
            OpaqueStateRef::new("").unwrap_err(),
            OpaqueStateRefError::NotOpaque
        );
        // Secret-/authorization-shaped (upper-case) tokens are non-canonical.
        assert_eq!(
            OpaqueStateRef::new("opaque:SECRET-owner-token").unwrap_err(),
            OpaqueStateRefError::NonCanonical
        );
        // Control characters are non-canonical.
        assert_eq!(
            OpaqueStateRef::new("opaque:user\u{7f}ref").unwrap_err(),
            OpaqueStateRefError::NonCanonical
        );
        assert_eq!(
            OpaqueStateRef::new("opaque:has space").unwrap_err(),
            OpaqueStateRefError::NonCanonical
        );
        // Over-length handles are rejected as a payload, not an id.
        let too_long = format!("opaque:{}", "a".repeat(OpaqueStateRef::MAX_HANDLE_LEN + 1));
        assert_eq!(
            OpaqueStateRef::new(too_long).unwrap_err(),
            OpaqueStateRefError::TooLong
        );
        // Round-trips through serde in its canonical spelling; a non-opaque or
        // control-char wire value fails closed at deserialize.
        let json = serde_json::to_string(&OpaqueStateRef::new("opaque:x").unwrap()).unwrap();
        assert_eq!(json, "\"opaque:x\"");
        assert!(serde_json::from_str::<OpaqueStateRef>("\"raw-secret\"").is_err());
        assert!(serde_json::from_str::<OpaqueStateRef>("\"opaque:SECRET\"").is_err());
    }

    // --- AC (17.4): a compatible schema attaches successfully (production path) ---
    #[test]
    fn compatible_schema_attaches() {
        let contract = state_contract("1", ExternalStateAccess::ReadWrite);
        let attachment =
            plan_production_attach(&contract, &instance("1")).expect("compatible schema attaches");
        assert_eq!(attachment.binding_name(), "data");
        assert_eq!(attachment.access(), ExternalStateAccess::ReadWrite);
        assert_eq!(attachment.schema_identity(), "1");
        assert_eq!(attachment.state_ref().as_str(), "opaque:user-state-ref");
        assert_eq!(attachment.generation(), "gen_456");
    }

    // --- AC (17.4): incompatible schema fails BEFORE read-write attach ---
    #[test]
    fn incompatible_schema_fails_before_attach() {
        let contract = state_contract("1", ExternalStateAccess::ReadWrite);
        let error = plan_production_attach(&contract, &instance("2")).unwrap_err();
        assert_eq!(
            error,
            ExternalStateAttachError::SchemaIncompatible {
                binding: "data".to_string(),
                expected: "1".to_string(),
                found: "2".to_string(),
            }
        );
        // Fail CLOSED: no attachment was produced, so nothing downstream can
        // mistake this for an attached read-write volume.
        assert!(plan_production_attach(&contract, &instance("2")).is_err());
    }

    // --- Major 1: synthetic validation instance is a DISTINCT type and only the
    // validation attach path accepts it ---
    #[test]
    fn synthetic_validation_instance_conforms_via_validation_path_only() {
        let contract = state_contract("1", ExternalStateAccess::ReadWrite);
        let synthetic = SyntheticValidationStateInstance::synthetic_for(&contract);
        assert_eq!(synthetic.generation(), "synthetic");
        assert!(
            synthetic
                .state_ref()
                .as_str()
                .starts_with("opaque:synthetic:")
        );
        // It conforms to the declared schema, so the VALIDATION path attaches it.
        let attachment =
            plan_validation_attach(&contract, &synthetic).expect("synthetic conforms to schema");
        assert_eq!(attachment.schema_identity(), "1");
        // A synthetic instance whose schema differs still fails the gate.
        let other = state_contract("2", ExternalStateAccess::ReadWrite);
        assert!(plan_validation_attach(&other, &synthetic).is_err());
        // The production attach path cannot take a synthetic instance at all — see
        // the `compile_fail` doctest on `plan_production_attach`.
    }

    // --- AC (17.4): Receipt records opaque ref + generation, never data/secrets ---
    #[test]
    fn receipt_records_opaque_ref_and_generation_without_data_or_secrets() {
        let contract = state_contract("1", ExternalStateAccess::ReadWrite);
        let inst = ExternalStateInstance {
            // Deliberately hostile owner/volume values: they must NOT reach the
            // receipt. There is no content/secret field on the instance at all.
            owner_id: "SECRET-owner-token".to_string(),
            volume_id: "SECRET-volume-key".to_string(),
            ..instance("1")
        };
        let receipt = plan_production_attach(&contract, &inst)
            .unwrap()
            .session_receipt();

        assert_eq!(receipt.schema(), SESSION_STATE_RECEIPT_V1_SCHEMA);
        assert_eq!(receipt.state_ref().as_str(), "opaque:user-state-ref");
        assert_eq!(receipt.state_generation(), "gen_456");
        assert_eq!(receipt.schema_identity(), "1");

        // The owner id, the volume id, and any secret value are absent from the
        // serialized receipt — structurally, there is no field to hold them.
        let json = serde_json::to_string(&receipt).unwrap();
        assert!(!json.contains("SECRET-owner-token"));
        assert!(!json.contains("SECRET-volume-key"));
        assert!(!json.contains("owner"));
        assert!(!json.contains("volume_id"));

        // The receipt round-trips through its typed, opaque-validated wire form and
        // its parse() consumer boundary.
        let parsed = SessionStateReceiptV1::parse(&json).expect("valid v1 receipt");
        assert_eq!(parsed, receipt);
    }

    // --- Major 2: a receipt carrying an unknown (secret/owner) field is REJECTED
    // at deserialize; a wrong schema is rejected at parse ---
    #[test]
    fn receipt_wire_is_fail_closed() {
        let contract = state_contract("1", ExternalStateAccess::ReadWrite);
        let receipt = plan_production_attach(&contract, &instance("1"))
            .unwrap()
            .session_receipt();
        let json = serde_json::to_string(&receipt).unwrap();

        // A receipt that smuggles an owner_id / secret field is rejected at
        // deserialize by deny_unknown_fields.
        for bad_field in [r#""owner_id":"u""#, r#""secret":"s""#, r#""volume_id":"v""#] {
            let tampered = json.replacen('{', &format!("{{{bad_field},"), 1);
            assert!(
                SessionStateReceiptV1::parse(&tampered).is_err(),
                "receipt with unknown field must be rejected: {bad_field}"
            );
            assert!(serde_json::from_str::<SessionStateReceiptV1>(&tampered).is_err());
        }

        // A receipt with the wrong schema discriminator is rejected at parse.
        let wrong_schema = json.replace(
            SESSION_STATE_RECEIPT_V1_SCHEMA,
            "ato.session.external-state-receipt/v2",
        );
        assert_eq!(
            SessionStateReceiptV1::parse(&wrong_schema).unwrap_err(),
            SessionStateReceiptError::UnsupportedSchema
        );
    }

    // --- Blocker 1 (17.3/8.3): External State OR restore-time secret bindings make
    // a live `running` workload ineligible — the 4 quadrants ---
    #[test]
    fn requires_restore_time_bindings_four_quadrants() {
        let mut contract = crate::contract_fixtures::sample_execution_contract();
        // The G0-1 sample contract declares one External State binding AND one
        // restore-time secret binding.
        assert!(!contract.external_state.is_empty());
        assert!(!contract.launch.secret_bindings.is_empty());

        // (both present) -> reject.
        assert!(requires_restore_time_bindings_for_live_workload(&contract));

        // (external present & no secret) -> reject.
        contract.launch.secret_bindings.clear();
        assert!(!contract.external_state.is_empty());
        assert!(contract.launch.secret_bindings.is_empty());
        assert!(requires_restore_time_bindings_for_live_workload(&contract));

        // (no external & secret present) -> reject.
        let mut with_secret = crate::contract_fixtures::sample_execution_contract();
        with_secret.external_state.clear();
        assert!(!with_secret.launch.secret_bindings.is_empty());
        assert!(requires_restore_time_bindings_for_live_workload(
            &with_secret
        ));

        // (both empty) -> eligible.
        contract.external_state.clear();
        assert!(contract.external_state.is_empty());
        assert!(contract.launch.secret_bindings.is_empty());
        assert!(!requires_restore_time_bindings_for_live_workload(&contract));

        // A read-only External State binding is still a required live attachment.
        let mut read_only = crate::contract_fixtures::sample_execution_contract();
        read_only.launch.secret_bindings.clear();
        read_only.external_state[0].access = ExternalStateAccess::ReadOnly;
        assert!(requires_restore_time_bindings_for_live_workload(&read_only));
    }

    // --- AC (17.1): mutating any External State CONTRACT facet changes execution_id ---
    #[test]
    fn state_contract_facet_mutations_change_execution_id() {
        let base = crate::contract_fixtures::sample_execution_contract();
        let base_id = base.compute_execution_id().expect("valid contract hashes");

        type Mutation = (&'static str, fn(&mut ExternalStateContract));
        let mutations: [Mutation; 4] = [
            ("name", |state| state.name = "store".to_string()),
            ("target", |state| {
                state.target = GuestPath::parse("/var/data").expect("canonical path");
            }),
            ("access", |state| {
                state.access = ExternalStateAccess::ReadOnly
            }),
            ("schema", |state| state.schema = "2".to_string()),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for (facet, mutate) in mutations {
            let mut mutated = base.clone();
            mutate(&mut mutated.external_state[0]);
            let id = mutated
                .compute_execution_id()
                .expect("valid contract hashes");
            assert_ne!(
                id, base_id,
                "mutating external_state.{facet} must change id"
            );
            // Pairwise-distinct: no two facet mutations collide onto the same id.
            assert!(
                seen.insert(id.to_string()),
                "external_state.{facet} id collides"
            );
        }
        // `snapshot` is identity-bearing too, but `SnapshotExclusion` has a single
        // v1 variant (`exclude`), so it cannot actually differ under v1.
    }

    // --- AC (17.1/4.3): instance/owner/generation facets do NOT change execution_id ---
    #[test]
    fn state_instance_facets_do_not_change_execution_id() {
        let contract = crate::contract_fixtures::sample_execution_contract();
        let base_id = contract
            .compute_execution_id()
            .expect("valid contract hashes");
        let binding = &contract.external_state[0];

        // Two different concrete instances of the SAME binding: different owner,
        // volume id, generation, and opaque ref. None is part of the identity
        // contract, so it can never have been an input to the id.
        let inst_a = ExternalStateInstance {
            owner_id: "owner-a".to_string(),
            volume_id: "vol-a".to_string(),
            generation: "gen_1".to_string(),
            ..instance("1")
        };
        let inst_b = ExternalStateInstance {
            owner_id: "owner-b".to_string(),
            volume_id: "vol-b".to_string(),
            generation: "gen_2".to_string(),
            state_ref: OpaqueStateRef::new("opaque:other-ref").unwrap(),
            ..instance("1")
        };
        let receipt_a = plan_production_attach(binding, &inst_a)
            .unwrap()
            .session_receipt();
        let receipt_b = plan_production_attach(binding, &inst_b)
            .unwrap()
            .session_receipt();

        // The receipts differ only in non-identity generation / ref facets...
        assert_ne!(receipt_a.state_generation(), receipt_b.state_generation());
        assert_ne!(receipt_a.state_ref(), receipt_b.state_ref());
        // ...while the schema-identity compatibility evidence is identical...
        assert_eq!(receipt_a.schema_identity(), receipt_b.schema_identity());
        // ...and the execution_id is unchanged regardless of which instance
        // attached (it never took the instance as input).
        assert_eq!(
            contract
                .compute_execution_id()
                .expect("valid contract hashes"),
            base_id
        );
    }

    // --- AC (4.3): instance facets are rejected as unknown fields of the identity contract ---
    #[test]
    fn external_state_contract_rejects_instance_facets_as_unknown_fields() {
        // owner / volume id / generation / data bytes / secret are NOT identity
        // fields: an execution contract that tries to smuggle one into an
        // external_state entry fails closed at deserialize (`deny_unknown_fields`),
        // so a concrete-instance fact can never ride into the JCS-hashed identity.
        for bad_key in ["owner", "volume_id", "generation", "data", "secret"] {
            let json = format!(
                r#"{{"name":"data","target":"/data","access":"read-write","schema":"1","snapshot":"exclude","{bad_key}":"x"}}"#
            );
            assert!(
                serde_json::from_str::<ExternalStateContract>(&json).is_err(),
                "instance facet `{bad_key}` must be rejected from the identity contract"
            );
        }
        // The exact five identity facets parse.
        let ok = r#"{"name":"data","target":"/data","access":"read-write","schema":"1","snapshot":"exclude"}"#;
        assert!(serde_json::from_str::<ExternalStateContract>(ok).is_ok());
    }

    /// A verified, identity-bound boundary for the sample contract, plus a manifest
    /// carrying the SAME Execution Identity so the boundary can be applied to it.
    /// `volume` is the separate state-volume address backing the sample's single
    /// `data` binding.
    fn verified_boundary_and_manifest(
        volume: ContentDigest,
    ) -> (VerifiedExcludedStateBoundary, SnapshotManifestV1) {
        let contract = crate::contract_fixtures::sample_execution_contract();
        let execution_id = contract
            .compute_execution_id()
            .expect("valid contract hashes");
        let envelope = crate::contract_fixtures::envelope_for(contract);
        let topology = ExcludedStateCaptureTopology::new([("data".to_string(), volume)]);
        let boundary = VerifiedExcludedStateBoundary::from_verified_capture(&envelope, &topology)
            .expect("verified contract + separate-volume topology mints a boundary");
        let manifest = SnapshotManifestV1 {
            execution_id,
            ..crate::contract_fixtures::sample_snapshot_manifest()
        };
        (boundary, manifest)
    }

    // --- AC (17.4): excluded state bytes are absent from every shared Snapshot layer ---
    #[test]
    fn ensure_excluded_state_absent_scans_all_shared_layers() {
        let separate = digest(0x99);
        let (boundary, manifest) = verified_boundary_and_manifest(separate);
        assert!(boundary.contains(&separate));

        // The separate state-volume address is absent from every shared layer.
        assert!(!manifest.memory_layer_refs.contains(&separate));
        boundary
            .ensure_absent_from_shared_layers(&manifest)
            .expect("a separate volume address is absent from shared layers");

        // If the excluded state volume leaks into ANY shared layer, it fails
        // closed — checked for memory, vmstate, and disk independently.
        for layer in ["memory", "vmstate", "disk"] {
            let mut leaked = manifest.clone();
            match layer {
                "memory" => leaked.memory_layer_refs = vec![separate],
                "vmstate" => leaked.vmstate_layer_refs = vec![separate],
                _ => leaked.disk_layer_refs = vec![separate],
            }
            let err = boundary
                .ensure_absent_from_shared_layers(&leaked)
                .unwrap_err();
            assert_eq!(
                err,
                ExclusionViolation::StateBytesInSharedLayer {
                    layer,
                    address: separate.to_string(),
                }
            );
        }
    }

    // --- Blocker 2: the boundary is identity-bound — a boundary for Identity A
    // rejects a manifest of Identity B ---
    #[test]
    fn boundary_rejects_a_manifest_of_a_different_identity() {
        let (boundary, _manifest) = verified_boundary_and_manifest(digest(0x99));
        // A manifest with a DIFFERENT execution_id (the sample manifest's hardcoded
        // id) cannot be checked against this boundary at all.
        let foreign = crate::contract_fixtures::sample_snapshot_manifest();
        assert_ne!(&foreign.execution_id, boundary.execution_id());
        let err = boundary
            .ensure_absent_from_shared_layers(&foreign)
            .unwrap_err();
        assert!(matches!(
            err,
            ExclusionViolation::ExecutionIdentityMismatch { .. }
        ));
    }

    // --- Blocker 2: an EMPTY boundary is still identity-bound and cannot be
    // applied to a foreign manifest ---
    #[test]
    fn empty_boundary_is_identity_bound() {
        // A contract with no External State yields an empty boundary...
        let contract = {
            let mut contract = crate::contract_fixtures::sample_execution_contract();
            contract.external_state.clear();
            contract
        };
        let execution_id = contract
            .compute_execution_id()
            .expect("valid contract hashes");
        let envelope = crate::contract_fixtures::envelope_for(contract);
        let boundary = VerifiedExcludedStateBoundary::from_verified_capture(
            &envelope,
            &ExcludedStateCaptureTopology::default(),
        )
        .expect("no external state → empty but identity-bound boundary");
        assert!(boundary.is_empty());

        // ...which trivially passes for its OWN manifest...
        let own = SnapshotManifestV1 {
            execution_id,
            ..crate::contract_fixtures::sample_snapshot_manifest()
        };
        boundary
            .ensure_absent_from_shared_layers(&own)
            .expect("empty boundary passes for its own identity");

        // ...but is refused for a foreign-identity manifest — an empty boundary is
        // not a wildcard.
        let foreign = crate::contract_fixtures::sample_snapshot_manifest();
        assert_ne!(&foreign.execution_id, boundary.execution_id());
        assert!(matches!(
            boundary
                .ensure_absent_from_shared_layers(&foreign)
                .unwrap_err(),
            ExclusionViolation::ExecutionIdentityMismatch { .. }
        ));
    }

    // --- Blocker 2: structural checks — a separate volume per excluded binding is
    // required; missing / shared / extraneous volumes fail closed ---
    #[test]
    fn boundary_requires_a_separate_volume_per_excluded_binding() {
        let contract = crate::contract_fixtures::sample_execution_contract();
        let envelope = crate::contract_fixtures::envelope_for(contract);

        // Missing: the `data` binding has no state volume in the topology.
        let empty = ExcludedStateCaptureTopology::default();
        assert_eq!(
            VerifiedExcludedStateBoundary::from_verified_capture(&envelope, &empty).unwrap_err(),
            ExcludedStateBoundaryError::MissingStateVolume("data".to_string())
        );

        // Extraneous: a topology naming a volume for a binding the contract does
        // not declare.
        let extra = ExcludedStateCaptureTopology::new([
            ("data".to_string(), digest(0x99)),
            ("ghost".to_string(), digest(0xaa)),
        ]);
        assert_eq!(
            VerifiedExcludedStateBoundary::from_verified_capture(&envelope, &extra).unwrap_err(),
            ExcludedStateBoundaryError::UnknownStateVolume(1)
        );
    }

    // --- Blocker 2: two excluded bindings that share one volume are NOT separate ---
    #[test]
    fn boundary_rejects_two_bindings_sharing_one_volume() {
        // A contract with two excluded bindings, both pointed at the SAME volume.
        // `external_state` is a canonical (sorted) list, so append `store` (> `data`).
        let mut contract = crate::contract_fixtures::sample_execution_contract();
        contract.external_state.push(ExternalStateContract {
            name: "store".to_string(),
            target: guest_path("/store"),
            access: ExternalStateAccess::ReadWrite,
            schema: "1".to_string(),
            snapshot: capsule::execution_contract::SnapshotExclusion::Exclude,
        });
        let envelope = crate::contract_fixtures::envelope_for(contract);
        let shared = digest(0x99);
        let topology = ExcludedStateCaptureTopology::new([
            ("data".to_string(), shared),
            ("store".to_string(), shared),
        ]);
        assert_eq!(
            VerifiedExcludedStateBoundary::from_verified_capture(&envelope, &topology).unwrap_err(),
            ExcludedStateBoundaryError::SharedStateVolume(shared.to_string())
        );
    }

    // --- Blocker 2: a boundary cannot be minted from an UNVERIFIED contract ---
    #[test]
    fn boundary_cannot_be_minted_from_an_unverified_contract() {
        let contract = crate::contract_fixtures::sample_execution_contract();
        let mut envelope = crate::contract_fixtures::envelope_for(contract);
        // Tamper the stored id so it no longer equals the contract's canonical hash.
        envelope.execution_id =
            ExecutionId::new(format!("blake3:{}", "e".repeat(64))).expect("valid id shape");
        let topology = ExcludedStateCaptureTopology::new([("data".to_string(), digest(0x99))]);
        assert_eq!(
            VerifiedExcludedStateBoundary::from_verified_capture(&envelope, &topology).unwrap_err(),
            ExcludedStateBoundaryError::UnverifiedContract
        );
    }
}
