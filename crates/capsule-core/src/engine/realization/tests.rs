//! Unit tests for the Capsule Realization Contract (#498-A).
//!
//! These lock the typed classification contract: the right status for each
//! node shape, fail-closed only on `Unavailable`, the #473 runtime-tool guard,
//! and the #501 identity boundary (container id / rendered command are
//! evidence, never identity).

use std::collections::BTreeMap;

use super::*;
use crate::engine::execution_graph::{
    ExecutionGraphBuilder, GraphLaunchInput, GraphSourceInput, GraphTargetInput,
    LaunchGraphBundleInput,
};

const EXEC_ID: &str = "resolved-exec-id-deadbeef";

fn request(nodes: Vec<RealizationNode>) -> RealizationRequest {
    RealizationRequest {
        resolved_execution_id: EXEC_ID.to_string(),
        nodes,
        edges: Vec::new(),
    }
}

fn status_of<'a>(contract: &'a RealizationContract, node_id: &str) -> &'a RealizationNodeStatus {
    contract
        .node_statuses
        .iter()
        .find(|n| n.node_id == node_id)
        .unwrap_or_else(|| panic!("node {node_id} not classified"))
}

#[test]
fn realization_contract_reports_realized_for_minimal_managed_graph() {
    let contract = classify(request(vec![
        RealizationNode::required(
            "source",
            RealizationNodeFacts::Source {
                declared_tree_hash: Some("sha256:src".into()),
                materialized_tree_hash: None,
            },
        ),
        RealizationNode::required(
            "runtime",
            RealizationNodeFacts::Runtime {
                declared_identity: Some("node@20".into()),
                materialized_binary_hash: None,
            },
        ),
        RealizationNode::required(
            "runtime-tool:pnpm",
            RealizationNodeFacts::RuntimeTool {
                binary_sha256: Some("sha256:pnpm".into()),
                materialized_match: true,
            },
        ),
        RealizationNode::required(
            "entrypoint",
            RealizationNodeFacts::Entrypoint {
                argv_declared: true,
                cwd_declared: true,
            },
        ),
        RealizationNode::required(
            "env-closure",
            RealizationNodeFacts::EnvClosure {
                undeclared_required: Vec::new(),
            },
        ),
    ]));

    assert!(
        contract.result.is_realized(),
        "minimal managed graph realizes"
    );
    assert!(!contract.has_status(RealizationStatus::Unavailable));
    assert_eq!(
        status_of(&contract, "runtime-tool:pnpm").status,
        RealizationStatus::Verified,
    );
    assert_eq!(
        status_of(&contract, "source").status,
        RealizationStatus::Materializable,
    );
}

#[test]
fn realization_contract_marks_missing_immutable_node_unrealizable() {
    let contract = classify(request(vec![RealizationNode::required(
        "source",
        RealizationNodeFacts::Source {
            declared_tree_hash: None,
            materialized_tree_hash: None,
        },
    )]));

    assert_eq!(
        status_of(&contract, "source").status,
        RealizationStatus::Unavailable,
    );
    match contract.result {
        RealizationResult::Unrealizable { reasons } => {
            assert_eq!(
                reasons,
                vec![UnrealizableReason::MissingImmutableInput {
                    node_id: "source".into(),
                    node_kind: RealizationNodeKind::Source,
                }]
            );
        }
        RealizationResult::Realized => panic!("missing immutable source must be unrealizable"),
    }
}

#[test]
fn realization_contract_marks_runtime_tool_without_binary_sha256_unavailable() {
    let contract = classify(request(vec![RealizationNode::required(
        "runtime-tool:pnpm",
        RealizationNodeFacts::RuntimeTool {
            binary_sha256: None,
            materialized_match: false,
        },
    )]));

    let status = status_of(&contract, "runtime-tool:pnpm");
    assert_eq!(
        status.status,
        RealizationStatus::Unavailable,
        "missing binary_sha256 must never be Verified (#473)",
    );
    assert_ne!(status.status, RealizationStatus::Verified);
    match contract.result {
        RealizationResult::Unrealizable { reasons } => assert_eq!(
            reasons,
            vec![UnrealizableReason::RuntimeToolBinaryHashUnavailable {
                node_id: "runtime-tool:pnpm".into(),
            }]
        ),
        RealizationResult::Realized => panic!("must be unrealizable"),
    }
}

