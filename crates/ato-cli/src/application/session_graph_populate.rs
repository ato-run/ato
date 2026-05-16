//! Populate `StoredSessionInfo.graph` from the same inputs that produce
//! `StoredDependencyContracts`.
//!
//! Slice A of umbrella ato-run/ato#74 Phase 3 (issue #125):
//! **strictly additive, write-only**.
//!
//! - This module only *writes* the persisted [`StoredExecutionGraph`]
//!   subset alongside the legacy [`StoredDependencyContracts`].
//! - Teardown (`stop_session` / `stop_*`) is unchanged and continues to
//!   read `dependency_contracts` exclusively.
//! - The populated `graph` is therefore a passive observation today.
//!   Slice -B will add a teardown driver that consumes it; slice -C will
//!   flip `dependency_contracts` to a derived view of the graph.
//!
//! ## Implementation choice
//!
//! Two adapter shapes are possible:
//!
//! 1. Manifest+lock → [`ExecutionGraphBuildInput`] →
//!    [`ExecutionGraphBuilder::build`] → typed lowering to
//!    [`StoredExecutionGraph`].
//! 2. [`StoredDependencyContracts`] → [`StoredExecutionGraph`] directly.
//!
//! This slice picks (2). Rationale: the producer subset (provider node
//! per alias + `Provides` edge to its output) is a 1:1 lowering of
//! `StoredDependencyContracts.providers`, so going through the full
//! builder buys nothing here and adds an intermediate type and a second
//! call site to keep in sync. Slice -B / -C, which expand the populated
//! subset (Service nodes, runtime, host facets), will likely flip to
//! shape (1) — that's the right time, not now.
//!
//! ## Equivalence guard
//!
//! Every callsite passes through [`populate_graph_from_dependency_contracts`].
//! Inside, after building the graph, a `debug_assert_eq!` pins
//! provider-set parity between the input `StoredDependencyContracts` and
//! the produced `StoredExecutionGraph`. Same pattern as the validate
//! migration's PR-4a / PR-4b parity asserts.

use std::collections::BTreeMap;

use ato_session_core::{
    StoredDependencyContracts, StoredDependencyProvider, StoredExecutionGraph, StoredGraphEdge,
    StoredGraphNode, StoredOrchestrationServices,
};

/// Node kind written by this slice. Kept as a string constant so
/// `record.rs` can stay stringly-typed (forward-compat for additional
/// node kinds in slice -B / -C without a wire bump).
pub(crate) const NODE_KIND_PROVIDER: &str = "provider";

/// Node kind for orchestration `[services.<name>]` entries. Appended by
/// [`append_orchestration_services_to_graph`] when the wrapper has
/// detached resolved services (managed / OCI). Teardown reads this
/// kind from `record.graph` to stop services without re-reading the
/// `orchestration_services` legacy subset.
pub(crate) const NODE_KIND_SERVICE: &str = "service";

/// Edge kind written by this slice. Mirrors
/// `ExecutionGraphEdgeKind::Provides` for the provider → output direction.
pub(crate) const EDGE_KIND_PROVIDES: &str = "provides";

/// Identifier prefix for output (dependency-output) nodes/edges. The
/// slice-A subset does not emit `DependencyOutput` *nodes* (only
/// providers + `provides` edges to a synthetic output identifier),
/// because the legacy `StoredDependencyContracts` carries no per-output
/// data beyond the alias. Slice -B / -C will add the output node when
/// per-output facets (resolved coordinates, content hashes) are
/// persisted.
const OUTPUT_IDENTIFIER_PREFIX: &str = "output://";

