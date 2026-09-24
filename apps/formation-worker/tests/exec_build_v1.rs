//! Authored `exec` steps, executed: env for the workload only, a cwd that is
//! resolved on the real workspace right before its step, and a declared
//! network that is enforced — the narrower of step and policy — with a step
//! that needs more than the policy allows refused before anything runs.
//!
//! Everything here runs under bwrap; on a host without it each test says so
//! and returns. Nothing reaches the internet: the network cases dial a
//! listener this test opens on the host's loopback.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use ato_formation::capsule_toml::parse_capsule_toml;
use ato_formation::detect::detect;
use ato_formation::failure::FormationFailure;
use ato_formation::intent::{
    BuildStepV1, EFFECTIVE_BUILD_PLAN_V1_SCHEMA, EffectiveBuildPlanV1, Lane,
};
use ato_formation::source::{RESOLVER_CONTRACT_V1, SourceClosureRef};
use ato_formation_worker::build::{BuildAttempt, run_build};
use ato_formation_worker::job::plan_candidate;
use ato_formation_worker::sandbox::{
    BuildLimits, BuildSandbox, NetworkPolicy, containment_available, sandboxed_build_step_command,
};

fn step(name: &str, script: &str) -> BuildStepV1 {
    BuildStepV1 {
        name: name.to_owned(),
        argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()],
        needs_network: false,
        cwd_relative: String::new(),
        env: BTreeMap::new(),
        toolchain_access: ato_formation::intent::ToolchainAccess::ReadOnly,
    }
}

fn plan_of(steps: Vec<BuildStepV1>) -> EffectiveBuildPlanV1 {
    EffectiveBuildPlanV1 {
        schema: EFFECTIVE_BUILD_PLAN_V1_SCHEMA.to_owned(),
        lane: Lane::PythonProcess,
        workspace_guest_root: "/app".to_owned(),
        runtime: BTreeMap::new(),
        steps,
        output_root: String::new(),
    }
}

struct Scratch {
    _dir: tempfile::TempDir,
    source: PathBuf,
    workspace: PathBuf,
    policy: PathBuf,
}

fn scratch() -> Scratch {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    Scratch {
        policy: dir.path().join("policy.json"),
        source,
        workspace,
        _dir: dir,
    }
}

fn build(at: &Scratch, plan: &EffectiveBuildPlanV1, network: NetworkPolicy) -> anyhow::Result<()> {
    run_build(
        plan,
        BuildAttempt {
            job_id: "job".to_owned(),
            attempt_id: "attempt".to_owned(),
            attempt_fence: 1,
        },
        &BuildSandbox {
            source_root: &at.source,
            workspace_root: &at.workspace,
            cache_root: None,
            shim: Path::new(env!("CARGO_BIN_EXE_ato-formation-worker")),
            policy_host_path: &at.policy,
            network,
            limits: BuildLimits::default(),
            toolchain: ato_formation_worker::sandbox::ToolchainAccess::ReadOnly,
        },
    )
    .map(|_| ())
}

fn read(at: &Scratch, name: &str) -> String {
    std::fs::read_to_string(at.workspace.join(name)).unwrap_or_else(|_| "<absent>".to_owned())
}

fn contained() -> bool {
    if containment_available() {
        return true;
    }
    eprintln!("skipping: bwrap is unavailable");
    false
}

// ── env ─────────────────────────────────────────────────────────────────────

#[test]
fn authored_env_is_given_to_the_workload_and_never_to_the_sandbox() {
    if !contained() {
        return;
    }
    let at = scratch();
    let env = BTreeMap::from([
        ("EXPECTED".to_owned(), "x y".to_owned()),
        ("PATH".to_owned(), "/usr/bin:/bin:/authored".to_owned()),
        ("LD_PRELOAD".to_owned(), String::new()),
    ]);

    // Structurally: bubblewrap sets only its own fixed environment; the
    // authored variables are handed to the shim after `sandbox-exec`, which
    // applies them to the workload's exec and to nothing earlier.
    let command = sandboxed_build_step_command(
        &["/bin/true".to_owned()],
        None,
        &env,
        &BuildSandbox {
            source_root: &at.source,
            workspace_root: &at.workspace,
            cache_root: None,
            shim: Path::new(env!("CARGO_BIN_EXE_ato-formation-worker")),
            policy_host_path: &at.policy,
            network: NetworkPolicy::Denied,
            limits: BuildLimits::default(),
            toolchain: ato_formation_worker::sandbox::ToolchainAccess::ReadOnly,
        },
    )
    .expect("a command");
    let shim_at = command
        .argv
        .iter()
        .position(|arg| arg == "sandbox-exec")
        .expect("the shim");
    for (index, arg) in command.argv.iter().enumerate() {
        if arg == "--setenv" {
            let name = &command.argv[index + 1];
            assert!(
                !env.contains_key(name) || name == "PATH",
                "authored {name} reached bubblewrap"
            );
            if name == "PATH" {
                assert_eq!(command.argv[index + 2], "/usr/local/bin:/usr/bin:/bin");
            }
        }
        if env.keys().any(|name| arg.starts_with(&format!("{name}="))) {
            assert!(index > shim_at, "{arg} precedes the shim");
            assert_eq!(command.argv[index - 1], "--env");
        }
    }

    // And in effect: the step sees exactly what was authored for it; the
    // next step, which authored nothing, sees none of it.
    let mut first = step(
        "first",
        "printf '%s|%s|%s' \"$EXPECTED\" \"$PATH\" \"${LD_PRELOAD-unset}\" > first.txt",
    );
    first.env = env;
    let second = step(
        "second",
        "printf '%s|%s' \"${EXPECTED-unset}\" \"$PATH\" > second.txt",
    );
    build(&at, &plan_of(vec![first, second]), NetworkPolicy::Denied).expect("builds");
    assert_eq!(read(&at, "first.txt"), "x y|/usr/bin:/bin:/authored|");
    assert_eq!(
        read(&at, "second.txt"),
        "unset|/usr/local/bin:/usr/bin:/bin"
    );
}

