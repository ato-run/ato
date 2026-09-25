//! Authored `ato.process@1` `exec` steps, projected into the build plan.
//!
//! A route is `exec* serve`. Each exec is D semantics — argv, cwd, env and
//! network are all part of the `DerivationRef` — and lands in the
//! EffectiveBuildPlan exactly as written, after the platform's own
//! prerequisites and INSTEAD of any application build inferred from the
//! source. Nothing about the plan flows back into D.

use std::collections::BTreeMap;

use ato_formation::capsule_toml::parse_capsule_toml;
use ato_formation::detect::detect;
use ato_formation::failure::FormationFailure;
use ato_formation::source::{RESOLVER_CONTRACT_V1, SourceClosureRef};
use ato_formation_worker::job::{PlannedCandidate, plan_candidate};

fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, contents) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }
    dir
}

fn plan(route: &str, files: &[(&str, &str)]) -> anyhow::Result<PlannedCandidate> {
    let dir = tree(files);
    let draft = parse_capsule_toml(route).map_err(|error| anyhow::anyhow!("{error}"))?;
    let closure = SourceClosureRef::derive(
        "sha256:1111111111111111111111111111111111111111111111111111111111111111",
        "",
        RESOLVER_CONTRACT_V1,
    )
    .expect("closure");
    plan_candidate(
        &draft,
        &closure,
        &detect(dir.path()).expect("detects"),
        BTreeMap::new(),
        "/app",
        "x86_64-linux-gnu",
    )
}

fn code(error: &anyhow::Error) -> String {
    error
        .downcast_ref::<FormationFailure>()
        .map(|failure| failure.code.clone())
        .unwrap_or_else(|| format!("untyped: {error:#}"))
}

const HEAD: &str = r#"
schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[runtime]]
name = "python"
version = "3.12.7"
"#;

const INSTALL: &str = r#"
[[derive.step]]
id = "install"
use = "ato.process@1"
op = "exec"
argv = ["/opt/ato/toolchains/python/3.12.7/bin/python3", "-m", "pip", "install", "--target", "deps", "-r", "requirements.txt"]
network = "dependency-resolution"
"#;

const GENERATE: &str = r#"
[[derive.step]]
id = "generate"
use = "ato.process@1"
op = "exec"
argv = ["/opt/ato/toolchains/python/3.12.7/bin/python3", "gen.py", "a b", ""]
cwd = "tools"

[derive.step.env]
EXPECTED = "x y"
"#;

const SERVE: &str = r#"
[[derive.step]]
id = "app"
use = "ato.process@1"
op = "serve"
argv = ["/opt/ato/toolchains/python/3.12.7/bin/python3", "-B", "app.py"]

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
"#;

fn route(steps: &[&str]) -> String {
    let mut text = HEAD.to_owned();
    for step in steps {
        text.push_str(step);
    }
    text
}

/// A Python source with a `requirements.txt` — which, WITHOUT authored
/// steps, the platform installs itself.
const PYTHON_SOURCE: &[(&str, &str)] = &[
    ("requirements.txt", "flask==3.0.0\n"),
    ("app.py", "print('app')\n"),
    ("tools/gen.py", "print('gen')\n"),
];

#[test]
fn exec_steps_run_in_authored_order_after_the_platform_prerequisites() {
    let planned = plan(&route(&[INSTALL, GENERATE, SERVE]), PYTHON_SOURCE).expect("plans");
    let names: Vec<_> = planned
        .plan
        .steps(&planned.derivation)
        .unwrap()
        .iter()
        .map(|s| s.name.clone())
        .collect();
    // The interpreter is a platform prerequisite; the install is the
    // author's. The platform's own `create-site-packages` / `pip-install`
    // are NOT planned beside it: one install of one graph.
    assert_eq!(names, ["provision-python", "install", "generate"]);
    assert!(planned.plan.actions.iter().any(|action| matches!(
        action,
        ato_formation::execution::BuildAction::Authored { .. }
    )));

    let install = &planned.plan.steps(&planned.derivation).unwrap()[1];
    assert!(install.needs_network);
    assert_eq!(install.cwd_relative, "");
    assert!(install.env.is_empty());

    let generate = &planned.plan.steps(&planned.derivation).unwrap()[2];
    // The argv array as written: four elements, the one with a space and the
    // empty one intact. Never joined into a line and split again.
    assert_eq!(
        generate.argv,
        [
            "/opt/ato/toolchains/python/3.12.7/bin/python3",
            "gen.py",
            "a b",
            ""
        ]
    );
    assert_eq!(generate.cwd_relative, "tools");
    assert_eq!(
        generate.env,
        BTreeMap::from([("EXPECTED".to_owned(), "x y".to_owned())])
    );
    assert!(!generate.needs_network);
}

