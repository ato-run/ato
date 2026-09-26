//! Semantic binding and physical lowering are separate authority boundaries.
use anyhow::{Context, Result};
use ato_formation::authoring::{
    AuthoringDraft, BindingContext, BoundContract, BoundDerivation, HTTP_CONTRACT_VERIFIER, bind,
};
use ato_formation::detect::DetectorEvidence;
use ato_formation::execution::{ExecutionPlan, InputFacts, RuntimeBinding, lower_execution};
use ato_formation::failure::{FailureStage, FormationFailure};
use ato_formation::source::SourceClosureRef;
use ato_formation::verify::CandidateObservation;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub struct BoundCandidate {
    pub contract: BoundContract,
    pub derivation: BoundDerivation,
    pub contract_ref: String,
    pub derivation_ref: String,
}

pub struct PlannedCandidate {
    pub bound: BoundCandidate,
    pub plan: ExecutionPlan,
}
impl std::ops::Deref for PlannedCandidate {
    type Target = BoundCandidate;
    fn deref(&self) -> &Self::Target {
        &self.bound
    }
}

pub fn bind_candidate(
    draft: &AuthoringDraft,
    closure_ref: &SourceClosureRef,
) -> Result<BoundCandidate> {
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
    Ok(BoundCandidate {
        contract,
        derivation,
        contract_ref,
        derivation_ref,
    })
}

pub fn plan_candidate(
    draft: &AuthoringDraft,
    closure_ref: &SourceClosureRef,
    evidence: &DetectorEvidence,
    authored_overrides: BTreeMap<String, String>,
    guest_root: &str,
    triple: &str,
) -> Result<PlannedCandidate> {
    // Frontends must express semantic choices in their draft. A post-bind
    // override must never execute a different D under the same address.
    if !authored_overrides.is_empty() {
        return Err(FormationFailure::new(
            "authoring_overrides_require_draft",
            FailureStage::Authoring,
            "authoring overrides must be resolved into AuthoringDraft before binding".to_owned(),
        )
        .into());
    }
    let facts = InputFacts::capture(evidence);
    let bound = bind_candidate(draft, closure_ref)?;
    let plan = lower_execution(
        &bound.derivation,
        facts,
        RuntimeBinding {
            workspace_guest_root: guest_root,
            target_triple: triple,
        },
    )
    .map_err(|error| {
        FormationFailure::new(error.code(), FailureStage::Projection, error.to_string())
    })?;
    Ok(PlannedCandidate { bound, plan })
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
    contract: &BoundContract,
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
        runtime_readiness: contract
            .requirements
            .iter()
            .find(|r| r.verifier == HTTP_CONTRACT_VERIFIER)
            .and_then(|r| {
                r.port
                    .as_ref()
                    .map(|port| (port.clone(), r.path.clone().unwrap_or_else(|| "/".into())))
            }),
        instance_snapshot_ref: None,
    }
}

impl PlannedCandidate {
    /// What an attempt of this candidate verifies: its frozen K and D, with
    /// the input identities it resolved. The intent and build plan stay with
    /// the Formation realizer that builds it.
    pub fn attempt_spec(&self) -> crate::spec::AttemptSpec<'_> {
        crate::spec::AttemptSpec {
            contract: &self.contract,
            contract_ref: &self.contract_ref,
            derivation: &self.derivation,
            derivation_ref: &self.derivation_ref,
            shape: if self.plan.lane.is_process() {
                crate::spec::CandidateShape::Process
            } else {
                crate::spec::CandidateShape::StaticWeb
            },
            input_refs: observe_candidate(&self.derivation, &self.contract, None).input_refs,
            instance_snapshot_ref: None,
        }
    }
}
