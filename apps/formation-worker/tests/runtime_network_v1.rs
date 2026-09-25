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
    AttemptResultReport, AttemptTicket, MAX_ATTEMPT_EXPANDED_BYTES, MAX_ATTEMPT_STORED_BYTES,
    MAX_SEARCH_ATTEMPTS, MAX_SEARCH_DEADLINE_SECONDS, MAX_SEARCH_EXPANDED_BYTES,
    MAX_SEARCH_STORED_BYTES, MAX_SEARCH_TRANSFER_BYTES, NATIVE_ENVIRONMENT, ResourceUsage,
    RuntimeConstraintWire, SatisfyBudget, SatisfyPolicy, SatisfyRequest, ServeConfig, Settlement,
    Submission, TicketResourceBudget, UnknownAttempt, execute_ticket, new_search_id,
    prepare_submission,
};

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
        SatisfyBudget::ceilings(4, "first_pass"),
        "search_test",
    )
    .expect("the requester plans the route")
}

#[test]
fn memory_and_file_source_transport_bind_identical_closure_k_and_d() {
    use ato_formation::source::{DownloadedArchive, SourceLimits};
    let dir = site(
        r#"
[[contract.require]]
id = "source-identity"
use = "ato.contract.workspace@1"
input = "workspace"
[contract.require.expect]
digest = "capture"
"#,
    );
    let scratch = tempfile::tempdir().unwrap();
    let submission = submit(dir.path(), scratch.path());
    let mut bytes = Vec::new(); // Small fixture only: exercise the legacy memory API.
    std::io::Read::read_to_end(&mut submission.source_file().unwrap(), &mut bytes).unwrap();
    let verified = DownloadedArchive::new(bytes)
        .verify_archive_digest(&submission.request.source.archive_digest)
        .unwrap()
        .verify_tree_digest(None, SourceLimits::default())
        .unwrap();
    let closure = verified.closure_ref("").unwrap();
    assert_eq!(closure.as_str(), submission.request.source.closure_ref);
    let tree = verified
        .materialize(
            &scratch.path().join("legacy-tree"),
            "",
            SourceLimits::default(),
        )
        .unwrap();
    let route = std::fs::read_to_string(tree.join("capsule.toml")).unwrap();
    let draft = ato_formation::capsule_toml::parse_capsule_toml(&route).unwrap();
    let planned = ato_formation_worker::job::plan_candidate(
        &draft,
        &closure,
        &ato_formation::detect::detect(&tree).unwrap(),
        BTreeMap::new(),
        "/app",
        &ato_formation_worker::local::host_triple(),
    )
    .unwrap();
    assert_eq!(planned.contract_ref, submission.request.base_contract_ref);
    assert_eq!(
        planned.derivation_ref,
        submission.request.authorized_derivations[0].derivation_ref
    );
}

