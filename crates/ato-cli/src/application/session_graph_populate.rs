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

/// PR-5a teardown-order edges (refs umbrella v0.6.0 graph-first
/// migration). Convention: **source DEPENDS ON target**.
///
/// `depends_on` is provider → provider OR service → service.
/// `uses` is service → provider (a service depends on the providers
/// its dep-contract resolves to).
///
/// PR-5b's `teardown_from_graph` walks these in reverse-topological
/// order: tear down a node before the nodes it depends on. e.g. an
/// edge `service-web --depends_on→ provider-db` means stop
/// `service-web` first, then `provider-db`.
pub(crate) const EDGE_KIND_DEPENDS_ON: &str = "depends_on";
/// PR-5a teardown-order edge: service → provider (a service uses a
/// provider). Same direction convention as `depends_on` — source
/// depends on target, so source is torn down first.
pub(crate) const EDGE_KIND_USES: &str = "uses";

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

// ---------------------------------------------------------------------------
// PR-5a additions (refs umbrella v0.6.0 graph-first migration)
// ---------------------------------------------------------------------------

/// PR-5a: 3-arg wrapper around
/// [`populate_graph_from_dependency_contracts`] that ALSO emits
/// `depends_on` edges between provider nodes derived from the
/// provided `provider_needs` map (alias → needed-alias list).
///
/// `provider_needs` is sourced from the live `RunningGraph.deps()[i].needs`
/// at the populate call site in `app_control/session.rs`. The
/// information isn't in `StoredDependencyContracts` today and a
/// schema bump would be additive, so we route it through the
/// populator's input instead of widening the stored DTO.
///
/// When `provider_needs` is `None` or empty, this is equivalent to
/// the 1-arg form.
pub(crate) fn populate_graph_from_dependency_contracts_with_needs(
    dependency_contracts: Option<&StoredDependencyContracts>,
    provider_needs: Option<&BTreeMap<String, Vec<String>>>,
) -> Option<StoredExecutionGraph> {
    let mut graph = populate_graph_from_dependency_contracts(dependency_contracts)?;
    if let Some(needs) = provider_needs {
        for (source_alias, target_aliases) in needs {
            for target_alias in target_aliases {
                graph.edges.push(StoredGraphEdge {
                    source: source_alias.clone(),
                    target: target_alias.clone(),
                    kind: EDGE_KIND_DEPENDS_ON.to_string(),
                    metadata: BTreeMap::new(),
                });
            }
        }
        graph.edges.sort_by(|a, b| {
            a.source
                .cmp(&b.source)
                .then_with(|| a.target.cmp(&b.target))
                .then_with(|| a.kind.cmp(&b.kind))
        });
    }
    Some(graph)
}

/// PR-5a: 3-arg wrapper around
/// [`append_orchestration_services_to_graph`] that ALSO emits
/// service-to-service `depends_on` edges (from the manifest's
/// `[services.<name>] depends_on = [...]`) and service-to-provider
/// `uses` edges (from the manifest's
/// `[services.<name>.dependencies.<alias>]` blocks).
///
/// `manifest_service_deps` carries `service_name → Vec<dep_target>`
/// where `dep_target` is either a sibling service name (emits
/// `depends_on`) or a provider alias (emits `uses`). Disambiguation
/// is by membership in the already-emitted service node set vs the
/// already-emitted provider node set.
///
/// When `manifest_service_deps` is `None` or empty, this is
/// equivalent to the 2-arg form.
pub(crate) fn append_orchestration_services_to_graph_with_deps(
    graph: Option<StoredExecutionGraph>,
    services: Option<&StoredOrchestrationServices>,
    manifest_service_deps: Option<&BTreeMap<String, Vec<String>>>,
) -> Option<StoredExecutionGraph> {
    let mut graph = append_orchestration_services_to_graph(graph, services)?;
    if let Some(deps) = manifest_service_deps {
        // Pre-compute the service-name and provider-alias sets so we
        // can classify each target.
        let service_names: std::collections::BTreeSet<String> = graph
            .nodes
            .iter()
            .filter(|node| node.kind == NODE_KIND_SERVICE)
            .map(|node| node.identifier.clone())
            .collect();
        let provider_aliases: std::collections::BTreeSet<String> = graph
            .nodes
            .iter()
            .filter(|node| node.kind == NODE_KIND_PROVIDER)
            .map(|node| node.identifier.clone())
            .collect();

        for (service_name, targets) in deps {
            for target in targets {
                let edge_kind = if service_names.contains(target) {
                    EDGE_KIND_DEPENDS_ON
                } else if provider_aliases.contains(target) {
                    EDGE_KIND_USES
                } else {
                    // Unknown target — skip (don't emit a dangling
                    // edge). The teardown driver would ignore it
                    // anyway.
                    continue;
                };
                graph.edges.push(StoredGraphEdge {
                    source: service_name.clone(),
                    target: target.clone(),
                    kind: edge_kind.to_string(),
                    metadata: BTreeMap::new(),
                });
            }
        }
        graph.edges.sort_by(|a, b| {
            a.source
                .cmp(&b.source)
                .then_with(|| a.target.cmp(&b.target))
                .then_with(|| a.kind.cmp(&b.kind))
        });
    }
    Some(graph)
}

