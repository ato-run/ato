//! Strict fail-closed realization gate (#500).
//!
//! > Ato does not guarantee identical behavior. Ato guarantees that a resolved
//! > execution identity either reconstructs an equivalent launch envelope or
//! > fails with a typed explanation.
//!
//! [`super::classify`] (#498) decides, with typed reasons, whether a resolved
//! capsule *can* be realized; [`super::verify`] (#499) lets a node legitimately
//! claim [`RealizationStatus::Verified`]. Neither stops a launch — both are
//! deliberately observation-free and non-breaking. This module is the layer
//! that turns those findings into a launch decision **under an explicit strict
//! profile**: in [`LaunchProfile::Strict`] a node that cannot be verified blocks
//! the launch with a typed, redacted [`StrictRealizationGateError`] *before* any
//! guest process, runtime process, or container is created.
//!
//! ## What strict mode does (and only this)
//!
//! Strict mode escalates the realization findings #498/#499 already produce; it
//! never invents new claims. Concretely it blocks a launch when a required node
//! is:
//!
//! - [`RealizationStatus::Unavailable`] — missing/mismatched/invalid immutable
//!   input, a runtime tool with no `binary_sha256`, a missing dependency output,
//!   or a missing required state binding;
//! - [`RealizationStatus::HostBound`] — strict disallows the host fallback;
//! - [`RealizationStatus::PolicyDowngraded`] — strict requires enforcement.
//!
//! A [`RealizationStatus::Materializable`] node (re-derivable from a declared
//! identity) or a genuinely [`RealizationStatus::Verified`] node does **not**
//! block: strict mode is fail-closed, not refuse-everything.
//!
//! ## Boundaries it keeps (#473, #495, #501)
//!
//! - It is pure and provider-agnostic. Provider code may *produce* evidence; the
//!   gate only evaluates normalized realization/materialization facts.
//! - It never synthesizes an `observed_execution_id`, emits
//!   `GraphCompleteness::Complete`, claims runtime observation, or treats a
//!   container id / pid / log path / rendered provider command as identity.
//! - A runtime tool with no populated `binary_sha256` is blocked, never
//!   silently accepted as `Verified` (#473).
//! - The typed error carries only redacted, structured fields: node id/kind, a
//!   reason code, content-hash *summaries* (never a raw path, env value, secret,
//!   or provider command), and a short explanation.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::model::{
    RealizationContract, RealizationEvidence, RealizationNodeKind, RealizationNodeStatus,
    RealizationStatus,
};
use super::verify::{
    MaterializationUnavailableReason, MaterializationVerification,
    MaterializationVerificationResult, is_content_hash,
};

/// The launch profile governing how realization findings affect a launch.
///
/// [`Self::Normal`] is the default and is non-breaking: realization findings are
/// recorded as conservative receipts/reasons but never *newly* block a launch
/// (the #498/#499 semantics). [`Self::Strict`] is explicit, opt-in fail-closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum LaunchProfile {
    /// Conservative default. Strict-only verification gaps do not block launch.
    #[default]
    Normal,
    /// Fail-closed: an unverifiable required node blocks the launch with a typed
    /// error before execution.
    Strict,
}

impl LaunchProfile {
    pub fn is_strict(self) -> bool {
        matches!(self, Self::Strict)
    }
}

/// State-binding compatibility, when the launch path has already evaluated it.
///
/// Optional refinement: when `None`, the gate relies on the node's
/// [`RealizationStatus`] alone (a missing required binding already classifies as
/// [`RealizationStatus::Unavailable`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateBindingCompatibility {
    Compatible,
    Incompatible,
    NotApplicable,
}

/// Effective policy enforcement, when the launch path has already evaluated it.
///
/// Optional refinement over the node's [`RealizationStatus`]: a downgraded
/// enforcement reported here blocks strict launch even if the node otherwise
/// classified as materializable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyEnforcement {
    Enforced,
    Downgraded,
    NotApplicable,
}

