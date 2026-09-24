//! After the verification point: a Formation stops the candidate, a Run
//! keeps it (ADR-026 outcomes, roadmap stage 2b).
//!
//! The receipt is fixed when K is decided. `Continuation::Stop` then stops
//! the candidate; `Continuation::HandOff` leaves a verified candidate
//! running and hands it to the caller, whose cleanup is `not_attempted`
//! (`handed_off`), not a failure. An unverified candidate is stopped either
//! way. Static cases run on every host; the process case needs bwrap and
//! the Python toolchain.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ato_formation::capsule_toml::parse_capsule_toml;
use ato_formation::detect::detect;
use ato_formation::request::{AttemptStatus, OutcomeState};
use ato_formation::source::{DownloadedArchive, SourceLimits};
use ato_formation_worker::attempt::{AttemptOutcome, AttemptRequest, Continuation, run_attempt};
use ato_formation_worker::executor::LocalAttemptExecutor;
use ato_formation_worker::job::{PlannedCandidate, digest, plan_candidate};
use ato_formation_worker::journal::AttemptJournal;
use ato_formation_worker::local::{host_triple, probe_local_runtime, snapshot_directory};
use ato_formation_worker::sandbox::{
    BuildLimits, NetworkPolicy, TOOLCHAIN_ROOT, containment_available,
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

[[port]]
id = "app.http"
use = "ato.http@1"
from = "site"

[[contract.require]]
id = "page"
use = "ato.contract.http@1"
port = "app.http"
path = "PATH"

[contract.require.expect]
status = 200
"#;

const PROCESS_ROUTE: &str = r#"
schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[runtime]]
name = "python"
version = "3.12.7"

[[derive.step]]
id = "app"
use = "ato.process@1"
op = "serve"
argv = ["/opt/ato/toolchains/python/3.12.7/bin/python3", "-B", "/app/server.py"]

[[port]]
id = "app.http"
use = "ato.http@1"
from = "app"
guest_port = 8000

[[contract.require]]
id = "app-responds"
use = "ato.contract.http@1"
port = "app.http"
path = "/health"

[contract.require.expect]
status = 200
"#;

const SERVER: &str = r#"
import os
from http.server import BaseHTTPRequestHandler, HTTPServer

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200 if self.path == "/health" else 404)
        self.end_headers()
        self.wfile.write(b"ok")

    def log_message(self, *args):
        pass

HTTPServer(("127.0.0.1", int(os.environ["ATO_ENDPOINT_APP_HTTP_PORT"])), Handler).serve_forever()
"#;

struct Fixture {
    scratch: tempfile::TempDir,
    source_root: PathBuf,
    planned: PlannedCandidate,
}

/// Freeze `files` the way a Formation does and plan `route` against it.
fn fixture(files: &[(&str, &str)], route: &str) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, contents) in files {
        std::fs::write(dir.path().join(name), contents).expect("write");
    }
    let archive = snapshot_directory(dir.path()).expect("snapshot");
    let archive_digest = digest(&archive);
    let verified = DownloadedArchive::new(archive)
        .verify_archive_digest(&archive_digest)
        .and_then(|archive| archive.verify_tree_digest(None, SourceLimits::default()))
        .expect("a usable source");
    let closure_ref = verified.closure_ref("").expect("closure");
    let scratch = tempfile::tempdir().expect("scratch");
    let source_root = verified
        .materialize(&scratch.path().join("tree"), "", SourceLimits::default())
        .expect("materialized");
    let evidence = detect(&source_root).expect("detection");
    let draft = parse_capsule_toml(route).expect("route");
    let planned = plan_candidate(
        &draft,
        &closure_ref,
        &evidence,
        BTreeMap::new(),
        "/app",
        &host_triple(),
    )
    .expect("planned");
    Fixture {
        scratch,
        source_root,
        planned,
    }
}

