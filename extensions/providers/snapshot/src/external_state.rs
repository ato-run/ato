//! External State exclusion boundary, schema gate, and receipt boundary
//! (issue #1090, Gate-0 style: pure, deterministic, no live wiring).
//!
//! External State is mutable or principal-specific state attached to a Session —
//! user data, persistent app data, secret/API-key values, OAuth tokens, Ato
//! identity, concrete database/service bindings (RFC
//! `docs/rfcs/archived/CAPSULE_V1_EXECUTION_MODEL_SPEC.md` §9.1). Capsule v1 makes
//! it a **structurally separate runtime attachment** whose *schema contract* is
//! identity-bearing while its concrete *instance* and *values* are not.
//!
//! This module holds the pure pieces #1090 owns (the RFC names it directly at
//! §"References" → `crates/snapshot/src/external_state.rs`):
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
//!    binding is backed by a **separate** volume, so its bytes MUST be absent from
//!    every shared Snapshot layer (memory / vmstate / disk). The proof-carrying,
//!    identity-bound [`VerifiedCaptureTopology`] asserts the STRUCTURAL guarantees
//!    (§17.4 is *structurally set up* here, not byte-proven — see the type doc),
//!    judging volume separateness by a **mount-boundary id**, not a content digest.
//! 3. **Schema gate + attach + receipt boundary** (§9.2, §9.3): a production
//!    attachment can be minted only from a [`VerifiedExternalStateBinding`] proven
//!    against the current verified Execution Contract; an incompatible state schema
//!    fails **before** the read-write attach; and the Session Receipt records only
//!    an *opaque* state reference + generation + non-secret compatibility evidence,
//!    plus the verified `execution_id` as opaque identity evidence — never content,
//!    secret values, owner, or instance id.
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
use capsule::snapshot_manifest::{SnapshotId, SnapshotManifestV1};
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

/// The identity of a capture-time **mount boundary** — the mount source / volume
/// instance a `snapshot = "exclude"` binding is backed by at capture.
///
/// This is deliberately **not** a [`ContentDigest`]. A content digest identifies
/// *bytes*; a mount-boundary id identifies a *volume instance* / mount source.
/// Two distinct empty volumes share one content digest (they hold identical
/// bytes) yet are separate mounts; one mutable volume across generations has
/// different content digests yet is the same mount. Volume *separateness* is
/// therefore a property of the mount boundary, never of the bytes — so it is the
/// [`CaptureMountId`] (not the digest) that decides whether two excluded bindings
/// occupy separate volumes.
///
/// A mount-boundary id is supplied by the trusted capture backend that actually
/// placed each excluded volume; this pure slice models it as an opaque identifier.
/// Even a trusted-backend id is **validated on construction** (non-empty, bounded,
/// canonical, control-char-free) so an in-crate mis-implementation cannot promote a
/// malformed identifier into a proof.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CaptureMountId(String);

/// Why a string is not a valid capture-time mount-boundary id.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CaptureMountIdError {
    /// The id is empty. An empty mount boundary is never a real volume instance and
    /// would collapse separateness, so it is refused fail-closed.
    #[error("capture mount id is empty")]
    Empty,
    /// The id exceeds the maximum mount-boundary length. A mount id names a volume
    /// instance, never a payload; an over-long value is refused.
    #[error("capture mount id exceeds the maximum length")]
    TooLong,
    /// The id is not the canonical spelling: it must begin with a lowercase-ASCII
    /// alphanumeric and otherwise contain only `[a-z0-9._:/-]`. This rejects control
    /// characters, whitespace, and upper-case tokens.
    #[error(
        "capture mount id is not canonical: it must be a lowercase ASCII [a-z0-9] first \
         character followed by [a-z0-9._:/-], with no control characters"
    )]
    NonCanonical,
}

impl CaptureMountId {
    /// The maximum accepted mount-boundary length. A mount id names a volume
    /// instance, not a payload.
    const MAX_LEN: usize = 256;

    /// Validate and wrap a capture-time mount-source / volume-instance identifier.
    /// Supplied by the trusted capture backend, but validated here so a malformed
    /// (empty / over-length / control-char / non-canonical) id cannot enter a
    /// [`VerifiedCaptureTopology`] proof.
    pub fn new(value: impl Into<String>) -> Result<Self, CaptureMountIdError> {
        let value = value.into();
        if value.is_empty() {
            return Err(CaptureMountIdError::Empty);
        }
        if value.len() > Self::MAX_LEN {
            return Err(CaptureMountIdError::TooLong);
        }
        if !Self::is_canonical(&value) {
            return Err(CaptureMountIdError::NonCanonical);
        }
        Ok(Self(value))
    }

    /// A canonical mount id is a lowercase ASCII `[a-z0-9]` first character followed
    /// by `[a-z0-9._:/-]`. Rejects control characters, whitespace, and upper-case
    /// tokens.
    fn is_canonical(value: &str) -> bool {
        let mut chars = value.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
            return false;
        }
        chars.all(|ch| {
            ch.is_ascii_lowercase()
                || ch.is_ascii_digit()
                || matches!(ch, '-' | '_' | '.' | ':' | '/')
        })
    }

    /// The opaque mount-boundary identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CaptureMountId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One excluded binding's **separate volume**, carried as two *distinct* facts:
/// its capture-time [`CaptureMountId`] (the mount boundary / volume instance) and
/// its [`ContentDigest`] (the CAS-closure content address of its bytes).
///
/// Separateness of excluded volumes is judged by the mount boundary; the content
/// digest is used only for the shared-layer disjointness scan
/// ([`VerifiedCaptureTopology::ensure_absent_from_shared_layers`]). Keeping the
/// two as separate fields is the whole point: conflating them (judging a volume
/// boundary by its bytes) is semantically wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateVolumeMount {
    /// The capture-time mount boundary / volume instance identity.
    mount: CaptureMountId,
    /// The CAS-closure content address of this volume's bytes.
    content: ContentDigest,
}

impl StateVolumeMount {
    /// Bind a mount boundary to the content address of the volume it placed.
    #[must_use]
    pub fn new(mount: CaptureMountId, content: ContentDigest) -> Self {
        Self { mount, content }
    }

    /// The capture-time mount boundary / volume instance identity.
    #[must_use]
    pub fn mount(&self) -> &CaptureMountId {
        &self.mount
    }

    /// The CAS-closure content address of this volume's bytes.
    #[must_use]
    pub fn content(&self) -> ContentDigest {
        self.content
    }
}

/// The capture-time topology backing a Capsule's `snapshot = "exclude"` External
/// State: each excluded binding name mapped to the **separate** volume that holds
/// its bytes ([`StateVolumeMount`] = mount boundary + content address).
///
/// This is the structural capture fact a [`VerifiedCaptureTopology`] is minted
/// from. Its constructor is deliberately `pub(crate)` — there is **no public
/// arbitrary-map constructor**. A caller-declared name→volume map is *not* a proof
/// that the capture backend really placed excluded state on separate volumes; it
/// merely moves an earlier caller-supplied boolean into a caller-supplied map. In
/// live wiring (PR-2) a **trusted capture backend** mints this from the mounts it
/// actually placed; here it exists only so the sound verified type can consume it.
///
/// ```compile_fail
/// use snapshot::external_state::{ExcludedStateCaptureTopology, StateVolumeMount};
/// // `try_new` is pub(crate): a caller outside the crate cannot fabricate a topology
/// // from an arbitrary name→volume map.
/// let _ = ExcludedStateCaptureTopology::try_new(std::iter::empty::<(String, StateVolumeMount)>());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludedStateCaptureTopology {
    /// binding name → the separate state volume backing it.
    state_volumes: BTreeMap<String, StateVolumeMount>,
}

/// Why a capture topology could not be built from its `(binding_name, volume)` pairs.
/// Every variant fails closed: no topology is produced, so a duplicate or malformed
/// report from a trusted backend cannot be silently promoted into a proof.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TopologyError {
    /// A `(binding_name, volume)` pair carries an empty binding name — never a real
    /// declared External State binding, so it is refused fail-closed.
    #[error("capture topology entry has an empty binding name")]
    EmptyBindingName,
    /// Two `(binding_name, volume)` pairs share one binding name. A duplicate report
    /// is a trusted-backend bug; rather than silently overwrite one volume with the
    /// other (dropping a binding), the whole topology is refused fail-closed.
    #[error("capture topology names binding `{0}` more than once")]
    DuplicateBindingName(String),
}

impl ExcludedStateCaptureTopology {
    /// Build a topology from `(binding_name, separate_volume)` pairs — the separate
    /// volume that backs each `snapshot = "exclude"` binding at capture.
    ///
    /// Fail-closed on malformed input so an in-crate mis-implementation cannot be
    /// promoted into a proof: an empty binding name is
    /// [`TopologyError::EmptyBindingName`]; a **duplicate** binding name is
    /// [`TopologyError::DuplicateBindingName`] (never a silent last-writer-wins
    /// overwrite). Every distinct binding is preserved exactly once.
    ///
    /// `pub(crate)`: the arbitrary-map path is closed to callers. In PR-2 a trusted
    /// capture backend is the sole minter; until that live wiring lands, only the
    /// in-crate tests construct a topology, so the non-test build has no caller yet.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "trusted-backend minter is PR-2 live wiring; only tests construct a topology today"
        )
    )]
    pub(crate) fn try_new(
        volumes: impl IntoIterator<Item = (String, StateVolumeMount)>,
    ) -> Result<Self, TopologyError> {
        let mut state_volumes = BTreeMap::new();
        for (name, volume) in volumes {
            if name.is_empty() {
                return Err(TopologyError::EmptyBindingName);
            }
            // `insert` returns the previous value when the key already exists; a
            // duplicate binding name is a fail-closed error, not a silent overwrite.
            if state_volumes.insert(name.clone(), volume).is_some() {
                return Err(TopologyError::DuplicateBindingName(name));
            }
        }
        Ok(Self { state_volumes })
    }
}

/// Why a [`VerifiedCaptureTopology`] could not be minted from a verified contract +
/// capture topology. Every variant fails closed: no topology is produced, so no
/// exclusion claim can rest on a malformed capture.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExcludedStateBoundaryError {
    /// The Execution Contract envelope failed verification (its stored
    /// `execution_id` is not the canonical hash of the embedded contract), so no
    /// identity-bound topology can be minted from it.
    #[error(
        "execution contract failed verification; cannot bind an External-State exclusion boundary"
    )]
    UnverifiedContract,
    /// The capture candidate's own `execution_id` does not match the verified
    /// Execution Contract — the candidate does not belong to this contract, so a
    /// topology cannot be bound to it.
    #[error(
        "capture candidate Execution Identity {candidate} does not match the verified contract \
         identity {verified}"
    )]
    CandidateIdentityMismatch { candidate: String, verified: String },
    /// The capture candidate manifest has no derivable content address (its
    /// canonical form is malformed), so the topology cannot be bound to a candidate
    /// id.
    #[error("capture candidate manifest has no derivable snapshot id")]
    MalformedCandidate,
    /// A declared `snapshot = "exclude"` binding has no separate state volume in the
    /// capture topology: its bytes are not shown to be backed by a separate volume,
    /// so exclusion cannot be structurally set up.
    #[error(
        "excluded External State binding `{0}` has no separate state volume in the capture topology"
    )]
    MissingStateVolume(String),
    /// Two excluded bindings map to the SAME **mount boundary** — they are not the
    /// SEPARATE volumes §9.2 requires. (Judged by mount boundary, never by content
    /// digest: two distinct empty volumes legitimately share a content digest.)
    #[error(
        "excluded External State bindings share one mount boundary {0} — \
         each excluded binding must be a separate volume"
    )]
    SharedStateVolume(String),
    /// The capture topology names volumes for bindings the identity contract does
    /// not declare as External State — the topology must cover exactly the declared
    /// excluded set, no more.
    #[error(
        "capture topology names {0} state volume(s) for bindings not declared as External State"
    )]
    UnknownStateVolume(usize),
}

