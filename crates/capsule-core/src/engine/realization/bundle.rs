//! Adapter: [`LaunchGraphBundle`] + host/provider evidence → [`RealizationContract`].
//!
//! The launch bundle is the source of *structure* (which nodes exist, the
//! entrypoint, the required-env set, declared policy facets, dependency
//! aliases) and of the one identity field, `resolved_execution_id`. It does
//! **not** carry realization *evidence* — content hashes, materialized
//! artifacts, host paths, provider enforcement capability, or state bindings.
//! Those are host/provider facts supplied via [`RealizationEnvironment`], whose
//! future producer is the `ato-cli` provider-projection layer (#501). Keeping
//! them in a separate, explicit input means the contract can never silently
//! invent evidence it does not have.
//!
//! ## Identity boundary (#501)
//!
//! `resolved_execution_id` is copied from
//! [`crate::engine::execution_graph::DerivedExecutionIds::resolved_execution_id`].
//! A provider's `container_id` may be supplied on
//! [`ProviderProjectionEvidence`] to *prove* the adapter ignores it — it is
//! never copied into a node id, the contract id, or any status.
//!
//! ## Scope (#498-A)
//!
//! This adapter does not derive realization edges from the resolved graph: the
//! realization-node taxonomy is a re-projection of the raw graph, so a faithful
//! edge mapping is deferred. Edge classification is still fully supported by
//! [`super::classify`] for callers that build edges directly.

use std::collections::{BTreeMap, BTreeSet};

use crate::engine::execution_graph::{ExecutionGraphNode, LaunchGraphBundle};

use super::classify::{
    MountFact, RealizationNode, RealizationNodeFacts, RealizationRequest, classify,
};
use super::model::{RealizationContract, RealizationNodeKind, RedactedProjectionCommand};
use super::verify::{
    MaterializationVerificationRequest, MaterializedNodeInput, MaterializedNodeSource,
};

/// Realization evidence about a runtime toolchain.
#[derive(Debug, Clone, Default)]
pub struct RuntimeEvidence {
    pub declared_identity: Option<String>,
    pub materialized_binary_hash: Option<String>,
}

/// Realization evidence about a pinned runtime tool (lockfile `binary_sha256`).
#[derive(Debug, Clone, Default)]
pub struct RuntimeToolEvidence {
    /// `None` ⇒ not yet populated ⇒ the tool is `Unavailable` (#473).
    pub binary_sha256: Option<String>,
    /// A local binary is present and matched `binary_sha256`.
    pub materialized_match: bool,
}

/// Realization evidence about a persistent-state binding.
#[derive(Debug, Clone)]
pub struct StateBindingEvidence {
    pub id: String,
    pub binding_present: bool,
    pub has_creation_policy: bool,
    pub reference: Option<String>,
}

/// Realization evidence about a provider projection (e.g. OCI).
///
/// `renderer` + `raw_argv` are a transient *input*: this struct is not
/// serializable and never reaches the contract. The adapter redacts `raw_argv`
/// into a [`RedactedProjectionCommand`] at projection time (#498-A review), so
/// the raw command — which may embed env values, tokens, or URLs — cannot be
/// persisted by the downstream receipt writeback (#493).
#[derive(Debug, Clone, Default)]
pub struct ProviderProjectionEvidence {
    /// Provider id (`"oci:podman"`, …). Used to derive the node id.
    pub provider: String,
    /// Renderer label, e.g. `"podman-create"`. Value-free.
    pub renderer: String,
    /// Raw rendered argv. Redacted by the adapter; never stored on the
    /// contract. Empty ⇒ no projection-command evidence is emitted.
    pub raw_argv: Vec<String>,
    /// Runtime handle the provider returns. **Never** identity (#501); carried
    /// here only so the adapter can demonstrably ignore it.
    pub container_id: Option<String>,
}

/// Host/provider realization evidence the launch bundle does not carry.
#[derive(Debug, Clone, Default)]
pub struct RealizationEnvironment {
    pub declared_source_hash: Option<String>,
    pub materialized_source_hash: Option<String>,
    /// Runtime evidence keyed by runtime identifier (matches the graph's
    /// `Runtime` node identifier where one exists).
    pub runtimes: BTreeMap<String, RuntimeEvidence>,
    /// Runtime-tool evidence keyed by tool identifier.
    pub runtime_tools: BTreeMap<String, RuntimeToolEvidence>,
    /// Dependency output content hashes keyed by dependency alias.
    pub dependency_output_hashes: BTreeMap<String, String>,
    /// Env names that are declared/closed at launch. A required env name not in
    /// this set is undeclared.
    pub declared_env: BTreeSet<String>,
    /// Filesystem mounts the host will project.
    pub mounts: Vec<MountFact>,
    /// Persistent-state bindings.
    pub state_bindings: Vec<StateBindingEvidence>,
    /// Whether the projecting provider can enforce network policy.
    pub provider_enforces_network: bool,
    /// Whether the projecting provider can enforce capability policy.
    pub provider_enforces_capability: bool,
    /// Provider projection, if one applies.
    pub provider_projection: Option<ProviderProjectionEvidence>,
}

