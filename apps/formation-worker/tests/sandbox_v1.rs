//! The isolation a Formation build runs under, and the fence that decides
//! whether its result may be published.
//!
//! The build is assumed hostile: `uv sync` runs arbitrary build-backend hooks
//! and a `setup.py` runs at install time, so these tests are about what the
//! sandbox denies rather than what it permits.

use std::path::{Path, PathBuf};

use ato_formation::intent::{
    BuildStepV1, EFFECTIVE_BUILD_PLAN_V1_SCHEMA, EffectiveBuildPlanV1, Lane,
};
use ato_formation_worker::build::{
    BuildAttempt, BuildOutcome, may_publish, output_root, run_build,
};
use ato_formation_worker::sandbox::*;

fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().expect("tempdir");
    let source = root.path().join("source");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&source).expect("mkdir");
    std::fs::create_dir_all(&workspace).expect("mkdir");
    (root, source, workspace)
}

fn command(
    argv: &[&str],
    source: &Path,
    workspace: &Path,
    cache: Option<&Path>,
    network: NetworkPolicy,
) -> anyhow::Result<SandboxedBuildCommand> {
    sandboxed_build_command(
        &argv.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>(),
        &BuildSandbox {
            source_root: source,
            workspace_root: workspace,
            cache_root: cache,
            shim: Path::new("/usr/local/bin/ato-formation-worker"),
            policy_host_path: Path::new("/tmp/policy.json"),
            network,
            limits: BuildLimits::default(),
        },
    )
}

#[test]
fn the_source_is_mounted_read_only_and_the_workspace_is_not() {
    if !containment_available() {
        eprintln!("skipping: bwrap is unavailable, so no build may be contained here");
        return;
    }
    let (_root, source, workspace) = dirs();
    let built = command(&["true"], &source, &workspace, None, NetworkPolicy::Denied)
        .expect("builds a command");
    let argv = built.argv.join(" ");
    // A build that edits its own source produces an artifact whose closure ref
    // no longer describes it.
    assert!(argv.contains(&format!("--ro-bind {} /src", source.display())));
    assert!(argv.contains(&format!("--bind {} /app", workspace.display())));
}

#[test]
fn a_denied_network_is_actually_unshared() {
    if !containment_available() {
        eprintln!("skipping: bwrap is unavailable");
        return;
    }
    let (_root, source, workspace) = dirs();
    let denied =
        command(&["true"], &source, &workspace, None, NetworkPolicy::Denied).expect("command");
    assert!(denied.argv.contains(&"--unshare-all".to_owned()));
    assert!(
        !denied.argv.contains(&"--share-net".to_owned()),
        "a denied network must not be shared"
    );

    let allowed = command(
        &["true"],
        &source,
        &workspace,
        None,
        NetworkPolicy::DependencyResolution,
    )
    .expect("command");
    assert!(allowed.argv.contains(&"--share-net".to_owned()));
}

#[test]
fn provenance_says_which_network_was_actually_in_force() {
    // A later reader must be able to tell whether an artifact was built under
    // isolation. "sandboxed" with unrestricted egress would be a lie.
    assert_eq!(
        NetworkPolicy::Denied.provenance(),
        "bubblewrap+landlock;network=denied"
    );
    assert!(
        NetworkPolicy::DependencyResolution
            .provenance()
            .contains("host-unrestricted"),
        "unrestricted egress must say so"
    );
}

#[test]
fn the_build_environment_is_cleared_rather_than_inherited() {
    if !containment_available() {
        eprintln!("skipping: bwrap is unavailable");
        return;
    }
    let (_root, source, workspace) = dirs();
    let built =
        command(&["true"], &source, &workspace, None, NetworkPolicy::Denied).expect("command");
    // An ambient token in the worker's environment is exactly what an untrusted
    // build would go looking for.
    assert!(built.argv.contains(&"--clearenv".to_owned()));
    let argv = built.argv.join(" ");
    assert!(argv.contains("PYTHONDONTWRITEBYTECODE 1"));
    assert!(argv.contains("PYTHONNOUSERSITE 1"));
}

#[test]
fn credential_directories_are_overlaid_with_empty_tmpfs() {
    if !containment_available() {
        eprintln!("skipping: bwrap is unavailable");
        return;
    }
    let (_root, source, workspace) = dirs();
    let built =
        command(&["true"], &source, &workspace, None, NetworkPolicy::Denied).expect("command");
    let argv = built.argv.join(" ");
    for expected in [".ssh", ".aws"] {
        assert!(argv.contains(expected), "{expected} was not overlaid");
    }
}