#[test]
fn without_exec_the_platform_still_installs_the_dependencies() {
    let planned = plan(&route(&[SERVE]), PYTHON_SOURCE).expect("plans");
    let names: Vec<_> = planned
        .plan
        .steps(&planned.derivation)
        .unwrap()
        .iter()
        .map(|s| s.name.clone())
        .collect();
    assert_eq!(
        names,
        ["provision-python", "create-site-packages", "pip-install"]
    );
}

const STATIC_BUILD: &str = r#"
schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[derive.step]]
id = "build"
use = "ato.process@1"
op = "exec"
argv = ["/bin/sh", "-c", "mkdir -p dist && echo '<!doctype html>' > dist/index.html"]

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
fn an_authored_static_build_is_the_only_build() {
    // A package.json with a build script and a lockfile: without authored
    // steps this is `provision-node` → `npm ci` → `npm run build`.
    let planned = plan(
        STATIC_BUILD,
        &[
            (
                "package.json",
                r#"{"name":"s","private":true,"scripts":{"build":"vite build"},"devDependencies":{"vite":"5"}}"#,
            ),
            ("package-lock.json", r#"{"lockfileVersion":3}"#),
        ],
    )
    .expect("plans");
    let names: Vec<_> = planned
        .plan
        .steps(&planned.derivation)
        .unwrap()
        .iter()
        .map(|s| s.name.clone())
        .collect();
    assert_eq!(names, ["build"]);
    assert!(!planned.plan.actions.iter().any(|action| matches!(action, ato_formation::execution::BuildAction::Prerequisite(step) if step.name == "static-build")));
    assert_eq!(
        planned
            .plan
            .serving(&planned.derivation)
            .root
            .as_deref()
            .unwrap_or_default(),
        "dist"
    );
}

// ── ordering ────────────────────────────────────────────────────────────────

#[test]
fn an_exec_after_the_serving_step_is_refused() {
    for steps in [[SERVE, GENERATE].as_slice(), &[INSTALL, SERVE, GENERATE]] {
        let error = plan(&route(steps), PYTHON_SOURCE).err().expect("refused");
        assert_eq!(code(&error), "projection_step_order", "{error:#}");
    }
}

#[test]
fn two_serving_steps_are_still_refused_as_such() {
    let second = SERVE
        .replace("id = \"app\"", "id = \"app2\"")
        .replace("id = \"app.http\"", "id = \"app2.http\"")
        .replace("from = \"app\"", "from = \"app2\"");
    let second = &second[..second.find("[[contract.require]]").unwrap()];
    let error = plan(&route(&[SERVE, second]), PYTHON_SOURCE)
        .err()
        .expect("refused");
    assert_eq!(code(&error), "projection_serving_steps", "{error:#}");
}

// ── what cannot be run as written ───────────────────────────────────────────

#[test]
fn a_cwd_outside_the_workspace_is_refused() {
    for cwd in ["/tmp", "/etc", "..", "../x", "a/../../x", "a/../b"] {
        let step = GENERATE.replace("cwd = \"tools\"", &format!("cwd = {cwd:?}"));
        let error = plan(&route(&[&step, SERVE]), PYTHON_SOURCE)
            .err()
            .unwrap_or_else(|| panic!("{cwd} was accepted"));
        assert_eq!(code(&error), "projection_unprojectable", "{cwd}: {error:#}");
    }
}

#[test]
fn an_env_name_no_process_can_receive_is_refused() {
    let step = GENERATE.replace("EXPECTED = \"x y\"", "\"A=B\" = \"x\"");
    let error = plan(&route(&[&step, SERVE]), PYTHON_SOURCE)
        .err()
        .expect("refused");
    assert_eq!(code(&error), "projection_unprojectable", "{error:#}");
}

#[test]
fn only_an_exec_step_declares_a_network() {
    let serve = SERVE.replace(
        "argv = [\"/opt/ato/toolchains/python/3.12.7/bin/python3\", \"-B\", \"app.py\"]",
        "argv = [\"/opt/ato/toolchains/python/3.12.7/bin/python3\", \"-B\", \"app.py\"]\nnetwork = \"denied\"",
    );
    assert!(parse_capsule_toml(&route(&[&serve])).is_err());
    let unknown = INSTALL.replace("dependency-resolution", "everything");
    assert!(parse_capsule_toml(&route(&[&unknown, SERVE])).is_err());
}

// ── D identity ──────────────────────────────────────────────────────────────

#[test]
fn every_part_of_an_exec_step_is_part_of_the_derivation() {
    let base = plan(&route(&[INSTALL, GENERATE, SERVE]), PYTHON_SOURCE)
        .expect("plans")
        .derivation_ref
        .clone();
    let variants = [
        ("argv", GENERATE.replace("\"a b\"", "\"a  b\"")),
        ("cwd", GENERATE.replace("cwd = \"tools\"", "cwd = \"lib\"")),
        ("env", GENERATE.replace("\"x y\"", "\"x z\"")),
        (
            "network",
            GENERATE.replace(
                "cwd = \"tools\"",
                "cwd = \"tools\"\nnetwork = \"dependency-resolution\"",
            ),
        ),
    ];
    for (what, generate) in variants {
        let changed = plan(&route(&[INSTALL, &generate, SERVE]), PYTHON_SOURCE)
            .expect("plans")
            .derivation_ref
            .clone();
        assert_ne!(
            changed, base,
            "a {what} change left the DerivationRef as it was"
        );
    }
    // Order is part of the route too.
    let swapped = plan(&route(&[GENERATE, INSTALL, SERVE]), PYTHON_SOURCE)
        .expect("plans")
        .derivation_ref
        .clone();
    assert_ne!(swapped, base);
}

#[test]
fn a_network_stated_as_the_default_is_the_same_derivation() {
    let stated = GENERATE.replace("cwd = \"tools\"", "cwd = \"tools\"\nnetwork = \"denied\"");
    let implicit = plan(&route(&[GENERATE, SERVE]), PYTHON_SOURCE).expect("plans");
    let explicit = plan(&route(&[&stated, SERVE]), PYTHON_SOURCE).expect("plans");
    assert_eq!(implicit.derivation_ref, explicit.derivation_ref);
}

#[test]
fn the_plan_does_not_flow_back_into_the_derivation() {
    // Same D on two targets: different plans (another interpreter download),
    // one DerivationRef.
    let draft = parse_capsule_toml(&route(&[INSTALL, SERVE])).expect("parses");
    let dir = tree(PYTHON_SOURCE);
    let closure = SourceClosureRef::derive(
        "sha256:1111111111111111111111111111111111111111111111111111111111111111",
        "",
        RESOLVER_CONTRACT_V1,
    )
    .expect("closure");
    let evidence = detect(dir.path()).expect("detects");
    let on = |triple: &str| {
        plan_candidate(&draft, &closure, &evidence, BTreeMap::new(), "/app", triple).expect("plans")
    };
    let (x86, arm) = (on("x86_64-linux-gnu"), on("aarch64-linux-gnu"));
    assert_ne!(x86.plan, arm.plan);
    assert_eq!(x86.derivation_ref, arm.derivation_ref);
}
