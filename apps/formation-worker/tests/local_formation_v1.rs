//! Local Formation — Phase 1 (ADR-019).
//!
//! 'I -- D on this Runtime --> C', then 'C |= K' observed for real: a static
//! candidate is decided from the artifact it produced, a process candidate is
//! launched ephemerally and measured over loopback HTTP, and nothing deferred
//! reaches 'Formed'.
//!
//! What runs where: the static lane and the ephemeral observer need no
//! sandbox, so they are exercised on every host. A candidate whose build plan
//! has steps needs bwrap — on a host without it the attempt is Filtered,
//! which is itself the evidence the test asserts.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use ato_formation::intent::{DependencyPlan, Lane, PROGRAM_INTENT_V1_SCHEMA, ProgramIntentV1};
use ato_formation::request::{
    AttemptStatus, ContractSource, FormationNetworkPolicy, FormationPolicy, FormationRequest,
    FormationResult, InitialCondition, RuntimeConstraint, SearchBudget,
};
use ato_formation::source::SourceLimits;
use ato_formation_worker::ephemeral::{RequiredObservation, observe_process_candidate};
use ato_formation_worker::local::{self, LocalFormation};
use ato_formation_worker::sandbox::{BuildLimits, containment_available};
use sha2::{Digest as _, Sha256};

fn site(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, contents) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }
    dir
}

fn formation(scratch: &tempfile::TempDir) -> LocalFormation {
    LocalFormation {
        work_root: scratch.path().join("work"),
        out_dir: scratch.path().join("out"),
        // This package's own worker binary: the one binary a contained build
        // can re-enter as sandbox-exec. The test binary itself has no such
        // entry point.
        shim: PathBuf::from(env!("CARGO_BIN_EXE_ato-formation-worker")),
        limits: BuildLimits::default(),
        source_limits: SourceLimits::default(),
    }
}

fn request(path: &Path, network: FormationNetworkPolicy) -> FormationRequest {
    FormationRequest {
        initial_condition: InitialCondition::LocalDirectory {
            path: path.to_path_buf(),
        },
        contract: ContractSource::Infer,
        runtime: RuntimeConstraint::Exact {
            runtime_id: "local".to_owned(),
        },
        policy: FormationPolicy { network },
        budget: SearchBudget { max_attempts: 4 },
    }
}

// ── formed ──────────────────────────────────────────────────────────────────

#[test]
fn a_static_site_is_formed_end_to_end() {
    let dir = site(&[
        ("index.html", "<!doctype html><h1>hello</h1>"),
        ("style.css", "body{margin:0}"),
    ]);
    let scratch = tempfile::tempdir().expect("tempdir");
    let result = local::run(
        &request(dir.path(), FormationNetworkPolicy::Denied),
        &formation(&scratch),
    )
    .expect("the driver ran");

    let FormationResult::Formed {
        contract_ref,
        verified_routes,
        attempts,
    } = result
    else {
        panic!("expected formed, got {result:?}");
    };
    assert!(contract_ref.starts_with("sha256:"));
    assert_eq!(verified_routes.len(), 1);
    assert_eq!(verified_routes[0].runtime_id, "local");
    assert!(
        verified_routes[0]
            .materialization_ref
            .starts_with("sha256:")
    );

    // The attempt that formed it was VERIFIED — every Contract observation
    // decided satisfied, nothing deferred.
    let verified = attempts
        .iter()
        .find(|attempt| attempt.status == AttemptStatus::Verified)
        .expect("a verified attempt exists");
    assert_eq!(verified.candidate, "static-files/v1");
    assert!(
        verified
            .verification
            .as_ref()
            .expect("a verdict set")
            .fully_satisfied()
    );

    // And the artifact was kept, content-addressed, under --out.
    assert!(scratch.path().join("out/bundles").is_dir());
}

#[test]
fn a_source_can_try_several_routes_and_the_second_can_win() {
    // package.json + lockfile + build script AND a root index.html fit two
    // presets. The build route is tried first; where it cannot run, the
    // already-a-site route still forms — a second CANDIDATE, never a second
    // Runtime.
    let dir = site(&[
        ("index.html", "<!doctype html><h1>already built</h1>"),
        ("package.json", r#"{"scripts":{"build":"x"}}"#),
        ("package-lock.json", r#"{"lockfileVersion":3}"#),
    ]);
    let scratch = tempfile::tempdir().expect("tempdir");
    let result = local::run(
        &request(dir.path(), FormationNetworkPolicy::Denied),
        &formation(&scratch),
    )
    .expect("the driver ran");

    let FormationResult::Formed { attempts, .. } = result else {
        panic!("expected formed via the second route, got {result:?}");
    };
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].candidate, "node-static/v1");
    assert_ne!(attempts[0].status, AttemptStatus::Verified);
    assert_eq!(attempts[1].candidate, "static-files/v1");
    assert_eq!(attempts[1].status, AttemptStatus::Verified);
}

// ── no verified route ───────────────────────────────────────────────────────

#[test]
fn an_unsupported_source_reports_evidence_not_a_fallback() {
    let dir = site(&[("data.bin", "not an app")]);
    let scratch = tempfile::tempdir().expect("tempdir");
    let result = local::run(
        &request(dir.path(), FormationNetworkPolicy::Denied),
        &formation(&scratch),
    )
    .expect("the driver ran");

    let FormationResult::NoVerifiedRoute { attempts, .. } = result else {
        panic!("expected no_verified_route, got {result:?}");
    };
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].status, AttemptStatus::Filtered);
    assert_eq!(
        attempts[0].failure.as_ref().expect("a reason").code,
        "preset_no_match"
    );
}

