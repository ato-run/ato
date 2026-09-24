//! One candidate Derivation, bound and projected onto the execution
//! machinery: the plan an attempt executes, the staged workspace it builds
//! in, and what the executed candidate is observed as.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};
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
use ato_formation::verify::CandidateObservation;

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

/// Copy the source into the workspace the build writes to.
///
/// A copy rather than a bind: the source is read-only inside the sandbox on
/// purpose, and a build that edited it would produce an artifact whose closure
/// ref no longer describes it.
pub fn stage_workspace(source_root: &Path, workspace_root: &Path) -> Result<()> {
    std::fs::create_dir_all(workspace_root)
        .with_context(|| format!("cannot create {}", workspace_root.display()))?;
    copy_tree(source_root, workspace_root)
}

pub fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let metadata = std::fs::symlink_metadata(entry.path())?;
        let target = to.join(entry.file_name());
        // A link is recreated as the same link — never followed, never copied
        // as its target's content. The source resolver admitted it only as a
        // contained, relative link, so the same string stays inside the
        // staged tree too.
        if metadata.is_symlink() {
            recreate_symlink(&std::fs::read_link(entry.path())?, &target)?;
            continue;
        }
        if metadata.is_dir() {
            std::fs::create_dir_all(&target)?;
            copy_tree(&entry.path(), &target)?;
        } else if metadata.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn recreate_symlink(link: &Path, at: &Path) -> Result<()> {
    std::os::unix::fs::symlink(link, at)
        .with_context(|| format!("cannot recreate the link {}", at.display()))
}

#[cfg(not(unix))]
fn recreate_symlink(_link: &Path, at: &Path) -> Result<()> {
    bail!("cannot recreate the link {} on this platform", at.display())
}

pub fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// What the executed Derivation actually produced, in the terms the Contract
/// observes.
///
/// Deliberately built from things this worker HOLDS. A static surface knows
/// which paths it serves because it just wrote the manifest; a process
/// candidate does not exist yet — it starts on a Runner — so its HTTP
/// observation is handed to the readiness gate the projection derived from the
/// very same Contract, and to nothing else.
pub fn observe_candidate(
    derivation: &BoundDerivation,
    projected: &ato_formation::projection::DerivationProjection,
    static_bundle: Option<&crate::static_lane::StaticFormationOutput>,
) -> CandidateObservation {
    let mut statically_served_paths = BTreeSet::new();
    if let Some(bundle) = static_bundle {
        // Read every path from the manifest that was just produced, so this is
        // a statement about the artifact rather than the plan that hoped to
        // produce it. This matters for body-bound proof resources such as
        // `/proof.txt`; Formation defers their bytes to the runtime verifier,
        // but must first prove the exact endpoint exists.
        statically_served_paths.insert("/".to_owned());
        statically_served_paths.extend(bundle.served_paths.iter().cloned());
    }
    CandidateObservation {
        input_refs: derivation
            .inputs
            .iter()
            .map(|input| (input.id.clone(), input.content_ref.clone()))
            .collect(),
        exported_ports: derivation
            .ports
            .iter()
            .map(|port| port.id.clone())
            .collect(),
        statically_served_paths,
        runtime_readiness: projected
            .readiness
            .as_ref()
            .map(|readiness| (readiness.port_id.clone(), readiness.path.clone())),
        instance_snapshot_ref: None,
    }
}
