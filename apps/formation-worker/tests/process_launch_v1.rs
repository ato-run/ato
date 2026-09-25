//! The Runtime's contained process launch, end to end, with the real
//! `sandbox-exec` shim.
//!
//! This was a unit test of the launch module that used the test executable
//! as the shim. A libtest binary has no `sandbox-exec` command, so the
//! workload never ran as specified and a `bwrap … sleep 30` could outlive
//! the test and hold its stdout open. Here the shim is this package's own
//! worker binary — the one a temporary realization launches with — and the
//! test checks that nothing of the launch survives `stop`.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use ato_ipc::runtime_launch::{
    EndpointAllocationV1, EndpointV1, LaunchRealizationV1, LifecycleV1, ProcessRealizationV1,
    RuntimeLaunchSpecV1, StateAccessV1,
};
use ato_runtime_attempt::launch::process_executor::{
    ProcessLaunchHost, launch_process_with, observed_launch, state_working_copy,
};
use ato_runtime_attempt::launch::resolved::{
    ResolvedRuntimeLaunchContext, ResolvedSecret, ResolvedStateAttachment, allocate_endpoint,
};
use ato_runtime_attempt::launch::sandbox::containment_available;

const PROCESS_FIXTURE: &str =
    include_str!("../../../lib/ipc/tests/fixtures/runtime-launch-spec-v1/fastapi-process.json");

/// Pids whose command line contains `marker`; the `[x]` form keeps this
/// pgrep, and any other running concurrently, out of the answer.
fn processes_matching(marker: &str) -> Vec<String> {
    let (first, rest) = marker.split_at(1);
    let output = std::process::Command::new("pgrep")
        .args(["-f", &format!("[{first}]{rest}")])
        .output()
        .expect("pgrep");
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_launched_workload_actually_runs_and_stops() {
    if !containment_available() {
        // Not a skip worth hiding: this Runtime cannot contain a workload,
        // so it must not launch one.
        eprintln!("skipping: `bwrap` is not available, so no workload may be launched here");
        return;
    }
    let lease = tempfile::tempdir().expect("tempdir");
    let workspace = lease.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let working = state_working_copy(&workspace, "app_data");
    let context = ResolvedRuntimeLaunchContext::new(
        workspace.clone(),
        "",
        BTreeMap::from([("PORT".to_owned(), "8000".to_owned())]),
        vec![ResolvedSecret::new("APP_SECRET_KEY", "hunter2")],
        vec![ResolvedStateAttachment::new(
            "app_data",
            None,
            working,
            "/data",
            StateAccessV1::ReadWrite,
        )],
        vec![allocate_endpoint(
            &EndpointV1 {
                name: "web".to_owned(),
                protocol: "http".to_owned(),
                guest_port: Some(8000),
                allocation: EndpointAllocationV1::Automatic,
                preferred_port: None,
            },
            34567,
        )],
    )
    .expect("context resolves");

    let marker = format!("ato-launch-test-{}", std::process::id());
    let mut spec = RuntimeLaunchSpecV1::parse(PROCESS_FIXTURE).expect("fixture spec");
    spec.realization = LaunchRealizationV1::Process(ProcessRealizationV1 {
        // `$0` carries the marker, so every process of this launch — bwrap,
        // the shim, the shell — can be found by it.
        argv: vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "test \"$APP_SECRET_KEY\" = hunter2 && touch /data/secret-available && sleep 30"
                .to_owned(),
            marker.clone(),
        ],
        executable: None,
    });
    let launched = launch_process_with(
        &spec,
        &context,
        &ProcessLaunchHost {
            shim: env!("CARGO_BIN_EXE_ato-formation-worker").into(),
            runtime_root: lease.path().join("process-runtime"),
            output: None,
        },
    )
    .expect("launches");
    assert!(launched.pid() > 0);
    // The attachment directory exists BEFORE the workload starts, so an app
    // cannot mistake a missing path for an empty one.
    assert!(state_working_copy(&workspace, "app_data").is_dir());

    // The workload itself is running — not a shim that exited on arrival.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !state_working_copy(&workspace, "app_data")
        .join("secret-available")
        .exists()
    {
        assert!(Instant::now() < deadline, "the workload never started");
        std::thread::sleep(Duration::from_millis(50));
    }

    let observed = observed_launch(&spec, &context, &launched);
    assert_eq!(
        observed.get("state").map(String::as_str),
        Some("app_data@<new>")
    );
    assert!(!format!("{observed:?}").contains("hunter2"));

    launched
        .stop(&LifecycleV1 {
            graceful_shutdown_ms: 2_000,
            force_kill_after_ms: 4_000,
        })
        .expect("stops");

    // Nothing of the launch outlives it: no bwrap, no shim, no shell.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !processes_matching(&marker).is_empty() {
        assert!(
            Instant::now() < deadline,
            "processes outlived the launch: {:?}",
            processes_matching(&marker)
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}