/// Stable, machine-readable reason a strict launch was blocked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrictGateReasonCode {
    /// A required immutable input is missing or could not be materialized.
    MaterializationMissing,
    /// Declared identity and materialized identity disagree.
    IdentityMismatch,
    /// A declared or materialized identity is not a well-formed content hash.
    InvalidIdentity,
    /// A runtime tool has no populated `binary_sha256`, so it cannot be verified.
    RuntimeToolUnverified,
    /// A node requires a host-specific binding and strict disallows host
    /// fallback.
    HostBoundDisallowed,
    /// A required state-bound input has no compatible state binding.
    StateBindingMissing,
    /// A required policy cannot be fully enforced by the selected backend.
    PolicyDowngraded,
}

impl StrictGateReasonCode {
    /// A short, actionable, value-free explanation. It never embeds caller
    /// input, so it cannot leak a path, env value, secret, or command.
    pub fn explanation(self) -> &'static str {
        match self {
            Self::MaterializationMissing => {
                "required input could not be materialized or verified; its declared identity is missing or unavailable"
            }
            Self::IdentityMismatch => {
                "declared identity does not match the materialized identity; the input cannot be trusted"
            }
            Self::InvalidIdentity => {
                "declared or materialized identity is not a well-formed content hash and was rejected"
            }
            Self::RuntimeToolUnverified => {
                "runtime tool has no populated binary_sha256, so its identity cannot be verified"
            }
            Self::HostBoundDisallowed => {
                "node requires a host-specific binding; strict mode disallows host fallback"
            }
            Self::StateBindingMissing => {
                "required state-bound input has no compatible state binding"
            }
            Self::PolicyDowngraded => {
                "required policy cannot be fully enforced by the selected backend"
            }
        }
    }
}

/// Why a strict launch was blocked for one node. Serde-ready and redacted: the
/// only values that survive are the node id/kind, a reason code, content-hash
/// *summaries*, and a static explanation. A raw host path, env value, secret, or
/// rendered provider command can never reach this payload by construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("strict realization gate blocked launch for node '{node_id}': {reason_code:?}")]
pub struct StrictRealizationGateError {
    /// Always [`LaunchProfile::Strict`] — present so a reader can see the gate
    /// that produced the block.
    pub profile: LaunchProfile,
    pub node_id: String,
    pub node_kind: RealizationNodeKind,
    pub reason_code: StrictGateReasonCode,
    /// Redacted summary of the declared identity (a content-hash summary, or
    /// `<redacted>` for a value that is not a content hash). `None` when no
    /// declared identity was present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_identity: Option<String>,
    /// Redacted summary of the materialized identity. `None` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialized_identity: Option<String>,
    /// Short, actionable, value-free explanation.
    pub explanation: String,
}

impl StrictRealizationGateError {
    fn block(
        node_id: &str,
        node_kind: RealizationNodeKind,
        reason_code: StrictGateReasonCode,
        declared: Option<&str>,
        materialized: Option<&str>,
    ) -> Self {
        Self {
            profile: LaunchProfile::Strict,
            node_id: node_id.to_string(),
            node_kind,
            reason_code,
            declared_identity: redact_identity(declared),
            materialized_identity: redact_identity(materialized),
            explanation: reason_code.explanation().to_string(),
        }
    }
}

/// Normalized per-node facts the strict gate evaluates.
///
/// Assembled by the caller from #498 classification and #499 materialization
/// outputs. The gate stays pure and provider-agnostic: every provider-specific
/// concern has already been reduced to a normalized fact here.
#[derive(Debug, Clone)]
pub struct StrictGateNodeInput {
    pub node_id: String,
    pub node_kind: RealizationNodeKind,
    /// Whether this node is a required launch input. Non-required nodes only
    /// block on a *positively wrong* fact (mismatch / invalid / false-verified),
    /// never on an absent one.
    pub required: bool,
    /// Declared content identity, if any (a `algo:digest` content hash).
    pub declared_identity: Option<String>,
    /// Materialized content identity, if any.
    pub materialized_identity: Option<String>,
    /// #498 realization classification for this node.
    pub realization_status: RealizationStatus,
    /// #499 materialization-verifier verdict, when available. Takes precedence
    /// over [`Self::realization_status`] for the cases it covers.
    pub materialization: Option<MaterializationVerificationResult>,
    /// State-binding compatibility, when already evaluated by the launch path.
    pub state_binding: Option<StateBindingCompatibility>,
    /// Effective policy enforcement, when already evaluated.
    pub policy_enforcement: Option<PolicyEnforcement>,
}

