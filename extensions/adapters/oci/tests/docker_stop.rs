//! Real-Docker checks of stop classification and ownership scanning.
//!
//! Ignored by default: they need a native Linux Docker host and pull a pinned
//! image. Run with `cargo test -p ato-adapter-oci --test docker_stop -- --ignored`.

use std::collections::BTreeMap;
use std::time::Duration;

use ato_adapter_oci::{
    DockerOciAdapter, OciOwner, OciResourceLimits, OciSpec, OwnedResourceScanner, StopBudget,
    StopOutcome,
};

/// nginx-unprivileged ships a POSIX shell, which lets one image both honour
/// and ignore its stop signal. The image declares `STOPSIGNAL SIGQUIT`: the
/// stop must send the image's signal, not a hard-coded SIGTERM.
const IMAGE: &str = "docker.io/nginxinc/nginx-unprivileged@sha256:28d91bdce70ad09025ea901458fdd149259d8e05982ade79d4ef2c0d9470eb48";

fn owner(lease: &str) -> OciOwner {
    OciOwner {
        runner_id: "itest-runner".to_owned(),
        slot_id: format!("itest-slot-{}", std::process::id()),
        lease_id: lease.to_owned(),
        run_id: format!("run_{lease}"),
        incarnation: "itest".to_owned(),
    }
}

fn spec(lease: &str, script: &str) -> OciSpec {
    OciSpec {
        id: format!("itest-{lease}"),
        image: IMAGE.to_owned(),
        platform: "linux/amd64".to_owned(),
        entrypoint: Some("/bin/sh".to_owned()),
        argv: vec!["-c".to_owned(), script.to_owned()],
        working_dir: "/app".to_owned(),
        workspace_mount_path: "/app".to_owned(),
        environment: BTreeMap::new(),
        endpoints: Vec::new(),
        mounts: Vec::new(),
        limits: OciResourceLimits {
            memory_bytes: 64 * 1024 * 1024,
            cpu_limit_millis: 200,
            pids_limit: 32,
        },
        stop_timeout_seconds: 2,
        labels: owner(lease).labels(None).unwrap(),
    }
}

fn budget() -> StopBudget {
    StopBudget {
        graceful: Duration::from_secs(3),
        force: Duration::from_secs(5),
    }
}

#[test]
#[ignore = "needs a native Linux Docker host"]
fn a_container_that_honours_its_stop_signal_stops_gracefully() {
    let workspace = tempfile::tempdir().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let adapter = DockerOciAdapter::new(spec(
        "graceful",
        "trap 'exit 0' QUIT; while :; do sleep 1; done",
    ))
    .unwrap();
    let handle = adapter.spawn(workspace.path(), runtime.path()).unwrap();
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(
        handle.stop_gracefully(budget()),
        StopOutcome::Graceful { exit_code: 0 }
    );
}

#[test]
#[ignore = "needs a native Linux Docker host"]
fn a_container_that_ignores_its_stop_signal_is_forced_and_reported_so() {
    let workspace = tempfile::tempdir().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let adapter = DockerOciAdapter::new(spec(
        "forced",
        "trap '' QUIT TERM; while :; do sleep 1; done",
    ))
    .unwrap();
    let handle = adapter.spawn(workspace.path(), runtime.path()).unwrap();
    std::thread::sleep(Duration::from_secs(1));
    let outcome = handle.stop_gracefully(budget());
    assert!(
        matches!(outcome, StopOutcome::Forced { exit_code: 137 }),
        "{outcome:?}"
    );
}

#[test]
#[ignore = "needs a native Linux Docker host"]
fn a_scan_finds_only_this_slots_leftovers_and_stops_them() {
    let workspace = tempfile::tempdir().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let adapter = DockerOciAdapter::new(spec(
        "leftover",
        "trap 'exit 0' QUIT; while :; do sleep 1; done",
    ))
    .unwrap();
    let handle = adapter.spawn(workspace.path(), runtime.path()).unwrap();
    // Simulate a Runner that died: the handle is gone, the container is not.
    std::mem::forget(handle);

    let me = owner("leftover");
    let scanner = OwnedResourceScanner::new(&me.runner_id, &me.slot_id).unwrap();
    let owned = scanner.scan().unwrap();
    assert_eq!(owned.containers.len(), 1, "{owned:?}");
    assert_eq!(owned.containers[0].lease_id(), Some("leftover"));
    assert_eq!(owned.networks.len(), 1);

    // Another slot sees none of it.
    let other = OwnedResourceScanner::new(&me.runner_id, "itest-other-slot").unwrap();
    assert!(other.scan().unwrap().containers.is_empty());

    assert!(
        scanner
            .stop_container(&owned.containers[0], budget())
            .is_confirmed()
    );
    scanner.remove_network(&owned.networks[0]).unwrap();
    let after = scanner.scan().unwrap();
    assert!(after.containers.is_empty() && after.networks.is_empty());
}
