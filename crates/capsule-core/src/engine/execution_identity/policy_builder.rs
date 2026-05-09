//! Named builder for [`PolicyIdentityV2`].
//!
//! This is a Wave 1 scaffold (refs #74, #102). The builder accepts an
//! [`ExecutionIdentityInputV2`] and returns the policy facet that the
//! caller has already populated — a typed pass-through. The body must
//! remain byte-equivalent to reading `input.policy` directly so that
//! introducing this entry point does not perturb any execution_id
//! already in circulation.
//!
//! ## Why pass-through today
//!
//! The actual policy-hash computation (canonical sandbox-policy hash
//! over runtime / network / mount-set algo facts; provisioning and
//! capability segment hashes from the consent ledger) lives in
//! `ato-cli` alongside the [`ExecutionPlan`]-aware observers. Lifting
//! it requires capsule-core to see the resolved execution plan, which
//! it does not today. Wave 3 (PR-5a, refs #99) will introduce a
//! resolved `ExecutionGraph` that exposes those inputs to capsule-core;
//! at that point this builder's body becomes "consume the graph's
//! policy node, compute the three policy hashes" and call sites in
//! `ato-cli` switch to invoking it.
//!
//! ## Future graph-input fields (do not add now)
//!
//! When the graph wiring lands, the builder will need facts that are
//! not in [`ExecutionIdentityInputV2`] today. Documenting them here so
//! PR-5a knows where to plug them in:
//!
//! - mount-set algorithm id and version (currently rolled into
//!   `sandbox_policy_hash` opaquely by the observer)
//! - allow-host count and network-mode marker (also rolled into
//!   `sandbox_policy_hash`; exposing them as graph edges would let the
//!   builder hash them canonically without re-encoding)
//! - consent-ledger segment provenance (today the observer reads
//!   `provisioning_policy_hash` and `policy_segment_hash` directly off
//!   `ExecutionPlan.consent`; the graph should carry that linkage
//!   explicitly)
//!
//! These are intentionally NOT placeholders on the struct; the v2
//! schema is frozen and adding them now would change execution_id
//! outputs.

use super::{ExecutionIdentityInputV2, PolicyIdentityV2};

/// Typed entry point for producing a [`PolicyIdentityV2`].
///
/// See the module-level docs for why this is a pass-through in Wave 1
/// and what its body becomes once the graph-input wiring lands.
pub struct PolicyIdentityBuilder;

impl PolicyIdentityBuilder {
    /// Build the policy identity facet for the given input.
    ///
    /// Currently a clone of `input.policy` — byte-equivalent to the
    /// inline reads in [`super::identity_projection_v2`]. Do not add
    /// transformations here; doing so would change execution_id outputs
    /// and break the v2 schema contract.
    pub fn build(input: &ExecutionIdentityInputV2) -> PolicyIdentityV2 {
        input.policy.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::sample_input_v2;
    use super::*;
    use crate::execution_identity::Tracked;

    /// Byte-equivalence: the builder must produce a facet whose three
    /// policy hashes (and tracking statuses) match the input exactly.
    /// This is the load-bearing contract of Wave 1.
    #[test]
    fn builder_output_matches_input_policy_facet_exactly() {
        let input = sample_input_v2();
        let built = PolicyIdentityBuilder::build(&input);
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
}
