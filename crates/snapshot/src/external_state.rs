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
//!    is ineligible because the live workload requires External State. This is
//!    the analysis the sanctioned
//!    [`crate::acceptance::VerifiedRunningSnapshotEligibility`] production
//!    constructor runs — fail closed, never a caller-supplied bool.
//! 2. **Snapshot exclusion boundary** (§9.2, §17.4): each `snapshot = "exclude"`
//!    binding is backed by a **separate** volume, so its bytes MUST be absent
//!    from every shared Snapshot layer (memory / vmstate / disk).
//!    [`ensure_excluded_state_absent_from_shared_layers`] asserts exactly that.
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

use std::collections::BTreeSet;

use capsule::execution_contract::{
    ContentDigest, ExecutionContractV1, ExternalStateAccess, ExternalStateContract, GuestPath,
};
use capsule::snapshot_manifest::SnapshotManifestV1;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Schema id of the Session Receipt's External-State record wire format.
pub const SESSION_STATE_RECEIPT_V1_SCHEMA: &str = "ato.session.external-state-receipt/v1";

// ---------------------------------------------------------------------------
// 1. Live-workload External-State requirement analysis (RFC §8.3)
// ---------------------------------------------------------------------------

/// Whether a `running` capture of this Capsule is ineligible because its **live
/// workload requires External State**.
///
/// RFC §8.3 fixes the two capture policies: `running` = "the workload requires
/// **no** External State to be live"; `workload_idle` = "the workload requires
/// External State or restore-time bindings". §17.3 restates the test obligation:
/// "`running` captures contain no required External State." §18 confirms the
/// consequence: applications that need real External State "use `workload_idle`
/// or cold launch", not a running capture.
///
/// **Fail-closed reduction.** Any declared External State binding is state the
/// live workload consumes, so its presence makes a `running` capture ineligible.
/// Access mode does **not** weaken this: a `read-only` binding is still a
/// required live attachment. `workload_idle` (the eligible policy for such
/// Capsules) is an independent lifecycle follow-up (#1093) and out of scope here;
/// until it lands, such a build MUST fail closed as ineligible and MUST NOT fall
/// back to a secret-bearing running capture (RFC §8.3).
#[must_use]
pub fn requires_external_state_for_live_workload(contract: &ExecutionContractV1) -> bool {
    !contract.external_state.is_empty()
}

// ---------------------------------------------------------------------------
// 2. Snapshot exclusion boundary (RFC §9.2, §17.4)
// ---------------------------------------------------------------------------

/// The set of content addresses of the External-State volumes that MUST be
/// excluded from every shared Snapshot layer.
///
/// Each `external_state[].snapshot = "exclude"` binding is backed by a
/// **separate** writable volume (RFC §9.2: "`/data` is a separate writable
/// boundary, not part of shared Snapshot layers"). Durable state lives in its own
/// state-volume files attached at restore time — never baked into the shared
/// rootfs/memory/vmstate layers — so its content addresses must never appear
/// among a manifest's shared layer refs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExcludedStateBoundary {
    volume_addresses: BTreeSet<ContentDigest>,
}

impl ExcludedStateBoundary {
    /// The excluded External-State volume addresses to keep out of shared layers.
    #[must_use]
    pub fn new(addresses: impl IntoIterator<Item = ContentDigest>) -> Self {
        Self {
            volume_addresses: addresses.into_iter().collect(),
        }
    }

    /// Whether the boundary excludes no addresses (a Capsule with no External
    /// State). An empty boundary trivially satisfies the exclusion assertion.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.volume_addresses.is_empty()
    }

    /// Whether `address` is one of the excluded External-State volume addresses.
    #[must_use]
    pub fn contains(&self, address: &ContentDigest) -> bool {
        self.volume_addresses.contains(address)
    }
}

/// A fail-closed exclusion violation: an excluded External-State volume address
/// was found inside a shared Snapshot layer.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExclusionViolation {
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

