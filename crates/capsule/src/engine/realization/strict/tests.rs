//! Tests for the strict fail-closed realization gate (#500).

use super::*;
use crate::engine::realization::model::{
    RealizationContract, RealizationEvidence, RealizationNodeKind, RealizationNodeStatus,
    RealizationResult, RealizationStatus, UnrealizableReason,
};
use crate::engine::realization::verify::{
    MaterializationUnavailableReason, MaterializationVerification,
    MaterializationVerificationResult,
};

const HASH_A: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
const HASH_B: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
/// A raw host path — deliberately not a content hash.
const RAW_HOST_PATH: &str = "/home/alice/.ato/secret/material.bin";
/// A secret-bearing env assignment — deliberately not a content hash.
const RAW_SECRET: &str = "API_TOKEN=hunter2-do-not-leak";

/// A node input with sensible defaults; tests override only the relevant fields.
fn node(kind: RealizationNodeKind, status: RealizationStatus) -> StrictGateNodeInput {
    StrictGateNodeInput {
        node_id: "node-1".to_string(),
        node_kind: kind,
        required: true,
        declared_identity: None,
        materialized_identity: None,
        realization_status: status,
        materialization: None,
        state_binding: None,
        policy_enforcement: None,
    }
}

fn evaluate_strict(input: &StrictGateNodeInput) -> Result<(), StrictRealizationGateError> {
    StrictRealizationGate::evaluate(input, LaunchProfile::Strict)
}

// ---------------------------------------------------------------------------
// Blocking cases (strict profile)
// ---------------------------------------------------------------------------

#[test]
fn strict_blocks_missing_immutable_materialization() {
    let input = node(RealizationNodeKind::Source, RealizationStatus::Unavailable);
    let err = evaluate_strict(&input).expect_err("missing immutable input must block");
    assert_eq!(
        err.reason_code,
        StrictGateReasonCode::MaterializationMissing
    );
    assert_eq!(err.node_id, "node-1");
    assert_eq!(err.node_kind, RealizationNodeKind::Source);
}

#[test]
fn strict_blocks_mismatched_materialization_hash() {
    let mut input = node(RealizationNodeKind::Source, RealizationStatus::Unavailable);
    input.declared_identity = Some(HASH_A.to_string());
    input.materialized_identity = Some(HASH_B.to_string());
    let err = evaluate_strict(&input).expect_err("hash mismatch must block");
    assert_eq!(err.reason_code, StrictGateReasonCode::IdentityMismatch);
    // The declared/materialized summaries are present but truncated, never raw.
    assert!(
        err.declared_identity
            .as_deref()
            .unwrap()
            .starts_with("sha256:")
    );
    assert!(
        err.materialized_identity
            .as_deref()
            .unwrap()
            .starts_with("sha256:")
    );
}

#[test]
fn strict_blocks_mismatch_from_materialization_verifier() {
    // #499 verdict drives the decision even if the #498 status looks benign.
    let mut input = node(
        RealizationNodeKind::DependencyOutput,
        RealizationStatus::Materializable,
    );
    input.materialization = Some(MaterializationVerificationResult::Mismatch {
        expected: HASH_A.to_string(),
        actual: HASH_B.to_string(),
    });
    let err = evaluate_strict(&input).expect_err("verifier mismatch must block");
    assert_eq!(err.reason_code, StrictGateReasonCode::IdentityMismatch);
}

#[test]
fn strict_blocks_invalid_hash_identity() {
    let mut input = node(
        RealizationNodeKind::Source,
        RealizationStatus::Materializable,
    );
    input.declared_identity = Some("not-a-content-hash".to_string());
    let err = evaluate_strict(&input).expect_err("invalid identity must block");
    assert_eq!(err.reason_code, StrictGateReasonCode::InvalidIdentity);
}

#[test]
fn strict_blocks_runtime_tool_without_binary_sha256() {
    // The #498 path: a runtime tool with no binary_sha256 classifies Unavailable.
    let input = node(
        RealizationNodeKind::RuntimeTool,
        RealizationStatus::Unavailable,
    );
    let err = evaluate_strict(&input).expect_err("runtime tool without hash must block");
    assert_eq!(err.reason_code, StrictGateReasonCode::RuntimeToolUnverified);

    // The #499 path: the verifier reports the specific unpopulated reason.
    let mut via_verifier = node(RealizationNodeKind::RuntimeTool, RealizationStatus::Unknown);
    via_verifier.materialization = Some(MaterializationVerificationResult::Unavailable {
        reason: MaterializationUnavailableReason::RuntimeToolBinaryHashUnpopulated,
    });
    let err = evaluate_strict(&via_verifier).expect_err("runtime tool without hash must block");
    assert_eq!(err.reason_code, StrictGateReasonCode::RuntimeToolUnverified);
}

