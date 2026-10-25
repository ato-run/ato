#![allow(dead_code)]
//! One candidate Derivation, bound and projected onto the execution
//! machinery: the plan an attempt executes, the staged workspace it builds
//! in, and what the executed candidate is observed as.

use std::collections::BTreeMap;

use anyhow::Result;
use ato_formation::authoring::{
    AuthoringDraft, AuthoringProvenance, BindingContext, BoundContract, BoundDerivation, bind,
};
use ato_formation::detect::{DetectorEvidence, FieldOrigins};
use ato_formation::failure::{FailureStage, FormationFailure};
use ato_formation::intent::{
    AuthoredOverrides, EffectiveBuildPlanV1, ProgramIntentV1, compile_build_plan, compile_intent,
};
use ato_formation::projection::{DerivationProjection, project};
use ato_formation::source::SourceClosureRef;

/// One candidate Derivation, bound and projected onto this worker's
/// execution machinery — everything an attempt needs to run.
pub struct PlannedCandidate {
    pub contract: BoundContract,
    pub derivation: BoundDerivation,
    pub contract_ref: String,
    pub derivation_ref: String,
    pub projected: DerivationProjection,
    pub intent: ProgramIntentV1,
    pub plan: EffectiveBuildPlanV1,
    pub intent_digest: String,
    pub plan_digest: String,
}

/// Bind one authoring draft against the verified closure, then project and
/// compile it into an intent and a build plan.
///
/// This is the shared middle of every Formation attempt: the hosted job calls
/// it once, a local Formation calls it per candidate. Nothing here executes —
/// it turns a route somebody named into a plan a Runtime can be asked to run.
pub fn plan_candidate(
    draft: &AuthoringDraft,
    closure_ref: &SourceClosureRef,
    evidence: &DetectorEvidence,
    authored_overrides: BTreeMap<String, String>,
    guest_root: &str,
    triple: &str,
) -> Result<PlannedCandidate> {
    // ── bind: drafts become addressable ─────────────────────────────────────
    //
    // A draft names a workspace by path and may ask for an observation to be
    // captured rather than stated. Binding resolves both against the closure
    // this build has already verified, and only then is there something to
    // hash. The Contract's digest is the Capsule's identity; the Derivation's
    // is this route's, separately.
    let (contract, derivation) = bind(
        draft,
        &BindingContext {
            source_closure_ref: closure_ref.as_str(),
        },
    )
    .map_err(FormationFailure::from)?;
    let contract_ref = contract.contract_ref().map_err(FormationFailure::from)?;
    let derivation_ref = derivation
        .derivation_ref()
        .map_err(FormationFailure::from)?;

    // ── project onto this worker's execution machinery ──────────────────────
    //
    // `ProgramIntent` and `EffectiveBuildPlan` are below this line: an
    // execution plan for running THIS Derivation on THIS worker, and never an
    // input to either digest above.
    let projected = project(&derivation, &contract).map_err(FormationFailure::from)?;

    let mut authored = authored_overrides;
    match draft.provenance {
        // An author who wrote a route is authoritative over it. A job override
        // silently changing an authored argv is the same sin as a Preset
        // fallback, arriving through a different door.
        AuthoringProvenance::Authored => {
            for (key, value) in projected.overrides.0.clone() {
                authored.insert(key, value);
            }
        }
        // Nobody stated an intent, so an explicit override from the caller is
        // the most specific thing anybody said.
        AuthoringProvenance::PresetSynthesized { .. } => {
            for (key, value) in projected.overrides.0.clone() {
                authored.entry(key).or_insert(value);
            }
        }
    }
    let overrides = AuthoredOverrides(authored);

    let mut origins = FieldOrigins::new();
    let intent = compile_intent(evidence, &overrides, guest_root, &mut origins)
        .map_err(FormationFailure::from)?;
    // A plan that cannot be compiled is a projection problem, not the author's
    // grammar: it is this worker failing to turn a valid intent into steps.
    let mut plan = compile_build_plan(&intent, guest_root, triple).map_err(|error| {
        FormationFailure::new(error.code(), FailureStage::Projection, error.to_string())
    })?;
    // The author's `exec` steps, in the order written, after the platform's
    // prerequisites (a provisioned interpreter). The projection already kept
    // any inferred application build from being planned beside them.
    plan.steps.extend(projected.build_steps.iter().cloned());
    // Digest failures are ours, not the author's: nothing they could change
    // would fix one, so they stay anonymous and reach the operator log only.
    let intent_digest = intent
        .canonical_digest()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let plan_digest = plan
        .canonical_digest()
        .map_err(|error| anyhow::anyhow!("{error}"))?;

    Ok(PlannedCandidate {
        contract,
        derivation,
        contract_ref,
        derivation_ref,
        projected,
        intent,
        plan,
        intent_digest,
        plan_digest,
    })
}