/// Assert that **no** excluded External-State volume address appears in any
/// shared Snapshot layer (memory / vmstate / disk).
///
/// RFC §17.4: "excluded mount bytes are absent from every shared Snapshot layer."
/// This is the structural check that the `snapshot = "exclude"` contract was
/// honored: the candidate's layer refs (the CAS content addresses that make its
/// `snapshot_id` a true content address) must not include any address belonging
/// to an excluded state volume. Fails closed on the first breach.
pub fn ensure_excluded_state_absent_from_shared_layers(
    manifest: &SnapshotManifestV1,
    boundary: &ExcludedStateBoundary,
) -> Result<(), ExclusionViolation> {
    // An empty boundary can never be violated; skip the scan.
    if boundary.is_empty() {
        return Ok(());
    }
    for (layer, refs) in [
        ("memory", &manifest.memory_layer_refs),
        ("vmstate", &manifest.vmstate_layer_refs),
        ("disk", &manifest.disk_layer_refs),
    ] {
        for address in refs {
            if boundary.contains(address) {
                return Err(ExclusionViolation::StateBytesInSharedLayer {
                    layer,
                    address: address.to_string(),
                });
            }
        }
    }
    Ok(())
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
}

/// An **opaque** reference to a concrete External State instance (RFC §9.3:
/// `state_ref = opaque:user-state-ref`).
///
/// It names *which* state without carrying any of its content or secret values.
/// It is validated to the canonical `opaque:<handle>` spelling on construction
/// and deserialization, so a non-opaque (potentially content-bearing) reference
/// can never enter a Receipt. It is a non-identity value (RFC §4.3): it never
/// influences `execution_id`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OpaqueStateRef(String);

impl OpaqueStateRef {
    /// Prefix every opaque reference carries.
    const PREFIX: &'static str = "opaque:";

    /// Validate and wrap a `opaque:<handle>` reference.
    pub fn new(value: impl Into<String>) -> Result<Self, OpaqueStateRefError> {
        let value = value.into();
        match value.strip_prefix(Self::PREFIX) {
            Some(handle) if !handle.is_empty() => Ok(Self(value)),
            _ => Err(OpaqueStateRefError::NotOpaque),
        }
    }

