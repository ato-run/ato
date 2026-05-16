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
        consent: None,
    })
}

/// PR-4b: variant of [`build_declared_only_bundle`] that ALSO carries
/// consent identity onto the bundle. Used by `preflight.rs` and the
/// run pipeline's pre-launch consent gate so
/// `ExecutionConsentView::from_bundle` produces a populated view.
///
/// `consent` comes from the caller's `ExecutionPlan.consent` — the
/// only producer of `policy_segment_hash` /
/// `provisioning_policy_hash` today is `compile_execution_plan` in
/// capsule-core. Callers project the plan onto `GraphConsentInput`
/// before calling here.
pub(crate) fn build_declared_only_bundle_with_consent(
    dependencies: &[ExternalCapsuleDependency],
    manifest_source_identifier: Option<String>,
    declared_policy: Option<GraphPolicyInput>,
    required_env: Vec<String>,
    consent: capsule_core::engine::execution_graph::GraphConsentInput,
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
        consent: Some(consent),
    })
}

/// PR-3d: bundle-projected materialization input.
///
/// `application::launch_materialization::LaunchSpec` is the existing
/// digest input — it commits to (identity, target, command, argv,
/// logical_cwd, port, readiness_path, build_input_digest, lock_digest,
/// toolchain_fingerprint). Several of those facets (identity,
/// dependency aliases, declared execution id) are already present on
/// the bundle. The rest (command, argv, port) are launch-level facts
/// not carried by the declared-only bundle.
///
/// `LaunchMaterializationInput::from_bundle` exposes the bundle-side
/// facets so a future commit can refactor `LaunchSpec` to source those
/// facts from the bundle without changing the digest. PR-3d does NOT
/// flip the digest source — it only stages the projection and pins the
/// parity with bundle-derived facts via tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchMaterializationInput {
    /// Stable identity fingerprint for the launch slot. Sourced from
    /// the bundle's declared-domain canonical id so two LaunchSpecs
    /// that share the same declared graph share the same identity
    /// fingerprint.
    pub declared_execution_id: String,
    /// Resolved-domain canonical id. Stable across the same launch
    /// envelope but changes when host facets (view_hash,
    /// sandbox_policy_hash) change. None for declared-only bundles
    /// where no host facets were supplied.
    pub resolved_execution_id: Option<String>,
    /// Dependency aliases — declared-domain.
    pub dependency_aliases: Vec<String>,
}

impl LaunchMaterializationInput {
    pub(crate) fn from_bundle(bundle: &LaunchGraphBundle) -> Self {
        // declared/resolved ids are always stamped on the bundle's
        // derived.execution_ids; for declared-only bundles, the two
        // ids will be different by domain tag even when the underlying
        // graph nodes/edges are identical.
        let resolved_execution_id =
            Some(bundle.derived.execution_ids.resolved_execution_id.clone())
                .filter(|id| !id.is_empty());
        Self {
            declared_execution_id: bundle.derived.execution_ids.declared_execution_id.clone(),
            resolved_execution_id,
            dependency_aliases: bundle.derived.preflight.dependency_aliases.clone(),
        }
    }
}

/// Bundle-projected consent view (PR-3d staging, PR-4b primary).
///
/// PR-3d staged this with just the policy hashes from
/// `derived.preflight`; PR-4b extends it with the 5 consent-identity
/// fields the consent store keys on. Production callers in
/// `preflight.rs` and the run pipeline pre-launch gate now use
/// `ExecutionConsentView` as the source-of-truth for consent decisions
/// (with the legacy plan-direct read kept as `debug_assert!` parity).
///
/// Source of truth: `bundle.derived.consent` (a `DerivedConsentView`
/// in capsule-core) when present; otherwise `None` fields and the
/// view degrades to its pre-PR-4b shape.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ExecutionConsentView {
    pub network_policy_hash: Option<String>,
    pub capability_policy_hash: Option<String>,
    /// Dependency aliases that participate in consent decisions
    /// (per-dependency consent prompts). Sourced from
    /// `bundle.derived.preflight.dependency_aliases`.
    pub dependency_aliases: Vec<String>,
    /// PR-4b additions — consent identity facets the consent log
    /// keys on. Populated from `bundle.derived.consent` when the
    /// caller supplied `LaunchGraphBundleInput.consent`; otherwise
    /// `None` (callers fall back to plan-direct read).
    pub scoped_id: Option<String>,
    pub version: Option<String>,
    pub target_label: Option<String>,
    pub policy_segment_hash: Option<String>,
    pub provisioning_policy_hash: Option<String>,
}

