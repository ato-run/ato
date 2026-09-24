//! One attempt of one fixed Derivation against one fixed Contract, on this
//! Runtime — the single entry every Formation caller executes through.
//!
//! ```text
//! admission ──▶ start record (durable) ──▶ execute D ──▶ observe C over HTTP
//!                                                              │
//!            receipt (immutable, this attempt's evidence) ◀── C ⊨ K
//!                                                              │
//!                                     stop + cleanup, recorded separately
//! ```
//!
//! The caller decides WHICH candidate to try and what to do with a verified
//! one; it cannot decide whether the candidate satisfied K. Verification is
//! always from runtime observations of this attempt — a static candidate is
//! served and requested exactly like a process candidate is — and the
//! receipt states what was observed, whatever happens to the candidate
//! afterwards. Stopping and cleaning up are recorded beside the receipt, never
//! folded into it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ato_formation::authoring::HTTP_CONTRACT_VERIFIER;
use ato_formation::browser::{BrowserTarget, BrowserVerdict, BrowserVerificationReceipt};
use ato_formation::failure::{FailureStage, FormationFailure};
use ato_formation::request::{
    AttemptFailure, AttemptStatus, FormationAttempt, RealizationEvidence, RuntimeProfile,
};
use ato_formation::verify::{
    ContractVerification, ContractVerificationReceipt, RuntimeHttpObservation, RuntimeObservation,
    VerificationExecutionEvidence, verify_runtime,
};
use ato_portable_application::StaticApplicationServer;

use crate::admission::{admit, effects_name};
use crate::browser_verify::{BrowserVerification, verify_in_browser};
use crate::ephemeral::{
    RequiredObservation, RequiredPort, TemporaryRealization, TemporaryRealizationRequest,
};
use crate::executor::{AttemptExecution, AttemptExecutor, ExecutedCandidate};
use crate::job::{PlannedCandidate, observe_candidate};
use crate::journal::{AttemptJournal, BeginRefusal, StartIdentity};
use crate::sandbox::NetworkPolicy;

/// Bodies larger than this are not hashed into evidence.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Everything one attempt needs, already decided by the caller.
pub struct AttemptRequest<'a> {
    /// The request this attempt spends budget for. Every attempt of one
    /// request — retry, other Derivation, redelivery — carries the same id.
    pub request_id: &'a str,
    pub attempt_id: &'a str,
    /// How the caller names this candidate in evidence: `authored` or a
    /// preset id.
    pub label: &'a str,
    /// K and D, bound and frozen before this attempt.
    pub candidate: &'a PlannedCandidate,
    /// The K this attempt is recorded under.
    pub contract_ref: &'a str,
    pub source_root: &'a Path,
    pub runtime_id: &'a str,
    pub profile: &'a RuntimeProfile,
    pub network: NetworkPolicy,
    pub browser: Option<&'a BrowserVerification>,
    /// Scratch this attempt owns.
    pub attempt_root: &'a Path,
    /// The binary bwrap re-enters as `sandbox-exec`.
    pub shim: &'a Path,
}

/// What an attempt established.
pub struct AttemptOutcome {
    /// The evidence, as the caller reports it.
    pub attempt: FormationAttempt,
    /// Whether anything of the candidate may have run: true from the moment
    /// the start record was written, never inferred afterwards.
    pub execution_started: bool,
    /// The candidate, only when every observation of K was satisfied here
    /// and the candidate was stopped and cleaned up.
    pub verified: Option<ExecutedCandidate>,
}