/// A **proof-carrying, identity- and candidate-bound** exclusion topology: the set
/// of content addresses of the separate External-State volumes that MUST be
/// excluded from every shared Snapshot layer, bound to the verified Execution
/// Identity *and* the specific capture candidate it was minted for.
///
/// **Why proof-carrying.** There is deliberately **no** public constructor that
/// takes a caller-chosen volume set (its fields are private and it is not
/// `Deserialize`): a caller-declared name→volume map bound to nothing was the hole
/// this type closes. The **only** constructor is
/// [`VerifiedCaptureTopology::from_verified_capture`], which verifies the Execution
/// Contract, requires the candidate to belong to that verified identity, requires
/// each declared `snapshot = "exclude"` binding to be backed by its own separate
/// **mount boundary**, and binds the result to both the verified `execution_id` and
/// the candidate's `snapshot_id`. A topology minted for contract A / candidate A
/// therefore cannot be applied to a Snapshot manifest of Identity B or candidate B.
///
/// **Mount boundary, not content digest, decides separateness.** Two excluded
/// bindings are separate volumes iff they occupy distinct [`CaptureMountId`]s. The
/// content digests are retained only for the shared-layer disjointness scan.
///
/// **What is proven here — be honest.** This pure slice sets up §17.4 exclusion
/// *structurally*, it does not byte-prove it. What holds:
///
/// * production state is not attached pre-capture (enforced upstream by the
///   `running`-eligibility gate; External State / secret bindings make a running
///   capture ineligible);
/// * each excluded binding is backed by a **separate** mount boundary (structural
///   check here); and
/// * the topology is **bound to the verified Execution Identity and capture
///   candidate** and refuses to apply to any other manifest.
///
/// [`Self::ensure_absent_from_shared_layers`] additionally checks the excluded
/// volume content addresses are not referenced verbatim as shared-layer refs. That
/// detects the state volume being listed as a shared layer; it does **not** prove
/// state bytes were never *copied into* a memory/vmstate/disk layer. The byte-level
/// CAS-closure disjointness proof — and the **trusted-backend attestation that
/// actually MINTS** a [`VerifiedCaptureTopology`] from mounts the backend really
/// placed — is deliberate PR-2 live-wiring; this slice defines the sound proof TYPE
/// and closes the self-declared map.
///
/// ```compile_fail
/// use snapshot::external_state::VerifiedCaptureTopology;
/// // No `Default` derive and no public constructor taking a caller-chosen volume
/// // set: the only constructor is `from_verified_capture`.
/// let _ = VerifiedCaptureTopology::default();
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCaptureTopology {
    /// The verified Execution Identity this topology is bound to. Private, so a
    /// topology can never be re-pointed at a foreign manifest.
    execution_id: ExecutionId,
    /// The specific capture candidate address this topology was minted for.
    /// Private, so a topology minted for candidate A cannot validate candidate B.
    candidate_id: SnapshotId,
    /// The separate excluded-state volume content addresses to keep out of shared
    /// layers (used only for the disjointness scan; separateness is decided by
    /// mount boundary at mint).
    volume_addresses: BTreeSet<ContentDigest>,
}

impl VerifiedCaptureTopology {
    /// Mint an identity- and candidate-bound exclusion topology from a **verified**
    /// Execution Contract envelope, the immutable capture `candidate`, and the
    /// capture topology — the only way to construct one.
    ///
    /// Steps, all fail-closed:
    ///
    /// 1. **Verify the contract** — [`ExecutionContractEnvelopeV1::verified_execution_id`]
    ///    recomputes the canonical hash and matches it against the stored id; a
    ///    disagreement yields [`ExcludedStateBoundaryError::UnverifiedContract`].
    /// 2. **Bind the candidate to that identity** — the `candidate`'s own
    ///    `execution_id` must equal the verified identity
    ///    ([`ExcludedStateBoundaryError::CandidateIdentityMismatch`] otherwise), and
    ///    its content address is derived once as the bound candidate id.
    /// 3. **Require a separate mount boundary per excluded binding** — every
    ///    `snapshot = "exclude"` External State binding (v1 has the single `exclude`
    ///    variant, so all declared bindings are excluded) must have its own distinct
    ///    [`CaptureMountId`] in `topology`; a missing entry is
    ///    [`ExcludedStateBoundaryError::MissingStateVolume`], two bindings sharing
    ///    one mount boundary is [`ExcludedStateBoundaryError::SharedStateVolume`].
    /// 4. **Reject extraneous volumes** — a topology naming volumes for bindings the
    ///    contract does not declare is [`ExcludedStateBoundaryError::UnknownStateVolume`].
    /// 5. **Bind** the topology to the verified `execution_id` and candidate id.
    ///
    /// Because the id is bound from the *verified* contract and the analysis reads
    /// that *same* contract, there is no seam between "which contract was proven"
    /// and "which contract was analyzed".
    pub fn from_verified_capture(
        envelope: &ExecutionContractEnvelopeV1,
        candidate: &SnapshotManifestV1,
        topology: &ExcludedStateCaptureTopology,
    ) -> Result<Self, ExcludedStateBoundaryError> {
        let verified = envelope
            .verified_execution_id()
            .map_err(|_| ExcludedStateBoundaryError::UnverifiedContract)?;
        let execution_id = verified.as_execution_id().clone();

        if candidate.execution_id != execution_id {
            return Err(ExcludedStateBoundaryError::CandidateIdentityMismatch {
                candidate: candidate.execution_id.to_string(),
                verified: execution_id.to_string(),
            });
        }
        let candidate_id = candidate
            .snapshot_id()
            .map_err(|_| ExcludedStateBoundaryError::MalformedCandidate)?;

        let mut mounts = BTreeSet::new();
        let mut volume_addresses = BTreeSet::new();
        for binding in &envelope.execution_contract.external_state {
            let volume = topology.state_volumes.get(&binding.name).ok_or_else(|| {
                ExcludedStateBoundaryError::MissingStateVolume(binding.name.clone())
            })?;
            if !mounts.insert(volume.mount.clone()) {
                return Err(ExcludedStateBoundaryError::SharedStateVolume(
                    volume.mount.to_string(),
                ));
            }
            volume_addresses.insert(volume.content);
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
            execution_id,
            candidate_id,
            volume_addresses,
        })
    }

    /// The verified Execution Identity this topology is bound to.
    #[must_use]
    pub fn execution_id(&self) -> &ExecutionId {
        &self.execution_id
    }

    /// The specific capture candidate address this topology is bound to.
    #[must_use]
    pub fn candidate_id(&self) -> &SnapshotId {
        &self.candidate_id
    }

    /// Whether the topology excludes no addresses (a Capsule with no External
    /// State). Even an empty topology is identity- and candidate-bound: it still
    /// refuses to apply to a foreign manifest.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.volume_addresses.is_empty()
    }

    /// Whether `address` is one of the excluded External-State volume content
    /// addresses.
    #[must_use]
    pub fn contains(&self, address: &ContentDigest) -> bool {
        self.volume_addresses.contains(address)
    }

    /// Assert that (a) `manifest` belongs to the SAME verified Execution Identity
    /// this topology was bound to, (b) it is the SAME capture candidate, and (c) no
    /// excluded External-State volume content address appears in any shared Snapshot
    /// layer (memory / vmstate / disk).
    ///
    /// Fail-closed on the first breach. This is the STRUCTURAL check that §17.4 is
    /// set up (state volume separate and not referenced verbatim as a shared layer,
    /// topology bound to its own identity and candidate) — **not** a byte-level
    /// disjointness proof (see the type doc): it cannot prove state bytes copied
    /// into a layer are absent.
    pub fn ensure_absent_from_shared_layers(
        &self,
        manifest: &SnapshotManifestV1,
    ) -> Result<(), ExclusionViolation> {
        // Identity binding: a topology for Identity A can never be applied to a
        // manifest of Identity B.
        if manifest.execution_id != self.execution_id {
            return Err(ExclusionViolation::ExecutionIdentityMismatch {
                boundary: self.execution_id.to_string(),
                manifest: manifest.execution_id.to_string(),
            });
        }
        // Candidate binding: a topology minted for candidate A can never be applied
        // to a different candidate of the same identity.
        let manifest_id = manifest
            .snapshot_id()
            .map_err(|_| ExclusionViolation::MalformedManifest)?;
        if manifest_id != self.candidate_id {
            return Err(ExclusionViolation::CandidateMismatch {
                boundary: self.candidate_id.to_string(),
                manifest: manifest_id.to_string(),
            });
        }
        // An empty topology can never be violated; skip the scan.
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
    /// The topology was applied to a manifest of a DIFFERENT Execution Identity
    /// than it was bound to — refused fail-closed (RFC §9.2 identity binding).
    #[error(
        "External State exclusion topology bound to Execution Identity {boundary} cannot be \
         applied to a Snapshot manifest of Identity {manifest}"
    )]
    ExecutionIdentityMismatch { boundary: String, manifest: String },
    /// The topology was applied to a DIFFERENT capture candidate than it was minted
    /// for (same identity, different snapshot address) — refused fail-closed.
    #[error(
        "External State exclusion topology bound to capture candidate {boundary} cannot be \
         applied to a different candidate {manifest}"
    )]
    CandidateMismatch { boundary: String, manifest: String },
    /// The manifest under check has no derivable content address (malformed
    /// canonical form) — refused fail-closed.
    #[error("Snapshot manifest under exclusion check has no derivable snapshot id")]
    MalformedManifest,
    /// An excluded External-State volume content address appears in a shared
    /// Snapshot layer — the `snapshot = "exclude"` boundary was breached and the
    /// candidate MUST be rejected.
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
    /// The handle is not the canonical spelling: it must begin with a
    /// lowercase-ASCII alphanumeric and otherwise contain only `[a-z0-9._:-]`. This
    /// rejects control characters, whitespace, and upper-case "shouting" tokens
    /// like `SECRET-...` or `owner-...` in caps. This canonical form is
    /// **defense-in-depth**, not the non-secret guarantee itself (see the type doc).
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
///
/// **Where "non-secret" comes from.** The non-secret / non-authorization property
/// is a consequence of **trusted-resolver minting**: a production ref is issued by
/// a trusted resolver that never mints a secret- or authorization-bearing handle.
/// The canonical `opaque:<handle>` grammar checked here — bounded length, lowercase
/// ASCII `[a-z0-9]` + `[-._:]`, no control characters — is **defense-in-depth**,
/// not the guarantee: a lowercase secret such as `opaque:sk_live_1234` satisfies the
/// grammar, so the grammar alone cannot certify a value is non-secret. It only
/// stops obviously non-opaque, over-long, control-char, or "shouting" tokens from
/// entering a Receipt. The synthetic namespace ([`Self::synthetic`], `pub(crate)`)
/// is separated from trusted-resolver refs and is validation-only. A ref is a
/// non-identity value (RFC §4.3): it never influences `execution_id`.
///
/// The synthetic namespace is `pub(crate)`: a caller outside the crate cannot mint
/// a synthetic-namespaced ref, and there is no other public constructor besides the
/// validated [`Self::new`].
///
/// ```compile_fail
/// use snapshot::external_state::OpaqueStateRef;
/// // `synthetic` is pub(crate): unreachable from outside the crate.
/// let _ = OpaqueStateRef::synthetic("data");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OpaqueStateRef(String);

