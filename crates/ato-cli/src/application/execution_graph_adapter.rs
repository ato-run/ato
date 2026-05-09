//! Real-types → [`ExecutionGraphBuildInput`] adapter.
//!
//! Wave 2 / PR-4a of the v0.6.0 graph-based core migration tracked by
//! ato-run/ato#74 and partially addresses ato-run/ato#97.
//!
//! This adapter is intentionally pure: it takes a manifest TOML value (and
//! optionally an [`AtoLock`]) and produces an
//! [`ExecutionGraphBuildInput`] suitable for
//! [`ExecutionGraphBuilder::build`][builder]. It does **no** I/O, holds **no**
//! runtime state, and never spawns providers — it is the static counterpart
//! to the real `dependency_contracts` derivation.
//!
//! The adapter is *not* yet load-bearing. PR-4a builds the graph alongside
//! the existing `manifest_external_capsule_dependencies` derivation in the
//! `validate` command and asserts equivalence in tests. The legacy
//! derivation remains the source of truth; the graph is computed for
//! shape-parity observation only.
//!
//! [builder]: capsule_core::engine::execution_graph::ExecutionGraphBuilder

#[cfg(test)]
use anyhow::Result;

use capsule_core::engine::execution_graph::{
    ExecutionGraphBuildInput, GraphDependencyInput, GraphSourceInput,
};
#[cfg(test)]
use capsule_core::lockfile::manifest_external_capsule_dependencies;
use capsule_core::types::ExternalCapsuleDependency;

/// Convention: provider node identifier for a top-level
/// `[dependencies.<alias>]` entry. Kept stable so equivalence tests can
/// pin the surface.
pub(crate) fn provider_identifier_for_alias(alias: &str) -> String {
    format!("provider://{alias}")
}

/// Convention: dependency-output node identifier for a top-level
/// `[dependencies.<alias>]` entry. Mirrors the provider identifier so the
/// builder's `Provides`/`MaterializesTo` edges land on a stable surface.
pub(crate) fn output_identifier_for_alias(alias: &str) -> String {
    format!("output://{alias}")
}

/// Build an [`ExecutionGraphBuildInput`] from a raw manifest TOML value.
///
/// The current shape is intentionally narrow: the adapter only emits the
/// dependency-side facets that the legacy `manifest_external_capsule_dependencies`
/// derivation produces. Targets, host facets, and policy constraints are
/// left at their defaults; later waves will populate them as additional
/// call sites migrate.
///
/// `source_identifier` is an opaque label used for the graph's `Source`
/// node — call sites pass something stable per call (e.g. the manifest
/// path display string). It is *not* canonicalised here.
///
/// Currently exposed only to the in-module equivalence tests; once a real
/// caller migrates to the manifest-shaped entry point in PR-4b, the
/// `cfg(test)` gate will drop.
#[cfg(test)]
pub(crate) fn build_input_from_manifest(
    manifest_raw: &toml::Value,
    source_identifier: Option<String>,
) -> Result<ExecutionGraphBuildInput> {
    let dependencies = manifest_external_capsule_dependencies(manifest_raw)?;
    Ok(build_input_from_external_dependencies(
        &dependencies,
        source_identifier,
    ))
}