/// Build a [`StoredExecutionGraph`] subset from a
/// [`StoredDependencyContracts`].
///
/// Returns `None` when `dependency_contracts` is `None` — call sites
/// that build a session record without a meaningful provider subset
/// (guest sessions, single-target runtime sessions without
/// `[dependencies.*]`, the legacy orchestration supervisor) keep
/// `graph: None` for now.
///
/// When `Some`, the returned graph carries one `Provider` node per
/// provider alias plus a `Provides` edge from the provider identifier
/// to its synthetic output identifier. Nodes are sorted by
/// `(kind, identifier)` and edges by `(source, target, kind)` so the
/// on-disk shape is deterministic regardless of the input provider
/// order.
///
/// ## Parity guard
///
/// In debug builds this function panics if the produced provider node
/// set diverges from the input `dependency_contracts.providers`. The
/// guard fires before the value is returned, so every caller — production
/// and tests alike — gets equivalence enforcement for free. Same shape as
/// PR-4a / PR-4b's `ato validate` parity asserts.
pub(crate) fn populate_graph_from_dependency_contracts(
    dependency_contracts: Option<&StoredDependencyContracts>,
) -> Option<StoredExecutionGraph> {
    let contracts = dependency_contracts?;

    let mut nodes: Vec<StoredGraphNode> = Vec::with_capacity(contracts.providers.len());
    let mut edges: Vec<StoredGraphEdge> = Vec::with_capacity(contracts.providers.len());

    for provider in &contracts.providers {
        let provider_identifier = provider.alias.clone();
        let output_identifier = format!("{OUTPUT_IDENTIFIER_PREFIX}{}", provider.alias);

        let mut metadata = BTreeMap::new();
        metadata.insert("resolved".to_string(), provider.resolved.clone());
        metadata.insert(
            "state_dir".to_string(),
            provider.state_dir.display().to_string(),
        );
        if let Some(log_path) = &provider.log_path {
            metadata.insert("log_path".to_string(), log_path.display().to_string());
        }
        if !provider.runtime_export_keys.is_empty() {
            metadata.insert(
                "runtime_export_keys".to_string(),
                provider.runtime_export_keys.join(","),
            );
        }

        nodes.push(StoredGraphNode {
            kind: NODE_KIND_PROVIDER.to_string(),
            identifier: provider_identifier.clone(),
            pid: Some(provider.pid),
            state_dir: Some(provider.state_dir.clone()),
            port: provider.allocated_port,
            container_id: None,
            capability: None,
            metadata,
        });
        edges.push(StoredGraphEdge {
            source: provider_identifier,
            target: output_identifier,
            kind: EDGE_KIND_PROVIDES.to_string(),
            metadata: BTreeMap::new(),
        });
    }

    nodes.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| a.identifier.cmp(&b.identifier))
    });
    edges.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then_with(|| a.target.cmp(&b.target))
            .then_with(|| a.kind.cmp(&b.kind))
    });

    let graph = StoredExecutionGraph {
        schema_version: StoredExecutionGraph::SCHEMA_VERSION,
        nodes,
        edges,
    };

    debug_assert!(
        graph_provider_set_matches(&graph, contracts),
        "session_graph_populate: provider node set diverges from dependency_contracts.providers \
         (contracts={contracts:?}, graph={graph:?})"
    );

    Some(graph)
}

/// Append a `Service` node per detached orchestration service onto the
/// graph produced by [`populate_graph_from_dependency_contracts`].
///
/// Slice-B of #125: the persisted [`StoredOrchestrationServices`] subset
/// is the existing source of truth for `[services.<name>]` teardown.
/// This appender mirrors it into the graph so the teardown driver can
/// read services and providers from a single graph rather than two
/// parallel subsets. The legacy `orchestration_services` subset is
/// still written by the call site for parity; teardown reads either.
///
/// Returns the input graph unchanged when `services` is `None` or has
/// no entries. When the input graph is `None` but services exist, this
/// function creates a graph with only the service nodes (no providers)
/// so teardown still has the data it needs.
pub(crate) fn append_orchestration_services_to_graph(
    graph: Option<StoredExecutionGraph>,
    services: Option<&StoredOrchestrationServices>,
) -> Option<StoredExecutionGraph> {
    let services = services?;
    if services.services.is_empty() {
        return graph;
    }

    let mut graph = graph.unwrap_or(StoredExecutionGraph {
        schema_version: StoredExecutionGraph::SCHEMA_VERSION,
        nodes: Vec::new(),
        edges: Vec::new(),
    });

    for (order, service) in services.services.iter().enumerate() {
        let mut metadata = BTreeMap::new();
        metadata.insert("order".to_string(), order.to_string());
        metadata.insert("target_label".to_string(), service.target_label.clone());
        if !service.host_ports.is_empty() {
            let encoded = service
                .host_ports
                .iter()
                .map(|(host, container)| format!("{host}:{container}"))
                .collect::<Vec<_>>()
                .join(",");
            metadata.insert("host_ports".to_string(), encoded);
        }
        if let Some(port) = service.published_port {
            metadata.insert("published_port".to_string(), port.to_string());
        }

        graph.nodes.push(StoredGraphNode {
            kind: NODE_KIND_SERVICE.to_string(),
            identifier: service.name.clone(),
            pid: service.local_pid,
            state_dir: None,
            port: service.published_port,
            container_id: service.container_id.clone(),
            capability: None,
            metadata,
        });
    }

    // Re-sort nodes by (kind, identifier) so the on-disk shape is
    // deterministic regardless of the input service order.
    graph.nodes.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| a.identifier.cmp(&b.identifier))
    });

    Some(graph)
}