#[test]
fn realization_contract_does_not_verify_hash_mismatch() {
    let contract = classify(request(vec![RealizationNode::required(
        "source",
        RealizationNodeFacts::Source {
            declared_tree_hash: Some("sha256:declared".into()),
            materialized_tree_hash: Some("sha256:actual-different".into()),
        },
    )]));

    let status = status_of(&contract, "source");
    assert_eq!(
        status.status,
        RealizationStatus::Unavailable,
        "a declared/materialized hash mismatch must never Verify",
    );
    assert_ne!(status.status, RealizationStatus::Verified);
    assert!(matches!(
        status.evidence.first(),
        Some(RealizationEvidence::HashMismatch { .. })
    ));
    match contract.result {
        RealizationResult::Unrealizable { reasons } => assert_eq!(
            reasons,
            vec![UnrealizableReason::MismatchedImmutableInput {
                node_id: "source".into(),
                node_kind: RealizationNodeKind::Source,
                expected: "sha256:declared".into(),
                actual: "sha256:actual-different".into(),
            }]
        ),
        RealizationResult::Realized => panic!("hash mismatch must be unrealizable"),
    }
}

#[test]
fn realization_contract_does_not_verify_materialized_hash_without_declared_identity() {
    // Required: a materialized hash with no declared identity has nothing to be
    // checked against, so it must be Unavailable — never Verified.
    let contract = classify(request(vec![RealizationNode::required(
        "runtime",
        RealizationNodeFacts::Runtime {
            declared_identity: None,
            materialized_binary_hash: Some("sha256:actual".into()),
        },
    )]));

    let status = status_of(&contract, "runtime");
    assert_ne!(
        status.status,
        RealizationStatus::Verified,
        "no declared identity ⇒ nothing to verify against",
    );
    assert_eq!(status.status, RealizationStatus::Unavailable);
    match contract.result {
        RealizationResult::Unrealizable { reasons } => assert_eq!(
            reasons,
            vec![UnrealizableReason::MissingImmutableInput {
                node_id: "runtime".into(),
                node_kind: RealizationNodeKind::Runtime,
            }]
        ),
        RealizationResult::Realized => panic!("must be unrealizable"),
    }

    // Optional variant: Unknown, not Verified, and not blocking.
    let optional = classify(RealizationRequest {
        resolved_execution_id: EXEC_ID.to_string(),
        nodes: vec![RealizationNode::optional(
            "runtime",
            RealizationNodeFacts::Runtime {
                declared_identity: None,
                materialized_binary_hash: Some("sha256:actual".into()),
            },
        )],
        edges: Vec::new(),
    });
    assert_eq!(
        status_of(&optional, "runtime").status,
        RealizationStatus::Unknown,
    );
    assert!(optional.result.is_realized());
}

#[test]
fn realization_contract_marks_state_binding_as_state_bound() {
    let contract = classify(request(vec![RealizationNode::required(
        "state-binding:pgdata",
        RealizationNodeFacts::StateBinding {
            binding_present: true,
            has_creation_policy: false,
            reference: Some("volume://pgdata".into()),
        },
    )]));

    assert_eq!(
        status_of(&contract, "state-binding:pgdata").status,
        RealizationStatus::StateBound,
    );
    // StateBound is visible but not fail-closed.
    assert!(contract.result.is_realized());
}

#[test]
fn realization_contract_marks_host_path_as_host_bound() {
    let contract = classify(request(vec![RealizationNode::required(
        "filesystem-view",
        RealizationNodeFacts::FilesystemView {
            mounts: vec![MountFact {
                role: "config".into(),
                host_path_required: true,
                projectable: false,
            }],
        },
    )]));

    let status = status_of(&contract, "filesystem-view");
    assert_eq!(status.status, RealizationStatus::HostBound);
    assert!(matches!(
        status.evidence.first(),
        Some(RealizationEvidence::HostBinding { .. })
    ));
    assert!(contract.result.is_realized());
}

#[test]
fn realization_contract_marks_unenforceable_network_policy_as_policy_downgraded() {
    let contract = classify(request(vec![RealizationNode::required(
        "network-policy",
        RealizationNodeFacts::NetworkPolicy {
            required: true,
            provider_can_enforce: false,
            policy_ref: Some("sha256:netpol".into()),
        },
    )]));

    let status = status_of(&contract, "network-policy");
    assert_eq!(status.status, RealizationStatus::PolicyDowngraded);
    assert!(matches!(
        status.evidence.first(),
        Some(RealizationEvidence::PolicyEnforcementGap { .. })
    ));
    // Downgrade is surfaced, not reported as clean; strict fail-closed is #500.
    assert!(contract.result.is_realized());
}

