//! Tests for strict OCI enforcement (#501).

use super::*;
use capsule_core::execution_identity::OciEnforcementStatus;
use capsule_core::execution_plan::model::{OciPolicyEnvelope, OciPolicyMode};

const RID: &str = "sha256:oci-test";
const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// Facts for a fully-pinned image with no policy/mount concerns (the pass case).
fn clean_facts() -> OciStrictFacts {
    OciStrictFacts {
        network_policy_required: false,
        capability_policy_required: false,
        image_digest: Some(DIGEST.to_string()),
        host_bound_mount_targets: Vec::new(),
    }
}

fn strict(
    facts: &OciStrictFacts,
    enforcement: &OciProviderEnforcement,
) -> Result<(), AtoExecutionError> {
    enforce_strict_oci(facts, enforcement, LaunchProfile::Strict, RID)
}

fn first_reason(err: &AtoExecutionError) -> String {
    let details = err.details.clone().expect("details present");
    details["blocked"][0]["reason_code"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

#[test]
fn strict_oci_blocks_required_network_policy_when_provider_cannot_enforce() {
    let mut facts = clean_facts();
    facts.network_policy_required = true;
    // The podman model: a required network (egress) policy is unsupported.
    let enforcement = OciProviderEnforcement::podman(true);
    assert_eq!(enforcement.network, OciEnforcementStatus::Unsupported);

    let err = strict(&facts, &enforcement).expect_err("required+unenforceable network must block");
    assert_eq!(err.code, "ATO_ERR_STRICT_REALIZATION_BLOCKED");
    assert_eq!(first_reason(&err), "policy_downgraded");
}

#[test]
fn strict_oci_blocks_required_capability_policy_when_provider_cannot_enforce() {
    let mut facts = clean_facts();
    facts.capability_policy_required = true;
    // A provider that cannot enforce capability policy (provider-agnostic gate).
    let enforcement = OciProviderEnforcement {
        network: OciEnforcementStatus::Enforced,
        capability: OciEnforcementStatus::Unsupported,
    };
    let err =
        strict(&facts, &enforcement).expect_err("required+unenforceable capability must block");
    assert_eq!(first_reason(&err), "policy_downgraded");
}

#[test]
fn strict_oci_blocks_unpinned_required_image_digest() {
    let mut facts = clean_facts();
    facts.image_digest = None; // tag-only / unpinned
    let err = strict(&facts, &OciProviderEnforcement::podman(false))
        .expect_err("an unpinned required image must block in strict mode");
    assert_eq!(first_reason(&err), "materialization_missing");
}

#[test]
fn strict_oci_blocks_host_bound_mount_fallback() {
    let mut facts = clean_facts();
    facts.host_bound_mount_targets = vec!["/data".to_string()];
    let err = strict(&facts, &OciProviderEnforcement::podman(false))
        .expect_err("a host-bound mount must block in strict mode");
    assert_eq!(first_reason(&err), "host_bound_disallowed");
}

#[test]
fn strict_oci_passes_pinned_enforced_launch() {
    // Fully pinned image, no declared policy, no host-bound mounts → passes even
    // in strict mode (fail-closed, not refuse-everything).
    assert!(strict(&clean_facts(), &OciProviderEnforcement::podman(false)).is_ok());
}

#[test]
fn normal_oci_policy_downgrade_does_not_block() {
    // Same unenforceable facts as the blocking cases, but under the default
    // (Normal) profile — must NOT block (no behavior regression for OCI).
    let mut facts = clean_facts();
    facts.network_policy_required = true;
    facts.host_bound_mount_targets = vec!["/data".to_string()];
    facts.image_digest = None;
    assert!(
        enforce_strict_oci(
            &facts,
            &OciProviderEnforcement::podman(true),
            LaunchProfile::Normal,
            RID,
        )
        .is_ok(),
        "normal mode must never newly block an OCI launch"
    );
}

#[test]
fn strict_oci_error_payload_leaks_no_secret_or_host_path() {
    // Host-bound mount target is a path-ish role label; the strict-gate payload
    // must not echo a raw host path or any value.
    let mut facts = clean_facts();
    facts.host_bound_mount_targets = vec!["/home/alice/secret-data".to_string()];
    let err = strict(&facts, &OciProviderEnforcement::podman(false)).expect_err("blocks");
    let serialized = serde_json::to_string(&err.details).expect("serialize");
    assert!(
        !serialized.contains("/home/alice"),
        "no raw host path in strict error: {serialized}"
    );
}

// ── facts derivation from the resolved envelope + projection plan ──

fn envelope(egress: Vec<String>, mode: OciPolicyMode) -> OciPolicyEnvelope {
    OciPolicyEnvelope {
        declared_image_ref: "docker.io/library/nginx:1.27".to_string(),
        resolved_image: None,
        port_exposure: None,
        egress_allow: egress,
        policy_mode: mode,
    }
}

#[test]
fn facts_derive_network_required_from_egress_allowlist() {
    use crate::application::provider_projection::oci::OciProjectionPlan;
    use capsule_core::runtime::oci::OciContainerRequest;
    use std::collections::HashMap;

    let request = OciContainerRequest {
        name: "ato-x".to_string(),
        image: format!("repo/app@{DIGEST}"),
        cmd: vec![],
        env: HashMap::new(),
        working_dir: None,
        labels: HashMap::new(),
        mounts: vec![],
        ports: vec![],
        network: None,
        aliases: vec![],
        platform: None,
        extra_hosts: vec![],
        user: None,
    };
    let plan = OciProjectionPlan::from_container_request(&request);

    // No egress → network not required; image pinned.
    let facts = OciStrictFacts::from_launch(&envelope(vec![], OciPolicyMode::Off), &plan);
    assert!(!facts.network_policy_required);
    assert_eq!(facts.image_digest.as_deref(), Some(DIGEST));
    assert!(facts.host_bound_mount_targets.is_empty());

    // Egress declared → network required (regardless of policy_mode).
    let facts = OciStrictFacts::from_launch(
        &envelope(vec!["api.example.com".to_string()], OciPolicyMode::Loose),
        &plan,
    );
    assert!(facts.network_policy_required);
}