/// The ticket a coordinator would issue for the submission's first route.
fn ticket(submission: &Submission) -> (AttemptTicket, Vec<u8>) {
    let request = &submission.request;
    let route = &request.authorized_derivations[0];
    let mut archive = Vec::new();
    std::io::Read::read_to_end(&mut submission.source_file().unwrap(), &mut archive).unwrap();
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
            input: ato_formation_worker::runtime_network::AttemptInput::Source {
                capsule_toml: route.capsule_toml.clone(),
                archive_digest: request.source.archive_digest.clone(),
            },
            browser_contract: None,
            bindings: BTreeMap::new(),
            network: "denied".to_owned(),
            // What a Coordinator reserves for a fresh search at the ceilings.
            resource_budget: TicketResourceBudget {
                transfer_bytes: archive.len() as u64,
                expanded_bytes: MAX_ATTEMPT_EXPANDED_BYTES,
                stored_bytes: MAX_ATTEMPT_STORED_BYTES,
            },
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

const FIXTURES: [(&str, &str); 10] = [
    (
        "satisfy-request",
        include_str!("fixtures/runtime-network-v0/satisfy-request.json"),
    ),
    (
        "satisfy-source-object",
        include_str!("fixtures/runtime-network-v0/satisfy-source-object.json"),
    ),
    (
        "attempt-ticket",
        include_str!("fixtures/runtime-network-v0/attempt-ticket.json"),
    ),
    (
        "search-budget-ceilings",
        include_str!("fixtures/runtime-network-v0/search-budget-ceilings.json"),
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
        let reserialized = if name == "satisfy-request" || name == "satisfy-source-object" {
            let request: SatisfyRequest = serde_json::from_value(value.clone()).expect(name);
            assert!(request.search_id.starts_with("search_"));
            request
                .budget
                .validate()
                .expect("the fixture budget is within the ceilings");
            serde_json::to_value(&request)
        } else if name == "attempt-ticket" {
            let ticket: ato_formation_worker::runtime_network::LegacySourceTicket =
                serde_json::from_value(value.clone()).expect(name);
            let current = AttemptTicket::from_wire(value.clone()).expect("legacy read view");
            assert!(matches!(
                current.input,
                ato_formation_worker::runtime_network::AttemptInput::Source { .. }
            ));
            serde_json::to_value(&ticket)
        } else if name == "search-budget-ceilings" {
            // The Coordinator checks its constants against the same file.
            assert_eq!(
                value,
                serde_json::json!({
                    "max_attempts": MAX_SEARCH_ATTEMPTS,
                    "deadline_seconds": MAX_SEARCH_DEADLINE_SECONDS,
                    "max_transfer_bytes": MAX_SEARCH_TRANSFER_BYTES,
                    "max_expanded_bytes": MAX_SEARCH_EXPANDED_BYTES,
                    "max_stored_bytes": MAX_SEARCH_STORED_BYTES,
                    "max_attempt_expanded_bytes": MAX_ATTEMPT_EXPANDED_BYTES,
                    "max_attempt_stored_bytes": MAX_ATTEMPT_STORED_BYTES,
                })
            );
            Ok(value.clone())
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
    let finished = include_str!("fixtures/runtime-network-v0/attempt-result-finished.json");
    let mut missing: serde_json::Value = serde_json::from_str(finished).unwrap();
    missing["attestation"]
        .as_object_mut()
        .unwrap()
        .remove("attempt_record");
    assert!(serde_json::from_value::<AttemptResultReport>(missing).is_err());
    // Nor one without its resource usage, nor a ticket without its caps:
    // a Runtime never runs an attempt it could not hold to a budget.
    let mut unmeasured: serde_json::Value = serde_json::from_str(finished).unwrap();
    unmeasured.as_object_mut().unwrap().remove("resource_usage");
    assert!(serde_json::from_value::<AttemptResultReport>(unmeasured).is_err());
    let mut uncapped: serde_json::Value = serde_json::from_str(include_str!(
        "fixtures/runtime-network-v0/attempt-ticket.json"
    ))
    .unwrap();
    uncapped.as_object_mut().unwrap().remove("resource_budget");
    assert!(serde_json::from_value::<AttemptTicket>(uncapped).is_err());
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
    if let ato_formation_worker::runtime_network::AttemptInput::Source { capsule_toml, .. } =
        &mut ticket.input
    {
        *capsule_toml = "not valid TOML".into();
    }
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

// ── a ticket's caps are hard limits (ADR-031) ─────────────────────────────

/// The logical bytes a site's source expands to: its files' content.
fn tree_bytes(dir: &Path) -> u64 {
    ["index.html", "capsule.toml"]
        .iter()
        .map(|name| std::fs::metadata(dir.join(name)).expect("file").len())
        .sum()
}

fn formation_outcome(report: &AttemptResultReport, name: &str) -> serde_json::Value {
    report.formation_attempt.as_ref().expect("attempt evidence")["outcomes"][name].clone()
}

#[test]
fn a_source_longer_than_the_transfer_cap_is_refused_before_it_is_used() {
    let dir = site("");
    let scratch = tempfile::tempdir().expect("scratch");
    let (mut ticket, archive) = ticket(&submit(dir.path(), scratch.path()));
    ticket.resource_budget.transfer_bytes = archive.len() as u64 - 1;
    let report = run(&ticket, archive, scratch.path());
    assert_eq!(report.outcome, "inconclusive");
    assert_eq!(failure_code(&report), "search_transfer_budget_exceeded");
    assert!(!report.attestation.execution_started);
    assert_eq!(
        report.attestation.attempt_record,
        AttemptRecordState::NotStarted
    );
    assert_eq!(report.resource_usage, ResourceUsage::default());
}

#[test]
fn the_runtime_reads_at_most_one_byte_past_the_transfer_cap() {
    use std::io::{Read as _, Write as _};
    for with_length in [true, false] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listen");
        let port = listener.local_addr().expect("address").port();
        // A coordinator (or anything in between) that sends far more than the
        // ticket authorized.
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            let body = vec![b'x'; 256 * 1024];
            let _ = stream.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/x-tar\r\n{}connection: close\r\n\r\n",
                if with_length { format!("content-length: {}\r\n", body.len()) } else { String::new() }
            )
            .as_bytes(),
        );
            let _ = stream.write_all(&body);
        });
        let client = ato_formation_worker::runtime_network::Client::new(
            &format!("http://127.0.0.1:{port}"),
            "token",
        )
        .expect("client");
        let root = tempfile::tempdir().unwrap();
        let received = client.source("att_test", 100, 1, root.path());
        assert!(
            received
                .unwrap_err()
                .to_string()
                .contains("exceeds transfer budget")
        );
        let _ = server.join();
    }
}

