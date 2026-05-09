//! Named builder for [`FilesystemIdentityV2`].
//!
//! This is a Wave 1 scaffold (refs #74, #100). The builder accepts an
//! [`ExecutionIdentityInputV2`] and returns the filesystem facet that the
//! caller has already populated — a typed pass-through. The body must
//! remain byte-equivalent to reading `input.filesystem` directly so that
//! introducing this entry point does not perturb any
//! `view_hash` already in circulation.
//!
//! ## Why pass-through today
//!
//! The actual observation logic that produces a `FilesystemIdentityV2`
//! (mount enumeration, semantics probing, partial-view-hash calculation)
//! lives in the launch-time observers in `ato-cli`. Lifting that logic
//! into capsule-core requires inputs that capsule-core does not currently
//! see (manifest data, the runtime launch context, the resolved launch
//! spec). Wave 3 (PR-5a, refs #99) will introduce a resolved
//! `ExecutionGraph` that exposes those inputs to capsule-core; at that
//! point this builder's body becomes "consume the graph node, compute the
//! identity facet" and call sites in `ato-cli` switch to invoking it.
//!
//! ## Future graph-input fields (do not add now)
//!
//! When the graph wiring lands, the builder will need access to facts
//! that are not in [`ExecutionIdentityInputV2`] today. Documenting them
//! here so PR-5a knows where to plug them in:
//!
//! - explicit ephemeral-tmp marker (today inferred by
//!   [`crate::execution_identity::TmpPolicy`] but not exposed as a
//!   first-class graph edge)
//! - sidecar / proxy mount source identity (currently folded into
//!   `readonly_layers` as opaque blake3 hashes)
//! - content-class marker for state mounts (today inferred by string
//!   prefix on the locator inside the observer)
//!
//! These are intentionally NOT placeholders on the struct; the v2 schema
//! is frozen and adding them now would change `view_hash` outputs.

use super::{ExecutionIdentityInputV2, FilesystemIdentityV2};

/// Typed entry point for producing a [`FilesystemIdentityV2`].
///
/// See the module-level docs for why this is a pass-through in Wave 1
/// and what its body becomes once the graph-input wiring lands.
pub struct FilesystemIdentityBuilder;

impl FilesystemIdentityBuilder {
    /// Build the filesystem identity facet for the given input.
    ///
    /// Currently a clone of `input.filesystem` — byte-equivalent to the
    /// inline reads in [`super::identity_projection_v2`]. Do not add
    /// transformations here; doing so would change `view_hash` outputs
    /// and break the v2 schema contract.
    pub fn build(input: &ExecutionIdentityInputV2) -> FilesystemIdentityV2 {
        input.filesystem.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::sample_input_v2;
    use super::*;
    use crate::execution_identity::{ReadonlyLayerIdentity, Tracked};

    /// Byte-equivalence: the builder must produce a facet whose
    /// `view_hash` (and full struct contents) match the input exactly.
    /// This is the load-bearing contract of Wave 1 — if it ever fails,
    /// the refactor has silently changed identity output.
    #[test]
    fn builder_output_matches_input_filesystem_facet_exactly() {
        let input = sample_input_v2();
        let built = FilesystemIdentityBuilder::build(&input);
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
}
