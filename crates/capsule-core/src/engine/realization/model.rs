//! Output model for the Capsule Realization Contract (#498-A).
//!
//! These types are the *typed answer* to a single question:
//!
//! > Can a resolved Capsule reconstruct an equivalent launch envelope, or
//! > does it fail with a typed explanation of what is missing?
//!
//! They are deliberately a pure data model — no I/O, no provider handles, no
//! runtime evidence. A [`RealizationContract`] is produced by
//! [`super::classify`] from typed node facts, or by [`super::bundle`] from a
//! [`crate::engine::execution_graph::LaunchGraphBundle`] plus host/provider
//! evidence.
//!
//! ## Identity boundary (#501)
//!
//! The contract's only identity field is [`RealizationContract::resolved_execution_id`],
//! which is always the graph-derived resolved execution id. A container id,
//! process id, image digest, or rendered `docker`/`podman run` string is
//! **never** identity here — when such a string appears it is carried as
//! [`RealizationEvidence::DerivedProjectionCommand`], i.e. evidence derived
//! *from* the graph, not the source of truth.

use serde::{Deserialize, Serialize};

/// The kind of node being classified.
///
/// This is the realization-contract taxonomy, not the
/// [`crate::engine::execution_graph::ExecutionGraphNode`] taxonomy. The two
/// overlap but are not identical: the realization view splits concerns the
/// raw graph keeps merged (e.g. a policy facet becomes a distinct
/// [`Self::NetworkPolicy`] / [`Self::CapabilityPolicy`] node), and #498-A only
/// classifies the subset that can be grounded from today's launch bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RealizationNodeKind {
    /// Project source tree.
    Source,
    /// Language/runtime toolchain the entrypoint executes against.
    Runtime,
    /// A pinned runtime tool binary (keyed by lockfile `binary_sha256`).
    RuntimeTool,
    /// A dependency derivation (the build that produces a dependency output).
    DependencyDerivation,
    /// A concrete dependency output artifact.
    DependencyOutput,
    /// The materialized filesystem view (mounts/overlays) presented to launch.
    FilesystemView,
    /// The closed set of environment values required at launch.
    EnvClosure,
    /// A network policy facet (deny-by-default, allow-list, …).
    CapabilityPolicy,
    /// A capability/sandbox policy facet (mount readonly, dropped caps, …).
    NetworkPolicy,
    /// A binding to persistent/prior state.
    StateBinding,
    /// The launch entrypoint (argv + cwd).
    Entrypoint,
    /// A provider projection of the resolved capsule (e.g. OCI). The rendered
    /// invocation is evidence, never identity.
    ProviderProjection,
}

/// Typed classification of whether a single node can be realized.
///
/// The three "bound" statuses ([`Self::HostBound`], [`Self::StateBound`],
/// [`Self::PolicyDowngraded`]) are **not** failures: a capsule can still be
/// [`RealizationResult::Realized`] while depending on host conditions,
/// persistent state, or a provider that cannot fully enforce a policy. They
/// exist so those facts stay *visible* rather than being silently absorbed
/// into a clean `Verified`. Only [`Self::Unavailable`] forces an
/// [`RealizationResult::Unrealizable`] result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RealizationStatus {
    /// Re-derivable: materializing it would reconstruct the node, and there is
    /// declared evidence (a hash/identity) backing that claim.
    Materializable,
    /// Already present and checked against its declared identity (local
    /// artifact, runtime, source, or state that exists and matches).
    Verified,
    /// Depends on a host-specific condition (an absolute host path, a host
    /// fingerprint). Realizable, but not host-portable.
    HostBound,
    /// Depends on persistent state or a prior binding that lives outside the
    /// resolved graph.
    StateBound,
    /// A required node cannot be reconstructed or verified.
    Unavailable,
    /// A policy is required but the projecting provider cannot fully enforce
    /// it. The downgrade is surfaced rather than reported as clean.
    PolicyDowngraded,
    /// Classification could not be decided from available facts.
    Unknown,
}

impl RealizationStatus {
    /// Whether this status, on a node the contract treats as required, makes
    /// the overall result [`RealizationResult::Unrealizable`].
    ///
    /// Only [`Self::Unavailable`] is fail-closed here. Strict fail-closed for
    /// `PolicyDowngraded`/`HostBound` is a *profile* decision and is out of
    /// scope for #498-A (it lands with strict-profile launch policy, #500).
    pub fn is_blocking(self) -> bool {
        matches!(self, Self::Unavailable)
    }
}