impl OpaqueStateRef {
    /// Prefix every opaque reference carries.
    const PREFIX: &'static str = "opaque:";
    /// The reserved synthetic-namespace prefix. A ref whose handle begins with
    /// `synthetic:` (i.e. the full ref begins with `opaque:synthetic:`) is a
    /// validation-only ephemeral ref (RFC §8.4) and can never be a production proof.
    const SYNTHETIC_PREFIX: &'static str = "opaque:synthetic:";
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

    /// A canonical handle is a lowercase ASCII `[a-z0-9]` first character followed
    /// by `[a-z0-9._:-]`. Rejects control characters, whitespace, and any
    /// upper-case token. Defense-in-depth over trusted minting, not a non-secret
    /// proof on its own.
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

    /// A validation-only **synthetic** opaque reference for `binding` (RFC §8.4):
    /// disposable acceptance and build may attach only ephemeral synthetic
    /// bindings, never a real state ref.
    ///
    /// `pub(crate)`: the synthetic namespace is separated from trusted-resolver
    /// refs — a caller outside the crate cannot mint one. It routes through
    /// [`Self::new`] and therefore **returns `Result`**: a `binding` containing
    /// uppercase, control chars, whitespace, or an over-length value fails closed
    /// rather than producing an asymmetric value that serializes but is rejected on
    /// deserialize. Used solely by [`SyntheticValidationStateInstance`].
    pub(crate) fn synthetic(binding: &str) -> Result<Self, OpaqueStateRefError> {
        Self::new(format!("{}{binding}", Self::SYNTHETIC_PREFIX))
    }

    /// Whether this ref is in the reserved synthetic (validation-only) namespace
    /// (`opaque:synthetic:*`). A synthetic ref is disposable acceptance/build state and
    /// is never a production proof, so it can never be promoted to a
    /// [`TrustedProductionStateRef`].
    #[cfg(test)]
    #[must_use]
    pub(crate) fn is_synthetic(&self) -> bool {
        self.0.starts_with(Self::SYNTHETIC_PREFIX)
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
// 4. Verified binding + concrete instance + schema-gated attach (RFC §9.2)
// ---------------------------------------------------------------------------

/// Why a [`VerifiedExternalStateBinding`] could not be resolved from a verified
/// envelope.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VerifiedExternalStateBindingError {
    /// The Execution Contract envelope failed verification, so no binding proven to
    /// belong to the current verified execution contract can be resolved from it.
    #[error(
        "execution contract failed verification; cannot resolve a verified External-State binding"
    )]
    UnverifiedContract,
    /// The verified Execution Contract declares no External State binding with the
    /// requested name — a caller cannot bind a foreign / fabricated contract.
    #[error("execution contract declares no External State binding named `{0}`")]
    UnknownBinding(String),
}

/// A **proof-carrying** External State binding: an [`ExternalStateContract`] proven
/// to belong to a specific **verified** Execution Identity by exact binding-name
/// lookup against that identity's contract.
///
/// This closes the hole where a production attachment could be minted from a bare,
/// caller-supplied `&ExternalStateContract`: a Session started under Execution
/// Identity A could be handed a fabricated contract B (matching only on schema) and
/// mint an attachment that nothing downstream could reconcile. Here the contract is
/// never trusted from the caller — it is **read out of the verified envelope** by
/// name, and the verified `execution_id` travels with it.
///
/// Its fields are private and it is not `Deserialize`; the only constructor is
/// [`VerifiedExternalStateBinding::from_verified_envelope`].
///
/// ```compile_fail
/// use snapshot::external_state::VerifiedExternalStateBinding;
/// // Fields are private: no struct-literal construction from outside the module.
/// let _ = VerifiedExternalStateBinding {
///     execution_id: unreachable!(),
///     contract: unreachable!(),
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedExternalStateBinding {
    /// The verified Execution Identity the contract was read from. Private, so an
    /// attachment can only ever be bound to the identity that actually declared it.
    execution_id: ExecutionId,
    /// The identity-bearing binding contract, copied out of the verified envelope.
    contract: ExternalStateContract,
}

impl VerifiedExternalStateBinding {
    /// Resolve the External State binding named `binding_name` from a **verified**
    /// Execution Contract envelope — the only way to construct one.
    ///
    /// Steps, fail-closed:
    ///
    /// 1. **Verify the contract** — [`ExecutionContractEnvelopeV1::verified_execution_id`]
    ///    recomputes the canonical hash and matches it against the stored id; a
    ///    disagreement yields [`VerifiedExternalStateBindingError::UnverifiedContract`].
    /// 2. **Exact binding-name lookup** — the binding must be declared in that same
    ///    verified contract; an absent name is
    ///    [`VerifiedExternalStateBindingError::UnknownBinding`]. The contract is read
    ///    from the verified envelope, never taken from the caller.
    /// 3. **Bind** the verified `execution_id` to the resolved contract.
    pub fn from_verified_envelope(
        envelope: &ExecutionContractEnvelopeV1,
        binding_name: &str,
    ) -> Result<Self, VerifiedExternalStateBindingError> {
        let verified = envelope
            .verified_execution_id()
            .map_err(|_| VerifiedExternalStateBindingError::UnverifiedContract)?;
        let contract = envelope
            .execution_contract
            .external_state
            .iter()
            .find(|binding| binding.name == binding_name)
            .ok_or_else(|| {
                VerifiedExternalStateBindingError::UnknownBinding(binding_name.to_string())
            })?
            .clone();
        Ok(Self {
            execution_id: verified.as_execution_id().clone(),
            contract,
        })
    }

    /// The verified Execution Identity this binding belongs to.
    #[must_use]
    pub fn execution_id(&self) -> &ExecutionId {
        &self.execution_id
    }

    /// The identity-bearing binding contract, as read from the verified envelope.
    #[must_use]
    pub fn contract(&self) -> &ExternalStateContract {
        &self.contract
    }
}

/// Why a raw opaque ref may not be promoted to a [`TrustedProductionStateRef`].
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum TrustedProductionStateRefError {
    /// The ref is in the reserved synthetic (validation-only) namespace
    /// (`opaque:synthetic:*`). A synthetic ref is disposable acceptance/build state
    /// (RFC §8.4) and is never a production proof, so it can never become a trusted
    /// production ref — not even through the test factory.
    #[error("synthetic-namespace ref `{0}` cannot be promoted to a trusted production ref")]
    SyntheticNamespace(String),
}

/// A production state reference **minted only by a trusted resolver**.
///
/// The non-secret / non-authorization property of a production `state_ref` is a
/// consequence of *who minted it*, not of the [`OpaqueStateRef`] wire grammar (a
/// lowercase secret such as `opaque:sk_live_123456` satisfies the grammar). This
/// type makes "minted by a trusted resolver" a **type-level** fact rather than a
/// comment: it has **no** public constructor, **no** `From`/`TryFrom` from [`String`]
/// or [`OpaqueStateRef`], and **no** generic `pub(crate)` constructor either — so
/// neither an out-of-crate caller nor any other in-crate module can wrap a raw wire
/// ref (or a synthetic ref) into a trusted one. The sanctioned live production
/// resolver / verified-`BindingLease` constructor is added in **PR-2**; until then the
/// **only** constructor is a `#[cfg(test)]` factory, and even that factory rejects the
/// synthetic namespace. Consequently, in a non-test build the type has no constructor
/// at all, so a production [`ExternalStateInstance`] cannot be built until PR-2.
///
/// ```compile_fail
/// use snapshot::external_state::{OpaqueStateRef, TrustedProductionStateRef};
/// // No `From`/`Into`: a raw wire ref cannot become a trusted production ref.
/// let raw = OpaqueStateRef::new("opaque:x").unwrap();
/// let _: TrustedProductionStateRef = raw.into();
/// ```
///
/// ```compile_fail
/// use snapshot::external_state::TrustedProductionStateRef;
/// // No `TryFrom<String>`: a bare string cannot become a trusted production ref.
/// let _ = TrustedProductionStateRef::try_from("opaque:x".to_string());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedProductionStateRef {
    value: OpaqueStateRef,
    _private: (),
}

impl TrustedProductionStateRef {
    /// A `#[cfg(test)]` factory standing in for the PR-2 trusted resolver, so tests
    /// can mint a production ref without a public (or generic `pub(crate)`) constructor
    /// existing. It **rejects the synthetic namespace** (`opaque:synthetic:*`) with
    /// [`TrustedProductionStateRefError::SyntheticNamespace`]: a validation-only ref can
    /// never be promoted to a production proof, even in a test. There is deliberately no
    /// generic `new(OpaqueStateRef)`; PR-2 adds the sanctioned production-resolver /
    /// verified-`BindingLease` constructor in this same module.
    #[cfg(test)]
    pub(crate) fn for_test(value: OpaqueStateRef) -> Result<Self, TrustedProductionStateRefError> {
        if value.is_synthetic() {
            return Err(TrustedProductionStateRefError::SyntheticNamespace(
                value.as_str().to_string(),
            ));
        }
        Ok(Self {
            value,
            _private: (),
        })
    }

    /// The underlying opaque wire reference.
    #[must_use]
    pub fn as_opaque(&self) -> &OpaqueStateRef {
        &self.value
    }
}

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
/// **Closed construction.** Its fields are private, it has no public struct literal,
/// and its only constructor ([`ExternalStateInstance::new`], `pub(crate)`) requires a
/// [`TrustedProductionStateRef`] — so a raw [`OpaqueStateRef`] can never build a
/// production instance, and the whole production path is unreachable from outside the
/// crate until PR-2 wires the sanctioned resolver.
///
/// It is a DISTINCT type from [`SyntheticValidationStateInstance`]: only a
/// production instance is accepted by the production attach path
/// ([`plan_production_attach`]).
///
/// ```compile_fail
/// use snapshot::external_state::{ExternalStateInstance, OpaqueStateRef};
/// // A raw opaque ref cannot build a production instance: `new` requires a
/// // `TrustedProductionStateRef` (and is itself `pub(crate)`).
/// let raw = OpaqueStateRef::new("opaque:x").unwrap();
/// let _ = ExternalStateInstance::new(raw, "gen", "1", "owner", "vol");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalStateInstance {
    /// Trusted-resolver-minted opaque handle naming which concrete state this is.
    state_ref: TrustedProductionStateRef,
    /// Monotonic generation marker of the concrete state (non-identity).
    generation: String,
    /// The concrete volume's schema identity, gated against the contract's.
    schema: String,
    /// The owning principal id (non-identity; deliberately never read here — it must
    /// never reach a Receipt).
    #[allow(
        dead_code,
        reason = "owner id is a deliberately inert input: it is carried but never propagated, so it can never leak into a Receipt"
    )]
    owner_id: String,
    /// The volume/binding instance id (non-identity; deliberately never read here — it
    /// must never reach a Receipt).
    #[allow(
        dead_code,
        reason = "volume id is a deliberately inert input: it is carried but never propagated, so it can never leak into a Receipt"
    )]
    volume_id: String,
}

