//! Local Formation — Phase 1.
//!
//! `I -- D on this Runtime --> C`, then `C |= K` observed for real
//! (ADR-019): candidates are built and, for a process lane, launched
//! ephemerally so every Contract observation is decided before `Formed` is
//! reported.
//!
//! The driver holds no client for anywhere else. `--runtime local` is the
//! whole candidate space, so a failed attempt produces evidence and never a
//! fallback — the property is structural, not a policy the code remembers to
//! follow.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use ato_formation::authoring::{AuthoringDraft, AuthoringProvenance, HTTP_CONTRACT_VERIFIER};
use ato_formation::capsule_toml::{parse_capsule_toml, read_capsule_toml};
use ato_formation::detect::{DetectorEvidence, detect};
use ato_formation::failure::{FailureStage, FormationFailure};
use ato_formation::preset::candidates;
use ato_formation::request::{
    AttemptFailure, AttemptStatus, ContractSource, FormationAttempt, FormationNetworkPolicy,
    FormationRequest, FormationResult, InitialCondition, RuntimeConstraint, RuntimeProfile,
    VerifiedRoute,
};
use ato_formation::source::{
    RESOLVER_CONTRACT_V1, SourceClosureRef, SourceLimits, measure_source_tree,
};
use ato_formation::verify::{RuntimeObservation, verify, verify_runtime};

use crate::ephemeral::{RequiredObservation, observe_process_candidate};
use crate::executor::{AttemptExecution, AttemptExecutor, ExecutedCandidate, LocalAttemptExecutor};
use crate::job::{PlannedCandidate, copy_tree, digest, observe_candidate, plan_candidate};
use crate::pack::pack_tree;
use crate::sandbox::{BuildLimits, NetworkPolicy, TOOLCHAIN_ROOT, containment_available};

/// What a local Formation needs beyond the request itself.
pub struct LocalFormation {
    /// Per-attempt scratch: staged workspace, build cache, bundle output.
    pub work_root: PathBuf,
    /// Verified artifacts are written here, content-addressed.
    pub out_dir: PathBuf,
    /// The calling binary, re-exec'd inside the build sandbox.
    pub shim: PathBuf,
    pub limits: BuildLimits,
    pub source_limits: SourceLimits,
}

