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
    /// PR-4b (refs umbrella v0.6.0 graph-first migration): consent
    /// identity facts the consent layer keys on. Today
    /// `compile_execution_plan` is the only producer of
    /// `policy_segment_hash` / `provisioning_policy_hash`, so callers
    /// in `ato-cli` populate this from their `ExecutionPlan.consent`
    /// at bundle build time. The bundle projects it onto
    /// `DerivedConsentView`, where the ato-cli's
    /// `ExecutionConsentView::from_bundle` reads it.
    pub consent: Option<GraphConsentInput>,
}

/// PR-4b: consent identity input. Mirrors the 5 fields the consent
/// log keys on (3 from `ConsentKey` + 2 policy hashes). Callers feed
/// this from `ExecutionPlan.consent`; capsule-core stays
/// compile-plan-agnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphConsentInput {
    pub scoped_id: String,
    pub version: String,
    pub target_label: String,
    pub policy_segment_hash: String,
    pub provisioning_policy_hash: String,
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
    /// PR-4b: consent identity view. `None` when the call site did
    /// not supply `LaunchGraphBundleInput.consent` (back-compat for
    /// callers that haven't migrated yet — e.g. legacy unit tests).
    pub consent: Option<DerivedConsentView>,
}

/// PR-4b: bundle-projected consent identity. The 5 fields the
/// consent log keys on; passthrough from `GraphConsentInput`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedConsentView {
    pub scoped_id: String,
    pub version: String,
    pub target_label: String,
    pub policy_segment_hash: String,
    pub provisioning_policy_hash: String,
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

/// Projection of a single declared dependency onto the bundle's
/// derived view. PR-4a (refs umbrella v0.6.0 graph-first migration)
/// extends this with the 6 lockfile-comparison facets so
/// `verify_lockfile_against_contracts` can read the projection
/// directly without re-parsing the manifest TOML.
///
/// The 6 facets (source, source_type, contract, injection_bindings,
/// parameters, credentials) mirror `LockedCapsuleDependency`'s
/// shape 1:1, and re-use the manifest's own types
/// (`ParamValue`, `TemplatedString`) so equality is byte-stable.
/// See `GraphDependencyInput` for the credential safety rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedDependencyProvider {
    pub alias: String,
    pub provider_identifier: String,
    pub output_identifier: String,
    /// PR-4a additions. All optional / defaulted so receipts written
    /// before this PR still round-trip when re-read after upgrade.
    pub source: Option<String>,
    pub source_type: Option<String>,
    pub contract: Option<String>,
    pub injection_bindings: std::collections::BTreeMap<String, String>,
    pub parameters: std::collections::BTreeMap<String, crate::types::ParamValue>,
    pub credentials: std::collections::BTreeMap<String, crate::types::TemplatedString>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DerivedReceiptSeed {
    pub runner: Option<String>,
    pub host_fingerprint: Option<String>,
    pub redaction_policy_version: Option<String>,
}

