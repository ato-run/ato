//! A process route is language-independent: its runtimes and package manager
//! come from the Derivation's `[[runtime]]` and the source's own
//! `packageManager`, never from its argv. Offline: nothing here provisions or
//! runs anything; it checks what gets planned.

use std::collections::BTreeMap;

use ato_formation::capsule_toml::parse_capsule_toml;
use ato_formation::detect::detect;
use ato_formation::failure::FormationFailure;
use ato_formation::intent::{Lane, ResolvedPackageManager};
use ato_formation::source::{RESOLVER_CONTRACT_V1, SourceClosureRef};
use ato_formation_worker::job::{PlannedCandidate, plan_candidate};
use ato_formation_worker::runtime_network::derivation_requirements;

fn tree(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, contents) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }
    dir
}

fn plan_on(route: &str, files: &[(&str, &str)], triple: &str) -> anyhow::Result<PlannedCandidate> {
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
        triple,
    )
}

fn plan(route: &str, files: &[(&str, &str)]) -> anyhow::Result<PlannedCandidate> {
    plan_on(route, files, "x86_64-linux-gnu")
}

fn code(error: &anyhow::Error) -> String {
    error
        .downcast_ref::<FormationFailure>()
        .map(|failure| failure.code.clone())
        .unwrap_or_else(|| format!("untyped: {error:#}"))
}

fn names(planned: &PlannedCandidate) -> Vec<&str> {
    planned.plan.steps.iter().map(|s| s.name.as_str()).collect()
}

const HEAD: &str = r#"
schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."
"#;

fn runtime(name: &str, version: &str) -> String {
    format!("\n[[runtime]]\nname = {name:?}\nversion = {version:?}\n")
}

fn exec(id: &str, argv: &[&str], network: bool) -> String {
    format!(
        "\n[[derive.step]]\nid = {id:?}\nuse = \"ato.process@1\"\nop = \"exec\"\nargv = {argv:?}\n{}",
        if network {
            "network = \"dependency-resolution\"\n"
        } else {
            ""
        }
    )
}

fn serve(argv: &[&str]) -> String {
    format!(
        r#"
[[derive.step]]
id = "app"
use = "ato.process@1"
op = "serve"
argv = {argv:?}

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
"#
    )
}

