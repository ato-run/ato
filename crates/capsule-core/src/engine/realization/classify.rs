//! Pure classifier: typed node facts → [`RealizationContract`].
//!
//! This is the core of #498-A. It takes a [`RealizationRequest`] of typed,
//! per-node facts and decides, for each node, a [`RealizationStatus`] with
//! backing evidence — then aggregates a [`RealizationResult`]. It performs no
//! I/O and never inspects runtime evidence (container ids, pids). The
//! [`super::bundle`] adapter is responsible for turning a real launch bundle
//! plus host/provider evidence into a [`RealizationRequest`].
//!
//! ## Invariants enforced here
//!
//! - A runtime tool with no `binary_sha256` is [`RealizationStatus::Unavailable`],
//!   never `Verified`. Cross-platform lockfile population is incomplete, so a
//!   missing hash cannot be treated as verified (#473).
//! - `HostBound` / `StateBound` / `PolicyDowngraded` never collapse into a
//!   clean `Verified` — they stay visible and never silently realize.
//! - Only `Unavailable` is fail-closed; policy downgrades do not (here) make
//!   launch impossible. Strict fail-closed is #500.

use std::collections::BTreeMap;

use super::model::{
    RealizationContract, RealizationEdgeState, RealizationEdgeStatus, RealizationEvidence,
    RealizationNodeKind, RealizationNodeStatus, RealizationResult, RealizationStatus,
    UnrealizableReason,
};

/// A request to classify whether a resolved capsule can be realized.
#[derive(Debug, Clone)]
pub struct RealizationRequest {
    /// Graph-derived resolved execution id. The contract copies this through
    /// verbatim; it is never replaced by a container id or rendered command.
    pub resolved_execution_id: String,
    pub nodes: Vec<RealizationNode>,
    pub edges: Vec<RealizationEdge>,
}

/// A single node to classify.
#[derive(Debug, Clone)]
pub struct RealizationNode {
    pub node_id: String,
    /// Whether this node is a required immutable input. When `true`, missing
    /// evidence yields `Unavailable`; when `false`, it yields `Unknown` and
    /// does not block the result.
    pub required: bool,
    pub facts: RealizationNodeFacts,
}

impl RealizationNode {
    pub fn required(node_id: impl Into<String>, facts: RealizationNodeFacts) -> Self {
        Self {
            node_id: node_id.into(),
            required: true,
            facts,
        }
    }

    pub fn optional(node_id: impl Into<String>, facts: RealizationNodeFacts) -> Self {
        Self {
            node_id: node_id.into(),
            required: false,
            facts,
        }
    }
}

/// A mount that contributes to a [`RealizationNodeFacts::FilesystemView`].
#[derive(Debug, Clone)]
pub struct MountFact {
    pub role: String,
    /// The mount source is an absolute host path (host-portable concern).
    pub host_path_required: bool,
    /// The mount can be projected from declared graph facts.
    pub projectable: bool,
}

/// Typed, per-kind facts the classifier reasons over. Each variant maps to a
/// [`RealizationNodeKind`]. Hash fields are declared content/identity hashes,
/// never resolved secret values.
#[derive(Debug, Clone)]
pub enum RealizationNodeFacts {
    Source {
        declared_tree_hash: Option<String>,
        materialized_tree_hash: Option<String>,
    },
    Runtime {
        declared_identity: Option<String>,
        materialized_binary_hash: Option<String>,
    },
    RuntimeTool {
        /// Lockfile `binary_sha256`. `None` means not yet populated → the tool
        /// cannot be verified (#473).
        binary_sha256: Option<String>,
        /// A local binary is present and its hash matched `binary_sha256`.
        materialized_match: bool,
    },
    DependencyDerivation {
        declared_hash: Option<String>,
    },
    DependencyOutput {
        dependency_output_hash: Option<String>,
    },
    FilesystemView {
        mounts: Vec<MountFact>,
    },
    EnvClosure {
        /// Required env names that are not declared/closed. Non-empty ⇒
        /// `Unavailable`.
        undeclared_required: Vec<String>,
    },
    NetworkPolicy {
        required: bool,
        provider_can_enforce: bool,
        policy_ref: Option<String>,
    },
    CapabilityPolicy {
        required: bool,
        provider_can_enforce: bool,
        policy_ref: Option<String>,
    },
    StateBinding {
        binding_present: bool,
        has_creation_policy: bool,
        reference: Option<String>,
    },
    Entrypoint {
        argv_declared: bool,
        cwd_declared: bool,
    },
    ProviderProjection {
        provider: String,
        /// Rendered provider invocation (e.g. `podman run …`). Carried as
        /// evidence only — never identity (#501).
        projection_command: Option<String>,
    },
}