static ATTEMPT_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Run a Formation request against the local Runtime, end to end.
///
/// Hard errors — an unreadable directory, an authored `capsule.toml` that
/// does not parse — come back as `Err`. Everything a candidate did or did
/// not do comes back inside the result's attempts.
pub fn run(request: &FormationRequest, env: &LocalFormation) -> Result<FormationResult> {
    let RuntimeConstraint::Exact { runtime_id } = &request.runtime;
    if runtime_id != "local" {
        bail!("Phase 1 admits exactly one Runtime: --runtime local (got {runtime_id:?})");
    }
    let profile = probe_local_runtime();

    let InitialCondition::LocalDirectory { path } = &request.initial_condition;
    let (closure_ref, source_root) = stage_local_source(path, env.source_limits)?;
    let evidence = detect(&source_root).context("detection failed")?;

    // Candidate Derivations: one authored route, or every preset the source
    // honestly fits. An authored document that fails to parse stops the whole
    // Formation — substituting a guess for a route somebody wrote is the one
    // failure mode the strict rule exists to prevent.
    let drafts: Vec<AuthoringDraft> = match &request.contract {
        ContractSource::Authored { toml } => {
            vec![parse_capsule_toml(toml).map_err(FormationFailure::from)?]
        }
        ContractSource::Infer => {
            match read_capsule_toml(&source_root).map_err(FormationFailure::from)? {
                Some(text) => vec![parse_capsule_toml(&text).map_err(FormationFailure::from)?],
                None => match candidates(&evidence) {
                    Ok(list) => list.into_iter().map(synthesize).collect(),
                    Err(mismatch) => {
                        return Ok(FormationResult::NoVerifiedRoute {
                            attempted_contract_refs: Vec::new(),
                            attempts: vec![FormationAttempt {
                                candidate: "detect".to_owned(),
                                derivation_ref: None,
                                contract_ref: None,
                                runtime_id: runtime_id.clone(),
                                status: AttemptStatus::Filtered,
                                verification: None,
                                failure: Some(AttemptFailure {
                                    code: mismatch.code.to_owned(),
                                    stage: FailureStage::Preset.as_str().to_owned(),
                                    message: mismatch.message,
                                }),
                            }],
                        });
                    }
                },
            }
        }
    };

    let network = match request.policy.network {
        FormationNetworkPolicy::Denied => NetworkPolicy::Denied,
        FormationNetworkPolicy::DependencyResolution => NetworkPolicy::DependencyResolution,
    };
    let executor = LocalAttemptExecutor {
        shim: env.shim.clone(),
        network,
        limits: env.limits,
    };

    let mut attempts = Vec::new();
    for draft in drafts.iter().take(request.budget.max_attempts.max(1)) {
        let (attempt, formed) = attempt_one(
            draft,
            &closure_ref,
            &source_root,
            &evidence,
            runtime_id,
            &profile,
            &executor,
            env,
        );
        attempts.push(attempt);
        if let Some((contract_ref, route)) = formed {
            return Ok(FormationResult::Formed {
                contract_ref,
                verified_routes: vec![route],
                attempts,
            });
        }
    }

    Ok(FormationResult::NoVerifiedRoute {
        attempted_contract_refs: attempts
            .iter()
            .filter_map(|attempt| attempt.contract_ref.clone())
            .collect(),
        attempts,
    })
}