/// Reverse projection: read provider nodes out of a graph back into the
/// legacy [`StoredDependencyContracts`] shape so teardown call sites that
/// haven't migrated to the graph reader yet keep working. Slice-C will
/// flip the few remaining readers and let this function fall out of use.
///
/// Returns `None` when the input graph has no `Provider` nodes (matching
/// the legacy `dependency_contracts: Option<...>` contract — empty means
/// "no contract was active").
pub(crate) fn dependency_contracts_from_graph(
    graph: Option<&StoredExecutionGraph>,
    consumer_pid: i32,
) -> Option<StoredDependencyContracts> {
    let graph = graph?;
    let providers: Vec<StoredDependencyProvider> = graph
        .nodes
        .iter()
        .filter(|node| node.kind == NODE_KIND_PROVIDER)
        .map(|node| {
            let resolved = node
                .metadata
                .get("resolved")
                .cloned()
                .unwrap_or_else(|| format!("{OUTPUT_IDENTIFIER_PREFIX}{}", node.identifier));
            let state_dir = node.state_dir.clone().unwrap_or_else(|| {
                node.metadata
                    .get("state_dir")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_default()
            });
            let log_path = node
                .metadata
                .get("log_path")
                .map(std::path::PathBuf::from);
            let runtime_export_keys = node
                .metadata
                .get("runtime_export_keys")
                .map(|encoded| {
                    encoded
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            StoredDependencyProvider {
                alias: node.identifier.clone(),
                pid: node.pid.unwrap_or(0),
                state_dir,
                resolved,
                allocated_port: node.port,
                log_path,
                runtime_export_keys,
            }
        })
        .collect();

    if providers.is_empty() {
        return None;
    }

    Some(StoredDependencyContracts {
        consumer_pid,
        providers,
    })
}

/// Returns `true` when `graph.nodes[kind == "provider"]` (as a set of
/// identifiers) equals `contracts.providers` aliases (as a set), and the
/// counts match. Used as the body of the `debug_assert!` parity guard
/// inside [`populate_graph_from_dependency_contracts`].
fn graph_provider_set_matches(
    graph: &StoredExecutionGraph,
    contracts: &StoredDependencyContracts,
) -> bool {
    let mut graph_aliases: Vec<&str> = graph
        .nodes
        .iter()
        .filter(|node| node.kind == NODE_KIND_PROVIDER)
        .map(|node| node.identifier.as_str())
        .collect();
    graph_aliases.sort_unstable();

    let mut contract_aliases: Vec<&str> = contracts
        .providers
        .iter()
        .map(|p| p.alias.as_str())
        .collect();
    contract_aliases.sort_unstable();

    graph_aliases.len() == contract_aliases.len() && graph_aliases == contract_aliases
}

#[cfg(test)]
mod tests {
    use super::*;
    use ato_session_core::StoredDependencyProvider;
    use std::path::PathBuf;

    fn provider(alias: &str, pid: i32) -> StoredDependencyProvider {
        StoredDependencyProvider {
            alias: alias.to_string(),
            pid,
            state_dir: PathBuf::from(format!("/tmp/{alias}")),
            resolved: format!("capsule://example/{alias}@1"),
            allocated_port: None,
            log_path: None,
            runtime_export_keys: Vec::new(),
        }
    }

    /// Bare provider-kind node helper for parity-guard tests that only
    /// need identifier + kind. Real `populate_graph_from_dependency_contracts`
    /// produces nodes with the full provider envelope populated.
    fn provider_node(identifier: &str) -> StoredGraphNode {
        StoredGraphNode {
            kind: NODE_KIND_PROVIDER.to_string(),
            identifier: identifier.to_string(),
            pid: None,
            state_dir: None,
            port: None,
            container_id: None,
            capability: None,
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn none_dependency_contracts_returns_none() {
        assert!(populate_graph_from_dependency_contracts(None).is_none());
    }

    #[test]
    fn empty_providers_returns_some_empty_graph() {
        let contracts = StoredDependencyContracts {
            consumer_pid: 4242,
            providers: Vec::new(),
        };
        let graph = populate_graph_from_dependency_contracts(Some(&contracts))
            .expect("populate returns Some");
        assert_eq!(graph.schema_version, StoredExecutionGraph::SCHEMA_VERSION);
        assert!(graph.nodes.is_empty());
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn single_provider_yields_one_node_and_one_provides_edge() {
        let contracts = StoredDependencyContracts {
            consumer_pid: 4242,
            providers: vec![provider("db", 5252)],
        };
        let graph = populate_graph_from_dependency_contracts(Some(&contracts))
            .expect("populate returns Some");
        assert_eq!(graph.schema_version, StoredExecutionGraph::SCHEMA_VERSION);
        assert_eq!(graph.nodes.len(), 1);
        assert_eq!(graph.nodes[0].kind, NODE_KIND_PROVIDER);
        assert_eq!(graph.nodes[0].identifier, "db");
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].kind, EDGE_KIND_PROVIDES);
        assert_eq!(graph.edges[0].source, "db");
        assert_eq!(graph.edges[0].target, "output://db");
    }

    #[test]
    fn multi_provider_nodes_are_sorted_deterministically() {
        // Input order is `c, a, b`; output must come back sorted.
        let contracts = StoredDependencyContracts {
            consumer_pid: 4242,
            providers: vec![provider("c", 1), provider("a", 2), provider("b", 3)],
        };
        let graph = populate_graph_from_dependency_contracts(Some(&contracts))
            .expect("populate returns Some");
        let identifiers: Vec<&str> = graph.nodes.iter().map(|n| n.identifier.as_str()).collect();
        assert_eq!(identifiers, vec!["a", "b", "c"]);
        // Edges are `Provides` source = provider identifier, sorted by source.
        let sources: Vec<&str> = graph.edges.iter().map(|e| e.source.as_str()).collect();
        assert_eq!(sources, vec!["a", "b", "c"]);
        // All edges share the canonical kind.
        assert!(graph.edges.iter().all(|e| e.kind == EDGE_KIND_PROVIDES));
    }

    /// Parity guard load-bearing test: a synthetic graph whose provider set
    /// disagrees with the contract's must trip `graph_provider_set_matches`.
    /// We exercise the predicate directly (rather than letting
    /// `populate_graph_from_dependency_contracts` build the graph) so the
    /// test pins the equivalence-check logic itself, not the populate
    /// path's input parity.
    #[test]
    fn parity_guard_detects_divergent_provider_set() {
        let contracts = StoredDependencyContracts {
            consumer_pid: 4242,
            providers: vec![provider("db", 1), provider("cache", 2)],
        };
        // Synthesized "wrong" graph: only one provider node, missing `cache`.
        let divergent_graph = StoredExecutionGraph {
            schema_version: StoredExecutionGraph::SCHEMA_VERSION,
            nodes: vec![provider_node("db")],
            edges: Vec::new(),
        };
        assert!(!graph_provider_set_matches(&divergent_graph, &contracts));

        // Sanity: an exactly-matching graph passes the predicate.
        let matching_graph = StoredExecutionGraph {
            schema_version: StoredExecutionGraph::SCHEMA_VERSION,
            nodes: vec![provider_node("cache"), provider_node("db")],
            edges: Vec::new(),
        };
        assert!(graph_provider_set_matches(&matching_graph, &contracts));
    }

    /// In debug builds (where this slice is verified pre-merge), feeding
    /// the populator a contract and then mutating the result so the
    /// provider set diverges and re-running the parity check must panic.
    /// We can't make `populate_*` itself panic on legitimate input — by
    /// construction it produces a matching graph — so we drive the
    /// `debug_assert!` body's predicate directly with a hand-built
    /// divergent graph and confirm the check is wired.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "session_graph_populate")]
    fn parity_guard_panics_on_synthetic_divergence_in_debug_builds() {
        // The real populate path never produces a divergent graph, so we
        // emulate the assertion firing by calling the macro directly with
        // hand-built mismatching inputs. This verifies the panic message
        // shape and that the macro is enabled in debug builds.
        let contracts = StoredDependencyContracts {
            consumer_pid: 4242,
            providers: vec![provider("db", 1), provider("cache", 2)],
        };
        let divergent_graph = StoredExecutionGraph {
            schema_version: StoredExecutionGraph::SCHEMA_VERSION,
            nodes: vec![provider_node("db")],
            edges: Vec::new(),
        };
        debug_assert!(
            graph_provider_set_matches(&divergent_graph, &contracts),
            "session_graph_populate: provider node set diverges (synthetic test)"
        );
    }

    /// Slice-A scope pin: the populated graph has exactly the node kinds
    /// `dependency_contracts` already represents — nothing else (no
    /// Service / Bridge / Filesystem / State / Runtime nodes; those are
    /// slice -B / -C territory).
    #[test]
    fn populated_graph_only_contains_provider_nodes() {
        let contracts = StoredDependencyContracts {
            consumer_pid: 4242,
            providers: vec![provider("db", 1), provider("cache", 2)],
        };
        let graph = populate_graph_from_dependency_contracts(Some(&contracts))
            .expect("populate returns Some");
        assert!(
            graph.nodes.iter().all(|n| n.kind == NODE_KIND_PROVIDER),
            "slice A only emits Provider nodes; got kinds: {:?}",
            graph.nodes.iter().map(|n| &n.kind).collect::<Vec<_>>()
        );
    }
}
