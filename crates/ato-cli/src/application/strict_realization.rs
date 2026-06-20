//! Strict fail-closed realization gate wiring for `ato run` (#500).
//!
//! This is the thin launch-path adapter around the pure
//! [`capsule::realization::strict`] gate. It runs at the prelaunch boundary
//! — after the [`LaunchGraphBundle`] is built and before any guest process,
//! runtime process, or container is created — and only when the operator has
//! explicitly opted into the strict profile (`--strict-realization`).
//!
//! Responsibilities (kept deliberately small):
//!
//! 1. Map the CLI flag onto a [`LaunchProfile`].
//! 2. Project the launch bundle + available host/provider evidence into the #498
//!    [`RealizationContract`] via [`realization_from_launch_bundle`].
//! 3. Run the core strict gate and, on a block, convert its typed per-node
//!    failures into a single typed [`AtoExecutionError`] carrying the redacted
//!    structured payload.
//!
//! All realization *logic* lives in `capsule`; this module makes no launch
//! decisions of its own beyond "strict on ⇒ consult the gate".
//!
//! ## Evidence scope (#501)
//!
//! The host/provider realization-evidence producer (declared/materialized
//! content hashes, provider enforcement capability, state bindings) is #501 and
//! is not yet wired into the launch path. Until it lands, [`launch_environment`]
//! supplies the conservative default, so strict mode is genuinely fail-closed:
//! a required input whose identity cannot yet be grounded is blocked rather than
//! waved through. The default profile is unaffected and never consults the gate.

use capsule::engine::execution_graph::LaunchGraphBundle;
use capsule::execution_plan::error::AtoExecutionError;
use capsule::realization::bundle::RealizationEnvironment;
use capsule::realization::{
    LaunchProfile, MaterializationVerification, RealizationContract, StrictRealizationGateError,
    evaluate_strict_gate_with_materialization, materialization_request_from_launch_bundle,
    realization_from_launch_bundle, verify_materialization,
};

/// Map the `--strict-realization` flag onto a launch profile. `false` (the
/// default) is the conservative, non-breaking [`LaunchProfile::Normal`].
pub(crate) fn launch_profile(strict_realization: bool) -> LaunchProfile {
    if strict_realization {
        LaunchProfile::Strict
    } else {
        LaunchProfile::Normal
    }
}

/// The host/provider realization evidence available to the launch path today.
///
/// Conservative by design: the producer that grounds declared/materialized
/// content identity, provider enforcement capability, and state bindings is
/// #501. Returning the default keeps strict mode honestly fail-closed until that
/// evidence exists, and keeps this seam in one obvious place for #501 to enrich.
pub(crate) fn launch_environment() -> RealizationEnvironment {
    RealizationEnvironment::default()
}

/// Enforce the strict realization gate over a launch bundle before execution.
///
/// In [`LaunchProfile::Normal`] this is a no-op (`Ok`). In
/// [`LaunchProfile::Strict`] it consumes **both** upstream layers — it builds the
/// #498 realization contract and runs the #499 materialization verifier over the
/// bundle's materialized evidence — then blocks the launch with a typed
/// [`AtoExecutionError`] if any required input cannot be verified.
///
/// Today the #499 overlay is conservative/empty because the materialized-hash
/// producer is #501; the same call path carries real verifier verdicts the
/// moment that evidence exists (see
/// [`materialization_request_from_launch_bundle`]).
pub(crate) fn enforce_strict_realization(
    bundle: &LaunchGraphBundle,
    env: &RealizationEnvironment,
    profile: LaunchProfile,
) -> Result<(), Box<AtoExecutionError>> {
    if !profile.is_strict() {
        return Ok(());
    }
    // #498 — classify the resolved graph into a realization contract.
    let contract = realization_from_launch_bundle(bundle, env);
    // #499 — verify the materialized evidence and overlay its verdicts (matched
    // by node id) so a mismatch/invalid/unpopulated artifact is caught even when
    // the #498 classification alone would deem the node materializable.
    let materializations =
        verify_materialization(materialization_request_from_launch_bundle(bundle, env));
    evaluate_contract(&contract, &materializations, profile)
}

/// Run the core gate over an already-built #498 contract plus #499 verifier
/// verdicts, and convert a block into a typed launch error. Split out from
/// [`enforce_strict_realization`] so it can be unit-tested against hand-built
/// contracts/verdicts without a bundle.
pub(crate) fn evaluate_contract(
    contract: &RealizationContract,
    materializations: &[MaterializationVerification],
    profile: LaunchProfile,
) -> Result<(), Box<AtoExecutionError>> {
    // Boxed Err: `AtoExecutionError` is ~528 bytes (clippy::result_large_err);
    // callers that hand it to anyhow unbox it first so the
    // `downcast_ref::<AtoExecutionError>()` contract (utils/error.rs) is kept.
    match evaluate_strict_gate_with_materialization(contract, materializations, profile) {
        Ok(()) => Ok(()),
        Err(errors) => Err(Box::new(to_execution_error(&errors))),
    }
}