impl ExternalStateInstance {
    /// Assemble a production instance from a [`TrustedProductionStateRef`] and its
    /// non-identity facts. `pub(crate)`: the production path is unexported until PR-2
    /// wires the sanctioned resolver, and a raw [`OpaqueStateRef`] cannot be passed
    /// where a `TrustedProductionStateRef` is required.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "production instance minter is PR-2 live wiring; only tests construct one today"
        )
    )]
    pub(crate) fn new(
        state_ref: TrustedProductionStateRef,
        generation: impl Into<String>,
        schema: impl Into<String>,
        owner_id: impl Into<String>,
        volume_id: impl Into<String>,
    ) -> Self {
        Self {
            state_ref,
            generation: generation.into(),
            schema: schema.into(),
            owner_id: owner_id.into(),
            volume_id: volume_id.into(),
        }
    }
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
    /// real External State. Returns `Err` when the binding name cannot form a
    /// canonical synthetic ref (see [`OpaqueStateRef::synthetic`]).
    pub fn synthetic_for(contract: &ExternalStateContract) -> Result<Self, OpaqueStateRefError> {
        Ok(Self {
            state_ref: OpaqueStateRef::synthetic(&contract.name)?,
            generation: "synthetic".to_string(),
            schema: contract.schema.clone(),
        })
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

/// The non-secret, non-content facts shared by every attachment: the binding name,
/// target, and access mode (from the identity-bearing contract), the matched schema
/// identity (compatibility evidence), and the opaque ref + generation of the
/// concrete instance. It never carries owner id, volume id, data bytes, or secret
/// values.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExternalStateAttachmentCore {
    binding_name: String,
    target: GuestPath,
    access: ExternalStateAccess,
    schema_identity: String,
    state_ref: OpaqueStateRef,
    generation: String,
}

/// A sanctioned proof that a **compatible production** External State instance was
/// attached to a binding proven to belong to a verified Execution Identity.
///
/// It is minted **only** by [`plan_production_attach`] from a
/// [`VerifiedExternalStateBinding`], and only *after* the schema gate has passed —
/// so its mere existence proves both that the incompatible-schema path fails
/// **before** any attachment is produced (RFC §9.2) and that the attachment is
/// bound to the current verified `execution_id`. It is a DISTINCT type from
/// [`ValidationExternalStateAttachment`]: a validation attachment can never be
/// passed where a production one is required, so provenance is not lost after the
/// input boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductionExternalStateAttachment {
    execution_id: ExecutionId,
    core: ExternalStateAttachmentCore,
}

impl ProductionExternalStateAttachment {
    /// The verified Execution Identity this attachment is bound to.
    #[must_use]
    pub fn execution_id(&self) -> &ExecutionId {
        &self.execution_id
    }

    /// The identity-bearing binding name.
    #[must_use]
    pub fn binding_name(&self) -> &str {
        &self.core.binding_name
    }

    /// The identity-bearing mount/injection target.
    #[must_use]
    pub fn target(&self) -> &GuestPath {
        &self.core.target
    }

    /// The identity-bearing access mode.
    #[must_use]
    pub fn access(&self) -> ExternalStateAccess {
        self.core.access
    }

    /// The matched schema identity (non-secret compatibility evidence).
    #[must_use]
    pub fn schema_identity(&self) -> &str {
        &self.core.schema_identity
    }

    /// The opaque reference to the attached concrete state.
    #[must_use]
    pub fn state_ref(&self) -> &OpaqueStateRef {
        &self.core.state_ref
    }

    /// The attached state's generation (non-identity).
    #[must_use]
    pub fn generation(&self) -> &str {
        &self.core.generation
    }

    /// Produce the Session Receipt's External-State record for this **production**
    /// attachment — the **only** sanctioned way state facts reach a production
    /// Receipt. It copies solely the verified `execution_id` (opaque identity
    /// evidence, non-secret), the opaque ref, the generation, and the non-secret
    /// compatibility evidence; there is structurally no owner/volume/content/secret
    /// field to copy (RFC §9.3, §12, §14).
    #[must_use]
    pub fn session_receipt(&self) -> SessionStateReceiptV1 {
        SessionStateReceiptV1 {
            schema: SESSION_STATE_RECEIPT_V1_SCHEMA.to_string(),
            execution_id: self.execution_id.clone(),
            binding_name: self.core.binding_name.clone(),
            target: self.core.target.clone(),
            access: self.core.access,
            schema_identity: self.core.schema_identity.clone(),
            state_ref: self.core.state_ref.clone(),
            state_generation: self.core.generation.clone(),
        }
    }
}

/// A sanctioned proof that a **synthetic validation** instance was attached to a
/// contract binding for disposable verification (RFC §8.4).
///
/// It is minted **only** by [`plan_validation_attach`]. It is a DISTINCT type from
/// [`ProductionExternalStateAttachment`] and — crucially — carries **no**
/// `execution_id` and produces **no** production Session Receipt: a synthetic
/// instance run through the validation path can never be mistaken for production
/// downstream or mint a production Receipt.
///
/// A validation attachment cannot be passed where a production one is required, nor
/// coerced into a production Receipt — there is no `session_receipt` / `execution_id`
/// on this type:
///
/// ```compile_fail
/// use snapshot::external_state::ValidationExternalStateAttachment;
/// fn demo(validation: &ValidationExternalStateAttachment) {
///     // No production Session Receipt exists on the validation attachment.
///     let _ = validation.session_receipt();
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationExternalStateAttachment {
    core: ExternalStateAttachmentCore,
}

impl ValidationExternalStateAttachment {
    /// The identity-bearing binding name.
    #[must_use]
    pub fn binding_name(&self) -> &str {
        &self.core.binding_name
    }

    /// The identity-bearing mount/injection target.
    #[must_use]
    pub fn target(&self) -> &GuestPath {
        &self.core.target
    }

    /// The identity-bearing access mode.
    #[must_use]
    pub fn access(&self) -> ExternalStateAccess {
        self.core.access
    }

    /// The matched schema identity (non-secret compatibility evidence).
    #[must_use]
    pub fn schema_identity(&self) -> &str {
        &self.core.schema_identity
    }

    /// The opaque reference to the attached synthetic state.
    #[must_use]
    pub fn state_ref(&self) -> &OpaqueStateRef {
        &self.core.state_ref
    }

    /// The attached synthetic instance's generation (always `synthetic`).
    #[must_use]
    pub fn generation(&self) -> &str {
        &self.core.generation
    }
}

/// Plan the attach of a **production** instance to a **verified** contract binding,
/// running the **schema gate before attach** (RFC §9.2). The binding is a
/// [`VerifiedExternalStateBinding`] proven to belong to the current verified
/// Execution Identity — a raw `&ExternalStateContract` cannot be passed here, so a
/// caller-fabricated contract can never drive a production attach. Fail closed with
/// [`ExternalStateAttachError::SchemaIncompatible`] when the instance's schema
/// identity does not match the binding's identity-bearing schema — before any
/// attachment is produced. On success, mints a
/// [`ProductionExternalStateAttachment`] carrying the verified `execution_id`.
///
/// This path takes an [`ExternalStateInstance`] only: a
/// [`SyntheticValidationStateInstance`] is a distinct type and cannot be passed
/// here, so a validation-only instance can never drive a production attach.
///
/// It is `pub(crate)`: the production attach path is **unexported until PR-2** wires
/// the sanctioned resolver. Two type-level invariants hold regardless: the `binding`
/// is a [`VerifiedExternalStateBinding`] (a raw `&ExternalStateContract` cannot be
/// passed, so a caller-fabricated contract can never drive a production attach), and
/// the `instance` is an [`ExternalStateInstance`] (a [`SyntheticValidationStateInstance`]
/// is a distinct type and cannot be passed here). See the `compile_fail` doctests on
/// [`ExternalStateInstance`] and [`TrustedProductionStateRef`] for the boundary that
/// keeps the whole path unreachable from outside the crate.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "production attach path is PR-2 live wiring; only tests drive it today"
    )
)]
pub(crate) fn plan_production_attach(
    binding: &VerifiedExternalStateBinding,
    instance: &ExternalStateInstance,
) -> Result<ProductionExternalStateAttachment, ExternalStateAttachError> {
    let core = attach_core(
        binding.contract(),
        &instance.schema,
        instance.state_ref.as_opaque(),
        &instance.generation,
    )?;
    Ok(ProductionExternalStateAttachment {
        execution_id: binding.execution_id().clone(),
        core,
    })
}

/// Plan the attach of a **synthetic validation** instance to its contract binding,
/// running the same schema gate before attach (RFC §9.2 / §8.4). This is the only
/// path that accepts a [`SyntheticValidationStateInstance`], and it produces a
/// [`ValidationExternalStateAttachment`] — never a production attachment or a
/// production Receipt. Production attaches go through [`plan_production_attach`].
///
/// **#1093 handoff (not a blocker for the running lane).** This path still takes a
/// raw `&ExternalStateContract`, not an identity-bound binding — acceptable today
/// because the running lane rejects External State outright (see
/// [`requires_restore_time_bindings_for_live_workload`]), so nothing drives this path
/// against a live workload. When #1093 lands `workload_idle`, the validation path MUST
/// also require a [`VerifiedExternalStateBinding`] (or an equivalent identity-bound
/// validation binding) so disposable verification cannot be run against a *different*
/// schema contract than the one under the verified Execution Identity. Do not promote
/// this raw-contract signature into the `workload_idle` lifecycle without that gate.
pub fn plan_validation_attach(
    contract: &ExternalStateContract,
    instance: &SyntheticValidationStateInstance,
) -> Result<ValidationExternalStateAttachment, ExternalStateAttachError> {
    let core = attach_core(
        contract,
        &instance.schema,
        &instance.state_ref,
        &instance.generation,
    )?;
    Ok(ValidationExternalStateAttachment { core })
}