impl StrictGateNodeInput {
    /// Whether this input *claims* to be verified (via either layer). A verified
    /// claim is only honored when a declared identity exists and the materialized
    /// identity matches it — a materialized object on its own is never enough.
    fn claims_verified(&self) -> bool {
        self.realization_status == RealizationStatus::Verified
            || matches!(
                self.materialization,
                Some(MaterializationVerificationResult::Verified)
            )
    }
}

/// The strict fail-closed realization gate.
pub struct StrictRealizationGate;

impl StrictRealizationGate {
    /// Evaluate a single normalized node under `profile`.
    ///
    /// `Ok(())` means the node does not block the launch. In
    /// [`LaunchProfile::Normal`] this is always `Ok(())` — normal mode never
    /// *newly* blocks a launch. In [`LaunchProfile::Strict`] an unverifiable
    /// required node, a mismatched/invalid identity, or a false "verified" claim
    /// returns a typed [`StrictRealizationGateError`].
    pub fn evaluate(
        input: &StrictGateNodeInput,
        profile: LaunchProfile,
    ) -> Result<(), StrictRealizationGateError> {
        if !profile.is_strict() {
            return Ok(());
        }

        let id = input.node_id.as_str();
        let kind = input.node_kind;
        let declared = input.declared_identity.as_deref();
        let materialized = input.materialized_identity.as_deref();

        // 1. The #499 materialization verifier is the highest authority for the
        //    cases it covers; honor its verdict first.
        if let Some(result) = &input.materialization {
            match result {
                MaterializationVerificationResult::Mismatch { expected, actual } => {
                    return Err(StrictRealizationGateError::block(
                        id,
                        kind,
                        StrictGateReasonCode::IdentityMismatch,
                        Some(expected),
                        Some(actual),
                    ));
                }
                MaterializationVerificationResult::Unavailable { reason } => {
                    return Err(StrictRealizationGateError::block(
                        id,
                        kind,
                        reason_code_for_materialization_unavailable(reason),
                        declared,
                        materialized,
                    ));
                }
                // A verified verdict still has to pass the consistency guard below.
                MaterializationVerificationResult::Verified => {}
            }
        }

        // 2. A declared or materialized identity that is not a well-formed
        //    content hash is rejected rather than trusted (#499 review). This is
        //    a *positively wrong* fact, so it blocks regardless of `required`.
        if let Some(value) = declared {
            if !is_content_hash(value) {
                return Err(StrictRealizationGateError::block(
                    id,
                    kind,
                    StrictGateReasonCode::InvalidIdentity,
                    declared,
                    materialized,
                ));
            }
        }
        if let Some(value) = materialized {
            if !is_content_hash(value) {
                return Err(StrictRealizationGateError::block(
                    id,
                    kind,
                    StrictGateReasonCode::InvalidIdentity,
                    declared,
                    materialized,
                ));
            }
        }

        // 3. Declared vs materialized mismatch (both present, both valid). Also a
        //    positively wrong fact — block regardless of `required`.
        if let (Some(d), Some(m)) = (declared, materialized) {
            if d != m {
                return Err(StrictRealizationGateError::block(
                    id,
                    kind,
                    StrictGateReasonCode::IdentityMismatch,
                    declared,
                    materialized,
                ));
            }
        }

        // 4. False-`Verified` guard (regression invariant): a `Verified` claim is
        //    only honored when a declared identity exists AND the materialized
        //    identity matches it. A materialized object existing by itself is not
        //    enough. This blocks regardless of `required` — a false verified
        //    claim is dangerous either way.
        if input.claims_verified() {
            if declared.is_none() {
                return Err(StrictRealizationGateError::block(
                    id,
                    kind,
                    StrictGateReasonCode::MaterializationMissing,
                    declared,
                    materialized,
                ));
            }
            if materialized.is_none() {
                return Err(StrictRealizationGateError::block(
                    id,
                    kind,
                    StrictGateReasonCode::MaterializationMissing,
                    declared,
                    materialized,
                ));
            }
            // declared == materialized (step 3) and both are content hashes
            // (step 2): a genuine verified node. Apply policy/state refinements
            // below, then accept.
        }

        // 5. Absent / can't-verify / downgraded facts only block a *required*
        //    node. An optional node that is merely undecided does not stop launch.
        if !input.required {
            return Ok(());
        }

        // A downgraded policy or an incompatible state binding blocks even when
        // the node otherwise classified as materializable/verified.
        if input.policy_enforcement == Some(PolicyEnforcement::Downgraded) {
            return Err(StrictRealizationGateError::block(
                id,
                kind,
                StrictGateReasonCode::PolicyDowngraded,
                declared,
                materialized,
            ));
        }
        if input.state_binding == Some(StateBindingCompatibility::Incompatible) {
            return Err(StrictRealizationGateError::block(
                id,
                kind,
                StrictGateReasonCode::StateBindingMissing,
                declared,
                materialized,
            ));
        }

        // 6. Status-driven blocks for required nodes.
        match input.realization_status {
            RealizationStatus::Unavailable => Err(StrictRealizationGateError::block(
                id,
                kind,
                reason_code_for_unavailable_kind(kind),
                declared,
                materialized,
            )),
            RealizationStatus::HostBound => Err(StrictRealizationGateError::block(
                id,
                kind,
                StrictGateReasonCode::HostBoundDisallowed,
                declared,
                materialized,
            )),
            RealizationStatus::PolicyDowngraded => Err(StrictRealizationGateError::block(
                id,
                kind,
                StrictGateReasonCode::PolicyDowngraded,
                declared,
                materialized,
            )),
            // Materializable / Verified / StateBound / Unknown: realizable. A
            // StateBound node with a *present* binding is allowed (an
            // incompatible one was already rejected in step 5).
            RealizationStatus::Materializable
            | RealizationStatus::Verified
            | RealizationStatus::StateBound
            | RealizationStatus::Unknown => Ok(()),
        }
    }
}

