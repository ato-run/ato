//! Runtime Network Phase 1 — what a Runtime establishes about a ticket by
//! itself, before anything of the candidate runs.
//!
//! The ticket's route and archive are the authority; the request's metadata
//! (effects, requirements) is only a scheduling hint. Every case here uses a
//! static route, which needs no sandbox, so it runs on any host.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ato_formation::browser::{
    BROWSER_VERIFIER_PROTOCOL, BrowserContractV0, BrowserEvent, BrowserEvidence, BrowserTarget,
    BrowserVerdict, BrowserVerificationReceipt, BrowserVerificationResult, CriterionResult,
    EVENT_NAVIGATION, EVIDENCE_BROWSER_SNAPSHOT, JudgeDecision, PRIMARY_CRITERION_ID,
    VerifierContainment, VerifierIdentity, effective_contract_ref,
};
use ato_formation_worker::journal::AttemptRecordState;
use ato_formation_worker::runtime_network::{
    AttemptResultReport, AttemptTicket, NATIVE_ENVIRONMENT, RuntimeConstraintWire, SatisfyBudget,
    SatisfyPolicy, SatisfyRequest, ServeConfig, Settlement, Submission, UnknownAttempt,
    execute_ticket, new_search_id, prepare_submission,
};
use base64::Engine as _;

const STATIC_ROUTE: &str = r#"
schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[derive.step]]
id = "site"
use = "ato.browser@1"
op = "serve"
source = "workspace"
entry = "index.html"
spa_fallback = false

[[port]]
id = "app.http"
use = "ato.http@1"
from = "site"

[[contract.require]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
method = "GET"
path = "/"

[contract.require.expect]
status = 200
"#;

fn site(extra_toml: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("index.html"), "<!doctype html><h1>hi</h1>").expect("html");
    std::fs::write(
        dir.path().join("capsule.toml"),
        format!("{STATIC_ROUTE}\n{extra_toml}"),
    )
    .expect("toml");
    dir
}

fn submit(dir: &Path, scratch: &Path) -> Submission {
    prepare_submission(
        dir,
        &[],
        None,
        &scratch.join("submit"),
        RuntimeConstraintWire::Any,
        SatisfyPolicy {
            network: "denied".to_owned(),
            allow_managed: false,
        },
        SatisfyBudget {
            max_attempts: 4,
            mode: "first_pass".to_owned(),
        },
        "search_test",
    )
    .expect("the requester plans the route")
}

/// The ticket a coordinator would issue for the submission's first route.
fn ticket(submission: &Submission) -> (AttemptTicket, Vec<u8>) {
    let request = &submission.request;
    let route = &request.authorized_derivations[0];
    let archive = base64::engine::general_purpose::STANDARD
        .decode(&request.source.archive_base64)
        .expect("archive");
    (
        AttemptTicket {
            attempt_id: "att_test".to_owned(),
            fence: 1,
            satisfy_id: "sat_test".to_owned(),
            runtime_id: "rt_test".to_owned(),
            environment_id: NATIVE_ENVIRONMENT.to_owned(),
            contract_ref: request.contract_ref.clone(),
            base_contract_ref: request.base_contract_ref.clone(),
            derivation_ref: route.derivation_ref.clone(),
            capsule_toml: route.capsule_toml.clone(),
            browser_contract: None,
            archive_digest: request.source.archive_digest.clone(),
            bindings: BTreeMap::new(),
            network: "denied".to_owned(),
        },
        archive,
    )
}

fn serve_config(scratch: &Path) -> ServeConfig {
    ServeConfig {
        api: "http://127.0.0.1:9".to_owned(),
        token: "unused".to_owned(),
        work_root: scratch.join("work"),
        out_dir: scratch.join("out"),
        shim: PathBuf::from(env!("CARGO_BIN_EXE_ato-formation-worker")),
        browser_verifier: None,
        poll: Duration::from_secs(1),
        max_tickets: Some(1),
    }
}

fn run(ticket: &AttemptTicket, archive: Vec<u8>, scratch: &Path) -> AttemptResultReport {
    execute_ticket(&serve_config(scratch), ticket, archive)
}

fn failure_code(report: &AttemptResultReport) -> &str {
    report
        .failure
        .as_ref()
        .map(|f| f.code.as_str())
        .unwrap_or("")
}

