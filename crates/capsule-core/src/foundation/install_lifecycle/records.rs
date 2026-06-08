//! Install-output identity records (RFC: Ato Resource Namespace §"Install
//! Outputs and Launch Reuse"; tracked by issue #581).
//!
//! These are the immutable, source-only records that an *install* produces.
//! They are deliberately split so the identity boundaries the RFC requires can
//! never be collapsed into one mutable object:
//!
//! ```text
//! ArtifactBuild            build-artifact cache identity (no session/runtime facts)
//! RequirementGraphSnapshot compiled application requirements (typed + hashable)
//! StateContractSnapshot    state/storage expected shape + hash
//! InstallReceipt           what install resolved / generated (NOT the execution receipt)
//! InstallRevision          immutable revision binding all of the above together
//! ```
//!
//! Identity separation enforced here:
//!
//! - [`ArtifactBuildId`] is content-addressed from build inputs only
//!   ([`ArtifactBuildIdentityInputs`]). It must never embed a session id,
//!   dynamic port, process / container id, live route, log cursor, observed
//!   status, timestamp, or secret value.
//! - [`InstallRevisionId`] (the revision identity) is distinct from
//!   [`ArtifactBuildId`] (the build identity): one immutable revision points at
//!   one build, but the two id spaces never alias (`rev_…` vs `build_…`).
//! - The per-session [`super::materialization::LaunchMaterializationRecord`] is
//!   the *only* place runtime projections are recorded; nothing in this file
//!   carries observed facts.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::hashing::canonical_hash;
use super::ids::{ArtifactBuildId, InstallProfileKey, InstallRevisionId};

// ── ArtifactBuild ───────────────────────────────────────────────────────────

/// The *only* inputs allowed to influence an [`ArtifactBuildId`].
///
/// Every field here describes the build artifact itself — its source, its
/// resolved dependencies, its output, and the platform it targets. There is
/// intentionally no field for any session-specific or runtime-observed fact;
/// that exclusion is the load-bearing invariant of the artifact-build identity
/// and is asserted by [`tests::artifact_build_id_excludes_runtime_observed_fields`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactBuildIdentityInputs {
    /// Canonical capsule reference (e.g. `"ato.run/<publisher>/<app>/<version>"`
    /// or `"github.com/<owner>/<repo>/<commit>"`). Not a live route.
    pub capsule_ref: String,
    /// Reference to source provenance (git commit, release tag, …). Not a secret.
    pub source_provenance_ref: String,
    /// Content hash of the source tree (`blake3:<hex>` or similar).
    pub source_tree_hash: String,
    /// Content hash of resolved dependency outputs, if the build has any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency_output_hash: Option<String>,
    /// Content hash of the produced output artifact.
    pub output_content_hash: String,
    /// Build platform profile (`"linux/x86_64"`, `"wasm32-unknown"`, …).
    pub platform: String,
}

/// Content-address an [`ArtifactBuildId`] from its build inputs.
///
/// Shape: `build_<64 hex>` where the hex is `SHA256(JCS(inputs))`. Deterministic
/// for the same inputs, so re-building the same source/deps/output is idempotent.
/// Because the only argument is [`ArtifactBuildIdentityInputs`], it is
/// structurally impossible for a session id, port, route, or observed status to
/// change the resulting id.
pub fn derive_artifact_build_id(inputs: &ArtifactBuildIdentityInputs) -> Result<ArtifactBuildId> {
    let canonical = serde_jcs::to_vec(inputs)?;
    let digest = Sha256::digest(&canonical);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    Ok(ArtifactBuildId::new(format!("build_{hex}")))
}