#[test]
fn realization_contract_serializes_unrealizable_reasons() {
    let contract = classify(request(vec![
        RealizationNode::required(
            "runtime-tool:pnpm",
            RealizationNodeFacts::RuntimeTool {
                binary_sha256: None,
                materialized_match: false,
            },
        ),
        RealizationNode::required(
            "dependency-output:db",
            RealizationNodeFacts::DependencyOutput {
                dependency_output_hash: None,
            },
        ),
    ]));

    let json = serde_json::to_string(&contract).expect("serialize");
    assert!(json.contains("runtime-tool-binary-hash-unavailable"));
    assert!(json.contains("missing-dependency-output"));

    let round_trip: RealizationContract = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(round_trip, contract);
}

// ---------------------------------------------------------------------------
// Bundle adapter + identity boundary (#501)
// ---------------------------------------------------------------------------

fn minimal_bundle() -> crate::engine::execution_graph::LaunchGraphBundle {
    ExecutionGraphBuilder::build_launch_bundle(LaunchGraphBundleInput {
        source: Some(GraphSourceInput {
            identifier: "capsule://app".into(),
        }),
        targets: vec![GraphTargetInput {
            identifier: "entry".into(),
            runtime: "node-20".into(),
        }],
        launch: Some(GraphLaunchInput {
            command: "node".into(),
            args: vec!["server.js".into()],
            logical_cwd: "/app".into(),
            declared_port: Some(3000),
            effective_port: None,
            readiness_port: None,
            readiness_path: "/".into(),
            build_input_digest: None,
            lock_digest: None,
            toolchain_fingerprint: "tc-1".into(),
        }),
        ..Default::default()
    })
}