// ── cwd ─────────────────────────────────────────────────────────────────────

#[test]
fn a_cwd_an_earlier_step_created_is_where_the_step_runs() {
    if !contained() {
        return;
    }
    let at = scratch();
    let make = step("make", "mkdir -p made/sub && ln -s made/sub via-link");
    let mut in_sub = step("in-sub", "pwd > /app/in-sub.txt");
    in_sub.cwd_relative = "made/sub".to_owned();
    let mut via_link = step("via-link", "pwd > /app/via-link.txt");
    via_link.cwd_relative = "via-link".to_owned();
    build(
        &at,
        &plan_of(vec![make, in_sub, via_link]),
        NetworkPolicy::Denied,
    )
    .expect("builds");
    assert_eq!(read(&at, "in-sub.txt").trim(), "/app/made/sub");
    // A contained link is followed, and the step runs where it resolved.
    assert_eq!(read(&at, "via-link.txt").trim(), "/app/made/sub");
}

#[test]
fn a_cwd_that_resolves_outside_the_workspace_is_refused_before_its_step_runs() {
    if !contained() {
        return;
    }
    for (link, target) in [("escape", "/etc"), ("climb", "../.."), ("missing", "")] {
        let at = scratch();
        let setup = if target.is_empty() {
            step("setup", ":")
        } else {
            step("setup", &format!("ln -s {target} {link}"))
        };
        let mut inside = step("inside", "touch /app/ran.txt");
        inside.cwd_relative = link.to_owned();
        let error =
            build(&at, &plan_of(vec![setup, inside]), NetworkPolicy::Denied).expect_err(link);
        let failure = error
            .downcast_ref::<FormationFailure>()
            .unwrap_or_else(|| panic!("{link}: untyped {error:#}"));
        assert_eq!(failure.code, "build_cwd_outside_workspace", "{link}");
        assert!(
            !failure.message.contains(&*at.workspace.to_string_lossy()),
            "{link}: host path in {}",
            failure.message
        );
        assert_eq!(read(&at, "ran.txt"), "<absent>", "{link}: the step ran");
    }
}

// ── network ─────────────────────────────────────────────────────────────────

/// A listener on the host's loopback, and a step that dials it.
///
/// The probe's stderr goes to a workspace file, not `/dev/null`: `/dev` is
/// read-only under the build's Landlock policy, so a redirect there fails
/// and would make every dial look unreachable.
fn listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
    let port = listener.local_addr().unwrap().port();
    let script = format!(
        "if /bin/bash -c 'exec 3<>/dev/tcp/127.0.0.1/{port}' 2>dial.err; \
         then echo reached > dial.txt; else echo unreachable > dial.txt; fi"
    );
    (listener, script)
}

/// The step's network, stated in a Derivation and planned from it.
fn planned_dial(network: Option<&str>, script: &str) -> EffectiveBuildPlanV1 {
    let network = network
        .map(|value| format!("network = {value:?}\n"))
        .unwrap_or_default();
    let route = format!(
        r#"
schema = "ato.capsule/1"

[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."

[[derive.step]]
id = "marker"
use = "ato.process@1"
op = "exec"
argv = ["/bin/sh", "-c", "echo ran > marker.txt"]

[[derive.step]]
id = "dial"
use = "ato.process@1"
op = "exec"
argv = ["/bin/sh", "-c", {script:?}]
{network}
[[derive.step]]
id = "site"
use = "ato.browser@1"
op = "serve"
source = "workspace"

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
"#
    );
    let draft = parse_capsule_toml(&route).expect("parses");
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "<!doctype html>").unwrap();
    let closure = SourceClosureRef::derive(
        "sha256:1111111111111111111111111111111111111111111111111111111111111111",
        "",
        RESOLVER_CONTRACT_V1,
    )
    .unwrap();
    plan_candidate(
        &draft,
        &closure,
        &detect(dir.path()).unwrap(),
        BTreeMap::new(),
        "/app",
        "x86_64-linux-gnu",
    )
    .expect("plans")
    .plan
}

