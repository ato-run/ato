//! Named builder for [`PolicyIdentityV2`].
//!
//! Originally a Wave 1 typed pass-through (refs #74, #102). PR-5a
//! (#102, #99) wires it to graph-derived inputs where the
//! [`crate::engine::execution_graph::ExecutionGraph`] already exposes
//! enough data; remaining facets continue to read from the V2 input.
//!
//! ## Mixed-source contract
//!
//! When [`PolicyIdentityBuilder::build_with_graph`] is given
//! `Some(graph)`, the three policy hashes come from graph labels under
//! the `policy.*` namespace (see
//! [`crate::engine::execution_graph::identity_labels`]); the V2 input is
//! the fallback when a label is absent.
//!
//! Today's graph-sourced fields:
//!
//! - `network_policy_hash` — from
//!   [`crate::engine::execution_graph::identity_labels::POLICY_NETWORK_HASH`]
//! - `capability_policy_hash` — from
//!   [`crate::engine::execution_graph::identity_labels::POLICY_CAPABILITY_HASH`]
//! - `sandbox_policy_hash` — from
//!   [`crate::engine::execution_graph::identity_labels::POLICY_SANDBOX_HASH`]
//!
//! Byte-equivalence guarantee: when the graph is `None`, OR when graph
//! labels match the V2 input exactly (the case the adapter currently
//! produces), the output is byte-identical to `input.policy.clone()`.
//! This keeps every execution_id in circulation stable across the
//! wiring.
//!
//! ## Future graph-input fields (do not add now)
//!
//! When the graph wiring expands further, the builder will need facts
//! that are not modelled today. Documenting them so follow-on PRs know
//! where to plug them in:
//!
//! - mount-set algorithm id and version (currently rolled into
//!   `sandbox_policy_hash` opaquely)
//! - allow-host count and network-mode marker (also rolled into
//!   `sandbox_policy_hash`; exposing them as graph edges would let the
//!   builder hash them canonically without re-encoding)
//! - consent-ledger segment provenance (today the observer reads
//!   `provisioning_policy_hash` and `policy_segment_hash` directly off
//!   `ExecutionPlan.consent`; the graph should carry that linkage
//!   explicitly)

use super::{ExecutionIdentityInputV2, PolicyIdentityV2, Tracked};
use crate::engine::execution_graph::{identity_labels, ExecutionGraph};

/// Typed entry point for producing a [`PolicyIdentityV2`].
///
/// See the module-level docs for the mixed-source wiring contract.
pub struct PolicyIdentityBuilder;

impl PolicyIdentityBuilder {
    /// Build the policy identity facet for the given input.
    ///
    /// Pass-through entry point preserved for callers that have no
    /// `ExecutionGraph` available. New call sites should prefer
    /// [`Self::build_with_graph`].
    pub fn build(input: &ExecutionIdentityInputV2) -> PolicyIdentityV2 {
        Self::build_with_graph(input, None)
    }