fn attach_core(
    contract: &ExternalStateContract,
    instance_schema: &str,
    state_ref: &OpaqueStateRef,
    generation: &str,
) -> Result<ExternalStateAttachmentCore, ExternalStateAttachError> {
    if contract.schema != instance_schema {
        return Err(ExternalStateAttachError::SchemaIncompatible {
            binding: contract.name.clone(),
            expected: contract.schema.clone(),
            found: instance_schema.to_string(),
        });
    }
    Ok(ExternalStateAttachmentCore {
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
    /// a non-opaque `state_ref`, a malformed `target`, or a malformed
    /// `execution_id`.
    #[error("session state receipt is not a valid v1 wire record: {0}")]
    Malformed(String),
    /// The receipt's `schema` is not [`SESSION_STATE_RECEIPT_V1_SCHEMA`].
    #[error("session state receipt schema is not the supported v1 schema")]
    UnsupportedSchema,
}

/// The private wire twin of [`SessionStateReceiptV1`]: a raw, `deny_unknown_fields`
/// decode of the v1 record. It exists **only** as the input to
/// [`SessionStateReceiptV1`]'s custom `Deserialize`, which validates the schema
/// discriminator before yielding a public receipt. A generic consumer therefore
/// cannot obtain a `SessionStateReceiptV1` whose `schema` was never checked (the
/// wire-version-dispatch bypass): every deserialize routes through validation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionStateReceiptWireV1 {
    schema: String,
    execution_id: ExecutionId,
    binding_name: String,
    target: GuestPath,
    access: ExternalStateAccess,
    schema_identity: String,
    state_ref: OpaqueStateRef,
    state_generation: String,
}

/// The Session Receipt's record of one attached External State binding.
///
/// Records **only** the verified `execution_id` (opaque identity evidence,
/// non-secret), an opaque state reference, the state generation, and non-secret
/// compatibility evidence (binding name, target, access mode, and the matched
/// schema identity). It never carries content, data bytes, secret values, identity
/// assertions, the owner id, or the volume instance id — there is structurally no
/// field for any of those, and `deny_unknown_fields` (enforced on the wire twin)
/// refuses a wire record that tries to smuggle one in (RFC §9.3, §12 "Receipts MUST
/// redact secret values and identity assertions", §14). The generation is a
/// recorded fact that does not change `execution_id` (RFC §9.3).
///
/// Its fields are **private** and it has a **custom `Deserialize`** that runs the
/// schema-discriminator check: a receipt is minted only via
/// [`ProductionExternalStateAttachment::session_receipt`] (fail-closed by
/// construction) and, for a receipt read off the wire, obtained only through the
/// validated boundary — `serde_json::from_str` of a wrong-schema receipt is
/// **rejected**, never silently read as v1. [`SessionStateReceiptV1::parse`] is the
/// sanctioned entry that additionally maps decode errors into a typed error.
///
/// **Inspection only.** A decoded `SessionStateReceiptV1` is self-declared: its
/// `execution_id` is a string the wire record carries, not a proof that its
/// `binding_name` / `target` / `access` / `schema_identity` actually belong to that
/// identity. Use a bare receipt only to *inspect* recorded facts.
/// [`SessionStateReceiptV1::match_against`] additionally reconciles it against the
/// Execution Contract and yields a [`ContractMatchedSessionStateReceiptV1`], but that
/// proves only STRUCTURAL match — not the authenticity of the issuer, the `state_ref`,
/// or the generation. A security decision (authorization / audit evidence / state
/// reattach) additionally requires composition with an authenticated outer receipt
/// (MAC/signature), which is PR-2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionStateReceiptV1 {
    /// Always [`SESSION_STATE_RECEIPT_V1_SCHEMA`].
    schema: String,
    /// The verified Execution Identity this attachment was bound to (opaque
    /// identity evidence, non-secret).
    execution_id: ExecutionId,
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

impl<'de> Deserialize<'de> for SessionStateReceiptV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = SessionStateReceiptWireV1::deserialize(deserializer)?;
        let receipt = Self {
            schema: wire.schema,
            execution_id: wire.execution_id,
            binding_name: wire.binding_name,
            target: wire.target,
            access: wire.access,
            schema_identity: wire.schema_identity,
            state_ref: wire.state_ref,
            state_generation: wire.state_generation,
        };
        // Wire-version dispatch: the schema discriminator is enforced HERE, so a
        // wrong/unknown schema can never be read as v1 through the raw path.
        receipt.validate().map_err(serde::de::Error::custom)?;
        Ok(receipt)
    }
}

impl SessionStateReceiptV1 {
    /// Parse + validate a receipt from JSON, fail-closed. The custom `Deserialize`
    /// already enforces the schema discriminator and `deny_unknown_fields` (via the
    /// wire twin) rejects a receipt carrying an `owner_id` / `secret` / `volume_id`
    /// field, a non-opaque `state_ref`, a malformed `execution_id`, or a malformed
    /// `target`; this is the sanctioned entry that maps decode errors into a typed
    /// [`SessionStateReceiptError`] for a receipt a consumer did not itself mint.
    pub fn parse(json: &str) -> Result<Self, SessionStateReceiptError> {
        serde_json::from_str(json).map_err(|error| {
            // A schema mismatch surfaced through the custom Deserialize as a serde
            // error; re-map it to the typed UnsupportedSchema so callers can match
            // on it, and everything else to Malformed.
            let message = error.to_string();
            if message.contains("supported v1 schema") {
                SessionStateReceiptError::UnsupportedSchema
            } else {
                SessionStateReceiptError::Malformed(message)
            }
        })
    }

    /// Enforce the schema discriminator. The typed fields (opaque-validated
    /// `state_ref`, canonical `GuestPath` `target`, validated `ExecutionId`, enum
    /// `access`) already fail closed at deserialize; this covers the string-valued
    /// `schema` serde cannot, and is run inside the custom `Deserialize`.
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

    /// The verified Execution Identity this attachment was bound to.
    #[must_use]
    pub fn execution_id(&self) -> &ExecutionId {
        &self.execution_id
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

    /// **Match** this receipt structurally against the Execution Contract it claims to
    /// belong to, yielding a [`ContractMatchedSessionStateReceiptV1`] — the **only**
    /// way to obtain one.
    ///
    /// A bare [`SessionStateReceiptV1`] is self-declared: its `execution_id` is just a
    /// string a wire record carries, so a receipt can claim identity A while its
    /// `binding_name` / `target` / `access` / `schema_identity` describe a *foreign*
    /// contract B. Matching reconciles those facets against the verified contract and
    /// rejects a mismatch. **It proves only STRUCTURAL match**, not the authenticity of
    /// the issuer, the `state_ref`, or the generation: the Execution Contract and its
    /// `execution_id` are public, so a hand-authored receipt with correct facets but a
    /// foreign `state_ref`/generation still matches. A security decision (authorization
    /// / audit evidence / state reattach) additionally requires composition with an
    /// authenticated outer receipt (MAC/signature), which is PR-2.
    ///
    /// Steps, all fail-closed:
    ///
    /// 1. **Re-verify the envelope** — [`ExecutionContractEnvelopeV1::verified_execution_id`]
    ///    recomputes the canonical hash and matches it against the stored id; a
    ///    disagreement is [`SessionStateReceiptMatchError::UnverifiedContract`].
    /// 2. **Bind identity** — the receipt's `execution_id` must equal the *verified*
    ///    identity ([`SessionStateReceiptMatchError::ExecutionIdentityMismatch`]).
    /// 3. **Exact binding lookup** — `binding_name` must be declared in that verified
    ///    contract's `external_state` ([`SessionStateReceiptMatchError::UnknownBinding`]).
    /// 4. **Exact facet match** — `target`, `access`, and `schema_identity` must each
    ///    EXACTLY equal that binding's identity-bearing facets
    ///    ([`SessionStateReceiptMatchError::TargetMismatch`] /
    ///    [`SessionStateReceiptMatchError::AccessMismatch`] /
    ///    [`SessionStateReceiptMatchError::SchemaIdentityMismatch`]).
    /// 5. **Non-empty generation** — a recorded state must carry a generation
    ///    ([`SessionStateReceiptMatchError::EmptyGeneration`]).
    pub fn match_against(
        self,
        envelope: &ExecutionContractEnvelopeV1,
    ) -> Result<ContractMatchedSessionStateReceiptV1, SessionStateReceiptMatchError> {
        let verified = envelope
            .verified_execution_id()
            .map_err(|_| SessionStateReceiptMatchError::UnverifiedContract)?;
        let verified_id = verified.as_execution_id();
        if &self.execution_id != verified_id {
            return Err(SessionStateReceiptMatchError::ExecutionIdentityMismatch {
                receipt: self.execution_id.to_string(),
                verified: verified_id.to_string(),
            });
        }
        let binding = envelope
            .execution_contract
            .external_state
            .iter()
            .find(|binding| binding.name == self.binding_name)
            .ok_or_else(|| {
                SessionStateReceiptMatchError::UnknownBinding(self.binding_name.clone())
            })?;
        if self.target != binding.target {
            return Err(SessionStateReceiptMatchError::TargetMismatch {
                binding: self.binding_name.clone(),
                expected: binding.target.to_string(),
                found: self.target.to_string(),
            });
        }
        if self.access != binding.access {
            return Err(SessionStateReceiptMatchError::AccessMismatch {
                binding: self.binding_name.clone(),
                expected: format!("{:?}", binding.access),
                found: format!("{:?}", self.access),
            });
        }
        if self.schema_identity != binding.schema {
            return Err(SessionStateReceiptMatchError::SchemaIdentityMismatch {
                binding: self.binding_name.clone(),
                expected: binding.schema.clone(),
                found: self.schema_identity.clone(),
            });
        }
        if self.state_generation.is_empty() {
            return Err(SessionStateReceiptMatchError::EmptyGeneration);
        }
        Ok(ContractMatchedSessionStateReceiptV1 {
            receipt: self,
            _private: (),
        })
    }
}

/// Why a [`SessionStateReceiptV1`] failed to structurally match its Execution
/// Contract. Every variant fails closed: no [`ContractMatchedSessionStateReceiptV1`]
/// is produced, so a receipt whose facets do not reconcile with the verified contract
/// can never be presented as a contract match.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SessionStateReceiptMatchError {
    /// The Execution Contract envelope failed verification (its stored `execution_id`
    /// is not the canonical hash of the embedded contract), so the receipt cannot be
    /// matched against it at all.
    #[error("execution contract failed verification; cannot match a session state receipt")]
    UnverifiedContract,
    /// The receipt's self-declared `execution_id` does not equal the verified identity
    /// — the receipt does not belong to this contract.
    #[error(
        "session state receipt Execution Identity {receipt} does not match the verified contract \
         identity {verified}"
    )]
    ExecutionIdentityMismatch { receipt: String, verified: String },
    /// The verified contract declares no External State binding with the receipt's
    /// `binding_name` — the receipt names a binding that is not part of this identity.
    #[error("verified execution contract declares no External State binding named `{0}`")]
    UnknownBinding(String),
    /// The receipt's `target` does not exactly match the verified binding's target.
    #[error(
        "session state receipt target mismatch for binding `{binding}`: verified `{expected}`, \
         receipt `{found}`"
    )]
    TargetMismatch {
        binding: String,
        expected: String,
        found: String,
    },
    /// The receipt's `access` does not exactly match the verified binding's access.
    #[error(
        "session state receipt access mismatch for binding `{binding}`: verified `{expected}`, \
         receipt `{found}`"
    )]
    AccessMismatch {
        binding: String,
        expected: String,
        found: String,
    },
    /// The receipt's `schema_identity` does not exactly match the verified binding's
    /// schema — the compatibility evidence belongs to a different schema contract.
    #[error(
        "session state receipt schema identity mismatch for binding `{binding}`: verified \
         `{expected}`, receipt `{found}`"
    )]
    SchemaIdentityMismatch {
        binding: String,
        expected: String,
        found: String,
    },
    /// The receipt carries an empty `state_generation` — a recorded state must have a
    /// generation marker.
    #[error("session state receipt carries an empty state generation")]
    EmptyGeneration,
}

/// A proof that a [`SessionStateReceiptV1`] **structurally matches** the Execution
/// Contract it claims to belong to — identity, exact binding, and each
/// identity-bearing facet reconciled (see [`SessionStateReceiptV1::match_against`]).
///
/// Proves only STRUCTURAL match with the Execution Contract. Does NOT prove the
/// authenticity of the issuer, the `state_ref`, or the generation. A security decision
/// (authorization / audit evidence / state reattach) additionally requires composition
/// with an authenticated outer receipt (MAC/signature), which is PR-2.
///
/// The Execution Contract and its `execution_id` are public, so an attacker can
/// hand-author a receipt with correct binding facets but a foreign `state_ref` /
/// generation and still obtain this type — contract match is not authenticity. Its
/// fields are private and it is not `Deserialize`, so the **only** way to obtain one is
/// [`SessionStateReceiptV1::match_against`]; a wire receipt can never be decoded
/// directly into a contract-matched proof.
///
/// ```compile_fail
/// use snapshot::external_state::{SessionStateReceiptV1, ContractMatchedSessionStateReceiptV1};
/// // No public constructor and no struct literal (private fields): a contract-matched
/// // receipt can only come from `match_against`.
/// fn demo(receipt: SessionStateReceiptV1) {
///     let _ = ContractMatchedSessionStateReceiptV1 { receipt, _private: () };
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractMatchedSessionStateReceiptV1 {
    receipt: SessionStateReceiptV1,
    _private: (),
}

