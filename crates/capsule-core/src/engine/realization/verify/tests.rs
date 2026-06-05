//! Unit tests for the materialization verifier (#499-A).

use std::path::Path;

use super::*;
use crate::engine::realization::{RealizationNodeKind, RealizationStatus, UnrealizableReason};

fn node(
    source: MaterializedNodeSource,
    expected: Option<&str>,
    actual: Option<&str>,
) -> MaterializedNodeInput {
    MaterializedNodeInput {
        node_id: format!("{}-node", source.role_label()),
        node_kind: RealizationNodeKind::Runtime,
        expected_hash: expected.map(String::from),
        actual_hash: actual.map(String::from),
        required: true,
        source,
        materialized_path: None,
    }
}

fn verify_one(node: MaterializedNodeInput) -> MaterializationVerification {
    let mut out = verify_materialization(MaterializationVerificationRequest { nodes: vec![node] });
    out.pop().expect("one verification")
}

#[test]
fn materialization_verifier_marks_matching_hash_verified() {
    let v = verify_one(node(
        MaterializedNodeSource::RuntimeBinary,
        Some("sha256:abc"),
        Some("sha256:abc"),
    ));
    assert_eq!(v.result, MaterializationVerificationResult::Verified);
    assert!(
        v.evidence
            .contains(&MaterializationVerificationEvidence::HashCompared {
                algorithm: "sha256".into(),
            })
    );
}

#[test]
fn materialization_verifier_marks_hash_mismatch() {
    let v = verify_one(node(
        MaterializedNodeSource::DependencyOutput,
        Some("sha256:expected"),
        Some("sha256:actual"),
    ));
    assert_eq!(
        v.result,
        MaterializationVerificationResult::Mismatch {
            expected: "sha256:expected".into(),
            actual: "sha256:actual".into(),
        }
    );
}

#[test]
fn materialization_verifier_marks_missing_expected_hash_unavailable() {
    let v = verify_one(node(
        MaterializedNodeSource::DependencyOutput,
        None,
        Some("sha256:actual"),
    ));
    assert_eq!(
        v.result,
        MaterializationVerificationResult::Unavailable {
            reason: MaterializationUnavailableReason::MissingExpectedHash,
        }
    );
    assert!(!v.result.is_verified());
}

#[test]
fn materialization_verifier_marks_missing_actual_hash_unavailable() {
    let v = verify_one(node(
        MaterializedNodeSource::BuildArtifact,
        Some("sha256:expected"),
        None,
    ));
    assert_eq!(
        v.result,
        MaterializationVerificationResult::Unavailable {
            reason: MaterializationUnavailableReason::MissingMaterializedObject,
        }
    );
}

#[test]
fn materialization_verifier_marks_runtime_tool_without_binary_sha256_unavailable() {
    // #469/#473: a runtime tool with no binary_sha256 is Unavailable with its
    // own typed reason — never Verified, even though there is no actual hash.
    let v = verify_one(node(MaterializedNodeSource::RuntimeTool, None, None));
    assert_eq!(
        v.result,
        MaterializationVerificationResult::Unavailable {
            reason: MaterializationUnavailableReason::RuntimeToolBinaryHashUnpopulated,
        }
    );
    assert!(!v.result.is_verified());
}

#[test]
fn materialization_verifier_does_not_emit_unredacted_host_paths() {
    let mut n = node(
        MaterializedNodeSource::FilesystemView,
        Some("sha256:x"),
        Some("sha256:x"),
    );
    n.materialized_path = Some("/Users/alice/secret-proj/.env".into());
    let v = verify_one(n);

    // The path is reduced to a role label; no raw path component survives.
    assert!(
        v.evidence
            .contains(&MaterializationVerificationEvidence::RedactedPath {
                label: "filesystem-view".into(),
            })
    );
    let json = serde_json::to_string(&v).expect("serialize");
    assert!(!json.contains("/Users/alice"));
    assert!(!json.contains("secret-proj"));
    assert!(!json.contains(".env"));
}

#[test]
fn materialization_verifier_does_not_emit_secret_values() {
    // A path that embeds a secret-looking token must not survive into output.
    let mut n = node(
        MaterializedNodeSource::SourceTree,
        Some("sha256:declared"),
        Some("sha256:materialized-different"),
    );
    n.materialized_path = Some("/home/u/app/OPENAI_API_KEY=sk-test-secret".into());
    let v = verify_one(n);

    let json = serde_json::to_string(&v).expect("serialize");
    for leaked in ["sk-test-secret", "OPENAI_API_KEY", "/home/u/app"] {
        assert!(
            !json.contains(leaked),
            "`{leaked}` leaked into verifier output"
        );
    }
    // The content hashes (safe) are still present in the typed mismatch.
    assert!(json.contains("sha256:declared"));
    assert!(json.contains("sha256:materialized-different"));
}