/// Build / materialization output identity record.
///
/// Session-independent (`Session 固有か = No` in the RFC table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactBuild {
    pub artifact_build_id: ArtifactBuildId,
    /// Canonical capsule reference this build came from. `None` when the caller
    /// did not supply it (never an in-band `"unknown"` sentinel).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capsule_ref: Option<String>,
    /// Source provenance reference (git commit / release tag, or the registry
    /// content hash for a pre-built artifact). `None` when unresolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_provenance_ref: Option<String>,
    /// Content-addressed output artifact ref (e.g. `/artifacts/blake3/<hex>`).
    /// `None` when no content hash is known yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_ref: Option<String>,
    /// Content hash of the produced output. `None` when unresolved — never a
    /// `"unset"` sentinel, so a reader can never mistake a placeholder for a
    /// real hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_content_hash: Option<String>,
    /// Content hash of resolved dependency outputs, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dependency_output_hash: Option<String>,
    /// Build platform profile (`"linux/x86_64"`, `"wasm32-unknown"`, …).
    ///
    /// Descriptive metadata only — the build identity is `artifact_build_id`
    /// (content-addressed by the producer); `ArtifactBuild` is never hashed, so
    /// this field never feeds an id or cache key. (The id-derivation path uses
    /// [`ArtifactBuildIdentityInputs::platform`] instead.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// Reference to the build receipt (not the execution receipt).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_receipt_ref: Option<String>,
    /// RFC 3339 creation timestamp. This is build-metadata, *not* a hash input.
    pub created_at: String,
}

// ── RequirementGraphSnapshot ─────────────────────────────────────────────────

/// Kind of an application requirement node.
///
/// Minimal but explicitly typed — the full Application Requirement Graph
/// (RFC PR 12) is out of scope for this wave, but the snapshot must be a typed,
/// hashable value rather than opaque JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementKind {
    Runtime,
    Storage,
    Network,
    Secret,
    Auth,
    Input,
    Output,
    Device,
    Service,
    Policy,
    Io,
}

/// A typed edge relation between two requirement nodes (RFC `RequirementRelation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementRelation {
    DependsOn,
    CoLocateWith,
    Consumes,
    Produces,
    Exposes,
    RequiresConsent,
    MustUseSameRunner,
    MustNotUseRunner,
    RequiresNetworkPolicy,
    RequiresSecretProjection,
}

/// One requirement node in the compiled graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequirementGraphNode {
    pub id: String,
    pub kind: RequirementKind,
    pub name: String,
    /// Stable, declaration-only attributes (e.g. `"persistence" => "durable"`).
    /// Never holds a secret value or a runtime-observed fact.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, String>,
    pub required: bool,
}

/// A typed edge between two requirement nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequirementGraphEdge {
    pub from: String,
    pub to: String,
    pub relation: RequirementRelation,
}

/// A typed, hashable compiled application-requirement graph.
///
/// Node / edge ordering is significant for [`RequirementGraph::graph_hash`];
/// callers are expected to emit nodes and edges in a normalized order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RequirementGraph {
    pub graph_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<RequirementGraphNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<RequirementGraphEdge>,
}

impl RequirementGraph {
    /// `blake3:<hex>` over the canonical form of the graph.
    pub fn graph_hash(&self) -> Result<String> {
        canonical_hash(self)
    }
}

/// Why a compiled requirement graph is not yet complete (#581 wave 3A).
///
/// Typed so a partial graph can never be mistaken for a complete one and so the
/// specific missing analysis is auditable. Not a hash input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementGraphCompletenessReason {
    RuntimeRequirementNotCompiled,
    EntrypointRequirementNotCompiled,
    StateContractsNotAnalyzed,
    NetworkPolicyNotAnalyzed,
    SecretRequirementsNotAnalyzed,
    StorageRequirementsNotAnalyzed,
    ManifestFactsUnavailable,
    ProfileFactsUnavailable,
}

/// How complete a compiled [`RequirementGraphSnapshot`] is.
///
/// A snapshot is `Complete` only when every requirement class was analyzed.
/// The standard install path is `Partial` (no parsed manifest yet) and must
/// never be presented as `Complete`. This is **not** a `graph_hash` input —
/// `graph_hash` is over `graph` only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RequirementGraphCompleteness {
    Complete,
    Partial {
        reasons: Vec<RequirementGraphCompletenessReason>,
    },
}

impl Default for RequirementGraphCompleteness {
    /// Conservative default for snapshots written before the completeness field
    /// existed (or constructed without explicit completeness): not `Complete`.
    fn default() -> Self {
        Self::Partial {
            reasons: Vec::new(),
        }
    }
}

impl RequirementGraphCompleteness {
    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Complete)
    }
}