const NODE_SOURCE: &[(&str, &str)] = &[
    ("package.json", r#"{"name":"n","private":true}"#),
    ("server.js", "require('http')"),
    ("build.js", "1"),
];

const NODE_HOME: &str = "/opt/ato/toolchains/node/22.14.0";

#[test]
fn a_node_route_is_a_generic_process_with_its_declared_node() {
    let route = format!(
        "{HEAD}{}{}{}",
        runtime("node", "22.14.0"),
        exec("build", &["node", "build.js"], false),
        serve(&["node", "server.js"])
    );
    let planned = plan(&route, NODE_SOURCE).expect("plans");
    assert_eq!(planned.intent.lane, Lane::Process);
    assert_eq!(
        planned.intent.runtime,
        BTreeMap::from([("node".to_owned(), "22.14.0".to_owned())])
    );
    assert_eq!(names(&planned), ["provision-node", "build"]);
    assert!(planned.plan.steps[0].argv[2].contains("node-v22.14.0-linux-x64"));
    assert_eq!(planned.plan.toolchain_path, [format!("{NODE_HOME}/bin")]);
    // The Run finds the same Node first, and nothing of the host's.
    assert_eq!(
        planned.intent.public_env.get("PATH").map(String::as_str),
        Some(&*format!("{NODE_HOME}/bin:/usr/local/bin:/usr/bin:/bin"))
    );
    assert_eq!(planned.intent.launch_argv, ["node", "server.js"]);

    // It needs a process Runtime, and provisions the declared toolchain.
    let (requirements, provisions) = derivation_requirements(&planned);
    assert!(
        requirements
            .iter()
            .any(|requirement| requirement.fact == "runtime.process")
    );
    assert!(
        !requirements
            .iter()
            .any(|r| r.fact.starts_with("runtime.node"))
    );
    assert_eq!(provisions, ["toolchain.node.22.14.0"]);
}

#[test]
fn an_authored_path_is_kept_as_written() {
    let route = format!(
        "{HEAD}{}{}\n[derive.step.env]\nPATH = \"/app/bin:/usr/bin\"\n",
        runtime("node", "22.14.0"),
        serve(&["node", "server.js"])
    );
    let planned = plan(&route, NODE_SOURCE).expect("plans");
    assert_eq!(
        planned.intent.public_env.get("PATH").map(String::as_str),
        Some("/app/bin:/usr/bin")
    );
}

#[test]
fn a_node_argv_without_a_declared_node_gets_no_node() {
    // No [[runtime]]: the v1 Python lane, exactly as before this change. The
    // argv's `node` is not read as a declaration.
    let route = format!("{HEAD}{}", serve(&["node", "server.js"]));
    let planned = plan(&route, &[("server.py", "x"), ("server.js", "x")]).expect("plans");
    assert_eq!(planned.intent.lane, Lane::PythonProcess);
    assert!(!planned.intent.runtime.contains_key("node"));
    assert!(!names(&planned).contains(&"provision-node"));
}

#[test]
fn an_unsupported_node_is_refused_by_name() {
    let route = format!(
        "{HEAD}{}{}",
        runtime("node", "18.20.4"),
        serve(&["node", "server.js"])
    );
    let error = plan(&route, NODE_SOURCE).err().expect("refused");
    assert_eq!(code(&error), "intent_unsupported_runtime", "{error:#}");
}

// ── package managers ────────────────────────────────────────────────────────

fn with_package_manager(declared: &str, lockfile: Option<&str>) -> Vec<(&'static str, String)> {
    let mut files = vec![
        (
            "package.json",
            format!(r#"{{"name":"n","private":true{declared}}}"#),
        ),
        ("server.js", "x".to_owned()),
    ];
    if let Some(lock) = lockfile {
        files.push((
            if lock == "pnpm" {
                "pnpm-lock.yaml"
            } else {
                "yarn.lock"
            },
            "lock".to_owned(),
        ));
    }
    files
}

fn plan_pm(
    declared: &str,
    lockfile: Option<&str>,
    extra: &str,
) -> anyhow::Result<PlannedCandidate> {
    let files = with_package_manager(declared, lockfile);
    let files: Vec<(&str, &str)> = files.iter().map(|(n, c)| (*n, c.as_str())).collect();
    let route = format!(
        "{HEAD}{}{extra}{}{}",
        runtime("node", "22.14.0"),
        exec("install", &["pnpm", "install"], true),
        serve(&["pnpm", "start"])
    );
    plan(&route, &files)
}

#[test]
fn an_exact_pnpm_pin_is_provisioned_and_heads_the_path() {
    let planned = plan_pm(
        r#","packageManager":"pnpm@10.4.1+sha512.c753b6c3ad7afa13af388fa6d808035a008e30ea9993f58c6663e2bc5ff21679aa834db094987129aa4d488b86df57f7b634981b2f827cdcacc698cc0cfb88af""#,
        Some("pnpm"),
        "",
    )
    .expect("plans");
    assert_eq!(
        planned.intent.package_manager,
        Some(ResolvedPackageManager {
            name: "pnpm".to_owned(),
            version: "10.4.1".to_owned()
        })
    );
    assert_eq!(
        names(&planned),
        ["provision-node", "provision-pnpm", "install"]
    );
    let provision = &planned.plan.steps[1].argv[2];
    assert!(provision.contains("pnpm@10.4.1"), "{provision}");
    assert!(
        provision.contains("/opt/ato/toolchains/pnpm/10.4.1"),
        "{provision}"
    );
    // The provisioned pnpm, then the declared Node, then the system.
    assert_eq!(
        planned.plan.toolchain_path,
        [
            "/opt/ato/toolchains/pnpm/10.4.1/bin",
            &*format!("{NODE_HOME}/bin")
        ]
    );
    assert!(planned.intent.public_env["PATH"].starts_with("/opt/ato/toolchains/pnpm/10.4.1/bin:"));
    let (_, provisions) = derivation_requirements(&planned);
    assert_eq!(
        provisions,
        ["toolchain.node.22.14.0", "toolchain.pnpm.10.4.1"]
    );
}

#[test]
fn yarn_1_and_yarn_berry_are_provisioned_from_their_own_packages() {
    let classic = plan_pm(r#","packageManager":"yarn@1.22.22""#, Some("yarn"), "").expect("plans");
    assert!(classic.plan.steps[1].argv[2].contains(" yarn@1.22.22"));
    let berry = plan_pm(r#","packageManager":"yarn@4.5.3""#, Some("yarn"), "").expect("plans");
    assert!(berry.plan.steps[1].argv[2].contains("@yarnpkg/cli-dist@4.5.3"));
    assert_eq!(berry.plan.steps[1].name, "provision-yarn");
}

#[test]
fn a_package_manager_without_an_exact_version_is_refused() {
    for (declared, lock) in [
        ("", Some("pnpm")),                               // lockfile, no packageManager
        (r#","packageManager":"pnpm@^9""#, Some("pnpm")), // a range
        (r#","packageManager":"pnpm""#, None),            // no version at all
        ("", Some("yarn")),
    ] {
        let error = plan_pm(declared, lock, "").err().expect("refused");
        assert_eq!(
            code(&error),
            "package_manager_version_unresolved",
            "{declared} {lock:?}: {error:#}"
        );
    }
}

#[test]
fn a_derivation_may_pin_the_package_manager_the_source_does_not() {
    let planned = plan_pm("", Some("pnpm"), &runtime("pnpm", "9.15.4")).expect("plans");
    assert_eq!(planned.intent.package_manager.unwrap().version, "9.15.4");
    // …but not contradict the one the source pins.
    let error = plan_pm(
        r#","packageManager":"pnpm@10.4.1""#,
        Some("pnpm"),
        &runtime("pnpm", "9.15.4"),
    )
    .err()
    .expect("refused");
    assert_eq!(code(&error), "intent_malformed", "{error:#}");
}

#[test]
fn npm_is_the_one_the_declared_node_ships() {
    let planned = plan_pm(r#","packageManager":"npm@10.9.2""#, None, "").expect("plans");
    assert_eq!(planned.intent.package_manager, None);
    assert_eq!(names(&planned), ["provision-node", "install"]);
}

#[test]
fn the_resolved_manager_is_part_of_the_plan_and_not_of_the_derivation() {
    let a = plan_pm(r#","packageManager":"pnpm@10.4.1""#, Some("pnpm"), "").expect("plans");
    let a_again = plan_pm(r#","packageManager":"pnpm@10.4.1""#, Some("pnpm"), "").expect("plans");
    let b = plan_pm(r#","packageManager":"pnpm@10.5.0""#, Some("pnpm"), "").expect("plans");
    assert_eq!(a.plan_digest, a_again.plan_digest);
    assert_ne!(a.plan_digest, b.plan_digest);
    // The source differs (it is I); the route does not.
    assert_eq!(a.derivation_ref, b.derivation_ref);
}

// ── not a Node-only model ───────────────────────────────────────────────────

#[test]
fn a_python_and_node_route_provisions_both_and_serves_python() {
    // Open WebUI's shape: Node builds the frontend, Python serves.
    let route = format!(
        "{HEAD}{}{}{}{}",
        runtime("python", "3.12.7"),
        runtime("node", "22.14.0"),
        exec("frontend", &["npm", "run", "build"], false),
        serve(&["python3", "-m", "backend"])
    );
    let planned = plan(
        &route,
        &[
            ("package.json", r#"{"name":"w","private":true}"#),
            ("package-lock.json", "{}"),
            ("backend/__init__.py", ""),
        ],
    )
    .expect("plans");
    assert_eq!(planned.intent.lane, Lane::Process);
    assert_eq!(
        names(&planned),
        ["provision-python", "provision-node", "frontend"]
    );
    assert_eq!(
        planned.plan.toolchain_path,
        [
            format!("{NODE_HOME}/bin"),
            "/opt/ato/toolchains/python/3.12.7/bin".to_owned()
        ]
    );
    assert_eq!(planned.intent.launch_argv, ["python3", "-m", "backend"]);
    let (_, provisions) = derivation_requirements(&planned);
    assert_eq!(
        provisions,
        ["toolchain.node.22.14.0", "toolchain.python.3.12.7"]
    );
}

#[test]
fn a_static_route_builds_on_the_node_it_declares() {
    // excalidraw's shape: Node is a build toolchain for an authored build.
    let route = format!(
        r#"{HEAD}{}{}
[[derive.step]]
id = "site"
use = "ato.browser@1"
op = "serve"
source = "workspace"
root = "dist"

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
"#,
        runtime("node", "20.20.2"),
        exec("build", &["npm", "run", "build"], false),
    );
    let planned = plan(
        &route,
        &[
            (
                "package.json",
                r#"{"name":"s","private":true,"scripts":{"build":"vite build"}}"#,
            ),
            ("package-lock.json", "{}"),
        ],
    )
    .expect("plans");
    assert_eq!(planned.intent.lane, Lane::StaticWeb);
    assert_eq!(planned.intent.static_build, None);
    assert_eq!(names(&planned), ["provision-node", "build"]);
    assert_eq!(
        planned.plan.toolchain_path,
        ["/opt/ato/toolchains/node/20.20.2/bin"]
    );
}

#[test]
fn the_same_route_plans_its_own_node_per_target_and_keeps_one_derivation() {
    let route = format!(
        "{HEAD}{}{}",
        runtime("node", "22.14.0"),
        serve(&["node", "server.js"])
    );
    let x86 = plan_on(&route, NODE_SOURCE, "x86_64-linux-gnu").expect("plans");
    let arm = plan_on(&route, NODE_SOURCE, "aarch64-linux-gnu").expect("plans");
    assert!(arm.plan.steps[0].argv[2].contains("node-v22.14.0-linux-arm64"));
    assert_ne!(x86.plan_digest, arm.plan_digest);
    assert_eq!(x86.derivation_ref, arm.derivation_ref);
}

#[test]
fn only_the_platforms_provisioning_steps_may_write_the_shared_toolchains() {
    use ato_formation::intent::ToolchainAccess;
    let planned = plan_pm(
        r#","packageManager":"pnpm@10.4.1+sha512.c753b6c3ad7afa13af388fa6d808035a008e30ea9993f58c6663e2bc5ff21679aa834db094987129aa4d488b86df57f7b634981b2f827cdcacc698cc0cfb88af""#,
        Some("pnpm"),
        // An authored step that borrows a platform step's name gets no
        // privilege from it.
        &exec("provision-node", &["node", "-e", "1"], false),
    )
    .expect("plans");
    let access: Vec<(&str, ToolchainAccess)> = planned
        .plan
        .steps
        .iter()
        .map(|step| (step.name.as_str(), step.toolchain_access))
        .collect();
    assert_eq!(
        access,
        [
            ("provision-node", ToolchainAccess::Provision),
            ("provision-pnpm", ToolchainAccess::Provision),
            ("provision-node", ToolchainAccess::ReadOnly),
            ("install", ToolchainAccess::ReadOnly),
        ]
    );
}
