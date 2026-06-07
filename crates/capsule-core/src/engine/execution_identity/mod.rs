#[path = "env_origin.rs"]
mod env_origin;

mod drift;
mod filesystem_builder;
mod observed;
mod policy_builder;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::engine::execution_graph::ExecutionGraph;
use crate::error::{CapsuleError, Result};
use crate::types::{OciLaunchEnvelope, StateSharing};
pub use drift::{
    DriftClass, DriftError, ReceiptDriftChange, ReceiptDriftReport, diff_receipt_documents,
};
pub use env_origin::{EnvOrigin, default_env_origin};
pub use filesystem_builder::FilesystemIdentityBuilder;
pub use observed::{ObservedLaunchEnvelope, ObservedRuntimeEvidence};
pub use policy_builder::PolicyIdentityBuilder;

pub const EXECUTION_IDENTITY_SCHEMA_VERSION: u32 = 1;
pub const EXECUTION_IDENTITY_SCHEMA_VERSION_V2_EXPERIMENTAL: u32 = 2;
pub const EXECUTION_IDENTITY_CANONICALIZATION: &str = "jcs";
pub const EXECUTION_IDENTITY_HASH_ALGORITHM: &str = "blake3-256";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrackingStatus {
    Known,
    Unknown,
    Untracked,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tracked<T> {
    pub status: TrackingStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl<T> Tracked<T> {
    pub fn known(value: T) -> Self {
        Self {
            status: TrackingStatus::Known,
            value: Some(value),
            reason: None,
        }
    }

    pub fn unknown(reason: impl Into<String>) -> Self {
        Self {
            status: TrackingStatus::Unknown,
            value: None,
            reason: Some(reason.into()),
        }
    }

    pub fn untracked(reason: impl Into<String>) -> Self {
        Self {
            status: TrackingStatus::Untracked,
            value: None,
            reason: Some(reason.into()),
        }
    }

    pub fn not_applicable() -> Self {
        Self {
            status: TrackingStatus::NotApplicable,
            value: None,
            reason: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionIdentityInput {
    pub schema_version: u32,
    pub canonicalization: String,
    pub hash_algorithm: String,
    pub source: SourceIdentity,
    pub dependencies: DependencyIdentity,
    pub runtime: RuntimeIdentity,
    pub environment: EnvironmentIdentity,
    pub filesystem: FilesystemIdentity,
    pub policy: PolicyIdentity,
    pub launch: LaunchIdentity,
    pub reproducibility: ReproducibilityIdentity,
}

impl ExecutionIdentityInput {
    // Each argument corresponds to one of the canonical execution-identity
    // facets pinned by the v1 schema; they don't generalize into a builder
    // without obscuring which facet is which at the call site.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source: SourceIdentity,
        dependencies: DependencyIdentity,
        runtime: RuntimeIdentity,
        environment: EnvironmentIdentity,
        filesystem: FilesystemIdentity,
        policy: PolicyIdentity,
        launch: LaunchIdentity,
        reproducibility: ReproducibilityIdentity,
    ) -> Self {
        Self {
            schema_version: EXECUTION_IDENTITY_SCHEMA_VERSION,
            canonicalization: EXECUTION_IDENTITY_CANONICALIZATION.to_string(),
            hash_algorithm: EXECUTION_IDENTITY_HASH_ALGORITHM.to_string(),
            source,
            dependencies,
            runtime,
            environment,
            filesystem,
            policy,
            launch,
            reproducibility,
        }
    }

    pub fn compute_id(&self) -> Result<ExecutionIdentityDigest> {
        compute_execution_id(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionIdentityDigest {
    pub execution_id: String,
    pub input_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    pub schema_version: u32,
    pub execution_id: String,
    pub computed_at: String,
    pub identity: ExecutionIdentityMetadata,
    pub source: SourceIdentity,
    pub dependencies: DependencyIdentity,
    pub runtime: RuntimeIdentity,
    pub environment: EnvironmentIdentity,
    pub filesystem: FilesystemIdentity,
    pub policy: PolicyIdentity,
    pub launch: LaunchIdentity,
    pub reproducibility: ReproducibilityIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionIdentityMetadata {
    pub canonicalization: String,
    pub hash_algorithm: String,
    pub input_hash: String,
}

impl ExecutionReceipt {
    pub fn from_input(input: ExecutionIdentityInput, computed_at: String) -> Result<Self> {
        let digest = input.compute_id()?;
        Ok(Self {
            schema_version: input.schema_version,
            execution_id: digest.execution_id,
            computed_at,
            identity: ExecutionIdentityMetadata {
                canonicalization: input.canonicalization.clone(),
                hash_algorithm: input.hash_algorithm.clone(),
                input_hash: digest.input_hash,
            },
            source: input.source,
            dependencies: input.dependencies,
            runtime: input.runtime,
            environment: input.environment,
            filesystem: input.filesystem,
            policy: input.policy,
            launch: input.launch,
            reproducibility: input.reproducibility,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIdentity {
    pub source_ref: Tracked<String>,
    pub source_tree_hash: Tracked<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionIdentityInputV2 {
    pub schema_version: u32,
    pub canonicalization: String,
    pub hash_algorithm: String,
    pub source: SourceIdentityV2,
    pub source_provenance: SourceProvenance,
    pub dependencies: DependencyIdentityV2,
    pub runtime: RuntimeIdentityV2,
    pub environment: EnvironmentIdentityV2,
    pub filesystem: FilesystemIdentityV2,
    pub policy: PolicyIdentityV2,
    pub launch: LaunchIdentityV2,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oci: Option<OciLaunchEnvelope>,
    pub local: Option<LocalExecutionLocator>,
    pub reproducibility: ReproducibilityIdentity,
    /// Graph-derived identity in the `Declared` domain (manifest + lock +
    /// policy only, host-independent). Populated in v0.6.0+ when the receipt
    /// builder constructs a declared `ExecutionGraph`. Optional so the field
    /// is additive on existing v2 receipts; readers built before #99 ignore
    /// it. Not part of the JCS execution_id projection — it is a parallel
    /// diagnostic identity surfaced alongside the JCS-derived `execution_id`.
    /// Spec: `docs/execution-identity.md` §"Graph-based execution identity".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_execution_id: Option<String>,
    /// Graph-derived identity in the `Resolved` domain (declared + host
    /// resolution outputs). See `declared_execution_id` for the additivity /
    /// projection-exclusion contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_execution_id: Option<String>,
    /// Graph-derived identity in the `Observed` domain (resolved +
    /// undeclared edges). Reserved for the runtime observation feature
    /// (Phase 4 in the umbrella tracker); always `None` in v0.6.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_execution_id: Option<String>,
}

impl ExecutionIdentityInputV2 {
    // V2 adds source_provenance + local on top of v1's eight facets; like
    // the v1 constructor these are all canonical schema fields, not a place
    // for builder-style indirection.
    //
    // Graph-derived ids (declared/resolved/observed) are NOT positional
    // arguments here — they were added later (#99 / PR-5a) and live behind
    // dedicated `with_*_execution_id` setters so existing call sites keep
    // their argument shape and graph plumbing is opt-in per call site.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source: SourceIdentityV2,
        source_provenance: SourceProvenance,
        dependencies: DependencyIdentityV2,
        runtime: RuntimeIdentityV2,
        environment: EnvironmentIdentityV2,
        filesystem: FilesystemIdentityV2,
        policy: PolicyIdentityV2,
        launch: LaunchIdentityV2,
        local: Option<LocalExecutionLocator>,
        reproducibility: ReproducibilityIdentity,
    ) -> Self {
        Self {
            schema_version: EXECUTION_IDENTITY_SCHEMA_VERSION_V2_EXPERIMENTAL,
            canonicalization: EXECUTION_IDENTITY_CANONICALIZATION.to_string(),
            hash_algorithm: EXECUTION_IDENTITY_HASH_ALGORITHM.to_string(),
            source,
            source_provenance,
            dependencies,
            runtime,
            environment,
            filesystem,
            policy,
            launch,
            oci: None,
            local,
            reproducibility,
            declared_execution_id: None,
            resolved_execution_id: None,
            observed_execution_id: None,
        }
    }

    /// Attach a graph-derived `declared_execution_id` (see field docs).
    pub fn with_declared_execution_id(mut self, id: Option<String>) -> Self {
        self.declared_execution_id = id;
        self
    }

    /// Attach a graph-derived `resolved_execution_id` (see field docs).
    pub fn with_resolved_execution_id(mut self, id: Option<String>) -> Self {
        self.resolved_execution_id = id;
        self
    }

    /// Attach a graph-derived `observed_execution_id` (see field docs).
    /// Always `None` in v0.6.0; the setter exists for forward-compat so
    /// the future observation feature has a typed entry point.
    pub fn with_observed_execution_id(mut self, id: Option<String>) -> Self {
        self.observed_execution_id = id;
        self
    }

    pub fn with_oci_launch_envelope(mut self, envelope: Option<OciLaunchEnvelope>) -> Self {
        self.oci = envelope;
        self
    }

    pub fn compute_id(&self) -> Result<ExecutionIdentityDigest> {
        compute_execution_id_v2(self)
    }
}

// `Eq` is intentionally omitted because `failure_envelope.details` carries
// a `serde_json::Value` (which is `PartialEq` only — `Number` is f64, no
// total order). Receipts are persisted as JSON anyway, so `Eq`-keyed
// containers were never a use case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionReceiptV2 {
    pub schema_version: u32,
    pub execution_id: String,
    pub computed_at: String,
    pub identity: ExecutionIdentityMetadata,
    pub source: SourceIdentityV2,
    pub source_provenance: SourceProvenance,
    pub dependencies: DependencyIdentityV2,
    pub runtime: RuntimeIdentityV2,
    pub environment: EnvironmentIdentityV2,
    pub filesystem: FilesystemIdentityV2,
    pub policy: PolicyIdentityV2,
    pub launch: LaunchIdentityV2,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oci: Option<OciLaunchEnvelope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local: Option<LocalExecutionLocator>,
    pub reproducibility: ReproducibilityIdentity,
    /// Graph-derived `Declared`-domain id; see
    /// [`ExecutionIdentityInputV2::declared_execution_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_execution_id: Option<String>,
    /// Graph-derived `Resolved`-domain id; see
    /// [`ExecutionIdentityInputV2::resolved_execution_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_execution_id: Option<String>,
    /// Graph-derived `Observed`-domain id; always `None` in v0.6.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_execution_id: Option<String>,
    /// Receipt result classification (refs #74, #99). Defaults to
    /// [`ReceiptResultClass::Passed`] for serde back-compat: existing v2
    /// receipts on disk re-deserialize cleanly, and the field is omitted
    /// from successful receipts so the happy-path wire bytes are
    /// unchanged. Populated by the boundary-level
    /// `emit_receipt_on_result` wrapper (`ato-cli`'s
    /// `application::receipt_boundary`).
    #[serde(default, skip_serializing_if = "ReceiptResultClass::is_passed")]
    pub result: ReceiptResultClass,
    /// Typed failure envelope, populated when `result` is
    /// `RecoverableFailure` or `Aborted`. Carries the
    /// `AtoError`-derived diagnostic shape (kind/code/phase/etc.) so
    /// downstream consumers can route on it without re-parsing the
    /// human message. Excluded from the JCS projection — diagnostic
    /// only, never feeds `execution_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_envelope: Option<ReceiptFailureEnvelope>,
    /// Identifier of the runtime that emitted this receipt. Added in
    /// PR-3a; `None` for receipts written before the field existed.
    /// Diagnostic only — excluded from the JCS projection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<ExecutionRunnerIdentity>,
    /// Host fingerprint string of the form `<os>:<arch>:<libc>` captured
    /// at receipt-emit time. Diagnostic; the structured platform facts
    /// are still recorded under `runtime.platform`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_fingerprint: Option<String>,
    /// Whether `graph_receipt` / `node_receipts` / `edge_receipts`
    /// describe the full launch graph or a partial slice. See
    /// [`GraphCompleteness`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_completeness: Option<GraphCompleteness>,
    /// Lifecycle-pass record for the launch graph attached to this
    /// receipt. Distinguishes launch-passed receipts (envelope
    /// resolved) from readiness-passed receipts (workload reached
    /// readiness gate). See [`GraphReceipt`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_receipt: Option<GraphReceipt>,
    /// Per-node projection of the launch graph (#493): one entry per graph
    /// node, populated by [`Self::with_graph_projection`] from the
    /// declared/resolved graph. Empty only for legacy/no-graph launches. See
    /// [`NodeReceipt`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub node_receipts: Vec<NodeReceipt>,
    /// Per-edge projection of the launch graph (#493); see `node_receipts`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edge_receipts: Vec<EdgeReceipt>,
    /// Receipt-safe provider projection evidence for OCI launches (#493,
    /// derived from the #516 provider projection boundary). One entry per
    /// realized service/target. Diagnostic only: like `runner` and
    /// `node_receipts`, this is attached after `from_input` and is **not** part
    /// of the JCS projection, so it never feeds `execution_id`. Carries only
    /// receipt-safe fields (env var *names*, image ref/digest status, mount
    /// targets, ports, network aliases, capability flags) — never resolved env
    /// values, argv, container id, pid, or log path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provider_projections: Vec<OciProviderReceiptEvidence>,
    /// Which execution-identity evidence layers this receipt observed
    /// (refs #495). Distinguishes a declared/resolved-evidence receipt from a
    /// runtime-observed one. `None` for receipts written before the field
    /// existed (back-compat); populated as
    /// [`ObservationScope::declared_resolved`] for graph-backed receipts. See
    /// [`ObservationScope`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_scope: Option<ObservationScope>,
    /// Typed reasons the launch graph is [`GraphCompleteness::Partial`] rather
    /// than `Complete` (refs #494). A `Partial` graph-backed receipt always
    /// carries at least one reason — chiefly that the runtime layer was not
    /// observed. Empty (and omitted) for legacy / non-graph receipts written
    /// before the field existed. See [`GraphCompletenessReason`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub graph_completeness_reasons: Vec<GraphCompletenessReason>,
    /// Runtime observation v1 evidence (#490): the observed launch envelope plus
    /// diagnostic facts, captured post-spawn once the workload reached
    /// readiness. `None` for pre-observation receipts (the default) and for
    /// failed/partial observations. Only [`ObservedRuntimeEvidence::envelope`]
    /// feeds `observed_execution_id`; the diagnostic fields never do. Additive
    /// (serde default) so older receipts round-trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_runtime: Option<ObservedRuntimeEvidence>,
}

impl ExecutionReceiptV2 {
    pub fn from_input(input: ExecutionIdentityInputV2, computed_at: String) -> Result<Self> {
        let digest = input.compute_id()?;
        Ok(Self {
            schema_version: input.schema_version,
            execution_id: digest.execution_id,
            computed_at,
            identity: ExecutionIdentityMetadata {
                canonicalization: input.canonicalization.clone(),
                hash_algorithm: input.hash_algorithm.clone(),
                input_hash: digest.input_hash,
            },
            source: input.source,
            source_provenance: input.source_provenance,
            dependencies: input.dependencies,
            runtime: input.runtime,
            environment: input.environment,
            filesystem: input.filesystem,
            policy: input.policy,
            launch: input.launch,
            oci: input.oci,
            local: input.local,
            reproducibility: input.reproducibility,
            declared_execution_id: input.declared_execution_id,
            resolved_execution_id: input.resolved_execution_id,
            observed_execution_id: input.observed_execution_id,
            result: ReceiptResultClass::Passed,
            failure_envelope: None,
            runner: None,
            host_fingerprint: None,
            graph_completeness: None,
            graph_receipt: None,
            node_receipts: Vec::new(),
            edge_receipts: Vec::new(),
            provider_projections: Vec::new(),
            // Set by the graph-backed builder via `with_observation_scope`;
            // `None` keeps non-graph / legacy paths back-compatible.
            observation_scope: None,
            // Populated by the graph-backed builder once the scope is known;
            // empty keeps legacy receipts back-compatible.
            graph_completeness_reasons: Vec::new(),
            // Populated post-spawn by `with_observation` (#490); `None` keeps
            // pre-observation receipts back-compatible.
            observed_runtime: None,
        })
    }

    /// Attach a result class + failure envelope onto an already-built
    /// receipt. Used by the boundary wrapper to mark the
    /// recoverable-failure / aborted path without rebuilding the receipt
    /// from observers.
    pub fn with_result(
        mut self,
        result: ReceiptResultClass,
        failure_envelope: Option<ReceiptFailureEnvelope>,
    ) -> Self {
        self.result = result;
        self.failure_envelope = failure_envelope;
        self
    }

    /// Stamp the runtime identity (binary name + version) that produced
    /// this receipt. Diagnostic only; never feeds `execution_id`.
    pub fn with_runner(mut self, runner: ExecutionRunnerIdentity) -> Self {
        self.runner = Some(runner);
        self
    }

    /// Stamp a host fingerprint string (`os:arch:libc`). Diagnostic only;
    /// the structured platform facts still live under `runtime.platform`.
    pub fn with_host_fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
        self.host_fingerprint = Some(fingerprint.into());
        self
    }

    /// Stamp the graph completeness label. `Partial` means the attached
    /// `graph_receipt` / `node_receipts` / `edge_receipts` describe a
    /// subset of the launch graph; `Complete` means they cover the full
    /// graph (no production caller emits `Complete` yet).
    pub fn with_graph_completeness(mut self, completeness: GraphCompleteness) -> Self {
        self.graph_completeness = Some(completeness);
        self
    }

    /// Attach a [`GraphReceipt`] lifecycle-pass record.
    pub fn with_graph_receipt(mut self, receipt: GraphReceipt) -> Self {
        self.graph_receipt = Some(receipt);
        self
    }

    /// Populate `node_receipts` and `edge_receipts` from a launch
    /// [`ExecutionGraph`] (#493).
    ///
    /// The receipts are a faithful projection of the declared/resolved launch
    /// graph: one [`NodeReceipt`] per graph node (its identifier + kind) and one
    /// [`EdgeReceipt`] per graph edge (source + target + kind). `status` is left
    /// `None` — this PR derives receipts from the *declared/resolved* graph only;
    /// it does **not** observe runtime lifecycle pass/fail, so claiming a status
    /// would be fabricated evidence. Observed status is future work (#495).
    ///
    /// Node identity comes from the graph (manifest/lock/policy-derived), never
    /// from runtime command strings, container ids, or session-local data — so
    /// re-running the same launch in a new session yields identical receipts.
    pub fn with_graph_projection(mut self, graph: &ExecutionGraph) -> Self {
        self.node_receipts = graph
            .nodes
            .iter()
            .map(|node| NodeReceipt {
                node_identifier: node.identifier().to_string(),
                kind: node.kind_label().to_string(),
                status: None,
            })
            .collect();
        self.edge_receipts = graph
            .edges
            .iter()
            .map(|edge| EdgeReceipt {
                source: edge.source.clone(),
                target: edge.target.clone(),
                kind: edge.kind.kind_label().to_string(),
                status: None,
            })
            .collect();
        self
    }

    /// Attach receipt-safe OCI provider projection evidence (#493, #516).
    pub fn with_provider_projections(
        mut self,
        projections: Vec<OciProviderReceiptEvidence>,
    ) -> Self {
        self.provider_projections = projections;
        self
    }

    /// Fold OCI provider-evidence facts into this receipt's assessment layer
    /// (#501): append the value-free provider-gap completeness reasons and merge
    /// the implied reproducibility causes, then recompute the class conservatively.
    ///
    /// Strictly additive and pre-observation: it never changes
    /// `graph_completeness` (stays `Partial`), `observation_scope`, or
    /// `observed_execution_id`, and merging causes can only *lower*
    /// reproducibility, never raise it. A no-op when there is no provider
    /// evidence (so source-native receipts are unaffected). Call this *after*
    /// `provider_projections` are final.
    pub fn with_oci_provider_assessment(mut self) -> Self {
        if self.provider_projections.is_empty() {
            return self;
        }
        let assessment = assess_oci_provider_projection_facts(&self.provider_projections);
        self.graph_completeness_reasons
            .extend(assessment.completeness_reasons);
        if !assessment.reproducibility_causes.is_empty() {
            self.reproducibility
                .causes
                .extend(assessment.reproducibility_causes);
            self.reproducibility.causes.sort();
            self.reproducibility.causes.dedup();
            self.reproducibility.class =
                ReproducibilityClass::from_causes(&self.reproducibility.causes);
        }
        self
    }

    /// Attach an [`ObservationScope`] describing which evidence layers this
    /// receipt observed (refs #495).
    pub fn with_observation_scope(mut self, scope: ObservationScope) -> Self {
        self.observation_scope = Some(scope);
        self
    }

    /// Attach typed [`GraphCompletenessReason`]s explaining why the launch
    /// graph is `Partial` (refs #494). Typically derived from the receipt's
    /// [`ObservationScope`] via
    /// [`ObservationScope::graph_completeness_reasons`].
    pub fn with_graph_completeness_reasons(
        mut self,
        reasons: Vec<GraphCompletenessReason>,
    ) -> Self {
        self.graph_completeness_reasons = reasons;
        self
    }

    /// Stamp runtime observation v1 (#490) onto an already-built receipt.
    ///
    /// Derives `observed_execution_id` from the evidence's canonical envelope
    /// (never copied from `resolved_execution_id`), flips the observation
    /// scope's runtime layer to [`RuntimeObservation::Observed`] (preserving the
    /// declared/resolved layers), recomputes the completeness reasons, and
    /// refreshes any [`GraphReceipt`]'s observed id. **Keeps
    /// [`GraphCompleteness`] untouched** — observation v1 never emits
    /// `Complete`.
    ///
    /// Call ONLY after a real post-spawn observation that carries minimal
    /// evidence ([`ObservedLaunchEnvelope::has_minimal_evidence`]); a
    /// failed/partial/aborted observation must leave the pre-observation receipt
    /// unchanged (`observed_execution_id` stays `None`).
    pub fn with_observation(mut self, evidence: ObservedRuntimeEvidence) -> Self {
        let observed_id = evidence.envelope.compute_observed_execution_id();
        let scope = match self.observation_scope {
            Some(existing) => ObservationScope {
                observed: RuntimeObservation::Observed,
                ..existing
            },
            None => ObservationScope::declared_resolved_observed(),
        };
        self.graph_completeness_reasons = scope.graph_completeness_reasons();
        self.observation_scope = Some(scope);
        self.observed_execution_id = Some(observed_id.clone());
        if let Some(graph_receipt) = self.graph_receipt.as_mut() {
            graph_receipt.observed_execution_id = Some(observed_id);
        }
        self.observed_runtime = Some(evidence);
        self
    }

    /// Build a partial v2 receipt for a launch that failed before the
    /// full launch envelope was resolved (refs #74, #99).
    ///
    /// `execution_id` is synthetic (`partial:blake3:<digest>`) — content-
    /// addressed over the failure envelope and any graph-derived
    /// identities the boundary already knew. JCS-derived `execution_id`s
    /// require the full launch envelope (source/deps/runtime/env/fs/
    /// policy/launch), so partial receipts use this disjoint id space
    /// to avoid collisions with happy-path JCS ids.
    ///
    /// `computed_at` is intentionally NOT mixed into the hash: two
    /// retries with the same envelope and the same graph state must
    /// produce the same `execution_id` so downstream consumers
    /// (e.g. `ato diff execution`, GC roots) can treat the synthetic
    /// id as content-addressed. If per-attempt uniqueness is needed
    /// in the future it must ride on a separate field, not on
    /// `execution_id`.
    ///
    /// Identity facets are filled with `Tracked::untracked` placeholders
    /// because the launch envelope is by definition incomplete here.
    /// Graph-derived ids (`declared_execution_id`,
    /// `resolved_execution_id`) are populated by the boundary wrapper
    /// when the failure happened *after* the corresponding graph was
    /// built; otherwise they stay `None`.
    pub fn partial_failure(
        computed_at: String,
        result: ReceiptResultClass,
        failure_envelope: ReceiptFailureEnvelope,
        declared_execution_id: Option<String>,
        resolved_execution_id: Option<String>,
        local: Option<LocalExecutionLocator>,
    ) -> Self {
        let envelope_canonical = serde_jcs::to_vec(&failure_envelope).unwrap_or_default();
        let mut hasher = blake3::Hasher::new();
        hasher.update(&envelope_canonical);
        if let Some(id) = declared_execution_id.as_deref() {
            hasher.update(b"declared:");
            hasher.update(id.as_bytes());
        }
        if let Some(id) = resolved_execution_id.as_deref() {
            hasher.update(b"resolved:");
            hasher.update(id.as_bytes());
        }
        let synthetic_id = format!("partial:blake3:{}", hasher.finalize().to_hex());

        let untracked_string =
            || Tracked::untracked("partial receipt: launch envelope not resolved");

        // Annotate evidence from whichever graph ids the boundary already
        // had. Computed before the move into the struct fields below.
        let observation_scope = ObservationScope {
            declared: if declared_execution_id.is_some() {
                LayerEvidence::Present
            } else {
                LayerEvidence::Absent
            },
            resolved: if resolved_execution_id.is_some() {
                LayerEvidence::Present
            } else {
                LayerEvidence::Absent
            },
            // Runtime is never observed on a failed launch.
            observed: RuntimeObservation::NotObserved,
        };
        // Diagnostic reasons the (incomplete) graph evidence is not Complete,
        // derived from the scope before it is moved into the struct below.
        let graph_completeness_reasons = observation_scope.graph_completeness_reasons();

        Self {
            schema_version: EXECUTION_IDENTITY_SCHEMA_VERSION_V2_EXPERIMENTAL,
            execution_id: synthetic_id.clone(),
            computed_at,
            identity: ExecutionIdentityMetadata {
                canonicalization: EXECUTION_IDENTITY_CANONICALIZATION.to_string(),
                hash_algorithm: EXECUTION_IDENTITY_HASH_ALGORITHM.to_string(),
                input_hash: synthetic_id,
            },
            source: SourceIdentityV2 {
                source_tree_hash: untracked_string(),
                manifest_path_role: untracked_string(),
            },
            source_provenance: SourceProvenance {
                kind: SourceProvenanceKind::Unknown,
                git_remote: None,
                git_commit: None,
                registry_ref: None,
            },
            dependencies: DependencyIdentityV2 {
                derivation_hash: untracked_string(),
                output_hash: untracked_string(),
                derivation_inputs: None,
            },
            runtime: RuntimeIdentityV2 {
                declared: None,
                resolved_ref: untracked_string(),
                binary_hash: untracked_string(),
                dynamic_linkage: untracked_string(),
                completeness: RuntimeCompleteness::DeclaredOnly,
                platform: PlatformIdentity {
                    os: std::env::consts::OS.to_string(),
                    arch: std::env::consts::ARCH.to_string(),
                    libc: "unknown".to_string(),
                },
            },
            environment: EnvironmentIdentityV2 {
                entries: Vec::new(),
                fd_layout: Tracked::untracked("partial receipt"),
                umask: untracked_string(),
                ulimits: Tracked::untracked("partial receipt"),
                mode: EnvironmentMode::Untracked,
                ambient_untracked_keys: Vec::new(),
            },
            filesystem: FilesystemIdentityV2 {
                view_hash: untracked_string(),
                partial_view_hash: None,
                source_root: untracked_string(),
                working_directory: untracked_string(),
                readonly_layers: Vec::new(),
                writable_dirs: Vec::new(),
                persistent_state: Vec::new(),
                semantics: FilesystemSemantics {
                    case_sensitivity: Tracked::untracked("partial receipt"),
                    symlink_policy: Tracked::untracked("partial receipt"),
                    tmp_policy: Tracked::untracked("partial receipt"),
                },
            },
            policy: PolicyIdentityV2 {
                network_policy_hash: untracked_string(),
                capability_policy_hash: untracked_string(),
                sandbox_policy_hash: untracked_string(),
            },
            launch: LaunchIdentityV2 {
                entry_point: LaunchEntryPoint::Untracked {
                    reason: "partial receipt: launch envelope not resolved".to_string(),
                },
                argv: Vec::new(),
                working_directory: untracked_string(),
            },
            oci: None,
            local,
            reproducibility: ReproducibilityIdentity {
                class: ReproducibilityClass::BestEffort,
                causes: vec![ReproducibilityCause::LifecycleUnknown],
            },
            declared_execution_id,
            resolved_execution_id,
            observed_execution_id: None,
            result,
            failure_envelope: Some(failure_envelope),
            runner: None,
            host_fingerprint: None,
            graph_completeness: None,
            graph_receipt: None,
            node_receipts: Vec::new(),
            edge_receipts: Vec::new(),
            provider_projections: Vec::new(),
            observation_scope: Some(observation_scope),
            graph_completeness_reasons,
            // A partial/failure receipt never carries runtime observation.
            observed_runtime: None,
        }
    }
}

/// Runner identity (who emitted the receipt). Surfaces the producing
/// binary's name and version so downstream consumers can route on
/// "emitted by ato-cli vs ato-desktop" without parsing the receipt's
/// host_fingerprint.
///
/// Stored optionally on `ExecutionReceiptV2.runner` with serde default
/// to keep v2 receipt back-compat for pre-PR-3a payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRunnerIdentity {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl ExecutionRunnerIdentity {
    pub fn new(name: impl Into<String>, version: Option<String>) -> Self {
        Self {
            name: name.into(),
            version,
        }
    }
}

/// Whether the graph attached to this receipt is the full launch graph
/// (`Complete`) or a subset captured before the launch was fully resolved
/// (`Partial`). Today only `Partial` is emitted — receipts include the
/// declared / resolved / preflight projection, not the observed
/// post-spawn extension — but the variant exists so future waves can
/// upgrade to `Complete` without a schema bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GraphCompleteness {
    Partial,
    Complete,
}

impl GraphCompleteness {
    pub fn as_str(&self) -> &'static str {
        match self {
            GraphCompleteness::Partial => "partial",
            GraphCompleteness::Complete => "complete",
        }
    }
}

/// Evidence state of a declared- or resolved-domain layer in a receipt
/// (refs #495).
///
/// `Present` means the receipt carries that layer's identity evidence (the
/// declared / resolved execution-id facets and the corresponding graph
/// projection). `Absent` means it was not derived for this receipt (e.g. a
/// preflight receipt that never reached host resolution).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LayerEvidence {
    Present,
    Absent,
}

impl LayerEvidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            LayerEvidence::Present => "present",
            LayerEvidence::Absent => "absent",
        }
    }
}

/// Observation state of the runtime (`Observed`-domain) layer (refs #495, #490).
///
/// Runtime observation v1 (#490) adds the [`RuntimeObservation::Observed`]
/// variant: a receipt whose workload spawned, reached readiness, and had its
/// *observed launch envelope* captured ([`ObservedLaunchEnvelope`]). The
/// non-observed variants remain so a reader can still tell "we have
/// declared/resolved evidence but did not observe the runtime" from "we
/// observed the runtime". `Observed` is emitted **only** from a real post-spawn
/// observation; it is never synthesized just because a process started, and a
/// failed/partial observation stays `Deferred` with `observed_execution_id =
/// None`. Observation v1 is envelope-level only — it does **not** upgrade
/// [`GraphCompleteness`] to `Complete` (per-node/edge lifecycle and drift
/// reconciliation remain #521/#522).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeObservation {
    /// Runtime was not observed for this receipt.
    NotObserved,
    /// Runtime observation is intentionally outside this receipt's scope.
    OutOfScope,
    /// Runtime observation is deferred to a later stage/wave (#521/#522).
    Deferred,
    /// The runtime was observed post-spawn at the envelope level (#490): the
    /// receipt carries [`ObservedRuntimeEvidence`] and a real
    /// `observed_execution_id`. Still envelope-level only, so the graph stays
    /// [`GraphCompleteness::Partial`].
    Observed,
}

impl RuntimeObservation {
    pub fn as_str(&self) -> &'static str {
        match self {
            RuntimeObservation::NotObserved => "not-observed",
            RuntimeObservation::OutOfScope => "out-of-scope",
            RuntimeObservation::Deferred => "deferred",
            RuntimeObservation::Observed => "observed",
        }
    }
}

/// What evidence a receipt actually observed, per execution-identity layer
/// (refs #495).
///
/// A v2 receipt mixes facets derived at different stages: `declared`
/// (manifest / lock / policy), `resolved` (after host resolution), and
/// `observed` (runtime observation of the spawned workload). Before #521/#522
/// land NodeReceipt/EdgeReceipt population and the realization classifier, a
/// graph-backed receipt carries declared + resolved evidence only — it has not
/// observed the runtime. `ObservationScope` records that explicitly so a
/// reader never has to infer "runtime not observed" from an absent field.
///
/// This type does **not** synthesize `observed_execution_id` and does **not**
/// upgrade [`GraphCompleteness`] to `Complete`; it only annotates what the
/// receipt does and does not contain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationScope {
    pub declared: LayerEvidence,
    pub resolved: LayerEvidence,
    pub observed: RuntimeObservation,
}

