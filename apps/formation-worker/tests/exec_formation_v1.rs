//! Authored `exec` steps, formed end to end on this Runtime.
//!
//! Process: exec → exec → serve → typed K → artifact → VerifiedRoute, with
//! the execs' output in the artifact and the realization's side effects not.
//! Static: an exec that writes the site, served from where it wrote it.
//!
//! Both need bwrap and a toolchain root; on a host without them the attempt
//! is Filtered before execution, which is the evidence asserted instead.
//! One process realization in this binary, so no two candidates here ever
//! race for a port.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use ato_formation::request::{
    AttemptStatus, ContractSource, FormationNetworkPolicy, FormationPolicy, FormationRequest,
    FormationResult, InitialCondition, RuntimeConstraint, SearchBudget,
};
use ato_formation::source::SourceLimits;
use ato_formation_worker::local::{self, LocalFormation};
use ato_formation_worker::sandbox::{BuildLimits, TOOLCHAIN_ROOT, containment_available};

fn site(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, contents) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }
    dir
}

fn form(dir: &Path, scratch: &Path) -> FormationResult {
    local::run(
        &FormationRequest {
            initial_condition: InitialCondition::LocalDirectory {
                path: dir.to_path_buf(),
            },
            contract: ContractSource::Infer,
            runtime: RuntimeConstraint::Exact {
                runtime_id: "local".to_owned(),
            },
            // The toolchain download is the platform's; no authored step here
            // declares a network, so none of them gets one.
            policy: FormationPolicy {
                network: FormationNetworkPolicy::DependencyResolution,
            },
            budget: SearchBudget { max_attempts: 1 },
            browser_contract: None,
        },
        &LocalFormation {
            work_root: scratch.join("work"),
            out_dir: scratch.join("out"),
            shim: PathBuf::from(env!("CARGO_BIN_EXE_ato-formation-worker")),
            limits: BuildLimits::default(),
            source_limits: SourceLimits::default(),
            browser_verifier: None,
            browser_budget: Default::default(),
        },
    )
    .expect("the driver ran")
}

/// On a host that cannot contain a build, the candidate is Filtered.
fn filtered_here(result: &FormationResult) -> bool {
    if containment_available() && Path::new(TOOLCHAIN_ROOT).is_dir() {
        return false;
    }
    let FormationResult::NoVerifiedRoute { attempts, .. } = result else {
        panic!("expected no_verified_route on this host, got {result:?}");
    };
    assert_eq!(attempts[0].status, AttemptStatus::Filtered, "{attempts:?}");
    true
}

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
id = "configure"
use = "ato.process@1"
op = "exec"
argv = ["/opt/ato/toolchains/python/3.12.7/bin/python3", "-B", "tools/configure.py"]

[[derive.step]]
id = "render"
use = "ato.process@1"
op = "exec"
argv = ["/opt/ato/toolchains/python/3.12.7/bin/python3", "-B", "/app/tools/render.py", "rendered by exec.txt"]
cwd = "generated"

[derive.step.env]
EXPECTED = "greeting=hello"

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

/// exec #1: runs in the workspace root and creates `generated/`.
const CONFIGURE: &str = r#"
import os
os.makedirs("generated", exist_ok=True)
with open("generated/config.txt", "w") as handle:
    handle.write("greeting=hello")
"#;

/// exec #2: runs in `generated/` (created by exec #1), with an authored env
/// and an argv element carrying a space.
const RENDER: &str = r#"
import os, sys
config = open("config.txt").read()
if os.environ.get("EXPECTED") != config:
    sys.exit("EXPECTED does not match the configuration exec #1 wrote")
if os.getcwd() != "/app/generated":
    sys.exit("running in " + os.getcwd())
with open(sys.argv[1], "w") as handle:
    handle.write(config)
os.makedirs("/app/site", exist_ok=True)
with open("/app/site/health.txt", "w") as handle:
    handle.write("ok " + config)
"#;

/// Serves what the build produced, and tries to leave a mark on the way up.
const SERVER: &str = r#"
import os
from http.server import BaseHTTPRequestHandler, HTTPServer

try:
    with open("/app/runtime-created.txt", "w") as handle:
        handle.write("written by the candidate")
except OSError:
    pass

HEALTH = open("/app/site/health.txt", "rb").read()

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        ok = self.path == "/health"
        self.send_response(200 if ok else 404)
        self.end_headers()
        self.wfile.write(HEALTH if ok else b"")

    def log_message(self, *args):
        pass

HTTPServer(("127.0.0.1", int(os.environ["ATO_ENDPOINT_APP_HTTP_PORT"])), Handler).serve_forever()
"#;

fn tar_entries(path: &Path) -> Vec<String> {
    tar::Archive::new(std::fs::File::open(path).expect("artifact"))
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
fn a_process_route_with_exec_steps_reaches_a_verified_route() {
    let dir = site(&[
        ("capsule.toml", PROCESS_ROUTE),
        ("tools/configure.py", CONFIGURE),
        ("tools/render.py", RENDER),
        ("server.py", SERVER),
    ]);
    let scratch = tempfile::tempdir().expect("tempdir");
    let result = form(dir.path(), scratch.path());
    if filtered_here(&result) {
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
    let verification = attempts[0].verification.as_ref().expect("verdicts");
    assert!(verification.fully_satisfied(), "{verification:?}");

    let reference = &verified_routes[0].materialization_ref;
    let entries = tar_entries(
        &scratch
            .path()
            .join("out/artifacts")
            .join(format!("{}.tar", &reference["sha256:".len()..])),
    );
    for built in [
        "generated/config.txt",
        "generated/rendered by exec.txt",
        "site/health.txt",
    ] {
        assert!(
            entries.iter().any(|entry| entry == built),
            "{built} is not in the artifact: {entries:?}"
        );
    }
    assert!(
        !entries
            .iter()
            .any(|entry| entry.contains("runtime-created")),
        "a realization side effect reached the artifact: {entries:?}"
    );
}

const STATIC_ROUTE: &str = r#"
schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[derive.step]]
id = "build"
use = "ato.process@1"
op = "exec"
argv = ["/bin/sh", "-c", "mkdir -p dist && cp page.html dist/index.html"]

[[derive.step]]
id = "site"
use = "ato.browser@1"
op = "serve"
source = "workspace"
root = "dist"
entry = "index.html"

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

#[test]
fn a_static_site_an_exec_step_builds_satisfies_its_contract() {
    // A package.json the platform would otherwise build with npm: the
    // authored exec is the only build, so no Node is provisioned and no
    // network is needed.
    let dir = site(&[
        ("capsule.toml", STATIC_ROUTE),
        ("page.html", "<!doctype html><h1>built by exec</h1>"),
        (
            "package.json",
            r#"{"name":"s","private":true,"scripts":{"build":"vite build"}}"#,
        ),
        ("package-lock.json", r#"{"lockfileVersion":3}"#),
    ]);
    let scratch = tempfile::tempdir().expect("tempdir");
    let result = form(dir.path(), scratch.path());
    if filtered_here(&result) {
        return;
    }
    let FormationResult::Formed { attempts, .. } = &result else {
        panic!("expected formed, got {result:?}");
    };
    let verification = attempts[0].verification.as_ref().expect("verdicts");
    assert!(verification.fully_satisfied(), "{verification:?}");
}
