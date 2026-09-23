//! Node as a generic process, formed end to end on this Runtime: the declared
//! Node builds AND serves, from `/opt/ato/toolchains`, never the host's.
//!
//! Each server answers `/health` 200 only when it is itself running on the
//! declared Node (`process.version` and `process.execPath`), so a typed K
//! PASS is the runtime evidence; the build output records the same for the
//! build.
//!
//! Needs bwrap and a toolchain root. Provisioning a toolchain that is not
//! already on the host downloads it, so these run only when the toolchain is
//! present or `ATO_TEST_NETWORK=1` says the network may be used; otherwise
//! each test says why it skipped. One realization per test, and the tests of
//! this binary run one at a time, so no two candidates race for a port.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ato_formation::request::{
    ContractSource, FormationNetworkPolicy, FormationPolicy, FormationRequest, FormationResult,
    InitialCondition, RuntimeConstraint, SearchBudget,
};
use ato_formation::source::SourceLimits;
use ato_formation_worker::local::{self, LocalFormation};
use ato_formation_worker::sandbox::{BuildLimits, TOOLCHAIN_ROOT, containment_available};

static SERIAL: Mutex<()> = Mutex::new(());

const NODE: &str = "22.14.0";

fn site(files: &[(&str, String)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, contents) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }
    dir
}

/// Whether this host can run the test, and why not.
fn runnable(toolchains: &[&str]) -> bool {
    if !containment_available() || !Path::new(TOOLCHAIN_ROOT).is_dir() {
        eprintln!("skipping: no bwrap or no {TOOLCHAIN_ROOT}");
        return false;
    }
    let present = toolchains.iter().all(|toolchain| {
        Path::new(TOOLCHAIN_ROOT)
            .join(toolchain)
            .join("bin")
            .is_dir()
    });
    if !present && std::env::var("ATO_TEST_NETWORK").as_deref() != Ok("1") {
        eprintln!("skipping: {toolchains:?} not provisioned and ATO_TEST_NETWORK is not 1");
        return false;
    }
    true
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
            // For the platform's own provisioning only: no authored step
            // below declares a network, so none of them gets one.
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

fn route(steps: &str, serve_argv: &str, extra_runtime: &str) -> String {
    format!(
        r#"
schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[runtime]]
name = "node"
version = "{NODE}"
{extra_runtime}
{steps}
[[derive.step]]
id = "app"
use = "ato.process@1"
op = "serve"
argv = {serve_argv}

[[port]]
id = "app.http"
use = "ato.http@1"
from = "app"
guest_port = 3000

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
"#
    )
}

/// Records which Node ran the build, what it could see, and a secret
/// canary from the worker's environment.
const BUILD: &str = r#"
const fs = require("fs");
fs.mkdirSync("out", { recursive: true });
let home = null;
try { home = fs.readdirSync(process.argv[2] || "/nonexistent").length; } catch (e) { home = e.code; }
fs.writeFileSync("out/build.json", JSON.stringify({
  version: process.version,
  execPath: process.execPath,
  env: Object.keys(process.env).sort(),
  userAgent: process.env.npm_config_user_agent || null,
  hostHome: home,
}));
"#;

/// Answers /health only when it runs on the declared Node.
const SERVER: &str = r#"
const http = require("http");
const fs = require("fs");
const want = "v" + process.env.EXPECT_NODE;
const ok = process.version === want
  && process.execPath.startsWith("/opt/ato/toolchains/node/" + process.env.EXPECT_NODE + "/")
  && fs.existsSync("/app/out/build.json");
try { fs.writeFileSync("/app/runtime-created.txt", "x"); } catch (e) {}
http.createServer((req, res) => {
  if (req.url === "/health") {
    res.writeHead(ok ? 200 : 500);
    res.end(JSON.stringify({ version: process.version, execPath: process.execPath }));
  } else { res.writeHead(404); res.end(); }
}).listen(Number(process.env.ATO_ENDPOINT_APP_HTTP_PORT), "127.0.0.1");
"#;

fn artifact_file(scratch: &Path, result: &FormationResult, name: &str) -> String {
    let FormationResult::Formed {
        verified_routes, ..
    } = result
    else {
        panic!("expected formed, got {result:?}");
    };
    let reference = &verified_routes[0].materialization_ref;
    let path = scratch
        .join("out/artifacts")
        .join(format!("{}.tar", &reference["sha256:".len()..]));
    let mut archive = tar::Archive::new(std::fs::File::open(path).expect("artifact"));
    let mut found = None;
    let mut names = Vec::new();
    for entry in archive.entries().expect("entries") {
        let mut entry = entry.expect("entry");
        let path = entry.path().expect("path").to_string_lossy().into_owned();
        if path == name {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut entry, &mut text).expect("read");
            found = Some(text);
        }
        names.push(path);
    }
    assert!(
        !names.iter().any(|entry| entry.contains("runtime-created")),
        "a realization side effect reached the artifact"
    );
    found.unwrap_or_else(|| panic!("{name} is not in the artifact: {names:?}"))
}