    /// A validation-only synthetic opaque reference for `binding` (RFC §8.4):
    /// disposable acceptance and build may attach only ephemeral synthetic
    /// bindings, never a real state ref.
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

/// A concrete External State instance presented at attach time.
///
/// Everything here **except** the schema identity is a non-identity infrastructure
/// fact (RFC §4.3): the owner id, the volume/binding instance id, the generation,
/// and the opaque ref never change `execution_id` and never enter shared
/// Snapshots. Critically, this type carries **no** data-byte or secret-value
/// field: the raw state bytes and secret values live only in the separate volume
/// the runner attaches, never in a value passed through this pure layer — so they
/// cannot leak into a lockfile, Snapshot, or Receipt via this path.
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

impl ExternalStateInstance {
    /// A validation-only **synthetic ephemeral** instance conforming to
    /// `contract`'s declared schema (RFC §8.3 / §8.4 / §9.2: "build and acceptance
    /// use an empty or synthetic ephemeral volume").
    ///
    /// It connects **no** production owner, user state, secret, or Ato identity:
    /// the owner and volume ids are explicit validation markers and the ref is a
    /// synthetic opaque handle. Because it conforms to the declared schema, it
    /// passes [`plan_attach`] for disposable validation without ever touching real
    /// External State. (For a `running` capture the eligibility proof already
    /// requires an **empty** External State contract, so no attach happens on that
    /// path at all; this helper serves disposable validation and the future
    /// `workload_idle` lane.)
    #[must_use]
    pub fn synthetic_for(contract: &ExternalStateContract) -> Self {
        Self {
            state_ref: OpaqueStateRef::synthetic(&contract.name),
            generation: "synthetic".to_string(),
            schema: contract.schema.clone(),
            owner_id: "validation-only".to_string(),
            volume_id: "validation-only".to_string(),
        }
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
/// It is minted **only** by [`plan_attach`], and only *after* the schema gate has
/// passed — so its mere existence proves the incompatible-schema path fails
/// **before** any attachment is produced (RFC §9.2). It carries only non-secret,
/// non-content facts: the binding name and target and access mode (from the
/// identity-bearing contract), the matched schema identity (compatibility
/// evidence), and the opaque ref + generation of the concrete instance. It never
/// carries owner id, volume id, data bytes, or secret values.
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

/// Plan the attach of a concrete instance to its contract binding, running the
/// **schema gate before attach** (RFC §9.2). Fail closed with
/// [`ExternalStateAttachError::SchemaIncompatible`] when the instance's schema
/// identity does not match the contract's identity-bearing schema — before any
/// [`ExternalStateAttachment`] is produced, so no read-write attach can proceed
/// on an incompatible schema. On success, mints the sanctioned attachment.
pub fn plan_attach(
    contract: &ExternalStateContract,
    instance: &ExternalStateInstance,
) -> Result<ExternalStateAttachment, ExternalStateAttachError> {
    if contract.schema != instance.schema {
        return Err(ExternalStateAttachError::SchemaIncompatible {
            binding: contract.name.clone(),
            expected: contract.schema.clone(),
            found: instance.schema.clone(),
        });
    }
    Ok(ExternalStateAttachment {
        binding_name: contract.name.clone(),
        target: contract.target.clone(),
        access: contract.access,
        schema_identity: contract.schema.clone(),
        state_ref: instance.state_ref.clone(),
        generation: instance.generation.clone(),
    })
}

// ---------------------------------------------------------------------------
// 5. Session Receipt External-State record (RFC §9.3, §12)
// ---------------------------------------------------------------------------

/// The Session Receipt's record of one attached External State binding.
///
/// Records **only** an opaque state reference, the state generation, and
/// non-secret compatibility evidence (binding name, target, access mode, and the
/// matched schema identity). It never carries content, data bytes, secret values,
/// identity assertions, the owner id, or the volume instance id — there is
/// structurally no field for any of those (RFC §9.3, §12 "Receipts MUST redact
/// secret values and identity assertions", §14). The generation is a recorded
/// fact that does not change `execution_id` (RFC §9.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStateReceiptV1 {
    /// Always [`SESSION_STATE_RECEIPT_V1_SCHEMA`].
    pub schema: String,
    /// The identity-bearing binding name.
    pub binding_name: String,
    /// The identity-bearing mount/injection target.
    pub target: GuestPath,
    /// The identity-bearing access mode.
    pub access: ExternalStateAccess,
    /// Non-secret compatibility evidence: the schema identity the instance was
    /// gated against.
    pub schema_identity: String,
    /// Opaque handle — names which state, carries none of its content or secrets.
    pub state_ref: OpaqueStateRef,
    /// Non-identity generation marker (RFC §9.3: "state generation changes do not
    /// change `execution_id`").
    pub state_generation: String,
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

    // --- Opaque ref: only `opaque:<handle>` is accepted ---
    #[test]
    fn opaque_state_ref_rejects_non_opaque_and_empty() {
        assert!(OpaqueStateRef::new("opaque:user-state-ref").is_ok());
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
        // Round-trips through serde in its canonical spelling; a non-opaque wire
        // value fails closed at deserialize.
        let json = serde_json::to_string(&OpaqueStateRef::new("opaque:x").unwrap()).unwrap();
        assert_eq!(json, "\"opaque:x\"");
        assert!(serde_json::from_str::<OpaqueStateRef>("\"raw-secret\"").is_err());
    }

    // --- AC (17.4): a compatible schema attaches successfully ---
    #[test]
    fn compatible_schema_attaches() {
        let contract = state_contract("1", ExternalStateAccess::ReadWrite);
        let attachment =
            plan_attach(&contract, &instance("1")).expect("compatible schema attaches");
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
        let error = plan_attach(&contract, &instance("2")).unwrap_err();
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
        assert!(plan_attach(&contract, &instance("2")).is_err());
    }

    // --- Synthetic ephemeral state conforms to the declared schema (§8.4) ---
    #[test]
    fn synthetic_instance_conforms_and_carries_no_real_state() {
        let contract = state_contract("1", ExternalStateAccess::ReadWrite);
        let synthetic = ExternalStateInstance::synthetic_for(&contract);
        assert_eq!(synthetic.owner_id, "validation-only");
        assert_eq!(synthetic.volume_id, "validation-only");
        assert!(
            synthetic
                .state_ref
                .as_str()
                .starts_with("opaque:synthetic:")
        );
        // It conforms to the declared schema, so disposable validation can attach
        // it without ever touching real External State.
        assert!(plan_attach(&contract, &synthetic).is_ok());
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
        let receipt = plan_attach(&contract, &inst).unwrap().session_receipt();

        assert_eq!(receipt.schema, SESSION_STATE_RECEIPT_V1_SCHEMA);
        assert_eq!(receipt.state_ref.as_str(), "opaque:user-state-ref");
        assert_eq!(receipt.state_generation, "gen_456");
        assert_eq!(receipt.schema_identity, "1");

        // The owner id, the volume id, and any secret value are absent from the
        // serialized receipt — structurally, there is no field to hold them.
        let json = serde_json::to_string(&receipt).unwrap();
        assert!(!json.contains("SECRET-owner-token"));
        assert!(!json.contains("SECRET-volume-key"));
        assert!(!json.contains("owner"));
        assert!(!json.contains("volume_id"));

        // The receipt round-trips through its typed, opaque-validated wire form.
        let parsed: SessionStateReceiptV1 = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, receipt);
    }