#[test]
fn a_step_that_declares_no_network_gets_none_even_when_the_policy_allows_it() {
    if !contained() {
        return;
    }
    let (_listener, script) = listener();
    let plan = planned_dial(None, &script);
    assert!(plan.steps.iter().all(|step| !step.needs_network));
    let at = scratch();
    build(&at, &plan, NetworkPolicy::DependencyResolution).expect("builds");
    assert_eq!(read(&at, "dial.txt").trim(), "unreachable");
    // Unreachable because the dial was refused — not because the probe
    // itself could not run.
    assert!(
        read(&at, "dial.err").contains("Connection refused"),
        "{}",
        read(&at, "dial.err")
    );
}

#[test]
fn a_step_that_needs_the_network_under_a_policy_that_denies_it_is_refused_before_anything_runs() {
    if !contained() {
        return;
    }
    let (_listener, script) = listener();
    let plan = planned_dial(Some("dependency-resolution"), &script);
    let at = scratch();
    let error = build(&at, &plan, NetworkPolicy::Denied).expect_err("refused");
    assert!(
        format!("{error:#}").contains("needs the network"),
        "{error:#}"
    );
    // Not even the step before it ran.
    assert_eq!(read(&at, "marker.txt"), "<absent>");
    assert_eq!(read(&at, "dial.txt"), "<absent>");
}

#[test]
fn a_step_that_needs_the_network_gets_it_when_the_policy_allows_it() {
    if !contained() {
        return;
    }
    let (_listener, script) = listener();
    let plan = planned_dial(Some("dependency-resolution"), &script);
    let at = scratch();
    build(&at, &plan, NetworkPolicy::DependencyResolution).expect("builds");
    assert_eq!(read(&at, "marker.txt").trim(), "ran");
    assert_eq!(
        read(&at, "dial.txt").trim(),
        "reached",
        "{}",
        read(&at, "dial.err")
    );
}

// ── host-side writes and shared toolchains ──────────────────────────────────

/// The host rewrites the build policy before every step. A step that plants a
/// link where the policy used to be written must not redirect that write: the
/// policy now lives in a control directory no step can reach.
#[test]
fn a_link_planted_by_one_step_does_not_redirect_the_hosts_next_write() {
    if !contained() {
        return;
    }
    let at = scratch();
    let canary = at._dir.path().join("canary");
    std::fs::write(&canary, "untouched").unwrap();
    let plan = plan_of(vec![
        step(
            "plant",
            &format!(
                "ln -s {} /app/.ato-build-policy.json; ln -s {} /app/build-policy.json",
                canary.display(),
                canary.display()
            ),
        ),
        step("next", "true"),
    ]);
    build(&at, &plan, NetworkPolicy::Denied).expect("both steps run");
    assert_eq!(std::fs::read_to_string(&canary).unwrap(), "untouched");
}

#[test]
fn a_policy_inside_the_workspace_is_refused_before_any_step_runs() {
    let at = scratch();
    let marker = at.workspace.join("ran");
    let plan = plan_of(vec![step("mark", "touch /app/ran")]);
    let error = run_build(
        &plan,
        BuildAttempt {
            job_id: "job".to_owned(),
            attempt_id: "attempt".to_owned(),
            attempt_fence: 1,
        },
        &BuildSandbox {
            source_root: &at.source,
            workspace_root: &at.workspace,
            cache_root: None,
            shim: Path::new(env!("CARGO_BIN_EXE_ato-formation-worker")),
            policy_host_path: &at.workspace.join(".ato-build-policy.json"),
            network: NetworkPolicy::Denied,
            limits: BuildLimits::default(),
            toolchain: ato_formation_worker::sandbox::ToolchainAccess::ReadOnly,
        },
    )
    .unwrap_err();
    assert!(format!("{error}").contains("outside every path"), "{error}");
    assert!(!marker.exists());
}

/// An authored step runs source-controlled code, and the toolchain root is
/// shared by every later build and attempt on this host.
#[test]
fn an_authored_step_cannot_write_the_shared_toolchain_root() {
    use ato_formation_worker::sandbox::TOOLCHAIN_ROOT;
    if !contained() {
        return;
    }
    if !Path::new(TOOLCHAIN_ROOT).is_dir() {
        eprintln!("skipping: {TOOLCHAIN_ROOT} is absent on this host");
        return;
    }
    let at = scratch();
    let probe = format!("{TOOLCHAIN_ROOT}/.ato-authored-write-{}", std::process::id());
    let plan = plan_of(vec![step("write", &format!("touch {probe}"))]);
    let outcome = build(&at, &plan, NetworkPolicy::Denied);
    let written = Path::new(&probe).exists();
    let _ = std::fs::remove_file(&probe);
    assert!(outcome.is_err(), "the write was allowed");
    assert!(!written, "an authored step wrote {probe}");
}
