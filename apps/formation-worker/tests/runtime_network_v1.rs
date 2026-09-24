//! Runtime Network Phase 1 — what a Runtime establishes about a ticket by
//! itself, before anything of the candidate runs.
//!
//! The ticket's route and archive are the authority; the request's metadata
//! (effects, requirements) is only a scheduling hint. Every case here uses a
//! static route, which needs no sandbox, so it runs on any host.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ato_formation_worker::runtime_network::{
    AttemptResultReport, AttemptTicket, NATIVE_ENVIRONMENT, RuntimeConstraintWire, SatisfyBudget,
    SatisfyPolicy, ServeConfig, Submission, execute_ticket, prepare_submission,
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
            },
        )
        .expect("begun");
    drop(unfinished);

    let report = run(&ticket, archive, scratch.path());
    assert_eq!(failure_code(&report), "request_effect_unknown");
    assert!(
        !report.attestation.execution_started,
        "this attempt did not start"
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
