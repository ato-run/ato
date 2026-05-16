//! PR-3c: bundle-derived view adapters for validate / preflight.
//!
//! Both `ato validate` and the `application::preflight` collector
//! historically built their gating input by reading the manifest's
//! `[dependencies.<alias>]` table (`manifest_external_capsule_dependencies`)
//! and stitching together per-target consent / required_env facts
//! directly. The umbrella v0.6.0 plan (refs #74) flips this so the
//! source of truth is `LaunchGraphBundle.derived.*` — a single
//! projection from the graph — and the legacy derivations are demoted
//! to `debug_assert!` parity guards.
//!
//! This module is the adapter layer: it turns a `LaunchGraphBundle`
//! into the shapes validate.rs / preflight.rs read on the gating path.
//! The bundle itself is built upstream (manifest + lock + policy facts);
//! these views are pure projections.

use capsule_core::engine::execution_graph::{LaunchGraphBundle, LaunchGraphBundleInput};
use capsule_core::engine::execution_graph::{
    ExecutionGraphBuilder, GraphPolicyInput, GraphPreflightInput, GraphSourceInput,
};
use capsule_core::types::ExternalCapsuleDependency;

use crate::application::execution_graph_adapter::build_input_from_external_dependencies;

/// Provider-alias view of a [`LaunchGraphBundle`]. Mirrors the shape
/// validate.rs / session_graph_populate.rs treat as the dependency
/// contract surface: a list of `(alias, provider_identifier,
/// output_identifier)` triples in stable order.
///
/// Pure projection over `bundle.derived.dependency_contracts`. Equivalent
/// to the receipt builder's "what providers fed the resolved-domain
/// graph" question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DependencyContracts {
    pub providers: Vec<DependencyContractProvider>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DependencyContractProvider {
    pub alias: String,
    pub provider_identifier: String,
    pub output_identifier: String,
}