/// Build an [`ExecutionGraphBuildInput`] directly from a precomputed list
/// of [`ExternalCapsuleDependency`].
///
/// Useful when the caller has already paid for the
/// `manifest_external_capsule_dependencies` evaluation and wants to feed
/// the same vector into both the legacy check and the graph builder
/// without re-deriving.
pub(crate) fn build_input_from_external_dependencies(
    dependencies: &[ExternalCapsuleDependency],
    source_identifier: Option<String>,
) -> ExecutionGraphBuildInput {
    let graph_dependencies = dependencies
        .iter()
        .map(|dependency| GraphDependencyInput {
            provider: provider_identifier_for_alias(&dependency.alias),
            output: output_identifier_for_alias(&dependency.alias),
        })
        .collect::<Vec<_>>();

    ExecutionGraphBuildInput {
        source: source_identifier.map(|identifier| GraphSourceInput { identifier }),
        targets: Vec::new(),
        dependencies: graph_dependencies,
        host: None,
        policy: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::engine::execution_graph::{ExecutionGraphBuilder, ExecutionGraphNode};

    fn manifest_with_two_dependencies() -> toml::Value {
        toml::from_str(
            r#"
schema_version = "0.3"
name = "consumer"
version = "0.1.0"
type = "app"
runtime = "source/python"
run = "main.py"

[dependencies.db]
capsule = "capsule://ato/acme-postgres@16"
contract = "service@1"

  [dependencies.db.parameters]
  database = "appdb"

[dependencies.cache]
capsule = "capsule://ato/acme-redis@7"
contract = "service@1"
"#,
        )
        .expect("parse manifest")
    }

    #[test]
    fn provider_node_set_matches_legacy_dependency_contract_derivation() {
        // The load-bearing equivalence test for PR-4a: the
        // (real types → adapter → builder) flow must produce the same
        // *provider-node set* as the legacy
        // `manifest_external_capsule_dependencies` derivation.

        let manifest = manifest_with_two_dependencies();

        // Legacy derivation, treated as the source of truth.
        let legacy_dependencies = manifest_external_capsule_dependencies(&manifest)
            .expect("legacy dependency derivation");
        let mut legacy_aliases: Vec<&str> = legacy_dependencies
            .iter()
            .map(|dependency| dependency.alias.as_str())
            .collect();
        legacy_aliases.sort_unstable();

        // New flow: adapter → builder → graph.
        let input = build_input_from_manifest(&manifest, None).expect("build input from manifest");
        let graph = ExecutionGraphBuilder::build(input);

        let mut graph_provider_aliases: Vec<String> = graph
            .nodes
            .iter()
            .filter_map(|node| match node {
                ExecutionGraphNode::Provider { identifier } => Some(
                    identifier
                        .strip_prefix("provider://")
                        .unwrap_or(identifier.as_str())
                        .to_string(),
                ),
                _ => None,
            })
            .collect();
        graph_provider_aliases.sort();

        let graph_provider_aliases_str: Vec<&str> = graph_provider_aliases
            .iter()
            .map(String::as_str)
            .collect();

        assert_eq!(graph_provider_aliases_str, legacy_aliases);
        assert_eq!(graph_provider_aliases.len(), legacy_dependencies.len());
    }

    #[test]
    fn manifest_without_dependencies_yields_empty_provider_set() {
        let manifest: toml::Value = toml::from_str(
            r#"
schema_version = "0.3"
name = "consumer"
version = "0.1.0"
type = "app"
runtime = "source/python"
run = "main.py"
"#,
        )
        .expect("parse manifest");

        let legacy_dependencies =
            manifest_external_capsule_dependencies(&manifest).expect("legacy derivation");
        assert!(legacy_dependencies.is_empty());

        let input = build_input_from_manifest(&manifest, None).expect("build input");
        let graph = ExecutionGraphBuilder::build(input);

        let provider_count = graph
            .nodes
            .iter()
            .filter(|node| matches!(node, ExecutionGraphNode::Provider { .. }))
            .count();
        assert_eq!(provider_count, 0);
    }

    #[test]
    fn source_identifier_propagates_into_source_node() {
        let manifest = manifest_with_two_dependencies();
        let input = build_input_from_manifest(&manifest, Some("manifest://test".to_string()))
            .expect("build input");
        let graph = ExecutionGraphBuilder::build(input);

        let source_identifiers: Vec<&str> = graph
            .nodes
            .iter()
            .filter_map(|node| match node {
                ExecutionGraphNode::Source { identifier } => Some(identifier.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(source_identifiers, vec!["manifest://test"]);
    }
}
