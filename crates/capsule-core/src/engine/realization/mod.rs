//! Capsule Realization Contract (#498-A).
//!
//! > Ato guarantees that a resolved capsule can either reconstruct an
//! > equivalent launch envelope or fail with a typed explanation.
//!
//! This module is the *typed core* of that guarantee. It classifies whether
//! each node of a resolved capsule can be **materialized, verified, or only
//! conditionally realized**, explains — with typed reasons — when it cannot,
//! and (under an explicit strict profile) turns an unverifiable required node
//! into a launch that fails before execution rather than a silent or
//! merely-warned one.
//!
//! ## Layers
//!
//! - [`model`] — the contract output types ([`RealizationContract`],
//!   [`RealizationStatus`], [`UnrealizableReason`], …). Pure data, serde-ready.
//! - [`classify`] — the pure classifier over typed per-node facts.
//! - [`bundle`] — an adapter from a real
//!   [`crate::engine::execution_graph::LaunchGraphBundle`] plus host/provider
//!   evidence onto a [`classify::RealizationRequest`].
//! - [`verify`] — the pure materialization verifier (#499-A): compares declared
//!   vs actual content identity and maps the typed result back into the
//!   contract. It changes no launch behavior on its own.
//! - [`strict`] — the strict fail-closed launch gate (#500): consumes the
//!   `classify`/`verify` outputs and, in [`strict::LaunchProfile::Strict`],
//!   blocks a launch *before execution* with a typed, redacted error. The
//!   default [`strict::LaunchProfile::Normal`] never newly blocks a launch.
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
pub mod strict;
pub mod verify;

#[cfg(test)]
mod tests;

pub use bundle::{
    ProviderProjectionEvidence, RealizationEnvironment, RuntimeEvidence, RuntimeToolEvidence,
    StateBindingEvidence, materialization_request_from_launch_bundle,
    realization_from_launch_bundle,
};
pub use classify::{
    MountFact, RealizationEdge, RealizationNode, RealizationNodeFacts, RealizationRequest, classify,
};
pub use model::{
    RealizationContract, RealizationEdgeState, RealizationEdgeStatus, RealizationEvidence,
    RealizationNodeKind, RealizationNodeStatus, RealizationResult, RealizationStatus,
    RedactedProjectionCommand, UnrealizableReason,
};
pub use strict::{
    LaunchProfile, PolicyEnforcement, StateBindingCompatibility, StrictGateNodeInput,
    StrictGateReasonCode, StrictRealizationGate, StrictRealizationGateError, evaluate_strict_gate,
    evaluate_strict_gate_with_materialization,
};
pub use verify::{
    MaterializationHashError, MaterializationUnavailableReason, MaterializationVerification,
    MaterializationVerificationEvidence, MaterializationVerificationRequest,
    MaterializationVerificationResult, MaterializedHashProvider, MaterializedNodeInput,
    MaterializedNodeSource, materialization_result_to_realization_status,
    materialization_result_to_unrealizable_reason, verify_materialization,
    verify_materialization_with_provider,
};