/// #500 — strict mode could not obtain a resolved launch graph to verify. This
/// is an integration-level fail-closed block (deliberately **not** one of the
/// per-node [`capsule::realization::StrictGateReasonCode`] values): strict
/// mode must refuse to launch what it cannot even inspect, rather than silently
/// proceeding when the graph is absent.
pub(crate) fn missing_launch_graph_error() -> AtoExecutionError {
    AtoExecutionError::strict_realization_blocked(
        "strict realization gate blocked launch: no resolved launch graph was available to verify",
        serde_json::json!({
            "profile": "strict",
            "reason_code": "launch_graph_missing",
            "explanation": "strict mode requires a resolved launch graph before execution; \
                            none was produced for this launch path",
        }),
    )
}

/// Collapse the per-node gate failures into one typed launch error. The message
/// names only the count and the first node id/reason (all value-free); the full
/// redacted payload rides in `details`.
fn to_execution_error(errors: &[StrictRealizationGateError]) -> AtoExecutionError {
    let message = match errors.first() {
        Some(first) => format!(
            "strict realization gate blocked launch: {} required input(s) could not be verified \
             (first: node '{}', reason {})",
            errors.len(),
            first.node_id,
            reason_code_str(first),
        ),
        None => "strict realization gate blocked launch".to_string(),
    };

    // `errors` are already redacted by construction (the core gate never emits a
    // raw path, env value, secret, or provider command), so serializing them
    // into `details` cannot leak local detail.
    let details = serde_json::json!({
        "profile": "strict",
        "blocked_count": errors.len(),
        "blocked": errors,
    });

    AtoExecutionError::strict_realization_blocked(message, details)
}