#[test]
fn a_disposable_route_runs_and_is_attested() {
    let dir = site("");
    let scratch = tempfile::tempdir().expect("scratch");
    let (ticket, archive) = ticket(&submit(dir.path(), scratch.path()));
    let report = run(&ticket, archive, scratch.path());
    assert_eq!(report.outcome, "pass", "{report:?}");
    let attested = &report.attestation;
    assert!(attested.execution_started);
    // Its start and its finish are both durable.
    assert_eq!(attested.attempt_record, AttemptRecordState::Finished);
    assert_eq!(attested.environment_id, NATIVE_ENVIRONMENT);
    assert_eq!(attested.effects.as_deref(), Some("pure"));
    assert_eq!(
        attested.derivation_ref.as_deref(),
        Some(ticket.derivation_ref.as_str())
    );
    assert_eq!(
        attested.contract_ref.as_deref(),
        Some(ticket.contract_ref.as_str())
    );
}

#[test]
fn a_non_repeatable_route_is_refused_whatever_the_request_claimed() {
    let dir = site("[effects]\ndefault = \"non-repeatable\"\n");
    let scratch = tempfile::tempdir().expect("scratch");
    let mut submission = submit(dir.path(), scratch.path());
    // A requester that lies: the ticket is built from the same route bytes,
    // so the lie cannot reach the Runtime's decision.
    submission.request.authorized_derivations[0].effects = "pure".to_owned();
    let (ticket, archive) = ticket(&submission);
    let report = run(&ticket, archive, scratch.path());
    assert_eq!(report.outcome, "inconclusive");
    assert_eq!(failure_code(&report), "effect_policy");
    assert_eq!(
        report.attestation.effects.as_deref(),
        Some("non-repeatable")
    );
    assert!(!report.attestation.execution_started);
    assert_eq!(
        report.attestation.attempt_record,
        AttemptRecordState::NotStarted
    );
    assert!(report.verifier_receipts.is_empty());
}

#[test]
fn a_route_for_another_platform_is_refused_before_it_runs() {
    let other = if std::env::consts::OS == "windows" {
        "[[platform]]\nos = \"linux\"\narch = \"aarch64\"\n"
    } else {
        "[[platform]]\nos = \"windows\"\narch = \"x86_64\"\n"
    };
    let dir = site(other);
    let scratch = tempfile::tempdir().expect("scratch");
    let mut submission = submit(dir.path(), scratch.path());
    // Metadata that forgot the platform requirement changes nothing.
    submission.request.authorized_derivations[0]
        .requirements
        .retain(|requirement| requirement.fact != "platform");
    let (ticket, archive) = ticket(&submission);
    let report = run(&ticket, archive, scratch.path());
    assert_eq!(report.outcome, "inconclusive");
    assert_eq!(failure_code(&report), "platform_unsupported");
    assert!(!report.attestation.execution_started);
    assert!(
        report
            .attestation
            .requirements
            .iter()
            .any(|requirement| requirement.fact == "platform"),
        "the Runtime attests the platform requirement it computed: {:?}",
        report.attestation.requirements
    );
}

#[test]
fn a_ticket_for_another_environment_is_refused() {
    let dir = site("");
    let scratch = tempfile::tempdir().expect("scratch");
    let (mut ticket, archive) = ticket(&submit(dir.path(), scratch.path()));
    ticket.environment_id = "container".to_owned();
    let report = run(&ticket, archive, scratch.path());
    assert_eq!(report.outcome, "inconclusive");
    assert_eq!(failure_code(&report), "environment_mismatch");
    assert!(!report.attestation.execution_started);
    assert_eq!(
        report.attestation.attempt_record,
        AttemptRecordState::NotStarted
    );
    assert_eq!(report.attestation.environment_id, NATIVE_ENVIRONMENT);
}

#[test]
fn a_ticket_whose_refs_the_route_does_not_reach_is_refused_before_it_runs() {
    let dir = site("");
    let scratch = tempfile::tempdir().expect("scratch");
    let (mut ticket, archive) = ticket(&submit(dir.path(), scratch.path()));
    ticket.derivation_ref = format!("sha256:{}", "0".repeat(64));
    let report = run(&ticket, archive, scratch.path());
    assert_eq!(report.outcome, "inconclusive");
    assert_eq!(failure_code(&report), "ticket_mismatch");
    assert!(!report.attestation.execution_started);
}

#[test]
fn a_redelivered_ticket_is_never_executed_twice() {
    let dir = site("");
    let scratch = tempfile::tempdir().expect("scratch");
    let (ticket, archive) = ticket(&submit(dir.path(), scratch.path()));
    let first = run(&ticket, archive.clone(), scratch.path());
    assert_eq!(first.outcome, "pass", "{:?}", first.failure);

    // The same attempt, delivered again (a lost result, a retried claim):
    // it started once, so it is reported as started and not run again.
    let again = run(&ticket, archive, scratch.path());
    assert_eq!(failure_code(&again), "attempt_already_started");
    assert!(again.attestation.execution_started);
    // Its record says the first delivery finished.
    assert_eq!(
        again.attestation.attempt_record,
        AttemptRecordState::Finished
    );
    assert_eq!(again.outcome, "inconclusive");
    assert!(again.materialization_ref.is_none());
}