/// A stable snapshot of compiled application requirements + profile defaults.
///
/// Session-independent. The `graph_hash` and `profile_defaults_hash` are the
/// values that flow into a [`super::launch_template::LaunchTemplateKey`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequirementGraphSnapshot {
    pub snapshot_id: String,
    pub graph: RequirementGraph,
    pub graph_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision_ref: Option<String>,
    pub profile_defaults_hash: String,
    /// How complete the compiled graph is. Defaults (for pre-3A snapshots) to
    /// `Partial { reasons: [] }` — never silently `Complete`. Not a `graph_hash`
    /// input (but it IS a [`requirement_graph_snapshot_hash`](Self::requirement_graph_snapshot_hash) input).
    #[serde(default)]
    pub completeness: RequirementGraphCompleteness,
    /// Snapshot-level identity (`blake3:<hex>`) binding `graph_hash` +
    /// `profile_defaults_hash` + `completeness` (#581 wave 3B). This — NOT
    /// `graph_hash` alone — is what launch-template identity keys on, so a
    /// `Partial` graph can never be reused as if it were `Complete`.
    ///
    /// `#[serde(default)]` is the empty string for pre-3B snapshots on disk. An
    /// empty hash is NOT equivalent to any real snapshot (a real `blake3:` hash
    /// is never empty) and must be recomputed via
    /// [`recompute_snapshot_hash`](Self::recompute_snapshot_hash) before use for
    /// launch-template identity — never silently treated as equivalent. Never a
    /// hash of secret values or observed diagnostics.
    #[serde(default)]
    pub requirement_graph_snapshot_hash: String,
}

impl RequirementGraphSnapshot {
    /// Build a snapshot, computing `graph_hash` from `graph` and the
    /// snapshot-level hash from `graph_hash` + `profile_defaults_hash` +
    /// completeness. Completeness defaults to `Partial { reasons: [] }`; set it
    /// (and recompute the snapshot hash) with
    /// [`RequirementGraphSnapshot::with_completeness`].
    pub fn new(
        snapshot_id: impl Into<String>,
        graph: RequirementGraph,
        source_revision_ref: Option<String>,
        profile_defaults_hash: impl Into<String>,
    ) -> Result<Self> {
        let graph_hash = graph.graph_hash()?;
        let profile_defaults_hash = profile_defaults_hash.into();
        let completeness = RequirementGraphCompleteness::default();
        let requirement_graph_snapshot_hash = compute_requirement_graph_snapshot_hash(
            &graph_hash,
            &profile_defaults_hash,
            &completeness,
        )?;
        Ok(Self {
            snapshot_id: snapshot_id.into(),
            graph,
            graph_hash,
            source_revision_ref,
            profile_defaults_hash,
            completeness,
            requirement_graph_snapshot_hash,
        })
    }

    /// Set the typed completeness and recompute the snapshot-level hash
    /// (consuming builder). Recomputing is mandatory: completeness is a
    /// snapshot-hash input, so a stale hash would make a `Partial` graph look
    /// identical to a `Complete` one.
    pub fn with_completeness(mut self, completeness: RequirementGraphCompleteness) -> Result<Self> {
        self.requirement_graph_snapshot_hash = compute_requirement_graph_snapshot_hash(
            &self.graph_hash,
            &self.profile_defaults_hash,
            &completeness,
        )?;
        self.completeness = completeness;
        Ok(self)
    }

    /// Recompute `requirement_graph_snapshot_hash` from the current fields. Use
    /// after deserializing a pre-3B snapshot (empty snapshot hash) before using
    /// it for launch-template identity.
    pub fn recompute_snapshot_hash(&mut self) -> Result<()> {
        self.requirement_graph_snapshot_hash = compute_requirement_graph_snapshot_hash(
            &self.graph_hash,
            &self.profile_defaults_hash,
            &self.completeness,
        )?;
        Ok(())
    }

    /// True if the snapshot carries a (non-empty) snapshot-level hash.
    pub fn has_snapshot_hash(&self) -> bool {
        !self.requirement_graph_snapshot_hash.is_empty()
    }
}