    // --- AC (17.3/8.3): any External State makes a live `running` workload ineligible ---
    #[test]
    fn requires_external_state_is_true_iff_a_binding_is_declared() {
        let mut contract = crate::contract_fixtures::sample_execution_contract();
        // The G0-1 sample contract declares one External State binding.
        assert!(!contract.external_state.is_empty());
        assert!(requires_external_state_for_live_workload(&contract));
        // Even a read-only binding is a required live attachment.
        contract.external_state[0].access = ExternalStateAccess::ReadOnly;
        assert!(requires_external_state_for_live_workload(&contract));
        // No binding at all → eligible for a running capture.
        contract.external_state.clear();
        assert!(!requires_external_state_for_live_workload(&contract));
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
        let receipt_a = plan_attach(binding, &inst_a).unwrap().session_receipt();
        let receipt_b = plan_attach(binding, &inst_b).unwrap().session_receipt();

        // The receipts differ only in non-identity generation / ref facets...
        assert_ne!(receipt_a.state_generation, receipt_b.state_generation);
        assert_ne!(receipt_a.state_ref, receipt_b.state_ref);
        // ...while the schema-identity compatibility evidence is identical...
        assert_eq!(receipt_a.schema_identity, receipt_b.schema_identity);
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

    // --- AC (17.4): excluded state bytes are absent from every shared Snapshot layer ---
    #[test]
    fn ensure_excluded_state_absent_scans_all_shared_layers() {
        let manifest = crate::contract_fixtures::sample_snapshot_manifest();
        let memory = manifest.memory_layer_refs[0];
        let vmstate = manifest.vmstate_layer_refs[0];
        let disk = manifest.disk_layer_refs[0];

        // An external-state volume address that is NOT among any shared layer is
        // correctly absent.
        let separate = digest(0x99);
        assert!(!manifest.memory_layer_refs.contains(&separate));
        ensure_excluded_state_absent_from_shared_layers(
            &manifest,
            &ExcludedStateBoundary::new([separate]),
        )
        .expect("a separate volume address is absent from shared layers");

        // If an excluded state volume address leaks into ANY shared layer, it
        // fails closed — checked for memory, vmstate, and disk.
        for (expected_layer, leaked) in [("memory", memory), ("vmstate", vmstate), ("disk", disk)] {
            let err = ensure_excluded_state_absent_from_shared_layers(
                &manifest,
                &ExcludedStateBoundary::new([leaked]),
            )
            .unwrap_err();
            assert_eq!(
                err,
                ExclusionViolation::StateBytesInSharedLayer {
                    layer: expected_layer,
                    address: leaked.to_string(),
                }
            );
        }

        // An empty boundary (a Capsule with no External State) trivially passes.
        ensure_excluded_state_absent_from_shared_layers(
            &manifest,
            &ExcludedStateBoundary::default(),
        )
        .expect("no external state → nothing to exclude");
    }
}
