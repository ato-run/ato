//! Strict OCI enforcement (#501): reject an OCI launch *before container
//! creation* when a required policy facet cannot be enforced by the selected
//! provider, or when the image identity is unreproducible.
//!
//! This is the operational bridge from the #516 provider projection boundary to
//! the #500 strict realization gate. It does not re-implement the gate: it
//! normalizes OCI launch facts into #498 realization nodes, classifies them, and
//! runs the existing strict gate via
//! [`crate::application::strict_realization::evaluate_contract`]. Two notions of
//! "strict" coexist and are independent:
//!
//! * `OciPolicyMode::Strict` — the capsule's own declared policy mode, enforced
//!   by `oci_single_target::enforce_policy_gate`.
//! * [`LaunchProfile::Strict`] (`--strict-realization`, #500) — an opt-in,
//!   operator-driven fail-closed profile, enforced here.
//!
//! ## Identity boundary preserved (#501)
//!
//! Nothing here treats an image digest, container id, pid, log path, or rendered
//! argv as identity. The realization nodes carry only normalized policy facts and
//! the (content-hash) image digest as a *materialization* input. The strict-gate
//! error payload is already redacted by construction (see #500).

use capsule_core::execution_identity::{OciEnforcementStatus, OciProviderReceiptEvidence};
use capsule_core::execution_plan::error::AtoExecutionError;
use capsule_core::execution_plan::model::OciPolicyEnvelope;
use capsule_core::realization::{
    LaunchProfile, MountFact, RealizationContract, RealizationNode, RealizationNodeFacts,
    RealizationRequest, classify,
};

use super::oci::{OciImageDigest, OciProjectionPlan};

/// What the selected provider can enforce for a projection's declared policy.
///
/// Conservative by design (#501): anything other than [`OciEnforcementStatus::Enforced`]
/// — downgraded, unsupported, or unknown — is treated as **not enforced** by the
/// strict gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OciProviderEnforcement {
    pub network: OciEnforcementStatus,
    pub capability: OciEnforcementStatus,
}

impl OciProviderEnforcement {
    /// The static enforcement model for the PodmanProvider (`oci-podman-v1`).
    ///
    /// Podman **cannot** enforce a network egress allowlist (the same fact the
    /// existing `enforce_policy_gate` relies on), so a declared network policy is
    /// `Unsupported`. It applies a default capability sandbox, so capability
    /// policy is `Enforced`. When no policy of a given kind is declared, the
    /// status is `Enforced` (there is nothing to downgrade).
    pub(crate) fn podman(network_policy_required: bool) -> Self {
        Self {
            network: if network_policy_required {
                OciEnforcementStatus::Unsupported
            } else {
                OciEnforcementStatus::Enforced
            },
            capability: OciEnforcementStatus::Enforced,
        }
    }
}

/// Normalized, provider-agnostic OCI launch facts the strict gate reasons over.
///
/// Built from the resolved [`OciPolicyEnvelope`] plus the [`OciProjectionPlan`]
/// — never from a raw `podman`/`docker` command string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OciStrictFacts {
    /// A network policy (e.g. an egress allowlist) is declared and must be
    /// enforced.
    pub network_policy_required: bool,
    /// A capability/sandbox policy is declared and must be enforced.
    pub capability_policy_required: bool,
    /// The pinned image digest, if the launch image is fully pinned. `None` for
    /// a tag-only / unpinned reference — which strict mode treats as an
    /// unreproducible (missing) required image identity.
    pub image_digest: Option<String>,
    /// Targets of host-path bind mounts (engine-managed volumes excluded). Each
    /// is a host-bound fallback strict mode disallows.
    pub host_bound_mount_targets: Vec<String>,
}