#[test]
fn an_unfinished_attempt_holds_every_later_attempt_of_its_request() {
    use ato_formation_worker::journal::{AttemptJournal, StartIdentity};

    let dir = site("");
    let scratch = tempfile::tempdir().expect("scratch");
    let (ticket, archive) = ticket(&submit(dir.path(), scratch.path()));
    // An earlier attempt of the same request started and its process died
    // before recording how it ended.
    let journal = AttemptJournal::new(scratch.path().join("out").join("attempt-records"));
    let unfinished = journal
        .begin(
            &ticket.satisfy_id,
            "att_earlier",
            StartIdentity {
                contract_ref: ticket.contract_ref.clone(),
                derivation_ref: ticket.derivation_ref.clone(),
                runtime_id: ticket.runtime_id.clone(),
                effects: "pure".to_owned(),
                network: "denied".to_owned(),
                authorization: "unattended".to_owned(),
            },
        )
        .expect("begun");
    drop(unfinished);

    let report = run(&ticket, archive, scratch.path());
    assert_eq!(failure_code(&report), "request_effect_unknown");
    assert!(
        report.attestation.execution_started,
        "compatibility bit is conservative for a search barrier"
    );
    assert_eq!(
        report.attestation.attempt_record,
        AttemptRecordState::BlockedByUnknown {
            attempt_id: "att_earlier".into()
        }
    );
    assert!(report.materialization_ref.is_none());
}

#[test]
fn a_passing_attempt_carries_a_receipt_bound_to_its_attempt() {
    let dir = site("");
    let scratch = tempfile::tempdir().expect("scratch");
    let (ticket, archive) = ticket(&submit(dir.path(), scratch.path()));
    let report = run(&ticket, archive, scratch.path());
    assert_eq!(report.outcome, "pass", "{:?}", report.failure);
    assert!(report.attestation.execution_started);
    let receipt = report
        .verifier_receipts
        .iter()
        .find(|receipt| receipt["kind"] == "contract_verification")
        .expect("a contract verification receipt")["receipt"]
        .clone();
    assert_eq!(receipt["fully_satisfied"], true);
    assert_eq!(receipt["derivation_ref"], ticket.derivation_ref.as_str());
    assert_eq!(receipt["execution"]["attempt_id"], "att_test");
    assert_eq!(receipt["execution"]["request_id"], "sat_test");
    assert_eq!(receipt["target"]["kind"], "formation-runtime");
    // No transport existed; none is invented.
    assert!(receipt.get("bundle_sha256").is_none());
    // Observed over HTTP, not decided from the artifact's file list.
    assert_eq!(receipt["observations"][0]["evidence"]["status"], 200);
}

// ── the requester accepts a route only with evidence for the whole K ──────

/// The status a coordinator returns once a route passed, built from the
/// Runtime's own report: the shape of `satisfyView` / `routeView`.
fn settled(
    ticket: &AttemptTicket,
    effective_contract_ref: &str,
    receipts: Vec<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "satisfy_id": ticket.satisfy_id,
        "status": "satisfied",
        "attempts": [{
            "attempt_id": ticket.attempt_id,
            "runtime_id": ticket.runtime_id,
            "environment_id": ticket.environment_id,
            "derivation_ref": ticket.derivation_ref,
            "status": "pass",
        }],
        "verified_routes": [{
            "attempt_id": ticket.attempt_id,
            "effective_contract_ref": effective_contract_ref,
            "derivation_ref": ticket.derivation_ref,
            "runtime_id": ticket.runtime_id,
            "environment_id": ticket.environment_id,
            "verifier_receipts": receipts,
        }],
    })
}

fn base_receipt(report: &AttemptResultReport) -> serde_json::Value {
    report
        .verifier_receipts
        .iter()
        .find(|receipt| receipt["kind"] == "contract_verification")
        .expect("a contract verification receipt")
        .clone()
}

fn accepts(
    submission: &Submission,
    satisfy_id: &str,
    status: &serde_json::Value,
) -> Result<(), String> {
    let (accepted, refused) = ato_formation_worker::runtime_network::accept_verified_routes(
        submission, satisfy_id, status,
    );
    match (accepted.len(), refused.first()) {
        (1, None) => Ok(()),
        (0, Some(reason)) => Err(reason.clone()),
        other => panic!("unexpected split {other:?}"),
    }
}