#[test]
fn a_tree_larger_than_the_expanded_cap_is_refused_before_it_runs() {
    let dir = site("");
    let scratch = tempfile::tempdir().expect("scratch");
    let (mut ticket, archive) = ticket(&submit(dir.path(), scratch.path()));
    ticket.resource_budget.expanded_bytes = tree_bytes(dir.path()) - 1;
    let report = run(&ticket, archive, scratch.path());
    assert_eq!(report.outcome, "inconclusive");
    assert_eq!(failure_code(&report), "search_expanded_budget_exceeded");
    assert!(!report.attestation.execution_started);
    assert_eq!(
        report.attestation.attempt_record,
        AttemptRecordState::NotStarted
    );
    // Refused while measuring: nothing of the tree was expanded.
    assert_eq!(report.resource_usage.expanded_bytes, 0);
    assert!(report.verifier_receipts.is_empty());
    assert!(report.materialization_ref.is_none());
}

#[test]
fn an_attempt_reports_the_logical_bytes_it_expanded_and_kept() {
    let dir = site("");
    let tree = tree_bytes(dir.path());
    let scratch = tempfile::tempdir().expect("scratch");
    let (mut ticket, archive) = ticket(&submit(dir.path(), scratch.path()));
    // Exactly the tree fits.
    ticket.resource_budget.expanded_bytes = tree;
    let report = run(&ticket, archive, scratch.path());
    assert_eq!(report.outcome, "pass", "{:?}", report.failure);
    assert_eq!(report.resource_usage.expanded_bytes, tree);
    let kept = report.resource_usage.stored_bytes;
    assert!(kept > 0, "a kept static bundle has bytes");
    // What it reports is what the store holds for the artifact.
    let bundle = scratch
        .path()
        .join("out/bundles")
        .join(&report.materialization_ref.as_deref().expect("kept")["sha256:".len()..]);
    let mut on_disk = 0;
    let mut pending = vec![bundle];
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(path).expect("bundle") {
            let entry = entry.expect("entry");
            let metadata = std::fs::symlink_metadata(entry.path()).expect("metadata");
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                on_disk += metadata.len();
            }
        }
    }
    assert_eq!(kept, on_disk);

    // A stored cap of exactly that size keeps it too.
    let exact = tempfile::tempdir().expect("scratch");
    let (mut capped, archive) = self::ticket(&submit(dir.path(), exact.path()));
    capped.resource_budget.stored_bytes = kept;
    let report = run(&capped, archive, exact.path());
    assert_eq!(report.outcome, "pass", "{:?}", report.failure);
    assert_eq!(report.resource_usage.stored_bytes, kept);
}