/// Compute the snapshot-level identity hash binding graph-content identity +
/// profile defaults + completeness (#581 wave 3B).
///
/// This is what launch-template identity keys on — NOT `graph_hash` alone — so a
/// `Partial` graph can never be reused as a `Complete` one. Completeness reasons
/// are sorted + de-duplicated so reason order never affects identity. By
/// construction the inputs are content/config/completeness hashes only: no
/// session id, port, pid, container id, route, log cursor, observed status,
/// timestamp, or secret value. `source_revision_ref` is intentionally excluded —
/// the revision is keyed separately in
/// [`super::launch_template::LaunchTemplateKey`], and this hash is the identity
/// of the *requirements + completeness*, not provenance.
pub fn compute_requirement_graph_snapshot_hash(
    graph_hash: &str,
    profile_defaults_hash: &str,
    completeness: &RequirementGraphCompleteness,
) -> Result<String> {
    let normalized = match completeness {
        RequirementGraphCompleteness::Complete => RequirementGraphCompleteness::Complete,
        RequirementGraphCompleteness::Partial { reasons } => {
            let mut reasons = reasons.clone();
            reasons.sort();
            reasons.dedup();
            RequirementGraphCompleteness::Partial { reasons }
        }
    };
    canonical_hash(&(
        "ato.requirement_graph_snapshot.v1",
        graph_hash,
        profile_defaults_hash,
        &normalized,
    ))
}

// ── StateContractSnapshot ────────────────────────────────────────────────────

/// Expected shape + hash for one instance-scoped state/storage contract.
///
/// Session-independent. Storage *credentials* are never recorded here — only
/// the contract name and the expected shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateContractSnapshot {
    pub contract_name: String,
    /// Hash of the expected state shape (`blake3:<hex>`).
    pub expected_shape_hash: String,
    /// Combined hash binding `contract_name` + `expected_shape_hash`.
    pub state_contract_hash: String,
}

impl StateContractSnapshot {
    pub fn new(
        contract_name: impl Into<String>,
        expected_shape_hash: impl Into<String>,
    ) -> Result<Self> {
        let contract_name = contract_name.into();
        let expected_shape_hash = expected_shape_hash.into();
        let state_contract_hash =
            canonical_hash(&(contract_name.as_str(), expected_shape_hash.as_str()))?;
        Ok(Self {
            contract_name,
            expected_shape_hash,
            state_contract_hash,
        })
    }
}

/// Combine a set of state-contract snapshots into one stable hash, suitable for
/// the `state_contract_hash` slot of a [`super::launch_template::LaunchTemplateKey`].
///
/// Order-independent: the per-contract hashes are sorted before combining.
pub fn combined_state_contract_hash(contracts: &[StateContractSnapshot]) -> Result<String> {
    let mut hashes: Vec<&str> = contracts
        .iter()
        .map(|c| c.state_contract_hash.as_str())
        .collect();
    hashes.sort_unstable();
    canonical_hash(&hashes)
}

// ── InstallReceipt ────────────────────────────────────────────────────────────

/// Audit record of what an install resolved / generated.
///
/// This is **not** the execution receipt (`engine::execution_identity::ExecutionReceipt`).
/// It records install-time facts only; runtime / observed facts never appear here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallReceipt {
    pub receipt_id: String,
    pub install_profile_key: InstallProfileKey,
    pub install_revision_id: InstallRevisionId,
    pub artifact_build_id: ArtifactBuildId,
    /// References to resources resolved during install (storage, secret refs, …).
    /// References only — never secret values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolved_input_refs: Vec<String>,
    /// Output content hashes produced by the install.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_hashes: Vec<String>,
    /// RFC 3339 timestamp of when the install completed.
    pub occurred_at: String,
}

// ── InstallRevision ───────────────────────────────────────────────────────────