fn oci_environment() -> RealizationEnvironment {
    RealizationEnvironment {
        declared_source_hash: Some("sha256:src".into()),
        runtimes: BTreeMap::from([(
            "node-20".to_string(),
            RuntimeEvidence {
                declared_identity: Some("node@20".into()),
                materialized_binary_hash: None,
            },
        )]),
        provider_projection: Some(ProviderProjectionEvidence {
            provider: "oci:podman".into(),
            renderer: "podman-create".into(),
            raw_argv: [
                "create",
                "--rm",
                "--name",
                "app-prod",
                "app@sha256:img",
                "server.js",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            container_id: Some("container-abc123def".into()),
        }),
        ..Default::default()
    }
}

#[test]
fn realization_contract_never_uses_container_id_as_identity() {
    let bundle = minimal_bundle();
    let env = oci_environment();
    let contract = realization_from_launch_bundle(&bundle, &env);

    // Identity is the graph-derived resolved execution id, full stop.
    assert_eq!(
        contract.resolved_execution_id,
        bundle.derived.execution_ids.resolved_execution_id,
    );
    assert_ne!(contract.resolved_execution_id, "container-abc123def");

    // The container id must not leak anywhere in the serialized contract.
    let json = serde_json::to_string(&contract).expect("serialize");
    assert!(
        !json.contains("container-abc123def"),
        "container id must never appear in the realization contract",
    );

    // No node id is the container id.
    assert!(
        contract
            .node_statuses
            .iter()
            .all(|n| !n.node_id.contains("container-abc123def")),
    );
}

#[test]
fn oci_projection_command_is_derived_evidence_not_identity() {
    let bundle = minimal_bundle();
    let env = oci_environment();
    let contract = realization_from_launch_bundle(&bundle, &env);

    let projection = contract
        .node_statuses
        .iter()
        .find(|n| n.node_kind == RealizationNodeKind::ProviderProjection)
        .expect("provider projection node present");

    // Node id is derived from the provider, not from the rendered command.
    assert_eq!(projection.node_id, "provider-projection:oci:podman");
    assert_eq!(projection.status, RealizationStatus::Materializable);

    // The command lives in evidence only, and only as redacted shape: flags
    // survive, positional/value tokens (the image ref, container name) do not.
    let Some(RealizationEvidence::DerivedProjectionCommand { command, .. }) =
        projection.evidence.first()
    else {
        panic!("expected a DerivedProjectionCommand evidence item");
    };
    assert!(command.redacted);
    assert_eq!(command.renderer, "podman-create");
    assert!(command.argv_shape.contains(&"--rm".to_string()));
    assert!(command.argv_shape.contains(&"--name".to_string()));
    assert!(
        !command
            .argv_shape
            .iter()
            .any(|t| t.contains("app@sha256:img")),
        "image ref (a value) must be redacted out of argv_shape",
    );
    assert!(
        !command.argv_shape.iter().any(|t| t.contains("app-prod")),
        "container name (a value) must be redacted out of argv_shape",
    );

    // The raw image ref must not survive serialization anywhere.
    let json = serde_json::to_string(&contract).expect("serialize");
    assert!(!json.contains("app@sha256:img"));
    assert_ne!(contract.resolved_execution_id, "app@sha256:img");
}

#[test]
fn materialization_request_only_verifies_materialized_artifacts() {
    let bundle = minimal_bundle();

    // Default (conservative) evidence: nothing is materialized yet (#501), so the
    // verifier request is empty — strict mode does not over-block declared-only
    // re-derivable inputs.
    let empty =
        materialization_request_from_launch_bundle(&bundle, &RealizationEnvironment::default());
    assert!(
        empty.nodes.is_empty(),
        "no materialized artifacts ⇒ empty verifier request, got {:?}",
        empty.nodes,
    );

    let source_hash = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

    // A declared-only source (no materialized hash) is still omitted: it is
    // #498-Materializable, not a #499 verifier concern, until #501 grounds it.
    let declared_only = RealizationEnvironment {
        declared_source_hash: Some(source_hash.into()),
        ..Default::default()
    };
    assert!(
        materialization_request_from_launch_bundle(&bundle, &declared_only)
            .nodes
            .is_empty(),
        "declared-only source must not be handed to the verifier",
    );

    // Once a materialized source hash exists, the source is verified; a runtime
    // tool is always emitted so its missing binary_sha256 surfaces (#473).
    let materialized = RealizationEnvironment {
        declared_source_hash: Some(source_hash.into()),
        materialized_source_hash: Some(source_hash.into()),
        runtime_tools: BTreeMap::from([(
            "pnpm".to_string(),
            RuntimeToolEvidence {
                binary_sha256: None,
                materialized_match: false,
            },
        )]),
        ..Default::default()
    };
    let results = verify_materialization(materialization_request_from_launch_bundle(
        &bundle,
        &materialized,
    ));
    let source = results
        .iter()
        .find(|r| r.node_kind == RealizationNodeKind::Source)
        .expect("source verified");
    assert!(source.result.is_verified());
    let tool = results
        .iter()
        .find(|r| r.node_id == "runtime-tool:pnpm")
        .expect("runtime tool present");
    assert_eq!(
        tool.result,
        MaterializationVerificationResult::Unavailable {
            reason: MaterializationUnavailableReason::RuntimeToolBinaryHashUnpopulated,
        },
    );
}

#[test]
fn realization_contract_does_not_serialize_raw_projection_command_or_secrets() {
    let bundle = minimal_bundle();
    let env = RealizationEnvironment {
        declared_source_hash: Some("sha256:src".into()),
        provider_projection: Some(ProviderProjectionEvidence {
            provider: "oci:podman".into(),
            renderer: "podman-create".into(),
            // A rendered argv embedding env values, a token, and a DB URL —
            // exactly the shape that must never reach a serde-ready contract.
            raw_argv: [
                "create",
                "--rm",
                "--env",
                "OPENAI_API_KEY=sk-test-secret",
                "-e",
                "DATABASE_URL=postgres://user:password@host/db",
                "--env=TOKEN=secret-token",
                "app@sha256:img",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            container_id: None,
        }),
        ..Default::default()
    };

    let contract = realization_from_launch_bundle(&bundle, &env);
    let json = serde_json::to_string(&contract).expect("serialize");

    for leaked in [
        "sk-test-secret",
        "postgres://user:password@host/db",
        "password",
        "secret-token",
        "OPENAI_API_KEY=sk-test-secret",
        "TOKEN=secret-token",
        "app@sha256:img",
    ] {
        assert!(
            !json.contains(leaked),
            "raw value `{leaked}` leaked into the serialized contract",
        );
    }

    // The redaction placeholder is present — the evidence is preserved in shape.
    assert!(json.contains(RedactedProjectionCommand::PLACEHOLDER));
}