#[test]
fn an_artifact_over_the_stored_cap_is_verified_but_not_kept() {
    let dir = site("");
    let scratch = tempfile::tempdir().expect("scratch");
    let (mut ticket, archive) = ticket(&submit(dir.path(), scratch.path()));
    ticket.resource_budget.stored_bytes = 1;
    let report = run(&ticket, archive, scratch.path());
    // Not a pass: a Runtime Network route needs the artifact kept.
    assert_eq!(report.outcome, "fail");
    assert_eq!(failure_code(&report), "search_stored_budget_exceeded");
    assert_eq!(report.failure.as_ref().expect("failure").stage, "publish");
    assert!(report.materialization_ref.is_none());
    // It ran and finished; only keeping its artifact was refused.
    assert_eq!(
        report.attestation.attempt_record,
        AttemptRecordState::Finished
    );
    assert!(report.resource_usage.expanded_bytes > 0);
    assert_eq!(report.resource_usage.stored_bytes, 0);
    // K verification stands, with its receipt; publication failed on its own.
    assert_eq!(
        formation_outcome(&report, "runtime_verification")["state"],
        "succeeded"
    );
    assert_eq!(
        formation_outcome(&report, "publication"),
        serde_json::json!({ "state": "failed", "reason": "search_stored_budget_exceeded" })
    );
    let receipt = report
        .verifier_receipts
        .iter()
        .find(|receipt| receipt["kind"] == "contract_verification")
        .expect("the verification receipt is kept as evidence")["receipt"]
        .clone();
    assert_eq!(receipt["fully_satisfied"], true);
    assert_eq!(receipt["execution"]["attempt_id"], "att_test");
    // Nothing was written to the store.
    let bundles = scratch.path().join("out/bundles");
    assert!(
        !bundles.exists()
            || std::fs::read_dir(&bundles)
                .expect("bundles")
                .next()
                .is_none(),
        "an artifact over its stored cap is not kept"
    );
}

#[test]
fn a_search_budget_above_a_ceiling_is_never_sent() {
    let within = SatisfyBudget::ceilings(MAX_SEARCH_ATTEMPTS, "all");
    within
        .validate()
        .expect("the ceilings themselves are a valid budget");
    for over in [
        SatisfyBudget {
            max_attempts: MAX_SEARCH_ATTEMPTS + 1,
            ..within.clone()
        },
        SatisfyBudget {
            max_attempts: 0,
            ..within.clone()
        },
        SatisfyBudget {
            deadline_seconds: MAX_SEARCH_DEADLINE_SECONDS + 1,
            ..within.clone()
        },
        SatisfyBudget {
            max_transfer_bytes: MAX_SEARCH_TRANSFER_BYTES + 1,
            ..within.clone()
        },
        SatisfyBudget {
            max_expanded_bytes: MAX_SEARCH_EXPANDED_BYTES + 1,
            ..within.clone()
        },
        SatisfyBudget {
            max_stored_bytes: MAX_SEARCH_STORED_BYTES + 1,
            ..within.clone()
        },
        SatisfyBudget {
            mode: "some".to_owned(),
            ..within.clone()
        },
    ] {
        assert!(over.validate().is_err(), "{over:?}");
    }
    let dir = site("");
    let scratch = tempfile::tempdir().expect("scratch");
    let refused = prepare_submission(
        dir.path(),
        &[],
        None,
        &scratch.path().join("submit"),
        RuntimeConstraintWire::Any,
        SatisfyPolicy {
            network: "denied".to_owned(),
            allow_managed: false,
        },
        SatisfyBudget {
            max_stored_bytes: MAX_SEARCH_STORED_BYTES + 1,
            ..within
        },
        "search_test",
    );
    assert!(refused.is_err());
}

#[test]
fn an_attempt_never_expands_more_than_the_runtime_source_ceiling() {
    // The Coordinator caps an attempt's expanded reservation at this; it is
    // the ceiling the Runtime enforces on every source anyway.
    assert_eq!(
        MAX_ATTEMPT_EXPANDED_BYTES,
        ato_formation::source::SourceLimits::default().max_total_bytes
    );
}