fn attempt(
    fixture: &Fixture,
    continuation: Continuation,
    network: NetworkPolicy,
) -> AttemptOutcome {
    let shim = PathBuf::from(env!("CARGO_BIN_EXE_ato-formation-worker"));
    let attempt_root = fixture.scratch.path().join("attempt");
    run_attempt(
        &AttemptRequest {
            request_id: "req-handoff",
            attempt_id: "att-handoff",
            label: "authored",
            candidate: &fixture.planned,
            contract_ref: &fixture.planned.contract_ref,
            source_root: &fixture.source_root,
            runtime_id: "local",
            profile: &probe_local_runtime(),
            network,
            browser: None,
            attempt_root: &attempt_root,
            shim: &shim,
            continuation,
        },
        &LocalAttemptExecutor {
            shim: shim.clone(),
            network,
            limits: BuildLimits::default(),
        },
        &AttemptJournal::new(fixture.scratch.path().join("records")),
    )
}

fn get(url: &str) -> Option<u16> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?
        .get(url)
        .send()
        .ok()
        .map(|response| response.status().as_u16())
}

fn static_site(path: &str) -> Fixture {
    fixture(
        &[("index.html", "<!doctype html><h1>hi</h1>")],
        &STATIC_ROUTE.replace("PATH", path),
    )
}

#[test]
fn a_handed_off_static_candidate_keeps_serving_until_its_new_owner_stops_it() {
    let site = static_site("/");
    let outcome = attempt(&site, Continuation::HandOff, NetworkPolicy::Denied);
    let attempt = &outcome.attempt;
    assert_eq!(
        attempt.status,
        AttemptStatus::Verified,
        "{:?}",
        attempt.failure
    );
    assert!(attempt.receipt.as_ref().expect("a receipt").fully_satisfied);
    let outcomes = &attempt.outcomes;
    assert_eq!(outcomes.runtime_verification.state, OutcomeState::Succeeded);
    assert_eq!(outcomes.seal.state, OutcomeState::Succeeded);
    // Handed off is normal, not a cleanup failure.
    assert_eq!(outcomes.cleanup.state, OutcomeState::NotAttempted);
    assert_eq!(outcomes.cleanup.reason.as_deref(), Some("handed_off"));
    assert_eq!(outcomes.publication.state, OutcomeState::NotAttempted);
    assert!(!attempt.realization.as_ref().unwrap().destroyed);

    let live = outcome.live.expect("the verified candidate is handed off");
    let endpoint = live.endpoint().expect("an endpoint");
    assert_eq!(get(&endpoint), Some(200), "still serving after the attempt");
    live.stop().expect("stopped by its new owner");
    assert_eq!(get(&endpoint), None, "gone once stopped");
}

#[test]
fn an_unverified_candidate_is_stopped_even_when_a_hand_off_was_asked_for() {
    let site = static_site("/missing.html");
    let outcome = attempt(&site, Continuation::HandOff, NetworkPolicy::Denied);
    assert!(outcome.live.is_none());
    let attempt = &outcome.attempt;
    assert_eq!(
        attempt.failure.as_ref().unwrap().code,
        "http_status_mismatch"
    );
    // The receipt says what was observed; stopping afterwards changes nothing in it.
    assert!(!attempt.receipt.as_ref().expect("a receipt").fully_satisfied);
    let outcomes = &attempt.outcomes;
    assert_eq!(outcomes.runtime_verification.state, OutcomeState::Failed);
    assert_eq!(
        outcomes.runtime_verification.reason.as_deref(),
        Some("http_status_mismatch")
    );
    assert_eq!(outcomes.cleanup.state, OutcomeState::Succeeded);
    assert!(attempt.realization.as_ref().unwrap().destroyed);
}