/// An immutable installed-output revision.
///
/// Ties together the build artifact, the compiled requirement graph, the state
/// contracts, and the install receipt under one [`InstallRevisionId`]. Launch
/// templates and the compatibility index are attached in Stage 2 (they are
/// additive, session-independent install outputs); see
/// [`super::launch_template`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRevision {
    pub install_revision_id: InstallRevisionId,
    pub install_profile_key: InstallProfileKey,
    pub artifact_build_id: ArtifactBuildId,
    pub requirement_graph: RequirementGraphSnapshot,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state_contracts: Vec<StateContractSnapshot>,
    pub install_receipt: InstallReceipt,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
    /// Reusable, session-independent launch templates frozen at install time.
    /// Additive: pre-Stage-2 revisions deserialize with an empty vec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub launch_templates: Vec<super::launch_template::LaunchTemplate>,
    /// Runner-class / capability precheck frozen at install time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility_index: Option<super::launch_template::CompatibilityIndex>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_inputs() -> ArtifactBuildIdentityInputs {
        ArtifactBuildIdentityInputs {
            capsule_ref: "ato.run/acme/pgweb/1.2.3".into(),
            source_provenance_ref: "github.com/acme/pgweb@abc123".into(),
            source_tree_hash: "blake3:1111".into(),
            dependency_output_hash: Some("blake3:2222".into()),
            output_content_hash: "blake3:3333".into(),
            platform: "linux/x86_64".into(),
        }
    }

    // ── Acceptance: artifact_build_id excludes runtime/session/observed fields ─

    #[test]
    fn artifact_build_id_excludes_runtime_observed_fields() {
        // Derive the build id from build inputs only.
        let inputs = sample_inputs();
        let id_before_any_session = derive_artifact_build_id(&inputs).unwrap();

        // Now a session "runs": it allocates a dynamic port, gets a pid, a
        // container id, a live route, a log cursor, an observed status, and a
        // timestamp. None of these are arguments to `derive_artifact_build_id`,
        // so re-deriving from the *same build inputs* must yield the same id.
        let _observed = (
            "ses_01",
            53124u16,             // dynamic port
            4242u32,              // process id
            "ctr_deadbeef",       // container id
            "https://live.route", // live route
            "cursor:9000",        // log cursor
            "running",            // observed status
            "2026-06-08T00:00:00Z",
        );
        let id_after_session = derive_artifact_build_id(&inputs).unwrap();

        assert_eq!(
            id_before_any_session, id_after_session,
            "artifact_build_id must depend only on build inputs, never on runtime/session/observed facts"
        );
        assert!(
            id_after_session.is_valid(),
            "must be a well-formed build_<64hex>"
        );
        assert!(
            !crate::foundation::install_lifecycle::ids::ExecutionId::looks_like(
                id_after_session.as_str()
            ),
            "artifact_build_id must not resemble an execution_id"
        );
    }

    #[test]
    fn artifact_build_id_changes_when_build_input_changes() {
        let base = derive_artifact_build_id(&sample_inputs()).unwrap();

        let mut diff_source = sample_inputs();
        diff_source.source_tree_hash = "blake3:9999".into();
        assert_ne!(base, derive_artifact_build_id(&diff_source).unwrap());

        let mut diff_output = sample_inputs();
        diff_output.output_content_hash = "blake3:dddd".into();
        assert_ne!(base, derive_artifact_build_id(&diff_output).unwrap());

        let mut diff_platform = sample_inputs();
        diff_platform.platform = "wasm32-unknown".into();
        assert_ne!(base, derive_artifact_build_id(&diff_platform).unwrap());
    }

    // ── Acceptance: install revision identity distinct from artifact build id ──

    #[test]
    fn install_revision_identity_distinct_from_artifact_build_identity() {
        use crate::foundation::install_lifecycle::ids::{
            InstalledAppId, ProfileId, derive_install_profile_key, revision_id_for_build,
        };

        let build_id = derive_artifact_build_id(&sample_inputs()).unwrap();
        let revision_id = revision_id_for_build(&build_id);

        // Distinct id spaces: build_… vs rev_…
        assert!(build_id.as_str().starts_with("build_"));
        assert!(revision_id.as_str().starts_with("rev_"));
        assert_ne!(
            build_id.as_str(),
            revision_id.as_str(),
            "revision identity must not alias the build identity"
        );

        let app = InstalledAppId::new("app_pgweb");
        let profile = ProfileId::new("default");
        let ipk = derive_install_profile_key(&app, &profile);

        let graph = RequirementGraph {
            graph_id: "g1".into(),
            nodes: vec![RequirementGraphNode {
                id: "runtime".into(),
                kind: RequirementKind::Runtime,
                name: "server_process".into(),
                attributes: BTreeMap::new(),
                required: true,
            }],
            edges: vec![],
        };
        let snapshot =
            RequirementGraphSnapshot::new("snap1", graph, None, "blake3:profdef").unwrap();

        let revision = InstallRevision {
            install_revision_id: revision_id.clone(),
            install_profile_key: ipk.clone(),
            artifact_build_id: build_id.clone(),
            requirement_graph: snapshot,
            state_contracts: vec![],
            install_receipt: InstallReceipt {
                receipt_id: "irecpt_1".into(),
                install_profile_key: ipk,
                install_revision_id: revision_id.clone(),
                artifact_build_id: build_id.clone(),
                resolved_input_refs: vec!["/secrets/sec_db".into()],
                output_hashes: vec!["blake3:3333".into()],
                occurred_at: "2026-06-08T00:00:00Z".into(),
            },
            created_at: "2026-06-08T00:00:00Z".into(),
            launch_templates: vec![],
            compatibility_index: None,
        };

        // The revision references the build id but is identified by the revision id.
        assert_eq!(revision.artifact_build_id, build_id);
        assert_eq!(revision.install_revision_id, revision_id);
        assert_ne!(
            revision.install_revision_id.as_str(),
            revision.artifact_build_id.as_str()
        );
    }

    #[test]
    fn requirement_graph_hash_is_deterministic_and_content_sensitive() {
        let g = RequirementGraph {
            graph_id: "g".into(),
            nodes: vec![RequirementGraphNode {
                id: "n1".into(),
                kind: RequirementKind::Storage,
                name: "user_data".into(),
                attributes: BTreeMap::from([("persistence".into(), "durable".into())]),
                required: true,
            }],
            edges: vec![],
        };
        assert_eq!(g.graph_hash().unwrap(), g.graph_hash().unwrap());

        let mut g2 = g.clone();
        g2.nodes[0].required = false;
        assert_ne!(g.graph_hash().unwrap(), g2.graph_hash().unwrap());
    }

    // ── #581 wave 3B: snapshot-level hash (completeness-aware) ──────────────

    fn sample_graph() -> RequirementGraph {
        RequirementGraph {
            graph_id: "g".into(),
            nodes: vec![RequirementGraphNode {
                id: "req:profile-defaults".into(),
                kind: RequirementKind::Policy,
                name: "profile-defaults".into(),
                attributes: BTreeMap::new(),
                required: true,
            }],
            edges: vec![],
        }
    }

    #[test]
    fn snapshot_hash_distinguishes_partial_from_complete() {
        let partial = RequirementGraphSnapshot::new("s", sample_graph(), None, "blake3:prof")
            .unwrap()
            .with_completeness(RequirementGraphCompleteness::Partial {
                reasons: vec![RequirementGraphCompletenessReason::ManifestFactsUnavailable],
            })
            .unwrap();
        let complete = RequirementGraphSnapshot::new("s", sample_graph(), None, "blake3:prof")
            .unwrap()
            .with_completeness(RequirementGraphCompleteness::Complete)
            .unwrap();

        // graph_hash is content-only: identical for both (same graph).
        assert_eq!(
            partial.graph_hash, complete.graph_hash,
            "graph_hash must not change when only completeness changes"
        );
        // snapshot hash distinguishes Partial from Complete.
        assert_ne!(
            partial.requirement_graph_snapshot_hash, complete.requirement_graph_snapshot_hash,
            "snapshot hash must distinguish Partial from Complete"
        );
        assert!(
            partial
                .requirement_graph_snapshot_hash
                .starts_with("blake3:")
        );
        assert!(
            complete
                .requirement_graph_snapshot_hash
                .starts_with("blake3:")
        );
    }

    #[test]
    fn snapshot_hash_is_reason_order_independent() {
        let a = RequirementGraphSnapshot::new("s", sample_graph(), None, "blake3:prof")
            .unwrap()
            .with_completeness(RequirementGraphCompleteness::Partial {
                reasons: vec![
                    RequirementGraphCompletenessReason::RuntimeRequirementNotCompiled,
                    RequirementGraphCompletenessReason::NetworkPolicyNotAnalyzed,
                ],
            })
            .unwrap();
        let b = RequirementGraphSnapshot::new("s", sample_graph(), None, "blake3:prof")
            .unwrap()
            .with_completeness(RequirementGraphCompleteness::Partial {
                reasons: vec![
                    RequirementGraphCompletenessReason::NetworkPolicyNotAnalyzed,
                    RequirementGraphCompletenessReason::RuntimeRequirementNotCompiled,
                ],
            })
            .unwrap();
        assert_eq!(
            a.requirement_graph_snapshot_hash, b.requirement_graph_snapshot_hash,
            "completeness reason order must not affect the snapshot hash"
        );
    }

    #[test]
    fn snapshot_hash_changes_when_profile_defaults_change() {
        let a = RequirementGraphSnapshot::new("s", sample_graph(), None, "blake3:prof_a").unwrap();
        let b = RequirementGraphSnapshot::new("s", sample_graph(), None, "blake3:prof_b").unwrap();
        assert_eq!(a.graph_hash, b.graph_hash, "graph content is identical");
        assert_ne!(
            a.requirement_graph_snapshot_hash, b.requirement_graph_snapshot_hash,
            "snapshot hash must change when profile_defaults_hash changes"
        );
    }

    #[test]
    fn snapshot_hash_excludes_source_revision_ref() {
        // The revision is keyed separately in LaunchTemplateKey; two snapshots
        // differing only in source_revision_ref share a snapshot hash.
        let a =
            RequirementGraphSnapshot::new("a", sample_graph(), Some("rev_a".into()), "blake3:p")
                .unwrap();
        let b =
            RequirementGraphSnapshot::new("b", sample_graph(), Some("rev_b".into()), "blake3:p")
                .unwrap();
        assert_eq!(
            a.requirement_graph_snapshot_hash,
            b.requirement_graph_snapshot_hash
        );
    }

    #[test]
    fn recompute_snapshot_hash_repairs_pre_3b_snapshot() {
        let snap = RequirementGraphSnapshot::new("s", sample_graph(), None, "blake3:prof")
            .unwrap()
            .with_completeness(RequirementGraphCompleteness::Complete)
            .unwrap();
        // Simulate a pre-3B on-disk snapshot: empty snapshot hash via serde default.
        let mut json: serde_json::Value = serde_json::to_value(&snap).unwrap();
        json.as_object_mut()
            .unwrap()
            .remove("requirement_graph_snapshot_hash");
        let mut loaded: RequirementGraphSnapshot = serde_json::from_value(json).unwrap();
        assert!(
            !loaded.has_snapshot_hash(),
            "pre-3B snapshot deserializes with an empty (not fabricated) snapshot hash"
        );
        // Conservative: empty hash is not equal to the real one, never silently equivalent.
        assert_ne!(
            loaded.requirement_graph_snapshot_hash,
            snap.requirement_graph_snapshot_hash
        );
        loaded.recompute_snapshot_hash().unwrap();
        assert!(loaded.has_snapshot_hash());
        assert_eq!(
            loaded.requirement_graph_snapshot_hash, snap.requirement_graph_snapshot_hash,
            "recompute reproduces the original snapshot hash from the stable fields"
        );
    }

    #[test]
    fn combined_state_contract_hash_is_order_independent() {
        let a = StateContractSnapshot::new("user_data", "blake3:aaaa").unwrap();
        let b = StateContractSnapshot::new("cache", "blake3:bbbb").unwrap();
        let h1 = combined_state_contract_hash(&[a.clone(), b.clone()]).unwrap();
        let h2 = combined_state_contract_hash(&[b, a]).unwrap();
        assert_eq!(
            h1, h2,
            "combined state contract hash must be order-independent"
        );
    }

    #[test]
    fn records_serde_roundtrip() {
        let build = ArtifactBuild {
            artifact_build_id: derive_artifact_build_id(&sample_inputs()).unwrap(),
            capsule_ref: Some("ato.run/acme/pgweb/1.2.3".into()),
            source_provenance_ref: Some("github.com/acme/pgweb@abc123".into()),
            output_ref: Some("/artifacts/blake3/3333".into()),
            output_content_hash: Some("blake3:3333".into()),
            dependency_output_hash: Some("blake3:2222".into()),
            platform: Some("linux/x86_64".into()),
            build_receipt_ref: None,
            created_at: "2026-06-08T00:00:00Z".into(),
        };
        let json = serde_json::to_string(&build).unwrap();
        let back: ArtifactBuild = serde_json::from_str(&json).unwrap();
        assert_eq!(build, back);

        // A build with no resolved facts serializes without the optional keys
        // (no in-band sentinels) and round-trips to all-None.
        let bare = ArtifactBuild {
            artifact_build_id: derive_artifact_build_id(&sample_inputs()).unwrap(),
            capsule_ref: None,
            source_provenance_ref: None,
            output_ref: None,
            output_content_hash: None,
            dependency_output_hash: None,
            platform: None,
            build_receipt_ref: None,
            created_at: "2026-06-08T00:00:00Z".into(),
        };
        let bare_json = serde_json::to_string(&bare).unwrap();
        assert!(!bare_json.contains("unset") && !bare_json.contains("unknown"));
        assert!(!bare_json.contains("output_content_hash"));
        let bare_back: ArtifactBuild = serde_json::from_str(&bare_json).unwrap();
        assert_eq!(bare, bare_back);
    }
}