#[test]
fn pax_boundary_refusal_leaves_no_tree_and_preserves_started_history() {
    use sha2::{Digest, Sha256};
    let dir = site("");
    let scratch = tempfile::tempdir().unwrap();
    let (mut ticket, _) = ticket(&submit(dir.path(), scratch.path()));
    let mut builder = tar::Builder::new(Vec::new());
    builder
        .append_pax_extensions([("size", b"65536".as_slice())])
        .unwrap();
    let mut header = tar::Header::new_gnu();
    header.set_path("file").unwrap();
    header.set_size(0);
    header.set_cksum();
    // Keep the PAX header, then append the mismatched header and physical bytes.
    let mut archive = builder.into_inner().unwrap();
    archive.truncate(1024);
    archive.extend_from_slice(header.as_bytes());
    archive.extend_from_slice(&vec![0; 65536 + 1024]);
    if let ato_formation_worker::runtime_network::AttemptInput::Source { archive_digest, .. } =
        &mut ticket.input
    {
        *archive_digest = format!("sha256:{:x}", Sha256::digest(&archive));
    }
    ticket.resource_budget.transfer_bytes = archive.len() as u64;
    ticket.resource_budget.expanded_bytes = 1024;
    let report = run(&ticket, archive.clone(), scratch.path());
    assert_eq!(failure_code(&report), "ticket_unplannable");
    assert!(report.failure.unwrap().message.contains("PAX size"));
    assert_eq!(
        report.attestation.attempt_record,
        AttemptRecordState::NotStarted
    );
    assert!(!report.attestation.execution_started);
    assert_eq!(report.resource_usage.expanded_bytes, 0);
    assert!(
        !scratch
            .path()
            .join("work")
            .join(&ticket.attempt_id)
            .exists()
    );
    let scratch = tempfile::tempdir().unwrap();
    leave_started(&ticket, scratch.path());
    let report = run(&ticket, archive.clone(), scratch.path());
    assert_eq!(failure_code(&report), "attempt_already_started");
    assert_eq!(
        report.attestation.attempt_record,
        AttemptRecordState::StartedUnfinished
    );
    let prior_attempt = ticket.attempt_id.clone();
    ticket.attempt_id.push_str("-next");
    let report = run(&ticket, archive, scratch.path());
    assert_eq!(
        report.attestation.attempt_record,
        AttemptRecordState::BlockedByUnknown {
            attempt_id: prior_attempt,
        }
    );
}

/// Produces an ordinary, correctly planned K/D request for the real API
/// acceptance harness. The harness replaces only its source transport object.
#[test]
#[ignore = "manual Linux Network acceptance fixture; requires ATO_HARDENING_REQUEST"]
fn export_source_hardening_request() {
    let output = std::env::var_os("ATO_HARDENING_REQUEST").expect("fixture output path");
    let dir = site("");
    let route = STATIC_ROUTE.replace(
        "[[derive.step]]",
        r#"[[derive.step]]
id = "build-marker"
use = "ato.process@1"
op = "exec"
argv = ["/bin/sh", "-c", "echo BUILD_STARTED; echo started > build-started.marker"]

[[derive.step]]"#,
    );
    std::fs::write(dir.path().join("capsule.toml"), route).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let submission = submit(dir.path(), scratch.path());
    std::fs::write(
        output,
        serde_json::to_vec_pretty(&submission.request).unwrap(),
    )
    .unwrap();
}

#[test]
fn typed_source_and_retained_tickets_roundtrip_without_nullable_source_fields() {
    for bytes in [
        include_str!("fixtures/runtime-network-v0/attempt-ticket-source.json"),
        include_str!("fixtures/runtime-network-v0/attempt-ticket-retained.json"),
    ] {
        let value: serde_json::Value = serde_json::from_str(bytes).unwrap();
        let ticket = AttemptTicket::from_wire(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(ticket).unwrap(), value);
        assert!(value.get("capsule_toml").is_none());
        assert!(value.get("archive_digest").is_none());
    }
}