/// Run one attempt through admission, the start record, execution,
/// observation and verification.
pub fn run_attempt(
    request: &AttemptRequest<'_>,
    executor: &dyn AttemptExecutor,
    journal: &AttemptJournal,
) -> AttemptOutcome {
    let planned = request.candidate;
    let mut attempt = FormationAttempt {
        candidate: request.label.to_owned(),
        attempt_id: Some(request.attempt_id.to_owned()),
        derivation_ref: Some(planned.derivation_ref.clone()),
        contract_ref: Some(request.contract_ref.to_owned()),
        runtime_id: request.runtime_id.to_owned(),
        status: AttemptStatus::Filtered,
        verification: None,
        base_contract_ref: (request.contract_ref != planned.contract_ref)
            .then(|| planned.contract_ref.clone()),
        realization: None,
        browser_verification: None,
        receipt: None,
        failure: None,
    };
    let not_run = |attempt: FormationAttempt, started: bool| AttemptOutcome {
        attempt,
        execution_started: started,
        verified: None,
    };

    if let Some(failure) = admit(request.profile, planned, request.network, request.browser) {
        attempt.failure = Some(failure);
        return not_run(attempt, false);
    }

    // ── start record ────────────────────────────────────────────────────────
    //
    // Before the first thing that could have an effect: staging, a build
    // step, a dependency fetch, a launch. No record, no start.
    let started = match journal.begin(
        request.request_id,
        request.attempt_id,
        StartIdentity {
            contract_ref: request.contract_ref.to_owned(),
            derivation_ref: planned.derivation_ref.clone(),
            runtime_id: request.runtime_id.to_owned(),
            effects: effects_name(planned.derivation.effects),
            network: network_name(request.network).to_owned(),
        },
    ) {
        Ok(started) => started,
        Err(refusal) => {
            let already = matches!(refusal, BeginRefusal::AlreadyStarted { .. });
            attempt.failure = Some(AttemptFailure {
                code: refusal.code().to_owned(),
                stage: "admission".to_owned(),
                message: bounded(&refusal.message()),
            });
            // A redelivered attempt did start — the first time.
            return not_run(attempt, already);
        }
    };
    attempt.status = AttemptStatus::Failed;

    let mut outcome = execute_and_verify(request, executor, attempt);
    let recorded = match (&outcome.verified, &outcome.attempt.failure) {
        (Some(_), _) => "verified".to_owned(),
        (None, Some(failure)) => failure.code.clone(),
        (None, None) => "failed".to_owned(),
    };
    if let Err(error) = started.finish(&recorded) {
        // The attempt ran, but its end is not durable: the next attempt of
        // this request will read it as UNKNOWN, which is the safe reading.
        eprintln!(
            "[formation] cannot record the end of attempt {}: {error:#}",
            request.attempt_id
        );
        outcome.verified = None;
        outcome.attempt.status = AttemptStatus::Failed;
        outcome.attempt.failure = Some(AttemptFailure {
            code: "attempt_record_unfinished".to_owned(),
            stage: "record".to_owned(),
            message: "the attempt ran and its end could not be recorded".to_owned(),
        });
    }
    outcome
}