impl ObservationScope {
    /// Scope for a graph-backed receipt that carries declared + resolved
    /// evidence but did not observe the runtime. The runtime layer is
    /// [`RuntimeObservation::NotObserved`]: there is no observation hook in
    /// v0.6.0, so this is a statement of fact, not a promise.
    pub fn declared_resolved() -> Self {
        Self {
            declared: LayerEvidence::Present,
            resolved: LayerEvidence::Present,
            observed: RuntimeObservation::NotObserved,
        }
    }

    /// Scope for a runtime-observed receipt (#490): declared + resolved
    /// evidence plus a real post-spawn observed launch envelope. The graph is
    /// still [`GraphCompleteness::Partial`] — observation v1 is envelope-level
    /// only — so `graph_completeness_reasons` reports
    /// [`GraphCompletenessReason::RuntimeObservationMinimal`] rather than
    /// claiming completeness.
    pub fn declared_resolved_observed() -> Self {
        Self {
            declared: LayerEvidence::Present,
            resolved: LayerEvidence::Present,
            observed: RuntimeObservation::Observed,
        }
    }

    /// Scope for a receipt that only carries declared-domain evidence (e.g.
    /// host resolution never ran). Runtime layer is `NotObserved`.
    pub fn declared_only() -> Self {
        Self {
            declared: LayerEvidence::Present,
            resolved: LayerEvidence::Absent,
            observed: RuntimeObservation::NotObserved,
        }
    }

    /// Whether this scope claims runtime observation (#490). `true` only when
    /// `observed == RuntimeObservation::Observed` — i.e. a real post-spawn
    /// envelope was captured. The non-observed states (`NotObserved`,
    /// `Deferred`, `OutOfScope`) all return `false`, so a reader can never
    /// mistake a deferred/failed observation for a real one.
    pub fn has_runtime_observation(&self) -> bool {
        matches!(self.observed, RuntimeObservation::Observed)
    }

    /// Conservative reasons the graph attached to a receipt is
    /// [`GraphCompleteness::Partial`] rather than `Complete` (refs #494),
    /// derived purely from which evidence layers this scope carries.
    ///
    /// The runtime layer always contributes a reason — `RuntimeObservation`
    /// cannot say `Observed` — so the result is **never empty** while the
    /// runtime is unobserved. That is the structural reason a graph-backed
    /// receipt with this scope can never be `Complete`, *even when its
    /// `node_receipts` / `edge_receipts` are non-empty*: a populated
    /// declared/resolved projection is not runtime observation.
    pub fn graph_completeness_reasons(&self) -> Vec<GraphCompletenessReason> {
        let mut reasons = Vec::new();
        match self.observed {
            RuntimeObservation::NotObserved => {
                reasons.push(GraphCompletenessReason::RuntimeNotObserved)
            }
            RuntimeObservation::Deferred => {
                reasons.push(GraphCompletenessReason::RuntimeObservationDeferred)
            }
            RuntimeObservation::OutOfScope => {
                reasons.push(GraphCompletenessReason::RuntimeObservationOutOfScope)
            }
            // Observed at the envelope level (#490): the runtime *was* observed,
            // so this is not `RuntimeNotObserved` — but the graph is still
            // `Partial` because per-node/edge lifecycle observation and drift
            // reconciliation (#521/#522) have not run. That keeps the
            // "a Partial receipt always carries a reason" invariant true.
            RuntimeObservation::Observed => {
                reasons.push(GraphCompletenessReason::RuntimeObservationMinimal)
            }
        }
        if self.resolved == LayerEvidence::Absent {
            reasons.push(GraphCompletenessReason::ResolvedLayerAbsent);
        }
        if self.declared == LayerEvidence::Absent {
            reasons.push(GraphCompletenessReason::DeclaredLayerAbsent);
        }
        reasons
    }
}

/// Why a receipt's launch graph is [`GraphCompleteness::Partial`] (refs #494).
///
/// Typed — never a free-form string — so receipt readers can branch on the
/// reason. A `Partial` receipt always carries at least one of these. The
/// runtime-observation reasons mirror [`RuntimeObservation`]; the
/// layer-absent reasons mirror an `Absent` [`LayerEvidence`].
///
/// There is deliberately no "complete" reason: this slice never emits
/// [`GraphCompleteness::Complete`]. Runtime observation (#521 lifecycle
/// observations, #522 realization classifier) is what would let a future
/// wave clear these reasons.
// `Copy` is intentionally dropped: the additive `ProviderProjectionIncomplete`
// variant carries owned `String`s (a value-free provider kind + service label).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GraphCompletenessReason {
    /// The runtime was not observed (`ObservationScope.observed ==
    /// NotObserved`): the graph is a declared/resolved projection only.
    RuntimeNotObserved,
    /// Runtime observation is deferred to a later wave (#521/#522).
    RuntimeObservationDeferred,
    /// Runtime observation is intentionally outside this receipt's scope.
    RuntimeObservationOutOfScope,
    /// The runtime was observed at the envelope level (#490), but the graph is
    /// still `Partial`: per-node/edge lifecycle observation and drift
    /// reconciliation (#521/#522) have not run, so it is not yet `Complete`.
    RuntimeObservationMinimal,
    /// The resolved-domain layer was not derived for this receipt.
    ResolvedLayerAbsent,
    /// The declared-domain layer was not derived for this receipt.
    DeclaredLayerAbsent,
    /// A provider projection facet is incomplete before runtime observation
    /// (#501): an unpinned image, an unenforceable declared policy, or a
    /// host-bound mount. This is a declared/resolved-domain provider fact — it
    /// does NOT imply the workload was observed running. All fields are
    /// value-free (a coarse provider kind, an optional service label, and a typed
    /// gap) — never a host path, env value, secret, or command.
    ProviderProjectionIncomplete {
        provider_kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        service_label: Option<String>,
        gap: ProviderProjectionGap,
    },
}