impl OciStrictFacts {
    /// Derive strict facts from a projection plan plus whether a network policy
    /// (e.g. an egress allowlist) is declared. This is the shared core used by
    /// both the single-target and orchestration paths.
    pub(crate) fn from_projection(plan: &OciProjectionPlan, network_policy_required: bool) -> Self {
        let image_digest = match &plan.image.digest {
            OciImageDigest::Pinned(digest) => Some(digest.clone()),
            OciImageDigest::Unpinned => None,
        };
        Self {
            network_policy_required,
            // No OCI-specific capability/sandbox policy facet is modeled on the
            // envelope today; podman applies a default sandbox. Capability
            // blocking is still supported by the gate for providers that report
            // they cannot enforce it.
            capability_policy_required: false,
            image_digest,
            host_bound_mount_targets: plan
                .mounts
                .iter()
                .filter(|m| !m.engine_volume)
                .map(|m| m.target.clone())
                .collect(),
        }
    }

    /// Derive strict facts from the resolved policy envelope and projection plan.
    pub(crate) fn from_launch(envelope: &OciPolicyEnvelope, plan: &OciProjectionPlan) -> Self {
        Self::from_projection(plan, !envelope.egress_allow.is_empty())
    }
}

/// Placeholder used for [`RealizationContract::resolved_execution_id`] when the
/// graph-derived resolved execution id is not yet threaded into the OCI launch
/// path.
///
/// The single identity field of a realization contract is the *graph-derived*
/// resolved execution id (#501); a provider projection fingerprint is **not**
/// that id and must never be substituted for it. The OCI launch path does not
/// build a `LaunchGraphBundle` today (it returns before the source/host receipt
/// path), so the id is honestly unbound. This is a value-free constant — never a
/// digest, container id, pid, or command — and never surfaces in the per-node
/// strict-gate error payload.
const GRAPH_EXECUTION_ID_UNBOUND: &str = "oci-strict-gate:graph-execution-id-unbound";

/// Build a #498 [`RealizationContract`] from normalized OCI facts + the
/// provider's enforcement capability. The strict gate (#500) then decides.
///
/// `graph_resolved_execution_id` is the genuine graph-derived resolved execution
/// id when available; `None` records [`GRAPH_EXECUTION_ID_UNBOUND`] rather than
/// fabricating one from a provider projection fingerprint.
fn oci_realization_contract(
    facts: &OciStrictFacts,
    enforcement: &OciProviderEnforcement,
    graph_resolved_execution_id: Option<&str>,
) -> RealizationContract {
    classify(RealizationRequest {
        resolved_execution_id: graph_resolved_execution_id
            .unwrap_or(GRAPH_EXECUTION_ID_UNBOUND)
            .to_string(),
        nodes: oci_realization_nodes("oci-", facts, enforcement),
        edges: Vec::new(),
    })
}

/// Build the realization nodes for one OCI projection. `node_id_prefix` scopes
/// the node ids so a multi-service contract can identify the blocked service
/// (e.g. `"oci-"` for single-target, `"oci-<service>-"` for orchestration). The
/// prefix is a value-free service/node label — never a host path or secret.
fn oci_realization_nodes(
    node_id_prefix: &str,
    facts: &OciStrictFacts,
    enforcement: &OciProviderEnforcement,
) -> Vec<RealizationNode> {
    let mut nodes: Vec<RealizationNode> = Vec::new();

    nodes.push(RealizationNode::required(
        format!("{node_id_prefix}network-policy"),
        RealizationNodeFacts::NetworkPolicy {
            required: facts.network_policy_required,
            provider_can_enforce: enforcement.network.is_enforced(),
            policy_ref: Some("oci-egress".to_string()),
        },
    ));
    nodes.push(RealizationNode::required(
        format!("{node_id_prefix}capability-policy"),
        RealizationNodeFacts::CapabilityPolicy {
            required: facts.capability_policy_required,
            provider_can_enforce: enforcement.capability.is_enforced(),
            policy_ref: Some("oci-capability".to_string()),
        },
    ));
    // The image identity as a required materialization input: a pinned digest is
    // materializable; an unpinned (tag-only) reference is a missing required
    // immutable input. The digest is a content hash here — never identity.
    nodes.push(RealizationNode::required(
        format!("{node_id_prefix}image"),
        RealizationNodeFacts::DependencyOutput {
            dependency_output_hash: facts.image_digest.clone(),
        },
    ));
    if !facts.host_bound_mount_targets.is_empty() {
        let mounts = facts
            .host_bound_mount_targets
            .iter()
            .map(|target| MountFact {
                role: target.clone(),
                host_path_required: true,
                projectable: false,
            })
            .collect();
        nodes.push(RealizationNode::required(
            format!("{node_id_prefix}filesystem-view"),
            RealizationNodeFacts::FilesystemView { mounts },
        ));
    }

    nodes
}

