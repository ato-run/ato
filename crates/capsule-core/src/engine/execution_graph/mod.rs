//! Phase 1 skeleton of the unified execution graph.
//!
//! This module is part of the v0.6.0 graph-based core migration tracked
//! by ato-run/ato#74 and partially addresses ato-run/ato#97
//! ([`ExecutionGraphBuilder`] canonicalization).
//!
//! **Status — not load-bearing.** The types and builder here are
//! deliberately minimal: only enough surface to be imported, exercised by
//! a couple of unit tests, and extended by the staged plan.
//! Specifically:
//!
//! - The builder consumes a *decoupled* [`ExecutionGraphBuildInput`]
//!   shape, **not** the real `Manifest` / `LockFile` / `Policy` types.
//!   That boundary is intentionally not crossed yet.
//! - There is no canonicalization, no JCS hashing, and no
//!   `declared` / `resolved` / `observed` execution-id derivation. That
//!   is Wave 2 (#98).
//! - No production call site (session start, run pipeline, preflight)
//!   uses this module yet. Migrating those is Wave 2 / 3 (PR-4a, PR-4b).
//!
//! Consumers should treat [`ExecutionGraph`] as an internal staging
//! ground and **not** depend on its current shape being canonical.

mod builder;
#[cfg(test)]
mod tests;
mod types;

pub use builder::{
    ExecutionGraphBuildInput, ExecutionGraphBuilder, GraphDependencyInput, GraphHostInput,
    GraphPolicyInput, GraphSourceInput, GraphTargetInput,
};
pub use types::{
    ExecutionGraph, ExecutionGraphConstraint, ExecutionGraphEdge, ExecutionGraphEdgeKind,
    ExecutionGraphNode,
};