/// Build a [`RealizationContract`] from a launch bundle plus host/provider
/// evidence.
pub fn realization_from_launch_bundle(
    bundle: &LaunchGraphBundle,
    env: &RealizationEnvironment,
) -> RealizationContract {
    let resolved_execution_id = bundle.derived.execution_ids.resolved_execution_id.clone();

    let mut nodes: Vec<RealizationNode> = Vec::new();

    // Source node — keyed on the graph's Source node identifier when present.
    if let Some(source_id) = first_source_identifier(bundle) {
        nodes.push(RealizationNode::required(
            source_id,
            RealizationNodeFacts::Source {
                declared_tree_hash: env.declared_source_hash.clone(),
                materialized_tree_hash: env.materialized_source_hash.clone(),
            },
        ));
    }

    // Runtime nodes — one per Runtime node in the resolved graph.
    for runtime_id in runtime_identifiers(bundle) {
        let evidence = env.runtimes.get(&runtime_id).cloned().unwrap_or_default();
        nodes.push(RealizationNode::required(
            runtime_id,
            RealizationNodeFacts::Runtime {
                declared_identity: evidence.declared_identity,
                materialized_binary_hash: evidence.materialized_binary_hash,
            },
        ));
    }

    // Runtime tools — keyed entirely by the lockfile-derived evidence map.
    for (tool_id, evidence) in &env.runtime_tools {
        nodes.push(RealizationNode::required(
            format!("runtime-tool:{tool_id}"),
            RealizationNodeFacts::RuntimeTool {
                binary_sha256: evidence.binary_sha256.clone(),
                materialized_match: evidence.materialized_match,
            },
        ));
    }

    // Dependency outputs — one per declared dependency provider.
    for provider in &bundle.derived.dependency_contracts.providers {
        let hash = env.dependency_output_hashes.get(&provider.alias).cloned();
        nodes.push(RealizationNode::required(
            format!("dependency-output:{}", provider.output_identifier),
            RealizationNodeFacts::DependencyOutput {
                dependency_output_hash: hash,
            },
        ));
    }

    // Filesystem view — a single node summarising the projected mounts.
    if !env.mounts.is_empty() {
        nodes.push(RealizationNode::required(
            "filesystem-view",
            RealizationNodeFacts::FilesystemView {
                mounts: env.mounts.clone(),
            },
        ));
    }

    // Env closure — required env from preflight, minus what's declared.
    let required_env = &bundle.derived.preflight.required_env;
    if !required_env.is_empty() {
        let undeclared: Vec<String> = required_env
            .iter()
            .filter(|name| !env.declared_env.contains(*name))
            .cloned()
            .collect();
        nodes.push(RealizationNode::required(
            "env-closure",
            RealizationNodeFacts::EnvClosure {
                undeclared_required: undeclared,
            },
        ));
    }

    // Policy facets — present only when the preflight declares a policy hash.
    if let Some(hash) = &bundle.derived.preflight.network_policy_hash {
        nodes.push(RealizationNode::required(
            "network-policy",
            RealizationNodeFacts::NetworkPolicy {
                required: true,
                provider_can_enforce: env.provider_enforces_network,
                policy_ref: Some(hash.clone()),
            },
        ));
    }
    if let Some(hash) = &bundle.derived.preflight.capability_policy_hash {
        nodes.push(RealizationNode::required(
            "capability-policy",
            RealizationNodeFacts::CapabilityPolicy {
                required: true,
                provider_can_enforce: env.provider_enforces_capability,
                policy_ref: Some(hash.clone()),
            },
        ));
    }

    // State bindings.
    for binding in &env.state_bindings {
        nodes.push(RealizationNode::required(
            format!("state-binding:{}", binding.id),
            RealizationNodeFacts::StateBinding {
                binding_present: binding.binding_present,
                has_creation_policy: binding.has_creation_policy,
                reference: binding.reference.clone(),
            },
        ));
    }

    // Entrypoint — from the launch envelope view when present.
    if let Some(launch) = &bundle.derived.launch {
        nodes.push(RealizationNode::required(
            "entrypoint",
            RealizationNodeFacts::Entrypoint {
                argv_declared: !launch.command.is_empty(),
                cwd_declared: !launch.logical_cwd.is_empty(),
            },
        ));
    }

    // Provider projection — the rendered command is redacted into evidence
    // here, and the container id is deliberately dropped (#501, #498-A review).
    if let Some(projection) = &env.provider_projection {
        let projection_command = (!projection.raw_argv.is_empty()).then(|| {
            RedactedProjectionCommand::from_argv(&projection.renderer, &projection.raw_argv)
        });
        nodes.push(RealizationNode::optional(
            format!("provider-projection:{}", projection.provider),
            RealizationNodeFacts::ProviderProjection {
                provider: projection.provider.clone(),
                projection_command,
            },
        ));
    }

    classify(RealizationRequest {
        resolved_execution_id,
        nodes,
        edges: Vec::new(),
    })
}