impl RealizationNodeFacts {
    pub fn kind(&self) -> RealizationNodeKind {
        match self {
            Self::Source { .. } => RealizationNodeKind::Source,
            Self::Runtime { .. } => RealizationNodeKind::Runtime,
            Self::RuntimeTool { .. } => RealizationNodeKind::RuntimeTool,
            Self::DependencyDerivation { .. } => RealizationNodeKind::DependencyDerivation,
            Self::DependencyOutput { .. } => RealizationNodeKind::DependencyOutput,
            Self::FilesystemView { .. } => RealizationNodeKind::FilesystemView,
            Self::EnvClosure { .. } => RealizationNodeKind::EnvClosure,
            Self::NetworkPolicy { .. } => RealizationNodeKind::NetworkPolicy,
            Self::CapabilityPolicy { .. } => RealizationNodeKind::CapabilityPolicy,
            Self::StateBinding { .. } => RealizationNodeKind::StateBinding,
            Self::Entrypoint { .. } => RealizationNodeKind::Entrypoint,
            Self::ProviderProjection { .. } => RealizationNodeKind::ProviderProjection,
        }
    }
}

/// An edge between two nodes, classified by whether both endpoints realize.
#[derive(Debug, Clone)]
pub struct RealizationEdge {
    pub source: String,
    pub target: String,
    pub kind: String,
}

/// Classify a request into a [`RealizationContract`].
pub fn classify(request: RealizationRequest) -> RealizationContract {
    let node_statuses: Vec<RealizationNodeStatus> =
        request.nodes.iter().map(classify_node).collect();

    let status_by_id: BTreeMap<&str, RealizationStatus> = node_statuses
        .iter()
        .map(|n| (n.node_id.as_str(), n.status))
        .collect();

    let edge_statuses: Vec<RealizationEdgeStatus> = request
        .edges
        .iter()
        .map(|edge| {
            let broken = endpoint_unavailable(&status_by_id, &edge.source)
                || endpoint_unavailable(&status_by_id, &edge.target);
            RealizationEdgeStatus {
                source: edge.source.clone(),
                target: edge.target.clone(),
                kind: edge.kind.clone(),
                state: if broken {
                    RealizationEdgeState::BrokenByUnavailableEndpoint
                } else {
                    RealizationEdgeState::Connectable
                },
            }
        })
        .collect();

    let reasons: Vec<UnrealizableReason> = request
        .nodes
        .iter()
        .zip(node_statuses.iter())
        .filter(|(_, status)| status.status.is_blocking())
        .map(|(node, status)| unrealizable_reason(node, status.node_kind))
        .collect();

    let result = if reasons.is_empty() {
        RealizationResult::Realized
    } else {
        RealizationResult::Unrealizable { reasons }
    };

    RealizationContract {
        resolved_execution_id: request.resolved_execution_id,
        node_statuses,
        edge_statuses,
        result,
    }
}

fn endpoint_unavailable(status_by_id: &BTreeMap<&str, RealizationStatus>, id: &str) -> bool {
    matches!(status_by_id.get(id), Some(RealizationStatus::Unavailable))
}