/// Enforce the strict realization profile for an OCI launch.
///
/// In [`LaunchProfile::Normal`] this is a no-op (returns `Ok`) — normal mode may
/// record downgraded/best-effort evidence but never newly blocks an OCI launch.
/// In [`LaunchProfile::Strict`] it blocks (typed `AtoExecutionError`,
/// `ATO_ERR_STRICT_REALIZATION_BLOCKED`) when a required policy facet is not
/// enforced, the image is unpinned, or a host-bound mount fallback is required.
pub(crate) fn enforce_strict_oci(
    facts: &OciStrictFacts,
    enforcement: &OciProviderEnforcement,
    profile: LaunchProfile,
    graph_resolved_execution_id: Option<&str>,
) -> Result<(), AtoExecutionError> {
    if !profile.is_strict() {
        return Ok(());
    }
    let contract = oci_realization_contract(facts, enforcement, graph_resolved_execution_id);
    crate::application::strict_realization::evaluate_contract(&contract, &[], profile)
}

/// One orchestrated OCI service's strict input: a value-free service label plus
/// its normalized facts and the provider's enforcement capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OciServiceStrict {
    /// Stable service/node label used to scope the realization node ids so a
    /// blocked service is identifiable in the error payload. Value-free — never a
    /// host path, env value, or secret.
    pub service_label: String,
    pub facts: OciStrictFacts,
    pub enforcement: OciProviderEnforcement,
}

/// Enforce the strict realization profile across an OCI **service graph**.
///
/// Builds one realization contract whose nodes are scoped per service (so a
/// block names the offending service), classifies it, and runs the #500 strict
/// gate once — reusing the exact same node-building and gate as the single-target
/// path. In [`LaunchProfile::Normal`] it is a no-op; in strict mode it blocks the
/// launch *before* any provider side effect, reporting every blocked service.
pub(crate) fn enforce_strict_oci_services(
    services: &[OciServiceStrict],
    profile: LaunchProfile,
    graph_resolved_execution_id: Option<&str>,
) -> Result<(), AtoExecutionError> {
    if !profile.is_strict() {
        return Ok(());
    }
    let mut nodes: Vec<RealizationNode> = Vec::new();
    for service in services {
        let prefix = format!("oci-{}-", service.service_label);
        nodes.extend(oci_realization_nodes(
            &prefix,
            &service.facts,
            &service.enforcement,
        ));
    }
    let contract = classify(RealizationRequest {
        resolved_execution_id: graph_resolved_execution_id
            .unwrap_or(GRAPH_EXECUTION_ID_UNBOUND)
            .to_string(),
        nodes,
        edges: Vec::new(),
    });
    crate::application::strict_realization::evaluate_contract(&contract, &[], profile)
}

/// Build receipt-safe provider evidence for one projection, recording the
/// provider's enforcement status and reflecting a declared network policy (egress
/// allowlist) as a required `network-policy` capability — so
/// `capabilities_required` and `network_enforcement_status` always agree.
///
/// Shared by the single-target receipt builder and the orchestration path so the
/// capability/enforcement alignment lives in exactly one place (#501 review).
pub(crate) fn provider_receipt_evidence(
    plan: &OciProjectionPlan,
    enforcement: &OciProviderEnforcement,
    network_policy_required: bool,
) -> OciProviderReceiptEvidence {
    let mut evidence = plan.receipt_evidence_with(enforcement.network, enforcement.capability);
    if network_policy_required
        && !evidence
            .capabilities_required
            .iter()
            .any(|c| c == "network-policy")
    {
        evidence
            .capabilities_required
            .push("network-policy".to_string());
        evidence.capabilities_required.sort();
    }
    evidence
}

#[cfg(test)]
mod tests;