/// A browser receipt as a Runtime writes it for `attempt_id`.
fn browser_receipt(
    contract: &BrowserContractV0,
    verdict: BrowserVerdict,
    attempt_id: &str,
) -> serde_json::Value {
    let choice = match verdict {
        BrowserVerdict::Pass => Some("complete"),
        BrowserVerdict::Fail => Some("incomplete"),
        BrowserVerdict::Inconclusive => None,
    };
    let result = BrowserVerificationResult {
        protocol: BROWSER_VERIFIER_PROTOCOL.to_owned(),
        verdict,
        criteria: vec![CriterionResult {
            id: PRIMARY_CRITERION_ID.to_owned(),
            verdict,
            decision: choice.map(|choice| JudgeDecision {
                choice: choice.to_owned(),
                confidence: 0.9,
                probabilities: [(choice.to_owned(), 0.95)].into(),
                model: "jev-1.13.0".to_owned(),
            }),
            rounds: 1,
            evidence_refs: vec!["e1".to_owned()],
            reason: None,
        }],
        evidence: vec![BrowserEvidence {
            id: "e1".to_owned(),
            kind: EVIDENCE_BROWSER_SNAPSHOT.to_owned(),
            sequence: 1,
            url: Some("http://127.0.0.1:41234/".to_owned()),
            title: Some("hi".to_owned()),
            facts: vec!["the page says hi".to_owned()],
            text_excerpt: None,
        }],
        observed_events: vec![BrowserEvent {
            sequence: 1,
            kind: EVENT_NAVIGATION.to_owned(),
            url: Some("http://127.0.0.1:41234/".to_owned()),
        }],
        action_trace: Vec::new(),
        verifier: VerifierIdentity {
            verifier: "formation-browser-verifier/0".to_owned(),
            stagehand_version: None,
            browser_version: None,
            agent_model: None,
            judge_model: Some("jev-1.13.0".to_owned()),
        },
        reason: None,
    };
    let mut receipt = BrowserVerificationReceipt::new(
        contract,
        BrowserTarget {
            runtime_id: "rt_test".to_owned(),
            endpoint: "http://127.0.0.1:41234/".to_owned(),
            attempt_id: Some(attempt_id.to_owned()),
        },
        result,
    )
    .expect("a valid result");
    receipt.containment = Some(VerifierContainment {
        containment: "bwrap".to_owned(),
        filesystem: "allowlisted".to_owned(),
        network: "loopback candidate only".to_owned(),
        browser_process: "own pid namespace".to_owned(),
        secrets: "fd".to_owned(),
    });
    serde_json::json!({ "kind": "browser_contract", "receipt": receipt })
}

/// One real pass of the base route, then the request as it looks with and
/// without a browser Contract on top.
fn passed_base() -> (
    tempfile::TempDir,
    Submission,
    AttemptTicket,
    AttemptResultReport,
) {
    let dir = site("");
    let scratch = tempfile::tempdir().expect("scratch");
    let submission = submit(dir.path(), scratch.path());
    let (ticket, archive) = ticket(&submission);
    let report = run(&ticket, archive, scratch.path());
    assert_eq!(report.outcome, "pass", "{:?}", report.failure);
    drop(dir);
    (scratch, submission, ticket, report)
}

fn with_browser(mut submission: Submission, contract: &BrowserContractV0) -> Submission {
    submission.request.contract_ref =
        effective_contract_ref(&submission.request.base_contract_ref, Some(contract));
    submission.request.browser_contract = Some(contract.clone());
    submission
}

#[test]
fn a_base_route_is_accepted_only_with_a_satisfied_receipt_for_its_exact_attempt() {
    let (_scratch, submission, ticket, report) = passed_base();
    let effective = submission.request.contract_ref.clone();
    let status = settled(&ticket, &effective, vec![base_receipt(&report)]);
    assert_eq!(accepts(&submission, &ticket.satisfy_id, &status), Ok(()));

    // Another request's receipt, replayed under this one.
    let replayed = accepts(&submission, "sat_other", &status).unwrap_err();
    assert!(replayed.contains("receipt_request_mismatch"), "{replayed}");

    // A: a pass label over a well-formed FAILED receipt.
    let mut failing = base_receipt(&report);
    let observation = &mut failing["receipt"]["observations"][0];
    observation["outcome"] = "failed".into();
    observation["failure"] = "http_status_mismatch: GET / returned 500".into();
    observation["evidence"]["status"] = 500.into();
    failing["receipt"]["fully_satisfied"] = false.into();
    let refused = accepts(
        &submission,
        &ticket.satisfy_id,
        &settled(&ticket, &effective, vec![failing]),
    )
    .unwrap_err();
    assert!(
        refused.contains("receipt_contract_not_satisfied"),
        "{refused}"
    );

    // A verdict whose evidence was edited in transit.
    let mut forged = base_receipt(&report);
    forged["receipt"]["observations"][0]["evidence"]["status"] = 500.into();
    let refused = accepts(
        &submission,
        &ticket.satisfy_id,
        &settled(&ticket, &effective, vec![forged]),
    )
    .unwrap_err();
    assert!(
        refused.contains("receipt_evidence_inconsistent"),
        "{refused}"
    );

    // F: the route claims a different effective K.
    let other = settled(&ticket, "sha256:other", vec![base_receipt(&report)]);
    let refused = accepts(&submission, &ticket.satisfy_id, &other).unwrap_err();
    assert!(refused.contains("route_contract_mismatch"), "{refused}");

    // A route whose named attempt did not pass.
    let mut orphan = status.clone();
    orphan["attempts"][0]["status"] = "fail".into();
    let refused = accepts(&submission, &ticket.satisfy_id, &orphan).unwrap_err();
    assert!(refused.contains("route_attempt_not_passed"), "{refused}");
}