/// One candidate end to end: plan, admit, execute, observe, verify, keep.
fn attempt_one(
    draft: &AuthoringDraft,
    closure_ref: &SourceClosureRef,
    source_root: &Path,
    evidence: &DetectorEvidence,
    runtime_id: &str,
    profile: &RuntimeProfile,
    executor: &LocalAttemptExecutor,
    env: &LocalFormation,
) -> (FormationAttempt, Option<(String, VerifiedRoute)>) {
    let mut attempt = FormationAttempt {
        candidate: match &draft.provenance {
            AuthoringProvenance::Authored => "authored".to_owned(),
            AuthoringProvenance::PresetSynthesized { preset } => preset.to_string(),
        },
        derivation_ref: None,
        contract_ref: None,
        runtime_id: runtime_id.to_owned(),
        status: AttemptStatus::Failed,
        verification: None,
        failure: None,
    };

    let planned = match plan_candidate(
        draft,
        closure_ref,
        evidence,
        // No caller overrides locally: the request is the whole statement.
        BTreeMap::new(),
        "/app",
        &host_triple(),
    ) {
        Ok(planned) => planned,
        Err(error) => {
            attempt.failure = Some(failure_of(&error));
            return (attempt, None);
        }
    };
    attempt.contract_ref = Some(planned.contract_ref.clone());
    attempt.derivation_ref = Some(planned.derivation_ref.clone());

    if let Some(failure) = admits(profile, &planned, executor.network) {
        attempt.status = AttemptStatus::Filtered;
        attempt.failure = Some(failure);
        return (attempt, None);
    }

    let attempt_id = format!(
        "local-{}-{}",
        std::process::id(),
        ATTEMPT_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let attempt_root = env.work_root.join(&attempt_id);

    let executed = match executor.execute(&AttemptExecution {
        attempt_id: &attempt_id,
        candidate: &planned,
        source_root,
        attempt_root: &attempt_root,
    }) {
        Ok(executed) => executed,
        Err(error) => {
            attempt.failure = Some(failure_of(&error));
            return (attempt, None);
        }
    };

    // ── observe ─────────────────────────────────────────────────────────────
    //
    // Static candidates are decided from the artifact they just produced; a
    // process candidate is launched ephemerally and measured over loopback
    // HTTP. Either way the Contract sees only what was actually observed.
    let bundle = match &executed {
        ExecutedCandidate::StaticWeb { output } => Some(&**output),
        ExecutedCandidate::Process { .. } => None,
    };
    let observation = observe_candidate(&planned.derivation, &planned.projected, bundle);
    let verification = match &executed {
        // A static surface answers from its own files: decided from the
        // artifact that was just built. A body digest is not decided this way
        // and stays Deferred — which a local Formation refuses below.
        ExecutedCandidate::StaticWeb { .. } => verify(&planned.contract, &observation),
        ExecutedCandidate::Process { workspace_root } => {
            let required = required_observations(&planned);
            match observe_process_candidate(
                workspace_root,
                &planned.plan.workspace_guest_root,
                &planned.intent,
                &required,
            ) {
                Ok(http) => verify_runtime(
                    &planned.contract,
                    &RuntimeObservation {
                        input_refs: observation.input_refs.clone(),
                        http,
                        instance_snapshot_ref: None,
                    },
                ),
                Err(error) => {
                    // The candidate could not be observed at all: nothing is
                    // verified, and guessing verdicts would invent evidence.
                    attempt.failure = Some(AttemptFailure {
                        code: "candidate_not_observable".to_owned(),
                        stage: FailureStage::Verification.as_str().to_owned(),
                        message: bounded(&format!("{error:#}")),
                    });
                    return (attempt, None);
                }
            }
        }
    };

    if !verification.fully_satisfied() {
        attempt.failure = verification
            .failure()
            .map(|(id, code, detail)| AttemptFailure {
                code: code.to_owned(),
                stage: FailureStage::Verification.as_str().to_owned(),
                message: bounded(&format!("{id}: {detail}")),
            })
            .or_else(|| {
                // Nothing failed and yet not everything was satisfied: some
                // observation was deferred — decided by a gate this Runtime
                // does not run, so for a local Formation it is undecided.
                Some(AttemptFailure {
                    code: "observation_undecided".to_owned(),
                    stage: FailureStage::Verification.as_str().to_owned(),
                    message: "an observation was deferred to a run-time gate; a local Formation                          decides every observation itself"
                        .to_owned(),
                })
            });
        attempt.verification = Some(verification);
        return (attempt, None);
    }

    // Verified — only now does the artifact become worth keeping.
    match store_candidate(&executed, env) {
        Ok(materialization_ref) => {
            attempt.status = AttemptStatus::Verified;
            attempt.verification = Some(verification);
            (
                attempt,
                Some((
                    planned.contract_ref.clone(),
                    VerifiedRoute {
                        derivation_ref: planned.derivation_ref.clone(),
                        runtime_id: runtime_id.to_owned(),
                        materialization_ref,
                    },
                )),
            )
        }
        Err(error) => {
            attempt.failure = Some(AttemptFailure {
                code: "artifact_store_failed".to_owned(),
                stage: "publish".to_owned(),
                message: bounded(&format!("{error:#}")),
            });
            (attempt, None)
        }
    }
}

/// The hard gates a candidate must pass before an attempt is spent on it.
///
/// Capability match is not success — it only decides whether executing is
/// worth anything. Everything here is a fact about the Runtime or the plan,
/// not a guess about the outcome.
fn admits(
    profile: &RuntimeProfile,
    planned: &PlannedCandidate,
    network: NetworkPolicy,
) -> Option<AttemptFailure> {
    if !planned.plan.steps.is_empty()
        && profile.get("formation.containment") != Some("bwrap+landlock")
    {
        return Some(AttemptFailure {
            code: "runtime_cannot_contain_build".to_owned(),
            stage: "admission".to_owned(),
            message: "this candidate needs build steps and this Runtime cannot contain one                  (no bwrap); it was not attempted"
                .to_owned(),
        });
    }
    if planned.plan.steps.iter().any(|step| step.needs_network) && network == NetworkPolicy::Denied
    {
        return Some(AttemptFailure {
            code: "network_denied".to_owned(),
            stage: "admission".to_owned(),
            message: "this candidate's build resolves dependencies from the network and the                  request denies it; it was not attempted"
                .to_owned(),
        });
    }
    None
}

/// The Contract's HTTP observations, resolved to concrete loopback targets.
///
/// A requirement whose port the Derivation never exports is skipped here and
/// failed by the verifier — probing it would measure a port nobody claimed.
fn required_observations(planned: &PlannedCandidate) -> Vec<RequiredObservation> {
    planned
        .contract
        .requirements
        .iter()
        .filter(|requirement| requirement.verifier == HTTP_CONTRACT_VERIFIER)
        // Only GET is observed; anything else stays for the verifier to
        // refuse rather than be probed with the wrong method.
        .filter(|requirement| {
            requirement
                .method
                .as_deref()
                .is_none_or(|method| method == "GET")
        })
        .filter_map(|requirement| {
            let port_id = requirement.port.clone()?;
            let guest_port = planned
                .derivation
                .ports
                .iter()
                .find(|port| port.id == port_id)
                .and_then(|port| port.guest_port)?;
            Some(RequiredObservation {
                port_id,
                port: guest_port,
                path: requirement.path.clone().unwrap_or_else(|| "/".to_owned()),
            })
        })
        .collect()
}

/// Keep the artifact of a verified candidate, content-addressed.
fn store_candidate(executed: &ExecutedCandidate, env: &LocalFormation) -> Result<String> {
    match executed {
        ExecutedCandidate::Process { workspace_root } => {
            let packed = pack_tree(workspace_root)?;
            let reference = digest(&packed);
            let dir = env.out_dir.join("artifacts");
            std::fs::create_dir_all(&dir)?;
            std::fs::write(
                dir.join(format!("{}.tar", &reference["sha256:".len()..])),
                &packed,
            )?;
            Ok(reference)
        }
        ExecutedCandidate::StaticWeb { output } => {
            let dir = env.out_dir.join("bundles");
            std::fs::create_dir_all(&dir)?;
            let destination = dir.join(&output.manifest_digest["sha256:".len()..]);
            if !destination.exists() {
                copy_tree(&output.bundle.bundle_root, &destination)?;
            }
            Ok(output.manifest_digest.clone())
        }
    }
}

/// Measure a local directory into the same closure identity an uploaded
/// archive would get.
///
/// The tree is tarred under a `source/` wrapper — the same shape a codeload
/// tarball has — so `measure_source_tree` strips the wrapper and the digest
/// describes the directory's contents, not its transport. Symlinks are
/// skipped, matching what a build would actually receive.
fn stage_local_source(dir: &Path, limits: SourceLimits) -> Result<(SourceClosureRef, PathBuf)> {
    let source_root = dir
        .canonicalize()
        .with_context(|| format!("cannot read {}", dir.display()))?;
    if !source_root.is_dir() {
        bail!("{} is not a directory", source_root.display());
    }
    let archive = tar_directory(&source_root)?;
    let tree_digest =
        measure_source_tree(&archive, limits).map_err(|error| anyhow::anyhow!("{error}"))?;
    let closure_ref = SourceClosureRef::derive(&tree_digest, "", RESOLVER_CONTRACT_V1)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok((closure_ref, source_root))
}

/// Pack a directory into an in-memory tar under a `source/` wrapper,
/// deterministically: sorted paths, zeroed metadata, no symlinks, no `.git`.
fn tar_directory(root: &Path) -> Result<Vec<u8>> {
    let mut entries: Vec<(PathBuf, PathBuf, bool)> = Vec::new();
    collect_tree(root, root, &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut builder = tar::Builder::new(Vec::new());
    append_dir(&mut builder, "source")?;
    for (relative, absolute, is_dir) in entries {
        let path = format!("source/{}", relative.to_string_lossy());
        if is_dir {
            append_dir(&mut builder, &path)?;
        } else {
            let bytes = std::fs::read(&absolute)
                .with_context(|| format!("cannot read {}", absolute.display()))?;
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            builder
                .append_data(&mut header, &path, bytes.as_slice())
                .with_context(|| format!("cannot add {path} to the source archive"))?;
        }
    }
    Ok(builder.into_inner()?)
}

fn collect_tree(
    root: &Path,
    dir: &Path,
    entries: &mut Vec<(PathBuf, PathBuf, bool)>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        // VCS metadata is never app content — the same reason a codeload
        // tarball does not carry it.
        if name == ".git" {
            continue;
        }
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        // A link is not followed here either; the source module refuses them
        // and the build's own copy step skips them, so measuring without them
        // keeps all three agreeing.
        if metadata.is_symlink() {
            continue;
        }
        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        if metadata.is_dir() {
            entries.push((relative, path.clone(), true));
            collect_tree(root, &path, entries)?;
        } else if metadata.is_file() {
            entries.push((relative, path, false));
        }
    }
    Ok(())
}

fn append_dir(builder: &mut tar::Builder<Vec<u8>>, path: &str) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(0);
    header.set_mode(0o755);
    header.set_mtime(0);
    header.set_uid(0);
    header.set_gid(0);
    header.set_entry_type(tar::EntryType::Directory);
    header.set_cksum();
    builder.append_data(&mut header, path, std::io::empty())?;
    Ok(())
}

