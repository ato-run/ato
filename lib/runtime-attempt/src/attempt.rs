//! One attempt of one fixed Derivation against one fixed Contract, on this
//! Runtime — the single entry every caller that executes a Derivation goes
//! through: a Formation attempt, a Runtime Network ticket, a Run.
//!
//! ```text
//! admission ──▶ start record (durable) ──▶ realize ──▶ observe C over HTTP
//!                                                            │
//!          receipt (immutable, this attempt's evidence) ◀── C ⊨ K
//!                                                            │
//!                               stop, or hand off to the caller; recorded
//! ```
//!
//! The caller decides WHICH candidate to try and what to do with a verified
//! one; it cannot decide whether the candidate satisfied K. How the
//! candidate comes to be running is the realizer's (build from source,
//! unpack a `.capsule`). Verification is always from runtime observations of
//! this attempt, and the receipt states what was observed, whatever happens
//! to the candidate afterwards.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ato_formation::browser::{BrowserTarget, BrowserVerdict, BrowserVerificationReceipt};
use ato_formation::failure::{FailureStage, FormationFailure};
use ato_formation::request::{
    AttemptFailure, AttemptOutcomes, AttemptStatus, FormationAttempt, Outcome, RuntimeProfile,
};
use ato_formation::verify::{ContractVerification, RuntimeHttpObservation};

use crate::admission::{EffectAuthorization, admit, effects_name};
use crate::browser_verify::{BrowserVerification, verify_in_browser};
use crate::build_sandbox::NetworkPolicy;
use crate::executor::ExecutedCandidate;
use crate::journal::{
    AttemptLedger, AttemptPermit, AttemptRecordState, BeginRefusal, StartIdentity,
};
use crate::realize::{CandidateRealizer, LiveCandidate, RealizeFailure, RunningCandidate};
use crate::spec::AttemptSpec;
pub use crate::verification::ReceiptContext;
use crate::verification::{
    HTTP_OBSERVATION_METHOD, RequiredObservation, VerificationPoint, required_http_observations,
    verify_observed_candidate,
};

/// Bodies larger than this are not hashed into evidence.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// How long a candidate has to answer its first observation.
const OBSERVATION_DEADLINE: Duration = Duration::from_secs(60);
/// Per-request budget once it answers.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Everything one attempt needs, already decided by the caller.
pub struct AttemptRequest<'a> {
    /// The request this attempt spends budget for. Every attempt of one
    /// request — retry, other Derivation, redelivery — carries the same id.
    pub request_id: &'a str,
    pub attempt_id: &'a str,
    /// How the caller names this candidate in evidence: `authored`, a
    /// preset id, `portable`.
    pub label: &'a str,
    /// K and D, bound and frozen before this attempt.
    pub spec: &'a AttemptSpec<'a>,
    /// The K this attempt is recorded under (the effective K when a browser
    /// Contract is part of it).
    pub contract_ref: &'a str,
    pub runtime_id: &'a str,
    pub profile: &'a RuntimeProfile,
    /// Who authorized the Derivation's effects.
    pub authorization: EffectAuthorization<'a>,
    /// What a build may reach, recorded with the start. A Run builds nothing.
    pub network: NetworkPolicy,
    pub browser: Option<&'a BrowserVerification>,
    /// Scratch this attempt owns.
    pub attempt_root: &'a Path,
    /// What happens to a verified candidate after the receipt.
    pub continuation: Continuation,
    /// What the receipt names besides K, D and this attempt.
    pub receipt: ReceiptContext<'a>,
    /// Set when the caller is being stopped: observation gives up instead of
    /// waiting out its deadline.
    pub interrupt: Option<&'a AtomicBool>,
}

/// What happens to the running candidate once it has been verified.
///
/// The receipt is fixed at the verification point either way; this only
/// decides who owns the candidate afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continuation {
    /// Stop it and remove its runtime scratch (a Formation attempt).
    Stop,
    /// Keep it running and hand it to the caller (a Run). A candidate that
    /// was not verified is stopped regardless: there is nothing to hand off.
    HandOff,
}

/// What an attempt established.
pub struct AttemptOutcome {
    /// The evidence, as the caller reports it.
    pub attempt: FormationAttempt,
    /// What this Runtime's durable record says about the attempt: not
    /// started, finished, or started and never finished (UNKNOWN). Read from
    /// the record, never inferred from how the attempt ended.
    pub attempt_record: AttemptRecordState,
    /// What a Formation keeps, only when every observation of K was
    /// satisfied here and, under [`Continuation::Stop`], the candidate was
    /// stopped and cleaned up.
    pub verified: Option<ExecutedCandidate>,
    /// The running candidate, only under [`Continuation::HandOff`] and only
    /// when it was verified.
    pub live: Option<LiveCandidate>,
    /// The error behind a failure, for a caller that reports it in full to
    /// the person who asked (a Run). Never serialized: the attempt record
    /// carries the bounded sentence.
    pub error: Option<anyhow::Error>,
}

impl AttemptOutcome {
    /// Conservative compatibility bit: historical execution or a search
    /// barrier cannot be ruled out. The typed record is the authority.
    pub fn execution_started(&self) -> bool {
        self.attempt_record.execution_started()
    }
}

/// Run one attempt through admission, the start record, realization,
/// observation and verification.
pub fn run_attempt(
    request: &AttemptRequest<'_>,
    realizer: &dyn CandidateRealizer,
    journal: &dyn AttemptLedger,
) -> AttemptOutcome {
    run_reserved_attempt(
        request,
        realizer,
        journal.acquire(request.request_id, request.attempt_id),
    )
}