#[test]
fn an_attempt_is_matched_by_its_environment_not_only_its_runtime() {
    let (_scratch, submission, ticket, report) = passed_base();
    let effective = submission.request.contract_ref.clone();
    // G: the same Runtime also passed the same D in another environment.
    let other_env = serde_json::json!({
        "attempt_id": "att_other_env",
        "runtime_id": ticket.runtime_id,
        "environment_id": "container",
        "derivation_ref": ticket.derivation_ref,
        "status": "pass",
    });

    // A view without attempt ids: the native route resolves to the native
    // attempt, whose receipt it carries.
    let mut status = settled(&ticket, &effective, vec![base_receipt(&report)]);
    status["attempts"]
        .as_array_mut()
        .unwrap()
        .insert(0, other_env.clone());
    status["verified_routes"][0]
        .as_object_mut()
        .unwrap()
        .remove("attempt_id");
    assert_eq!(accepts(&submission, &ticket.satisfy_id, &status), Ok(()));

    // Named attempt in another environment: refused, never re-matched.
    status["verified_routes"][0]["attempt_id"] = "att_other_env".into();
    let refused = accepts(&submission, &ticket.satisfy_id, &status).unwrap_err();
    assert!(refused.contains("route_attempt_mismatch"), "{refused}");

    // The receipt of the other environment's attempt, under this route.
    let mut status = settled(&ticket, &effective, vec![base_receipt(&report)]);
    status["attempts"].as_array_mut().unwrap().push(other_env);
    status["verified_routes"][0]["environment_id"] = "container".into();
    status["verified_routes"][0]
        .as_object_mut()
        .unwrap()
        .remove("attempt_id");
    let refused = accepts(&submission, &ticket.satisfy_id, &status).unwrap_err();
    assert!(refused.contains("receipt_attempt_mismatch"), "{refused}");
}

#[test]
fn a_browser_contract_route_needs_a_passing_browser_receipt_from_the_same_attempt() {
    let (_scratch, submission, ticket, report) = passed_base();
    let contract = BrowserContractV0::from_prompt("The page greets the visitor.").unwrap();
    let submission = with_browser(submission, &contract);
    let effective = submission.request.contract_ref.clone();
    assert_ne!(effective, submission.request.base_contract_ref);
    let with = |browser: Option<serde_json::Value>| {
        let mut receipts = vec![base_receipt(&report)];
        receipts.extend(browser);
        settled(&ticket, &effective, receipts)
    };

    // H: base PASS, browser PASS, exact route and attempt.
    let passing = browser_receipt(&contract, BrowserVerdict::Pass, &ticket.attempt_id);
    assert_eq!(
        accepts(
            &submission,
            &ticket.satisfy_id,
            &with(Some(passing.clone()))
        ),
        Ok(())
    );

    // B: base PASS and no browser receipt.
    let refused = accepts(&submission, &ticket.satisfy_id, &with(None)).unwrap_err();
    assert!(refused.contains("browser_receipt_missing"), "{refused}");

    // C / D: a browser verdict that is not a pass.
    for verdict in [BrowserVerdict::Fail, BrowserVerdict::Inconclusive] {
        let receipt = browser_receipt(&contract, verdict, &ticket.attempt_id);
        let refused = accepts(&submission, &ticket.satisfy_id, &with(Some(receipt))).unwrap_err();
        assert!(refused.contains("not pass"), "{verdict:?}: {refused}");
    }

    // E: a PASS from another attempt.
    let elsewhere = browser_receipt(&contract, BrowserVerdict::Pass, "att_other");
    let refused = accepts(&submission, &ticket.satisfy_id, &with(Some(elsewhere))).unwrap_err();
    assert!(refused.contains("browser_receipt_rejected"), "{refused}");

    // A PASS label over criteria that do not add up to one.
    let mut relabelled =
        browser_receipt(&contract, BrowserVerdict::Inconclusive, &ticket.attempt_id);
    relabelled["receipt"]["overall"] = "pass".into();
    let refused = accepts(&submission, &ticket.satisfy_id, &with(Some(relabelled))).unwrap_err();
    assert!(refused.contains("browser_receipt_rejected"), "{refused}");

    // A PASS for a different browser Contract.
    let other = BrowserContractV0::from_prompt("Something else entirely.").unwrap();
    let foreign = browser_receipt(&other, BrowserVerdict::Pass, &ticket.attempt_id);
    let refused = accepts(&submission, &ticket.satisfy_id, &with(Some(foreign))).unwrap_err();
    assert!(refused.contains("another browser Contract"), "{refused}");

    // An uncontained verifier's PASS.
    let mut uncontained = passing;
    uncontained["receipt"]["containment"]["containment"] = "none".into();
    let refused = accepts(&submission, &ticket.satisfy_id, &with(Some(uncontained))).unwrap_err();
    assert!(refused.contains("contained"), "{refused}");

    // The base-only effective ref does not stand in for base + browser.
    let base_only = settled(
        &ticket,
        &submission.request.base_contract_ref,
        vec![base_receipt(&report)],
    );
    let refused = accepts(&submission, &ticket.satisfy_id, &base_only).unwrap_err();
    assert!(refused.contains("route_contract_mismatch"), "{refused}");
}