/// Project a launch bundle + host/provider evidence into a #499
/// [`MaterializationVerificationRequest`], using the *same* node ids as
/// [`realization_from_launch_bundle`] so a [`super::verify::verify_materialization`]
/// result overlays cleanly onto the #498 contract (matched by node id).
///
/// The rule that keeps strict mode honest rather than over-eager: a node is
/// only handed to the verifier when there is *something materialized to check*
/// (`actual_hash` is present), or it is a runtime tool (whose missing
/// `binary_sha256` is itself the #473 finding the verifier must surface). A
/// declared-only node is `Materializable` under #498 — re-derivable, not yet
/// built — and must **not** be escalated to a verifier "missing object" block
/// here; that judgment waits on the materialized-hash producer (#501).
///
/// Consequently, with today's conservative (default) evidence this request is
/// empty and the overlay is a no-op; the moment #501 grounds materialized
/// hashes the same projection starts carrying real verifications.
pub fn materialization_request_from_launch_bundle(
    bundle: &LaunchGraphBundle,
    env: &RealizationEnvironment,
) -> MaterializationVerificationRequest {
    let mut nodes: Vec<MaterializedNodeInput> = Vec::new();

    if let Some(source_id) = first_source_identifier(bundle) {
        push_when_materialized(
            &mut nodes,
            source_id,
            RealizationNodeKind::Source,
            MaterializedNodeSource::SourceTree,
            env.declared_source_hash.clone(),
            env.materialized_source_hash.clone(),
        );
    }

    for runtime_id in runtime_identifiers(bundle) {
        let evidence = env.runtimes.get(&runtime_id).cloned().unwrap_or_default();
        push_when_materialized(
            &mut nodes,
            runtime_id,
            RealizationNodeKind::Runtime,
            MaterializedNodeSource::RuntimeBinary,
            evidence.declared_identity,
            evidence.materialized_binary_hash,
        );
    }

    // Runtime tools always emit: a tool with no `binary_sha256` must surface as
    // the typed unpopulated-binary reason (#473), never be silently skipped.
    for (tool_id, evidence) in &env.runtime_tools {
        let actual = evidence
            .materialized_match
            .then(|| evidence.binary_sha256.clone())
            .flatten();
        nodes.push(MaterializedNodeInput {
            node_id: format!("runtime-tool:{tool_id}"),
            node_kind: RealizationNodeKind::RuntimeTool,
            expected_hash: evidence.binary_sha256.clone(),
            actual_hash: actual,
            required: true,
            source: MaterializedNodeSource::RuntimeTool,
            materialized_path: None,
        });
    }

    // Dependency outputs: only verify a materialized artifact. A declared-only
    // output is #498-Materializable and is deferred (no actual hash yet → #501).
    for provider in &bundle.derived.dependency_contracts.providers {
        let expected = env.dependency_output_hashes.get(&provider.alias).cloned();
        push_when_materialized(
            &mut nodes,
            format!("dependency-output:{}", provider.output_identifier),
            RealizationNodeKind::DependencyOutput,
            MaterializedNodeSource::DependencyOutput,
            expected,
            // No materialized dependency-output hash exists without #501.
            None,
        );
    }

    MaterializationVerificationRequest { nodes }
}

/// Push a verifier input only when a materialized artifact actually exists
/// (`actual` is `Some`). Declared-only nodes are intentionally omitted so the
/// #499 overlay never escalates a #498-`Materializable` node to a block.
fn push_when_materialized(
    nodes: &mut Vec<MaterializedNodeInput>,
    node_id: impl Into<String>,
    node_kind: RealizationNodeKind,
    source: MaterializedNodeSource,
    expected: Option<String>,
    actual: Option<String>,
) {
    if actual.is_none() {
        return;
    }
    nodes.push(MaterializedNodeInput {
        node_id: node_id.into(),
        node_kind,
        expected_hash: expected,
        actual_hash: actual,
        required: true,
        source,
        materialized_path: None,
    });
}

fn first_source_identifier(bundle: &LaunchGraphBundle) -> Option<String> {
    bundle
        .resolved_graph
        .nodes
        .iter()
        .find_map(|node| match node {
            ExecutionGraphNode::Source { identifier } => Some(identifier.clone()),
            _ => None,
        })
}

fn runtime_identifiers(bundle: &LaunchGraphBundle) -> Vec<String> {
    // BTreeSet (not `Vec::dedup`, which only collapses *adjacent* duplicates):
    // the resolved graph does not guarantee Runtime nodes for the same id are
    // emitted adjacently.
    bundle
        .resolved_graph
        .nodes
        .iter()
        .filter_map(|node| match node {
            ExecutionGraphNode::Runtime { identifier } => Some(identifier.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