#[test]
fn strict_blocks_host_bound_fallback() {
    let input = node(
        RealizationNodeKind::FilesystemView,
        RealizationStatus::HostBound,
    );
    let err = evaluate_strict(&input).expect_err("host-bound fallback must block");
    assert_eq!(err.reason_code, StrictGateReasonCode::HostBoundDisallowed);
}

#[test]
fn strict_blocks_policy_downgrade_when_enforcement_required() {
    // Via the #498 status.
    let input = node(
        RealizationNodeKind::NetworkPolicy,
        RealizationStatus::PolicyDowngraded,
    );
    let err = evaluate_strict(&input).expect_err("policy downgrade must block");
    assert_eq!(err.reason_code, StrictGateReasonCode::PolicyDowngraded);

    // Via the explicit refinement field even when the status is materializable.
    let mut refined = node(
        RealizationNodeKind::NetworkPolicy,
        RealizationStatus::Materializable,
    );
    refined.policy_enforcement = Some(PolicyEnforcement::Downgraded);
    let err = evaluate_strict(&refined).expect_err("downgraded enforcement must block");
    assert_eq!(err.reason_code, StrictGateReasonCode::PolicyDowngraded);
}

#[test]
fn strict_blocks_state_bound_without_compatible_binding() {
    // A missing required state binding classifies Unavailable in #498.
    let input = node(
        RealizationNodeKind::StateBinding,
        RealizationStatus::Unavailable,
    );
    let err = evaluate_strict(&input).expect_err("missing state binding must block");
    assert_eq!(err.reason_code, StrictGateReasonCode::StateBindingMissing);

    // An explicit incompatible-binding finding blocks even a StateBound node.
    let mut incompatible = node(
        RealizationNodeKind::StateBinding,
        RealizationStatus::StateBound,
    );
    incompatible.state_binding = Some(StateBindingCompatibility::Incompatible);
    let err = evaluate_strict(&incompatible).expect_err("incompatible binding must block");
    assert_eq!(err.reason_code, StrictGateReasonCode::StateBindingMissing);
}

// ---------------------------------------------------------------------------
// Non-blocking cases
// ---------------------------------------------------------------------------

#[test]
fn strict_allows_materializable_and_verified() {
    let materializable = {
        let mut n = node(
            RealizationNodeKind::Runtime,
            RealizationStatus::Materializable,
        );
        n.declared_identity = Some(HASH_A.to_string());
        n
    };
    assert!(evaluate_strict(&materializable).is_ok());

    let verified = {
        let mut n = node(RealizationNodeKind::Runtime, RealizationStatus::Verified);
        n.declared_identity = Some(HASH_A.to_string());
        n.materialized_identity = Some(HASH_A.to_string());
        n
    };
    assert!(evaluate_strict(&verified).is_ok());
}

#[test]
fn strict_allows_present_state_binding() {
    let mut bound = node(
        RealizationNodeKind::StateBinding,
        RealizationStatus::StateBound,
    );
    bound.state_binding = Some(StateBindingCompatibility::Compatible);
    assert!(evaluate_strict(&bound).is_ok());
}

