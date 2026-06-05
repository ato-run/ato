//! Tests for strict OCI enforcement (#501).

use super::*;
use capsule_core::execution_identity::OciEnforcementStatus;
use capsule_core::execution_plan::model::{OciPolicyEnvelope, OciPolicyMode};

/// A stand-in graph-derived resolved execution id (value-free) for tests.
const RID: Option<&str> = Some("graph-resolved-exec-id-test");
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
fn unbound_graph_execution_id_is_not_fabricated() {
    // With no graph-derived resolved execution id, the gate must NOT substitute a
    // provider fingerprint. It still blocks correctly, and the value-free
    // placeholder never surfaces in the redacted error payload.
    let mut facts = clean_facts();
    facts.image_digest = None;
    let err = enforce_strict_oci(
        &facts,
        &OciProviderEnforcement::podman(false),
        LaunchProfile::Strict,
        None,
    )
    .expect_err("unpinned image blocks");
    assert_eq!(first_reason(&err), "materialization_missing");
    let serialized = serde_json::to_string(&err.details).expect("serialize");
    assert!(
        !serialized.contains("graph-execution-id-unbound"),
        "placeholder id must not leak into the error payload: {serialized}"
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

// ── multi-service / orchestration gate ──

fn service(label: &str, facts: OciStrictFacts) -> OciServiceStrict {
    let enforcement = OciProviderEnforcement::podman(facts.network_policy_required);
    OciServiceStrict {
        service_label: label.to_string(),
        facts,
        enforcement,
    }
}

fn strict_services(services: &[OciServiceStrict]) -> Result<(), AtoExecutionError> {
    enforce_strict_oci_services(services, LaunchProfile::Strict, None)
}

#[test]
fn strict_multi_service_passes_when_all_services_clean() {
    let services = vec![service("web", clean_facts()), service("db", clean_facts())];
    assert!(strict_services(&services).is_ok());
}

#[test]
fn strict_multi_service_blocks_and_names_the_offending_service() {
    // Two clean services + one with a host-bound mount → blocked, and the error
    // node id carries the offending service label.
    let mut bad = clean_facts();
    bad.host_bound_mount_targets = vec!["/data".to_string()];
    let services = vec![
        service("web", clean_facts()),
        service("worker", bad),
        service("db", clean_facts()),
    ];
    let err = strict_services(&services).expect_err("host-bound service must block");
    let details = err.details.clone().expect("details");
    let blocked = details["blocked"].as_array().expect("array");
    // Exactly the offending service is blocked, identified by its node id.
    assert_eq!(blocked.len(), 1);
    let node_id = blocked[0]["node_id"].as_str().unwrap();
    assert!(
        node_id.contains("worker"),
        "node id names the service: {node_id}"
    );
    assert_eq!(blocked[0]["reason_code"], "host_bound_disallowed");
}

#[test]
fn strict_multi_service_blocks_required_egress_unsupported_by_podman() {
    let mut net = clean_facts();
    net.network_policy_required = true;
    let services = vec![service("web", net)];
    let err = strict_services(&services).expect_err("egress unsupported must block");
    let details = err.details.clone().expect("details");
    assert_eq!(details["blocked"][0]["reason_code"], "policy_downgraded");
}

#[test]
fn strict_multi_service_blocks_unpinned_required_image() {
    let mut unpinned = clean_facts();
    unpinned.image_digest = None;
    let services = vec![service("web", clean_facts()), service("api", unpinned)];
    let err = strict_services(&services).expect_err("unpinned image must block");
    let details = err.details.clone().expect("details");
    let node_id = details["blocked"][0]["node_id"].as_str().unwrap();
    assert!(node_id.contains("api"));
    assert_eq!(
        details["blocked"][0]["reason_code"],
        "materialization_missing"
    );
}

#[test]
fn normal_multi_service_does_not_block() {
    let mut bad = clean_facts();
    bad.network_policy_required = true;
    bad.host_bound_mount_targets = vec!["/data".to_string()];
    bad.image_digest = None;
    let services = vec![service("web", bad)];
    assert!(
        enforce_strict_oci_services(&services, LaunchProfile::Normal, None).is_ok(),
        "normal mode must never block a multi-service launch"
    );
}

#[test]
fn strict_multi_service_error_leaks_no_host_path() {
    let mut bad = clean_facts();
    bad.host_bound_mount_targets = vec!["/home/alice/project/secret".to_string()];
    let services = vec![service("web", bad)];
    let err = strict_services(&services).expect_err("blocks");
    let serialized = serde_json::to_string(&err.details).expect("serialize");
    assert!(
        !serialized.contains("/home/alice"),
        "no raw host path: {serialized}"
    );
}
