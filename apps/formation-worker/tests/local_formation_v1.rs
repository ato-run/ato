//! Local Formation — Phase 1 (ADR-019).
//!
//! 'I -- D on this Runtime --> C', then 'C |= K' observed for real: a static
//! candidate is decided from the artifact it produced, a process candidate is
//! realized temporarily on the local Runtime and measured over loopback HTTP,
//! and nothing deferred reaches 'Formed'.
//!
//! What runs where: the static lane needs no sandbox, so it is exercised on
//! every host. A candidate whose build plan has steps, or that must be run to
//! be verified, needs bwrap — on a host without it the attempt is Filtered,
//! which is itself the evidence the test asserts.

use std::path::{Path, PathBuf};

use ato_formation::request::{
    AttemptStatus, ContractSource, FormationNetworkPolicy, FormationPolicy, FormationRequest,
    FormationResult, InitialCondition, RuntimeConstraint, SearchBudget,
};
use ato_formation::source::SourceLimits;
use ato_formation_worker::executor::{
    AttemptExecution, AttemptExecutor, ExecutedCandidate, LocalAttemptExecutor,
};
use ato_formation_worker::local::{self, LocalFormation};
use ato_formation_worker::sandbox::{BuildLimits, NetworkPolicy, containment_available};

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

// ── the Initial Condition is frozen ─────────────────────────────────────────

/// Runs the real executor, but first rewrites the ORIGINAL directory — after
/// the Formation measured it and before anything is built.
struct EditsTheOriginalFirst {
    original: PathBuf,
    inner: LocalAttemptExecutor,
}

impl AttemptExecutor for EditsTheOriginalFirst {
    fn execute(&self, execution: &AttemptExecution<'_>) -> anyhow::Result<ExecutedCandidate> {
        std::fs::write(
            self.original.join("index.html"),
            "<!doctype html><h1>edited after the snapshot</h1>",
        )?;
        std::fs::write(self.original.join("late.html"), "added after the snapshot")?;
        self.inner.execute(execution)
    }
}

/// Does any file of the kept static bundle contain `needle`? The bundle is
/// content-addressed, so files are found by what they hold, not their name.
fn bundle_contains(scratch: &tempfile::TempDir, needle: &str) -> bool {
    let mut found = false;
    visit(&scratch.path().join("out/bundles"), &mut |path| {
        if std::fs::read(path)
            .map(|bytes| String::from_utf8_lossy(&bytes).contains(needle))
            .unwrap_or(false)
        {
            found = true;
        }
    });
    found
}

fn visit(dir: &Path, on_file: &mut dyn FnMut(&Path)) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit(&path, on_file);
        } else {
            on_file(&path);
        }
    }
}

fn contract_ref_of(result: &FormationResult) -> String {
    match result {
        FormationResult::Formed { contract_ref, .. } => contract_ref.clone(),
        other => panic!("expected formed, got {other:?}"),
    }
}

#[test]
fn what_is_built_is_the_snapshot_not_the_live_directory() {
    let dir = site(&[("index.html", "<!doctype html><h1>as snapshotted</h1>")]);
    let scratch = tempfile::tempdir().expect("tempdir");
    let executor = EditsTheOriginalFirst {
        original: dir.path().to_path_buf(),
        inner: LocalAttemptExecutor {
            shim: formation(&scratch).shim,
            network: NetworkPolicy::Denied,
            limits: BuildLimits::default(),
        },
    };
    let result = local::run_with_executor(
        &request(dir.path(), FormationNetworkPolicy::Denied),
        &formation(&scratch),
        &executor,
    )
    .expect("the driver ran");
    let edited_contract = contract_ref_of(&result);

    // The kept artifact is the snapshot: the edit and the late file never
    // reached the build.
    assert!(bundle_contains(&scratch, "as snapshotted"));
    assert!(!bundle_contains(&scratch, "edited after the snapshot"));
    assert!(!bundle_contains(&scratch, "added after the snapshot"));

    // And the Contract names the snapshot's identity: a fresh Formation of
    // the ORIGINAL content yields the same Contract.
    let pristine = site(&[("index.html", "<!doctype html><h1>as snapshotted</h1>")]);
    let scratch_again = tempfile::tempdir().expect("tempdir");
    let again = local::run(
        &request(pristine.path(), FormationNetworkPolicy::Denied),
        &formation(&scratch_again),
    )
    .expect("the driver ran");
    assert_eq!(contract_ref_of(&again), edited_contract);
}

#[test]
fn vcs_metadata_is_outside_the_initial_condition() {
    let plain = site(&[("index.html", "<!doctype html><h1>hello</h1>")]);
    let with_git = site(&[
        ("index.html", "<!doctype html><h1>hello</h1>"),
        (
            ".git/config",
            "[remote \"origin\"]\n\turl = https://token@example.invalid/x",
        ),
        (".git/HEAD", "ref: refs/heads/main"),
    ]);
    let scratch_plain = tempfile::tempdir().expect("tempdir");
    let scratch_git = tempfile::tempdir().expect("tempdir");
    let plain_result = local::run(
        &request(plain.path(), FormationNetworkPolicy::Denied),
        &formation(&scratch_plain),
    )
    .expect("the driver ran");
    let git_result = local::run(
        &request(with_git.path(), FormationNetworkPolicy::Denied),
        &formation(&scratch_git),
    )
    .expect("the driver ran");

    // Same measured I, so the same Contract; and nothing of .git was built.
    assert_eq!(contract_ref_of(&plain_result), contract_ref_of(&git_result));
    assert!(bundle_contains(&scratch_git, "<h1>hello</h1>"));
    assert!(!bundle_contains(&scratch_git, "token@example.invalid"));
    assert!(!bundle_contains(&scratch_git, "refs/heads/main"));
}