#[test]
fn an_authored_route_is_the_only_candidate() {
    // capsule.toml present: exactly one candidate, named "authored". A preset
    // is never synthesized beside it — the strict rule.
    let dir = site(&[
        (
            "main.py",
            "print('hello')
",
        ),
        (
            "capsule.toml",
            r#"
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
argv = ["/opt/ato/toolchains/python/3.12.7/bin/python3", "-B", "main.py"]

[[port]]
id = "app.http"
use = "ato.http@1"
from = "app"
guest_port = 8000

[[contract.require]]
id = "app-responds"
use = "ato.contract.http@1"
port = "app.http"
method = "GET"
path = "/health"

[contract.require.expect]
status = 200

[[contract.require]]
id = "source-identity"
use = "ato.contract.workspace@1"
input = "workspace"

[contract.require.expect]
digest = "capture"
"#,
        ),
    ]);
    let scratch = tempfile::tempdir().expect("tempdir");
    let result = local::run(
        &request(dir.path(), FormationNetworkPolicy::DependencyResolution),
        &formation(&scratch),
    )
    .expect("the driver ran");

    let attempts = match &result {
        FormationResult::Formed { attempts, .. } => attempts,
        FormationResult::NoVerifiedRoute { attempts, .. } => attempts,
    };
    assert_eq!(attempts.len(), 1, "the authored route and nothing else");
    assert_eq!(attempts[0].candidate, "authored");

    if !containment_available() {
        // This host cannot contain the toolchain-provisioning build step, so
        // the attempt is filtered BEFORE execution — evidence, not a retry
        // somewhere else.
        assert_eq!(attempts[0].status, AttemptStatus::Filtered);
        assert_eq!(
            attempts[0].failure.as_ref().expect("a reason").code,
            "runtime_cannot_contain_build"
        );
    }
}

#[test]
fn an_invalid_authored_document_fails_hard() {
    // A capsule.toml that does not parse stops the Formation — substituting a
    // preset guess for a route somebody wrote is the failure mode the strict
    // rule exists to prevent. index.html is present on purpose: a fallback
    // would have formed it.
    let dir = site(&[
        ("index.html", "<!doctype html><h1>hello</h1>"),
        (
            "capsule.toml",
            r#"schema = "ato.capsule/1"
[[[broken"#,
        ),
    ]);
    let scratch = tempfile::tempdir().expect("tempdir");
    let error = local::run(
        &request(dir.path(), FormationNetworkPolicy::Denied),
        &formation(&scratch),
    )
    .expect_err("an authored document that does not parse is a hard error");
    assert!(
        error
            .downcast_ref::<ato_formation::failure::FormationFailure>()
            .is_some(),
        "the failure is typed, not anonymous: {error:#}"
    );
}

#[test]
fn a_runtime_constraint_other_than_local_is_refused() {
    let dir = site(&[("index.html", "<!doctype html><h1>hello</h1>")]);
    let scratch = tempfile::tempdir().expect("tempdir");
    let mut req = request(dir.path(), FormationNetworkPolicy::Denied);
    req.runtime = RuntimeConstraint::Exact {
        runtime_id: "managed".to_owned(),
    };
    // There is no client for anywhere else — a non-local Runtime cannot even
    // be asked, so this is an error rather than a formed-Nothing.
    local::run(&req, &formation(&scratch)).expect_err("only 'local' exists");
}

// ── ephemeral observation ───────────────────────────────────────────────────

/// A candidate process is launched, measured over real loopback HTTP, and
/// torn down — the property ADR-019 adds to Phase 1. This needs no sandbox:
/// the observer runs on the host, which is what 'local' means.
#[test]
fn a_process_candidate_is_observed_over_real_http() {
    if std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("skipping: python3 is not on this host");
        return;
    }
    let dir = site(&[
        ("health", "ok"),
        (
            "server.py",
            r#"
import os
from http.server import BaseHTTPRequestHandler, HTTPServer

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/health":
            body = open("health", "rb").read()
            self.send_response(200)
        else:
            body = b""
            self.send_response(404)
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass

HTTPServer(("127.0.0.1", int(os.environ["PORT"])), Handler).serve_forever()
"#,
        ),
    ]);

    // A free port, asked of the kernel rather than guessed. An outer
    // sandbox can deny loopback binds entirely; that is the environment
    // refusing, not the candidate, so the test says so and stops.
    let probe = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("skipping: this environment denies loopback binds ({error})");
            return;
        }
    };
    let port = probe.local_addr().expect("addr").port();
    drop(probe);

    let intent = ProgramIntentV1 {
        schema: PROGRAM_INTENT_V1_SCHEMA.to_owned(),
        lane: Lane::PythonProcess,
        runtime: BTreeMap::new(),
        dependencies: DependencyPlan::None,
        launch_argv: vec!["python3".to_owned(), "server.py".to_owned()],
        cwd_relative: ".".to_owned(),
        public_env: BTreeMap::new(),
        exported_ports: vec![("app.http".to_owned(), port)],
        readiness_http_path: None,
        state_slots: Vec::new(),
        static_output_root: None,
        static_entry_path: None,
        static_spa_fallback: false,
        static_build: None,
        static_compile: None,
    };

    let observed = observe_process_candidate(
        dir.path(),
        "/app",
        &intent,
        &[RequiredObservation {
            port_id: "app.http".to_owned(),
            port,
            path: "/health".to_owned(),
        }],
    )
    .expect("the candidate was observed");

    let http = observed
        .iter()
        .find(|http| http.port == "app.http" && http.path == "/health")
        .expect("the /health observation was recorded");
    assert_eq!(http.status, 200);
    assert_eq!(
        http.body_digest,
        format!("sha256:{:x}", Sha256::digest(b"ok"))
    );
}