impl ContractMatchedSessionStateReceiptV1 {
    /// The underlying receipt, now proven to structurally match its verified Execution
    /// Contract (structural match only — not authenticity; see the type doc).
    #[must_use]
    pub fn receipt(&self) -> &SessionStateReceiptV1 {
        &self.receipt
    }

    /// Consume the proof, returning the contract-matched receipt.
    #[must_use]
    pub fn into_receipt(self) -> SessionStateReceiptV1 {
        self.receipt
    }
}

#[cfg(test)]
mod tests {
    use capsule::execution_contract::DigestAlgorithm;

    use super::*;

    fn digest(byte: u8) -> ContentDigest {
        ContentDigest::new(DigestAlgorithm::Blake3, [byte; 32])
    }

    fn mount(tag: &str) -> CaptureMountId {
        CaptureMountId::new(tag).expect("canonical mount id")
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

    fn trusted_ref(handle: &str) -> TrustedProductionStateRef {
        TrustedProductionStateRef::for_test(OpaqueStateRef::new(handle).expect("canonical ref"))
            .expect("a non-synthetic ref is a valid trusted production ref")
    }

    fn instance(schema: &str) -> ExternalStateInstance {
        ExternalStateInstance::new(
            trusted_ref("opaque:user-state-ref"),
            "gen_456",
            schema,
            "user-123",
            "vol-789",
        )
    }

    /// A verified binding for the sample contract's single `data` binding, with the
    /// binding's schema/access overridden.
    fn verified_data_binding(
        schema: &str,
        access: ExternalStateAccess,
    ) -> VerifiedExternalStateBinding {
        let mut contract = crate::contract_fixtures::sample_execution_contract();
        contract.external_state[0].schema = schema.to_string();
        contract.external_state[0].access = access;
        let envelope = crate::contract_fixtures::envelope_for(contract);
        VerifiedExternalStateBinding::from_verified_envelope(&envelope, "data")
            .expect("verified envelope + declared binding name resolves")
    }

    /// A valid `running` manifest bound to the sample contract's Execution Identity.
    fn sample_manifest_bound() -> SnapshotManifestV1 {
        let execution_id = crate::contract_fixtures::sample_execution_contract()
            .compute_execution_id()
            .expect("valid contract hashes");
        SnapshotManifestV1 {
            execution_id,
            ..crate::contract_fixtures::sample_snapshot_manifest()
        }
    }

    fn empty_topology() -> ExcludedStateCaptureTopology {
        ExcludedStateCaptureTopology::try_new(Vec::<(String, StateVolumeMount)>::new())
            .expect("empty topology is valid")
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

    // --- Major 2: synthetic refs route through new(), fail closed on non-canonical
    // bindings, and every constructible ref round-trips (symmetric type invariant) ---
    #[test]
    fn synthetic_ref_routes_through_new_and_round_trips() {
        // A canonical binding name yields a valid synthetic ref.
        let ok =
            OpaqueStateRef::synthetic("data").expect("canonical binding → valid synthetic ref");
        assert_eq!(ok.as_str(), "opaque:synthetic:data");

        // Uppercase / control / whitespace / over-length binding names fail CLOSED
        // through new(), rather than producing a value that serializes but is
        // rejected on deserialize.
        assert!(OpaqueStateRef::synthetic("DATA").is_err());
        assert!(OpaqueStateRef::synthetic("has space").is_err());
        assert!(OpaqueStateRef::synthetic("ctrl\u{7f}").is_err());
        assert!(OpaqueStateRef::synthetic(&"a".repeat(OpaqueStateRef::MAX_HANDLE_LEN)).is_err());

        // Every constructible ref round-trips: serialize then deserialize succeeds.
        for value in ["opaque:user-state-ref", "opaque:x", ok.as_str()] {
            let reference = OpaqueStateRef::new(value).unwrap();
            let json = serde_json::to_string(&reference).unwrap();
            let reparsed: OpaqueStateRef = serde_json::from_str(&json).unwrap();
            assert_eq!(reparsed, reference);
        }
    }

    // --- Blocker 2: the ONLY constructor of a TrustedProductionStateRef is the
    // #[cfg(test)] factory (no generic pub(crate) `new`), and even it REJECTS the
    // synthetic (validation-only) namespace — so a synthetic ref can never become a
    // production proof, and a raw/synthetic ref has no non-test promotion path (see the
    // compile_fail doctests on TrustedProductionStateRef). ---
    #[test]
    fn trusted_production_ref_rejects_synthetic_namespace() {
        // A normal (non-synthetic) opaque ref promotes fine through the test factory.
        let ok = TrustedProductionStateRef::for_test(
            OpaqueStateRef::new("opaque:user-state-ref").unwrap(),
        )
        .expect("a non-synthetic ref is a valid trusted production ref");
        assert_eq!(ok.as_opaque().as_str(), "opaque:user-state-ref");

        // A synthetic-namespace ref — even though it is a perfectly canonical
        // OpaqueStateRef — is REFUSED promotion to a trusted production ref.
        let synthetic = OpaqueStateRef::synthetic("data").expect("canonical synthetic ref");
        assert!(synthetic.is_synthetic());
        assert_eq!(
            TrustedProductionStateRef::for_test(synthetic.clone()).unwrap_err(),
            TrustedProductionStateRefError::SyntheticNamespace(synthetic.as_str().to_string())
        );

        // The same holds for a hand-written `opaque:synthetic:*` value.
        let hand = OpaqueStateRef::new("opaque:synthetic:forged").unwrap();
        assert!(matches!(
            TrustedProductionStateRef::for_test(hand).unwrap_err(),
            TrustedProductionStateRefError::SyntheticNamespace(_)
        ));
    }

    // --- AC (17.4): a compatible schema attaches successfully (production path) ---
    #[test]
    fn compatible_schema_attaches() {
        let binding = verified_data_binding("1", ExternalStateAccess::ReadWrite);
        let attachment =
            plan_production_attach(&binding, &instance("1")).expect("compatible schema attaches");
        assert_eq!(attachment.binding_name(), "data");
        assert_eq!(attachment.access(), ExternalStateAccess::ReadWrite);
        assert_eq!(attachment.schema_identity(), "1");
        assert_eq!(attachment.state_ref().as_str(), "opaque:user-state-ref");
        assert_eq!(attachment.generation(), "gen_456");
    }

    // --- AC (17.4): incompatible schema fails BEFORE read-write attach ---
    #[test]
    fn incompatible_schema_fails_before_attach() {
        let binding = verified_data_binding("1", ExternalStateAccess::ReadWrite);
        let error = plan_production_attach(&binding, &instance("2")).unwrap_err();
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
        assert!(plan_production_attach(&binding, &instance("2")).is_err());
    }

    // --- Blocker 1: the production attachment + receipt are bound to the verified
    // Execution Identity, and a production attachment can only be minted through a
    // verified binding (never a raw / foreign contract) ---
    #[test]
    fn production_attachment_and_receipt_carry_verified_execution_id() {
        let contract = crate::contract_fixtures::sample_execution_contract();
        let expected_id = contract
            .compute_execution_id()
            .expect("valid contract hashes");
        let envelope = crate::contract_fixtures::envelope_for(contract);
        let binding = VerifiedExternalStateBinding::from_verified_envelope(&envelope, "data")
            .expect("declared binding resolves");
        assert_eq!(binding.execution_id(), &expected_id);

        let attachment = plan_production_attach(&binding, &instance("1")).unwrap();
        assert_eq!(attachment.execution_id(), &expected_id);

        let receipt = attachment.session_receipt();
        assert_eq!(receipt.execution_id(), &expected_id);
    }

    // --- Blocker 1: a binding name absent from the verified contract, or an
    // unverified envelope, fails closed — no binding is resolved ---
    #[test]
    fn verified_binding_fails_closed_for_absent_or_unverified_binding() {
        let envelope = crate::contract_fixtures::envelope_for(
            crate::contract_fixtures::sample_execution_contract(),
        );
        // A binding name the verified contract does not declare fails closed.
        assert_eq!(
            VerifiedExternalStateBinding::from_verified_envelope(&envelope, "ghost").unwrap_err(),
            VerifiedExternalStateBindingError::UnknownBinding("ghost".to_string())
        );

        // A tampered (unverified) envelope cannot resolve a binding at all.
        let mut tampered = crate::contract_fixtures::envelope_for(
            crate::contract_fixtures::sample_execution_contract(),
        );
        tampered.execution_id =
            ExecutionId::new(format!("blake3:{}", "e".repeat(64))).expect("valid id shape");
        assert_eq!(
            VerifiedExternalStateBinding::from_verified_envelope(&tampered, "data").unwrap_err(),
            VerifiedExternalStateBindingError::UnverifiedContract
        );
    }

    // --- Major 1: production vs validation attachments are DISTINCT types; the
    // validation path produces no execution_id / production Receipt ---
    #[test]
    fn production_and_validation_attachments_are_type_separated() {
        let contract = crate::contract_fixtures::sample_execution_contract();
        let envelope = crate::contract_fixtures::envelope_for(contract.clone());
        let binding = VerifiedExternalStateBinding::from_verified_envelope(&envelope, "data")
            .expect("declared binding resolves");

        // Production: carries the verified execution id and mints a Session Receipt.
        let production = plan_production_attach(&binding, &instance("1")).unwrap();
        let _ = production.execution_id();
        let _ = production.session_receipt();

        // Validation: a synthetic instance drives the validation path to a DISTINCT
        // ValidationExternalStateAttachment that carries no execution_id and no
        // production Receipt (see the compile_fail doctest on
        // `ValidationExternalStateAttachment::session_receipt` absence).
        let synthetic =
            SyntheticValidationStateInstance::synthetic_for(&contract.external_state[0]).unwrap();
        let validation = plan_validation_attach(&contract.external_state[0], &synthetic).unwrap();
        assert_eq!(validation.schema_identity(), "1");
        assert!(
            validation
                .state_ref()
                .as_str()
                .starts_with("opaque:synthetic:")
        );
        assert_eq!(validation.generation(), "synthetic");
    }

    // --- Major 1 (cont.): the synthetic validation instance is a DISTINCT type and
    // only the validation attach path accepts it ---
    #[test]
    fn synthetic_validation_instance_conforms_via_validation_path_only() {
        let contract = state_contract("1", ExternalStateAccess::ReadWrite);
        let synthetic = SyntheticValidationStateInstance::synthetic_for(&contract).unwrap();
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

    // --- AC (17.4): Receipt records opaque ref + generation + execution_id, never
    // data/secrets ---
    #[test]
    fn receipt_records_opaque_ref_and_generation_without_data_or_secrets() {
        let contract = crate::contract_fixtures::sample_execution_contract();
        let expected_id = contract
            .compute_execution_id()
            .expect("valid contract hashes");
        let envelope = crate::contract_fixtures::envelope_for(contract);
        let binding = VerifiedExternalStateBinding::from_verified_envelope(&envelope, "data")
            .expect("declared binding resolves");
        // Deliberately hostile owner/volume values: they must NOT reach the receipt.
        // There is no content/secret field on the instance at all.
        let inst = ExternalStateInstance::new(
            trusted_ref("opaque:user-state-ref"),
            "gen_456",
            "1",
            "SECRET-owner-token",
            "SECRET-volume-key",
        );
        let receipt = plan_production_attach(&binding, &inst)
            .unwrap()
            .session_receipt();

        assert_eq!(receipt.schema(), SESSION_STATE_RECEIPT_V1_SCHEMA);
        assert_eq!(receipt.execution_id(), &expected_id);
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

    // --- Major 3: a receipt carrying an unknown (secret/owner) field is REJECTED at
    // deserialize; a wrong/unknown schema is REJECTED at deserialize (wire-version
    // dispatch), not silently read as v1 ---
    #[test]
    fn receipt_wire_is_fail_closed() {
        let binding = verified_data_binding("1", ExternalStateAccess::ReadWrite);
        let receipt = plan_production_attach(&binding, &instance("1"))
            .unwrap()
            .session_receipt();
        let json = serde_json::to_string(&receipt).unwrap();

        // A receipt that smuggles an owner_id / secret field is rejected at
        // deserialize by deny_unknown_fields (enforced on the wire twin).
        for bad_field in [r#""owner_id":"u""#, r#""secret":"s""#, r#""volume_id":"v""#] {
            let tampered = json.replacen('{', &format!("{{{bad_field},"), 1);
            assert!(
                SessionStateReceiptV1::parse(&tampered).is_err(),
                "receipt with unknown field must be rejected: {bad_field}"
            );
            assert!(serde_json::from_str::<SessionStateReceiptV1>(&tampered).is_err());
        }

        // A receipt with the wrong schema discriminator is rejected at parse AND at
        // the raw serde_json::from_str path (the custom Deserialize enforces the
        // wire version — it is never read as v1).
        let wrong_schema = json.replace(
            SESSION_STATE_RECEIPT_V1_SCHEMA,
            "ato.session.external-state-receipt/v999",
        );
        assert_eq!(
            SessionStateReceiptV1::parse(&wrong_schema).unwrap_err(),
            SessionStateReceiptError::UnsupportedSchema
        );
        assert!(serde_json::from_str::<SessionStateReceiptV1>(&wrong_schema).is_err());
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
        let envelope = crate::contract_fixtures::envelope_for(contract);
        let binding = VerifiedExternalStateBinding::from_verified_envelope(&envelope, "data")
            .expect("declared binding resolves");

        // Two different concrete instances of the SAME binding: different owner,
        // volume id, generation, and opaque ref. None is part of the identity
        // contract, so it can never have been an input to the id.
        let inst_a = ExternalStateInstance::new(
            trusted_ref("opaque:user-state-ref"),
            "gen_1",
            "1",
            "owner-a",
            "vol-a",
        );
        let inst_b = ExternalStateInstance::new(
            trusted_ref("opaque:other-ref"),
            "gen_2",
            "1",
            "owner-b",
            "vol-b",
        );
        let receipt_a = plan_production_attach(&binding, &inst_a)
            .unwrap()
            .session_receipt();
        let receipt_b = plan_production_attach(&binding, &inst_b)
            .unwrap()
            .session_receipt();

        // The receipts differ only in non-identity generation / ref facets...
        assert_ne!(receipt_a.state_generation(), receipt_b.state_generation());
        assert_ne!(receipt_a.state_ref(), receipt_b.state_ref());
        // ...while the schema-identity compatibility evidence AND the bound
        // execution_id are identical...
        assert_eq!(receipt_a.schema_identity(), receipt_b.schema_identity());
        assert_eq!(receipt_a.execution_id(), receipt_b.execution_id());
        assert_eq!(receipt_a.execution_id(), &base_id);
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

    /// A verified, identity- and candidate-bound topology for `manifest`, with the
    /// sample's single `data` binding backed by mount `mnt-data` at content
    /// `volume`.
    fn verified_topology_for_manifest(
        manifest: &SnapshotManifestV1,
        volume: ContentDigest,
    ) -> VerifiedCaptureTopology {
        let envelope = crate::contract_fixtures::envelope_for(
            crate::contract_fixtures::sample_execution_contract(),
        );
        let topology = ExcludedStateCaptureTopology::try_new([(
            "data".to_string(),
            StateVolumeMount::new(mount("mnt-data"), volume),
        )])
        .expect("distinct-mount topology is valid");
        VerifiedCaptureTopology::from_verified_capture(&envelope, manifest, &topology)
            .expect("verified contract + separate-volume topology mints a topology")
    }

    // --- AC (17.4): excluded state bytes are absent from every shared Snapshot layer ---
    #[test]
    fn ensure_excluded_state_absent_scans_all_shared_layers() {
        let separate = digest(0x99);
        let manifest = sample_manifest_bound();
        let topology = verified_topology_for_manifest(&manifest, separate);
        assert!(topology.contains(&separate));

        // The separate state-volume address is absent from every shared layer.
        assert!(!manifest.memory_layer_refs.contains(&separate));
        topology
            .ensure_absent_from_shared_layers(&manifest)
            .expect("a separate volume address is absent from shared layers");

        // If the excluded state volume leaks into ANY shared layer, it fails
        // closed — checked for memory, vmstate, and disk independently. The topology
        // is minted for that same (leaked) candidate so the candidate binding holds.
        for layer in ["memory", "vmstate", "disk"] {
            let mut leaked = sample_manifest_bound();
            match layer {
                "memory" => leaked.memory_layer_refs = vec![separate],
                "vmstate" => leaked.vmstate_layer_refs = vec![separate],
                _ => leaked.disk_layer_refs = vec![separate],
            }
            let topology = verified_topology_for_manifest(&leaked, separate);
            let err = topology
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

    // --- Blocker 2: the topology is identity-bound — a topology for Identity A
    // rejects a manifest of Identity B ---
    #[test]
    fn topology_rejects_a_manifest_of_a_different_identity() {
        let manifest = sample_manifest_bound();
        let topology = verified_topology_for_manifest(&manifest, digest(0x99));
        // A manifest with a DIFFERENT execution_id (the sample manifest's hardcoded
        // id) cannot be checked against this topology at all.
        let foreign = crate::contract_fixtures::sample_snapshot_manifest();
        assert_ne!(&foreign.execution_id, topology.execution_id());
        let err = topology
            .ensure_absent_from_shared_layers(&foreign)
            .unwrap_err();
        assert!(matches!(
            err,
            ExclusionViolation::ExecutionIdentityMismatch { .. }
        ));
    }

    // --- Blocker 2: the topology is candidate-bound — a topology minted for
    // candidate A rejects a DIFFERENT candidate B of the SAME identity ---
    #[test]
    fn topology_bound_to_candidate_a_rejects_candidate_b() {
        let manifest_a = sample_manifest_bound();
        let topology = verified_topology_for_manifest(&manifest_a, digest(0x99));

        // Candidate B: same Execution Identity, different snapshot address (mutate a
        // non-identity layer ref, which changes the derived snapshot_id).
        let mut manifest_b = sample_manifest_bound();
        manifest_b.disk_layer_refs = vec![digest(0x44)];
        assert_eq!(manifest_b.execution_id, manifest_a.execution_id);
        assert_ne!(
            manifest_b.snapshot_id().unwrap(),
            manifest_a.snapshot_id().unwrap()
        );

        let err = topology
            .ensure_absent_from_shared_layers(&manifest_b)
            .unwrap_err();
        assert!(matches!(err, ExclusionViolation::CandidateMismatch { .. }));
    }

    // --- Blocker 2: an EMPTY topology is still identity- and candidate-bound and
    // cannot be applied to a foreign manifest ---
    #[test]
    fn empty_topology_is_identity_bound() {
        // A contract with no External State yields an empty topology...
        let contract = {
            let mut contract = crate::contract_fixtures::sample_execution_contract();
            contract.external_state.clear();
            contract
        };
        let execution_id = contract
            .compute_execution_id()
            .expect("valid contract hashes");
        let envelope = crate::contract_fixtures::envelope_for(contract);
        let own = SnapshotManifestV1 {
            execution_id,
            ..crate::contract_fixtures::sample_snapshot_manifest()
        };
        let topology =
            VerifiedCaptureTopology::from_verified_capture(&envelope, &own, &empty_topology())
                .expect("no external state → empty but identity-bound topology");
        assert!(topology.is_empty());

        // ...which trivially passes for its OWN manifest...
        topology
            .ensure_absent_from_shared_layers(&own)
            .expect("empty topology passes for its own identity+candidate");

        // ...but is refused for a foreign-identity manifest — an empty topology is
        // not a wildcard.
        let foreign = crate::contract_fixtures::sample_snapshot_manifest();
        assert_ne!(&foreign.execution_id, topology.execution_id());
        assert!(matches!(
            topology
                .ensure_absent_from_shared_layers(&foreign)
                .unwrap_err(),
            ExclusionViolation::ExecutionIdentityMismatch { .. }
        ));
    }

    // --- Blocker 2: structural checks — a separate volume per excluded binding is
    // required; missing / extraneous volumes fail closed ---
    #[test]
    fn topology_requires_a_separate_volume_per_excluded_binding() {
        let manifest = sample_manifest_bound();
        let envelope = crate::contract_fixtures::envelope_for(
            crate::contract_fixtures::sample_execution_contract(),
        );

        // Missing: the `data` binding has no state volume in the topology.
        assert_eq!(
            VerifiedCaptureTopology::from_verified_capture(&envelope, &manifest, &empty_topology())
                .unwrap_err(),
            ExcludedStateBoundaryError::MissingStateVolume("data".to_string())
        );

        // Extraneous: a topology naming a volume for a binding the contract does
        // not declare.
        let extra = ExcludedStateCaptureTopology::try_new([
            (
                "data".to_string(),
                StateVolumeMount::new(mount("m-data"), digest(0x99)),
            ),
            (
                "ghost".to_string(),
                StateVolumeMount::new(mount("m-ghost"), digest(0xaa)),
            ),
        ])
        .expect("two distinct bindings is a valid topology");
        assert_eq!(
            VerifiedCaptureTopology::from_verified_capture(&envelope, &manifest, &extra)
                .unwrap_err(),
            ExcludedStateBoundaryError::UnknownStateVolume(1)
        );
    }

    // --- Blocker 2: separateness is judged by MOUNT BOUNDARY, not content digest.
    // Two bindings on ONE mount boundary are NOT separate (even with distinct
    // content); two distinct mounts with IDENTICAL (empty) content ARE separate ---
    #[test]
    fn separateness_is_by_mount_boundary_not_content_digest() {
        // Two excluded bindings; `external_state` is canonical (sorted), append
        // `store` (> `data`).
        let mut contract = crate::contract_fixtures::sample_execution_contract();
        contract.external_state.push(ExternalStateContract {
            name: "store".to_string(),
            target: guest_path("/store"),
            access: ExternalStateAccess::ReadWrite,
            schema: "1".to_string(),
            snapshot: capsule::execution_contract::SnapshotExclusion::Exclude,
        });
        let execution_id = contract
            .compute_execution_id()
            .expect("valid contract hashes");
        let manifest = SnapshotManifestV1 {
            execution_id,
            ..crate::contract_fixtures::sample_snapshot_manifest()
        };
        let envelope = crate::contract_fixtures::envelope_for(contract);

        // Same MOUNT boundary, DISTINCT content digests → NOT separate (rejected).
        let shared_mount = mount("shared-mnt");
        let shared = ExcludedStateCaptureTopology::try_new([
            (
                "data".to_string(),
                StateVolumeMount::new(shared_mount.clone(), digest(0x99)),
            ),
            (
                "store".to_string(),
                StateVolumeMount::new(shared_mount.clone(), digest(0xaa)),
            ),
        ])
        .expect("distinct binding names is a valid topology");
        assert_eq!(
            VerifiedCaptureTopology::from_verified_capture(&envelope, &manifest, &shared)
                .unwrap_err(),
            ExcludedStateBoundaryError::SharedStateVolume(shared_mount.to_string())
        );

        // Distinct MOUNT boundaries, IDENTICAL (empty) content digest → separate:
        // two distinct empty volumes must NOT be falsely rejected as shared.
        let empty_content = digest(0x00);
        let separate = ExcludedStateCaptureTopology::try_new([
            (
                "data".to_string(),
                StateVolumeMount::new(mount("mnt-data"), empty_content),
            ),
            (
                "store".to_string(),
                StateVolumeMount::new(mount("mnt-store"), empty_content),
            ),
        ])
        .expect("distinct mounts is a valid topology");
        let topology =
            VerifiedCaptureTopology::from_verified_capture(&envelope, &manifest, &separate)
                .expect("distinct mounts are separate even with identical empty content");
        assert!(topology.contains(&empty_content));
    }

    // --- Blocker 2: the mount-boundary id and content digest are DISTINCT fields ---
    #[test]
    fn mount_boundary_id_and_content_digest_are_distinct_fields() {
        let volume = StateVolumeMount::new(mount("mnt-data"), digest(0x99));
        assert_eq!(volume.mount().as_str(), "mnt-data");
        assert_eq!(volume.content(), digest(0x99));
        // The mount boundary and the content address are independent facts: the
        // mount id string bears no relation to the digest's hex.
        assert_ne!(volume.mount().as_str(), volume.content().to_string());
    }

    // --- Blocker 2: a topology cannot be minted from an UNVERIFIED contract, nor
    // bound to a candidate of a different identity ---
    #[test]
    fn topology_cannot_be_minted_from_unverified_or_foreign_candidate() {
        let manifest = sample_manifest_bound();
        let topology = ExcludedStateCaptureTopology::try_new([(
            "data".to_string(),
            StateVolumeMount::new(mount("mnt-data"), digest(0x99)),
        )])
        .expect("single distinct binding is a valid topology");

        // Unverified: tamper the stored id so it no longer equals the canonical hash.
        let mut tampered = crate::contract_fixtures::envelope_for(
            crate::contract_fixtures::sample_execution_contract(),
        );
        tampered.execution_id =
            ExecutionId::new(format!("blake3:{}", "e".repeat(64))).expect("valid id shape");
        assert_eq!(
            VerifiedCaptureTopology::from_verified_capture(&tampered, &manifest, &topology)
                .unwrap_err(),
            ExcludedStateBoundaryError::UnverifiedContract
        );

        // Foreign candidate: a verified contract, but a candidate manifest whose own
        // execution_id belongs to a different identity.
        let envelope = crate::contract_fixtures::envelope_for(
            crate::contract_fixtures::sample_execution_contract(),
        );
        let foreign_candidate = crate::contract_fixtures::sample_snapshot_manifest();
        assert_ne!(&foreign_candidate.execution_id, &envelope.execution_id);
        assert!(matches!(
            VerifiedCaptureTopology::from_verified_capture(
                &envelope,
                &foreign_candidate,
                &topology
            )
            .unwrap_err(),
            ExcludedStateBoundaryError::CandidateIdentityMismatch { .. }
        ));
    }

    // --- Major: CaptureMountId::new validates input fail-closed inside the crate ---
    #[test]
    fn capture_mount_id_rejects_malformed_input() {
        // A canonical id is accepted.
        assert_eq!(
            CaptureMountId::new("mnt-data").unwrap().as_str(),
            "mnt-data"
        );
        assert!(CaptureMountId::new("vol/0:a.b_c").is_ok());

        // Empty -> Empty.
        assert_eq!(
            CaptureMountId::new("").unwrap_err(),
            CaptureMountIdError::Empty
        );
        // Control characters -> NonCanonical.
        assert_eq!(
            CaptureMountId::new("mnt\u{7f}data").unwrap_err(),
            CaptureMountIdError::NonCanonical
        );
        assert_eq!(
            CaptureMountId::new("mnt data").unwrap_err(),
            CaptureMountIdError::NonCanonical
        );
        // Upper-case "shouting" tokens are non-canonical.
        assert_eq!(
            CaptureMountId::new("MNT-DATA").unwrap_err(),
            CaptureMountIdError::NonCanonical
        );
        // Over-length -> TooLong.
        let too_long = "a".repeat(CaptureMountId::MAX_LEN + 1);
        assert_eq!(
            CaptureMountId::new(too_long).unwrap_err(),
            CaptureMountIdError::TooLong
        );
    }

    // --- Major: ExcludedStateCaptureTopology::try_new rejects a duplicate / empty
    // binding name and preserves every distinct binding exactly once ---
    #[test]
    fn topology_try_new_is_fail_closed_on_duplicate_or_empty_binding() {
        // A valid distinct-mount topology is accepted and preserves both bindings.
        let ok = ExcludedStateCaptureTopology::try_new([
            (
                "data".to_string(),
                StateVolumeMount::new(mount("mnt-data"), digest(0x11)),
            ),
            (
                "store".to_string(),
                StateVolumeMount::new(mount("mnt-store"), digest(0x22)),
            ),
        ])
        .expect("two distinct bindings is a valid topology");
        assert_eq!(ok.state_volumes.len(), 2);
        assert_eq!(
            ok.state_volumes.get("data").unwrap().content(),
            digest(0x11)
        );
        assert_eq!(
            ok.state_volumes.get("store").unwrap().content(),
            digest(0x22)
        );

        // A duplicate binding name is REJECTED — never a silent last-writer-wins
        // overwrite that would drop a binding.
        assert_eq!(
            ExcludedStateCaptureTopology::try_new([
                (
                    "data".to_string(),
                    StateVolumeMount::new(mount("mnt-a"), digest(0x11)),
                ),
                (
                    "data".to_string(),
                    StateVolumeMount::new(mount("mnt-b"), digest(0x22)),
                ),
            ])
            .unwrap_err(),
            TopologyError::DuplicateBindingName("data".to_string())
        );

        // An empty binding name is REJECTED.
        assert_eq!(
            ExcludedStateCaptureTopology::try_new([(
                String::new(),
                StateVolumeMount::new(mount("mnt-data"), digest(0x11)),
            )])
            .unwrap_err(),
            TopologyError::EmptyBindingName
        );
    }

    // --- Blocker 1: `match_against` proves only STRUCTURAL contract match, never
    // authenticity; a ContractMatchedSessionStateReceiptV1 is obtainable ONLY by
    // reconciling against the verified Execution Contract, and a self-declared
    // execution_id / foreign or tampered facet / empty generation / unverified
    // envelope all fail closed ---
    #[test]
    fn session_receipt_match_against_is_fail_closed() {
        let envelope = crate::contract_fixtures::envelope_for(
            crate::contract_fixtures::sample_execution_contract(),
        );
        let binding = VerifiedExternalStateBinding::from_verified_envelope(&envelope, "data")
            .expect("declared binding resolves");
        let receipt = plan_production_attach(&binding, &instance("1"))
            .unwrap()
            .session_receipt();

        // Fully-consistent case -> Ok, and the proof exposes the same receipt.
        let verified = receipt
            .clone()
            .match_against(&envelope)
            .expect("a consistent receipt matches its contract");
        assert_eq!(verified.receipt(), &receipt);
        assert_eq!(verified.into_receipt(), receipt);

        // AUTHENTICITY GAP (documented, not a bug): contract match is NOT authenticity.
        // A hand-authored receipt carrying the SAME identity-bearing facets but a
        // FOREIGN `state_ref` and a different (non-empty) generation still matches,
        // because the Execution Contract and its `execution_id` are public and
        // `match_against` reconciles only the contract facets — never the authenticity
        // of the issuer, the state_ref, or the generation. This is exactly why the type
        // is named ContractMatched (not Verified/Authenticated) and why a security
        // decision needs an authenticated outer receipt (PR-2).
        let mut foreign_state = receipt.clone();
        foreign_state.state_ref = OpaqueStateRef::new("opaque:attacker-forged-ref").unwrap();
        foreign_state.state_generation = "gen_forged".to_string();
        let matched_forged = foreign_state
            .clone()
            .match_against(&envelope)
            .expect("contract match holds for a foreign state_ref — match is not authenticity");
        assert_eq!(
            matched_forged.receipt().state_ref().as_str(),
            "opaque:attacker-forged-ref"
        );

        // execution_id A + binding B: the receipt names a binding absent from the
        // verified contract A -> UnknownBinding.
        let mut foreign_binding = receipt.clone();
        foreign_binding.binding_name = "ghost".to_string();
        assert_eq!(
            foreign_binding.match_against(&envelope).unwrap_err(),
            SessionStateReceiptMatchError::UnknownBinding("ghost".to_string())
        );

        // A + tampered target -> reject.
        let mut bad_target = receipt.clone();
        bad_target.target = guest_path("/var/data");
        assert!(matches!(
            bad_target.match_against(&envelope).unwrap_err(),
            SessionStateReceiptMatchError::TargetMismatch { .. }
        ));

        // A + tampered access -> reject (sample binding is read-write).
        let mut bad_access = receipt.clone();
        bad_access.access = ExternalStateAccess::ReadOnly;
        assert!(matches!(
            bad_access.match_against(&envelope).unwrap_err(),
            SessionStateReceiptMatchError::AccessMismatch { .. }
        ));

        // A + tampered schema_identity -> reject.
        let mut bad_schema = receipt.clone();
        bad_schema.schema_identity = "2".to_string();
        assert!(matches!(
            bad_schema.match_against(&envelope).unwrap_err(),
            SessionStateReceiptMatchError::SchemaIdentityMismatch { .. }
        ));

        // Empty generation -> reject.
        let mut empty_generation = receipt.clone();
        empty_generation.state_generation = String::new();
        assert_eq!(
            empty_generation.match_against(&envelope).unwrap_err(),
            SessionStateReceiptMatchError::EmptyGeneration
        );

        // Self-declared execution_id that does not equal the verified identity ->
        // reject (the core "carry a foreign identity string" attack).
        let mut wrong_identity = receipt.clone();
        wrong_identity.execution_id =
            ExecutionId::new(format!("blake3:{}", "b".repeat(64))).expect("valid id shape");
        assert!(matches!(
            wrong_identity.match_against(&envelope).unwrap_err(),
            SessionStateReceiptMatchError::ExecutionIdentityMismatch { .. }
        ));

        // Unverified / tampered envelope -> reject (even a fully consistent receipt
        // cannot be verified against a contract that does not verify).
        let mut tampered_envelope = crate::contract_fixtures::envelope_for(
            crate::contract_fixtures::sample_execution_contract(),
        );
        tampered_envelope.execution_id =
            ExecutionId::new(format!("blake3:{}", "e".repeat(64))).expect("valid id shape");
        assert_eq!(
            receipt.match_against(&tampered_envelope).unwrap_err(),
            SessionStateReceiptMatchError::UnverifiedContract
        );
    }
}