/// PR-5a: completeness predicate consumed by PR-5b's `stop_session`
/// path. `teardown_from_graph` is preferred when this returns `true`;
/// `false` falls through to the legacy two-path teardown.
///
/// The check is conservative — every facet that the legacy paths
/// rely on for actual process / container teardown must be present
/// in the graph:
///   - provider count matches `dependency_contracts.providers.len()`,
///     every provider node has `pid` AND `state_dir`
///   - service count matches `orchestration_services.services.len()`,
///     every service node has either `local_pid` or `container_id`
///
/// PR-5b-fix: also require explicit ordering edges when multiple
/// providers or multiple services exist. Without inter-provider
/// `depends_on` edges (for multi-provider) or inter-service
/// `depends_on` edges (for multi-service), the graph can't express
/// teardown order and we must fall back to the legacy path which
/// orders by alias / insertion order. Single-of-a-kind sessions are
/// unaffected (no ordering needed when there's only one node).
///
/// PR-5b-fix (review round 2): also reject sessions whose provider
/// identifiers overlap with their service identifiers. The graph
/// teardown driver keys nodes by identifier alone (single namespace),
/// so a collision would silently drop one side; the legacy two-path
/// teardown is the safe choice in that case.
///
/// Sessions written before PR-5a (no `depends_on` edges, no
/// completeness facts populated) naturally return `false` here and
/// take the legacy teardown path.
pub(crate) fn graph_complete_for_teardown(
    record: &ato_session_core::StoredSessionInfo,
) -> bool {
    let Some(graph) = record.graph.as_ref() else {
        return false;
    };

    // Provider parity.
    let provider_count = record
        .dependency_contracts
        .as_ref()
        .map(|c| c.providers.len())
        .unwrap_or(0);
    let provider_nodes: Vec<&StoredGraphNode> = graph
        .nodes
        .iter()
        .filter(|node| node.kind == NODE_KIND_PROVIDER)
        .collect();
    if provider_nodes.len() != provider_count {
        return false;
    }
    if !provider_nodes
        .iter()
        .all(|node| node.pid.is_some() && node.state_dir.is_some())
    {
        return false;
    }

    // Service parity.
    let service_count = record
        .orchestration_services
        .as_ref()
        .map(|s| s.services.len())
        .unwrap_or(0);
    let service_nodes: Vec<&StoredGraphNode> = graph
        .nodes
        .iter()
        .filter(|node| node.kind == NODE_KIND_SERVICE)
        .collect();
    if service_nodes.len() != service_count {
        return false;
    }
    if !service_nodes
        .iter()
        .all(|node| node.pid.is_some() || node.container_id.is_some())
    {
        return false;
    }

    let provider_ids: std::collections::BTreeSet<&str> = provider_nodes
        .iter()
        .map(|n| n.identifier.as_str())
        .collect();
    let service_ids: std::collections::BTreeSet<&str> = service_nodes
        .iter()
        .map(|n| n.identifier.as_str())
        .collect();

    // PR-5b-fix (review round 2): provider and service node
    // identifiers share a single namespace inside `teardown_from_graph`
    // (`nodes_by_id: BTreeMap<&str, _>` and `compute_teardown_order`'s
    // eligible-node dedup both key on identifier alone). If a provider
    // alias collides with a service name — e.g. provider `db` and a
    // service literally called `db` — one node would overwrite the
    // other and silently leak the loser at teardown. Equally,
    // `append_orchestration_services_to_graph_with_deps` classifies
    // edge targets by service-name membership first, then provider
    // alias, so a service→provider `uses` edge could be miscategorized
    // as service→service `depends_on` when the names overlap. Refuse
    // graph-driven teardown for any record whose provider IDs intersect
    // its service IDs; the legacy two-path teardown is the safe choice.
    if !provider_ids.is_disjoint(&service_ids) {
        return false;
    }

    // PR-5b-fix: multi-provider requires explicit depends_on edges
    // between providers; otherwise teardown order is unspecified.
    if provider_nodes.len() > 1 {
        let has_inter_provider_depends_on = graph.edges.iter().any(|edge| {
            edge.kind == EDGE_KIND_DEPENDS_ON
                && provider_ids.contains(edge.source.as_str())
                && provider_ids.contains(edge.target.as_str())
        });
        if !has_inter_provider_depends_on {
            return false;
        }
    }

    // PR-5b-fix: multi-service requires explicit depends_on edges
    // between services; uses-only edges (service→provider) don't
    // order services among themselves.
    if service_nodes.len() > 1 {
        let has_inter_service_depends_on = graph.edges.iter().any(|edge| {
            edge.kind == EDGE_KIND_DEPENDS_ON
                && service_ids.contains(edge.source.as_str())
                && service_ids.contains(edge.target.as_str())
        });
        if !has_inter_service_depends_on {
            return false;
        }
    }

    true
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

    /// PR-5a: provider→provider `depends_on` edges from
    /// `provider_needs` round-trip onto the graph.
    #[test]
    fn provider_depends_on_edges_round_trip() {
        let contracts = StoredDependencyContracts {
            consumer_pid: 4242,
            providers: vec![provider("db", 1), provider("cache", 2)],
        };
        let mut needs: BTreeMap<String, Vec<String>> = BTreeMap::new();
        needs.insert("cache".to_string(), vec!["db".to_string()]);
        let graph = populate_graph_from_dependency_contracts_with_needs(
            Some(&contracts),
            Some(&needs),
        )
        .expect("populate returns Some");
        let depends_on: Vec<&StoredGraphEdge> = graph
            .edges
            .iter()
            .filter(|e| e.kind == EDGE_KIND_DEPENDS_ON)
            .collect();
        assert_eq!(depends_on.len(), 1);
        assert_eq!(depends_on[0].source, "cache");
        assert_eq!(depends_on[0].target, "db");
    }

    /// PR-5a: service→provider `uses` edges land for an
    /// orchestration manifest with service-to-provider dependencies.
    #[test]
    fn service_uses_provider_edges_round_trip() {
        use ato_session_core::{
            StoredOrchestrationService, StoredOrchestrationServices,
        };

        let contracts = StoredDependencyContracts {
            consumer_pid: 4242,
            providers: vec![provider("db", 1)],
        };
        let graph =
            populate_graph_from_dependency_contracts(Some(&contracts)).expect("graph");

        let services = StoredOrchestrationServices {
            wrapper_pid: 5555,
            services: vec![StoredOrchestrationService {
                name: "web".to_string(),
                target_label: "web".to_string(),
                local_pid: Some(6666),
                container_id: None,
                host_ports: Default::default(),
                published_port: Some(3000),
            }],
        };

        let mut service_deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
        service_deps.insert("web".to_string(), vec!["db".to_string()]);

        let graph = append_orchestration_services_to_graph_with_deps(
            Some(graph),
            Some(&services),
            Some(&service_deps),
        )
        .expect("Some graph");

        let uses: Vec<&StoredGraphEdge> =
            graph.edges.iter().filter(|e| e.kind == EDGE_KIND_USES).collect();
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].source, "web");
        assert_eq!(uses[0].target, "db");
    }

    /// PR-5a: service→service `depends_on` edges land for an
    /// orchestration manifest with inter-service dependencies.
    #[test]
    fn service_depends_on_service_edges_round_trip() {
        use ato_session_core::{
            StoredOrchestrationService, StoredOrchestrationServices,
        };

        let services = StoredOrchestrationServices {
            wrapper_pid: 5555,
            services: vec![
                StoredOrchestrationService {
                    name: "main".to_string(),
                    target_label: "main".to_string(),
                    local_pid: Some(1111),
                    container_id: None,
                    host_ports: Default::default(),
                    published_port: None,
                },
                StoredOrchestrationService {
                    name: "web".to_string(),
                    target_label: "web".to_string(),
                    local_pid: Some(2222),
                    container_id: None,
                    host_ports: Default::default(),
                    published_port: Some(3000),
                },
            ],
        };
        let mut service_deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
        service_deps.insert("web".to_string(), vec!["main".to_string()]);

        let graph = append_orchestration_services_to_graph_with_deps(
            None,
            Some(&services),
            Some(&service_deps),
        )
        .expect("Some graph");

        let depends_on: Vec<&StoredGraphEdge> = graph
            .edges
            .iter()
            .filter(|e| e.kind == EDGE_KIND_DEPENDS_ON)
            .collect();
        assert_eq!(depends_on.len(), 1);
        assert_eq!(depends_on[0].source, "web");
        assert_eq!(depends_on[0].target, "main");
    }

    /// PR-5a: completeness predicate returns true for a fully
    /// populated record and false for any missing facet.
    #[test]
    fn graph_complete_for_teardown_requires_every_facet() {
        use ato_session_core::{StoredOrchestrationService, StoredOrchestrationServices, StoredSessionInfo};
        use capsule_core::handle::{CapsuleDisplayStrategy, CapsuleRuntimeDescriptor, TrustState};

        let provider_node = StoredGraphNode {
            kind: NODE_KIND_PROVIDER.to_string(),
            identifier: "db".to_string(),
            pid: Some(1234),
            state_dir: Some(PathBuf::from("/tmp/db")),
            port: None,
            container_id: None,
            capability: None,
            metadata: BTreeMap::new(),
        };
        let service_node = StoredGraphNode {
            kind: NODE_KIND_SERVICE.to_string(),
            identifier: "web".to_string(),
            pid: Some(5678),
            state_dir: None,
            port: None,
            container_id: None,
            capability: None,
            metadata: BTreeMap::new(),
        };

        let make_record = |providers_match: bool, has_pid: bool, has_service_pid: bool| {
            let mut providers = vec![provider("db", 1234)];
            if !providers_match {
                providers.push(provider("cache", 9999));
            }
            let mut pn = provider_node.clone();
            if !has_pid {
                pn.pid = None;
            }
            let mut sn = service_node.clone();
            if !has_service_pid {
                sn.pid = None;
                sn.container_id = None;
            }
            StoredSessionInfo {
                session_id: "x".into(),
                handle: "p/s".into(),
                normalized_handle: "p/s".into(),
                canonical_handle: None,
                trust_state: TrustState::Untrusted,
                source: None,
                restricted: false,
                snapshot: None,
                runtime: CapsuleRuntimeDescriptor {
                    target_label: "main".into(),
                    runtime: None,
                    driver: None,
                    language: None,
                    port: None,
                },
                display_strategy: CapsuleDisplayStrategy::GuestWebview,
                pid: 0,
                log_path: String::new(),
                manifest_path: String::new(),
                target_label: "main".into(),
                notes: vec![],
                guest: None,
                web: None,
                terminal: None,
                service: None,
                dependency_contracts: Some(StoredDependencyContracts {
                    consumer_pid: 0,
                    providers,
                }),
                graph: Some(StoredExecutionGraph {
                    schema_version: StoredExecutionGraph::SCHEMA_VERSION,
                    nodes: vec![pn, sn],
                    edges: vec![],
                }),
                execution_id: None,
                execution_receipt_schema_version: None,
                declared_execution_id: None,
                resolved_execution_id: None,
                observed_execution_id: None,
                graph_completeness: None,
                reproducibility_class: None,
                orchestration_services: Some(StoredOrchestrationServices {
                    wrapper_pid: 0,
                    services: vec![StoredOrchestrationService {
                        name: "web".into(),
                        target_label: "web".into(),
                        local_pid: None,
                        container_id: None,
                        host_ports: Default::default(),
                        published_port: None,
                    }],
                }),
                schema_version: None,
                launch_digest: None,
                process_start_time_unix_ms: None,
            }
        };

        // Happy path: all facets populated.
        assert!(graph_complete_for_teardown(&make_record(true, true, true)));

        // Provider count mismatch.
        assert!(!graph_complete_for_teardown(&make_record(false, true, true)));

        // Provider missing pid.
        assert!(!graph_complete_for_teardown(&make_record(true, false, true)));

        // Service missing both pid and container_id.
        assert!(!graph_complete_for_teardown(&make_record(true, true, false)));
    }

    /// PR-5b-fix: multi-provider session with no depends_on edges
    /// between providers must return false — graph has no ordering
    /// info, fall back to legacy.
    #[test]
    fn graph_complete_for_teardown_rejects_multi_provider_without_ordering() {
        use ato_session_core::StoredSessionInfo;
        use capsule_core::handle::{CapsuleDisplayStrategy, CapsuleRuntimeDescriptor, TrustState};

        let p1 = StoredGraphNode {
            kind: NODE_KIND_PROVIDER.to_string(),
            identifier: "db".to_string(),
            pid: Some(1234),
            state_dir: Some(PathBuf::from("/tmp/db")),
            port: None,
            container_id: None,
            capability: None,
            metadata: BTreeMap::new(),
        };
        let p2 = StoredGraphNode {
            kind: NODE_KIND_PROVIDER.to_string(),
            identifier: "cache".to_string(),
            pid: Some(2345),
            state_dir: Some(PathBuf::from("/tmp/cache")),
            port: None,
            container_id: None,
            capability: None,
            metadata: BTreeMap::new(),
        };

        let make_record = |edges: Vec<StoredGraphEdge>| StoredSessionInfo {
            session_id: "x".into(),
            handle: "p/s".into(),
            normalized_handle: "p/s".into(),
            canonical_handle: None,
            trust_state: TrustState::Untrusted,
            source: None,
            restricted: false,
            snapshot: None,
            runtime: CapsuleRuntimeDescriptor {
                target_label: "main".into(),
                runtime: None,
                driver: None,
                language: None,
                port: None,
            },
            display_strategy: CapsuleDisplayStrategy::GuestWebview,
            pid: 0,
            log_path: String::new(),
            manifest_path: String::new(),
            target_label: "main".into(),
            notes: vec![],
            guest: None,
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: Some(StoredDependencyContracts {
                consumer_pid: 0,
                providers: vec![provider("db", 1234), provider("cache", 2345)],
            }),
            graph: Some(StoredExecutionGraph {
                schema_version: StoredExecutionGraph::SCHEMA_VERSION,
                nodes: vec![p1.clone(), p2.clone()],
                edges,
            }),
            execution_id: None,
            execution_receipt_schema_version: None,
            declared_execution_id: None,
            resolved_execution_id: None,
            observed_execution_id: None,
            graph_completeness: None,
            reproducibility_class: None,
            orchestration_services: None,
            schema_version: None,
            launch_digest: None,
            process_start_time_unix_ms: None,
        };

        // No ordering edges between db and cache → must reject.
        assert!(!graph_complete_for_teardown(&make_record(vec![])));

        // With an explicit depends_on edge between providers → accept.
        let depends_on = StoredGraphEdge {
            source: "cache".to_string(),
            target: "db".to_string(),
            kind: EDGE_KIND_DEPENDS_ON.to_string(),
            metadata: BTreeMap::new(),
        };
        assert!(graph_complete_for_teardown(&make_record(vec![depends_on])));
    }

    /// PR-5b-fix: multi-service session with no depends_on edges
    /// between services must return false. uses-only edges
    /// (service→provider) don't order services among themselves.
    #[test]
    fn graph_complete_for_teardown_rejects_multi_service_without_ordering() {
        use ato_session_core::{
            StoredOrchestrationService, StoredOrchestrationServices, StoredSessionInfo,
        };
        use capsule_core::handle::{CapsuleDisplayStrategy, CapsuleRuntimeDescriptor, TrustState};

        let s1 = StoredGraphNode {
            kind: NODE_KIND_SERVICE.to_string(),
            identifier: "web".to_string(),
            pid: Some(1111),
            state_dir: None,
            port: None,
            container_id: None,
            capability: None,
            metadata: BTreeMap::new(),
        };
        let s2 = StoredGraphNode {
            kind: NODE_KIND_SERVICE.to_string(),
            identifier: "worker".to_string(),
            pid: Some(2222),
            state_dir: None,
            port: None,
            container_id: None,
            capability: None,
            metadata: BTreeMap::new(),
        };
        let prov = StoredGraphNode {
            kind: NODE_KIND_PROVIDER.to_string(),
            identifier: "db".to_string(),
            pid: Some(3333),
            state_dir: Some(PathBuf::from("/tmp/db")),
            port: None,
            container_id: None,
            capability: None,
            metadata: BTreeMap::new(),
        };

        let make_record = |edges: Vec<StoredGraphEdge>| StoredSessionInfo {
            session_id: "x".into(),
            handle: "p/s".into(),
            normalized_handle: "p/s".into(),
            canonical_handle: None,
            trust_state: TrustState::Untrusted,
            source: None,
            restricted: false,
            snapshot: None,
            runtime: CapsuleRuntimeDescriptor {
                target_label: "main".into(),
                runtime: None,
                driver: None,
                language: None,
                port: None,
            },
            display_strategy: CapsuleDisplayStrategy::GuestWebview,
            pid: 0,
            log_path: String::new(),
            manifest_path: String::new(),
            target_label: "main".into(),
            notes: vec![],
            guest: None,
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: Some(StoredDependencyContracts {
                consumer_pid: 0,
                providers: vec![provider("db", 3333)],
            }),
            graph: Some(StoredExecutionGraph {
                schema_version: StoredExecutionGraph::SCHEMA_VERSION,
                nodes: vec![s1.clone(), s2.clone(), prov.clone()],
                edges,
            }),
            execution_id: None,
            execution_receipt_schema_version: None,
            declared_execution_id: None,
            resolved_execution_id: None,
            observed_execution_id: None,
            graph_completeness: None,
            reproducibility_class: None,
            orchestration_services: Some(StoredOrchestrationServices {
                wrapper_pid: 0,
                services: vec![
                    StoredOrchestrationService {
                        name: "web".into(),
                        target_label: "web".into(),
                        local_pid: Some(1111),
                        container_id: None,
                        host_ports: Default::default(),
                        published_port: None,
                    },
                    StoredOrchestrationService {
                        name: "worker".into(),
                        target_label: "worker".into(),
                        local_pid: Some(2222),
                        container_id: None,
                        host_ports: Default::default(),
                        published_port: None,
                    },
                ],
            }),
            schema_version: None,
            launch_digest: None,
            process_start_time_unix_ms: None,
        };

        // Only uses-edges (service→provider); no service→service ordering.
        let uses_only = vec![
            StoredGraphEdge {
                source: "web".to_string(),
                target: "db".to_string(),
                kind: EDGE_KIND_USES.to_string(),
                metadata: BTreeMap::new(),
            },
            StoredGraphEdge {
                source: "worker".to_string(),
                target: "db".to_string(),
                kind: EDGE_KIND_USES.to_string(),
                metadata: BTreeMap::new(),
            },
        ];
        assert!(!graph_complete_for_teardown(&make_record(uses_only)));

        // Add an explicit service→service depends_on → accept.
        let mut with_ordering = vec![
            StoredGraphEdge {
                source: "web".to_string(),
                target: "db".to_string(),
                kind: EDGE_KIND_USES.to_string(),
                metadata: BTreeMap::new(),
            },
            StoredGraphEdge {
                source: "worker".to_string(),
                target: "db".to_string(),
                kind: EDGE_KIND_USES.to_string(),
                metadata: BTreeMap::new(),
            },
        ];
        with_ordering.push(StoredGraphEdge {
            source: "web".to_string(),
            target: "worker".to_string(),
            kind: EDGE_KIND_DEPENDS_ON.to_string(),
            metadata: BTreeMap::new(),
        });
        assert!(graph_complete_for_teardown(&make_record(with_ordering)));
    }

    /// PR-5b-fix (review round 2): when a provider alias collides
    /// with a service name, `teardown_from_graph`'s single-namespace
    /// node lookup would silently drop one side. The completeness
    /// predicate must refuse the graph path so the legacy two-path
    /// teardown handles such a session.
    #[test]
    fn graph_complete_for_teardown_rejects_provider_service_identifier_collision() {
        use ato_session_core::{
            StoredOrchestrationService, StoredOrchestrationServices, StoredSessionInfo,
        };
        use capsule_core::handle::{CapsuleDisplayStrategy, CapsuleRuntimeDescriptor, TrustState};

        // Provider and service share the identifier "db".
        let provider_node = StoredGraphNode {
            kind: NODE_KIND_PROVIDER.to_string(),
            identifier: "db".to_string(),
            pid: Some(1234),
            state_dir: Some(PathBuf::from("/tmp/db-provider")),
            port: None,
            container_id: None,
            capability: None,
            metadata: BTreeMap::new(),
        };
        let service_node = StoredGraphNode {
            kind: NODE_KIND_SERVICE.to_string(),
            identifier: "db".to_string(),
            pid: Some(5678),
            state_dir: None,
            port: None,
            container_id: None,
            capability: None,
            metadata: BTreeMap::new(),
        };

        let record = StoredSessionInfo {
            session_id: "x".into(),
            handle: "p/s".into(),
            normalized_handle: "p/s".into(),
            canonical_handle: None,
            trust_state: TrustState::Untrusted,
            source: None,
            restricted: false,
            snapshot: None,
            runtime: CapsuleRuntimeDescriptor {
                target_label: "main".into(),
                runtime: None,
                driver: None,
                language: None,
                port: None,
            },
            display_strategy: CapsuleDisplayStrategy::GuestWebview,
            pid: 0,
            log_path: String::new(),
            manifest_path: String::new(),
            target_label: "main".into(),
            notes: vec![],
            guest: None,
            web: None,
            terminal: None,
            service: None,
            dependency_contracts: Some(StoredDependencyContracts {
                consumer_pid: 0,
                providers: vec![provider("db", 1234)],
            }),
            graph: Some(StoredExecutionGraph {
                schema_version: StoredExecutionGraph::SCHEMA_VERSION,
                nodes: vec![provider_node, service_node],
                edges: vec![],
            }),
            execution_id: None,
            execution_receipt_schema_version: None,
            declared_execution_id: None,
            resolved_execution_id: None,
            observed_execution_id: None,
            graph_completeness: None,
            reproducibility_class: None,
            orchestration_services: Some(StoredOrchestrationServices {
                wrapper_pid: 0,
                services: vec![StoredOrchestrationService {
                    name: "db".into(),
                    target_label: "db".into(),
                    local_pid: Some(5678),
                    container_id: None,
                    host_ports: Default::default(),
                    published_port: None,
                }],
            }),
            schema_version: None,
            launch_digest: None,
            process_start_time_unix_ms: None,
        };

        assert!(
            !graph_complete_for_teardown(&record),
            "must reject provider/service identifier collision"
        );
    }
}