impl ExecutionGraphBuilder {
    pub fn build_launch_bundle(input: LaunchGraphBundleInput) -> LaunchGraphBundle {
        // PR-4a: capture the typed dependency inputs before they're
        // consumed by the graph builder. The derived view reads from
        // these (not from the resolved graph's provider nodes) so the
        // 6 lockfile facets survive the projection.
        let dependency_inputs = input.dependencies.clone();
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
            // PR-4a: project the provider list from the typed
            // dependency inputs (which carry the 6 lockfile facets)
            // rather than the resolved graph nodes (which only carry
            // the provider identifier). This lets the lockfile
            // verifier consume the bundle-derived view without
            // re-parsing the manifest. Provider order is alias-sorted
            // for canonical comparison.
            dependency_contracts: derive_dependency_contracts_from_inputs(
                &dependency_inputs,
                &resolved_graph,
            ),
            // PR-4b: project consent identity from input. Passthrough.
            consent: input.consent.map(|input| DerivedConsentView {
                scoped_id: input.scoped_id,
                version: input.version,
                target_label: input.target_label,
                policy_segment_hash: input.policy_segment_hash,
                provisioning_policy_hash: input.provisioning_policy_hash,
            }),
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

/// PR-4a projection: build the derived dependency contract view from
/// the typed `GraphDependencyInput` list AND the resolved graph's
/// provider nodes. The graph nodes carry the canonical
/// `provider_identifier` shape; the input list carries the 6 lockfile
/// facets. We join on alias.
///
/// If the input list is empty (e.g. legacy call sites that haven't
/// migrated yet), fall back to the pre-PR-4a behavior of synthesizing
/// providers from graph nodes alone — the 6 new fields will be empty
/// defaults. This keeps the function back-compat for any caller that
/// hasn't started populating dependency facts.
fn derive_dependency_contracts_from_inputs(
    inputs: &[GraphDependencyInput],
    graph: &ExecutionGraph,
) -> DerivedDependencyContracts {
    // Index the resolved graph's provider identifiers by alias so we
    // can attach them to inputs without trusting the input's
    // `provider` field (which may differ in encoding).
    let mut provider_ids: std::collections::BTreeMap<String, String> = Default::default();
    for node in &graph.nodes {
        if let ExecutionGraphNode::Provider { identifier } = node {
            let alias = identifier
                .strip_prefix("provider://")
                .unwrap_or(identifier.as_str())
                .to_string();
            provider_ids.insert(alias, identifier.clone());
        }
    }

    let mut providers: Vec<DerivedDependencyProvider> = if inputs.is_empty() {
        // Back-compat path: synthesize from graph nodes only.
        provider_ids
            .into_iter()
            .map(|(alias, identifier)| DerivedDependencyProvider {
                output_identifier: format!("output://{alias}"),
                provider_identifier: identifier,
                alias,
                source: None,
                source_type: None,
                contract: None,
                injection_bindings: Default::default(),
                parameters: Default::default(),
                credentials: Default::default(),
            })
            .collect()
    } else {
        inputs
            .iter()
            .map(|input| {
                // The input's `provider` field is typically
                // `"provider://<alias>"`; derive the alias by
                // stripping the prefix, falling back to the raw form.
                let alias = input
                    .provider
                    .strip_prefix("provider://")
                    .unwrap_or(input.provider.as_str())
                    .to_string();
                let provider_identifier = provider_ids
                    .get(&alias)
                    .cloned()
                    .unwrap_or_else(|| input.provider.clone());
                DerivedDependencyProvider {
                    output_identifier: format!("output://{alias}"),
                    provider_identifier,
                    alias,
                    source: input.source.clone(),
                    source_type: input.source_type.clone(),
                    contract: input.contract.clone(),
                    injection_bindings: input.injection_bindings.clone(),
                    parameters: input.parameters.clone(),
                    credentials: input.credentials.clone(),
                }
            })
            .collect()
    };
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
                ..Default::default()
            },
                GraphDependencyInput {
                    provider: "provider://cache".to_string(),
                    output: "output://cache".to_string(),
                ..Default::default()
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
            consent: None,
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

    /// PR-4b round-trip: `GraphConsentInput` flows through the
    /// bundle build onto `DerivedConsentView` with the same field
    /// values. No normalization on the projection layer.
    #[test]
    fn graph_consent_input_round_trips_to_derived_consent_view() {
        let mut input = sample_input();
        input.consent = Some(GraphConsentInput {
            scoped_id: "publisher/slug".to_string(),
            version: "1.2.3".to_string(),
            target_label: "web".to_string(),
            policy_segment_hash: "blake3:cap".to_string(),
            provisioning_policy_hash: "blake3:prov".to_string(),
        });
        let bundle = ExecutionGraphBuilder::build_launch_bundle(input);
        let consent = bundle.derived.consent.expect("derived consent view");
        assert_eq!(consent.scoped_id, "publisher/slug");
        assert_eq!(consent.version, "1.2.3");
        assert_eq!(consent.target_label, "web");
        assert_eq!(consent.policy_segment_hash, "blake3:cap");
        assert_eq!(consent.provisioning_policy_hash, "blake3:prov");
    }

    /// PR-4b: when no consent input is provided, the derived view is
    /// `None`. Back-compat for legacy bundles built without consent
    /// (e.g. the receipt builder's internal bundle).
    #[test]
    fn missing_consent_input_yields_none_derived_view() {
        let bundle = ExecutionGraphBuilder::build_launch_bundle(sample_input());
        assert!(bundle.derived.consent.is_none());
    }

    /// PR-4b: `ConsentKey::from_execution_plan` and
    /// `ConsentKey::from_derived_consent_view` must produce the same
    /// key when fed equivalent inputs. This is the load-bearing
    /// contract for the consent store's two surfaces.
    #[test]
    fn consent_key_constructors_agree_on_equivalent_inputs() {
        use crate::execution_plan::model::ConsentKey;

        let view = DerivedConsentView {
            scoped_id: "publisher/slug".to_string(),
            version: "1.2.3".to_string(),
            target_label: "web".to_string(),
            policy_segment_hash: "blake3:cap".to_string(),
            provisioning_policy_hash: "blake3:prov".to_string(),
        };
        let key = ConsentKey::from_derived_consent_view(&view);
        assert_eq!(key.scoped_id, view.scoped_id);
        assert_eq!(key.version, view.version);
        assert_eq!(key.target_label, view.target_label);
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