#[cfg(unix)]
#[test]
fn a_symlink_is_decided_by_the_source_rules_not_skipped() {
    // An uploaded archive with a link is refused by the source module; a
    // local directory is held to the same rule rather than quietly measured
    // without it.
    let dir = site(&[("index.html", "<!doctype html><h1>hello</h1>")]);
    std::os::unix::fs::symlink("/etc/hostname", dir.path().join("leak")).expect("symlink");
    let scratch = tempfile::tempdir().expect("tempdir");
    let error = local::run(
        &request(dir.path(), FormationNetworkPolicy::Denied),
        &formation(&scratch),
    )
    .expect_err("a symlink in the Initial Condition is refused");
    assert!(format!("{error:#}").contains("symlink"), "{error:#}");
}

#[test]
fn the_frozen_tree_does_not_outlive_the_formation() {
    let dir = site(&[("index.html", "<!doctype html><h1>hello</h1>")]);
    let scratch = tempfile::tempdir().expect("tempdir");
    local::run(
        &request(dir.path(), FormationNetworkPolicy::Denied),
        &formation(&scratch),
    )
    .expect("the driver ran");
    let leftovers: Vec<_> = std::fs::read_dir(scratch.path().join("work"))
        .expect("work root")
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("source-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

// ── a process candidate, end to end ─────────────────────────────────────────

const AUTHORED_PROCESS: &str = r#"
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
argv = ["/opt/ato/toolchains/python/3.12.7/bin/python3", "-B", "/app/server.py", "8000"]

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
"#;

/// Serves /health, and tries to leave a mark in its workspace on the way up.
const MARKING_SERVER: &str = r#"
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

for target in ("/app/runtime-created.txt", "/tmp/runtime-created.txt"):
    try:
        with open(target, "w") as handle:
            handle.write("written by the candidate")
    except OSError:
        pass

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"ok" if self.path == "/health" else b""
        self.send_response(200 if self.path == "/health" else 404)
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass

HTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
"#;

fn tar_entries(path: &Path) -> Vec<String> {
    let file = std::fs::File::open(path).expect("artifact");
    tar::Archive::new(file)
        .entries()
        .expect("entries")
        .map(|entry| {
            entry
                .expect("entry")
                .path()
                .expect("path")
                .to_string_lossy()
                .into_owned()
        })
        .collect()
}

#[test]
fn a_process_candidate_is_realized_verified_and_kept_without_its_side_effects() {
    let dir = site(&[
        ("server.py", MARKING_SERVER),
        ("capsule.toml", AUTHORED_PROCESS),
    ]);
    let scratch = tempfile::tempdir().expect("tempdir");
    let result = local::run(
        &request(dir.path(), FormationNetworkPolicy::DependencyResolution),
        &formation(&scratch),
    )
    .expect("the driver ran");

    let toolchain_root = Path::new(ato_formation_worker::sandbox::TOOLCHAIN_ROOT).is_dir();
    if !containment_available() || !toolchain_root {
        // Nothing can be built or run contained here, so nothing is: the
        // attempt is Filtered before execution, with the reason.
        let FormationResult::NoVerifiedRoute { attempts, .. } = &result else {
            panic!("expected no_verified_route on this host, got {result:?}");
        };
        assert_eq!(attempts[0].status, AttemptStatus::Filtered);
        let code = &attempts[0].failure.as_ref().expect("a reason").code;
        assert!(
            code == "runtime_cannot_contain_build" || code == "runtime_has_no_toolchain_root",
            "{code}"
        );
        return;
    }

    let FormationResult::Formed {
        verified_routes,
        attempts,
        ..
    } = &result
    else {
        panic!("expected formed, got {result:?}");
    };
    let realization = attempts[0]
        .realization
        .as_ref()
        .expect("the candidate's realization was recorded");
    assert_eq!(realization.executor, "runtime-process");
    assert!(realization.destroyed);
    assert!(
        !realization.endpoints["app.http"].ends_with("host 8000"),
        "the guest port is not assumed to be the host port: {realization:?}"
    );

    // The kept artifact is the build output, not the realization's copy.
    let reference = &verified_routes[0].materialization_ref;
    let artifact = scratch
        .path()
        .join("out/artifacts")
        .join(format!("{}.tar", &reference["sha256:".len()..]));
    let entries = tar_entries(&artifact);
    assert!(
        entries.iter().any(|entry| entry.ends_with("server.py")),
        "{entries:?}"
    );
    assert!(
        !entries
            .iter()
            .any(|entry| entry.contains("runtime-created")),
        "a verification side effect reached the artifact: {entries:?}"
    );
    // And the realization's scratch is gone.
    let realizations: Vec<_> = walk(&scratch.path().join("work"))
        .into_iter()
        .filter(|path| path.ends_with("realization"))
        .collect();
    assert!(realizations.is_empty(), "{realizations:?}");
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        found.push(path.clone());
        if path.is_dir() && !path.is_symlink() {
            found.extend(walk(&path));
        }
    }
    found
}
