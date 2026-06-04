//! Unified execution graph (declared / resolved layers).
//!
//! This module is part of the v0.6.0 graph-based core migration tracked
//! by ato-run/ato#74 and lands ato-run/ato#97
//! ([`ExecutionGraphBuilder`] canonicalization) and ato-run/ato#98
//! (canonical form + domain-tagged digest).
//!
//! **Status — load-bearing.** The builder produces a `LaunchGraphBundle`
//! whose canonical `declared_execution_id` / `resolved_execution_id` are
//! consumed by production call sites (validate / preflight, the run
//! pipeline, and execution-receipt construction) and persisted to
//! `SessionRecord`. Notes on the current boundaries:
//!
//! - The builder consumes a *decoupled* [`ExecutionGraphBuildInput`]
//!   shape rather than the raw `Manifest` / `LockFile` / `Policy` types;
//!   an adapter feeds it from those sources. The decoupling is a
//!   deliberate seam, not an unfinished one.
//! - The canonical form (`canonical` submodule) produces deterministic
//!   bytes and a SHA-256 digest under a [`CanonicalGraphDomain`]; this is
//!   what backs the declared/resolved execution identities.
//! - The **observed** layer (`G_observed` / `observed_execution_id`) is
//!   not implemented yet — runtime observation, populated node/edge
//!   receipt evidence, computed `GraphCompleteness`, and drift detection
//!   are tracked by the Execution Graph Model RFC umbrella (ato-run/ato#490).
//!   See `docs/rfcs/draft/EXECUTION_GRAPH_MODEL.md`.
//!
//! Canonicalization is stable for the kinds it knows about (see
//! [`canonical::CANONICAL_FORM_VERSION`] and the spec at
//! `docs/execution-identity.md`).

mod builder;
pub mod canonical;
mod launch_bundle;
#[cfg(test)]
mod tests;
mod types;

pub use builder::{
    ExecutionGraphBuildInput, ExecutionGraphBuilder, GraphDependencyInput, GraphHostInput,
    GraphPolicyInput, GraphSourceInput, GraphTargetInput, identity_labels,
};
pub use canonical::{
    CANONICAL_FORM_VERSION, CanonicalGraphDomain, CanonicalizableGraph, GraphCanonicalForm,
};
pub use launch_bundle::{
    DerivedConsentView, DerivedDependencyContracts, DerivedDependencyProvider, DerivedExecutionIds,
    DerivedLaunchView, DerivedPreflightView, DerivedReceiptSeed, GraphConsentInput,
    GraphLaunchInput, GraphMaterializationSeedInput, GraphPreflightInput, GraphReceiptSeedInput,
    GraphRuntimeNodeInput, GraphRuntimeNodeKind, LaunchGraphBundle, LaunchGraphBundleInput,
    LaunchGraphDerivedViews,
};
pub use types::{
    ExecutionGraph, ExecutionGraphConstraint, ExecutionGraphEdge, ExecutionGraphEdgeKind,
    ExecutionGraphNode,
};