#[test]
fn materialization_verifier_maps_verified_to_realization_verified() {
    let result = MaterializationVerificationResult::Verified;
    assert_eq!(
        materialization_result_to_realization_status(&result),
        RealizationStatus::Verified,
    );
    assert_eq!(
        materialization_result_to_unrealizable_reason("n", RealizationNodeKind::Runtime, &result,),
        None,
    );
}

#[test]
fn materialization_verifier_maps_mismatch_to_realization_unavailable() {
    let result = MaterializationVerificationResult::Mismatch {
        expected: "sha256:a".into(),
        actual: "sha256:b".into(),
    };
    assert_eq!(
        materialization_result_to_realization_status(&result),
        RealizationStatus::Unavailable,
    );
    assert_eq!(
        materialization_result_to_unrealizable_reason(
            "runtime",
            RealizationNodeKind::Runtime,
            &result,
        ),
        Some(UnrealizableReason::MismatchedImmutableInput {
            node_id: "runtime".into(),
            node_kind: RealizationNodeKind::Runtime,
            expected: "sha256:a".into(),
            actual: "sha256:b".into(),
        }),
    );
}

#[test]
fn materialization_verifier_maps_runtime_tool_unavailable_to_typed_reason() {
    let result = MaterializationVerificationResult::Unavailable {
        reason: MaterializationUnavailableReason::RuntimeToolBinaryHashUnpopulated,
    };
    assert_eq!(
        materialization_result_to_unrealizable_reason(
            "runtime-tool:pnpm",
            RealizationNodeKind::RuntimeTool,
            &result,
        ),
        Some(UnrealizableReason::RuntimeToolBinaryHashUnavailable {
            node_id: "runtime-tool:pnpm".into(),
        }),
    );
}

#[test]
fn materialization_verifier_serializes_typed_mismatch() {
    let v = verify_one(node(
        MaterializedNodeSource::RuntimeBinary,
        Some("sha256:expected"),
        Some("sha256:actual"),
    ));
    let json = serde_json::to_string(&v).expect("serialize");
    assert!(json.contains("\"result\":\"mismatch\""));
    assert!(json.contains("sha256:expected"));
    assert!(json.contains("sha256:actual"));

    let round_trip: MaterializationVerification = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(round_trip, v);
}

// --- adapter seam ---------------------------------------------------------

struct FakeHashProvider {
    hash: Option<String>,
}

impl MaterializedHashProvider for FakeHashProvider {
    fn hash_file(&self, _path: &Path) -> Result<String, MaterializationHashError> {
        self.hash.clone().ok_or(MaterializationHashError::NotFound)
    }
}

#[test]
fn materialization_verifier_provider_fills_actual_hash() {
    let provider = FakeHashProvider {
        hash: Some("sha256:expected".into()),
    };
    let mut n = node(
        MaterializedNodeSource::BuildArtifact,
        Some("sha256:expected"),
        None,
    );
    n.materialized_path = Some("/tmp/whatever".into());

    let out = verify_materialization_with_provider(
        &provider,
        MaterializationVerificationRequest { nodes: vec![n] },
    );
    assert_eq!(out[0].result, MaterializationVerificationResult::Verified);
    // Path redacted even on the provider path.
    let json = serde_json::to_string(&out[0]).expect("serialize");
    assert!(!json.contains("/tmp/whatever"));
}

#[test]
fn materialization_verifier_provider_error_is_hash_computation_unavailable() {
    let provider = FakeHashProvider { hash: None };
    let mut n = node(
        MaterializedNodeSource::BuildArtifact,
        Some("sha256:expected"),
        None,
    );
    n.materialized_path = Some("/secret/path/token".into());

    let out = verify_materialization_with_provider(
        &provider,
        MaterializationVerificationRequest { nodes: vec![n] },
    );
    assert_eq!(
        out[0].result,
        MaterializationVerificationResult::Unavailable {
            reason: MaterializationUnavailableReason::HashComputationUnavailable,
        }
    );
    let json = serde_json::to_string(&out[0]).expect("serialize");
    assert!(!json.contains("/secret/path"));
}
