//! Named builder for [`FilesystemIdentityV2`].
//!
//! Originally a Wave 1 typed pass-through (refs #74, #100). PR-5a (#100,
//! #99) wires it to graph-derived inputs where the
//! [`crate::engine::execution_graph::ExecutionGraph`] already exposes
//! enough data; remaining facets continue to read from the V2 input.
//!
//! ## Mixed-source contract
//!
//! When [`FilesystemIdentityBuilder::build_with_graph`] is given
//! `Some(graph)`, fields with corresponding identity labels (see
//! [`crate::engine::execution_graph::identity_labels`]) are sourced from
//! the graph; everything else falls back to `input.filesystem`. This is a
//! deliberate intermediate state: each subsequent wave moves more facets
//! into the graph until the V2 input no longer carries a filesystem facet
//! at all.
//!
//! Today's graph-sourced fields:
//!
//! - `source_root` — from
//!   [`crate::engine::execution_graph::identity_labels::FS_SOURCE_ROOT`]
//! - `working_directory` — from
//!   [`crate::engine::execution_graph::identity_labels::FS_WORKING_DIRECTORY`]
//! - `view_hash` — from
//!   [`crate::engine::execution_graph::identity_labels::FS_VIEW_HASH`]
//!   (resolved-domain only; absent on the declared graph)
//!
//! Still V2-input-sourced (no graph representation yet):
//!
//! - `partial_view_hash`, `readonly_layers`, `writable_dirs`,
//!   `persistent_state`, `semantics` — these need richer graph node /
//!   edge shapes that #100's follow-on waves will introduce.
//!
//! The byte-equivalence guarantee from Wave 1 is preserved: when the
//! graph is `None`, OR when graph labels match the V2 input exactly
//! (which is the case the adapter currently produces), the output is
//! byte-identical to `input.filesystem.clone()`. This keeps every
//! `view_hash` in circulation stable across the wiring.
//!
//! ## Future graph-input fields (do not add now)
//!
//! When the graph wiring expands further, the builder will need access to
//! facts that are not modelled today. Documenting them so follow-on PRs
//! know where to plug them in:
//!
//! - explicit ephemeral-tmp marker (today inferred by [`super::TmpPolicy`]
//!   but not exposed as a first-class graph edge)
//! - sidecar / proxy mount source identity (currently folded into
//!   `readonly_layers` as opaque blake3 hashes)
//! - content-class marker for state mounts (today inferred by string
//!   prefix on the locator inside the observer)

use super::{ExecutionIdentityInputV2, FilesystemIdentityV2, Tracked};
use crate::engine::execution_graph::{identity_labels, ExecutionGraph};

/// Typed entry point for producing a [`FilesystemIdentityV2`].
///
/// See the module-level docs for the mixed-source wiring contract.
pub struct FilesystemIdentityBuilder;

impl FilesystemIdentityBuilder {
    /// Build the filesystem identity facet for the given input.
    ///
    /// Pass-through entry point preserved for callers that have no
    /// `ExecutionGraph` available (e.g. legacy receipt paths). New call
    /// sites should prefer [`Self::build_with_graph`].
    pub fn build(input: &ExecutionIdentityInputV2) -> FilesystemIdentityV2 {
        Self::build_with_graph(input, None)
    }