fn assert_built_on_declared_node(build: &str) {
    let build: serde_json::Value = serde_json::from_str(build).expect("build.json");
    assert_eq!(build["version"], format!("v{NODE}"), "{build}");
    assert!(
        build["execPath"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{TOOLCHAIN_ROOT}/node/{NODE}/")),
        "{build}"
    );
    // The worker's environment is not the build's.
    let env: Vec<&str> = build["env"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(!env.contains(&"ATO_TEST_WORKER_SECRET"), "{env:?}");
    // Nor is the worker's home: the sandbox shows an empty tmpfs or nothing.
    assert_ne!(build["hostHome"], serde_json::json!(null));
    if let Some(entries) = build["hostHome"].as_u64() {
        assert_eq!(entries, 0, "host home visible: {build}");
    }
}

fn formed(result: &FormationResult) {
    let FormationResult::Formed { attempts, .. } = result else {
        panic!("expected formed, got {result:?}");
    };
    let verification = attempts[0].verification.as_ref().expect("verdicts");
    assert!(verification.fully_satisfied(), "{verification:?}");
}

fn set_worker_secret() {
    // A variable the worker has and a build must never see.
    // SAFETY: tests of this binary hold SERIAL; nothing reads the env
    // concurrently.
    unsafe { std::env::set_var("ATO_TEST_WORKER_SECRET", "canary") };
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned())
}

#[test]
fn a_node_process_is_built_and_served_by_the_declared_node() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    if !runnable(&[&format!("node/{NODE}")]) {
        return;
    }
    set_worker_secret();
    let dir = site(&[
        (
            "capsule.toml",
            route(
                &format!(
                    "\n[[derive.step]]\nid = \"build\"\nuse = \"ato.process@1\"\nop = \"exec\"\nargv = [\"node\", \"build.js\", {:?}]\n",
                    home()
                ),
                r#"["node", "server.js"]

[derive.step.env]
EXPECT_NODE = "22.14.0""#,
                "",
            ),
        ),
        ("package.json", r#"{"name":"n","private":true}"#.to_owned()),
        ("build.js", BUILD.to_owned()),
        ("server.js", SERVER.to_owned()),
    ]);
    let scratch = tempfile::tempdir().expect("tempdir");
    let result = form(dir.path(), scratch.path());
    formed(&result);
    assert_built_on_declared_node(&artifact_file(scratch.path(), &result, "out/build.json"));
}

#[test]
fn npm_builds_and_starts_on_the_declared_node() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    if !runnable(&[&format!("node/{NODE}")]) {
        return;
    }
    set_worker_secret();
    let dir = site(&[
        (
            "capsule.toml",
            route(
                "\n[[derive.step]]\nid = \"build\"\nuse = \"ato.process@1\"\nop = \"exec\"\nargv = [\"sh\", \"-c\", \"npm run build\"]\n",
                r#"["npm", "start"]

[derive.step.env]
EXPECT_NODE = "22.14.0""#,
                "",
            ),
        ),
        (
            "package.json",
            format!(
                r#"{{"name":"n","private":true,"scripts":{{"prebuild":"node -e \"require('fs').writeFileSync('prebuild.txt', process.version)\"","build":"node build.js {}","start":"node server.js"}}}}"#,
                home()
            ),
        ),
        ("build.js", BUILD.to_owned()),
        ("server.js", SERVER.to_owned()),
    ]);
    let scratch = tempfile::tempdir().expect("tempdir");
    let result = form(dir.path(), scratch.path());
    formed(&result);
    let build = artifact_file(scratch.path(), &result, "out/build.json");
    assert_built_on_declared_node(&build);
    // Run through the declared distribution's npm, from a shell.
    let build: serde_json::Value = serde_json::from_str(&build).unwrap();
    let agent = build["userAgent"].as_str().expect("npm ran the script");
    assert!(agent.contains(&format!("node/v{NODE}")), "{agent}");
    // The package's lifecycle script ran too, contained, on the same Node.
    assert_eq!(
        artifact_file(scratch.path(), &result, "prebuild.txt"),
        format!("v{NODE}")
    );
}

/// pnpm / yarn: the exact pinned version, provisioned into the shared
/// toolchain root, used by the build AND by the Run.
fn package_manager_route(manager: &str, version: &str) {
    let dir = site(&[
        (
            "capsule.toml",
            route(
                &format!(
                    "\n[[derive.step]]\nid = \"build\"\nuse = \"ato.process@1\"\nop = \"exec\"\nargv = [\"{manager}\", \"run\", \"build\"]\n"
                ),
                &format!(
                    r#"["{manager}", "run", "start"]

[derive.step.env]
EXPECT_NODE = "22.14.0""#
                ),
                "",
            ),
        ),
        (
            "package.json",
            format!(
                r#"{{"name":"n","private":true,"packageManager":"{manager}@{version}","scripts":{{"build":"node build.js {}","start":"node server.js"}}}}"#,
                home()
            ),
        ),
        ("build.js", BUILD.to_owned()),
        ("server.js", SERVER.to_owned()),
    ]);
    let scratch = tempfile::tempdir().expect("tempdir");
    let result = form(dir.path(), scratch.path());
    formed(&result);
    let build = artifact_file(scratch.path(), &result, "out/build.json");
    assert_built_on_declared_node(&build);
    let build: serde_json::Value = serde_json::from_str(&build).unwrap();
    let agent = build["userAgent"]
        .as_str()
        .expect("the manager ran the script");
    assert!(agent.contains(&format!("{manager}/{version}")), "{agent}");
    assert!(
        Path::new(TOOLCHAIN_ROOT)
            .join(manager)
            .join(version)
            .join("bin")
            .join(manager)
            .exists()
    );
}

#[test]
fn an_exact_pnpm_pin_builds_and_serves() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    if !runnable(&[&format!("node/{NODE}"), "pnpm/9.15.4"]) {
        return;
    }
    set_worker_secret();
    package_manager_route("pnpm", "9.15.4");
}

#[test]
fn an_exact_yarn_pin_builds_and_serves() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    if !runnable(&[&format!("node/{NODE}"), "yarn/1.22.22"]) {
        return;
    }
    set_worker_secret();
    package_manager_route("yarn", "1.22.22");
}