// ── the wire both sides read (ato-api keeps the same files) ───────────────

const FIXTURES: [(&str, &str); 7] = [
    (
        "satisfy-request",
        include_str!("fixtures/runtime-network-v0/satisfy-request.json"),
    ),
    (
        "attempt-result-not-started",
        include_str!("fixtures/runtime-network-v0/attempt-result-not-started.json"),
    ),
    (
        "attempt-result-finished",
        include_str!("fixtures/runtime-network-v0/attempt-result-finished.json"),
    ),
    (
        "attempt-result-started-unfinished",
        include_str!("fixtures/runtime-network-v0/attempt-result-started-unfinished.json"),
    ),
    (
        "attempt-result-history-unavailable",
        include_str!("fixtures/runtime-network-v0/attempt-result-history-unavailable.json"),
    ),
    (
        "attempt-result-blocked-by-unknown",
        include_str!("fixtures/runtime-network-v0/attempt-result-blocked-by-unknown.json"),
    ),
    (
        "unknown-resolution",
        include_str!("fixtures/runtime-network-v0/unknown-resolution.json"),
    ),
];

/// The fixtures are byte-identical to `ato-api`'s
/// `src/tests/fixtures/runtime-network-v0/`: both repos check the same
/// recorded digests, so a change on one side fails until the other follows.
#[test]
fn the_wire_fixtures_are_the_recorded_bytes() {
    use sha2::{Digest as _, Sha256};
    let recorded: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/runtime-network-v0/expected-digests.json"
    ))
    .expect("recorded digests");
    for (name, text) in FIXTURES {
        assert_eq!(
            recorded[name].as_str(),
            Some(format!("sha256:{:x}", Sha256::digest(text.as_bytes())).as_str()),
            "{name} is not the recorded fixture"
        );
    }
}

#[test]
fn the_wire_fixtures_round_trip_through_the_runtime_types() {
    for (name, text) in FIXTURES {
        let value: serde_json::Value = serde_json::from_str(text).expect(name);
        let reserialized = if name == "satisfy-request" {
            let request: SatisfyRequest = serde_json::from_value(value.clone()).expect(name);
            assert!(request.search_id.starts_with("search_"));
            serde_json::to_value(&request)
        } else if name == "unknown-resolution" {
            let resolution: ato_formation_worker::runtime_network::UnknownResolution =
                serde_json::from_value(value.clone()).expect(name);
            serde_json::to_value(resolution)
        } else {
            let report: AttemptResultReport = serde_json::from_value(value.clone()).expect(name);
            let expected = match name {
                "attempt-result-not-started" => AttemptRecordState::NotStarted,
                "attempt-result-finished" => AttemptRecordState::Finished,
                "attempt-result-history-unavailable" => AttemptRecordState::HistoryUnavailable,
                "attempt-result-blocked-by-unknown" => AttemptRecordState::BlockedByUnknown {
                    attempt_id: "att_historical".into(),
                },
                _ => AttemptRecordState::StartedUnfinished,
            };
            assert_eq!(report.attestation.attempt_record, expected, "{name}");
            serde_json::to_value(&report)
        }
        .expect(name);
        assert_eq!(reserialized, value, "{name} does not round-trip");
    }
    // The record state is a closed set of names, spelled as the fixtures do.
    for (state, name) in [
        (AttemptRecordState::NotStarted, "not_started"),
        (AttemptRecordState::Finished, "finished"),
        (AttemptRecordState::StartedUnfinished, "started_unfinished"),
    ] {
        assert_eq!(serde_json::to_value(state).unwrap(), name);
    }
    // A report without it is not a report this Runtime reads or writes.
    let mut missing: serde_json::Value = serde_json::from_str(FIXTURES[2].1).unwrap();
    missing["attestation"]
        .as_object_mut()
        .unwrap()
        .remove("attempt_record");
    assert!(serde_json::from_value::<AttemptResultReport>(missing).is_err());
}