    /// Build the filesystem identity facet, consuming graph-derived facts
    /// where the graph carries them and falling back to the V2 input
    /// otherwise.
    ///
    /// Determinism contract:
    /// - `graph = None` → byte-equivalent to `input.filesystem.clone()`.
    /// - `graph = Some(g)` with no relevant labels → also pass-through.
    /// - `graph = Some(g)` with labels that match the V2 input → still
    ///   byte-equivalent (the labels just confirm what the V2 input
    ///   already says).
    pub fn build_with_graph(
        input: &ExecutionIdentityInputV2,
        graph: Option<&ExecutionGraph>,
    ) -> FilesystemIdentityV2 {
        let mut facet = input.filesystem.clone();
        let Some(graph) = graph else {
            return facet;
        };

        if let Some(value) = graph.labels.get(identity_labels::FS_SOURCE_ROOT) {
            facet.source_root = Tracked::known(value.clone());
        }
        if let Some(value) = graph.labels.get(identity_labels::FS_WORKING_DIRECTORY) {
            facet.working_directory = Tracked::known(value.clone());
        }
        if let Some(value) = graph.labels.get(identity_labels::FS_VIEW_HASH) {
            facet.view_hash = Tracked::known(value.clone());
        }

        facet
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::sample_input_v2;
    use super::*;
    use crate::engine::execution_graph::{
        ExecutionGraph, ExecutionGraphBuildInput, ExecutionGraphBuilder, GraphHostInput,
    };
    use crate::execution_identity::{ReadonlyLayerIdentity, Tracked};

    /// Byte-equivalence (no-graph case): the builder must produce a facet
    /// whose `view_hash` (and full struct contents) match the input
    /// exactly. This is the load-bearing contract carried over from
    /// Wave 1 — if it ever fails, the wiring has silently changed
    /// identity output.
    #[test]
    fn builder_output_matches_input_filesystem_facet_exactly_without_graph() {
        let input = sample_input_v2();
        let built = FilesystemIdentityBuilder::build(&input);
        assert_eq!(built, input.filesystem);
    }

    /// Byte-equivalence (no labels case): a graph that carries no
    /// filesystem labels must also degenerate to the pass-through.
    #[test]
    fn builder_output_matches_input_facet_when_graph_has_no_relevant_labels() {
        let input = sample_input_v2();
        let graph = ExecutionGraphBuilder::build(ExecutionGraphBuildInput::default());
        let built = FilesystemIdentityBuilder::build_with_graph(&input, Some(&graph));
        assert_eq!(built, input.filesystem);
    }

    /// Sensitivity preservation: the inline producer in `ato-cli` reacts
    /// to changes in `readonly_layers` (each entry is hashed into the
    /// canonical projection via `FilesystemProjectionV2.readonly_layers`,
    /// see `identity_projection_v2`). Confirm that the builder preserves
    /// that sensitivity — adding a readonly layer changes the resulting
    /// execution_id.
    #[test]
    fn adding_readonly_layer_changes_execution_id_through_builder() {
        let input = sample_input_v2();
        let baseline = input.compute_id().expect("baseline").execution_id;

        let mut perturbed = sample_input_v2();
        perturbed.filesystem = FilesystemIdentityBuilder::build(&perturbed);
        perturbed
            .filesystem
            .readonly_layers
            .push(ReadonlyLayerIdentity {
                role: "deps".to_string(),
                identity: Tracked::known("blake3:deps-projection".to_string()),
            });

        let after = perturbed.compute_id().expect("after").execution_id;
        assert_ne!(
            baseline, after,
            "builder must preserve readonly_layers sensitivity"
        );
    }

    /// Graph-sourced override: when the graph's `FS_VIEW_HASH` label
    /// disagrees with the V2 input, the builder takes the graph value.
    /// This is the wire that a future "graph is the source of truth"
    /// promotion will rely on. Until then it must stay opt-in (the
    /// adapter populates labels from the same V2 facts, so the override
    /// is a no-op in production).
    #[test]
    fn builder_takes_view_hash_from_graph_label_when_present() {
        let input = sample_input_v2();
        let graph = ExecutionGraphBuilder::build(ExecutionGraphBuildInput {
            host: Some(GraphHostInput {
                filesystem_view_hash: Some("blake3:graph-sourced".to_string()),
                ..GraphHostInput::default()
            }),
            ..ExecutionGraphBuildInput::default()
        });

        let built = FilesystemIdentityBuilder::build_with_graph(&input, Some(&graph));
        assert_eq!(
            built.view_hash,
            Tracked::known("blake3:graph-sourced".to_string())
        );
        // Other fields fall through unchanged — proves the wiring is
        // additive, not a wholesale replacement.
        assert_eq!(built.source_root, input.filesystem.source_root);
    }

    /// Mirror property: a graph populated from the same facts as the V2
    /// input produces a byte-equivalent facet. This is the production
    /// invariant — the adapter populates labels from the same observers
    /// that produce `input.filesystem`, so wiring the graph in must be a
    /// no-op for `view_hash` and friends.
    #[test]
    fn graph_populated_from_same_facts_yields_byte_equivalent_facet() {
        let input = sample_input_v2();

        let view_hash_value = input
            .filesystem
            .view_hash
            .value
            .clone()
            .expect("sample fixture has a known view_hash");
        let source_root_value = input
            .filesystem
            .source_root
            .value
            .clone()
            .expect("sample fixture has a known source_root");
        let working_directory_value = input
            .filesystem
            .working_directory
            .value
            .clone()
            .expect("sample fixture has a known working_directory");

        let graph: ExecutionGraph = ExecutionGraphBuilder::build(ExecutionGraphBuildInput {
            host: Some(GraphHostInput {
                filesystem_source_root: Some(source_root_value),
                filesystem_working_directory: Some(working_directory_value),
                filesystem_view_hash: Some(view_hash_value),
                ..GraphHostInput::default()
            }),
            ..ExecutionGraphBuildInput::default()
        });

        let built = FilesystemIdentityBuilder::build_with_graph(&input, Some(&graph));
        assert_eq!(built, input.filesystem);
    }
}