/// This machine as a Runtime: the facts a Formation filter can need.
///
/// Flat key/value — new facts are emitted, not added to a type.
pub fn probe_local_runtime() -> RuntimeProfile {
    let mut capabilities = BTreeMap::new();
    capabilities.insert("platform.os".to_owned(), std::env::consts::OS.to_owned());
    capabilities.insert(
        "platform.arch".to_owned(),
        std::env::consts::ARCH.to_owned(),
    );
    capabilities.insert(
        "formation.containment".to_owned(),
        if containment_available() {
            "bwrap+landlock".to_owned()
        } else {
            "none".to_owned()
        },
    );
    if Path::new(TOOLCHAIN_ROOT).is_dir() {
        capabilities.insert(
            "formation.toolchain_root".to_owned(),
            TOOLCHAIN_ROOT.to_owned(),
        );
    }
    RuntimeProfile {
        runtime_id: "local".to_owned(),
        capabilities,
    }
}

/// The triple the local machine builds for. A workspace produced here is
/// host-native; cross-compiling is a different request.
fn host_triple() -> String {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        other => other,
    };
    let os = match std::env::consts::OS {
        "linux" => "unknown-linux-gnu",
        "macos" => "apple-darwin",
        "windows" => "pc-windows-msvc",
        other => other,
    };
    format!("{arch}-{os}")
}

/// A failure a person can act on, recovered from the error's TYPE — the same
/// rule the hosted reporter follows: untyped errors stay anonymous because
/// their chains can carry paths and credentials.
fn failure_of(error: &anyhow::Error) -> AttemptFailure {
    match error.downcast_ref::<FormationFailure>() {
        Some(failure) => AttemptFailure {
            code: failure.code.clone(),
            stage: failure.stage.as_str().to_owned(),
            message: failure.bounded_message(crate::api::FAILURE_REASON_LIMIT),
        },
        None => AttemptFailure {
            code: "formation_failed".to_owned(),
            stage: "build".to_owned(),
            message: "the candidate could not be formed from this source".to_owned(),
        },
    }
}

/// One bounded line. Internal context stays in the log; the attempt record
/// carries a sentence.
fn bounded(reason: &str) -> String {
    crate::api::bounded_reason(reason)
}

fn synthesize(preset: ato_formation::preset::AppPreset) -> AuthoringDraft {
    ato_formation::preset::synthesize_authoring(preset)
}