#[test]
fn a_formation_attempt_stops_its_verified_candidate() {
    let site = static_site("/");
    let outcome = attempt(&site, Continuation::Stop, NetworkPolicy::Denied);
    assert!(outcome.live.is_none());
    assert!(outcome.verified.is_some());
    let outcomes = &outcome.attempt.outcomes;
    assert_eq!(outcomes.runtime_verification.state, OutcomeState::Succeeded);
    assert_eq!(outcomes.cleanup.state, OutcomeState::Succeeded);
    assert!(outcome.attempt.realization.as_ref().unwrap().destroyed);
}

#[test]
fn nothing_that_did_not_run_reports_a_cleanup() {
    let site = fixture(
        &[("index.html", "<!doctype html><h1>hi</h1>")],
        &format!(
            "{}\n[effects]\ndefault = \"non-repeatable\"\n",
            STATIC_ROUTE.replace("PATH", "/")
        ),
    );
    let outcome = attempt(&site, Continuation::HandOff, NetworkPolicy::Denied);
    assert!(!outcome.execution_started);
    let outcomes = &outcome.attempt.outcomes;
    assert_eq!(
        outcomes.runtime_verification.state,
        OutcomeState::NotAttempted
    );
    assert_eq!(
        outcomes.runtime_verification.reason.as_deref(),
        Some("effect_policy")
    );
    assert_eq!(outcomes.cleanup.state, OutcomeState::NotApplicable);
    assert_eq!(outcomes.publication.state, OutcomeState::NotAttempted);
}

#[test]
fn a_handed_off_process_candidate_keeps_running_until_its_new_owner_stops_it() {
    if !containment_available()
        || !Path::new(TOOLCHAIN_ROOT)
            .join("python/3.12.7/bin/python3")
            .is_file()
    {
        eprintln!("skipping: no bwrap or no provisioned Python 3.12.7");
        return;
    }
    let app = fixture(&[("server.py", SERVER)], PROCESS_ROUTE);
    let outcome = attempt(
        &app,
        Continuation::HandOff,
        NetworkPolicy::DependencyResolution,
    );
    let attempt = &outcome.attempt;
    assert_eq!(
        attempt.status,
        AttemptStatus::Verified,
        "{:?}",
        attempt.failure
    );
    assert_eq!(
        attempt.outcomes.cleanup.reason.as_deref(),
        Some("handed_off")
    );
    let live = outcome.live.expect("handed off");
    let endpoint = live.endpoint().expect("an endpoint");
    assert_eq!(get(&format!("{endpoint}health")), Some(200));
    live.stop().expect("stopped");
    assert_eq!(get(&format!("{endpoint}health")), None);
}

#[test]
fn dropping_a_handed_off_static_candidate_stops_it() {
    let site = static_site("/");
    let outcome = attempt(&site, Continuation::HandOff, NetworkPolicy::Denied);
    let live = outcome.live.expect("handed off");
    let endpoint = live.endpoint().expect("an endpoint");
    assert_eq!(get(&endpoint), Some(200));
    drop(live);
    assert_eq!(get(&endpoint), None, "gone once its owner drops it");
}

#[test]
fn dropping_a_handed_off_process_candidate_stops_it() {
    if !containment_available()
        || !Path::new(TOOLCHAIN_ROOT)
            .join("python/3.12.7/bin/python3")
            .is_file()
    {
        eprintln!("skipping: no bwrap or no provisioned Python 3.12.7");
        return;
    }
    let app = fixture(&[("server.py", SERVER)], PROCESS_ROUTE);
    let outcome = attempt(
        &app,
        Continuation::HandOff,
        NetworkPolicy::DependencyResolution,
    );
    let live = outcome.live.expect("handed off");
    let endpoint = live.endpoint().expect("an endpoint");
    assert_eq!(get(&format!("{endpoint}health")), Some(200));
    drop(live);
    assert_eq!(get(&format!("{endpoint}health")), None);
    assert!(
        !app.scratch.path().join("attempt/realization").exists(),
        "the realization scratch went with it"
    );
}
