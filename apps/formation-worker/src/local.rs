//! Local Formation — Phase 1.
//!
//! `I -- D on this Runtime --> C`, then `C |= K` observed for real
//! (ADR-019): candidates are built and, for a process lane, realized
//! temporarily on the local Runtime so every Contract observation is decided
//! before `Formed` is reported.
//!
//! The Initial Condition is frozen once. The directory is snapshotted into
//! one archive, that archive goes through the same proof-state chain an
//! uploaded source does, and the tree it materializes is the only thing
//! detection, planning and building ever read — the measured `I` is the built
//! `I`, whatever happens to the directory afterwards.
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
use ato_formation::browser::{
    BrowserBudget, BrowserTarget, BrowserVerdict, BrowserVerificationReceipt,
    effective_contract_ref,
};
use ato_formation::capsule_toml::{parse_capsule_toml, read_capsule_toml};
use ato_formation::detect::{DetectorEvidence, detect};
use ato_formation::failure::{FailureStage, FormationFailure};
use ato_formation::preset::candidates;
use ato_formation::request::{
    AttemptFailure, AttemptStatus, ContractSource, FormationAttempt, FormationNetworkPolicy,
    FormationRequest, FormationResult, InitialCondition, RealizationEvidence, RuntimeConstraint,
    RuntimeProfile, VerifiedRoute,
};
use ato_formation::source::{DownloadedArchive, SourceClosureRef, SourceLimits};
use ato_formation::verify::{ContractVerification, RuntimeObservation, verify, verify_runtime};

use crate::browser_verify::{BrowserVerification, BrowserVerifierCommand, verify_in_browser};

use crate::ephemeral::{
    RequiredObservation, RequiredPort, TemporaryRealization, TemporaryRealizationRequest,
};
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
    /// The browser verifier this Runtime has, if any. Used only when a
    /// request carries a browser Contract.
    pub browser_verifier: Option<BrowserVerifierCommand>,
    pub browser_budget: BrowserBudget,
}

static ATTEMPT_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Run a Formation request against the local Runtime, end to end.
///
/// Hard errors — an unreadable directory, an authored `capsule.toml` that
/// does not parse — come back as `Err`. Everything a candidate did or did
/// not do comes back inside the result's attempts.
pub fn run(request: &FormationRequest, env: &LocalFormation) -> Result<FormationResult> {
    run_with_executor(request, env, &local_executor(request, env))
}

pub(crate) fn local_executor(
    request: &FormationRequest,
    env: &LocalFormation,
) -> LocalAttemptExecutor {
    LocalAttemptExecutor {
        shim: env.shim.clone(),
        network: network_policy(request.policy.network),
        limits: env.limits,
    }
}

/// [`run`] with the attempt executor supplied by the caller.
///
/// The seam a different execution boundary plugs into; the search loop, the
/// frozen Initial Condition and the verification rules stay the same.
pub fn run_with_executor(
    request: &FormationRequest,
    env: &LocalFormation,
    executor: &dyn AttemptExecutor,
) -> Result<FormationResult> {
    let RuntimeConstraint::Exact { runtime_id } = &request.runtime;
    if runtime_id != "local" {
        bail!("Phase 1 admits exactly one Runtime: --runtime local (got {runtime_id:?})");
    }
    run_as(request, env, executor, runtime_id)
}

