//! Phase 1 skeleton of the unified execution graph.
//!
//! This module is part of the v0.6.0 graph-based core migration tracked
//! by ato-run/ato#74 and partially addresses ato-run/ato#97
//! ([`ExecutionGraphBuilder`] canonicalization) and ato-run/ato#98
//! (canonical form + domain-tagged digest).
//!
//! **Status — not load-bearing.** The types and builder here are
//! deliberately minimal: only enough surface to be imported, exercised by
//! a couple of unit tests, and extended by the staged plan.
//! Specifically:
//!
//! - The builder consumes a *decoupled* [`ExecutionGraphBuildInput`]
//!   shape, **not** the real `Manifest` / `LockFile` / `Policy` types.
//!   That boundary is intentionally not crossed yet.
//! - The canonical form (`canonical` submodule) produces deterministic
//!   bytes and a SHA-256 digest under a [`CanonicalGraphDomain`], but is
//!   not wired into any receipt or session call site. Plumbing
//!   `declared_execution_id` / `resolved_execution_id` into the receipt
//!   types lands in Wave 3 (PR-5a).
//! - No production call site (session start, run pipeline, preflight)
//!   uses this module yet. Migrating those is Wave 2 / 3 (PR-4a, PR-4b).
//!
//! Consumers should treat [`ExecutionGraph`] as an internal staging
//! ground; canonicalization is now stable for the kinds it knows about
//! (see [`canonical::CANONICAL_FORM_VERSION`] and the spec at
//! `docs/execution-identity.md`).

mod builder;
pub mod canonical;
mod launch_bundle;
#[cfg(test)]
mod tests;
mod types;

pub use builder::{
    identity_labels, ExecutionGraphBuildInput, ExecutionGraphBuilder, GraphDependencyInput,
    GraphHostInput, GraphPolicyInput, GraphSourceInput, GraphTargetInput,
};
pub use canonical::{
    CanonicalGraphDomain, CanonicalizableGraph, GraphCanonicalForm, CANONICAL_FORM_VERSION,
};
pub use launch_bundle::{
    DerivedDependencyContracts, DerivedDependencyProvider, DerivedExecutionIds,
    DerivedPreflightView, DerivedReceiptSeed, GraphMaterializationSeedInput, GraphPreflightInput,
    GraphReceiptSeedInput, GraphRuntimeNodeInput, GraphRuntimeNodeKind, LaunchGraphBundle,
    LaunchGraphBundleInput, LaunchGraphDerivedViews,
};
pub use types::{
    ExecutionGraph, ExecutionGraphConstraint, ExecutionGraphEdge, ExecutionGraphEdgeKind,
    ExecutionGraphNode,
};