#[test]
fn normal_profile_is_non_breaking() {
    // Every status that blocks under strict mode must pass under normal mode.
    for status in [
        RealizationStatus::Unavailable,
        RealizationStatus::HostBound,
        RealizationStatus::PolicyDowngraded,
        RealizationStatus::Materializable,
        RealizationStatus::Verified,
    ] {
        let input = node(RealizationNodeKind::Source, status);
        assert!(
            StrictRealizationGate::evaluate(&input, LaunchProfile::Normal).is_ok(),
            "normal mode must not block status {status:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Regression invariant: a materialized object alone is not enough
// ---------------------------------------------------------------------------

#[test]
fn verified_requires_declared_and_materialized_to_match() {
    // A "verified" claim with a materialized object but no declared identity to
    // verify against must NOT be honored — it is blocked, not accepted.
    let mut materialized_only = node(RealizationNodeKind::Source, RealizationStatus::Verified);
    materialized_only.declared_identity = None;
    materialized_only.materialized_identity = Some(HASH_A.to_string());
    let err = evaluate_strict(&materialized_only)
        .expect_err("a materialized object on its own is not a verified node");
    assert_eq!(
        err.reason_code,
        StrictGateReasonCode::MaterializationMissing
    );

    // A "verified" claim whose materialized identity disagrees with the declared
    // identity is a mismatch, never a pass.
    let mut disagreeing = node(RealizationNodeKind::Source, RealizationStatus::Verified);
    disagreeing.declared_identity = Some(HASH_A.to_string());
    disagreeing.materialized_identity = Some(HASH_B.to_string());
    let err = evaluate_strict(&disagreeing).expect_err("declared != materialized must block");
    assert_eq!(err.reason_code, StrictGateReasonCode::IdentityMismatch);
}

// ---------------------------------------------------------------------------
// Redaction & payload-shape guarantees
// ---------------------------------------------------------------------------

#[test]
fn typed_error_includes_node_id_kind_and_reason() {
    let input = node(
        RealizationNodeKind::DependencyOutput,
        RealizationStatus::Unavailable,
    );
    let err = evaluate_strict(&input).expect_err("must block");
    let json = serde_json::to_value(&err).expect("serialize");
    assert_eq!(json["node_id"], "node-1");
    assert_eq!(json["node_kind"], "dependency-output");
    assert_eq!(json["reason_code"], "materialization_missing");
    assert_eq!(json["profile"], "strict");
    assert!(!json["explanation"].as_str().unwrap().is_empty());
}

#[test]
fn typed_error_does_not_include_secret_values() {
    let mut input = node(
        RealizationNodeKind::EnvClosure,
        RealizationStatus::Materializable,
    );
    input.declared_identity = Some(RAW_SECRET.to_string());
    let err = evaluate_strict(&input).expect_err("invalid identity must block");
    let serialized = serde_json::to_string(&err).expect("serialize");
    assert!(
        !serialized.contains("hunter2"),
        "secret value must never appear in the error payload: {serialized}"
    );
    // A non-hash identity is reduced to a placeholder, never echoed.
    assert_eq!(err.declared_identity.as_deref(), Some("<redacted>"));
}

#[test]
fn typed_error_does_not_include_raw_host_paths() {
    let mut input = node(
        RealizationNodeKind::FilesystemView,
        RealizationStatus::Materializable,
    );
    input.materialized_identity = Some(RAW_HOST_PATH.to_string());
    let err = evaluate_strict(&input).expect_err("invalid identity must block");
    let serialized = serde_json::to_string(&err).expect("serialize");
    assert!(
        !serialized.contains("/home/alice"),
        "raw host path must never appear in the error payload: {serialized}"
    );
    assert_eq!(err.materialized_identity.as_deref(), Some("<redacted>"));
}

#[test]
fn typed_error_does_not_claim_runtime_observation() {
    let input = node(RealizationNodeKind::Source, RealizationStatus::Unavailable);
    let err = evaluate_strict(&input).expect_err("must block");
    let serialized = serde_json::to_string(&err).expect("serialize");
    // The strict gate is a pre-execution decision: it must never fabricate an
    // observed execution id, a completeness claim, or a runtime-observation flag.
    for forbidden in [
        "observed_execution_id",
        "GraphCompleteness",
        "Complete",
        "observed",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "strict error must not reference '{forbidden}': {serialized}"
        );
    }
}

// ---------------------------------------------------------------------------
// Contract-level evaluation (consumes #498 + #499 outputs)
// ---------------------------------------------------------------------------

fn node_status(
    id: &str,
    kind: RealizationNodeKind,
    status: RealizationStatus,
    evidence: Vec<RealizationEvidence>,
) -> RealizationNodeStatus {
    RealizationNodeStatus {
        node_id: id.to_string(),
        node_kind: kind,
        status,
        evidence,
    }
}

#[test]
fn evaluate_contract_blocks_in_strict_passes_in_normal() {
    let contract = RealizationContract {
        resolved_execution_id: "rid-1".to_string(),
        node_statuses: vec![
            node_status(
                "src",
                RealizationNodeKind::Source,
                RealizationStatus::Verified,
                vec![RealizationEvidence::VerifiedArtifact {
                    label: "source-tree".into(),
                    hash: HASH_A.into(),
                }],
            ),
            node_status(
                "dep",
                RealizationNodeKind::DependencyOutput,
                RealizationStatus::Unavailable,
                vec![],
            ),
        ],
        edge_statuses: vec![],
        result: RealizationResult::Unrealizable {
            reasons: vec![UnrealizableReason::MissingDependencyOutput {
                node_id: "dep".into(),
            }],
        },
    };

    // Normal mode: never blocks.
    assert!(evaluate_strict_gate(&contract, LaunchProfile::Normal).is_ok());

    // Strict mode: the unavailable dependency blocks; the verified source does not.
    let errors = evaluate_strict_gate(&contract, LaunchProfile::Strict)
        .expect_err("strict must block the unavailable dependency");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].node_id, "dep");
    assert_eq!(
        errors[0].reason_code,
        StrictGateReasonCode::MaterializationMissing
    );
}

