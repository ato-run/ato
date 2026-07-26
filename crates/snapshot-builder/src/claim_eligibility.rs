//! Eligibility for a running capture, from the contract the control plane pinned.
//!
//! # What this proves, and what it deliberately does not
//!
//! [`HoldPhase`](crate::hold_phase::HoldPhase) refuses to enter a hold without a
//! [`VerifiedRunningSnapshotEligibility`]. That proof answers ONE question
//! (#1090, RFC §8.3): *may this workload be captured while it is running, or does
//! it need `workload_idle`?* A workload that requires External State or
//! restore-time secret bindings must not be captured live, because its bindings
//! would be sealed into bytes many users restore.
//!
//! That question is answered by the **declared** Execution Contract. So this
//! mints the proof from the contract the control plane pinned on the claim, after
//! checking that the contract and the `execution_id` beside it agree.
//!
//! **It is NOT a proof that the build matches the contract.** That is
//! finalization (RFC §4.6) — recomputing every identity-bearing digest from the
//! concrete build — and it is a different guarantee about a different thing: what
//! gets *sealed*, rather than whether a live capture is permitted. Finalization
//! cannot run today anyway: `source.projection_digest` commits a payload whose
//! schema the contract module explicitly defers ("payload schemas are defined in
//! a later PR"), so `ExecutionObservationV1::finalize` stops at that facet. Making
//! the wizard wait for it would hold the lane behind an unwritten schema while
//! imposing a bar the auto-seal build path does not meet either — that path
//! finalizes when it can and otherwise proceeds with the legacy seal.
//!
//! Naming follows that boundary on purpose: this is `ClaimContractEligibility`,
//! not "verified build eligibility".
//!
//! # Fail-closed
//!
//! Every uncertainty is a refusal, never a downgrade:
//!
//! * no contract pinned on the claim ⇒ refuse (a hold cannot self-declare);
//! * contract present but no `execution_id` ⇒ refuse (nothing to check it against);
//! * the two disagree ⇒ refuse (`verified_execution_id` recomputes the canonical
//!   hash, so a tampered or stale pair cannot pass);
//! * the contract requires External State for a live workload ⇒ refuse with
//!   [`AcceptanceFailure::ExternalStateRequiresWorkloadIdle`], never a running
//!   capture.

use capsule::execution_contract::{ExecutionContractEnvelopeV1, ExecutionContractV1, ExecutionId};
use snapshot::acceptance::{AcceptanceFailure, VerifiedRunningSnapshotEligibility};

use crate::hold_phase::EligibilitySource;

/// Mints eligibility from the claim's pinned contract. See the module doc for
/// exactly which guarantee this is.
pub struct ClaimContractEligibility {
    envelope: Option<ExecutionContractEnvelopeV1>,
}

impl ClaimContractEligibility {
    /// Build from the identity fields of a claimed job.
    ///
    /// `None` for either half yields a source that refuses — the refusal is
    /// deferred to [`EligibilitySource::eligibility`] so the caller does not have
    /// to duplicate the fail-closed decision.
    pub fn from_claim(contract: Option<&ExecutionContractV1>, execution_id: Option<&str>) -> Self {
        let envelope = match (contract, execution_id) {
            // `ExecutionId::new` validates the `blake3:<hex>` shape. A malformed
            // id yields `None` here, i.e. the same refusal as no id at all —
            // never a coerced value that could then be "verified" against a
            // contract it does not name.
            (Some(contract), Some(id)) => {
                ExecutionId::new(id.to_string()).ok().map(|execution_id| {
                    ExecutionContractEnvelopeV1 {
                        execution_contract: contract.clone(),
                        execution_id,
                        capsule_program_id: None,
                        resolved_refs: Default::default(),
                        generated_at: None,
                        provenance: serde_json::Value::Null,
                        diagnostics: serde_json::Value::Null,
                        evidence: serde_json::Value::Null,
                    }
                })
            }
            _ => None,
        };
        Self { envelope }
    }
}

impl EligibilitySource for ClaimContractEligibility {
    fn eligibility(&mut self) -> Result<VerifiedRunningSnapshotEligibility, AcceptanceFailure> {
        let Some(envelope) = self.envelope.as_ref() else {
            // A hold whose job carries no pinned contract cannot be judged
            // eligible by anything the builder can see. Refuse rather than
            // assume the workload needs no External State.
            return Err(AcceptanceFailure::ExternalStateRequiresWorkloadIdle);
        };
        // Recomputes the canonical hash and matches it against the stored id, then
        // analyzes the SAME verified contract's restore-time-binding requirement.
        VerifiedRunningSnapshotEligibility::analyze_execution_contract(envelope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No pinned contract ⇒ refuse.
    ///
    /// The dangerous alternative is treating "nothing declared" as "nothing
    /// required" and capturing a live workload whose bindings then land in bytes
    /// many users restore. Absence of evidence is not evidence of absence.
    #[test]
    fn a_claim_with_no_contract_is_refused() {
        let mut e = ClaimContractEligibility::from_claim(None, None);
        assert!(matches!(
            e.eligibility(),
            Err(AcceptanceFailure::ExternalStateRequiresWorkloadIdle)
        ));
    }

    /// A well-formed id with NO contract beside it ⇒ still refuse.
    ///
    /// An id alone names a contract this builder cannot see, so there is nothing
    /// to analyze for External State.
    #[test]
    fn an_execution_id_without_a_contract_is_refused() {
        let mut e =
            ClaimContractEligibility::from_claim(None, Some(&format!("blake3:{}", "a".repeat(64))));
        assert!(matches!(
            e.eligibility(),
            Err(AcceptanceFailure::ExternalStateRequiresWorkloadIdle)
        ));
    }
}