#[test]
fn a_step_that_declared_no_network_does_not_get_one() {
    if !containment_available() {
        eprintln!("skipping: bwrap is unavailable");
        return;
    }
    let (root, source, workspace) = dirs();
    let plan = EffectiveBuildPlanV1 {
        schema: EFFECTIVE_BUILD_PLAN_V1_SCHEMA.to_owned(),
        lane: Lane::PythonProcess,
        workspace_guest_root: "/app".to_owned(),
        runtime: Default::default(),
        steps: vec![BuildStepV1 {
            name: "offline".to_owned(),
            argv: vec!["/bin/true".to_owned()],
            needs_network: false,
        }],
        output_root: String::new(),
    };
    // The job's policy allows the network; the step did not ask for it. The
    // narrower of the two wins.
    let binary = worker_binary();
    let outcome = run_build(
        &plan,
        attempt(1),
        &BuildSandbox {
            source_root: &source,
            workspace_root: &workspace,
            cache_root: None,
            shim: Path::new(&binary),
            policy_host_path: Path::new("/tmp/policy.json"),
            network: NetworkPolicy::DependencyResolution,
            limits: BuildLimits::default(),
        },
    );
    assert!(outcome.is_ok(), "{outcome:?}");
    drop(root);
}

#[test]
fn a_networked_step_under_a_denied_policy_is_refused() {
    let (_root, source, workspace) = dirs();
    let plan = EffectiveBuildPlanV1 {
        schema: EFFECTIVE_BUILD_PLAN_V1_SCHEMA.to_owned(),
        lane: Lane::PythonProcess,
        workspace_guest_root: "/app".to_owned(),
        runtime: Default::default(),
        steps: vec![BuildStepV1 {
            name: "install".to_owned(),
            argv: vec!["/bin/true".to_owned()],
            needs_network: true,
        }],
        output_root: String::new(),
    };
    let binary = worker_binary();
    let error = run_build(
        &plan,
        attempt(1),
        &BuildSandbox {
            source_root: &source,
            workspace_root: &workspace,
            cache_root: None,
            shim: Path::new(&binary),
            policy_host_path: Path::new("/tmp/policy.json"),
            network: NetworkPolicy::Denied,
            limits: BuildLimits::default(),
        },
    )
    .unwrap_err();
    assert!(format!("{error}").contains("denies it"), "{error}");
}

#[test]
fn a_stale_attempt_may_not_publish() {
    // A slow attempt finishing after its retry has published is the ordinary
    // case, not the exotic one. Without the fence it would overwrite newer
    // bytes with older ones and nothing downstream could tell.
    assert!(
        may_publish(&attempt(2), 2),
        "the current attempt may publish"
    );
    assert!(may_publish(&attempt(3), 2), "a newer attempt may publish");
    assert!(!may_publish(&attempt(1), 2), "a superseded attempt may not");
}

#[test]
fn a_declared_output_root_the_build_did_not_produce_is_a_failure() {
    let (_root, _source, workspace) = dirs();
    let plan = EffectiveBuildPlanV1 {
        schema: EFFECTIVE_BUILD_PLAN_V1_SCHEMA.to_owned(),
        lane: Lane::StaticWeb,
        workspace_guest_root: "/app".to_owned(),
        runtime: Default::default(),
        steps: Vec::new(),
        output_root: "dist".to_owned(),
    };
    let outcome = BuildOutcome {
        attempt: attempt(1),
        workspace_root: workspace.clone(),
        diagnostics: Vec::new(),
    };
    let error = output_root(&outcome, &plan).unwrap_err();
    // Declaration and execution disagreeing is a build failure, never a reason
    // to fall back to publishing the whole workspace.
    assert!(format!("{error}").contains("did not produce"), "{error}");

    std::fs::create_dir_all(workspace.join("dist")).expect("mkdir");
    assert!(output_root(&outcome, &plan).is_ok());
}

#[test]
fn a_worker_that_cannot_contain_a_build_refuses_it() {
    // Not a downgrade to unconfined: the artifact would be one nobody can
    // vouch for.
    if containment_available() {
        require_containment().expect("this host can contain a build");
    } else {
        let error = require_containment().unwrap_err();
        assert!(format!("{error}").contains("Refusing"), "{error}");
    }
}

fn attempt(fence: u64) -> BuildAttempt {
    BuildAttempt {
        job_id: "fjob_test".to_owned(),
        attempt_id: format!("fatt_{fence}"),
        attempt_fence: fence,
    }
}

/// This test binary's sibling worker binary, for the shim path.
fn worker_binary() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|dir| dir.join("ato-formation-worker")))
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "/nonexistent".to_owned())
}