/// Consume the permit acquired before source/planning. It remains held until
/// every refusal or execution result is final, so another delivery cannot race it.
pub fn run_reserved_attempt(
    request: &AttemptRequest<'_>,
    realizer: &dyn CandidateRealizer,
    permit: std::result::Result<Box<dyn AttemptPermit>, BeginRefusal>,
) -> AttemptOutcome {
    let spec = request.spec;
    let mut attempt = FormationAttempt {
        candidate: request.label.to_owned(),
        attempt_id: Some(request.attempt_id.to_owned()),
        derivation_ref: Some(spec.derivation_ref.to_owned()),
        contract_ref: Some(request.contract_ref.to_owned()),
        runtime_id: request.runtime_id.to_owned(),
        status: AttemptStatus::Filtered,
        verification: None,
        base_contract_ref: (request.contract_ref != spec.contract_ref)
            .then(|| spec.contract_ref.to_owned()),
        realization: None,
        browser_verification: None,
        receipt: None,
        outcomes: AttemptOutcomes::not_run("not started"),
        failure: None,
    };
    let not_run = |mut attempt: FormationAttempt, record: AttemptRecordState| {
        let reason = attempt
            .failure
            .as_ref()
            .map(|failure| failure.code.clone())
            .unwrap_or_default();
        attempt.outcomes = AttemptOutcomes::not_run(&reason);
        if record.execution_started() {
            // Historical activity or the blocking sibling cannot be ruled
            // out here; this delivery cannot claim cleanup.
            attempt.outcomes.cleanup = Outcome::not_attempted(&reason);
        }
        AttemptOutcome {
            attempt,
            attempt_record: record,
            verified: None,
            live: None,
            error: None,
        }
    };

    let mut permit = match permit {
        Ok(permit) => permit,
        Err(refusal) => {
            attempt.failure = Some(AttemptFailure {
                code: refusal.code().to_owned(),
                stage: "record".to_owned(),
                message: bounded(&refusal.message()),
            });
            return not_run(attempt, refusal.record_state());
        }
    };

    if let Some(failure) = admit(spec, request.authorization, request.browser)
        .or_else(|| realizer.admit(request.profile))
    {
        attempt.failure = Some(failure);
        return not_run(attempt, AttemptRecordState::NotStarted);
    }

    // ── start record ────────────────────────────────────────────────────────
    //
    // Before the first thing that could have an effect: staging, a build
    // step, a dependency fetch, a launch. No record, no start.
    let started = match permit.start(StartIdentity {
        contract_ref: request.contract_ref.to_owned(),
        derivation_ref: spec.derivation_ref.to_owned(),
        runtime_id: request.runtime_id.to_owned(),
        effects: effects_name(spec.derivation.effects),
        network: network_name(request.network).to_owned(),
        authorization: request.authorization.name().to_owned(),
    }) {
        Ok(started) => started,
        Err(refusal) => {
            let record = refusal.record_state();
            attempt.failure = Some(AttemptFailure {
                code: refusal.code().to_owned(),
                stage: "admission".to_owned(),
                message: bounded(&refusal.message()),
            });
            return not_run(attempt, record);
        }
    };
    attempt.status = AttemptStatus::Failed;

    let mut outcome = realize_and_verify(request, realizer, attempt);
    let recorded = match (outcome.attempt.status, &outcome.attempt.failure) {
        (AttemptStatus::Verified, _) => "verified".to_owned(),
        (_, Some(failure)) => failure.code.clone(),
        (_, None) => "failed".to_owned(),
    };
    match started.finish(&recorded) {
        Ok(()) => outcome.attempt_record = AttemptRecordState::Finished,
        Err(error) => unfinished(request, &mut outcome, error),
    }
    outcome
}

/// The attempt ran, but its end is not durable: the next attempt of this
/// request will read it as UNKNOWN, which is the safe reading, and so does
/// whoever reads this outcome (`attempt_record` stays `StartedUnfinished`).
fn unfinished(request: &AttemptRequest<'_>, outcome: &mut AttemptOutcome, error: anyhow::Error) {
    eprintln!(
        "[formation] cannot record the end of attempt {}: {error:#}",
        request.attempt_id
    );
    outcome.verified = None;
    if let Some(live) = outcome.live.take() {
        // Not handed off: an attempt whose end is not durable keeps nothing
        // running.
        outcome.attempt.outcomes.cleanup = match live.stop() {
            Ok(()) => Outcome::succeeded(),
            Err(error) => Outcome::failed(&bounded(&format!("{error:#}"))),
        };
    }
    outcome.attempt.status = AttemptStatus::Failed;
    outcome.attempt.failure = Some(AttemptFailure {
        code: "attempt_record_unfinished".to_owned(),
        stage: "record".to_owned(),
        message: "the attempt ran and its end could not be recorded".to_owned(),
    });
    outcome.error = Some(error.context("the attempt ran and its end could not be recorded"));
}