/// Evaluate a whole #498 [`RealizationContract`] under `profile`.
///
/// In [`LaunchProfile::Normal`] this is always `Ok(())`. In strict mode it
/// returns every per-node block (not just the first) so a caller can report all
/// the reasons a launch was refused.
pub fn evaluate_strict_gate(
    contract: &RealizationContract,
    profile: LaunchProfile,
) -> Result<(), Vec<StrictRealizationGateError>> {
    evaluate_strict_gate_with_materialization(contract, &[], profile)
}

/// Evaluate a #498 [`RealizationContract`], overlaying #499
/// [`MaterializationVerification`] verdicts by node id where present.
///
/// This is the entry that consumes *both* upstream outputs: the contract
/// supplies structure and classification (#498); the materialization verdicts
/// supply the authoritative verified/mismatch/unavailable judgment (#499).
pub fn evaluate_strict_gate_with_materialization(
    contract: &RealizationContract,
    materializations: &[MaterializationVerification],
    profile: LaunchProfile,
) -> Result<(), Vec<StrictRealizationGateError>> {
    if !profile.is_strict() {
        return Ok(());
    }

    let errors: Vec<StrictRealizationGateError> = contract
        .node_statuses
        .iter()
        .filter_map(|node| {
            let materialization = materializations
                .iter()
                .find(|m| m.node_id == node.node_id)
                .map(|m| m.result.clone());
            let input = node_input_from_status(node, materialization);
            StrictRealizationGate::evaluate(&input, profile).err()
        })
        .collect();

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Build a normalized [`StrictGateNodeInput`] from a contract node status. Every
/// node from a [`RealizationContract`] is treated as a required launch input:
/// the #498 classifier only emits the blocking statuses
/// (`Unavailable`/`HostBound`/`PolicyDowngraded`) for required nodes, so this is
/// faithful — an optional node classifies as `Unknown`/`Materializable`, which
/// never blocks.
fn node_input_from_status(
    node: &RealizationNodeStatus,
    materialization: Option<MaterializationVerificationResult>,
) -> StrictGateNodeInput {
    let (declared_identity, materialized_identity) = identities_from_evidence(&node.evidence);
    StrictGateNodeInput {
        node_id: node.node_id.clone(),
        node_kind: node.node_kind,
        required: true,
        declared_identity,
        materialized_identity,
        realization_status: node.status,
        materialization,
        state_binding: None,
        policy_enforcement: None,
    }
}

/// Extract declared/materialized content identities from a node's typed
/// evidence. Only content-hash-bearing evidence variants contribute; host path
/// roles, policy gaps, and redacted provider commands carry no identity.
fn identities_from_evidence(evidence: &[RealizationEvidence]) -> (Option<String>, Option<String>) {
    let mut declared = None;
    let mut materialized = None;
    for ev in evidence {
        match ev {
            RealizationEvidence::DeclaredHash { hash, .. } => declared = Some(hash.clone()),
            RealizationEvidence::VerifiedArtifact { hash, .. } => {
                declared = Some(hash.clone());
                materialized = Some(hash.clone());
            }
            RealizationEvidence::HashMismatch {
                declared: d,
                actual,
                ..
            } => {
                declared = Some(d.clone());
                materialized = Some(actual.clone());
            }
            RealizationEvidence::HostBinding { .. }
            | RealizationEvidence::StateBinding { .. }
            | RealizationEvidence::PolicyEnforcementGap { .. }
            | RealizationEvidence::DerivedProjectionCommand { .. }
            | RealizationEvidence::Note { .. } => {}
        }
    }
    (declared, materialized)
}

/// Map a #499 [`MaterializationUnavailableReason`] onto a strict reason code.
fn reason_code_for_materialization_unavailable(
    reason: &MaterializationUnavailableReason,
) -> StrictGateReasonCode {
    match reason {
        MaterializationUnavailableReason::RuntimeToolBinaryHashUnpopulated => {
            StrictGateReasonCode::RuntimeToolUnverified
        }
        MaterializationUnavailableReason::InvalidExpectedHashIdentity
        | MaterializationUnavailableReason::InvalidActualHashIdentity => {
            StrictGateReasonCode::InvalidIdentity
        }
        MaterializationUnavailableReason::MissingExpectedHash
        | MaterializationUnavailableReason::MissingMaterializedObject
        | MaterializationUnavailableReason::HashComputationUnavailable
        | MaterializationUnavailableReason::UnsupportedNodeKind => {
            StrictGateReasonCode::MaterializationMissing
        }
    }
}

/// Refine the reason code for an `Unavailable` node by its kind: a runtime tool
/// is specifically "unverified", a state binding is "missing"; everything else
/// is a generic missing materialization.
fn reason_code_for_unavailable_kind(kind: RealizationNodeKind) -> StrictGateReasonCode {
    match kind {
        RealizationNodeKind::RuntimeTool => StrictGateReasonCode::RuntimeToolUnverified,
        RealizationNodeKind::StateBinding => StrictGateReasonCode::StateBindingMissing,
        _ => StrictGateReasonCode::MaterializationMissing,
    }
}

/// Reduce an identity to a redacted, receipt-safe summary.
///
/// A well-formed content hash becomes `algo:prefix…` (algorithm plus a short
/// digest prefix) — enough to correlate, never the full value. Anything that is
/// **not** a content hash (a path, env assignment, or secret that slipped
/// through) is replaced wholesale with `<redacted>`: a raw local value is never
/// echoed into the error payload.
fn redact_identity(identity: Option<&str>) -> Option<String> {
    let value = identity?;
    if !is_content_hash(value) {
        return Some("<redacted>".to_string());
    }
    match value.split_once(':') {
        Some((algo, digest)) => {
            let prefix: String = digest.chars().take(12).collect();
            if digest.len() > 12 {
                Some(format!("{algo}:{prefix}…"))
            } else {
                Some(format!("{algo}:{prefix}"))
            }
        }
        None => Some("<redacted>".to_string()),
    }
}

#[cfg(test)]
mod tests;