/// [`run_with_executor`] on this machine, recording it in the attempt
/// evidence under `runtime_id`: the identity a Runtime Network ticket names
/// this Runtime by, rather than the anonymous `local`.
pub(crate) fn run_as(
    request: &FormationRequest,
    env: &LocalFormation,
    executor: &dyn AttemptExecutor,
    runtime_id: &str,
) -> Result<FormationResult> {
    let profile = probe_local_runtime();
    // Absolute from here on: these paths are bound into sandboxes whose
    // working directory is not this process's.
    let env = &LocalFormation {
        work_root: std::path::absolute(&env.work_root).context("cannot resolve the work root")?,
        out_dir: std::path::absolute(&env.out_dir).context("cannot resolve the out dir")?,
        shim: env.shim.clone(),
        limits: env.limits,
        source_limits: env.source_limits,
        browser_verifier: env.browser_verifier.clone(),
        browser_budget: env.browser_budget,
    };
    let browser = request
        .browser_contract
        .as_ref()
        .map(|contract| BrowserVerification {
            contract: contract.clone(),
            verifier: env.browser_verifier.clone(),
            budget: env.browser_budget,
        });

    let frozen = match &request.initial_condition {
        InitialCondition::LocalDirectory { path } => {
            freeze_local_source(path, &env.work_root, env.source_limits)?
        }
        InitialCondition::Archive {
            bytes,
            expected_digest,
        } => freeze_archive(
            bytes.clone(),
            expected_digest,
            &env.work_root,
            env.source_limits,
        )?,
    };
    let (closure_ref, source_root) = (frozen.closure_ref.clone(), frozen.root.clone());
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
                                runtime_id: runtime_id.to_owned(),
                                status: AttemptStatus::Filtered,
                                verification: None,
                                base_contract_ref: None,
                                realization: None,
                                browser_verification: None,
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

    let network = network_policy(request.policy.network);

    let mut attempts = Vec::new();
    for draft in drafts.iter().take(request.budget.max_attempts.max(1)) {
        let (attempt, formed) = attempt_one(
            draft,
            &closure_ref,
            &source_root,
            &evidence,
            runtime_id,
            &profile,
            executor,
            network,
            browser.as_ref(),
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
#[allow(clippy::too_many_arguments)]
fn attempt_one(
    draft: &AuthoringDraft,
    closure_ref: &SourceClosureRef,
    source_root: &Path,
    evidence: &DetectorEvidence,
    runtime_id: &str,
    profile: &RuntimeProfile,
    executor: &dyn AttemptExecutor,
    network: NetworkPolicy,
    browser: Option<&BrowserVerification>,
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
        base_contract_ref: None,
        realization: None,
        browser_verification: None,
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
    // The K this attempt verifies. A browser Contract is a condition of
    // success, so it is part of the identity; without one nothing changes.
    let contract_ref = effective_contract_ref(
        &planned.contract_ref,
        browser.map(|browser| &browser.contract),
    );
    if contract_ref != planned.contract_ref {
        attempt.base_contract_ref = Some(planned.contract_ref.clone());
    }
    attempt.contract_ref = Some(contract_ref.clone());
    attempt.derivation_ref = Some(planned.derivation_ref.clone());

    if let Some(failure) = admits(profile, &planned, network, browser) {
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
    // process candidate is realized temporarily on this Runtime and measured
    // over loopback HTTP. Either way the Contract sees only what was actually
    // observed.
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
            let (ports, required) = required_observations(&planned);
            let input_refs = observation.input_refs.clone();
            let measured = realize_and_observe(
                &TemporaryRealizationRequest {
                    workspace: workspace_root,
                    scratch: &attempt_root.join("realization"),
                    intent: &planned.intent,
                    ports: &ports,
                    shim: &env.shim,
                    attempt_id: &attempt_id,
                },
                &required,
                &|http| {
                    verify_runtime(
                        &planned.contract,
                        &RuntimeObservation {
                            input_refs: input_refs.clone(),
                            http,
                            instance_snapshot_ref: None,
                        },
                    )
                },
                browser,
                runtime_id,
                network,
                &mut attempt,
            );
            match measured {
                Ok(verification) => verification,
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

    // The acceptance prompt, when one was asked for: only a browser PASS
    // lets the candidate form. Fail and inconclusive are both "not formed".
    if browser.is_some() {
        let outcome = attempt
            .browser_verification
            .as_ref()
            .map(|receipt| (receipt.overall, receipt.reason.clone()));
        let failure = match outcome {
            Some((BrowserVerdict::Pass, _)) => None,
            Some((BrowserVerdict::Fail, _)) => Some((
                "browser_contract_failed",
                "the candidate was observed in a browser and did not satisfy the acceptance \
                 prompt"
                    .to_owned(),
            )),
            Some((BrowserVerdict::Inconclusive, reason)) => Some((
                "browser_contract_inconclusive",
                format!(
                    "the browser verification did not reach a verdict{}",
                    reason
                        .map(|reason| format!(": {reason}"))
                        .unwrap_or_default()
                ),
            )),
            None => Some((
                "browser_contract_inconclusive",
                "the candidate was not verified in a browser".to_owned(),
            )),
        };
        if let Some((code, message)) = failure {
            attempt.failure = Some(AttemptFailure {
                code: code.to_owned(),
                stage: FailureStage::Verification.as_str().to_owned(),
                message: bounded(&message),
            });
            attempt.verification = Some(verification);
            return (attempt, None);
        }
    }

    // Verified — only now does the artifact become worth keeping.
    match store_candidate(&executed, env) {
        Ok(materialization_ref) => {
            attempt.status = AttemptStatus::Verified;
            attempt.verification = Some(verification);
            (
                attempt,
                Some((
                    contract_ref.clone(),
                    VerifiedRoute {
                        derivation_ref: planned.derivation_ref.clone(),
                        runtime_id: runtime_id.to_owned(),
                        materialization_ref,
                    },
                )),
            )
        }
        Err(error) => {
            // The Contract WAS satisfied; only keeping the artifact failed.
            // The verdicts stay in the evidence so the two are not confused.
            attempt.verification = Some(verification);
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
    browser: Option<&BrowserVerification>,
) -> Option<AttemptFailure> {
    // The route's own platform statement is part of D, so it binds every
    // Runtime that is asked to run it — whatever a scheduler believed.
    let platforms = &planned.derivation.platforms;
    if !platforms.is_empty()
        && !platforms.iter().any(|platform| {
            platform.os == std::env::consts::OS && platform.arch == std::env::consts::ARCH
        })
    {
        return Some(AttemptFailure {
            code: "platform_unsupported".to_owned(),
            stage: "admission".to_owned(),
            message: format!(
                "this route runs on {}; this Runtime is {}/{}; it was not attempted",
                platforms
                    .iter()
                    .map(|platform| format!("{}/{}", platform.os, platform.arch))
                    .collect::<Vec<_>>()
                    .join(", "),
                std::env::consts::OS,
                std::env::consts::ARCH
            ),
        });
    }
    if browser.is_some() && planned.intent.lane != ato_formation::intent::Lane::PythonProcess {
        // A browser Contract is verified against a running candidate, and in
        // Phase 1 only a process lane is realized.
        return Some(AttemptFailure {
            code: "browser_contract_needs_realization".to_owned(),
            stage: "admission".to_owned(),
            message: "a browser Contract is verified against a running candidate; this \
                      candidate's lane is not realized on this Runtime"
                .to_owned(),
        });
    }
    if let Some(browser) = browser {
        match &browser.verifier {
            None => {
                return Some(AttemptFailure {
                    code: "browser_verifier_unavailable".to_owned(),
                    stage: "admission".to_owned(),
                    message: "the request carries a browser Contract and this Runtime has no \
                              browser verifier; it was not attempted"
                        .to_owned(),
                });
            }
            // Never verified outside the verifier sandbox unless a developer
            // chose an uncontained verifier explicitly.
            Some(verifier) if !verifier.usable() => {
                return Some(AttemptFailure {
                    code: "browser_verifier_containment_unavailable".to_owned(),
                    stage: "admission".to_owned(),
                    message: "the request carries a browser Contract and this Runtime cannot run \
                              its browser verifier contained (bubblewrap); it was not attempted"
                        .to_owned(),
                });
            }
            Some(_) => {}
        }
    }
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
    if !planned.plan.steps.is_empty() && profile.get("formation.toolchain_root").is_none() {
        return Some(AttemptFailure {
            code: "runtime_has_no_toolchain_root".to_owned(),
            stage: "admission".to_owned(),
            message: format!(
                "this candidate's build provisions toolchains into {TOOLCHAIN_ROOT}, which this \
                 Runtime does not have; it was not attempted"
            ),
        });
    }
    if planned.intent.lane == ato_formation::intent::Lane::PythonProcess
        && profile.get("formation.containment") != Some("bwrap+landlock")
    {
        return Some(AttemptFailure {
            code: "runtime_cannot_contain_candidate".to_owned(),
            stage: "admission".to_owned(),
            message: "verifying this candidate means running it, and this Runtime cannot contain \
                      a process (no bwrap); it was not attempted"
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

/// The Contract's HTTP observations, by logical port, and the ports they
/// need realized.
///
/// A requirement whose port the Derivation never exports is skipped here and
/// failed by the verifier — probing it would measure a port nobody claimed.
fn required_observations(
    planned: &PlannedCandidate,
) -> (Vec<RequiredPort>, Vec<RequiredObservation>) {
    let mut ports: Vec<RequiredPort> = Vec::new();
    let mut required = Vec::new();
    for requirement in &planned.contract.requirements {
        if requirement.verifier != HTTP_CONTRACT_VERIFIER {
            continue;
        }
        // Only GET is observed; anything else stays for the verifier to
        // refuse rather than be probed with the wrong method.
        if requirement
            .method
            .as_deref()
            .is_some_and(|method| method != "GET")
        {
            continue;
        }
        let Some(port_id) = requirement.port.clone() else {
            continue;
        };
        let Some(guest_port) = planned
            .derivation
            .ports
            .iter()
            .find(|port| port.id == port_id)
            .and_then(|port| port.guest_port)
        else {
            continue;
        };
        if !ports.iter().any(|port| port.port_id == port_id) {
            ports.push(RequiredPort {
                port_id: port_id.clone(),
                guest_port,
            });
        }
        required.push(RequiredObservation {
            port_id,
            path: requirement.path.clone().unwrap_or_else(|| "/".to_owned()),
        });
    }
    (ports, required)
}

/// Realize the candidate, observe it, destroy it — and record how it ran.
///
/// The realization is destroyed before this returns on every path: an
/// explicit `destroy` after observing, `Drop` on any early return.
#[allow(clippy::too_many_arguments)]
fn realize_and_observe(
    request: &TemporaryRealizationRequest<'_>,
    required: &[RequiredObservation],
    verify_http: &dyn Fn(
        Vec<ato_formation::verify::RuntimeHttpObservation>,
    ) -> ContractVerification,
    browser: Option<&BrowserVerification>,
    runtime_id: &str,
    build_network: NetworkPolicy,
    attempt: &mut FormationAttempt,
) -> Result<ContractVerification> {
    let mut evidence = RealizationEvidence {
        executor: "runtime-process".to_owned(),
        containment: "bwrap+landlock".to_owned(),
        workspace: "disposable-copy, read-only at /app; /tmp is tmpfs".to_owned(),
        // The Runtime's process policy, whatever the build was allowed: no
        // egress, TCP bind only on the allocated host ports. A
        // `dependency-resolution` request widens the BUILD, never the run.
        build_network: match build_network {
            NetworkPolicy::Denied => "denied",
            NetworkPolicy::DependencyResolution => "dependency-resolution",
        }
        .to_owned(),
        candidate_network: "no-egress; tcp bind limited to allocated host ports".to_owned(),
        endpoints: BTreeMap::new(),
        destroyed: false,
    };
    let result = (|| {
        let realization = TemporaryRealization::launch(request)?;
        evidence.endpoints = realization
            .endpoints()
            .iter()
            .map(|endpoint| {
                (
                    endpoint.port_id.clone(),
                    if endpoint.host_port == endpoint.guest_port {
                        format!(
                            "guest {} -> host {}",
                            endpoint.guest_port, endpoint.host_port
                        )
                    } else {
                        format!(
                            "guest {} -> host {} (guest port in use; carried by {})",
                            endpoint.guest_port,
                            endpoint.host_port,
                            crate::ephemeral::endpoint_env_name(&endpoint.port_id)
                        )
                    },
                )
            })
            .collect();
        let verification = realization.observe(required).map(verify_http);
        // The browser drives the SAME realization, and only one that already
        // satisfies the typed observations: a candidate that fails its HTTP
        // Contract has nothing to show a browser.
        if let (Ok(verification), Some(browser)) = (&verification, browser)
            && verification.fully_satisfied()
        {
            attempt.browser_verification = Some(browse(&realization, browser, runtime_id));
        }
        let destroyed = realization.destroy();
        evidence.destroyed = destroyed.is_ok();
        let verification = verification?;
        destroyed.context("the candidate could not be destroyed")?;
        Ok(verification)
    })();
    if result.is_err() && !evidence.destroyed {
        // Dropped on the error path: gone unless the Runtime said otherwise,
        // which it did loudly in the log.
        evidence.destroyed = !request.scratch.exists();
    }
    attempt.realization = Some(evidence);
    result
}

/// Verify the running candidate against the browser Contract, through the
/// endpoint the realization reports — never a guessed guest port.
fn browse(
    realization: &TemporaryRealization,
    browser: &BrowserVerification,
    runtime_id: &str,
) -> BrowserVerificationReceipt {
    let endpoints = realization.endpoints();
    let target = |endpoint: String| BrowserTarget {
        runtime_id: runtime_id.to_owned(),
        endpoint,
    };
    match endpoints {
        [endpoint] => verify_in_browser(
            browser,
            target(format!("http://127.0.0.1:{}/", endpoint.host_port)),
        ),
        _ => BrowserVerificationReceipt::unavailable(
            &browser.contract,
            target(String::new()),
            "none",
            &format!(
                "browser_endpoint_ambiguous: the candidate exposes {} observed ports; v0 \
                 verifies a candidate with exactly one",
                endpoints.len()
            ),
        ),
    }
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
                // copy_tree fills an existing directory; it does not create
                // the root, and whether a file or a directory is listed first
                // is up to the filesystem.
                std::fs::create_dir_all(&destination)?;
                copy_tree(&output.bundle.bundle_root, &destination)?;
            }
            Ok(output.manifest_digest.clone())
        }
    }
}

/// The Initial Condition, frozen: one snapshot of the directory, verified
/// and materialized. Removed when the Formation ends.
pub(crate) struct FrozenSource {
    pub(crate) closure_ref: SourceClosureRef,
    pub(crate) root: PathBuf,
    scratch: PathBuf,
}

impl Drop for FrozenSource {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

/// Snapshot a local directory once and turn it into the tree a Formation
/// builds from.
///
/// ```text
/// directory --tar once--> archive --digest--> DigestVerifiedArchive
///           --measure--> TreeVerifiedArchive --materialize--> frozen tree
/// ```
///
/// The archive is the codeload shape (a `source/` wrapper, no `.git`), so
/// the closure ref is the one an upload of the same tree would get and the
/// source module's rules — refused symlinks, path limits — apply unchanged.
/// After this returns the directory is never read again.
fn freeze_local_source(dir: &Path, work_root: &Path, limits: SourceLimits) -> Result<FrozenSource> {
    let archive = snapshot_directory(dir)?;
    let archive_digest = digest(&archive);
    freeze_archive(archive, &archive_digest, work_root, limits)
}

/// Snapshot a directory into the archive a Formation measures: the codeload
/// shape, no `.git`, symlinks kept for the source rules to decide.
pub fn snapshot_directory(dir: &Path) -> Result<Vec<u8>> {
    let directory = dir
        .canonicalize()
        .with_context(|| format!("cannot read {}", dir.display()))?;
    if !directory.is_dir() {
        bail!("{} is not a directory", directory.display());
    }
    tar_directory(&directory)
}

/// Verify an archive against the digest it was named by, measure its tree and
/// materialize it: the frozen Initial Condition.
pub(crate) fn freeze_archive(
    archive: Vec<u8>,
    archive_digest: &str,
    work_root: &Path,
    limits: SourceLimits,
) -> Result<FrozenSource> {
    let verified = DownloadedArchive::new(archive)
        .verify_archive_digest(archive_digest)
        .and_then(|archive| archive.verify_tree_digest(None, limits))
        .context("the directory is not a usable source")?;
    let closure_ref = verified
        .closure_ref("")
        .context("the directory is not a usable source")?;
    let scratch = work_root.join(format!(
        "source-{}-{}",
        std::process::id(),
        ATTEMPT_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&scratch)
        .with_context(|| format!("cannot create {}", scratch.display()))?;
    // Owned before materializing, so a failure part-way still removes it.
    let mut frozen = FrozenSource {
        closure_ref,
        root: PathBuf::new(),
        scratch: scratch.clone(),
    };
    frozen.root = verified
        .materialize(&scratch.join("tree"), "", limits)
        .context("the directory is not a usable source")?;
    Ok(frozen)
}

/// Pack a directory into an in-memory tar under a `source/` wrapper,
/// deterministically: sorted paths, zeroed metadata, no `.git`.
///
/// Symlinks are archived AS symlinks, not skipped: whether a link is
/// acceptable is the source module's decision, and it refuses them for every
/// source the same way.
fn tar_directory(root: &Path) -> Result<Vec<u8>> {
    let mut entries: Vec<(PathBuf, PathBuf, EntryKind)> = Vec::new();
    collect_tree(root, root, &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut builder = tar::Builder::new(Vec::new());
    append_dir(&mut builder, "source")?;
    for (relative, absolute, kind) in entries {
        let path = format!("source/{}", relative.to_string_lossy());
        if kind == EntryKind::Dir {
            append_dir(&mut builder, &path)?;
        } else if kind == EntryKind::Symlink {
            let target = std::fs::read_link(&absolute)
                .with_context(|| format!("cannot read link {}", absolute.display()))?;
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_mode(0o777);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_entry_type(tar::EntryType::Symlink);
            builder
                .append_link(&mut header, &path, &target)
                .with_context(|| format!("cannot add {path} to the source archive"))?;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Dir,
    File,
    Symlink,
}

fn collect_tree(
    root: &Path,
    dir: &Path,
    entries: &mut Vec<(PathBuf, PathBuf, EntryKind)>,
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
        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        // A link is recorded, never followed: the source module decides.
        if metadata.is_symlink() {
            entries.push((relative, path, EntryKind::Symlink));
        } else if metadata.is_dir() {
            entries.push((relative, path.clone(), EntryKind::Dir));
            collect_tree(root, &path, entries)?;
        } else if metadata.is_file() {
            entries.push((relative, path, EntryKind::File));
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

fn network_policy(policy: FormationNetworkPolicy) -> NetworkPolicy {
    match policy {
        FormationNetworkPolicy::Denied => NetworkPolicy::Denied,
        FormationNetworkPolicy::DependencyResolution => NetworkPolicy::DependencyResolution,
    }
}

/// This machine as a Runtime: the facts a Formation filter can need.
///
/// Only what admission reads: platform, containment, and whether the
/// toolchain root a build provisions into exists. Measured, never assumed.
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
pub(crate) fn host_triple() -> String {
    let arch = std::env::consts::ARCH;
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
