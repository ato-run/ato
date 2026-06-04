//! Capsule Realization Contract (#498-A).
//!
//! > Ato guarantees that a resolved capsule can either reconstruct an
//! > equivalent launch envelope or fail with a typed explanation.
//!
//! This module is the *typed core* of that guarantee. It does not launch
//! anything and does not verify content hashes end-to-end (#499) or enforce a
//! strict fail-closed launch profile (#500); it classifies whether each node of
//! a resolved capsule can be **materialized, verified, or only conditionally
//! realized**, and explains — with typed reasons — when it cannot.
//!
//! ## Layers
//!
//! - [`model`] — the contract output types ([`RealizationContract`],
//!   [`RealizationStatus`], [`UnrealizableReason`], …). Pure data, serde-ready.
//! - [`classify`] — the pure classifier over typed per-node facts.
//! - [`bundle`] — an adapter from a real
//!   [`crate::engine::execution_graph::LaunchGraphBundle`] plus host/provider
//!   evidence onto a [`classify::RealizationRequest`].
//!
//! ## Boundaries this module keeps (#473, #501)
//!
//! - A runtime tool with no `binary_sha256` is `Unavailable`, never `Verified`.
//! - `HostBound` / `StateBound` / `PolicyDowngraded` stay visible and never
//!   collapse into a clean `Verified`.
//! - The only identity field is the graph-derived `resolved_execution_id`. A
//!   container id, pid, image digest, or rendered `docker`/`podman run` string
//!   is evidence derived from the graph, never identity.

pub mod bundle;
pub mod classify;
pub mod model;

#[cfg(test)]
mod tests;

pub use bundle::{
    ProviderProjectionEvidence, RealizationEnvironment, RuntimeEvidence, RuntimeToolEvidence,
    StateBindingEvidence, realization_from_launch_bundle,
};
pub use classify::{
    MountFact, RealizationEdge, RealizationNode, RealizationNodeFacts, RealizationRequest, classify,
};
pub use model::{
    RealizationContract, RealizationEdgeState, RealizationEdgeStatus, RealizationEvidence,
    RealizationNodeKind, RealizationNodeStatus, RealizationResult, RealizationStatus,
    UnrealizableReason,
};