/// A specific way a provider projection is incomplete pre-observation (#501).
/// Typed and value-free, suitable for a [`GraphCompletenessReason`] in a receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderProjectionGap {
    /// The launch image is tag-only / unpinned: its immutable materialized
    /// identity is not known at projection time.
    ImageUnpinned,
    /// A declared network policy is enforced only best-effort by the provider.
    NetworkEnforcementDowngraded,
    /// A declared network policy cannot be enforced by the provider at all
    /// (e.g. podman + an egress allowlist).
    NetworkEnforcementUnsupported,
    /// Whether the provider can enforce the declared network policy is unknown.
    NetworkEnforcementUnknown,
    /// A declared capability/sandbox policy is not fully enforced by the provider.
    CapabilityEnforcementIncomplete,
    /// The projection depends on a host-path bind mount (a host-bound fallback),
    /// rather than engine-managed state.
    HostBoundMount,
}

impl GraphCompletenessReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            GraphCompletenessReason::RuntimeNotObserved => "runtime-not-observed",
            GraphCompletenessReason::RuntimeObservationDeferred => "runtime-observation-deferred",
            GraphCompletenessReason::RuntimeObservationOutOfScope => {
                "runtime-observation-out-of-scope"
            }
            GraphCompletenessReason::RuntimeObservationMinimal => "runtime-observation-minimal",
            GraphCompletenessReason::ResolvedLayerAbsent => "resolved-layer-absent",
            GraphCompletenessReason::DeclaredLayerAbsent => "declared-layer-absent",
            // Coarse label; the specific provider/service/gap live in the typed
            // fields (and the serde representation).
            GraphCompletenessReason::ProviderProjectionIncomplete { .. } => {
                "provider-projection-incomplete"
            }
        }
    }
}

/// Lifecycle-pass record for the launch graph attached to a receipt.
///
/// Replaces the earlier "ready=true on receipt readiness" sentinel by
/// recording WHICH gate (launch / readiness) passed and stamping the
/// declared / resolved / observed execution-id facets at that gate.
/// Receipt readers can therefore tell a launch-passed receipt from a
/// readiness-passed receipt without re-running the gating pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphReceipt {
    /// Which gate produced this record. `"launch-passed"` is stamped
    /// when the launch envelope was resolved (post-preflight, pre-spawn);
    /// `"readiness-passed"` is stamped when the workload reached its
    /// readiness gate (HTTP healthcheck OK, terminal ready, etc.).
    pub gate: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_execution_id: Option<String>,
}

impl GraphReceipt {
    pub fn launch_passed(
        declared_execution_id: Option<String>,
        resolved_execution_id: Option<String>,
        observed_execution_id: Option<String>,
    ) -> Self {
        Self {
            gate: "launch-passed".to_string(),
            declared_execution_id,
            resolved_execution_id,
            observed_execution_id,
        }
    }

    pub fn readiness_passed(
        declared_execution_id: Option<String>,
        resolved_execution_id: Option<String>,
        observed_execution_id: Option<String>,
    ) -> Self {
        Self {
            gate: "readiness-passed".to_string(),
            declared_execution_id,
            resolved_execution_id,
            observed_execution_id,
        }
    }
}

/// Per-node receipt entry: one node of the launch graph projected into the
/// receipt (#493).
///
/// Populated by [`ExecutionReceiptV2::with_graph_projection`] from the
/// declared/resolved launch graph — `node_identifier` and `kind` come from the
/// graph node, never from runtime command strings or session-local data.
/// `status` stays `None`: this is a declared/resolved-graph projection, not a
/// runtime observation; per-node lifecycle pass/fail status remains future work
/// (#495). Legacy paths with no graph available emit an empty list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeReceipt {
    pub node_identifier: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// Per-edge receipt entry: one edge of the launch graph projected into the
/// receipt (#493). See [`NodeReceipt`] for the declared/resolved-vs-observed
/// distinction — `status` is likewise `None` until observation lands (#495).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EdgeReceipt {
    pub source: String,
    pub target: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// Receipt-safe provider projection evidence for one OCI service/target (#493).
///
/// This is the receipt-facing summary of the #516 provider projection boundary:
/// it records *what the provider was asked to realize* for a Capsule, without
/// claiming runtime observation and without leaking secrets. Producers
/// (`ato-cli`'s `OciProjectionPlan::receipt_evidence`) MUST keep this
/// receipt-safe:
///
/// * env vars appear as **names only** ([`Self::env_keys`]) — never values;
/// * the image is recorded as a reference + a pinned/unpinned digest *status*;
/// * mounts record their *target* and flags, never source host paths;
/// * **excluded entirely**: resolved env values, argv/command strings, the
///   requested container name, container id, provider pid, and log paths.
///   Those are session-local provider evidence, not Capsule identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OciProviderReceiptEvidence {
    /// Provider class, e.g. `"oci"`.
    pub provider_kind: String,
    /// Concrete realizer name, e.g. `"podman"`.
    pub provider_name: String,
    /// The image reference as launched (`repo:tag` or `repo@sha256:…`).
    pub image_reference: String,
    /// How well the image is pinned. A digest is image evidence, not identity.
    pub image_digest_status: OciImageDigestStatus,
    /// Target platform, e.g. `"linux/amd64"`, when an override is declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// Environment variable **names** projected into the container, sorted.
    /// Values are never recorded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env_keys: Vec<String>,
    /// Mount projections — target + flags only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<OciMountReceiptEvidence>,
    /// Published container ports (declared container port + protocol).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<OciPortReceiptEvidence>,
    /// Service network aliases this container answers to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub network_aliases: Vec<String>,
    /// Names of the provider capabilities these launch conditions require,
    /// sorted (e.g. `"persistent-state"`, `"network-policy"`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities_required: Vec<String>,
    /// Provider version/family when known (e.g. `"oci-podman-v1"`). Coarse and
    /// value-free; never a host path or machine handle. Additive (#501).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_version: Option<String>,
    /// Whether the selected provider can enforce the *declared* network policy
    /// (e.g. an egress allowlist). `Unknown` by default for back-compat (#501).
    #[serde(default, skip_serializing_if = "OciEnforcementStatus::is_unknown")]
    pub network_enforcement_status: OciEnforcementStatus,
    /// Whether the selected provider can enforce the declared capability/sandbox
    /// policy. `Unknown` by default for back-compat (#501).
    #[serde(default, skip_serializing_if = "OciEnforcementStatus::is_unknown")]
    pub capability_enforcement_status: OciEnforcementStatus,
    /// The derived provider invocation (e.g. `podman create` argv) reduced to a
    /// **redacted** shape: flags survive, every value becomes `<redacted>`. This
    /// is derived projection evidence, never identity and never a raw command
    /// (#501). Optional/additive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived_command_redacted: Vec<String>,
    /// Value-free service/target label identifying which orchestrated service
    /// this evidence record describes. `None` for a single-target launch; set per
    /// service for a multi-service launch so one receipt can carry one evidence
    /// record per service. Never a host path or secret (#501). Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_label: Option<String>,
}

/// Whether a provider can enforce a declared policy facet for a projection.
///
/// Recorded as typed provider evidence (#501) so a receipt states honestly
/// whether a required policy is actually enforced, downgraded to best-effort, or
/// unsupported by the selected provider — rather than implying enforcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum OciEnforcementStatus {
    /// The provider fully enforces the declared policy facet.
    Enforced,
    /// The provider enforces it only partially / best-effort.
    Downgraded,
    /// The provider fundamentally cannot enforce it (e.g. podman + egress
    /// allowlist).
    Unsupported,
    /// Enforcement capability is not determined. The conservative default: an
    /// `Unknown` facet is treated as *not* enforced by strict mode.
    #[default]
    Unknown,
}

impl OciEnforcementStatus {
    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }

    /// Whether the declared policy facet is actually enforced. Only
    /// [`Self::Enforced`] counts — `Downgraded`/`Unsupported`/`Unknown` are all
    /// "not enforced" for a fail-closed decision (#500/#501).
    pub fn is_enforced(&self) -> bool {
        matches!(self, Self::Enforced)
    }
}

/// The receipt-assessment delta derived from OCI provider evidence (#501): the
/// declared/resolved provider facts that keep a receipt's graph honestly
/// `Partial` and its reproducibility conservative *before any runtime
/// observation*. Pure data — carries no execution identity and never implies an
/// observed run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OciProviderProjectionAssessment {
    /// Additional, value-free reasons the graph is `Partial` (one per provider
    /// gap). Appended to the receipt's existing reasons (e.g. `RuntimeNotObserved`).
    pub completeness_reasons: Vec<GraphCompletenessReason>,
    /// Reproducibility causes implied by the provider gaps. Merged into the
    /// receipt's causes; the class is then recomputed conservatively.
    pub reproducibility_causes: Vec<ReproducibilityCause>,
}

/// Assess OCI provider evidence into receipt-assessment facts (#501).
///
/// Pure and conservative: it classifies *declared/resolved* provider facts
/// only — an unpinned image, an unenforceable/downgraded/unknown declared
/// network or capability policy, and host-path bind mounts. It NEVER asserts
/// runtime observation, never clears `Partial`, and never derives an execution
/// identity. Each completeness reason is value-free and carries the service
/// label so a multi-service receipt attributes the gap without a host path.
pub fn assess_oci_provider_projection_facts(
    projections: &[OciProviderReceiptEvidence],
) -> OciProviderProjectionAssessment {
    let mut completeness_reasons = Vec::new();
    let mut reproducibility_causes = Vec::new();

    for evidence in projections {
        let reason =
            |gap: ProviderProjectionGap| GraphCompletenessReason::ProviderProjectionIncomplete {
                provider_kind: evidence.provider_kind.clone(),
                service_label: evidence.service_label.clone(),
                gap,
            };

        // An unpinned (tag-only) image has no known immutable materialized
        // identity → the dependency output is unknown.
        if matches!(evidence.image_digest_status, OciImageDigestStatus::Unpinned) {
            completeness_reasons.push(reason(ProviderProjectionGap::ImageUnpinned));
            reproducibility_causes.push(ReproducibilityCause::UnknownDependencyOutput);
        }

        // A declared network policy (e.g. an egress allowlist surfaced as the
        // `network-policy` required capability) that the provider does not fully
        // enforce. Any non-`Enforced` status leaves the launch network-bound.
        let network_required = evidence
            .capabilities_required
            .iter()
            .any(|cap| cap == "network-policy");
        if network_required {
            let gap = match evidence.network_enforcement_status {
                OciEnforcementStatus::Enforced => None,
                OciEnforcementStatus::Downgraded => {
                    Some(ProviderProjectionGap::NetworkEnforcementDowngraded)
                }
                OciEnforcementStatus::Unsupported => {
                    Some(ProviderProjectionGap::NetworkEnforcementUnsupported)
                }
                OciEnforcementStatus::Unknown => {
                    Some(ProviderProjectionGap::NetworkEnforcementUnknown)
                }
            };
            if let Some(gap) = gap {
                completeness_reasons.push(reason(gap));
                reproducibility_causes.push(ReproducibilityCause::NetworkBound);
            }
        }

        // A declared capability/sandbox policy the provider only partially
        // enforces or cannot enforce. (Unknown stays silent: it is the default
        // when no capability policy is modeled.)
        if matches!(
            evidence.capability_enforcement_status,
            OciEnforcementStatus::Downgraded | OciEnforcementStatus::Unsupported
        ) {
            completeness_reasons.push(reason(
                ProviderProjectionGap::CapabilityEnforcementIncomplete,
            ));
        }

        // A host-path bind mount (not an engine-managed volume) is a host-bound
        // fallback → not host-portable.
        if evidence.mounts.iter().any(|mount| !mount.engine_volume) {
            completeness_reasons.push(reason(ProviderProjectionGap::HostBoundMount));
            reproducibility_causes.push(ReproducibilityCause::HostBound);
        }
    }

    reproducibility_causes.sort();
    reproducibility_causes.dedup();
    OciProviderProjectionAssessment {
        completeness_reasons,
        reproducibility_causes,
    }
}

/// Pinned/unpinned status of an OCI image in a receipt. The digest is recorded
/// as image evidence; an unpinned (tag-only) reference is represented honestly
/// rather than fabricated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "state")]
pub enum OciImageDigestStatus {
    /// Fully pinned: `repo@sha256:<64 hex>`.
    Pinned { digest: String },
    /// Tag-only / unresolved at projection time.
    Unpinned,
}

/// Receipt-safe mount projection: target path and flags only (no source host
/// path, which could leak user filesystem layout).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OciMountReceiptEvidence {
    pub target: String,
    pub readonly: bool,
    /// `true` when backed by an engine-managed volume rather than a host bind.
    pub engine_volume: bool,
    /// `true` when this is durable engine-managed state (survives restarts).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub persistent_state: bool,
}

/// Receipt-safe port projection: declared container port + protocol. The
/// host-side port is runtime-allocated and is not recorded as identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OciPortReceiptEvidence {
    pub container_port: u16,
    pub protocol: String,
}

/// Outcome class for an execution receipt (refs #74, #99).
///
/// `Passed` — the launch envelope was fully resolved and the workload
/// reached the readiness gate (or completed normally for non-service
/// capsules). `RecoverableFailure` — the launch failed in the
/// Manifest / Provisioning / Execution phases at a step the user can
/// fix and retry (manifest parse error, runtime not resolved, consent
/// required, etc.). `Aborted` — internal panics, `AtoErrorPhase::Internal`,
/// or explicit user abort that the user cannot meaningfully retry
/// without external intervention.
///
/// Defaults to `Passed` so existing v2 receipts on disk re-deserialize
/// cleanly and the success wire shape stays byte-identical
/// (`skip_serializing_if = "is_passed"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum ReceiptResultClass {
    #[default]
    Passed,
    RecoverableFailure,
    Aborted,
}

impl ReceiptResultClass {
    pub fn is_passed(&self) -> bool {
        matches!(self, Self::Passed)
    }
}

/// Typed failure envelope for partial receipts.
///
/// Mirrors the diagnostic shape of [`crate::execution_plan::error::AtoExecutionError`]
/// (which is itself derived from [`crate::error::AtoError`] at the CLI
/// boundary), but lives on the receipt schema so consumers reading
/// `~/.ato/executions/<id>/receipt.json` don't have to re-parse the
/// human message to know the error code, phase, or whether the failure
/// is retryable. Diagnostic only — never participates in the JCS
/// `execution_id` projection.
///
/// `details` carries the typed `serde_json::Value` from `AtoError::details()`
/// verbatim, so the envelope is `PartialEq` but not `Eq` (Value contains
/// f64 which has no total order). The receipt itself drops `Eq` for the
/// same reason; this is the only field that introduces this constraint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReceiptFailureEnvelope {
    /// `recoverable` or `aborted`. Mirrors the receipt-level
    /// [`ReceiptResultClass`] for self-contained downstream routing
    /// (the receipt and the envelope can be inspected independently).
    pub kind: ReceiptFailureKind,
    /// Stable wire code (`E001`..`E999`). Matches `AtoExecutionError::code`.
    pub code: String,
    /// Stable variant name (e.g. `manifest_toml_parse`).
    pub name: String,
    /// `manifest`, `inference`, `provisioning`, `execution`, `internal`.
    pub phase: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interactive_resolution_required:
        Option<crate::interactive_resolution::InteractiveResolutionEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<crate::execution_plan::error::AtoErrorClassification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_status: Option<crate::execution_plan::error::CleanupStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleanup_actions: Vec<crate::execution_plan::error::CleanupActionRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_suggestion: Option<crate::execution_plan::error::ManifestSuggestion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReceiptFailureKind {
    Recoverable,
    Aborted,
}