fn execute_and_verify(
    request: &AttemptRequest<'_>,
    executor: &dyn AttemptExecutor,
    mut attempt: FormationAttempt,
) -> AttemptOutcome {
    let planned = request.candidate;
    let failed = |mut attempt: FormationAttempt, receipt| {
        attempt.receipt = receipt;
        AttemptOutcome {
            attempt,
            execution_started: true,
            verified: None,
        }
    };

    let executed = match executor.execute(&AttemptExecution {
        attempt_id: request.attempt_id,
        candidate: planned,
        source_root: request.source_root,
        attempt_root: request.attempt_root,
    }) {
        Ok(executed) => executed,
        Err(error) => {
            attempt.failure = Some(failure_of(&error));
            return failed(attempt, None);
        }
    };

    // ── observe ─────────────────────────────────────────────────────────────
    let input_refs = observe_candidate(&planned.derivation, &planned.projected, None).input_refs;
    let observed = match &executed {
        ExecutedCandidate::Process { workspace_root } => {
            observe_process(request, workspace_root, &mut attempt)
        }
        ExecutedCandidate::StaticWeb { output } => observe_static(output, planned, &mut attempt),
    };
    let (http, endpoint, cleanup) = match observed {
        Ok(observed) => observed,
        Err(error) => {
            // The candidate could not be observed at all: nothing is
            // verified, and guessing verdicts would invent evidence.
            attempt.failure = Some(AttemptFailure {
                code: "candidate_not_observable".to_owned(),
                stage: FailureStage::Verification.as_str().to_owned(),
                message: bounded(&format!("{error:#}")),
            });
            return failed(attempt, None);
        }
    };
    let runtime = RuntimeObservation {
        input_refs,
        http,
        instance_snapshot_ref: None,
    };
    let verification = verify_runtime(&planned.contract, &runtime);

    // The receipt covers the Contract's observations at this verification
    // point, and nothing that happens after it.
    let mut receipt = ContractVerificationReceipt::from_attempt(
        planned.contract_ref.clone(),
        planned.derivation_ref.clone(),
        &planned.contract,
        &runtime,
        verification.clone(),
    );
    receipt.execution = Some(VerificationExecutionEvidence {
        realization: match &executed {
            ExecutedCandidate::Process { .. } => "process",
            ExecutedCandidate::StaticWeb { .. } => "static_web",
        }
        .to_owned(),
        runtime_executable: None,
        runtime_version: None,
        pid: None,
        container_id: None,
        image: None,
        platform: Some(format!(
            "{}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        )),
        endpoint,
        run_id: None,
        lease_id: None,
        attempt_id: Some(request.attempt_id.to_owned()),
        request_id: Some(request.request_id.to_owned()),
        dependency_fetches: Vec::new(),
        portability_profile: None,
        embedded_oci_image_loaded: None,
        services: Vec::new(),
    });

    if let Some(failure) = verification_failure(&verification) {
        attempt.failure = Some(failure);
        attempt.verification = Some(verification);
        return failed(attempt, Some(receipt));
    }
    if let Some(failure) = browser_failure(request.browser, &attempt) {
        attempt.failure = Some(failure);
        attempt.verification = Some(verification);
        return failed(attempt, Some(receipt));
    }
    attempt.verification = Some(verification);

    // K was satisfied at the verification point; a candidate that could not
    // be stopped afterwards is still not one to keep, and the receipt still
    // says what was observed.
    if let Err(error) = cleanup {
        attempt.failure = Some(AttemptFailure {
            code: "candidate_cleanup_failed".to_owned(),
            stage: "cleanup".to_owned(),
            message: bounded(&format!("{error:#}")),
        });
        return failed(attempt, Some(receipt));
    }
    attempt.status = AttemptStatus::Verified;
    attempt.receipt = Some(receipt);
    AttemptOutcome {
        attempt,
        execution_started: true,
        verified: Some(executed),
    }
}

/// Every Contract observation must be Satisfied by this attempt. A verdict
/// that is not — failed, or undecided — is the attempt's failure.
fn verification_failure(verification: &ContractVerification) -> Option<AttemptFailure> {
    if verification.fully_satisfied() {
        return None;
    }
    Some(
        verification
            .failure()
            .map(|(id, code, detail)| AttemptFailure {
                code: code.to_owned(),
                stage: FailureStage::Verification.as_str().to_owned(),
                message: bounded(&format!("{id}: {detail}")),
            })
            .unwrap_or_else(|| AttemptFailure {
                code: "observation_undecided".to_owned(),
                stage: FailureStage::Verification.as_str().to_owned(),
                message: "an observation was not decided by this attempt; a Verified route \
                          needs every observation satisfied by the attempt itself"
                    .to_owned(),
            }),
    )
}

/// The acceptance prompt, when one was asked for: only a browser PASS lets
/// the candidate form. Fail and inconclusive are both "not formed".
fn browser_failure(
    browser: Option<&BrowserVerification>,
    attempt: &FormationAttempt,
) -> Option<AttemptFailure> {
    browser?;
    let (code, message) = match attempt
        .browser_verification
        .as_ref()
        .map(|receipt| (receipt.overall, receipt.reason.clone()))
    {
        Some((BrowserVerdict::Pass, _)) => return None,
        Some((BrowserVerdict::Fail, _)) => (
            "browser_contract_failed",
            "the candidate was observed in a browser and did not satisfy the acceptance prompt"
                .to_owned(),
        ),
        Some((BrowserVerdict::Inconclusive, reason)) => (
            "browser_contract_inconclusive",
            format!(
                "the browser verification did not reach a verdict{}",
                reason
                    .map(|reason| format!(": {reason}"))
                    .unwrap_or_default()
            ),
        ),
        None => (
            "browser_contract_inconclusive",
            "the candidate was not verified in a browser".to_owned(),
        ),
    };
    Some(AttemptFailure {
        code: code.to_owned(),
        stage: FailureStage::Verification.as_str().to_owned(),
        message: bounded(&message),
    })
}

/// The Contract's HTTP observations the Derivation exports a port for.
///
/// A requirement on a port the Derivation never exports is not probed — it
/// would measure a port nobody claimed — and the verifier fails it as
/// unobserved. Only GET is observed; anything else is left for the verifier
/// to refuse rather than be probed with the wrong method.
fn http_requirements(planned: &PlannedCandidate) -> Vec<RequiredObservation> {
    planned
        .contract
        .requirements
        .iter()
        .filter(|requirement| requirement.verifier == HTTP_CONTRACT_VERIFIER)
        .filter(|requirement| requirement.method.as_deref().is_none_or(|m| m == "GET"))
        .filter_map(|requirement| {
            let port_id = requirement.port.clone()?;
            planned
                .derivation
                .ports
                .iter()
                .any(|port| port.id == port_id)
                .then(|| RequiredObservation {
                    port_id,
                    path: requirement.path.clone().unwrap_or_else(|| "/".to_owned()),
                })
        })
        .collect()
}

type Observed = (Vec<RuntimeHttpObservation>, Option<String>, Result<()>);

/// Realize a process candidate on this Runtime, observe it, destroy it.
fn observe_process(
    request: &AttemptRequest<'_>,
    workspace_root: &Path,
    attempt: &mut FormationAttempt,
) -> Result<Observed> {
    let planned = request.candidate;
    let required: Vec<RequiredObservation> = http_requirements(planned);
    let mut ports: Vec<RequiredPort> = Vec::new();
    for observation in &required {
        let Some(guest_port) = planned
            .derivation
            .ports
            .iter()
            .find(|port| port.id == observation.port_id)
            .and_then(|port| port.guest_port)
        else {
            continue;
        };
        if !ports.iter().any(|port| port.port_id == observation.port_id) {
            ports.push(RequiredPort {
                port_id: observation.port_id.clone(),
                guest_port,
            });
        }
    }
    let required: Vec<RequiredObservation> = required
        .into_iter()
        .filter(|observation| ports.iter().any(|port| port.port_id == observation.port_id))
        .collect();

    let scratch = request.attempt_root.join("realization");
    let mut evidence = RealizationEvidence {
        executor: "runtime-process".to_owned(),
        containment: "bwrap+landlock".to_owned(),
        workspace: "disposable-copy, read-only at /app; /tmp is tmpfs".to_owned(),
        // The Runtime's process policy, whatever the build was allowed: no
        // egress, TCP bind only on the allocated host ports. A
        // `dependency-resolution` request widens the BUILD, never the run.
        build_network: network_name(request.network).to_owned(),
        candidate_network: "no-egress; tcp bind limited to allocated host ports".to_owned(),
        endpoints: BTreeMap::new(),
        destroyed: false,
    };
    let launched = TemporaryRealization::launch(&TemporaryRealizationRequest {
        workspace: workspace_root,
        scratch: &scratch,
        intent: &planned.intent,
        ports: &ports,
        shim: request.shim,
        attempt_id: request.attempt_id,
    });
    let realization = match launched {
        Ok(realization) => realization,
        Err(error) => {
            // Dropped on the error path: gone unless the Runtime said
            // otherwise, which it did loudly in the log.
            evidence.destroyed = !scratch.exists();
            attempt.realization = Some(evidence);
            return Err(error);
        }
    };
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
    let endpoint = realization
        .endpoints()
        .first()
        .map(|endpoint| format!("http://127.0.0.1:{}/", endpoint.host_port));
    let http = realization.observe(&required);
    // The browser drives the SAME realization, and only one that already
    // satisfies the typed observations: a candidate that fails its HTTP
    // Contract has nothing to show a browser.
    if let (Ok(http), Some(browser)) = (&http, request.browser) {
        let runtime = RuntimeObservation {
            input_refs: observe_candidate(&planned.derivation, &planned.projected, None).input_refs,
            http: http.clone(),
            instance_snapshot_ref: None,
        };
        if verify_runtime(&planned.contract, &runtime).fully_satisfied() {
            attempt.browser_verification = Some(browse(&realization, browser, request.runtime_id));
        }
    }
    let destroyed = realization
        .destroy()
        .context("the candidate could not be destroyed");
    evidence.destroyed = destroyed.is_ok();
    attempt.realization = Some(evidence);
    Ok((http?, endpoint, destroyed))
}

/// Serve a static candidate's produced bundle on loopback and request it —
/// the same observation a process candidate gets, from the bytes that would
/// be kept.
fn observe_static(
    output: &crate::static_lane::StaticFormationOutput,
    planned: &PlannedCandidate,
    attempt: &mut FormationAttempt,
) -> Result<Observed> {
    let manifest: ato_materializer_static_web::StaticWebManifestV1 =
        serde_json::from_slice(&output.bundle.manifest_bytes)
            .context("read the produced static web manifest")?;
    let blobs = output.bundle.bundle_root.join("blobs");
    let mut routes = BTreeMap::new();
    for (path, file) in &manifest.files {
        let (algorithm, hex) = file
            .blob
            .split_once(':')
            .with_context(|| format!("{path}: blob {} is not a digest", file.blob))?;
        let location: PathBuf = blobs.join(algorithm).join(hex);
        routes.insert(format!("/{path}"), (location, file.media_type.clone()));
    }
    let server = StaticApplicationServer::serve_routes(
        routes,
        format!("/{}", manifest.entry_path),
        manifest.routing.spa_fallback,
    )
    .map_err(|error| anyhow::anyhow!("cannot serve the static candidate: {error}"))?;
    let base = server.base_url();
    let mut evidence = RealizationEvidence {
        executor: "runtime-static-loopback".to_owned(),
        containment: "none; serves the produced bundle's files, runs nothing".to_owned(),
        workspace: "the produced static web bundle, read-only".to_owned(),
        build_network: "n/a".to_owned(),
        candidate_network: "loopback only".to_owned(),
        endpoints: BTreeMap::new(),
        destroyed: false,
    };
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let mut http = Vec::new();
    for observation in http_requirements(planned) {
        evidence.endpoints.insert(
            observation.port_id.clone(),
            "loopback static server".to_owned(),
        );
        let response = client
            .get(format!("{base}{}", observation.path))
            .send()
            .with_context(|| format!("GET {} failed", observation.path))?;
        let status = response.status().as_u16();
        let body = response.bytes().context("cannot read the response body")?;
        if body.len() > MAX_BODY_BYTES {
            anyhow::bail!(
                "GET {} returned more than {MAX_BODY_BYTES} bytes",
                observation.path
            );
        }
        http.push(RuntimeHttpObservation::from_response(
            observation.port_id,
            "GET",
            observation.path,
            status,
            &body,
        ));
    }
    drop(server);
    evidence.destroyed = true;
    attempt.realization = Some(evidence);
    Ok((http, Some(format!("{base}/")), Ok(())))
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

pub(crate) fn network_name(network: NetworkPolicy) -> &'static str {
    match network {
        NetworkPolicy::Denied => "denied",
        NetworkPolicy::DependencyResolution => "dependency-resolution",
    }
}

/// A failure a person can act on, recovered from the error's TYPE — the same
/// rule the hosted reporter follows: untyped errors stay anonymous because
/// their chains can carry paths and credentials.
pub(crate) fn failure_of(error: &anyhow::Error) -> AttemptFailure {
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
pub(crate) fn bounded(reason: &str) -> String {
    crate::api::bounded_reason(reason)
}