fn realize_and_verify(
    request: &AttemptRequest<'_>,
    realizer: &dyn CandidateRealizer,
    mut attempt: FormationAttempt,
) -> AttemptOutcome {
    let spec = request.spec;
    let failed = |attempt: FormationAttempt, error: anyhow::Error| AttemptOutcome {
        attempt,
        // The start is durable; run_attempt records the finish.
        attempt_record: AttemptRecordState::StartedUnfinished,
        verified: None,
        live: None,
        error: Some(error),
    };

    let realized = match realizer.realize(request.attempt_id, request.attempt_root) {
        Ok(realized) => realized,
        Err(RealizeFailure::Execution(error)) => {
            let failure = failure_of(&error);
            attempt.outcomes.seal = Outcome::not_attempted(&failure.code);
            attempt.outcomes.runtime_verification = Outcome::not_attempted(&failure.code);
            attempt.outcomes.cleanup = Outcome::not_applicable("no candidate was realized");
            attempt.outcomes.publication = Outcome::not_attempted(&failure.code);
            attempt.failure = Some(failure);
            return failed(attempt, error);
        }
        Err(RealizeFailure::Launch { error, evidence }) => {
            attempt.outcomes.publication = Outcome::not_attempted("candidate_not_observable");
            attempt.realization = evidence.map(|evidence| *evidence);
            return not_observable(attempt, error);
        }
    };
    // Publication is the caller's for what a Formation keeps; a candidate
    // that runs an existing artifact publishes nothing.
    attempt.outcomes.publication = match realized.kept {
        Some(_) => Outcome::not_attempted("decided by the caller"),
        None => Outcome::not_applicable("running an existing published bundle"),
    };
    attempt.realization = realized.evidence;
    let mut candidate = realized.candidate;

    // ── observe ─────────────────────────────────────────────────────────────
    let http = match observe_http(
        candidate.as_mut(),
        &required_http_observations(spec),
        request.interrupt,
    ) {
        Ok(http) => http,
        Err(error) => {
            let stopped = candidate.stop();
            if let Some(evidence) = attempt.realization.as_mut() {
                evidence.destroyed = stopped.is_ok();
            }
            return not_observable(attempt, error);
        }
    };
    // The receipt covers the Contract's observations at this verification
    // point, and nothing that happens after it.
    let mut execution = realized.execution;
    execution.attempt_id = Some(request.attempt_id.to_owned());
    execution.request_id = Some(request.request_id.to_owned());
    let VerificationPoint {
        verification,
        receipt,
    } = verify_observed_candidate(spec, http, execution, &request.receipt);

    // The browser drives the SAME candidate, and only one that already
    // satisfies the typed observations: a candidate that fails its HTTP
    // Contract has nothing to show a browser.
    if let Some(browser) = request.browser
        && verification.fully_satisfied()
    {
        attempt.browser_verification = Some(browse(
            candidate.endpoints(),
            browser,
            request.runtime_id,
            request.attempt_id,
        ));
    }

    let failure =
        verification_failure(&verification).or_else(|| browser_failure(request.browser, &attempt));
    let error = failure
        .as_ref()
        .map(|failure| anyhow::anyhow!("{}: {}", failure.code, failure.message));
    attempt.verification = Some(verification);
    attempt.receipt = Some(receipt);
    let (verified, live) = after_verification(
        &mut attempt,
        failure,
        candidate,
        request.continuation,
        |candidate| candidate.stop(),
    );
    AttemptOutcome {
        error: match (&attempt.failure, error) {
            (Some(_), Some(error)) => Some(error),
            (Some(failure), None) => Some(anyhow::anyhow!("{}: {}", failure.code, failure.message)),
            (None, _) => None,
        },
        attempt,
        // The start is durable; run_attempt records the finish.
        attempt_record: AttemptRecordState::StartedUnfinished,
        verified: if verified { realized.kept } else { None },
        live: live.map(LiveCandidate::new),
    }
}

/// The candidate could not be observed at all: nothing is verified, and
/// guessing verdicts would invent evidence. Whatever was realized is gone
/// (see `evidence.destroyed`).
fn not_observable(mut attempt: FormationAttempt, error: anyhow::Error) -> AttemptOutcome {
    attempt.outcomes.seal = Outcome::failed("candidate_not_observable");
    attempt.outcomes.runtime_verification = Outcome::failed("candidate_not_observable");
    attempt.outcomes.cleanup = match &attempt.realization {
        Some(evidence) if evidence.destroyed => Outcome::succeeded(),
        Some(_) => Outcome::failed("the realization was not confirmed gone"),
        None => Outcome::not_applicable("no candidate was realized"),
    };
    attempt.failure = Some(AttemptFailure {
        code: "candidate_not_observable".to_owned(),
        stage: FailureStage::Verification.as_str().to_owned(),
        message: bounded(&format!("{error:#}")),
    });
    AttemptOutcome {
        attempt,
        // The start is durable; run_attempt records the finish.
        attempt_record: AttemptRecordState::StartedUnfinished,
        verified: None,
        live: None,
        error: Some(error),
    }
}