    /// Build the policy identity facet, consuming graph-derived facts
    /// where the graph carries them and falling back to the V2 input
    /// otherwise.
    ///
    /// Determinism contract:
    /// - `graph = None` → byte-equivalent to `input.policy.clone()`.
    /// - `graph = Some(g)` with no relevant labels → also pass-through.
    /// - `graph = Some(g)` with labels that match the V2 input → still
    ///   byte-equivalent.
    pub fn build_with_graph(
        input: &ExecutionIdentityInputV2,
        graph: Option<&ExecutionGraph>,
    ) -> PolicyIdentityV2 {
        let mut facet = input.policy.clone();
        let Some(graph) = graph else {
            return facet;
        };

        if let Some(value) = graph.labels.get(identity_labels::POLICY_NETWORK_HASH) {
            facet.network_policy_hash = Tracked::known(value.clone());
        }
        if let Some(value) = graph.labels.get(identity_labels::POLICY_CAPABILITY_HASH) {
            facet.capability_policy_hash = Tracked::known(value.clone());
        }
        if let Some(value) = graph.labels.get(identity_labels::POLICY_SANDBOX_HASH) {
            facet.sandbox_policy_hash = Tracked::known(value.clone());
        }

        facet
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::sample_input_v2;
    use super::*;
    use crate::engine::execution_graph::{
        ExecutionGraph, ExecutionGraphBuildInput, ExecutionGraphBuilder, GraphPolicyInput,
    };
    use crate::execution_identity::Tracked;

    /// Byte-equivalence (no-graph case): the builder must produce a facet
    /// whose three policy hashes (and tracking statuses) match the input
    /// exactly. This is the load-bearing contract carried over from
    /// Wave 1.
    #[test]
    fn builder_output_matches_input_policy_facet_exactly_without_graph() {
        let input = sample_input_v2();
        let built = PolicyIdentityBuilder::build(&input);
        assert_eq!(built, input.policy);
    }

    /// Byte-equivalence (no labels case): a graph that carries no policy
    /// labels must also degenerate to the pass-through.
    #[test]
    fn builder_output_matches_input_facet_when_graph_has_no_relevant_labels() {
        let input = sample_input_v2();
        let graph = ExecutionGraphBuilder::build(ExecutionGraphBuildInput::default());
        let built = PolicyIdentityBuilder::build_with_graph(&input, Some(&graph));
        assert_eq!(built, input.policy);
    }

    /// Sensitivity preservation: the inline projection hashes all three
    /// policy hash fields into the canonical projection (see
    /// `PolicyProjectionV2` in `identity_projection_v2`). Confirm that
    /// the builder preserves the existing sensitivity — perturbing
    /// `sandbox_policy_hash` changes the resulting execution_id. This
    /// matches the existing
    /// `v2_policy_identity_changes_execution_id` invariant in the
    /// parent module's tests.
    #[test]
    fn perturbing_sandbox_policy_hash_changes_execution_id_through_builder() {
        let input = sample_input_v2();
        let baseline = input.compute_id().expect("baseline").execution_id;

        let mut perturbed = sample_input_v2();
        perturbed.policy = PolicyIdentityBuilder::build(&perturbed);
        perturbed.policy.sandbox_policy_hash = Tracked::known("blake3:sandbox2".to_string());

        let after = perturbed.compute_id().expect("after").execution_id;
        assert_ne!(
            baseline, after,
            "builder must preserve sandbox_policy_hash sensitivity"
        );
    }

    /// Graph-sourced override: when the graph's policy labels disagree
    /// with the V2 input, the builder takes the graph value. The adapter
    /// in production populates labels from the same V2 facts, so the
    /// override is a no-op there; this test pins the wire so a future
    /// "graph is the source of truth" promotion has the entry point
    /// already in place.
    #[test]
    fn builder_takes_policy_hashes_from_graph_labels_when_present() {
        let input = sample_input_v2();
        let graph: ExecutionGraph = ExecutionGraphBuilder::build(ExecutionGraphBuildInput {
            policy: Some(GraphPolicyInput {
                network_policy_hash: Some("blake3:network-graph".to_string()),
                capability_policy_hash: Some("blake3:capability-graph".to_string()),
                sandbox_policy_hash: Some("blake3:sandbox-graph".to_string()),
                ..GraphPolicyInput::default()
            }),
            ..ExecutionGraphBuildInput::default()
        });

        let built = PolicyIdentityBuilder::build_with_graph(&input, Some(&graph));
        assert_eq!(
            built.network_policy_hash,
            Tracked::known("blake3:network-graph".to_string())
        );
        assert_eq!(
            built.capability_policy_hash,
            Tracked::known("blake3:capability-graph".to_string())
        );
        assert_eq!(
            built.sandbox_policy_hash,
            Tracked::known("blake3:sandbox-graph".to_string())
        );
    }

    /// Mirror property: a graph populated from the same facts as the V2
    /// input produces a byte-equivalent facet. This is the production
    /// invariant — the adapter populates policy labels from the same
    /// observers that produce `input.policy`.
    #[test]
    fn graph_populated_from_same_facts_yields_byte_equivalent_facet() {
        let input = sample_input_v2();

        let network = input
            .policy
            .network_policy_hash
            .value
            .clone()
            .expect("sample fixture has a known network_policy_hash");
        let capability = input
            .policy
            .capability_policy_hash
            .value
            .clone()
            .expect("sample fixture has a known capability_policy_hash");
        let sandbox = input
            .policy
            .sandbox_policy_hash
            .value
            .clone()
            .expect("sample fixture has a known sandbox_policy_hash");

        let graph: ExecutionGraph = ExecutionGraphBuilder::build(ExecutionGraphBuildInput {
            policy: Some(GraphPolicyInput {
                network_policy_hash: Some(network),
                capability_policy_hash: Some(capability),
                sandbox_policy_hash: Some(sandbox),
                ..GraphPolicyInput::default()
            }),
            ..ExecutionGraphBuildInput::default()
        });

        let built = PolicyIdentityBuilder::build_with_graph(&input, Some(&graph));
        assert_eq!(built, input.policy);
    }
}