#[test]
fn a_search_id_is_opaque_and_new_each_time() {
    let one = new_search_id([0x0f; 16]);
    assert_eq!(one, format!("search_{}", "0f".repeat(16)));
    assert_ne!(new_search_id([1; 16]), new_search_id([2; 16]));
    let dir = site("");
    let scratch = tempfile::tempdir().expect("scratch");
    assert_eq!(
        submit(dir.path(), scratch.path()).request.search_id,
        "search_test"
    );
}

#[test]
fn an_unknown_request_is_its_own_terminal_reason_never_a_failure() {
    let status = serde_json::json!({
        "satisfy_id": "sat_1",
        "search_id": "search_1",
        "status": "unknown",
        "unknown_attempts": [
            {
                "attempt_id": "att_resolved", "runtime_id": "rnr_a",
                "unknown_reason": "result_not_received",
                "resolution": { "kind": "no_effect_confirmed" }
            },
            {
                "attempt_id": "att_open", "runtime_id": "rnr_b",
                "unknown_reason": "attempt_record_unfinished",
                "resolution": null
            }
        ]
    });
    let settlement = Settlement::of(&status).expect("a known status");
    assert_eq!(
        settlement,
        Settlement::EffectUnknown(vec![UnknownAttempt {
            attempt_id: "att_open".to_owned(),
            runtime_id: "rnr_b".to_owned(),
            reason: "attempt_record_unfinished".to_owned(),
        }])
    );
    assert_eq!(settlement.terminal_reason(), Some("effect_unknown"));
    let shown = settlement.to_string();
    assert!(shown.starts_with("effect_unknown: attempt att_open on Runtime rnr_b"));
    assert!(!shown.contains("unsatisfied") && !shown.contains("inconclusive"));

    for (status, reason) in [
        ("unsatisfied", Some("unsatisfied")),
        ("exhausted", Some("budget_exhausted")),
        ("stopped", Some("search_stopped")),
        ("satisfied", None),
        ("running", None),
    ] {
        let settlement = Settlement::of(&serde_json::json!({ "status": status })).unwrap();
        assert_eq!(settlement.terminal_reason(), reason, "{status}");
    }
    // A status this requester does not know is not read as one it does.
    assert!(Settlement::of(&serde_json::json!({ "status": "paused" })).is_err());
}

fn leave_started(ticket: &AttemptTicket, scratch: &Path) {
    use ato_formation_worker::journal::{AttemptJournal, StartIdentity};
    let journal = AttemptJournal::new(scratch.join("out/attempt-records"));
    drop(
        journal
            .begin(
                &ticket.satisfy_id,
                &ticket.attempt_id,
                StartIdentity {
                    contract_ref: ticket.contract_ref.clone(),
                    derivation_ref: ticket.derivation_ref.clone(),
                    runtime_id: ticket.runtime_id.clone(),
                    effects: "pure".into(),
                    network: "denied".into(),
                    authorization: "unattended".into(),
                },
            )
            .unwrap(),
    );
}

#[test]
fn historical_start_precedes_source_planning_and_environment_refusals() {
    use ato_formation_worker::runtime_network::execute_ticket_with_source;
    let dir = site("");
    let scratch = tempfile::tempdir().unwrap();
    let (mut ticket, archive) = ticket(&submit(dir.path(), scratch.path()));
    leave_started(&ticket, scratch.path());
    ticket.environment_id = "capability-removed".into();
    ticket.capsule_toml = "not valid TOML".into();
    let report = execute_ticket_with_source(&serve_config(scratch.path()), &ticket, || {
        anyhow::bail!("source fetch failed")
    });
    assert_eq!(
        report.attestation.attempt_record,
        AttemptRecordState::StartedUnfinished
    );
    let report = run(&ticket, archive, scratch.path());
    assert_eq!(
        report.attestation.attempt_record,
        AttemptRecordState::StartedUnfinished
    );
    assert_eq!(failure_code(&report), "attempt_already_started");
}