// Variants differ in size (V1 ~1.1KB, V2 ~2.4KB) but the enum is the
// canonical receipt envelope and is held by-value across many call
// sites that pattern-match `&doc`. Boxing V2 would force a `&**r` /
// `*Box::new(...)` ceremony at every call site (see ato-cli/src/cli/
// commands/inspect.rs and application/execution_replay.rs) for a few
// stack-bytes saved per receipt — not worth the churn.
// `Eq` dropped because `ExecutionReceiptV2` is no longer `Eq` (see
// note on that struct). `PartialEq` is preserved for tests.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "schema", rename_all = "kebab-case")]
pub enum ExecutionReceiptDocument {
    V1(ExecutionReceipt),
    V2(ExecutionReceiptV2),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionReceiptView {
    pub schema_version: u32,
    pub execution_id: String,
    pub portable: PortableExecutionIdentityView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local: Option<LocalExecutionLocator>,
    pub reproducibility: ReproducibilityIdentity,
}

// V2 is already boxed; V1 is left inline because it's the smaller
// variant. Symmetry isn't worth the call-site churn (see the
// ExecutionReceiptDocument note above for the same trade-off).
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PortableExecutionIdentityView {
    V1(ExecutionIdentityInput),
    V2(Box<ExecutionIdentityInputV2>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceIdentityV2 {
    pub source_tree_hash: Tracked<String>,
    pub manifest_path_role: Tracked<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceProvenance {
    pub kind: SourceProvenanceKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_remote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry_ref: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceProvenanceKind {
    Local,
    Git,
    Registry,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyIdentity {
    pub derivation_hash: Tracked<String>,
    pub output_hash: Tracked<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyIdentityV2 {
    pub derivation_hash: Tracked<String>,
    pub output_hash: Tracked<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derivation_inputs: Option<DependencyDerivationInputsV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyDerivationInputsV2 {
    pub package_manager: String,
    pub package_manager_version: Tracked<String>,
    pub runtime_resolved_ref: Tracked<String>,
    pub platform_abi: PlatformIdentity,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dependency_manifest_digests: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lockfile_digests: BTreeMap<String, String>,
    pub install_command: Vec<String>,
    pub package_manager_config_hash: Tracked<String>,
    pub lifecycle_script_policy_hash: Tracked<String>,
    pub registry_policy_hash: Tracked<String>,
    pub network_policy_hash: Tracked<String>,
    pub environment_allowlist_hash: Tracked<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declared_system_build_inputs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<String>,
    pub binary_hash: Tracked<String>,
    pub dynamic_linkage: Tracked<String>,
    pub platform: PlatformIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeIdentityV2 {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub declared: Option<String>,
    pub resolved_ref: Tracked<String>,
    pub binary_hash: Tracked<String>,
    pub dynamic_linkage: Tracked<String>,
    pub completeness: RuntimeCompleteness,
    pub platform: PlatformIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeCompleteness {
    DeclaredOnly,
    ResolvedBinary,
    BinaryWithDynamicClosure,
    BestEffort,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformIdentity {
    pub os: String,
    pub arch: String,
    pub libc: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentIdentity {
    pub closure_hash: Tracked<String>,
    pub mode: EnvironmentMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracked_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redacted_keys: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentIdentityV2 {
    pub entries: Vec<EnvironmentEntry>,
    pub fd_layout: Tracked<FdLayoutIdentity>,
    pub umask: Tracked<String>,
    pub ulimits: Tracked<UlimitIdentity>,
    pub mode: EnvironmentMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ambient_untracked_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentEntry {
    pub key: String,
    pub value_hash: Tracked<String>,
    pub normalization: ValueNormalizationStatus,
    #[serde(default = "default_env_origin", skip_serializing, skip_deserializing)]
    pub origin: EnvOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ValueNormalizationStatus {
    Normalized,
    NoHostPath,
    SecretReferenceRequired,
    UnnormalizedHostPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FdLayoutIdentity {
    pub stdin: String,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UlimitIdentity {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub limits: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnvironmentMode {
    Closed,
    Partial,
    Untracked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemIdentity {
    pub view_hash: Tracked<String>,
    pub projection_strategy: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writable_dirs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub persistent_state: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub known_readonly_layers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemIdentityV2 {
    pub view_hash: Tracked<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partial_view_hash: Option<String>,
    pub source_root: Tracked<String>,
    pub working_directory: Tracked<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readonly_layers: Vec<ReadonlyLayerIdentity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writable_dirs: Vec<WritableDirIdentity>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub persistent_state: Vec<StateBindingIdentity>,
    pub semantics: FilesystemSemantics,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadonlyLayerIdentity {
    pub role: String,
    pub identity: Tracked<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WritableDirIdentity {
    pub role: String,
    pub lifecycle: WritableDirLifecycle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WritableDirLifecycle {
    SessionLocal,
    PersistentState,
    HostPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateBindingIdentity {
    pub name: String,
    pub kind: StateBindingKind,
    pub identity: Tracked<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharing: Option<StateSharing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StateBindingKind {
    ContentSnapshot,
    AtoStateRef,
    HostPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemSemantics {
    pub case_sensitivity: Tracked<CaseSensitivity>,
    pub symlink_policy: Tracked<SymlinkPolicy>,
    pub tmp_policy: Tracked<TmpPolicy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaseSensitivity {
    Sensitive,
    Insensitive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SymlinkPolicy {
    Preserve,
    Resolve,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TmpPolicy {
    SessionLocal,
    HostTmp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyIdentity {
    pub network_policy_hash: Tracked<String>,
    pub capability_policy_hash: Tracked<String>,
    pub sandbox_policy_hash: Tracked<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyIdentityV2 {
    pub network_policy_hash: Tracked<String>,
    pub capability_policy_hash: Tracked<String>,
    pub sandbox_policy_hash: Tracked<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchIdentity {
    pub entry_point: String,
    pub argv: Vec<String>,
    pub working_directory: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchIdentityV2 {
    pub entry_point: LaunchEntryPoint,
    pub argv: Vec<LaunchArg>,
    pub working_directory: Tracked<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum LaunchEntryPoint {
    RuntimeManaged { resolved_ref: String },
    WorkspaceRelative { path: String },
    Command { name: String },
    Untracked { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchArg {
    pub value_hash: Tracked<String>,
    pub normalization: ValueNormalizationStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalExecutionLocator {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime_resolved_path: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub state_paths: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_point_raw: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub argv_raw: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePathCanonicalizer {
    workspace_root: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "value")]
pub enum CanonicalPath {
    WorkspaceRoot,
    WorkspaceRelative(String),
    OutsideWorkspace(String),
}

impl WorkspacePathCanonicalizer {
    pub fn new(workspace_root: impl AsRef<str>) -> Self {
        Self {
            workspace_root: normalize_host_path(workspace_root.as_ref()),
        }
    }

    pub fn canonicalize(&self, path: impl AsRef<str>) -> CanonicalPath {
        let path = normalize_host_path(path.as_ref());
        if path == self.workspace_root {
            return CanonicalPath::WorkspaceRoot;
        }
        let prefix = format!("{}/", self.workspace_root);
        if let Some(relative) = path.strip_prefix(&prefix) {
            return CanonicalPath::WorkspaceRelative(relative.to_string());
        }
        CanonicalPath::OutsideWorkspace(path)
    }

    pub fn role_string(&self, path: impl AsRef<str>) -> Tracked<String> {
        match self.canonicalize(path) {
            CanonicalPath::WorkspaceRoot => Tracked::known("workspace:.".to_string()),
            CanonicalPath::WorkspaceRelative(relative) => {
                Tracked::known(format!("workspace:{relative}"))
            }
            CanonicalPath::OutsideWorkspace(_) => Tracked::untracked("path is outside workspace"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRoleNormalizer {
    roles: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedValue {
    pub value: String,
    pub status: ValueNormalizationStatus,
}

impl PathRoleNormalizer {
    pub fn new<I, K, V>(roles: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut roles: Vec<(String, String)> = roles
            .into_iter()
            .map(|(token, path)| (token.into(), normalize_host_path(&path.into())))
            .collect();
        roles.sort_by_key(|(_, root)| std::cmp::Reverse(root.len()));
        Self { roles }
    }

    pub fn normalize_value(&self, value: &str) -> NormalizedValue {
        let mut normalized = normalize_host_path(value);
        let had_host_path = contains_absolute_path_like(&normalized);
        for (token, root) in &self.roles {
            normalized = normalized.replace(root, token);
        }
        let status = if !had_host_path {
            ValueNormalizationStatus::NoHostPath
        } else if contains_absolute_path_like(&normalized) {
            ValueNormalizationStatus::UnnormalizedHostPath
        } else {
            ValueNormalizationStatus::Normalized
        };
        NormalizedValue {
            value: normalized,
            status,
        }
    }

    pub fn tracked_hash(&self, value: &str) -> (Tracked<String>, ValueNormalizationStatus) {
        let normalized = self.normalize_value(value);
        match normalized.status {
            ValueNormalizationStatus::UnnormalizedHostPath => (
                Tracked::untracked("value contains unnormalized host path"),
                normalized.status,
            ),
            _ => (
                Tracked::known(hash_normalized_value(&normalized.value)),
                normalized.status,
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReproducibilityIdentity {
    pub class: ReproducibilityClass,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causes: Vec<ReproducibilityCause>,
}

/// Conservative, **pre-observation** reproducibility classification of an
/// execution (refs #494).
///
/// The class is derived from declared/resolved identity facets (manifest,
/// lock, policy, host resolution) — never from runtime observation, which this
/// slice does not perform. It is conservative: any uncertainty downgrades to
/// [`ReproducibilityClass::BestEffort`] rather than overclaiming. The
/// accompanying [`ReproducibilityCause`] list is the typed "why".
///
/// In particular, [`ReproducibilityClass::NetworkBound`] means the policy
/// **permits egress** (see [`ReproducibilityCause::NetworkBound`]); it does
/// **not** assert that the workload actually performed any network access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReproducibilityClass {
    Pure,
    HostBound,
    StateBound,
    TimeBound,
    /// Reproducibility may depend on network egress that policy permits. A
    /// *capability* statement (egress allowed), not an observation of traffic.
    NetworkBound,
    BestEffort,
}

impl ReproducibilityClass {
    /// Derive the conservative reproducibility class from a set of typed causes.
    ///
    /// The canonical precedence used across the codebase: no causes ⇒ `Pure`;
    /// any "unknown/untracked" cause ⇒ `BestEffort`; otherwise the most
    /// constraining bound (`StateBound` → `TimeBound` → `NetworkBound` →
    /// `HostBound`), falling back to `BestEffort`. Adding causes can only keep or
    /// *lower* reproducibility — it never upgrades a class.
    pub fn from_causes(causes: &[ReproducibilityCause]) -> Self {
        if causes.is_empty() {
            return Self::Pure;
        }
        if causes.iter().any(ReproducibilityCause::is_best_effort) {
            return Self::BestEffort;
        }
        if causes.contains(&ReproducibilityCause::StateBound) {
            return Self::StateBound;
        }
        if causes.contains(&ReproducibilityCause::TimeBound) {
            return Self::TimeBound;
        }
        if causes.contains(&ReproducibilityCause::NetworkBound) {
            return Self::NetworkBound;
        }
        if causes.contains(&ReproducibilityCause::HostBound) {
            return Self::HostBound;
        }
        Self::BestEffort
    }
}

/// Typed reason contributing to a [`ReproducibilityClass`] (refs #494).
///
/// Each cause is derived pre-observation from declared/resolved facets. They
/// are diagnostic and never imply runtime observation occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReproducibilityCause {
    HostBound,
    StateBound,
    TimeBound,
    /// Policy permits network egress (e.g. a non-empty allow-hosts list), so
    /// the result may depend on external services. This is an **egress-allowed
    /// capability** statement derived from policy — it does NOT mean the
    /// workload was observed making any network request.
    NetworkBound,
    UnknownDependencyOutput,
    UnknownRuntimeIdentity,
    UntrackedEnvironment,
    UntrackedFilesystemView,
    UntrackedDynamicDependency,
    LifecycleUnknown,
}

impl ReproducibilityCause {
    /// Whether this cause reflects *unknown/untracked* evidence (as opposed to a
    /// known-but-impure binding). Any such cause forces
    /// [`ReproducibilityClass::BestEffort`].
    pub fn is_best_effort(&self) -> bool {
        matches!(
            self,
            Self::UnknownDependencyOutput
                | Self::UnknownRuntimeIdentity
                | Self::UntrackedEnvironment
                | Self::UntrackedFilesystemView
                | Self::UntrackedDynamicDependency
                | Self::LifecycleUnknown
        )
    }
}

#[derive(Serialize)]
struct IdentityProjection<'a> {
    schema_version: u32,
    canonicalization: &'a str,
    hash_algorithm: &'a str,
    source: SourceProjection<'a>,
    dependencies: DependencyProjection<'a>,
    runtime: RuntimeProjection<'a>,
    environment: EnvironmentProjection<'a>,
    filesystem: FilesystemProjection<'a>,
    policy: PolicyProjection<'a>,
    launch: &'a LaunchIdentity,
}

#[derive(Serialize)]
struct SourceProjection<'a> {
    source_ref: TrackedProjection<'a, String>,
    source_tree_hash: TrackedProjection<'a, String>,
}

#[derive(Serialize)]
struct DependencyProjection<'a> {
    derivation_hash: TrackedProjection<'a, String>,
    output_hash: TrackedProjection<'a, String>,
}

#[derive(Serialize)]
struct RuntimeProjection<'a> {
    declared: &'a Option<String>,
    resolved: &'a Option<String>,
    binary_hash: TrackedProjection<'a, String>,
    dynamic_linkage: TrackedProjection<'a, String>,
    platform: &'a PlatformIdentity,
}

#[derive(Serialize)]
struct EnvironmentProjection<'a> {
    closure_hash: TrackedProjection<'a, String>,
    mode: EnvironmentMode,
    tracked_keys: &'a [String],
    redacted_keys: &'a [String],
    unknown_keys: &'a [String],
}

#[derive(Serialize)]
struct FilesystemProjection<'a> {
    view_hash: TrackedProjection<'a, String>,
    projection_strategy: &'a str,
    writable_dirs: &'a [String],
    persistent_state: &'a [String],
    known_readonly_layers: &'a [String],
}

#[derive(Serialize)]
struct PolicyProjection<'a> {
    network_policy_hash: TrackedProjection<'a, String>,
    capability_policy_hash: TrackedProjection<'a, String>,
    sandbox_policy_hash: TrackedProjection<'a, String>,
}

#[derive(Serialize)]
struct IdentityProjectionV2<'a> {
    schema_version: u32,
    canonicalization: &'a str,
    hash_algorithm: &'a str,
    source: SourceProjectionV2<'a>,
    dependencies: DependencyProjectionV2<'a>,
    runtime: RuntimeProjectionV2<'a>,
    environment: EnvironmentProjectionV2<'a>,
    filesystem: FilesystemProjectionV2<'a>,
    policy: PolicyProjectionV2<'a>,
    launch: LaunchProjectionV2<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    oci: Option<&'a OciLaunchEnvelope>,
}

#[derive(Serialize)]
struct SourceProjectionV2<'a> {
    source_tree_hash: TrackedProjection<'a, String>,
    manifest_path_role: TrackedProjection<'a, String>,
}

#[derive(Serialize)]
struct DependencyProjectionV2<'a> {
    derivation_hash: TrackedProjection<'a, String>,
    output_hash: TrackedProjection<'a, String>,
    derivation_inputs: &'a Option<DependencyDerivationInputsV2>,
}

#[derive(Serialize)]
struct RuntimeProjectionV2<'a> {
    declared: &'a Option<String>,
    resolved_ref: TrackedProjection<'a, String>,
    binary_hash: TrackedProjection<'a, String>,
    dynamic_linkage: TrackedProjection<'a, String>,
    completeness: RuntimeCompleteness,
    platform: &'a PlatformIdentity,
}

#[derive(Serialize)]
struct EnvironmentProjectionV2<'a> {
    entries: &'a [EnvironmentEntry],
    fd_layout: TrackedProjection<'a, FdLayoutIdentity>,
    umask: TrackedProjection<'a, String>,
    ulimits: TrackedProjection<'a, UlimitIdentity>,
    mode: EnvironmentMode,
}

#[derive(Serialize)]
struct FilesystemProjectionV2<'a> {
    view_hash: TrackedProjection<'a, String>,
    source_root: TrackedProjection<'a, String>,
    working_directory: TrackedProjection<'a, String>,
    readonly_layers: &'a [ReadonlyLayerIdentity],
    writable_dirs: &'a [WritableDirIdentity],
    persistent_state: &'a [StateBindingIdentity],
    semantics: FilesystemSemanticsProjection<'a>,
}

#[derive(Serialize)]
struct FilesystemSemanticsProjection<'a> {
    case_sensitivity: TrackedProjection<'a, CaseSensitivity>,
    symlink_policy: TrackedProjection<'a, SymlinkPolicy>,
    tmp_policy: TrackedProjection<'a, TmpPolicy>,
}

#[derive(Serialize)]
struct PolicyProjectionV2<'a> {
    network_policy_hash: TrackedProjection<'a, String>,
    capability_policy_hash: TrackedProjection<'a, String>,
    sandbox_policy_hash: TrackedProjection<'a, String>,
}

#[derive(Serialize)]
struct LaunchProjectionV2<'a> {
    entry_point: LaunchEntryPointProjection<'a>,
    argv: &'a [LaunchArg],
    working_directory: TrackedProjection<'a, String>,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
enum LaunchEntryPointProjection<'a> {
    RuntimeManaged { resolved_ref: &'a str },
    WorkspaceRelative { path: &'a str },
    Command { name: &'a str },
    Untracked { gap: &'static str },
}

#[derive(Serialize)]
struct TrackedProjection<'a, T> {
    status: TrackingStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<&'a T>,
}

impl<'a, T> From<&'a Tracked<T>> for TrackedProjection<'a, T> {
    fn from(value: &'a Tracked<T>) -> Self {
        Self {
            status: value.status,
            value: value.value.as_ref(),
        }
    }
}

pub fn compute_execution_id(input: &ExecutionIdentityInput) -> Result<ExecutionIdentityDigest> {
    validate_identity_header(input)?;
    let projection = identity_projection(input);
    let canonical = serde_jcs::to_vec(&projection).map_err(|err| {
        CapsuleError::Config(format!(
            "Failed to canonicalize execution identity input: {err}"
        ))
    })?;
    let digest = format!("blake3:{}", blake3::hash(&canonical).to_hex());
    Ok(ExecutionIdentityDigest {
        execution_id: digest.clone(),
        input_hash: digest,
    })
}

pub fn compute_execution_id_v2(
    input: &ExecutionIdentityInputV2,
) -> Result<ExecutionIdentityDigest> {
    validate_identity_header_v2(input)?;
    let projection = identity_projection_v2(input);
    let canonical = serde_jcs::to_vec(&projection).map_err(|err| {
        CapsuleError::Config(format!(
            "Failed to canonicalize execution identity v2 input: {err}"
        ))
    })?;
    let digest = format!("blake3:{}", blake3::hash(&canonical).to_hex());
    Ok(ExecutionIdentityDigest {
        execution_id: digest.clone(),
        input_hash: digest,
    })
}

fn validate_identity_header(input: &ExecutionIdentityInput) -> Result<()> {
    if input.schema_version != EXECUTION_IDENTITY_SCHEMA_VERSION {
        return Err(CapsuleError::Config(format!(
            "unsupported execution identity schema_version {}; expected {}",
            input.schema_version, EXECUTION_IDENTITY_SCHEMA_VERSION
        )));
    }
    if input.canonicalization != EXECUTION_IDENTITY_CANONICALIZATION {
        return Err(CapsuleError::Config(format!(
            "unsupported execution identity canonicalization {}; expected {}",
            input.canonicalization, EXECUTION_IDENTITY_CANONICALIZATION
        )));
    }
    if input.hash_algorithm != EXECUTION_IDENTITY_HASH_ALGORITHM {
        return Err(CapsuleError::Config(format!(
            "unsupported execution identity hash_algorithm {}; expected {}",
            input.hash_algorithm, EXECUTION_IDENTITY_HASH_ALGORITHM
        )));
    }
    Ok(())
}

fn validate_identity_header_v2(input: &ExecutionIdentityInputV2) -> Result<()> {
    if input.schema_version != EXECUTION_IDENTITY_SCHEMA_VERSION_V2_EXPERIMENTAL {
        return Err(CapsuleError::Config(format!(
            "unsupported execution identity v2 schema_version {}; expected {}",
            input.schema_version, EXECUTION_IDENTITY_SCHEMA_VERSION_V2_EXPERIMENTAL
        )));
    }
    if input.canonicalization != EXECUTION_IDENTITY_CANONICALIZATION {
        return Err(CapsuleError::Config(format!(
            "unsupported execution identity v2 canonicalization {}; expected {}",
            input.canonicalization, EXECUTION_IDENTITY_CANONICALIZATION
        )));
    }
    if input.hash_algorithm != EXECUTION_IDENTITY_HASH_ALGORITHM {
        return Err(CapsuleError::Config(format!(
            "unsupported execution identity v2 hash_algorithm {}; expected {}",
            input.hash_algorithm, EXECUTION_IDENTITY_HASH_ALGORITHM
        )));
    }
    Ok(())
}

fn identity_projection(input: &ExecutionIdentityInput) -> IdentityProjection<'_> {
    IdentityProjection {
        schema_version: input.schema_version,
        canonicalization: input.canonicalization.as_str(),
        hash_algorithm: input.hash_algorithm.as_str(),
        source: SourceProjection {
            source_ref: (&input.source.source_ref).into(),
            source_tree_hash: (&input.source.source_tree_hash).into(),
        },
        dependencies: DependencyProjection {
            derivation_hash: (&input.dependencies.derivation_hash).into(),
            output_hash: (&input.dependencies.output_hash).into(),
        },
        runtime: RuntimeProjection {
            declared: &input.runtime.declared,
            resolved: &input.runtime.resolved,
            binary_hash: (&input.runtime.binary_hash).into(),
            dynamic_linkage: (&input.runtime.dynamic_linkage).into(),
            platform: &input.runtime.platform,
        },
        environment: EnvironmentProjection {
            closure_hash: (&input.environment.closure_hash).into(),
            mode: input.environment.mode,
            tracked_keys: &input.environment.tracked_keys,
            redacted_keys: &input.environment.redacted_keys,
            unknown_keys: &input.environment.unknown_keys,
        },
        filesystem: FilesystemProjection {
            view_hash: (&input.filesystem.view_hash).into(),
            projection_strategy: input.filesystem.projection_strategy.as_str(),
            writable_dirs: &input.filesystem.writable_dirs,
            persistent_state: &input.filesystem.persistent_state,
            known_readonly_layers: &input.filesystem.known_readonly_layers,
        },
        policy: PolicyProjection {
            network_policy_hash: (&input.policy.network_policy_hash).into(),
            capability_policy_hash: (&input.policy.capability_policy_hash).into(),
            sandbox_policy_hash: (&input.policy.sandbox_policy_hash).into(),
        },
        launch: &input.launch,
    }
}

fn identity_projection_v2(input: &ExecutionIdentityInputV2) -> IdentityProjectionV2<'_> {
    IdentityProjectionV2 {
        schema_version: input.schema_version,
        canonicalization: input.canonicalization.as_str(),
        hash_algorithm: input.hash_algorithm.as_str(),
        source: SourceProjectionV2 {
            source_tree_hash: (&input.source.source_tree_hash).into(),
            manifest_path_role: (&input.source.manifest_path_role).into(),
        },
        dependencies: DependencyProjectionV2 {
            derivation_hash: (&input.dependencies.derivation_hash).into(),
            output_hash: (&input.dependencies.output_hash).into(),
            derivation_inputs: &input.dependencies.derivation_inputs,
        },
        runtime: RuntimeProjectionV2 {
            declared: &input.runtime.declared,
            resolved_ref: (&input.runtime.resolved_ref).into(),
            binary_hash: (&input.runtime.binary_hash).into(),
            dynamic_linkage: (&input.runtime.dynamic_linkage).into(),
            completeness: input.runtime.completeness,
            platform: &input.runtime.platform,
        },
        environment: EnvironmentProjectionV2 {
            entries: &input.environment.entries,
            fd_layout: (&input.environment.fd_layout).into(),
            umask: (&input.environment.umask).into(),
            ulimits: (&input.environment.ulimits).into(),
            mode: input.environment.mode,
        },
        filesystem: FilesystemProjectionV2 {
            view_hash: (&input.filesystem.view_hash).into(),
            source_root: (&input.filesystem.source_root).into(),
            working_directory: (&input.filesystem.working_directory).into(),
            readonly_layers: &input.filesystem.readonly_layers,
            writable_dirs: &input.filesystem.writable_dirs,
            persistent_state: &input.filesystem.persistent_state,
            semantics: FilesystemSemanticsProjection {
                case_sensitivity: (&input.filesystem.semantics.case_sensitivity).into(),
                symlink_policy: (&input.filesystem.semantics.symlink_policy).into(),
                tmp_policy: (&input.filesystem.semantics.tmp_policy).into(),
            },
        },
        policy: PolicyProjectionV2 {
            network_policy_hash: (&input.policy.network_policy_hash).into(),
            capability_policy_hash: (&input.policy.capability_policy_hash).into(),
            sandbox_policy_hash: (&input.policy.sandbox_policy_hash).into(),
        },
        launch: LaunchProjectionV2 {
            entry_point: (&input.launch.entry_point).into(),
            argv: &input.launch.argv,
            working_directory: (&input.launch.working_directory).into(),
        },
        oci: input.oci.as_ref(),
    }
}

impl<'a> From<&'a LaunchEntryPoint> for LaunchEntryPointProjection<'a> {
    fn from(value: &'a LaunchEntryPoint) -> Self {
        match value {
            LaunchEntryPoint::RuntimeManaged { resolved_ref } => {
                Self::RuntimeManaged { resolved_ref }
            }
            LaunchEntryPoint::WorkspaceRelative { path } => Self::WorkspaceRelative { path },
            LaunchEntryPoint::Command { name } => Self::Command { name },
            LaunchEntryPoint::Untracked { .. } => Self::Untracked {
                gap: "untracked-entry-point",
            },
        }
    }
}

fn normalize_host_path(value: &str) -> String {
    let mut normalized = value.replace('\\', "/");
    if let Some(stripped) = normalized.strip_prefix("//?/") {
        normalized = stripped.to_string();
    }
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    normalized
}

fn contains_absolute_path_like(value: &str) -> bool {
    let bytes = value.as_bytes();
    if value.starts_with('/') || value.starts_with("//") {
        return true;
    }
    bytes
        .windows(3)
        .any(|window| window[0].is_ascii_alphabetic() && window[1] == b':' && window[2] == b'/')
        || value.contains(":/")
        || value.contains(";//")
        || value.contains("://")
}

fn hash_normalized_value(value: &str) -> String {
    format!("blake3:{}", blake3::hash(value.as_bytes()).to_hex())
}

#[cfg(test)]
pub(in crate::engine::execution_identity) mod tests {
    use super::*;
    use crate::types::{
        OciImageResolution, OciLaunchEnvelope, OciPlatform, OciPolicyEnforcementLevel,
        OciPolicyEnforcementMode, OciPolicyEnvelope, OciPortExposureShape, OciPortPublishPolicy,
        OciProviderKind, OciProviderMode, OciProviderSemantics, OciProviderSubstrate,
        OciSecretDeliveryShape, OciSecretReferenceShape, OciServiceLaunchShape, OciStateMountShape,
    };

    fn sample_input() -> ExecutionIdentityInput {
        ExecutionIdentityInput::new(
            SourceIdentity {
                source_ref: Tracked::known("github.com/acme/app@abc123".to_string()),
                source_tree_hash: Tracked::known("blake3:source".to_string()),
            },
            DependencyIdentity {
                derivation_hash: Tracked::unknown("dependency derivation observer not enabled"),
                output_hash: Tracked::unknown("dependency output not observed"),
            },
            RuntimeIdentity {
                declared: Some("node@20".to_string()),
                resolved: Some("node@20.10.0".to_string()),
                binary_hash: Tracked::unknown("runtime binary hash not observed"),
                dynamic_linkage: Tracked::untracked("not implemented"),
                platform: PlatformIdentity {
                    os: "macos".to_string(),
                    arch: "aarch64".to_string(),
                    libc: "unknown".to_string(),
                },
            },
            EnvironmentIdentity {
                closure_hash: Tracked::known("blake3:env".to_string()),
                mode: EnvironmentMode::Closed,
                tracked_keys: vec!["LANG".to_string(), "PATH".to_string()],
                redacted_keys: vec!["OPENAI_API_KEY".to_string()],
                unknown_keys: Vec::new(),
            },
            FilesystemIdentity {
                view_hash: Tracked::known("blake3:fs".to_string()),
                projection_strategy: "direct".to_string(),
                writable_dirs: Vec::new(),
                persistent_state: Vec::new(),
                known_readonly_layers: Vec::new(),
            },
            PolicyIdentity {
                network_policy_hash: Tracked::known("blake3:network".to_string()),
                capability_policy_hash: Tracked::known("blake3:capability".to_string()),
                sandbox_policy_hash: Tracked::known("blake3:sandbox".to_string()),
            },
            LaunchIdentity {
                entry_point: "npm".to_string(),
                argv: vec!["run".to_string(), "dev".to_string()],
                working_directory: "/app".to_string(),
            },
            ReproducibilityIdentity {
                class: ReproducibilityClass::BestEffort,
                causes: vec![
                    ReproducibilityCause::UnknownDependencyOutput,
                    ReproducibilityCause::UnknownRuntimeIdentity,
                ],
            },
        )
    }

    fn sample_oci_envelope() -> OciLaunchEnvelope {
        OciLaunchEnvelope::new(
            OciProviderSemantics {
                kind: OciProviderKind::Podman,
                mode: OciProviderMode::Rootless,
                substrate: OciProviderSubstrate::PodmanMachine,
                policy_profile: "oci-podman-v1".to_string(),
            },
            vec![OciServiceLaunchShape {
                name: "main".to_string(),
                target_label: "app".to_string(),
                image: OciImageResolution {
                    declared_ref: "ghcr.io/acme/app:latest".to_string(),
                    resolved_digest: "sha256:111".to_string(),
                    platform: OciPlatform {
                        os: "linux".to_string(),
                        architecture: "arm64".to_string(),
                        variant: None,
                    },
                    importer_input_hash: Some("blake3:compose-input".to_string()),
                },
                entrypoint: None,
                command: vec!["serve".to_string()],
                working_dir: Some("/app".to_string()),
                env_keys: vec!["DATABASE_URL".to_string()],
                secret_refs: vec![OciSecretReferenceShape {
                    id: "db-password".to_string(),
                    delivery: OciSecretDeliveryShape::Env {
                        key: "POSTGRES_PASSWORD".to_string(),
                    },
                }],
                state_mounts: vec![OciStateMountShape {
                    state: "app_data".to_string(),
                    target: "/app/data".to_string(),
                    readonly: false,
                    durability: Some("persistent".to_string()),
                    snapshot_hash: None,
                }],
                ports: vec![OciPortExposureShape {
                    container_port: 3000,
                    protocol: "tcp".to_string(),
                    publish: OciPortPublishPolicy::LocalhostDynamic,
                }],
                network_aliases: vec!["main".to_string()],
                readiness_probe: Some("http-get:/health".to_string()),
                run_once: false,
            }],
            OciPolicyEnvelope {
                enforcement_mode: OciPolicyEnforcementMode::Strict,
                enforcement_level: OciPolicyEnforcementLevel::Enforced,
                network_policy_hash: Some("blake3:network".to_string()),
                filesystem_policy_hash: Some("blake3:fs-policy".to_string()),
                capability_policy_hash: None,
                unsupported_policy: Vec::new(),
            },
        )
    }

    pub(in crate::engine::execution_identity) fn sample_input_v2() -> ExecutionIdentityInputV2 {
        let normalizer = PathRoleNormalizer::new([
            ("${WORKSPACE}", "/Users/alice/proj"),
            ("${ATO_HOME}", "/Users/alice/.ato"),
            ("${ATO_RUNTIME}", "/Users/alice/.ato/runtimes"),
        ]);
        let (path_hash, path_status) = normalizer.tracked_hash("/Users/alice/proj/config/app.toml");

        ExecutionIdentityInputV2::new(
            SourceIdentityV2 {
                source_tree_hash: Tracked::known("blake3:source".to_string()),
                manifest_path_role: Tracked::known("workspace:capsule.toml".to_string()),
            },
            SourceProvenance {
                kind: SourceProvenanceKind::Local,
                git_remote: None,
                git_commit: None,
                registry_ref: None,
            },
            DependencyIdentityV2 {
                derivation_hash: Tracked::not_applicable(),
                output_hash: Tracked::not_applicable(),
                derivation_inputs: None,
            },
            RuntimeIdentityV2 {
                declared: Some("node@20".to_string()),
                resolved_ref: Tracked::known("node@20.10.0".to_string()),
                binary_hash: Tracked::known("blake3:runtime".to_string()),
                dynamic_linkage: Tracked::known("blake3:dyn".to_string()),
                completeness: RuntimeCompleteness::BinaryWithDynamicClosure,
                platform: PlatformIdentity {
                    os: "macos".to_string(),
                    arch: "aarch64".to_string(),
                    libc: "unknown".to_string(),
                },
            },
            EnvironmentIdentityV2 {
                entries: vec![EnvironmentEntry {
                    key: "CONFIG".to_string(),
                    value_hash: path_hash,
                    normalization: path_status,
                    origin: EnvOrigin::ManifestStatic,
                }],
                fd_layout: Tracked::known(FdLayoutIdentity {
                    stdin: "inherited".to_string(),
                    stdout: "inherited".to_string(),
                    stderr: "inherited".to_string(),
                }),
                umask: Tracked::known("022".to_string()),
                ulimits: Tracked::known(UlimitIdentity {
                    limits: BTreeMap::new(),
                }),
                mode: EnvironmentMode::Closed,
                ambient_untracked_keys: vec!["SHELL".to_string()],
            },
            FilesystemIdentityV2 {
                view_hash: Tracked::known("blake3:fs".to_string()),
                partial_view_hash: Some("blake3:diagnostic".to_string()),
                source_root: Tracked::known("workspace:.".to_string()),
                working_directory: Tracked::known("workspace:.".to_string()),
                readonly_layers: vec![ReadonlyLayerIdentity {
                    role: "source".to_string(),
                    identity: Tracked::known("blake3:source".to_string()),
                }],
                writable_dirs: vec![WritableDirIdentity {
                    role: "tmp".to_string(),
                    lifecycle: WritableDirLifecycle::SessionLocal,
                }],
                persistent_state: Vec::new(),
                semantics: FilesystemSemantics {
                    case_sensitivity: Tracked::known(CaseSensitivity::Sensitive),
                    symlink_policy: Tracked::known(SymlinkPolicy::Preserve),
                    tmp_policy: Tracked::known(TmpPolicy::SessionLocal),
                },
            },
            PolicyIdentityV2 {
                network_policy_hash: Tracked::known("blake3:network".to_string()),
                capability_policy_hash: Tracked::known("blake3:capability".to_string()),
                sandbox_policy_hash: Tracked::known("blake3:sandbox".to_string()),
            },
            LaunchIdentityV2 {
                entry_point: LaunchEntryPoint::Command {
                    name: "node".to_string(),
                },
                argv: vec![LaunchArg {
                    value_hash: Tracked::known(hash_normalized_value("server.js")),
                    normalization: ValueNormalizationStatus::NoHostPath,
                }],
                working_directory: Tracked::known("workspace:.".to_string()),
            },
            Some(LocalExecutionLocator {
                manifest_path: Some("/Users/alice/proj/capsule.toml".to_string()),
                workspace_root: Some("/Users/alice/proj".to_string()),
                working_directory_path: Some("/Users/alice/proj".to_string()),
                runtime_resolved_path: Some("/Users/alice/.ato/runtimes/node/bin/node".to_string()),
                state_paths: BTreeMap::new(),
                entry_point_raw: Some("/Users/alice/.ato/runtimes/node/bin/node".to_string()),
                argv_raw: vec!["server.js".to_string()],
            }),
            ReproducibilityIdentity {
                class: ReproducibilityClass::Pure,
                causes: Vec::new(),
            },
        )
    }

    #[test]
    fn execution_id_is_stable_for_identical_inputs() {
        let left = sample_input().compute_id().expect("left id").execution_id;
        let right = sample_input().compute_id().expect("right id").execution_id;
        assert_eq!(left, right);
        assert!(left.starts_with("blake3:"));
    }

    #[test]
    fn execution_id_changes_when_launch_argv_changes() {
        let before = sample_input().compute_id().expect("before id").execution_id;
        let mut input = sample_input();
        input.launch.argv.push("--port=3000".to_string());
        let after = input.compute_id().expect("after id").execution_id;
        assert_ne!(before, after);
    }

    #[test]
    fn execution_identity_drift_matrix_covers_launch_envelope_components() {
        type Perturbation = Box<dyn Fn(&mut ExecutionIdentityInput)>;
        let baseline = sample_input().compute_id().expect("baseline").execution_id;
        let mut perturbations: Vec<(&str, Perturbation)> = vec![
            (
                "source",
                Box::new(|input| {
                    input.source.source_tree_hash = Tracked::known("blake3:source2".to_string());
                }),
            ),
            (
                "dependencies",
                Box::new(|input| {
                    input.dependencies.output_hash = Tracked::known("blake3:deps2".to_string());
                }),
            ),
            (
                "runtime",
                Box::new(|input| {
                    input.runtime.binary_hash = Tracked::known("blake3:runtime2".to_string());
                }),
            ),
            (
                "environment",
                Box::new(|input| {
                    input.environment.closure_hash = Tracked::known("blake3:env2".to_string());
                }),
            ),
            (
                "filesystem",
                Box::new(|input| {
                    input.filesystem.view_hash = Tracked::known("blake3:fs2".to_string());
                }),
            ),
            (
                "policy",
                Box::new(|input| {
                    input.policy.network_policy_hash =
                        Tracked::known("blake3:network2".to_string());
                }),
            ),
            (
                "launch",
                Box::new(|input| {
                    input.launch.working_directory = "/different".to_string();
                }),
            ),
        ];

        for (component, perturb) in perturbations.drain(..) {
            let mut input = sample_input();
            perturb(&mut input);
            let changed = input.compute_id().expect(component).execution_id;
            assert_ne!(
                baseline, changed,
                "{component} drift must change execution_id"
            );
        }
    }

    #[test]
    fn execution_id_changes_when_tracking_status_changes() {
        let before = sample_input().compute_id().expect("before id").execution_id;
        let mut input = sample_input();
        input.dependencies.output_hash = Tracked::untracked("not in scope");
        let after = input.compute_id().expect("after id").execution_id;
        assert_ne!(before, after);
    }

    #[test]
    fn execution_id_ignores_tracking_reason_text() {
        let before = sample_input().compute_id().expect("before id").execution_id;
        let mut input = sample_input();
        input.dependencies.output_hash =
            Tracked::unknown("different wording for the same missing observation");
        let after = input.compute_id().expect("after id").execution_id;
        assert_eq!(before, after);
    }

    #[test]
    fn execution_id_ignores_reproducibility_classification_metadata() {
        let before = sample_input().compute_id().expect("before id").execution_id;
        let mut input = sample_input();
        input.reproducibility = ReproducibilityIdentity {
            class: ReproducibilityClass::Pure,
            causes: Vec::new(),
        };
        let after = input.compute_id().expect("after id").execution_id;
        assert_eq!(before, after);
    }

    #[test]
    fn v2_local_locator_does_not_affect_execution_id() {
        let before = sample_input_v2().compute_id().expect("before").execution_id;
        let mut input = sample_input_v2();
        input.local = Some(LocalExecutionLocator {
            manifest_path: Some("/home/bob/proj/capsule.toml".to_string()),
            workspace_root: Some("/home/bob/proj".to_string()),
            working_directory_path: Some("/home/bob/proj".to_string()),
            runtime_resolved_path: Some("/opt/ato/runtimes/node/bin/node".to_string()),
            state_paths: BTreeMap::from([(
                "data".to_string(),
                "/home/bob/.ato/state/data".to_string(),
            )]),
            entry_point_raw: Some("/opt/ato/runtimes/node/bin/node".to_string()),
            argv_raw: vec!["server.js".to_string()],
        });
        let after = input.compute_id().expect("after").execution_id;
        assert_eq!(before, after);
    }

    #[test]
    fn v2_source_provenance_does_not_affect_execution_id() {
        let before = sample_input_v2().compute_id().expect("before").execution_id;
        let mut input = sample_input_v2();
        input.source_provenance = SourceProvenance {
            kind: SourceProvenanceKind::Git,
            git_remote: Some("https://example.com/acme/app.git".to_string()),
            git_commit: Some("deadbeef".to_string()),
            registry_ref: None,
        };
        let after = input.compute_id().expect("after").execution_id;
        assert_eq!(before, after);
    }

    #[test]
    fn v2_filesystem_partial_hash_is_diagnostic_only() {
        let before = sample_input_v2().compute_id().expect("before").execution_id;
        let mut input = sample_input_v2();
        input.filesystem.partial_view_hash = Some("blake3:other-diagnostic".to_string());
        let after = input.compute_id().expect("after").execution_id;
        assert_eq!(before, after);
    }

    #[test]
    fn v2_launch_untracked_reason_text_is_not_hashed() {
        let mut before_input = sample_input_v2();
        before_input.launch.entry_point = LaunchEntryPoint::Untracked {
            reason: "absolute path outside workspace".to_string(),
        };
        let before = before_input.compute_id().expect("before").execution_id;
        let mut after_input = sample_input_v2();
        after_input.launch.entry_point = LaunchEntryPoint::Untracked {
            reason: "different operator wording".to_string(),
        };
        let after = after_input.compute_id().expect("after").execution_id;
        assert_eq!(before, after);
    }

    #[test]
    fn v2_graph_derived_execution_ids_do_not_affect_jcs_execution_id() {
        // Graph-derived ids are surfaced on the receipt JSON but MUST NOT
        // participate in the JCS projection — otherwise existing v2 receipts
        // would re-hash to different values once #99 lands. Pin both the
        // declared and the resolved id slot here.
        let before = sample_input_v2().compute_id().expect("before").execution_id;

        let mut input = sample_input_v2();
        input.declared_execution_id = Some("sha256:declared-test".to_string());
        input.resolved_execution_id = Some("sha256:resolved-test".to_string());
        // observed stays None per v0.6.0 contract; setting it here only
        // proves the future-compat field is also excluded from JCS.
        input.observed_execution_id = Some("sha256:observed-test".to_string());

        let after = input.compute_id().expect("after").execution_id;
        assert_eq!(
            before, after,
            "declared/resolved/observed_execution_id must be parallel diagnostic ids, not JCS inputs"
        );
    }

    #[test]
    fn v2_non_oci_projection_omits_oci_field() {
        let input = sample_input_v2();
        let projection = identity_projection_v2(&input);
        let canonical =
            String::from_utf8(serde_jcs::to_vec(&projection).expect("canonical")).unwrap();
        assert!(
            !canonical.contains("\"oci\""),
            "non-OCI identity projection must remain byte-compatible by omitting the OCI envelope"
        );
    }

    #[test]
    fn v2_oci_digest_drift_changes_execution_id() {
        let before = sample_input_v2()
            .with_oci_launch_envelope(Some(sample_oci_envelope()))
            .compute_id()
            .expect("before")
            .execution_id;
        let mut envelope = sample_oci_envelope();
        envelope.services[0].image.resolved_digest = "sha256:222".to_string();
        let after = sample_input_v2()
            .with_oci_launch_envelope(Some(envelope))
            .compute_id()
            .expect("after")
            .execution_id;
        assert_ne!(before, after);
    }

    #[test]
    fn v2_oci_provider_semantics_change_execution_id() {
        let before = sample_input_v2()
            .with_oci_launch_envelope(Some(sample_oci_envelope()))
            .compute_id()
            .expect("before")
            .execution_id;
        let mut envelope = sample_oci_envelope();
        envelope.provider.substrate = OciProviderSubstrate::NativeLinux;
        let after = sample_input_v2()
            .with_oci_launch_envelope(Some(envelope))
            .compute_id()
            .expect("after")
            .execution_id;
        assert_ne!(before, after);
    }

    #[test]
    fn v2_oci_envelope_excludes_live_runtime_state() {
        let envelope = sample_oci_envelope();
        let serialized = serde_json::to_string(&envelope).expect("serialize envelope");
        assert!(!serialized.contains("container_id"));
        assert!(!serialized.contains("host_port"));
        assert!(!serialized.contains("network_id"));
        assert!(!serialized.contains("volume_id"));
    }

    /// Flipping a service from long-running to `run_once` changes the start-
    /// order contract (dependents now wait for exit-0 instead of readiness)
    /// and the success/failure semantics — so the execution identity MUST
    /// change.
    #[test]
    fn run_once_lifecycle_changes_execution_identity() {
        let before = sample_input_v2()
            .with_oci_launch_envelope(Some(sample_oci_envelope()))
            .compute_id()
            .expect("before")
            .execution_id;
        let mut envelope = sample_oci_envelope();
        envelope.services[0].run_once = true;
        let after = sample_input_v2()
            .with_oci_launch_envelope(Some(envelope))
            .compute_id()
            .expect("after")
            .execution_id;
        assert_ne!(
            before, after,
            "run_once flip on a service must change execution_id"
        );
    }

    /// The `run_once` envelope shape carries the lifecycle bit, but never the
    /// exit timestamp — that is a live-runtime fact, not an identity input.
    /// Pinned as a structural check: serialized envelope must not contain
    /// any of the run-time fields below.
    #[test]
    fn run_once_exit_timestamp_not_in_identity() {
        let mut envelope = sample_oci_envelope();
        envelope.services[0].run_once = true;
        let serialized = serde_json::to_string(&envelope).expect("serialize envelope");
        for forbidden in [
            "exit_timestamp",
            "completed_at",
            "exit_code",
            "elapsed_ms",
            "elapsed_time",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "run_once envelope must not contain '{forbidden}'; got: {serialized}"
            );
        }
    }

    /// Container ids are runtime-allocated and must not feed identity.  The
    /// existing `v2_oci_envelope_excludes_live_runtime_state` test covers
    /// container_id for the long-running shape; this pins the same invariant
    /// when run_once is set so the property does not silently regress as the
    /// envelope evolves.
    #[test]
    fn run_once_container_id_not_in_identity() {
        let mut envelope = sample_oci_envelope();
        envelope.services[0].run_once = true;
        let serialized = serde_json::to_string(&envelope).expect("serialize envelope");
        assert!(
            !serialized.contains("container_id"),
            "run_once envelope must not contain container_id; got: {serialized}"
        );
    }

    #[test]
    fn v2_policy_identity_changes_execution_id() {
        let before = sample_input_v2().compute_id().expect("before").execution_id;
        let mut input = sample_input_v2();
        input.policy.sandbox_policy_hash = Tracked::known("blake3:sandbox2".to_string());
        let after = input.compute_id().expect("after").execution_id;
        assert_ne!(before, after);
    }

    #[test]
    fn workspace_path_canonicalizer_removes_unix_and_windows_roots() {
        let unix = WorkspacePathCanonicalizer::new("/Users/alice/proj");
        assert_eq!(
            unix.role_string("/Users/alice/proj/backend")
                .value
                .as_deref(),
            Some("workspace:backend")
        );

        let windows = WorkspacePathCanonicalizer::new(r"C:\Users\alice\proj");
        assert_eq!(
            windows
                .role_string(r"C:\Users\alice\proj\backend")
                .value
                .as_deref(),
            Some("workspace:backend")
        );
    }

    #[test]
    fn path_role_normalizer_hashes_role_tokens_not_host_roots() {
        let alice = PathRoleNormalizer::new([("${WORKSPACE}", "/Users/alice/proj")]);
        let bob = PathRoleNormalizer::new([("${WORKSPACE}", "/home/bob/proj")]);
        let (alice_hash, alice_status) = alice.tracked_hash("/Users/alice/proj/config.toml");
        let (bob_hash, bob_status) = bob.tracked_hash("/home/bob/proj/config.toml");

        assert_eq!(alice_status, ValueNormalizationStatus::Normalized);
        assert_eq!(bob_status, ValueNormalizationStatus::Normalized);
        assert_eq!(alice_hash, bob_hash);
    }

    #[test]
    fn path_role_normalizer_detects_unnormalized_host_paths() {
        let normalizer = PathRoleNormalizer::new([("${WORKSPACE}", "/Users/alice/proj")]);
        let (value_hash, status) = normalizer.tracked_hash("/private/other/config.toml");
        assert_eq!(status, ValueNormalizationStatus::UnnormalizedHostPath);
        assert_eq!(value_hash.status, TrackingStatus::Untracked);
    }

    #[test]
    fn receipt_preserves_reason_metadata() {
        let receipt = ExecutionReceipt::from_input(sample_input(), "2026-05-03T00:00:00Z".into())
            .expect("receipt");
        assert_eq!(receipt.schema_version, EXECUTION_IDENTITY_SCHEMA_VERSION);
        assert_eq!(receipt.execution_id, receipt.identity.input_hash);
        assert_eq!(
            receipt.dependencies.output_hash.reason.as_deref(),
            Some("dependency output not observed")
        );
    }

    /// Setting `result` and `failure_envelope` (the v2 fields added in
    /// #99) MUST NOT change the JCS-derived `execution_id`. They live
    /// only on the receipt-level type, not on `ExecutionIdentityInputV2`,
    /// so by construction they cannot feed the canonical projection. This
    /// test pins that property at the receipt-construction boundary.
    #[test]
    fn v2_result_and_failure_envelope_do_not_affect_jcs_execution_id() {
        let input = sample_input_v2();
        let baseline = input.compute_id().expect("baseline").execution_id;
        let baseline_receipt =
            ExecutionReceiptV2::from_input(input, "2026-05-03T00:00:00Z".to_string())
                .expect("baseline receipt");

        let labeled = baseline_receipt.clone().with_result(
            ReceiptResultClass::RecoverableFailure,
            Some(ReceiptFailureEnvelope {
                kind: ReceiptFailureKind::Recoverable,
                code: "E001".to_string(),
                name: "manifest_toml_parse".to_string(),
                phase: "manifest".to_string(),
                message: "fixture failure".to_string(),
                hint: None,
                resource: None,
                target: None,
                retryable: false,
                interactive_resolution_required: None,
                classification: None,
                cleanup_status: None,
                cleanup_actions: Vec::new(),
                manifest_suggestion: None,
                details: None,
            }),
        );

        assert_eq!(
            labeled.execution_id, baseline,
            "result/failure_envelope must be diagnostic-only — JCS execution_id stable"
        );
        assert_eq!(
            labeled.identity.input_hash, baseline_receipt.identity.input_hash,
            "result/failure_envelope must not feed the input_hash projection"
        );
    }

    /// Successful v2 receipts continue to serialize byte-identically
    /// before and after the #99 schema additions: `result: Passed`
    /// is `skip_serializing_if = "is_passed"` and `failure_envelope`
    /// is omitted when `None`. Regression-pin against the bytes a v2
    /// receipt produced before #99 — the JSON for a `from_input` receipt
    /// must contain neither `"result"` nor `"failure_envelope"`.
    #[test]
    fn v2_happy_path_receipt_bytes_omit_partial_failure_fields() {
        let receipt =
            ExecutionReceiptV2::from_input(sample_input_v2(), "2026-05-03T00:00:00Z".to_string())
                .expect("receipt");
        let json = serde_json::to_string(&receipt).expect("encode");
        assert!(
            !json.contains("\"result\""),
            "happy-path receipt JSON must not include `result` field; got: {json}"
        );
        assert!(
            !json.contains("\"failure_envelope\""),
            "happy-path receipt JSON must not include `failure_envelope` field; got: {json}"
        );
    }

    /// Reading a v2 receipt that pre-dates #99 (no `result` /
    /// `failure_envelope` keys) must round-trip cleanly with the new
    /// fields defaulting to `Passed` / `None`.
    #[test]
    fn v2_pre_99_receipt_deserializes_with_defaults() {
        let receipt =
            ExecutionReceiptV2::from_input(sample_input_v2(), "2026-05-03T00:00:00Z".to_string())
                .expect("receipt");
        let json = serde_json::to_string(&receipt).expect("encode");
        let decoded: ExecutionReceiptV2 = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded.result, ReceiptResultClass::Passed);
        assert!(decoded.failure_envelope.is_none());
    }

    // ── #493: non-empty NodeReceipt / EdgeReceipt from the launch graph ──────

    fn base_receipt() -> ExecutionReceiptV2 {
        ExecutionReceiptV2::from_input(sample_input_v2(), "2026-05-03T00:00:00Z".to_string())
            .expect("receipt")
    }

    // ── #501: OCI provider facts → receipt assessment ──

    #[allow(clippy::too_many_arguments)]
    fn oci_evidence(
        service: Option<&str>,
        image_digest_status: OciImageDigestStatus,
        network: OciEnforcementStatus,
        capability: OciEnforcementStatus,
        capabilities_required: &[&str],
        host_bind_mount: bool,
    ) -> OciProviderReceiptEvidence {
        OciProviderReceiptEvidence {
            provider_kind: "oci".to_string(),
            provider_name: "podman".to_string(),
            image_reference: "repo/app".to_string(),
            image_digest_status,
            platform: None,
            env_keys: Vec::new(),
            mounts: if host_bind_mount {
                vec![OciMountReceiptEvidence {
                    target: "/data".to_string(),
                    readonly: false,
                    engine_volume: false,
                    persistent_state: false,
                }]
            } else {
                Vec::new()
            },
            ports: Vec::new(),
            network_aliases: Vec::new(),
            capabilities_required: capabilities_required
                .iter()
                .map(|c| c.to_string())
                .collect(),
            provider_version: Some("oci-podman-v1".to_string()),
            network_enforcement_status: network,
            capability_enforcement_status: capability,
            derived_command_redacted: Vec::new(),
            service_label: service.map(|s| s.to_string()),
        }
    }

    fn has_gap(reasons: &[GraphCompletenessReason], wanted: ProviderProjectionGap) -> bool {
        reasons.iter().any(|r| {
            matches!(
                r,
                GraphCompletenessReason::ProviderProjectionIncomplete { gap, .. } if *gap == wanted
            )
        })
    }

    #[test]
    fn assess_unpinned_image_adds_materialization_reason() {
        let a = assess_oci_provider_projection_facts(&[oci_evidence(
            None,
            OciImageDigestStatus::Unpinned,
            OciEnforcementStatus::Enforced,
            OciEnforcementStatus::Enforced,
            &[],
            false,
        )]);
        assert!(has_gap(
            &a.completeness_reasons,
            ProviderProjectionGap::ImageUnpinned
        ));
        assert!(
            a.reproducibility_causes
                .contains(&ReproducibilityCause::UnknownDependencyOutput)
        );
    }

    #[test]
    fn assess_unsupported_network_enforcement_adds_policy_reason() {
        let a = assess_oci_provider_projection_facts(&[oci_evidence(
            None,
            OciImageDigestStatus::Pinned {
                digest: format!("sha256:{}", "a".repeat(64)),
            },
            OciEnforcementStatus::Unsupported,
            OciEnforcementStatus::Enforced,
            &["network-policy"],
            false,
        )]);
        assert!(has_gap(
            &a.completeness_reasons,
            ProviderProjectionGap::NetworkEnforcementUnsupported
        ));
        assert!(
            a.reproducibility_causes
                .contains(&ReproducibilityCause::NetworkBound)
        );
    }

    #[test]
    fn assess_unknown_network_enforcement_stays_conservative() {
        let a = assess_oci_provider_projection_facts(&[oci_evidence(
            None,
            OciImageDigestStatus::Pinned {
                digest: format!("sha256:{}", "a".repeat(64)),
            },
            OciEnforcementStatus::Unknown,
            OciEnforcementStatus::Enforced,
            &["network-policy"],
            false,
        )]);
        assert!(has_gap(
            &a.completeness_reasons,
            ProviderProjectionGap::NetworkEnforcementUnknown
        ));
        assert!(
            a.reproducibility_causes
                .contains(&ReproducibilityCause::NetworkBound)
        );
    }

    #[test]
    fn assess_pinned_enforced_no_mounts_is_clean() {
        let a = assess_oci_provider_projection_facts(&[oci_evidence(
            None,
            OciImageDigestStatus::Pinned {
                digest: format!("sha256:{}", "a".repeat(64)),
            },
            OciEnforcementStatus::Enforced,
            OciEnforcementStatus::Enforced,
            &["network-policy"],
            false,
        )]);
        assert!(a.completeness_reasons.is_empty());
        assert!(a.reproducibility_causes.is_empty());
    }

    #[test]
    fn assess_host_bound_mount_adds_host_bound() {
        let a = assess_oci_provider_projection_facts(&[oci_evidence(
            None,
            OciImageDigestStatus::Pinned {
                digest: format!("sha256:{}", "a".repeat(64)),
            },
            OciEnforcementStatus::Enforced,
            OciEnforcementStatus::Enforced,
            &[],
            true,
        )]);
        assert!(has_gap(
            &a.completeness_reasons,
            ProviderProjectionGap::HostBoundMount
        ));
        assert!(
            a.reproducibility_causes
                .contains(&ReproducibilityCause::HostBound)
        );
    }

    #[test]
    fn assess_multi_service_reasons_carry_value_free_service_label() {
        let a = assess_oci_provider_projection_facts(&[
            oci_evidence(
                Some("web"),
                OciImageDigestStatus::Unpinned,
                OciEnforcementStatus::Enforced,
                OciEnforcementStatus::Enforced,
                &[],
                false,
            ),
            oci_evidence(
                Some("db"),
                OciImageDigestStatus::Unpinned,
                OciEnforcementStatus::Enforced,
                OciEnforcementStatus::Enforced,
                &[],
                true,
            ),
        ]);
        let labels: Vec<&str> = a
            .completeness_reasons
            .iter()
            .filter_map(|r| match r {
                GraphCompletenessReason::ProviderProjectionIncomplete { service_label, .. } => {
                    service_label.as_deref()
                }
                _ => None,
            })
            .collect();
        assert!(labels.contains(&"web") && labels.contains(&"db"));
        // The reasons are value-free: no host path (e.g. the mount target) leaks.
        let json = serde_json::to_string(&a.completeness_reasons).expect("serialize");
        assert!(!json.contains("/data"), "no host path in reasons: {json}");
    }

    #[test]
    fn with_oci_provider_assessment_keeps_partial_and_conservative() {
        let receipt = base_receipt()
            .with_graph_completeness(GraphCompleteness::Partial)
            .with_graph_completeness_reasons(vec![GraphCompletenessReason::RuntimeNotObserved])
            .with_observation_scope(ObservationScope::declared_resolved())
            .with_provider_projections(vec![oci_evidence(
                Some("web"),
                OciImageDigestStatus::Unpinned,
                OciEnforcementStatus::Unsupported,
                OciEnforcementStatus::Enforced,
                &["network-policy"],
                true,
            )])
            .with_oci_provider_assessment();

        // Graph stays Partial; the runtime-not-observed reason is preserved and
        // the provider gaps are appended.
        assert_eq!(receipt.graph_completeness, Some(GraphCompleteness::Partial));
        assert_ne!(
            receipt.graph_completeness,
            Some(GraphCompleteness::Complete)
        );
        assert!(
            receipt
                .graph_completeness_reasons
                .iter()
                .any(|r| matches!(r, GraphCompletenessReason::RuntimeNotObserved))
        );
        assert!(has_gap(
            &receipt.graph_completeness_reasons,
            ProviderProjectionGap::ImageUnpinned
        ));
        assert!(has_gap(
            &receipt.graph_completeness_reasons,
            ProviderProjectionGap::NetworkEnforcementUnsupported
        ));
        assert!(has_gap(
            &receipt.graph_completeness_reasons,
            ProviderProjectionGap::HostBoundMount
        ));

        // Reproducibility is cause-bearing and conservative (unpinned image ⇒
        // an unknown dependency output ⇒ BestEffort).
        for cause in [
            ReproducibilityCause::UnknownDependencyOutput,
            ReproducibilityCause::NetworkBound,
            ReproducibilityCause::HostBound,
        ] {
            assert!(receipt.reproducibility.causes.contains(&cause));
        }
        assert_eq!(
            receipt.reproducibility.class,
            ReproducibilityClass::BestEffort
        );

        // Pre-observation invariants: no observed id; scope stays declared/resolved.
        assert!(receipt.observed_execution_id.is_none());
        assert_eq!(
            receipt.observation_scope,
            Some(ObservationScope::declared_resolved())
        );
    }

    #[test]
    fn with_oci_provider_assessment_is_noop_without_provider_evidence() {
        // Source-native receipts (no provider_projections) are unaffected.
        let before = base_receipt()
            .with_graph_completeness_reasons(vec![GraphCompletenessReason::RuntimeNotObserved]);
        let reasons_before = before.graph_completeness_reasons.clone();
        let class_before = before.reproducibility.class;
        let causes_before = before.reproducibility.causes.clone();

        let after = before.with_oci_provider_assessment();
        assert_eq!(after.graph_completeness_reasons, reasons_before);
        assert_eq!(after.reproducibility.class, class_before);
        assert_eq!(after.reproducibility.causes, causes_before);
    }

    #[test]
    fn provider_projections_are_never_used_as_resolved_execution_id() {
        let pinned_digest = format!("sha256:{}", "d".repeat(64));
        let input = sample_input_v2()
            .with_resolved_execution_id(Some("graph:resolved-exec-id".to_string()));
        let receipt = ExecutionReceiptV2::from_input(input, "2026-05-03T00:00:00Z".to_string())
            .expect("receipt")
            .with_provider_projections(vec![oci_evidence(
                Some("web"),
                OciImageDigestStatus::Pinned {
                    digest: pinned_digest.clone(),
                },
                OciEnforcementStatus::Enforced,
                OciEnforcementStatus::Enforced,
                &[],
                false,
            )])
            .with_oci_provider_assessment();

        // The resolved execution id is the graph-derived id, never the image
        // digest or any provider projection fact.
        assert_eq!(
            receipt.resolved_execution_id.as_deref(),
            Some("graph:resolved-exec-id")
        );
        assert_ne!(
            receipt.resolved_execution_id.as_deref(),
            Some(pinned_digest.as_str())
        );
        assert!(receipt.observed_execution_id.is_none());
    }

    #[test]
    fn graph_backed_receipt_has_non_empty_node_and_edge_receipts() {
        use crate::engine::execution_graph::{
            ExecutionGraph, ExecutionGraphEdge, ExecutionGraphEdgeKind, ExecutionGraphNode,
        };
        let graph = ExecutionGraph {
            nodes: vec![
                ExecutionGraphNode::Source {
                    identifier: "src:app".to_string(),
                },
                ExecutionGraphNode::DependencyOutput {
                    identifier: "dep:db".to_string(),
                },
                ExecutionGraphNode::Provider {
                    identifier: "provider:podman".to_string(),
                },
            ],
            edges: vec![ExecutionGraphEdge {
                source: "src:app".to_string(),
                target: "dep:db".to_string(),
                kind: ExecutionGraphEdgeKind::DependsOn,
            }],
            labels: Default::default(),
            constraints: Vec::new(),
        };

        let receipt = base_receipt().with_graph_projection(&graph);
        assert!(
            !receipt.node_receipts.is_empty(),
            "graph-backed launch must emit node receipts"
        );
        assert!(
            !receipt.edge_receipts.is_empty(),
            "graph-backed launch must emit edge receipts"
        );
        assert_eq!(receipt.node_receipts.len(), 3);
        assert_eq!(receipt.edge_receipts.len(), 1);
        // Completeness must NOT be auto-promoted to Complete by projection.
        assert_ne!(
            receipt.graph_completeness,
            Some(GraphCompleteness::Complete)
        );
    }

    #[test]
    fn graph_backed_receipt_json_has_non_empty_node_and_edge_receipts() {
        use crate::engine::execution_graph::{
            ExecutionGraph, ExecutionGraphEdge, ExecutionGraphEdgeKind, ExecutionGraphNode,
        };
        // A representative launch graph projected into a receipt the same way
        // the production builder does (`with_graph_projection`).
        let graph = ExecutionGraph {
            nodes: vec![
                ExecutionGraphNode::Source {
                    identifier: "src:app".to_string(),
                },
                ExecutionGraphNode::DependencyOutput {
                    identifier: "dep:db".to_string(),
                },
            ],
            edges: vec![ExecutionGraphEdge {
                source: "src:app".to_string(),
                target: "dep:db".to_string(),
                kind: ExecutionGraphEdgeKind::DependsOn,
            }],
            labels: Default::default(),
            constraints: Vec::new(),
        };
        let receipt = base_receipt().with_graph_projection(&graph);

        // #493 acceptance: the *serialized* receipt JSON carries non-empty
        // node_receipts / edge_receipts arrays — not just the in-memory struct.
        let json = serde_json::to_value(&receipt).expect("receipt json");
        assert!(
            json["node_receipts"]
                .as_array()
                .is_some_and(|a| !a.is_empty()),
            "serialized receipt must carry a non-empty node_receipts array; got: {json}"
        );
        assert!(
            json["edge_receipts"]
                .as_array()
                .is_some_and(|a| !a.is_empty()),
            "serialized receipt must carry a non-empty edge_receipts array; got: {json}"
        );
        // The graph-derived identity + kind contract is visible in the JSON.
        assert_eq!(json["node_receipts"][0]["node_identifier"], "src:app");
        assert_eq!(json["edge_receipts"][0]["kind"], "depends-on");
        // No observed status is fabricated in the wire bytes.
        assert!(json["node_receipts"][0].get("status").is_none());
    }

    #[test]
    fn node_receipts_are_derived_from_launch_graph_not_runtime_command() {
        use crate::engine::execution_graph::{ExecutionGraph, ExecutionGraphNode};
        let graph = ExecutionGraph {
            nodes: vec![
                ExecutionGraphNode::Service {
                    identifier: "service:web".to_string(),
                },
                ExecutionGraphNode::State {
                    identifier: "state:pgdata".to_string(),
                },
            ],
            edges: Vec::new(),
            labels: Default::default(),
            constraints: Vec::new(),
        };

        let receipt = base_receipt().with_graph_projection(&graph);
        // Node identity == graph node identifier + kind — never a runtime
        // command, container id, or session-local string.
        let ids: Vec<&str> = receipt
            .node_receipts
            .iter()
            .map(|n| n.node_identifier.as_str())
            .collect();
        assert_eq!(ids, vec!["service:web", "state:pgdata"]);
        let kinds: Vec<&str> = receipt
            .node_receipts
            .iter()
            .map(|n| n.kind.as_str())
            .collect();
        assert_eq!(kinds, vec!["service", "state"]);
        // No observed status is claimed — receipts derive from declared/resolved
        // graph only.
        assert!(receipt.node_receipts.iter().all(|n| n.status.is_none()));

        // The projection is a pure function of the graph: identical graph ⇒
        // identical receipts, independent of any other (runtime) receipt state.
        let again = base_receipt().with_graph_projection(&graph);
        assert_eq!(receipt.node_receipts, again.node_receipts);
    }

    #[test]
    fn edge_receipts_include_declared_dependency_edges() {
        use crate::engine::execution_graph::{
            ExecutionGraph, ExecutionGraphEdge, ExecutionGraphEdgeKind, ExecutionGraphNode,
        };
        let graph = ExecutionGraph {
            nodes: vec![
                ExecutionGraphNode::Source {
                    identifier: "src:app".to_string(),
                },
                ExecutionGraphNode::DependencyOutput {
                    identifier: "dep:db".to_string(),
                },
                ExecutionGraphNode::State {
                    identifier: "state:pgdata".to_string(),
                },
            ],
            edges: vec![
                ExecutionGraphEdge {
                    source: "src:app".to_string(),
                    target: "dep:db".to_string(),
                    kind: ExecutionGraphEdgeKind::DependsOn,
                },
                ExecutionGraphEdge {
                    source: "dep:db".to_string(),
                    target: "state:pgdata".to_string(),
                    kind: ExecutionGraphEdgeKind::Mounts,
                },
            ],
            labels: Default::default(),
            constraints: Vec::new(),
        };

        let receipt = base_receipt().with_graph_projection(&graph);
        assert!(
            receipt
                .edge_receipts
                .iter()
                .any(|e| e.kind == "depends-on" && e.source == "src:app" && e.target == "dep:db"),
            "declared dependency edge must produce a matching EdgeReceipt"
        );
        assert!(receipt.edge_receipts.iter().any(|e| e.kind == "mounts"));
        assert!(receipt.edge_receipts.iter().all(|e| e.status.is_none()));
    }

    #[test]
    fn legacy_or_incomplete_graph_receipt_stays_partial_without_panic() {
        use crate::engine::execution_graph::ExecutionGraph;
        // A receipt built without a graph projection (legacy path / graph
        // genuinely unavailable) keeps empty node/edge receipts and never panics.
        let base = base_receipt();
        assert!(base.node_receipts.is_empty());
        assert!(base.edge_receipts.is_empty());
        assert!(base.provider_projections.is_empty());

        // Projecting an empty graph is a no-op, not a panic.
        let projected = base.with_graph_projection(&ExecutionGraph::default());
        assert!(projected.node_receipts.is_empty());
        assert!(projected.edge_receipts.is_empty());

        // A pre-#493 receipt JSON (no node/edge/provider keys) round-trips with
        // serde defaults, and completeness is never silently Complete.
        let json = serde_json::to_string(&projected).expect("encode");
        let decoded: ExecutionReceiptV2 = serde_json::from_str(&json).expect("decode");
        assert!(decoded.node_receipts.is_empty());
        assert!(decoded.provider_projections.is_empty());
        assert_ne!(
            decoded.graph_completeness,
            Some(GraphCompleteness::Complete)
        );
    }

    #[test]
    fn partial_failure_receipt_carries_envelope_and_synthetic_id() {
        let envelope = ReceiptFailureEnvelope {
            kind: ReceiptFailureKind::Recoverable,
            code: "E001".to_string(),
            name: "manifest_toml_parse".to_string(),
            phase: "manifest".to_string(),
            message: "expected `=`".to_string(),
            hint: None,
            resource: None,
            target: None,
            retryable: false,
            interactive_resolution_required: None,
            classification: None,
            cleanup_status: None,
            cleanup_actions: Vec::new(),
            manifest_suggestion: None,
            details: None,
        };
        let receipt = ExecutionReceiptV2::partial_failure(
            "2026-05-03T00:00:00Z".to_string(),
            ReceiptResultClass::RecoverableFailure,
            envelope.clone(),
            None,
            None,
            None,
        );

        assert!(
            receipt.execution_id.starts_with("partial:blake3:"),
            "partial receipt execution_id must use the synthetic `partial:` prefix, got {}",
            receipt.execution_id
        );
        assert_eq!(receipt.result, ReceiptResultClass::RecoverableFailure);
        assert_eq!(receipt.failure_envelope.as_ref(), Some(&envelope));
        assert!(receipt.declared_execution_id.is_none());
        assert!(receipt.resolved_execution_id.is_none());
        assert!(receipt.observed_execution_id.is_none());
    }

    /// Two retries with the same envelope and the same graph state must
    /// produce the same synthetic `execution_id`. Pinning this lets
    /// downstream consumers (e.g. `ato diff execution`, GC roots)
    /// treat the synthetic id as content-addressed — the brief's
    /// invariant for the partial-receipt id space.
    #[test]
    fn partial_failure_id_is_stable_across_recomputation_with_same_envelope() {
        let envelope = ReceiptFailureEnvelope {
            kind: ReceiptFailureKind::Recoverable,
            code: "E001".to_string(),
            name: "manifest_toml_parse".to_string(),
            phase: "manifest".to_string(),
            message: "expected `=`".to_string(),
            hint: None,
            resource: None,
            target: None,
            retryable: false,
            interactive_resolution_required: None,
            classification: None,
            cleanup_status: None,
            cleanup_actions: Vec::new(),
            manifest_suggestion: None,
            details: None,
        };

        let first = ExecutionReceiptV2::partial_failure(
            "2026-05-03T00:00:00Z".to_string(),
            ReceiptResultClass::RecoverableFailure,
            envelope.clone(),
            Some("blake3:declared-fixture".to_string()),
            Some("blake3:resolved-fixture".to_string()),
            None,
        );
        let second = ExecutionReceiptV2::partial_failure(
            // Different `computed_at` — must NOT change the id.
            "2099-12-31T23:59:59Z".to_string(),
            ReceiptResultClass::RecoverableFailure,
            envelope,
            Some("blake3:declared-fixture".to_string()),
            Some("blake3:resolved-fixture".to_string()),
            None,
        );

        assert_eq!(
            first.execution_id, second.execution_id,
            "partial:blake3 execution_id must be content-addressed (envelope + graph ids only) — `computed_at` must not feed it"
        );
        assert_ne!(
            first.computed_at, second.computed_at,
            "fixture sanity: the two receipts should have different computed_at timestamps"
        );
    }

    /// PR-3a back-compat pin: a v2 receipt JSON written before PR-3a
    /// (no `runner` / `host_fingerprint` / `graph_completeness` /
    /// `graph_receipt` / `node_receipts` / `edge_receipts` fields) must
    /// round-trip through the post-PR-3a `ExecutionReceiptV2` shape
    /// without a serde error. The new fields default to `None` / empty
    /// when absent on disk.
    #[test]
    fn pre_pr3a_v2_receipt_round_trips_after_schema_extension() {
        let pre_pr3a_json = serde_json::json!({
            "schema_version": EXECUTION_IDENTITY_SCHEMA_VERSION_V2_EXPERIMENTAL,
            "execution_id": "blake3:fixture-execution",
            "computed_at": "2026-05-03T00:00:00Z",
            "identity": {
                "canonicalization": EXECUTION_IDENTITY_CANONICALIZATION,
                "hash_algorithm": EXECUTION_IDENTITY_HASH_ALGORITHM,
                "input_hash": "blake3:fixture-input"
            },
            "source": {
                "source_tree_hash": { "status": "known", "value": "blake3:src" },
                "manifest_path_role": { "status": "known", "value": "manifest:role" }
            },
            "source_provenance": { "kind": "unknown" },
            "dependencies": {
                "derivation_hash": { "status": "known", "value": "blake3:deps" },
                "output_hash": { "status": "known", "value": "blake3:out" }
            },
            "runtime": {
                "resolved_ref": { "status": "known", "value": "node@20.10.0" },
                "binary_hash": { "status": "untracked", "reason": "fixture" },
                "dynamic_linkage": { "status": "untracked", "reason": "fixture" },
                "completeness": "declared-only",
                "platform": { "os": "macos", "arch": "aarch64", "libc": "unknown" }
            },
            "environment": {
                "entries": [],
                "fd_layout": { "status": "untracked", "reason": "fixture" },
                "umask": { "status": "untracked", "reason": "fixture" },
                "ulimits": { "status": "untracked", "reason": "fixture" },
                "mode": "untracked",
                "ambient_untracked_keys": []
            },
            "filesystem": {
                "view_hash": { "status": "known", "value": "blake3:fs" },
                "source_root": { "status": "known", "value": "src-role" },
                "working_directory": { "status": "known", "value": "wd-role" },
                "readonly_layers": [],
                "writable_dirs": [],
                "persistent_state": [],
                "semantics": {
                    "case_sensitivity": { "status": "untracked", "reason": "fixture" },
                    "symlink_policy": { "status": "untracked", "reason": "fixture" },
                    "tmp_policy": { "status": "untracked", "reason": "fixture" }
                }
            },
            "policy": {
                "network_policy_hash": { "status": "known", "value": "blake3:net" },
                "capability_policy_hash": { "status": "known", "value": "blake3:cap" },
                "sandbox_policy_hash": { "status": "known", "value": "blake3:sbx" }
            },
            "launch": {
                "entry_point": { "kind": "command", "name": "fixture" },
                "argv": [],
                "working_directory": { "status": "known", "value": "wd-role" }
            },
            "reproducibility": { "class": "best-effort", "causes": [] }
        });

        let receipt: ExecutionReceiptV2 = serde_json::from_value(pre_pr3a_json)
            .expect("pre-PR-3a v2 receipt must re-deserialize after schema extension");

        // PR-3a additive fields default to None / empty when absent.
        assert!(receipt.runner.is_none());
        assert!(receipt.host_fingerprint.is_none());
        assert!(receipt.graph_completeness.is_none());
        assert!(receipt.graph_receipt.is_none());
        assert!(receipt.node_receipts.is_empty());
        assert!(receipt.edge_receipts.is_empty());
    }

    /// PR-3a builder methods produce the expected fields on the receipt.
    #[test]
    fn pr3a_builder_methods_set_expected_fields() {
        let receipt = ExecutionReceiptV2::partial_failure(
            "2026-05-03T00:00:00Z".to_string(),
            ReceiptResultClass::RecoverableFailure,
            ReceiptFailureEnvelope {
                kind: ReceiptFailureKind::Recoverable,
                code: "E001".to_string(),
                name: "fixture".to_string(),
                phase: "manifest".to_string(),
                message: "fixture".to_string(),
                hint: None,
                resource: None,
                target: None,
                retryable: false,
                interactive_resolution_required: None,
                classification: None,
                cleanup_status: None,
                cleanup_actions: Vec::new(),
                manifest_suggestion: None,
                details: None,
            },
            Some("blake3:declared".to_string()),
            Some("blake3:resolved".to_string()),
            None,
        )
        .with_runner(ExecutionRunnerIdentity::new(
            "ato-cli",
            Some("0.6.0".to_string()),
        ))
        .with_host_fingerprint("macos:aarch64:unknown-libc")
        .with_graph_completeness(GraphCompleteness::Partial)
        .with_graph_receipt(GraphReceipt::launch_passed(
            Some("blake3:declared".to_string()),
            Some("blake3:resolved".to_string()),
            None,
        ));

        let runner = receipt.runner.as_ref().expect("runner set");
        assert_eq!(runner.name, "ato-cli");
        assert_eq!(runner.version.as_deref(), Some("0.6.0"));
        assert_eq!(
            receipt.host_fingerprint.as_deref(),
            Some("macos:aarch64:unknown-libc")
        );
        assert_eq!(receipt.graph_completeness, Some(GraphCompleteness::Partial));
        let graph_receipt = receipt.graph_receipt.as_ref().expect("graph_receipt set");
        assert_eq!(graph_receipt.gate, "launch-passed");
        assert_eq!(
            graph_receipt.declared_execution_id.as_deref(),
            Some("blake3:declared")
        );
        assert_eq!(
            graph_receipt.resolved_execution_id.as_deref(),
            Some("blake3:resolved")
        );
    }

    // -----------------------------------------------------------------
    // #495 ObservationScope
    // -----------------------------------------------------------------

    /// A graph-backed receipt fixture: declared + resolved evidence, no
    /// runtime observation. Mirrors what the ato-cli graph-backed builder
    /// attaches (`with_observation_scope(ObservationScope::declared_resolved)`).
    fn observation_scope_fixture_receipt() -> ExecutionReceiptV2 {
        ExecutionReceiptV2::partial_failure(
            "2026-05-03T00:00:00Z".to_string(),
            ReceiptResultClass::RecoverableFailure,
            ReceiptFailureEnvelope {
                kind: ReceiptFailureKind::Recoverable,
                code: "E001".to_string(),
                name: "fixture".to_string(),
                phase: "manifest".to_string(),
                message: "fixture".to_string(),
                hint: None,
                resource: None,
                target: None,
                retryable: false,
                interactive_resolution_required: None,
                classification: None,
                cleanup_status: None,
                cleanup_actions: Vec::new(),
                manifest_suggestion: None,
                details: None,
            },
            Some("blake3:declared".to_string()),
            Some("blake3:resolved".to_string()),
            None,
        )
        .with_graph_completeness(GraphCompleteness::Partial)
        .with_observation_scope(ObservationScope::declared_resolved())
    }

    #[test]
    fn observation_scope_declared_resolved_marks_declared_resolved_evidence_only() {
        let scope = ObservationScope::declared_resolved();
        assert_eq!(scope.declared, LayerEvidence::Present);
        assert_eq!(scope.resolved, LayerEvidence::Present);
        // The runtime layer is explicitly not observed — not implied by an
        // absent field.
        assert_eq!(scope.observed, RuntimeObservation::NotObserved);
        assert!(!scope.has_runtime_observation());
    }

    #[test]
    fn observation_scope_never_claims_runtime_observation() {
        // Whatever reason the observed layer carries, it never asserts that
        // the runtime was observed. The `RuntimeObservation` enum has no
        // `Observed` variant, so this holds by construction for every value.
        for observed in [
            RuntimeObservation::NotObserved,
            RuntimeObservation::OutOfScope,
            RuntimeObservation::Deferred,
        ] {
            let scope = ObservationScope {
                declared: LayerEvidence::Present,
                resolved: LayerEvidence::Present,
                observed,
            };
            assert!(!scope.has_runtime_observation());
        }
    }

    #[test]
    fn receipt_with_observation_scope_synthesizes_no_observed_id_and_no_complete() {
        let receipt = observation_scope_fixture_receipt();

        // The receipt carries the declared/resolved-only scope.
        assert_eq!(
            receipt.observation_scope,
            Some(ObservationScope::declared_resolved())
        );

        // #495 invariants: no observed_execution_id is synthesized, and
        // completeness is never upgraded to Complete.
        assert!(
            receipt.observed_execution_id.is_none(),
            "observed_execution_id must not be synthesized in this slice"
        );
        assert_ne!(
            receipt.graph_completeness,
            Some(GraphCompleteness::Complete),
            "Complete must never be emitted in this slice"
        );
        assert_eq!(receipt.graph_completeness, Some(GraphCompleteness::Partial));
    }

    /// #490: applying a real observation stamps `observed_execution_id`, flips
    /// the scope's runtime layer to `Observed`, swaps the completeness reason
    /// from `RuntimeNotObserved` to `RuntimeObservationMinimal`, and — crucially
    /// — does NOT upgrade `GraphCompleteness` to `Complete`.
    #[test]
    fn with_observation_sets_observed_id_keeps_partial_no_complete() {
        let receipt = observation_scope_fixture_receipt();
        assert!(receipt.observed_execution_id.is_none());
        assert!(receipt.observed_runtime.is_none());
        let before_completeness = receipt.graph_completeness;

        let envelope = ObservedLaunchEnvelope {
            resolved_execution_id: receipt.resolved_execution_id.clone(),
            runtime_kind: "source/node".to_string(),
            runtime_identity: Some("deno 2.6.8".to_string()),
            entrypoint: vec!["node".to_string(), "server.js".to_string()],
            working_directory: Some(".".to_string()),
            env_keys: vec!["PORT".to_string()],
            mount_targets: vec![],
            provider_projection_digest: None,
        };
        let expected_id = envelope.compute_observed_execution_id();
        let observed = receipt
            .clone()
            .with_observation(ObservedRuntimeEvidence::new(envelope).with_bound_port(Some(40000)));

        assert_eq!(
            observed.observed_execution_id.as_deref(),
            Some(expected_id.as_str())
        );
        assert_ne!(
            observed.observed_execution_id, observed.resolved_execution_id,
            "observed id must not be a copy of resolved id"
        );
        let scope = observed.observation_scope.expect("scope");
        assert_eq!(scope.observed, RuntimeObservation::Observed);
        assert!(scope.has_runtime_observation());
        assert!(observed.observed_runtime.is_some());
        // Completeness label is untouched; never Complete.
        assert_eq!(observed.graph_completeness, before_completeness);
        assert_ne!(
            observed.graph_completeness,
            Some(GraphCompleteness::Complete)
        );
        assert!(
            observed
                .graph_completeness_reasons
                .contains(&GraphCompletenessReason::RuntimeObservationMinimal),
            "observed receipt must carry the minimal-observation reason"
        );
        assert!(
            !observed
                .graph_completeness_reasons
                .contains(&GraphCompletenessReason::RuntimeNotObserved),
            "RuntimeNotObserved must be cleared once observed"
        );
    }

    /// #490 identity rule: diagnostic/ephemeral runtime facts (bound port, local
    /// URL — runtime-assigned, like a PID or container id) must NOT change
    /// `observed_execution_id`. Only the canonical envelope does.
    #[test]
    fn observed_id_ignores_diagnostic_runtime_facts() {
        let receipt = observation_scope_fixture_receipt();
        let envelope = ObservedLaunchEnvelope {
            resolved_execution_id: receipt.resolved_execution_id.clone(),
            runtime_kind: "source/node".to_string(),
            runtime_identity: Some("deno 2.6.8".to_string()),
            entrypoint: vec!["node".to_string(), "server.js".to_string()],
            working_directory: Some(".".to_string()),
            env_keys: vec!["PORT".to_string()],
            mount_targets: vec![],
            provider_projection_digest: None,
        };
        let a = receipt.clone().with_observation(
            ObservedRuntimeEvidence::new(envelope.clone())
                .with_bound_port(Some(18890))
                .with_local_url(Some("http://127.0.0.1:18890/".to_string())),
        );
        let b = receipt.with_observation(
            ObservedRuntimeEvidence::new(envelope)
                .with_bound_port(Some(40000))
                .with_local_url(Some("http://127.0.0.1:40000/".to_string())),
        );
        assert_eq!(
            a.observed_execution_id, b.observed_execution_id,
            "bound port / local url are diagnostic and must not affect identity"
        );
    }

    #[test]
    fn observation_scope_serde_roundtrip_uses_kebab_strings() {
        let scope = ObservationScope::declared_resolved();
        let json = serde_json::to_string(&scope).expect("serialize scope");
        // Layer/runtime states serialize as kebab-case strings.
        assert!(json.contains("\"present\""));
        assert!(json.contains("\"not-observed\""));
        let parsed: ObservationScope = serde_json::from_str(&json).expect("deserialize scope");
        assert_eq!(parsed, scope);

        // declared_only differs only in the resolved layer.
        let declared_only = ObservationScope::declared_only();
        assert_eq!(declared_only.resolved, LayerEvidence::Absent);
        assert_ne!(declared_only, scope);
    }

    #[test]
    fn legacy_receipt_without_observation_scope_roundtrips_to_none() {
        // Serialize a receipt, then strip `observation_scope` to model a
        // receipt written before the field existed. It must still parse, with
        // `observation_scope == None`, and the rest of the receipt intact.
        let receipt = observation_scope_fixture_receipt();
        let mut value = serde_json::to_value(&receipt).expect("to_value");
        let obj = value.as_object_mut().expect("receipt is a JSON object");
        assert!(
            obj.remove("observation_scope").is_some(),
            "fixture receipt must serialize observation_scope so the strip is meaningful"
        );

        let legacy: ExecutionReceiptV2 = serde_json::from_value(value).expect("legacy parse");
        assert!(
            legacy.observation_scope.is_none(),
            "missing observation_scope must deserialize to None (back-compat)"
        );
        // Other fields survive the roundtrip.
        assert_eq!(legacy.execution_id, receipt.execution_id);
        assert_eq!(legacy.graph_completeness, receipt.graph_completeness);
        assert!(legacy.observed_execution_id.is_none());
    }

    #[test]
    fn happy_path_receipt_defaults_observation_scope_to_none() {
        // `from_input` (the non-graph / legacy construction path) leaves the
        // scope unset; the graph-backed builder is what attaches it.
        let receipt =
            ExecutionReceiptV2::from_input(sample_input_v2(), "2026-05-03T00:00:00Z".to_string())
                .unwrap();
        assert!(receipt.observation_scope.is_none());
    }

    // -----------------------------------------------------------------
    // #494 GraphCompleteness reasons
    // -----------------------------------------------------------------

    #[test]
    fn declared_resolved_scope_reasons_are_runtime_not_observed_only() {
        let reasons = ObservationScope::declared_resolved().graph_completeness_reasons();
        // declared + resolved present ⇒ the only reason is the unobserved
        // runtime layer. Never empty (so the receipt can never be Complete).
        assert_eq!(reasons, vec![GraphCompletenessReason::RuntimeNotObserved]);
    }

    #[test]
    fn declared_only_scope_reasons_include_resolved_absent() {
        let reasons = ObservationScope::declared_only().graph_completeness_reasons();
        assert!(reasons.contains(&GraphCompletenessReason::RuntimeNotObserved));
        assert!(reasons.contains(&GraphCompletenessReason::ResolvedLayerAbsent));
    }

    #[test]
    fn every_observation_scope_yields_at_least_one_reason() {
        // Exhaustive over the runtime-observation variants and both
        // resolved-layer states: a scope is never "Complete" (empty reasons)
        // while runtime is unobserved.
        for observed in [
            RuntimeObservation::NotObserved,
            RuntimeObservation::OutOfScope,
            RuntimeObservation::Deferred,
        ] {
            for resolved in [LayerEvidence::Present, LayerEvidence::Absent] {
                let scope = ObservationScope {
                    declared: LayerEvidence::Present,
                    resolved,
                    observed,
                };
                assert!(
                    !scope.graph_completeness_reasons().is_empty(),
                    "scope {scope:?} must yield >= 1 partial reason"
                );
            }
        }
    }

    #[test]
    fn graph_backed_receipt_is_partial_because_runtime_not_observed() {
        use crate::engine::execution_graph::{
            ExecutionGraph, ExecutionGraphEdge, ExecutionGraphEdgeKind, ExecutionGraphNode,
        };
        // Mirror the ato-cli builder: Partial + scope + reasons derived from
        // the scope, with populated node/edge receipts.
        let graph = ExecutionGraph {
            nodes: vec![
                ExecutionGraphNode::Source {
                    identifier: "src:app".to_string(),
                },
                ExecutionGraphNode::DependencyOutput {
                    identifier: "dep:db".to_string(),
                },
            ],
            edges: vec![ExecutionGraphEdge {
                source: "src:app".to_string(),
                target: "dep:db".to_string(),
                kind: ExecutionGraphEdgeKind::DependsOn,
            }],
            labels: Default::default(),
            constraints: Vec::new(),
        };
        let receipt = observation_scope_fixture_receipt()
            .with_graph_projection(&graph)
            .with_graph_completeness_reasons(
                ObservationScope::declared_resolved().graph_completeness_reasons(),
            );

        // Acceptance: Partial always carries >= 1 reason, and the reason is
        // the unobserved runtime.
        assert_eq!(receipt.graph_completeness, Some(GraphCompleteness::Partial));
        assert!(!receipt.graph_completeness_reasons.is_empty());
        assert!(
            receipt
                .graph_completeness_reasons
                .contains(&GraphCompletenessReason::RuntimeNotObserved)
        );

        // Acceptance: non-empty node/edge receipts must NOT promote to Complete.
        assert!(
            !receipt.node_receipts.is_empty() || !receipt.edge_receipts.is_empty(),
            "fixture graph should populate node/edge receipts"
        );
        assert_ne!(
            receipt.graph_completeness,
            Some(GraphCompleteness::Complete),
            "Complete must never be emitted in this slice"
        );
        // And no observed id is synthesized.
        assert!(receipt.observed_execution_id.is_none());
    }

    #[test]
    fn partial_failure_receipt_carries_graph_completeness_reasons() {
        // No graph ids ⇒ declared/resolved absent ⇒ reasons include the
        // absent layers plus the unobserved runtime.
        let receipt = ExecutionReceiptV2::partial_failure(
            "2026-05-03T00:00:00Z".to_string(),
            ReceiptResultClass::RecoverableFailure,
            ReceiptFailureEnvelope {
                kind: ReceiptFailureKind::Recoverable,
                code: "E001".to_string(),
                name: "fixture".to_string(),
                phase: "manifest".to_string(),
                message: "fixture".to_string(),
                hint: None,
                resource: None,
                target: None,
                retryable: false,
                interactive_resolution_required: None,
                classification: None,
                cleanup_status: None,
                cleanup_actions: Vec::new(),
                manifest_suggestion: None,
                details: None,
            },
            None,
            None,
            None,
        );
        assert!(
            receipt
                .graph_completeness_reasons
                .contains(&GraphCompletenessReason::RuntimeNotObserved)
        );
        assert!(
            receipt
                .graph_completeness_reasons
                .contains(&GraphCompletenessReason::ResolvedLayerAbsent)
        );
        assert_ne!(
            receipt.graph_completeness,
            Some(GraphCompleteness::Complete)
        );
        assert!(receipt.observed_execution_id.is_none());
    }

    #[test]
    fn graph_completeness_reasons_serde_uses_kebab_and_roundtrips() {
        let receipt = observation_scope_fixture_receipt().with_graph_completeness_reasons(vec![
            GraphCompletenessReason::RuntimeNotObserved,
            GraphCompletenessReason::ResolvedLayerAbsent,
        ]);
        let json = serde_json::to_string(&receipt).expect("serialize");
        assert!(json.contains("\"runtime-not-observed\""));
        assert!(json.contains("\"resolved-layer-absent\""));
        let parsed: ExecutionReceiptV2 = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(
            parsed.graph_completeness_reasons,
            receipt.graph_completeness_reasons
        );
    }

    #[test]
    fn legacy_receipt_without_graph_completeness_reasons_roundtrips_to_empty() {
        // A receipt written before #494 has no `graph_completeness_reasons`
        // key; it must still parse, with the field defaulting to empty.
        let receipt = observation_scope_fixture_receipt()
            .with_graph_completeness_reasons(vec![GraphCompletenessReason::RuntimeNotObserved]);
        let mut value = serde_json::to_value(&receipt).expect("to_value");
        let obj = value.as_object_mut().expect("object");
        assert!(obj.remove("graph_completeness_reasons").is_some());

        let legacy: ExecutionReceiptV2 = serde_json::from_value(value).expect("legacy parse");
        assert!(legacy.graph_completeness_reasons.is_empty());
        assert_eq!(legacy.graph_completeness, receipt.graph_completeness);
    }

    #[test]
    fn ingress_in_oci_envelope_changes_identity() {
        use crate::types::{IngressConfig, IngressMode, IngressRoute};

        let mut routes = BTreeMap::new();
        routes.insert(
            "web".to_string(),
            IngressRoute {
                target: "web".to_string(),
                port: 3000,
                listed: true,
                alias: None,
                strip_prefix: true,
                upstream_path_prefix: None,
                root: true,
            },
        );

        let ingress = IngressConfig {
            mode: IngressMode::Path,
            routes,
            env_inject: BTreeMap::new(),
        };

        let envelope_no_ingress = sample_oci_envelope();
        let envelope_with_ingress = sample_oci_envelope().with_ingress(Some(ingress));

        let base = sample_input_v2();

        let input_no = base
            .clone()
            .with_oci_launch_envelope(Some(envelope_no_ingress));
        let input_with = base.with_oci_launch_envelope(Some(envelope_with_ingress));

        let id_no = compute_execution_id_v2(&input_no).unwrap();
        let id_with = compute_execution_id_v2(&input_with).unwrap();

        assert_ne!(
            id_no.execution_id, id_with.execution_id,
            "adding ingress to OCI envelope must change the execution identity"
        );
    }

    #[test]
    fn ingress_env_inject_template_changes_identity() {
        use crate::types::{IngressConfig, IngressMode, IngressRoute};

        let mut routes = BTreeMap::new();
        routes.insert(
            "api".to_string(),
            IngressRoute {
                target: "api".to_string(),
                port: 5001,
                listed: false,
                alias: Some("api".to_string()),
                strip_prefix: true,
                upstream_path_prefix: Some("/api".to_string()),
                root: false,
            },
        );

        let mut env_a = BTreeMap::new();
        env_a.insert("URL".to_string(), "{{ingress.routes.api.url}}".to_string());

        let ingress_a = IngressConfig {
            mode: IngressMode::Path,
            routes: routes.clone(),
            env_inject: {
                let mut m = BTreeMap::new();
                m.insert("web".to_string(), env_a);
                m
            },
        };

        let mut env_b = BTreeMap::new();
        env_b.insert(
            "URL".to_string(),
            "{{ingress.routes.api.base_url}}".to_string(),
        );

        let ingress_b = IngressConfig {
            mode: IngressMode::Path,
            routes,
            env_inject: {
                let mut m = BTreeMap::new();
                m.insert("web".to_string(), env_b);
                m
            },
        };

        let envelope_a = sample_oci_envelope().with_ingress(Some(ingress_a));
        let envelope_b = sample_oci_envelope().with_ingress(Some(ingress_b));

        let base = sample_input_v2();

        let input_a = base.clone().with_oci_launch_envelope(Some(envelope_a));
        let input_b = base.with_oci_launch_envelope(Some(envelope_b));

        let id_a = compute_execution_id_v2(&input_a).unwrap();
        let id_b = compute_execution_id_v2(&input_b).unwrap();

        assert_ne!(
            id_a.execution_id, id_b.execution_id,
            "different ingress env_inject templates must produce different identity hashes"
        );
    }
}