impl DependencyContracts {
    /// PR-3c primary entry point: project a launch graph bundle into the
    /// dependency-contract view validate.rs and preflight.rs gate on.
    pub(crate) fn from_bundle(bundle: &LaunchGraphBundle) -> Self {
        Self {
            providers: bundle
                .derived
                .dependency_contracts
                .providers
                .iter()
                .map(|provider| DependencyContractProvider {
                    alias: provider.alias.clone(),
                    provider_identifier: provider.provider_identifier.clone(),
                    output_identifier: provider.output_identifier.clone(),
                })
                .collect(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.providers.len()
    }
}

/// Preflight view of a [`LaunchGraphBundle`]. Carries the facts the
/// aggregate preflight collector and the per-target gating logic need:
/// declared dependency aliases, required env keys, and the policy hashes
/// the consent layer keys on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PreflightView {
    pub required_env: Vec<String>,
    pub dependency_aliases: Vec<String>,
    pub sandbox_constraints: Vec<String>,
    pub runtime_constraints: Vec<String>,
    pub network_policy_hash: Option<String>,
    pub capability_policy_hash: Option<String>,
}

impl PreflightView {
    /// PR-3c primary entry point: project a launch graph bundle into the
    /// preflight collector's gating view.
    pub(crate) fn from_bundle(bundle: &LaunchGraphBundle) -> Self {
        let derived = &bundle.derived.preflight;
        Self {
            required_env: derived.required_env.clone(),
            dependency_aliases: derived.dependency_aliases.clone(),
            sandbox_constraints: derived.sandbox_constraints.clone(),
            runtime_constraints: derived.runtime_constraints.clone(),
            network_policy_hash: derived.network_policy_hash.clone(),
            capability_policy_hash: derived.capability_policy_hash.clone(),
        }
    }
}

/// Construct a *declared-domain-only* `LaunchGraphBundle` from a
/// manifest's external-capsule-dependency list. No host facets (no
/// `filesystem_view_hash`, no resolved sandbox policy) — used by
/// validate.rs / preflight.rs where the gating runs BEFORE the launch
/// observers populate those resolved facts.
///
/// The resulting bundle's declared graph is shape-equivalent to what
/// `execution_receipt_builder::build_launch_graph_bundle` builds when
/// called with empty host facets; the resolved graph degenerates to the
/// declared graph (no resolved-only facts present), so the canonical
/// `execution_id`s are stable across this code path and the receipt
/// path for the same manifest input.
pub(crate) fn build_declared_only_bundle(
    dependencies: &[ExternalCapsuleDependency],
    manifest_source_identifier: Option<String>,
    declared_policy: Option<GraphPolicyInput>,
    required_env: Vec<String>,
) -> LaunchGraphBundle {
    let base = build_input_from_external_dependencies(dependencies, manifest_source_identifier);

    let preflight = GraphPreflightInput {
        dependency_aliases: dependencies
            .iter()
            .map(|dependency| dependency.alias.clone())
            .collect(),
        required_env,
        network_policy_hash: declared_policy
            .as_ref()
            .and_then(|policy| policy.network_policy_hash.clone()),
        capability_policy_hash: declared_policy
            .as_ref()
            .and_then(|policy| policy.capability_policy_hash.clone()),
        ..GraphPreflightInput::default()
    };

    ExecutionGraphBuilder::build_launch_bundle(LaunchGraphBundleInput {
        source: base.source.or_else(|| {
            Some(GraphSourceInput {
                identifier: "manifest://declared-only".to_string(),
            })
        }),
        targets: base.targets,
        dependencies: base.dependencies,
        declared_host: None,
        resolved_host: None,
        declared_policy,
        resolved_policy: None,
        materialized: Default::default(),
        preflight,
        receipt: Default::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_core::lockfile::manifest_external_capsule_dependencies;

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

[dependencies.cache]
capsule = "capsule://ato/acme-redis@7"
contract = "service@1"
"#,
        )
        .expect("parse manifest")
    }

    #[test]
    fn dependency_contracts_from_bundle_lists_all_providers() {
        let manifest = manifest_with_two_dependencies();
        let dependencies = manifest_external_capsule_dependencies(&manifest).expect("deps");
        let bundle = build_declared_only_bundle(&dependencies, None, None, Vec::new());

        let contracts = DependencyContracts::from_bundle(&bundle);
        let mut aliases: Vec<&str> = contracts
            .providers
            .iter()
            .map(|p| p.alias.as_str())
            .collect();
        aliases.sort_unstable();
        assert_eq!(aliases, vec!["cache", "db"]);
    }

    #[test]
    fn dependency_contracts_parity_with_legacy_alias_set() {
        let manifest = manifest_with_two_dependencies();
        let dependencies = manifest_external_capsule_dependencies(&manifest).expect("deps");
        let bundle = build_declared_only_bundle(&dependencies, None, None, Vec::new());
        let contracts = DependencyContracts::from_bundle(&bundle);

        let mut legacy: Vec<&str> = dependencies.iter().map(|d| d.alias.as_str()).collect();
        legacy.sort_unstable();
        let mut graph: Vec<&str> = contracts
            .providers
            .iter()
            .map(|p| p.alias.as_str())
            .collect();
        graph.sort_unstable();
        assert_eq!(
            legacy, graph,
            "PR-3c parity: bundle-derived providers must equal legacy alias set"
        );
    }

    #[test]
    fn preflight_view_from_bundle_carries_required_env() {
        let manifest = manifest_with_two_dependencies();
        let dependencies = manifest_external_capsule_dependencies(&manifest).expect("deps");
        let bundle = build_declared_only_bundle(
            &dependencies,
            None,
            None,
            vec!["PG_PASSWORD".to_string(), "API_TOKEN".to_string()],
        );

        let view = PreflightView::from_bundle(&bundle);
        // PreflightInput dedup-sorts; both fields should round-trip.
        assert!(view.required_env.contains(&"PG_PASSWORD".to_string()));
        assert!(view.required_env.contains(&"API_TOKEN".to_string()));
        let mut aliases = view.dependency_aliases.clone();
        aliases.sort_unstable();
        assert_eq!(aliases, vec!["cache".to_string(), "db".to_string()]);
    }

    #[test]
    fn declared_only_bundle_has_no_host_facets_in_canonical_form() {
        // The whole point of declared-only is: resolved digest must
        // equal declared digest when no host facets are supplied, so
        // validate / preflight don't see drift from the receipt path
        // when host observation hasn't happened yet.
        let manifest = manifest_with_two_dependencies();
        let dependencies = manifest_external_capsule_dependencies(&manifest).expect("deps");
        let bundle = build_declared_only_bundle(&dependencies, None, None, Vec::new());

        // Domain-tagged digests differ by construction (the canonical
        // form's domain field is mixed in), but the underlying node /
        // edge set is the same — the resolved graph is structurally
        // the declared graph with no host facets added.
        assert_eq!(
            bundle.declared_graph.nodes.len(),
            bundle.resolved_graph.nodes.len(),
            "declared-only bundle: declared and resolved graphs must have the same node count"
        );
        assert_eq!(
            bundle.declared_graph.edges.len(),
            bundle.resolved_graph.edges.len(),
            "declared-only bundle: declared and resolved graphs must have the same edge count"
        );
    }
}