#[test]
fn evaluate_contract_with_materialization_overlay() {
    // The #498 contract classifies the node as materializable (declared hash
    // present), but the #499 verifier finds the materialized object mismatched.
    let contract = RealizationContract {
        resolved_execution_id: "rid-2".to_string(),
        node_statuses: vec![node_status(
            "dep",
            RealizationNodeKind::DependencyOutput,
            RealizationStatus::Materializable,
            vec![RealizationEvidence::DeclaredHash {
                label: "dependency-output".into(),
                hash: HASH_A.into(),
            }],
        )],
        edge_statuses: vec![],
        result: RealizationResult::Realized,
    };

    let materializations = vec![MaterializationVerification {
        node_id: "dep".to_string(),
        node_kind: RealizationNodeKind::DependencyOutput,
        result: MaterializationVerificationResult::Mismatch {
            expected: HASH_A.to_string(),
            actual: HASH_B.to_string(),
        },
        evidence: vec![],
    }];

    // Without the overlay the contract alone would pass strict (materializable).
    assert!(evaluate_strict_gate(&contract, LaunchProfile::Strict).is_ok());

    // With the #499 overlay the mismatch is caught and the launch is blocked.
    let errors = evaluate_strict_gate_with_materialization(
        &contract,
        &materializations,
        LaunchProfile::Strict,
    )
    .expect_err("materialization mismatch must block");
    assert_eq!(errors.len(), 1);
    assert_eq!(
        errors[0].reason_code,
        StrictGateReasonCode::IdentityMismatch
    );
}

#[test]
fn materialization_verified_overlay_passes_declared_only_contract() {
    // #498 classifies the node as Materializable (only a declared hash is in the
    // contract evidence — the materialized identity is not separately present).
    // #499 returns Verified, which is itself proof the declared/materialized pair
    // matched. The strict gate must trust that verdict and pass — not block it as
    // a false-verified "materialization_missing".
    let contract = RealizationContract {
        resolved_execution_id: "rid-3".to_string(),
        node_statuses: vec![node_status(
            "dep",
            RealizationNodeKind::DependencyOutput,
            RealizationStatus::Materializable,
            vec![RealizationEvidence::DeclaredHash {
                label: "dependency-output".into(),
                hash: HASH_A.into(),
            }],
        )],
        edge_statuses: vec![],
        result: RealizationResult::Realized,
    };

    let materializations = vec![MaterializationVerification {
        node_id: "dep".to_string(),
        node_kind: RealizationNodeKind::DependencyOutput,
        result: MaterializationVerificationResult::Verified,
        evidence: vec![],
    }];

    assert!(
        evaluate_strict_gate_with_materialization(
            &contract,
            &materializations,
            LaunchProfile::Strict,
        )
        .is_ok(),
        "a #499 Verified verdict must pass a declared-only contract node",
    );
}

#[test]
fn strict_trusts_materialization_verified_verdict() {
    // Direct per-node form: a Verified #499 verdict is authoritative even when the
    // contract carries only a declared identity (no separate materialized one)...
    let mut declared_only = node(
        RealizationNodeKind::Runtime,
        RealizationStatus::Materializable,
    );
    declared_only.declared_identity = Some(HASH_A.to_string());
    declared_only.materialized_identity = None;
    declared_only.materialization = Some(MaterializationVerificationResult::Verified);
    assert!(evaluate_strict(&declared_only).is_ok());

    // ...and it supersedes a #498 `Unavailable` materialization classification
    // (the verifier found and matched the artifact the contract thought missing).
    let mut unavailable_but_verified =
        node(RealizationNodeKind::Source, RealizationStatus::Unavailable);
    unavailable_but_verified.declared_identity = Some(HASH_A.to_string());
    unavailable_but_verified.materialization = Some(MaterializationVerificationResult::Verified);
    assert!(evaluate_strict(&unavailable_but_verified).is_ok());

    // But it does NOT override an orthogonal host-binding block.
    let mut verified_yet_host_bound = node(
        RealizationNodeKind::FilesystemView,
        RealizationStatus::HostBound,
    );
    verified_yet_host_bound.declared_identity = Some(HASH_A.to_string());
    verified_yet_host_bound.materialization = Some(MaterializationVerificationResult::Verified);
    let err = evaluate_strict(&verified_yet_host_bound)
        .expect_err("a verified content hash does not waive a host-fallback block");
    assert_eq!(err.reason_code, StrictGateReasonCode::HostBoundDisallowed);
}