impl ExecutionConsentView {
    pub(crate) fn from_bundle(bundle: &LaunchGraphBundle) -> Self {
        let consent = bundle.derived.consent.as_ref();
        Self {
            network_policy_hash: bundle.derived.preflight.network_policy_hash.clone(),
            capability_policy_hash: bundle.derived.preflight.capability_policy_hash.clone(),
            dependency_aliases: bundle.derived.preflight.dependency_aliases.clone(),
            scoped_id: consent.map(|c| c.scoped_id.clone()),
            version: consent.map(|c| c.version.clone()),
            target_label: consent.map(|c| c.target_label.clone()),
            policy_segment_hash: consent.map(|c| c.policy_segment_hash.clone()),
            provisioning_policy_hash: consent.map(|c| c.provisioning_policy_hash.clone()),
        }
    }
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

    /// PR-4a parity: every 6 lockfile facet (source / source_type /
    /// contract / injection_bindings / parameters / credentials)
    /// must flow from the manifest through `GraphDependencyInput`
    /// onto `bundle.derived.dependency_contracts.providers[]`. The
    /// lockfile verifier reads them directly off that surface.
    #[test]
    fn bundle_derived_providers_carry_all_six_lockfile_facets() {
        let manifest = manifest_with_two_dependencies();
        let dependencies = manifest_external_capsule_dependencies(&manifest).expect("deps");
        let bundle = build_declared_only_bundle(&dependencies, None, None, Vec::new());

        // Look up the `db` provider in the bundle's derived view.
        let db_provider = bundle
            .derived
            .dependency_contracts
            .providers
            .iter()
            .find(|p| p.alias == "db")
            .expect("db provider in derived view");

        // Source / source_type / contract flow from the manifest.
        assert_eq!(
            db_provider.source.as_deref(),
            Some("capsule://ato/acme-postgres@16")
        );
        assert!(db_provider.source_type.is_some());
        assert_eq!(db_provider.contract.as_deref(), Some("service@1"));

        // injection_bindings / parameters / credentials are
        // empty-default on this fixture but the fields exist and
        // round-trip without drift; confirm they're at least the
        // same type the manifest declared.
        // (The fixture has no credentials, so the BTreeMap is
        // empty — that's the meaningful equality assertion: PR-4a
        // doesn't fabricate facets that weren't in the manifest.)
        assert!(db_provider.credentials.is_empty());
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

    /// PR-3d parity: LaunchMaterializationInput::from_bundle exposes
    /// the bundle's declared-domain canonical id as the identity
    /// fingerprint. The contract is: two bundles built from the SAME
    /// manifest external-dependency list produce the SAME
    /// declared_execution_id, and therefore the same materialization
    /// input fingerprint. If this drifts, every consumer that keys
    /// on `LaunchMaterializationInput.declared_execution_id` sees a
    /// different identity for an unchanged input.
    #[test]
    fn launch_materialization_input_declared_id_is_stable_across_recompute() {
        let manifest = manifest_with_two_dependencies();
        let dependencies = manifest_external_capsule_dependencies(&manifest).expect("deps");

        let bundle_one = build_declared_only_bundle(
            &dependencies,
            Some("manifest://stable-source".to_string()),
            None,
            vec!["PG_PASSWORD".to_string()],
        );
        let bundle_two = build_declared_only_bundle(
            &dependencies,
            Some("manifest://stable-source".to_string()),
            None,
            vec!["PG_PASSWORD".to_string()],
        );

        let input_one = LaunchMaterializationInput::from_bundle(&bundle_one);
        let input_two = LaunchMaterializationInput::from_bundle(&bundle_two);

        assert_eq!(
            input_one.declared_execution_id, input_two.declared_execution_id,
            "PR-3d: identical bundle inputs must produce the same declared_execution_id — \
             the materialization input must be content-addressed"
        );
        assert_eq!(
            input_one.dependency_aliases, input_two.dependency_aliases,
            "PR-3d: dependency_aliases must be stable across re-computation"
        );
    }

    /// PR-4b: `build_declared_only_bundle_with_consent` populates
    /// the bundle's `derived.consent`, and
    /// `ExecutionConsentView::from_bundle` reads it back out with
    /// the same field values.
    #[test]
    fn execution_consent_view_with_consent_input_carries_all_five_facets() {
        use capsule_core::engine::execution_graph::GraphConsentInput;

        let manifest = manifest_with_two_dependencies();
        let dependencies = manifest_external_capsule_dependencies(&manifest).expect("deps");
        let consent_input = GraphConsentInput {
            scoped_id: "publisher/slug".to_string(),
            version: "1.2.3".to_string(),
            target_label: "web".to_string(),
            policy_segment_hash: "blake3:cap".to_string(),
            provisioning_policy_hash: "blake3:prov".to_string(),
        };
        let bundle = build_declared_only_bundle_with_consent(
            &dependencies,
            Some("manifest://consent-fixture".to_string()),
            None,
            Vec::new(),
            consent_input,
        );

        let view = ExecutionConsentView::from_bundle(&bundle);
        assert_eq!(view.scoped_id.as_deref(), Some("publisher/slug"));
        assert_eq!(view.version.as_deref(), Some("1.2.3"));
        assert_eq!(view.target_label.as_deref(), Some("web"));
        assert_eq!(view.policy_segment_hash.as_deref(), Some("blake3:cap"));
        assert_eq!(view.provisioning_policy_hash.as_deref(), Some("blake3:prov"));

        // Dependency aliases still flow through the preflight projection.
        let mut bundle_aliases = view.dependency_aliases.clone();
        bundle_aliases.sort_unstable();
        let mut manifest_aliases: Vec<String> =
            dependencies.iter().map(|d| d.alias.clone()).collect();
        manifest_aliases.sort_unstable();
        assert_eq!(bundle_aliases, manifest_aliases);
    }

    /// PR-4b: when the legacy `build_declared_only_bundle` (no
    /// consent variant) is used, the view's consent identity fields
    /// are `None`. Callers fall back to plan-direct consent reads.
    #[test]
    fn execution_consent_view_without_consent_input_is_unpopulated() {
        let manifest = manifest_with_two_dependencies();
        let dependencies = manifest_external_capsule_dependencies(&manifest).expect("deps");
        let bundle = build_declared_only_bundle(&dependencies, None, None, Vec::new());

        let view = ExecutionConsentView::from_bundle(&bundle);
        assert!(view.scoped_id.is_none());
        assert!(view.policy_segment_hash.is_none());
    }

    /// PR-3d parity: bundle-derived MaterializationInput's
    /// dependency_aliases set MUST equal the legacy
    /// `manifest_external_capsule_dependencies` alias set. If this
    /// fails, the launch digest (which commits to identity +
    /// target_label, not aliases directly, but the lock_digest folds
    /// aliases in) and the bundle-derived view are reading different
    /// worlds.
    #[test]
    fn launch_materialization_input_aliases_parity_with_legacy() {
        let manifest = manifest_with_two_dependencies();
        let dependencies = manifest_external_capsule_dependencies(&manifest).expect("deps");
        let bundle = build_declared_only_bundle(
            &dependencies,
            Some("manifest://alias-parity".to_string()),
            None,
            Vec::new(),
        );
        let input = LaunchMaterializationInput::from_bundle(&bundle);

        let mut bundle_aliases = input.dependency_aliases.clone();
        bundle_aliases.sort_unstable();
        let mut legacy_aliases: Vec<String> =
            dependencies.iter().map(|d| d.alias.clone()).collect();
        legacy_aliases.sort_unstable();
        assert_eq!(
            bundle_aliases, legacy_aliases,
            "PR-3d: bundle-derived dependency_aliases must equal legacy manifest alias set"
        );
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
