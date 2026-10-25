//! Compare compiler output against a fixed unprovisioned Runtime binding.
//! Runtime cache checks are covered by cached_toolchain and execution tests;
//! an ambient /opt cache must not change these historical compiler fixtures.
use std::collections::BTreeMap;

use ato_formation::{
    authoring::AuthoringDraft,
    detect::DetectorEvidence,
    execution::{InputFacts, RuntimeBinding, lower_execution},
    source::SourceClosureRef,
};
use ato_formation_worker::job::PlannedCandidate;

pub fn plan_candidate(
    draft: &AuthoringDraft,
    source: &SourceClosureRef,
    evidence: &DetectorEvidence,
    overrides: BTreeMap<String, String>,
    guest_root: &str,
    triple: &str,
) -> anyhow::Result<PlannedCandidate> {
    // Keep the actual worker's validation and semantic binding, then compare
    // the common compiler before Runtime-local cache substitution.
    let mut planned = ato_formation_worker::job::plan_candidate(
        draft, source, evidence, overrides, guest_root, triple,
    )?;
    planned.plan = lower_execution(
        &planned.derivation,
        InputFacts::capture(evidence),
        RuntimeBinding {
            workspace_guest_root: guest_root,
            target_triple: triple,
        },
    )?;
    Ok(planned)
}