/// GET every required path through the endpoint its logical port answers
/// on, waiting for the candidate to accept connections — and giving up as
/// soon as it exits, the caller is interrupted, or the deadline passes. A
/// port the candidate did not realize is not observed; the verifier fails
/// that observation as missing.
fn observe_http(
    candidate: &mut dyn RunningCandidate,
    required: &[RequiredObservation],
    interrupt: Option<&AtomicBool>,
) -> Result<Vec<RuntimeHttpObservation>> {
    let client = reqwest::blocking::Client::builder()
        // A candidate has no business redirecting its Contract observation
        // somewhere else; what it answers directly is what is measured.
        .redirect(reqwest::redirect::Policy::none())
        .timeout(REQUEST_TIMEOUT)
        .build()?;
    let deadline = Instant::now() + OBSERVATION_DEADLINE;
    let mut observed = Vec::with_capacity(required.len());
    for observation in required {
        let Some(base) = candidate.endpoints().get(&observation.port_id).cloned() else {
            continue;
        };
        let url = format!("{base}{}", observation.path);
        let response = loop {
            if interrupt.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                anyhow::bail!("interrupted before the Contract observations completed");
            }
            match client.get(&url).send() {
                Ok(response) => break response,
                Err(error) => {
                    if let Some(exit) = candidate.exited()? {
                        anyhow::bail!("the candidate exited before it could be observed: {exit}");
                    }
                    if Instant::now() >= deadline {
                        return Err(error).with_context(|| {
                            format!("the candidate did not become reachable at {url}")
                        });
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        };
        let status = response.status().as_u16();
        let body = response.bytes().context("cannot read the response body")?;
        if body.len() > MAX_BODY_BYTES {
            anyhow::bail!(
                "GET {} returned more than {MAX_BODY_BYTES} bytes",
                observation.path
            );
        }
        observed.push(RuntimeHttpObservation::from_response(
            observation.port_id.clone(),
            HTTP_OBSERVATION_METHOD,
            observation.path.clone(),
            status,
            &body,
        ));
    }
    Ok(observed)
}

/// Everything after the verification point: record what K's verdicts
/// established, then stop or hand off the candidate and record that
/// separately. Nothing here touches the receipt.
///
/// Returns whether the candidate counts as verified-and-kept (the legacy
/// `AttemptStatus::Verified`), and the candidate when it was handed off.
/// `stop` is how the candidate is stopped — a parameter so the independence
/// of the outcomes can be tested with a stop that fails.
fn after_verification<L>(
    attempt: &mut FormationAttempt,
    failure: Option<AttemptFailure>,
    live: L,
    continuation: Continuation,
    stop: impl FnOnce(L) -> Result<()>,
) -> (bool, Option<L>) {
    attempt.outcomes.runtime_verification = match &failure {
        None => Outcome::succeeded(),
        Some(failure) => Outcome::failed(&failure.code),
    };
    // A Formation attempt on a Runtime seals only what it fully verified.
    attempt.outcomes.seal = attempt.outcomes.runtime_verification.clone();

    // Handed off only when verified and asked for; otherwise stopped.
    let hand_off = failure.is_none() && continuation == Continuation::HandOff;
    let (live, stopped) = if hand_off {
        attempt.outcomes.cleanup = Outcome::not_attempted("handed_off");
        (Some(live), Ok(()))
    } else {
        let stopped = stop(live);
        attempt.outcomes.cleanup = match &stopped {
            Ok(()) => Outcome::succeeded(),
            Err(error) => Outcome::failed(&bounded(&format!("{error:#}"))),
        };
        (None, stopped)
    };
    if let Some(evidence) = attempt.realization.as_mut() {
        evidence.destroyed = !hand_off && stopped.is_ok();
    }

    if let Some(failure) = failure {
        attempt.failure = Some(failure);
        return (false, None);
    }
    // K was satisfied at the verification point; a candidate that could not
    // be stopped afterwards is still not one to keep. The receipt and
    // `runtime_verification` still say what was observed.
    if let Err(error) = stopped {
        attempt.failure = Some(AttemptFailure {
            code: "candidate_cleanup_failed".to_owned(),
            stage: "cleanup".to_owned(),
            message: bounded(&format!("{error:#}")),
        });
        return (false, None);
    }
    attempt.status = AttemptStatus::Verified;
    (true, live)
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

/// Verify the running candidate against the browser Contract, through the
/// endpoint the realization reports — never a guessed guest port.
fn browse(
    endpoints: &BTreeMap<String, String>,
    browser: &BrowserVerification,
    runtime_id: &str,
    attempt_id: &str,
) -> BrowserVerificationReceipt {
    let target = |endpoint: String| BrowserTarget {
        runtime_id: runtime_id.to_owned(),
        endpoint,
        attempt_id: Some(attempt_id.to_owned()),
    };
    match endpoints.values().collect::<Vec<_>>().as_slice() {
        [base] => verify_in_browser(browser, target(format!("{base}/"))),
        all => BrowserVerificationReceipt::unavailable(
            &browser.contract,
            target(String::new()),
            "none",
            &format!(
                "browser_endpoint_ambiguous: the candidate exposes {} observed ports; v0 \
                 verifies a candidate with exactly one",
                all.len()
            ),
        ),
    }
}

pub fn network_name(network: NetworkPolicy) -> &'static str {
    match network {
        NetworkPolicy::Denied => "denied",
        NetworkPolicy::DependencyResolution => "dependency-resolution",
    }
}

/// A failure a person can act on, recovered from the error's TYPE — the same
/// rule the hosted reporter follows: untyped errors stay anonymous because
/// their chains can carry paths and credentials.
pub fn failure_of(error: &anyhow::Error) -> AttemptFailure {
    // The requester gets a typed code and one sentence; the operator of this
    // Runtime gets the cause, bounded, on the worker's own log.
    // Head and tail: what failed is named first, and why is at the end.
    let detail = format!("{error:#}");
    let floor = |mut at: usize| {
        while !detail.is_char_boundary(at) {
            at -= 1;
        }
        at
    };
    if detail.len() <= 4000 {
        eprintln!("[formation] attempt failed: {detail}");
    } else {
        let head = floor(1000);
        let tail = floor(detail.len() - 3000);
        eprintln!(
            "[formation] attempt failed: {} … {}",
            &detail[..head],
            &detail[tail..]
        );
    }
    match error.downcast_ref::<FormationFailure>() {
        Some(failure) => AttemptFailure {
            code: failure.code.clone(),
            stage: failure.stage.as_str().to_owned(),
            message: failure.bounded_message(crate::text::FAILURE_REASON_LIMIT),
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
pub fn bounded(reason: &str) -> String {
    crate::text::bounded_reason(reason)
}

#[cfg(test)]
mod tests {
    use ato_formation::authoring::{
        BoundContract, BoundRequirement, HTTP_CONTRACT_VERIFIER,
        INSTANCE_SNAPSHOT_CONTRACT_VERIFIER, WORKSPACE_CONTRACT_VERIFIER,
    };
    use ato_formation::verify::{
        ContractVerificationReceipt, RuntimeObservation, VerificationTargetKind, verify_runtime,
    };

    use crate::journal::AttemptJournal;
    use ato_formation::request::{OutcomeState, RealizationEvidence};

    use super::*;

    /// An attempt whose K was fully satisfied: one HTTP observation, its
    /// receipt, and a realization that has not been stopped yet.
    fn verified_attempt() -> FormationAttempt {
        let contract = BoundContract {
            schema: "ato.contract/1".to_owned(),
            requirements: vec![BoundRequirement {
                id: "root".to_owned(),
                verifier: HTTP_CONTRACT_VERIFIER.to_owned(),
                port: Some("app.http".to_owned()),
                method: Some("GET".to_owned()),
                path: Some("/".to_owned()),
                status: Some(200),
                body_digest: None,
                input: None,
                digest: None,
            }],
        };
        let runtime = RuntimeObservation {
            input_refs: BTreeMap::new(),
            http: vec![RuntimeHttpObservation::from_response(
                "app.http", "GET", "/", 200, b"ok",
            )],
            instance_snapshot_ref: None,
        };
        let verification = verify_runtime(&contract, &runtime);
        let receipt = ContractVerificationReceipt::from_attempt(
            "sha256:k",
            "sha256:d",
            &contract,
            &runtime,
            verification.clone(),
        );
        assert!(receipt.fully_satisfied);
        FormationAttempt {
            candidate: "authored".to_owned(),
            attempt_id: Some("att".to_owned()),
            derivation_ref: Some("sha256:d".to_owned()),
            contract_ref: Some("sha256:k".to_owned()),
            base_contract_ref: None,
            runtime_id: "local".to_owned(),
            status: AttemptStatus::Failed,
            verification: Some(verification),
            realization: Some(RealizationEvidence {
                executor: "runtime-process".to_owned(),
                containment: "bwrap+landlock".to_owned(),
                workspace: "disposable-copy".to_owned(),
                build_network: "denied".to_owned(),
                candidate_network: "no-egress".to_owned(),
                endpoints: BTreeMap::new(),
                destroyed: false,
            }),
            browser_verification: None,
            receipt: Some(receipt),
            outcomes: AttemptOutcomes::not_run("not started"),
            failure: None,
        }
    }

    #[test]
    fn a_cleanup_failure_does_not_unverify_what_was_verified() {
        let mut attempt = verified_attempt();
        let receipt = attempt.receipt.clone();
        let (kept, live) = after_verification(&mut attempt, None, (), Continuation::Stop, |()| {
            Err(anyhow::anyhow!(
                "the realization scratch could not be removed"
            ))
        });
        // K was satisfied, and still is on record.
        assert_eq!(
            attempt.outcomes.runtime_verification.state,
            OutcomeState::Succeeded
        );
        assert_eq!(attempt.outcomes.seal.state, OutcomeState::Succeeded);
        assert_eq!(attempt.receipt, receipt, "the receipt is untouched");
        assert!(attempt.receipt.as_ref().unwrap().fully_satisfied);
        // Cleanup failed, on its own field.
        assert_eq!(attempt.outcomes.cleanup.state, OutcomeState::Failed);
        assert!(
            attempt
                .outcomes
                .cleanup
                .reason
                .as_deref()
                .unwrap()
                .contains("scratch could not be removed")
        );
        assert!(!attempt.realization.as_ref().unwrap().destroyed);
        // The legacy status may say Failed: a candidate that could not be
        // stopped is not one to keep. The outcomes keep the two apart.
        assert!(!kept);
        assert!(live.is_none());
        assert_eq!(attempt.status, AttemptStatus::Failed);
        assert_eq!(
            attempt.failure.as_ref().unwrap().code,
            "candidate_cleanup_failed"
        );
    }

    /// A realizer that only counts how often it was asked to run something.
    struct CountingRealizer(std::cell::Cell<u32>);

    impl CandidateRealizer for CountingRealizer {
        fn admit(&self, _profile: &RuntimeProfile) -> Option<AttemptFailure> {
            None
        }

        fn realize(
            &self,
            _attempt_id: &str,
            _attempt_root: &Path,
        ) -> Result<crate::realize::Realized, RealizeFailure> {
            self.0.set(self.0.get() + 1);
            Err(RealizeFailure::Execution(anyhow::anyhow!("nothing to run")))
        }
    }

    #[test]
    fn a_restarted_or_redelivered_attempt_is_never_realized_twice() {
        let contract = BoundContract {
            schema: "ato.contract/1".to_owned(),
            requirements: Vec::new(),
        };
        let derivation: ato_formation::authoring::BoundDerivation =
            serde_json::from_value(serde_json::json!({
                "schema": "ato.derivation/1",
                "inputs": [], "runtimes": {}, "steps": [], "ports": [], "state": [],
                "effects": "pure"
            }))
            .unwrap();
        let spec = AttemptSpec {
            contract: &contract,
            contract_ref: "sha256:k",
            derivation: &derivation,
            derivation_ref: "sha256:d",
            shape: crate::spec::CandidateShape::Process,
            input_refs: BTreeMap::new(),
            instance_snapshot_ref: None,
        };
        let scratch = tempfile::tempdir().unwrap();
        // One journal directory, read again as a restarted process would.
        let records = scratch.path().join("records");
        let realizer = CountingRealizer(std::cell::Cell::new(0));
        let attempt = |attempt_id: &str| {
            run_attempt(
                &AttemptRequest {
                    request_id: "run-1",
                    attempt_id,
                    label: "portable",
                    spec: &spec,
                    contract_ref: "sha256:k",
                    runtime_id: "local",
                    profile: &RuntimeProfile::default(),
                    authorization: EffectAuthorization::UserInvoked {
                        derivation_ref: "sha256:d",
                    },
                    network: NetworkPolicy::Denied,
                    browser: None,
                    attempt_root: scratch.path(),
                    continuation: Continuation::HandOff,
                    receipt: ReceiptContext::formation(),
                    interrupt: None,
                },
                &realizer,
                &AttemptJournal::new(&records),
            )
        };
        let first = attempt("run-1");
        assert!(first.execution_started());
        assert_eq!(first.attempt_record, AttemptRecordState::Finished);
        assert_eq!(realizer.0.get(), 1);

        let again = attempt("run-1");
        assert_eq!(realizer.0.get(), 1, "the same attempt was realized twice");
        assert!(again.execution_started(), "it did start — the first time");
        // Its record says the first run finished; this delivery ran nothing.
        assert_eq!(again.attempt_record, AttemptRecordState::Finished);
        assert_eq!(
            again.attempt.failure.as_ref().unwrap().code,
            "attempt_already_started"
        );
        assert!(again.live.is_none());
    }

    // ── what the durable record says, as the attempt reports it ─────────────

    /// K: `GET /` on `app.http` answers `status`.
    fn served_route(status: u16) -> (BoundContract, ato_formation::authoring::BoundDerivation) {
        let contract = BoundContract {
            schema: "ato.contract/1".to_owned(),
            requirements: vec![BoundRequirement {
                id: "root".to_owned(),
                verifier: HTTP_CONTRACT_VERIFIER.to_owned(),
                port: Some("app.http".to_owned()),
                method: Some("GET".to_owned()),
                path: Some("/".to_owned()),
                status: Some(status),
                body_digest: None,
                input: None,
                digest: None,
            }],
        };
        let derivation = serde_json::from_value(serde_json::json!({
            "schema": "ato.derivation/1",
            "inputs": [], "runtimes": {}, "steps": [],
            "ports": [{ "id": "app.http", "protocol": "http", "from": "process" }],
            "state": [],
            "effects": "pure"
        }))
        .unwrap();
        (contract, derivation)
    }

    /// A candidate that answers every request `200 ok` until it is stopped.
    struct Answering {
        endpoints: BTreeMap<String, String>,
        stopped: std::sync::Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for Answering {
        fn drop(&mut self) {
            self.stopped.store(true, Ordering::SeqCst);
            if let Some(thread) = self.thread.take() {
                thread.join().expect("test HTTP server stopped");
            }
        }
    }

    impl RunningCandidate for Answering {
        fn endpoints(&self) -> &BTreeMap<String, String> {
            &self.endpoints
        }
        fn exited(&mut self) -> Result<Option<String>> {
            Ok(None)
        }
        fn stop(self: Box<Self>) -> Result<()> {
            self.stopped.store(true, Ordering::SeqCst);
            Ok(())
        }
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    /// Realizes an [`Answering`] candidate, and says whether it was stopped.
    #[derive(Default)]
    struct AnsweringRealizer {
        stopped: std::sync::Arc<AtomicBool>,
    }

    impl CandidateRealizer for AnsweringRealizer {
        fn admit(&self, _profile: &RuntimeProfile) -> Option<AttemptFailure> {
            None
        }

        fn realize(
            &self,
            _attempt_id: &str,
            _attempt_root: &Path,
        ) -> Result<crate::realize::Realized, RealizeFailure> {
            let listener =
                std::net::TcpListener::bind("127.0.0.1:0").map_err(anyhow::Error::from)?;
            let port = listener.local_addr().map_err(anyhow::Error::from)?.port();
            listener
                .set_nonblocking(true)
                .map_err(anyhow::Error::from)?;
            let stopped = self.stopped.clone();
            let thread = std::thread::spawn(move || {
                use std::io::{Read as _, Write as _};
                while !stopped.load(Ordering::SeqCst) {
                    let mut stream = match listener.accept() {
                        Ok((stream, _)) => stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(_) => return,
                    };
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut request = [0_u8; 1024];
                    let _ = stream.read(&mut request);
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                    );
                }
            });
            Ok(crate::realize::Realized {
                candidate: Box::new(Answering {
                    endpoints: BTreeMap::from([(
                        "app.http".to_owned(),
                        format!("http://127.0.0.1:{port}"),
                    )]),
                    stopped: self.stopped.clone(),
                    thread: Some(thread),
                }),
                evidence: None,
                execution: serde_json::from_value(serde_json::json!({ "realization": "test" }))
                    .unwrap(),
                kept: None,
            })
        }
    }

    /// A ledger that makes the start durable and cannot make the finish so —
    /// the failure a full disk or a lost mount leaves behind.
    struct FinishFails(AttemptJournal);

    struct UnfinishableRecord;

    impl crate::journal::StartedRecord for UnfinishableRecord {
        fn finish(self: Box<Self>, _outcome: &str) -> Result<()> {
            Err(anyhow::anyhow!("no space left on device"))
        }
    }

    struct FinishFailsPermit(Box<dyn AttemptPermit>);
    impl AttemptPermit for FinishFailsPermit {
        fn start(
            &mut self,
            identity: StartIdentity,
        ) -> std::result::Result<Box<dyn crate::journal::StartedRecord>, BeginRefusal> {
            drop(self.0.start(identity)?);
            Ok(Box::new(UnfinishableRecord))
        }
    }
    impl AttemptLedger for FinishFails {
        fn acquire(
            &self,
            request_id: &str,
            attempt_id: &str,
        ) -> std::result::Result<Box<dyn AttemptPermit>, BeginRefusal> {
            Ok(Box::new(FinishFailsPermit(
                self.0.acquire(request_id, attempt_id)?,
            )))
        }
    }

    fn served_attempt(
        status: u16,
        authorization: EffectAuthorization<'_>,
        continuation: Continuation,
        realizer: &dyn CandidateRealizer,
        journal: &dyn AttemptLedger,
    ) -> AttemptOutcome {
        let (contract, derivation) = served_route(status);
        let spec = AttemptSpec {
            contract: &contract,
            contract_ref: "sha256:k",
            derivation: &derivation,
            derivation_ref: "sha256:d",
            shape: crate::spec::CandidateShape::Process,
            input_refs: BTreeMap::new(),
            instance_snapshot_ref: None,
        };
        let scratch = tempfile::tempdir().unwrap();
        run_attempt(
            &AttemptRequest {
                request_id: "satisfy-1",
                attempt_id: "attempt-1",
                label: "authored",
                spec: &spec,
                contract_ref: "sha256:k",
                runtime_id: "local",
                profile: &RuntimeProfile::default(),
                authorization,
                network: NetworkPolicy::Denied,
                browser: None,
                attempt_root: scratch.path(),
                continuation,
                receipt: ReceiptContext::formation(),
                interrupt: None,
            },
            realizer,
            journal,
        )
    }

    /// Someone who observes a candidate it did not run — the Hosted
    /// verifier — decides K through the same verification point. Given what
    /// `run_attempt` observed, it reaches the same verdicts and states the
    /// same receipt, satisfied or not.
    #[test]
    fn run_attempt_and_an_outside_observer_reach_the_same_verification_point() {
        for status in [200, 404] {
            let (mut contract, derivation) = served_route(status);
            contract.requirements.push(BoundRequirement {
                id: "source-identity".to_owned(),
                verifier: WORKSPACE_CONTRACT_VERIFIER.to_owned(),
                port: None,
                method: None,
                path: None,
                status: None,
                body_digest: None,
                input: Some("workspace".to_owned()),
                digest: Some("sha256:tree".to_owned()),
            });
            contract.requirements.push(BoundRequirement {
                id: "saved-state".to_owned(),
                verifier: INSTANCE_SNAPSHOT_CONTRACT_VERIFIER.to_owned(),
                port: None,
                method: None,
                path: None,
                status: None,
                body_digest: None,
                input: None,
                digest: Some("sha256:snapshot".to_owned()),
            });
            let spec = AttemptSpec {
                contract: &contract,
                contract_ref: "sha256:k",
                derivation: &derivation,
                derivation_ref: "sha256:d",
                shape: crate::spec::CandidateShape::Process,
                input_refs: BTreeMap::from([("workspace".to_owned(), "sha256:tree".to_owned())]),
                instance_snapshot_ref: Some("sha256:snapshot".to_owned()),
            };
            let context = ReceiptContext {
                target: VerificationTargetKind::CliLocal,
                bundle_sha256: Some("sha256:bundle"),
                portability_profile: Some("cached"),
                run_id: Some("run-1"),
            };
            let scratch = tempfile::tempdir().unwrap();
            let records = tempfile::tempdir().unwrap();
            let outcome = run_attempt(
                &AttemptRequest {
                    request_id: "request-1",
                    attempt_id: "attempt-1",
                    label: "portable",
                    spec: &spec,
                    contract_ref: "sha256:k",
                    runtime_id: "local",
                    profile: &RuntimeProfile::default(),
                    authorization: EffectAuthorization::Unattended,
                    network: NetworkPolicy::Denied,
                    browser: None,
                    attempt_root: scratch.path(),
                    continuation: Continuation::Stop,
                    receipt: context,
                    interrupt: None,
                },
                &AnsweringRealizer::default(),
                &AttemptJournal::new(records.path()),
            );
            let receipt = outcome
                .attempt
                .receipt
                .clone()
                .expect("the attempt observed its candidate");

            let observed = verify_observed_candidate(
                &spec,
                vec![RuntimeHttpObservation::from_response(
                    "app.http", "GET", "/", 200, b"ok",
                )],
                serde_json::from_value(serde_json::json!({
                    "realization": "test",
                    "attempt_id": "attempt-1",
                    "request_id": "request-1",
                }))
                .unwrap(),
                &context,
            );
            assert_eq!(observed.receipt, receipt);
            assert_eq!(outcome.attempt.verification, Some(observed.verification));
            assert_eq!(receipt.fully_satisfied, status == 200);
        }
    }

    #[test]
    fn a_refusal_at_admission_reports_that_the_attempt_never_started() {
        let records = tempfile::tempdir().unwrap();
        let realizer = AnsweringRealizer::default();
        let journal = AttemptJournal::new(records.path());
        let outcome = served_attempt(
            200,
            // A Run started for another Derivation does not authorize this one.
            EffectAuthorization::UserInvoked {
                derivation_ref: "sha256:another",
            },
            Continuation::Stop,
            &realizer,
            &journal,
        );
        assert_eq!(outcome.attempt_record, AttemptRecordState::NotStarted);
        assert!(!outcome.execution_started());
        assert_eq!(
            outcome.attempt.failure.as_ref().unwrap().code,
            "authorization_mismatch"
        );
        assert_eq!(
            journal.records("satisfy-1").unwrap()[0].state,
            crate::journal::AttemptState::NotStarted
        );
    }

    #[test]
    fn a_verified_attempt_reports_a_finished_record() {
        let records = tempfile::tempdir().unwrap();
        let realizer = AnsweringRealizer::default();
        let journal = AttemptJournal::new(records.path());
        let outcome = served_attempt(
            200,
            EffectAuthorization::Unattended,
            Continuation::Stop,
            &realizer,
            &journal,
        );
        assert_eq!(outcome.attempt.status, AttemptStatus::Verified);
        assert_eq!(outcome.attempt_record, AttemptRecordState::Finished);
        let recorded = journal.records("satisfy-1").unwrap();
        assert_eq!(recorded[0].state, crate::journal::AttemptState::Finished);
        assert_eq!(recorded[0].outcome.as_deref(), Some("verified"));
    }

    #[test]
    fn an_attempt_that_ran_and_failed_its_contract_reports_a_finished_record() {
        let records = tempfile::tempdir().unwrap();
        let realizer = AnsweringRealizer::default();
        let journal = AttemptJournal::new(records.path());
        // K wants 404; the candidate answers 200.
        let outcome = served_attempt(
            404,
            EffectAuthorization::Unattended,
            Continuation::Stop,
            &realizer,
            &journal,
        );
        assert_eq!(outcome.attempt.status, AttemptStatus::Failed);
        assert_eq!(outcome.attempt_record, AttemptRecordState::Finished);
        assert!(realizer.stopped.load(Ordering::SeqCst));
    }

    #[test]
    fn an_attempt_whose_finish_is_not_durable_reports_started_unfinished() {
        let records = tempfile::tempdir().unwrap();
        let realizer = AnsweringRealizer::default();
        let journal = FinishFails(AttemptJournal::new(records.path()));
        let outcome = served_attempt(
            200,
            EffectAuthorization::Unattended,
            Continuation::HandOff,
            &realizer,
            &journal,
        );
        // It ran and satisfied K here, and that is still not a known result.
        assert_eq!(
            outcome.attempt_record,
            AttemptRecordState::StartedUnfinished
        );
        assert!(outcome.execution_started());
        assert_eq!(outcome.attempt.status, AttemptStatus::Failed);
        assert!(outcome.verified.is_none());
        assert!(outcome.live.is_none(), "nothing is handed off");
        assert!(realizer.stopped.load(Ordering::SeqCst));
        // The durable record agrees: begun, never finished.
        let recorded = journal.0.records("satisfy-1").unwrap();
        assert_eq!(recorded[0].state, crate::journal::AttemptState::Started);
        // And the next attempt of the request is held by it.
        let next = served_attempt(
            200,
            EffectAuthorization::Unattended,
            Continuation::Stop,
            &realizer,
            &AttemptJournal::new(records.path()),
        );
        assert_eq!(
            next.attempt.failure.as_ref().unwrap().code,
            "attempt_already_started"
        );
        assert_eq!(next.attempt_record, AttemptRecordState::StartedUnfinished);
    }

    #[test]
    fn a_hand_off_never_stops_and_is_not_a_cleanup_failure() {
        let mut attempt = verified_attempt();
        let (kept, live) = after_verification(
            &mut attempt,
            None,
            "the running candidate",
            Continuation::HandOff,
            |_| panic!("a handed-off candidate is not stopped here"),
        );
        assert!(kept);
        assert_eq!(live, Some("the running candidate"));
        assert_eq!(attempt.outcomes.cleanup.state, OutcomeState::NotAttempted);
        assert_eq!(
            attempt.outcomes.cleanup.reason.as_deref(),
            Some("handed_off")
        );
        assert_eq!(attempt.status, AttemptStatus::Verified);
    }
    #[test]
    fn old_start_precedes_new_admission_refusal() {
        let records = tempfile::tempdir().unwrap();
        let journal = FinishFails(AttemptJournal::new(records.path()));
        let first = served_attempt(
            200,
            EffectAuthorization::Unattended,
            Continuation::Stop,
            &AnsweringRealizer::default(),
            &journal,
        );
        assert_eq!(first.attempt_record, AttemptRecordState::StartedUnfinished);
        let repeated = served_attempt(
            200,
            EffectAuthorization::UserInvoked {
                derivation_ref: "sha256:wrong",
            },
            Continuation::Stop,
            &AnsweringRealizer::default(),
            &journal,
        );
        assert_eq!(
            repeated.attempt_record,
            AttemptRecordState::StartedUnfinished
        );
        assert_eq!(
            repeated.attempt.failure.unwrap().code,
            "attempt_already_started"
        );
    }
}
