use std::collections::BTreeMap;

use super::builder::{
    ExecutionGraphBuildInput, ExecutionGraphBuilder, GraphDependencyInput, GraphHostInput,
    GraphPolicyInput, GraphSourceInput, GraphTargetInput,
};
use super::canonical::CanonicalGraphDomain;
use super::types::{ExecutionGraph, ExecutionGraphNode};

/// Production launch-graph API input.
///
/// This type deliberately carries graph facts rather than `ato-cli` runtime
/// handles. It can be populated from a manifest/router decision, lockfile,
/// execution plan, launch context, consent/policy facts, and source
/// materialization observations without making `capsule-core` depend on CLI
/// crates. All downstream execution views should be projections from the
/// resulting [`LaunchGraphBundle`].
#[derive(Debug, Clone, Default)]
pub struct LaunchGraphBundleInput {
    pub source: Option<GraphSourceInput>,
    pub targets: Vec<GraphTargetInput>,
    pub dependencies: Vec<GraphDependencyInput>,
    pub declared_host: Option<GraphHostInput>,
    pub resolved_host: Option<GraphHostInput>,
    pub declared_policy: Option<GraphPolicyInput>,
    pub resolved_policy: Option<GraphPolicyInput>,
    pub materialized: GraphMaterializationSeedInput,
    pub preflight: GraphPreflightInput,
    pub receipt: GraphReceiptSeedInput,
}

#[derive(Debug, Clone, Default)]
pub struct GraphMaterializationSeedInput {
    pub process_nodes: Vec<GraphRuntimeNodeInput>,
    pub runtime_instance_nodes: Vec<GraphRuntimeNodeInput>,
    pub state_nodes: Vec<GraphRuntimeNodeInput>,
    pub network_nodes: Vec<GraphRuntimeNodeInput>,
    pub bridge_capability_nodes: Vec<GraphRuntimeNodeInput>,
}