#[test]
fn unreadable_history_is_unknown_even_when_source_would_fail() {
    use ato_formation_worker::runtime_network::execute_ticket_with_source;
    let dir = site("");
    let scratch = tempfile::tempdir().unwrap();
    let (ticket, _) = ticket(&submit(dir.path(), scratch.path()));
    leave_started(&ticket, scratch.path());
    let root = scratch.path().join("out/attempt-records");
    let request_dir = std::fs::read_dir(root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(request_dir.join("corrupt.json"), "{").unwrap();
    let report = execute_ticket_with_source(&serve_config(scratch.path()), &ticket, || {
        anyhow::bail!("offline")
    });
    assert_eq!(
        report.attestation.attempt_record,
        AttemptRecordState::HistoryUnavailable
    );
    assert_eq!(failure_code(&report), "attempt_history_unavailable");
}

#[test]
fn a_finished_delivery_is_not_downgraded_by_a_new_source_error() {
    use ato_formation_worker::runtime_network::execute_ticket_with_source;
    let dir = site("");
    let scratch = tempfile::tempdir().unwrap();
    let (ticket, archive) = ticket(&submit(dir.path(), scratch.path()));
    assert_eq!(run(&ticket, archive, scratch.path()).outcome, "pass");
    let report = execute_ticket_with_source(&serve_config(scratch.path()), &ticket, || {
        anyhow::bail!("offline")
    });
    assert_eq!(
        report.attestation.attempt_record,
        AttemptRecordState::Finished
    );
}

// Test-binary-only fault injection: the real worker entry owns its reservation,
// fetched the bytes, and is paused before planning/execution. No production flag.
#[test]
fn paused_worker_child() {
    let Some(root) = std::env::var_os("ATO_3B_TEST_PAUSED_CHILD") else {
        return;
    };
    let root = PathBuf::from(root);
    let ticket: AttemptTicket =
        serde_json::from_slice(&std::fs::read(root.join("ticket.json")).unwrap()).unwrap();
    let archive = std::fs::read(root.join("archive.tar")).unwrap();
    ato_formation_worker::runtime_network::execute_ticket_with_source(
        &serve_config(&root),
        &ticket,
        || {
            std::fs::write(root.join("source-fetched"), archive.len().to_string()).unwrap();
            loop {
                std::thread::sleep(Duration::from_millis(20));
            }
        },
    );
}

#[test]
fn terminating_a_worker_after_source_fetch_fences_its_old_attempt_forever() {
    let dir = site("");
    let scratch = tempfile::tempdir().unwrap();
    let (ticket, archive) = ticket(&submit(dir.path(), scratch.path()));
    std::fs::write(
        scratch.path().join("ticket.json"),
        serde_json::to_vec(&ticket).unwrap(),
    )
    .unwrap();
    std::fs::write(scratch.path().join("archive.tar"), &archive).unwrap();
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Child(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "paused_worker_child", "--nocapture"])
            .env("ATO_3B_TEST_PAUSED_CHILD", scratch.path())
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !scratch.path().join("source-fetched").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "worker never reached pause"
        );
        assert!(child.0.try_wait().unwrap().is_none(), "worker exited early");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !scratch.path().join("work").exists(),
        "execution has not started"
    );
    child.0.kill().unwrap();
    child.0.wait().unwrap(); // physical termination, not timeout or a fence number
    let report = run(&ticket, archive, scratch.path());
    assert_eq!(
        report.attestation.attempt_record,
        AttemptRecordState::NotStarted
    );
    assert_eq!(failure_code(&report), "attempt_already_refused");
    assert!(
        !scratch.path().join("work").exists(),
        "old id was not re-executed"
    );
}

#[test]
fn concurrent_deliveries_cannot_start_after_a_not_started_refusal() {
    use ato_formation_worker::runtime_network::execute_ticket_with_source;
    let dir = site("");
    let scratch = tempfile::tempdir().unwrap();
    let (ticket, _) = ticket(&submit(dir.path(), scratch.path()));
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let config = serve_config(scratch.path());
    let first_ticket = ticket.clone();
    let first = std::thread::spawn(move || {
        execute_ticket_with_source(&config, &first_ticket, || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            anyhow::bail!("source unavailable")
        })
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let config = serve_config(scratch.path());
    let second = std::thread::spawn(move || {
        execute_ticket_with_source(&config, &ticket, || {
            panic!("a concurrent delivery must not refetch or execute after the refusal")
        })
    });
    release_tx.send(()).unwrap();
    let first = first.join().unwrap();
    let second = second.join().unwrap();
    assert_eq!(
        first.attestation.attempt_record,
        AttemptRecordState::NotStarted
    );
    assert_eq!(failure_code(&first), "source_unavailable");
    assert_eq!(
        second.attestation.attempt_record,
        AttemptRecordState::NotStarted
    );
    assert_eq!(failure_code(&second), "attempt_already_refused");
}