/// The serde (`snake_case`) string for a block's reason code, for the message.
fn reason_code_str(error: &StrictRealizationGateError) -> String {
    serde_json::to_value(error.reason_code)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule::realization::{
        RealizationContract, RealizationEvidence, RealizationNodeKind, RealizationNodeStatus,
        RealizationResult, RealizationStatus, UnrealizableReason,
    };

    const HASH_A: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

    fn contract_with(
        node: RealizationNodeStatus,
        result: RealizationResult,
    ) -> RealizationContract {
        RealizationContract {
            resolved_execution_id: "rid-test".to_string(),
            node_statuses: vec![node],
            edge_statuses: vec![],
            result,
        }
    }

    #[test]
    fn normal_profile_never_blocks() {
        let contract = contract_with(
            RealizationNodeStatus {
                node_id: "dep".to_string(),
                node_kind: RealizationNodeKind::DependencyOutput,
                status: RealizationStatus::Unavailable,
                evidence: vec![],
            },
            RealizationResult::Unrealizable {
                reasons: vec![UnrealizableReason::MissingDependencyOutput {
                    node_id: "dep".to_string(),
                }],
            },
        );
        assert!(evaluate_contract(&contract, &[], LaunchProfile::Normal).is_ok());
        assert!(evaluate_contract(&contract, &[], launch_profile(false)).is_ok());
    }

    #[test]
    fn strict_profile_blocks_with_typed_error() {
        let contract = contract_with(
            RealizationNodeStatus {
                node_id: "dep".to_string(),
                node_kind: RealizationNodeKind::DependencyOutput,
                status: RealizationStatus::Unavailable,
                evidence: vec![],
            },
            RealizationResult::Unrealizable {
                reasons: vec![UnrealizableReason::MissingDependencyOutput {
                    node_id: "dep".to_string(),
                }],
            },
        );

        let err = evaluate_contract(&contract, &[], launch_profile(true))
            .expect_err("strict must block the unavailable dependency");
        assert_eq!(err.code, "ATO_ERR_STRICT_REALIZATION_BLOCKED");
        assert_eq!(err.phase, "execution");

        // The redacted per-node payload is carried in `details`, with node id and
        // reason code — but never a raw value.
        let details = err.details.expect("details present");
        assert_eq!(details["profile"], "strict");
        assert_eq!(details["blocked_count"], 1);
        assert_eq!(details["blocked"][0]["node_id"], "dep");
        assert_eq!(details["blocked"][0]["node_kind"], "dependency-output");
        assert_eq!(
            details["blocked"][0]["reason_code"],
            "materialization_missing"
        );
    }

    #[test]
    fn strict_profile_passes_verified_contract() {
        let contract = contract_with(
            RealizationNodeStatus {
                node_id: "src".to_string(),
                node_kind: RealizationNodeKind::Source,
                status: RealizationStatus::Verified,
                evidence: vec![RealizationEvidence::VerifiedArtifact {
                    label: "source-tree".to_string(),
                    hash: HASH_A.to_string(),
                }],
            },
            RealizationResult::Realized,
        );
        assert!(evaluate_contract(&contract, &[], launch_profile(true)).is_ok());
    }

    #[test]
    fn strict_error_payload_has_no_raw_paths_or_secrets() {
        // Even when evidence carries a (validated) hash, the serialized launch
        // error must contain no host path or secret — only redacted summaries.
        let contract = contract_with(
            RealizationNodeStatus {
                node_id: "runtime".to_string(),
                node_kind: RealizationNodeKind::Runtime,
                status: RealizationStatus::HostBound,
                evidence: vec![RealizationEvidence::HostBinding {
                    detail: "host path required for mount 'data' at /home/alice/secret".to_string(),
                }],
            },
            RealizationResult::Realized,
        );
        let err =
            evaluate_contract(&contract, &[], launch_profile(true)).expect_err("host-bound blocks");
        let serialized = serde_json::to_string(&err.details).expect("serialize details");
        assert!(
            !serialized.contains("/home/alice"),
            "no raw host path: {serialized}"
        );
        assert!(
            !serialized.contains("secret"),
            "no evidence detail leak: {serialized}"
        );
    }

    #[test]
    fn missing_launch_graph_fails_closed_with_typed_error() {
        // Blocker-1 invariant: strict mode with no resolved launch graph must
        // block, not silently proceed. The reason code is an integration-level
        // `launch_graph_missing`, kept out of the per-node reason-code set.
        let err = missing_launch_graph_error();
        assert_eq!(err.code, "ATO_ERR_STRICT_REALIZATION_BLOCKED");
        assert_eq!(err.phase, "execution");
        let details = err.details.expect("details present");
        assert_eq!(details["profile"], "strict");
        assert_eq!(details["reason_code"], "launch_graph_missing");
    }

    #[test]
    fn launch_path_consumes_materialization_overlay() {
        // The #498 contract alone classifies the dependency output as
        // materializable (declared hash present) and would pass strict. The #499
        // verifier verdict — supplied through the same launch entry point —
        // catches the materialized mismatch and blocks the launch.
        let contract = contract_with(
            RealizationNodeStatus {
                node_id: "dep".to_string(),
                node_kind: RealizationNodeKind::DependencyOutput,
                status: RealizationStatus::Materializable,
                evidence: vec![RealizationEvidence::DeclaredHash {
                    label: "dependency-output".to_string(),
                    hash: HASH_A.to_string(),
                }],
            },
            RealizationResult::Realized,
        );
        let hash_b = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
        let materializations = vec![capsule::realization::MaterializationVerification {
            node_id: "dep".to_string(),
            node_kind: RealizationNodeKind::DependencyOutput,
            result: capsule::realization::MaterializationVerificationResult::Mismatch {
                expected: HASH_A.to_string(),
                actual: hash_b.to_string(),
            },
            evidence: vec![],
        }];

        // #498 alone: passes. With the #499 overlay: blocked on identity mismatch.
        assert!(evaluate_contract(&contract, &[], launch_profile(true)).is_ok());
        let err = evaluate_contract(&contract, &materializations, launch_profile(true))
            .expect_err("the #499 mismatch overlay must block");
        let details = err.details.expect("details present");
        assert_eq!(details["blocked"][0]["reason_code"], "identity_mismatch");
    }

    #[test]
    fn launch_path_trusts_verified_overlay_on_declared_only_node() {
        // A declared-only (#498-Materializable) node with an authoritative #499
        // Verified verdict must pass through the launch adapter — the verifier
        // already proved the materialized identity matched.
        let contract = contract_with(
            RealizationNodeStatus {
                node_id: "dep".to_string(),
                node_kind: RealizationNodeKind::DependencyOutput,
                status: RealizationStatus::Materializable,
                evidence: vec![RealizationEvidence::DeclaredHash {
                    label: "dependency-output".to_string(),
                    hash: HASH_A.to_string(),
                }],
            },
            RealizationResult::Realized,
        );
        let materializations = vec![capsule::realization::MaterializationVerification {
            node_id: "dep".to_string(),
            node_kind: RealizationNodeKind::DependencyOutput,
            result: capsule::realization::MaterializationVerificationResult::Verified,
            evidence: vec![],
        }];
        assert!(
            evaluate_contract(&contract, &materializations, launch_profile(true)).is_ok(),
            "a #499 Verified overlay must not be treated as false-verified",
        );
    }
}