#[derive(Debug, Clone)]
pub struct GraphRuntimeNodeInput {
    pub kind: GraphRuntimeNodeKind,
    pub identifier: String,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphRuntimeNodeKind {
    Process,
    RuntimeInstance,
    State,
    Network,
    BridgeCapability,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphPreflightInput {
    pub required_env: Vec<String>,
    pub sandbox_constraints: Vec<String>,
    pub runtime_constraints: Vec<String>,
    pub dependency_aliases: Vec<String>,
    pub network_policy_hash: Option<String>,
    pub capability_policy_hash: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphReceiptSeedInput {
    pub runner: Option<String>,
    pub host_fingerprint: Option<String>,
    pub redaction_policy_version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LaunchGraphBundle {
    pub declared_graph: ExecutionGraph,
    pub resolved_graph: ExecutionGraph,
    pub materialized_graph_seed: ExecutionGraph,
    pub derived: LaunchGraphDerivedViews,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchGraphDerivedViews {
    pub execution_ids: DerivedExecutionIds,
    pub preflight: DerivedPreflightView,
    pub dependency_contracts: DerivedDependencyContracts,
    pub receipt_seed: DerivedReceiptSeed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedExecutionIds {
    pub declared_execution_id: String,
    pub resolved_execution_id: String,
    pub observed_execution_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DerivedPreflightView {
    pub required_env: Vec<String>,
    pub sandbox_constraints: Vec<String>,
    pub runtime_constraints: Vec<String>,
    pub dependency_aliases: Vec<String>,
    pub network_policy_hash: Option<String>,
    pub capability_policy_hash: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DerivedDependencyContracts {
    pub providers: Vec<DerivedDependencyProvider>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedDependencyProvider {
    pub alias: String,
    pub provider_identifier: String,
    pub output_identifier: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DerivedReceiptSeed {
    pub runner: Option<String>,
    pub host_fingerprint: Option<String>,
    pub redaction_policy_version: Option<String>,
}

impl ExecutionGraphBuilder {
    pub fn build_launch_bundle(input: LaunchGraphBundleInput) -> LaunchGraphBundle {
        let declared_graph = Self::build(ExecutionGraphBuildInput {
            source: input.source.clone(),
            targets: input.targets.clone(),
            dependencies: input.dependencies.clone(),
            host: input.declared_host.clone(),
            policy: input.declared_policy.clone(),
        });
        let resolved_graph = Self::build(ExecutionGraphBuildInput {
            source: input.source,
            targets: input.targets,
            dependencies: input.dependencies,
            host: merge_host(input.declared_host, input.resolved_host),
            policy: merge_policy(input.declared_policy, input.resolved_policy),
        });
        let materialized_graph_seed = materialized_graph_seed(&resolved_graph, &input.materialized);
        let execution_ids = DerivedExecutionIds {
            declared_execution_id: declared_graph
                .canonical_form(CanonicalGraphDomain::Declared)
                .digest_hex(),
            resolved_execution_id: resolved_graph
                .canonical_form(CanonicalGraphDomain::Resolved)
                .digest_hex(),
            observed_execution_id: None,
        };
        let derived = LaunchGraphDerivedViews {
            execution_ids,
            preflight: DerivedPreflightView {
                required_env: sorted_dedup(input.preflight.required_env),
                sandbox_constraints: sorted_dedup(input.preflight.sandbox_constraints),
                runtime_constraints: sorted_dedup(input.preflight.runtime_constraints),
                dependency_aliases: sorted_dedup(input.preflight.dependency_aliases),
                network_policy_hash: input.preflight.network_policy_hash,
                capability_policy_hash: input.preflight.capability_policy_hash,
            },
            dependency_contracts: derive_dependency_contracts(&resolved_graph),
            receipt_seed: DerivedReceiptSeed {
                runner: input.receipt.runner,
                host_fingerprint: input.receipt.host_fingerprint,
                redaction_policy_version: input.receipt.redaction_policy_version,
            },
        };

        LaunchGraphBundle {
            declared_graph,
            resolved_graph,
            materialized_graph_seed,
            derived,
        }
    }
}

fn merge_host(
    declared: Option<GraphHostInput>,
    resolved: Option<GraphHostInput>,
) -> Option<GraphHostInput> {
    match (declared, resolved) {
        (None, None) => None,
        (Some(host), None) | (None, Some(host)) => Some(host),
        (Some(mut declared), Some(resolved)) => {
            declared.filesystem = resolved.filesystem.or(declared.filesystem);
            declared.network = resolved.network.or(declared.network);
            declared.env = resolved.env.or(declared.env);
            declared.state = resolved.state.or(declared.state);
            declared.filesystem_source_root = resolved
                .filesystem_source_root
                .or(declared.filesystem_source_root);
            declared.filesystem_working_directory = resolved
                .filesystem_working_directory
                .or(declared.filesystem_working_directory);
            declared.filesystem_view_hash = resolved
                .filesystem_view_hash
                .or(declared.filesystem_view_hash);
            Some(declared)
        }
    }
}

fn merge_policy(
    declared: Option<GraphPolicyInput>,
    resolved: Option<GraphPolicyInput>,
) -> Option<GraphPolicyInput> {
    match (declared, resolved) {
        (None, None) => None,
        (Some(policy), None) | (None, Some(policy)) => Some(policy),
        (Some(mut declared), Some(resolved)) => {
            declared.constraints.extend(resolved.constraints);
            declared.network_policy_hash = resolved
                .network_policy_hash
                .or(declared.network_policy_hash);
            declared.capability_policy_hash = resolved
                .capability_policy_hash
                .or(declared.capability_policy_hash);
            declared.sandbox_policy_hash = resolved
                .sandbox_policy_hash
                .or(declared.sandbox_policy_hash);
            Some(declared)
        }
    }
}

fn materialized_graph_seed(
    resolved_graph: &ExecutionGraph,
    input: &GraphMaterializationSeedInput,
) -> ExecutionGraph {
    let mut graph = resolved_graph.clone();
    for node in input
        .process_nodes
        .iter()
        .chain(input.runtime_instance_nodes.iter())
        .chain(input.state_nodes.iter())
        .chain(input.network_nodes.iter())
        .chain(input.bridge_capability_nodes.iter())
    {
        graph.nodes.push(match node.kind {
            GraphRuntimeNodeKind::Process => ExecutionGraphNode::Process {
                identifier: format!("process://{}", node.identifier),
            },
            GraphRuntimeNodeKind::RuntimeInstance => ExecutionGraphNode::RuntimeInstance {
                identifier: format!("runtime-instance://{}", node.identifier),
            },
            GraphRuntimeNodeKind::State => ExecutionGraphNode::State {
                identifier: node.identifier.clone(),
            },
            GraphRuntimeNodeKind::Network => ExecutionGraphNode::Network {
                identifier: node.identifier.clone(),
            },
            GraphRuntimeNodeKind::BridgeCapability => ExecutionGraphNode::BridgeCapability {
                identifier: node.identifier.clone(),
            },
        });
        for (key, value) in &node.metadata {
            graph.labels.insert(
                format!("materialized.{}.{}", node.identifier, key),
                value.clone(),
            );
        }
    }
    graph.nodes.sort_by(|a, b| {
        a.kind_discriminant()
            .cmp(&b.kind_discriminant())
            .then_with(|| a.identifier().cmp(b.identifier()))
    });
    graph.nodes.dedup();
    graph
}

fn derive_dependency_contracts(graph: &ExecutionGraph) -> DerivedDependencyContracts {
    let mut providers = graph
        .nodes
        .iter()
        .filter_map(|node| match node {
            ExecutionGraphNode::Provider { identifier } => {
                let alias = identifier
                    .strip_prefix("provider://")
                    .unwrap_or(identifier.as_str())
                    .to_string();
                Some(DerivedDependencyProvider {
                    output_identifier: format!("output://{alias}"),
                    provider_identifier: identifier.clone(),
                    alias,
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    providers.sort_by(|a, b| a.alias.cmp(&b.alias));
    DerivedDependencyContracts { providers }
}

fn sorted_dedup(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_input() -> LaunchGraphBundleInput {
        LaunchGraphBundleInput {
            source: Some(GraphSourceInput {
                identifier: "manifest://capsule.toml".to_string(),
            }),
            targets: vec![GraphTargetInput {
                identifier: "target://main".to_string(),
                runtime: "runtime://source/node".to_string(),
            }],
            dependencies: vec![
                GraphDependencyInput {
                    provider: "provider://db".to_string(),
                    output: "output://db".to_string(),
                },
                GraphDependencyInput {
                    provider: "provider://cache".to_string(),
                    output: "output://cache".to_string(),
                },
            ],
            declared_host: Some(GraphHostInput {
                filesystem_source_root: Some("workspace:.".to_string()),
                filesystem_working_directory: Some("workspace:.".to_string()),
                ..GraphHostInput::default()
            }),
            resolved_host: Some(GraphHostInput {
                filesystem_view_hash: Some("blake3:fs".to_string()),
                ..GraphHostInput::default()
            }),
            declared_policy: Some(GraphPolicyInput {
                network_policy_hash: Some("blake3:net".to_string()),
                capability_policy_hash: Some("blake3:cap".to_string()),
                ..GraphPolicyInput::default()
            }),
            resolved_policy: Some(GraphPolicyInput {
                sandbox_policy_hash: Some("blake3:sandbox".to_string()),
                ..GraphPolicyInput::default()
            }),
            materialized: GraphMaterializationSeedInput::default(),
            preflight: GraphPreflightInput {
                required_env: vec!["B".to_string(), "A".to_string(), "A".to_string()],
                dependency_aliases: vec!["db".to_string(), "cache".to_string()],
                network_policy_hash: Some("blake3:net".to_string()),
                capability_policy_hash: Some("blake3:cap".to_string()),
                ..GraphPreflightInput::default()
            },
            receipt: GraphReceiptSeedInput {
                runner: Some("ato-cli".to_string()),
                host_fingerprint: Some("darwin:arm64".to_string()),
                redaction_policy_version: Some("v1".to_string()),
            },
        }
    }

    #[test]
    fn launch_bundle_ids_are_stable_under_input_order() {
        let forward = ExecutionGraphBuilder::build_launch_bundle(sample_input());
        let mut reversed_input = sample_input();
        reversed_input.dependencies.reverse();
        reversed_input.targets.reverse();
        let reversed = ExecutionGraphBuilder::build_launch_bundle(reversed_input);
        assert_eq!(
            forward.derived.execution_ids.declared_execution_id,
            reversed.derived.execution_ids.declared_execution_id
        );
        assert_eq!(
            forward.derived.execution_ids.resolved_execution_id,
            reversed.derived.execution_ids.resolved_execution_id
        );
    }

    #[test]
    fn resolved_id_changes_with_resolved_facts_but_declared_id_does_not() {
        let baseline = ExecutionGraphBuilder::build_launch_bundle(sample_input());
        let mut changed = sample_input();
        changed.resolved_host = Some(GraphHostInput {
            filesystem_view_hash: Some("blake3:other-fs".to_string()),
            ..GraphHostInput::default()
        });
        let changed = ExecutionGraphBuilder::build_launch_bundle(changed);
        assert_eq!(
            baseline.derived.execution_ids.declared_execution_id,
            changed.derived.execution_ids.declared_execution_id
        );
        assert_ne!(
            baseline.derived.execution_ids.resolved_execution_id,
            changed.derived.execution_ids.resolved_execution_id
        );
    }

    #[test]
    fn derived_dependency_contracts_are_provider_projection() {
        let bundle = ExecutionGraphBuilder::build_launch_bundle(sample_input());
        let aliases = bundle
            .derived
            .dependency_contracts
            .providers
            .iter()
            .map(|provider| provider.alias.as_str())
            .collect::<Vec<_>>();
        assert_eq!(aliases, vec!["cache", "db"]);
    }

    #[test]
    fn materialized_fields_do_not_affect_resolved_id() {
        let baseline = ExecutionGraphBuilder::build_launch_bundle(sample_input());
        let mut materialized = sample_input();
        materialized
            .materialized
            .process_nodes
            .push(GraphRuntimeNodeInput {
                kind: GraphRuntimeNodeKind::Process,
                identifier: "main".to_string(),
                metadata: BTreeMap::from([("pid".to_string(), "1234".to_string())]),
            });
        let materialized = ExecutionGraphBuilder::build_launch_bundle(materialized);
        assert_eq!(
            baseline.derived.execution_ids.resolved_execution_id,
            materialized.derived.execution_ids.resolved_execution_id
        );
        assert_ne!(
            baseline.materialized_graph_seed.nodes,
            materialized.materialized_graph_seed.nodes
        );
    }
}