/// A typed piece of evidence backing a node's status.
///
/// Evidence never contains resolved secret values. The
/// [`Self::DerivedProjectionCommand`] variant is the explicit home for a
/// rendered provider invocation (e.g. an OCI `podman run …` string): it is
/// derived *from* the graph and recorded as evidence, never promoted to
/// identity (#501).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "evidence", rename_all = "kebab-case")]
pub enum RealizationEvidence {
    /// A declared content/identity hash that makes a node materializable.
    DeclaredHash { label: String, hash: String },
    /// A present artifact verified against its declared identity.
    VerifiedArtifact { label: String, hash: String },
    /// A host-specific binding (e.g. an absolute host path role).
    HostBinding { detail: String },
    /// A persistent-state binding reference (never a secret value).
    StateBinding { reference: String },
    /// A gap between a required policy and provider enforcement.
    PolicyEnforcementGap { policy: String, detail: String },
    /// A rendered provider invocation, derived from the graph. Evidence only —
    /// explicitly **not** identity (#501).
    DerivedProjectionCommand { provider: String, command: String },
    /// Free-form note for facts that do not yet have a typed variant.
    Note { detail: String },
}

/// Why a resolved capsule cannot reconstruct an equivalent launch envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum UnrealizableReason {
    /// A required immutable input node cannot be reconstructed or verified.
    MissingImmutableInput {
        node_id: String,
        node_kind: RealizationNodeKind,
    },
    /// A runtime tool's `binary_sha256` is not populated, so it cannot be
    /// verified. Until cross-platform lockfile population lands it must be
    /// `Unavailable`, never `Verified` (#473).
    RuntimeToolBinaryHashUnavailable { node_id: String },
    /// A required dependency output has no content hash to materialize from.
    MissingDependencyOutput { node_id: String },
    /// A required persistent-state binding is missing and no creation policy
    /// exists to establish it.
    MissingStateBinding { node_id: String },
    /// The launch requires an environment value that is not declared/closed.
    UndeclaredEnvRequired { node_id: String },
}

/// The overall verdict of a [`RealizationContract`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum RealizationResult {
    /// The resolved capsule can reconstruct an equivalent launch envelope.
    Realized,
    /// It cannot, with one or more typed explanations.
    Unrealizable { reasons: Vec<UnrealizableReason> },
}

impl RealizationResult {
    pub fn is_realized(&self) -> bool {
        matches!(self, Self::Realized)
    }
}

/// Per-node classification result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealizationNodeStatus {
    pub node_id: String,
    pub node_kind: RealizationNodeKind,
    pub status: RealizationStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<RealizationEvidence>,
}

/// Whether an edge's endpoints can both be realized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RealizationEdgeState {
    /// Both endpoints are realizable (neither is `Unavailable`).
    Connectable,
    /// At least one endpoint is `Unavailable`, so the relationship cannot hold.
    BrokenByUnavailableEndpoint,
}

/// Per-edge classification result. The edge `kind` is carried as a string so
/// the contract stays decoupled from the
/// [`crate::engine::execution_graph::ExecutionGraphEdgeKind`] vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealizationEdgeStatus {
    pub source: String,
    pub target: String,
    pub kind: String,
    pub state: RealizationEdgeState,
}

/// The typed Capsule Realization Contract for one resolved capsule.
///
/// `resolved_execution_id` is the sole identity field and is always the
/// graph-derived resolved execution id — never a container id, pid, or
/// rendered command string (#501).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RealizationContract {
    pub resolved_execution_id: String,
    pub node_statuses: Vec<RealizationNodeStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edge_statuses: Vec<RealizationEdgeStatus>,
    pub result: RealizationResult,
}

impl RealizationContract {
    /// Iterator over node statuses with the given status value.
    pub fn nodes_with_status(
        &self,
        status: RealizationStatus,
    ) -> impl Iterator<Item = &RealizationNodeStatus> {
        self.node_statuses
            .iter()
            .filter(move |n| n.status == status)
    }

    /// Whether any node carries the given status.
    pub fn has_status(&self, status: RealizationStatus) -> bool {
        self.nodes_with_status(status).next().is_some()
    }
}