/// Map an `Unavailable` node onto its typed reason.
fn unrealizable_reason(node: &RealizationNode, kind: RealizationNodeKind) -> UnrealizableReason {
    match kind {
        RealizationNodeKind::RuntimeTool => UnrealizableReason::RuntimeToolBinaryHashUnavailable {
            node_id: node.node_id.clone(),
        },
        RealizationNodeKind::DependencyOutput => UnrealizableReason::MissingDependencyOutput {
            node_id: node.node_id.clone(),
        },
        RealizationNodeKind::StateBinding => UnrealizableReason::MissingStateBinding {
            node_id: node.node_id.clone(),
        },
        RealizationNodeKind::EnvClosure => UnrealizableReason::UndeclaredEnvRequired {
            node_id: node.node_id.clone(),
        },
        other => UnrealizableReason::MissingImmutableInput {
            node_id: node.node_id.clone(),
            node_kind: other,
        },
    }
}

fn classify_node(node: &RealizationNode) -> RealizationNodeStatus {
    let kind = node.facts.kind();
    let (status, evidence) = match &node.facts {
        RealizationNodeFacts::Source {
            declared_tree_hash,
            materialized_tree_hash,
        } => classify_hashed(
            "source-tree",
            declared_tree_hash.as_deref(),
            materialized_tree_hash.as_deref(),
            node.required,
        ),
        RealizationNodeFacts::Runtime {
            declared_identity,
            materialized_binary_hash,
        } => classify_hashed(
            "runtime",
            declared_identity.as_deref(),
            materialized_binary_hash.as_deref(),
            node.required,
        ),
        RealizationNodeFacts::RuntimeTool {
            binary_sha256,
            materialized_match,
        } => classify_runtime_tool(binary_sha256.as_deref(), *materialized_match),
        RealizationNodeFacts::DependencyDerivation { declared_hash } => classify_hashed(
            "dependency-derivation",
            declared_hash.as_deref(),
            None,
            node.required,
        ),
        RealizationNodeFacts::DependencyOutput {
            dependency_output_hash,
        } => match dependency_output_hash.as_deref() {
            Some(hash) => (
                RealizationStatus::Materializable,
                vec![RealizationEvidence::DeclaredHash {
                    label: "dependency-output".into(),
                    hash: hash.to_string(),
                }],
            ),
            None if node.required => (RealizationStatus::Unavailable, vec![]),
            None => (RealizationStatus::Unknown, vec![]),
        },
        RealizationNodeFacts::FilesystemView { mounts } => classify_filesystem(mounts),
        RealizationNodeFacts::EnvClosure {
            undeclared_required,
        } => {
            if undeclared_required.is_empty() {
                (RealizationStatus::Materializable, vec![])
            } else {
                (
                    RealizationStatus::Unavailable,
                    vec![RealizationEvidence::Note {
                        detail: format!("undeclared env: {}", undeclared_required.join(", ")),
                    }],
                )
            }
        }
        RealizationNodeFacts::NetworkPolicy {
            required,
            provider_can_enforce,
            policy_ref,
        } => classify_policy(
            "network",
            *required,
            *provider_can_enforce,
            policy_ref.as_deref(),
        ),
        RealizationNodeFacts::CapabilityPolicy {
            required,
            provider_can_enforce,
            policy_ref,
        } => classify_policy(
            "capability",
            *required,
            *provider_can_enforce,
            policy_ref.as_deref(),
        ),
        RealizationNodeFacts::StateBinding {
            binding_present,
            has_creation_policy,
            reference,
        } => classify_state(
            *binding_present,
            *has_creation_policy,
            reference.as_deref(),
            node.required,
        ),
        RealizationNodeFacts::Entrypoint {
            argv_declared,
            cwd_declared,
        } => {
            if *argv_declared && *cwd_declared {
                (RealizationStatus::Materializable, vec![])
            } else if node.required {
                (RealizationStatus::Unavailable, vec![])
            } else {
                (RealizationStatus::Unknown, vec![])
            }
        }
        RealizationNodeFacts::ProviderProjection {
            provider,
            projection_command,
        } => {
            let mut evidence = Vec::new();
            if let Some(command) = projection_command {
                evidence.push(RealizationEvidence::DerivedProjectionCommand {
                    provider: provider.clone(),
                    command: command.clone(),
                });
            }
            (RealizationStatus::Materializable, evidence)
        }
    };

    RealizationNodeStatus {
        node_id: node.node_id.clone(),
        node_kind: kind,
        status,
        evidence,
    }
}

/// Shared rule for nodes backed by a declared identity hash plus an optional
/// present/materialized hash: present ⇒ `Verified`, declared-only ⇒
/// `Materializable`, neither ⇒ `Unavailable`/`Unknown`.
fn classify_hashed(
    label: &str,
    declared: Option<&str>,
    materialized: Option<&str>,
    required: bool,
) -> (RealizationStatus, Vec<RealizationEvidence>) {
    if let Some(hash) = materialized {
        return (
            RealizationStatus::Verified,
            vec![RealizationEvidence::VerifiedArtifact {
                label: label.to_string(),
                hash: hash.to_string(),
            }],
        );
    }
    if let Some(hash) = declared {
        return (
            RealizationStatus::Materializable,
            vec![RealizationEvidence::DeclaredHash {
                label: label.to_string(),
                hash: hash.to_string(),
            }],
        );
    }
    if required {
        (RealizationStatus::Unavailable, vec![])
    } else {
        (RealizationStatus::Unknown, vec![])
    }
}

/// Runtime-tool rule (#473): a missing `binary_sha256` is `Unavailable`, never
/// `Verified`.
fn classify_runtime_tool(
    binary_sha256: Option<&str>,
    materialized_match: bool,
) -> (RealizationStatus, Vec<RealizationEvidence>) {
    match binary_sha256 {
        None => (RealizationStatus::Unavailable, vec![]),
        Some(hash) if materialized_match => (
            RealizationStatus::Verified,
            vec![RealizationEvidence::VerifiedArtifact {
                label: "runtime-tool".into(),
                hash: hash.to_string(),
            }],
        ),
        Some(hash) => (
            RealizationStatus::Materializable,
            vec![RealizationEvidence::DeclaredHash {
                label: "runtime-tool".into(),
                hash: hash.to_string(),
            }],
        ),
    }
}

fn classify_filesystem(mounts: &[MountFact]) -> (RealizationStatus, Vec<RealizationEvidence>) {
    if let Some(host_mount) = mounts.iter().find(|m| m.host_path_required) {
        return (
            RealizationStatus::HostBound,
            vec![RealizationEvidence::HostBinding {
                detail: format!("host path required for mount '{}'", host_mount.role),
            }],
        );
    }
    if mounts.iter().all(|m| m.projectable) {
        return (RealizationStatus::Materializable, vec![]);
    }
    let unprojectable = mounts
        .iter()
        .find(|m| !m.projectable)
        .map(|m| m.role.clone())
        .unwrap_or_default();
    (
        RealizationStatus::Unavailable,
        vec![RealizationEvidence::Note {
            detail: format!("mount '{unprojectable}' is not projectable"),
        }],
    )
}

fn classify_policy(
    domain: &str,
    required: bool,
    provider_can_enforce: bool,
    policy_ref: Option<&str>,
) -> (RealizationStatus, Vec<RealizationEvidence>) {
    if !required {
        return (RealizationStatus::Materializable, vec![]);
    }
    if provider_can_enforce {
        (RealizationStatus::Materializable, vec![])
    } else {
        (
            RealizationStatus::PolicyDowngraded,
            vec![RealizationEvidence::PolicyEnforcementGap {
                policy: policy_ref.unwrap_or(domain).to_string(),
                detail: format!("provider cannot fully enforce {domain} policy"),
            }],
        )
    }
}

fn classify_state(
    binding_present: bool,
    has_creation_policy: bool,
    reference: Option<&str>,
    required: bool,
) -> (RealizationStatus, Vec<RealizationEvidence>) {
    if binding_present {
        return (
            RealizationStatus::StateBound,
            vec![RealizationEvidence::StateBinding {
                reference: reference.unwrap_or("<bound>").to_string(),
            }],
        );
    }
    if has_creation_policy {
        return (RealizationStatus::Materializable, vec![]);
    }
    if required {
        (RealizationStatus::Unavailable, vec![])
    } else {
        (RealizationStatus::Unknown, vec![])
    }
}
