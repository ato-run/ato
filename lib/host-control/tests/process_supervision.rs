#![cfg(unix)]

//! Real-process tests for [`ProcessSupervisor`] and [`NativeHost`]: spawn,
//! liveness, reap, and whole-group teardown.

use std::path::PathBuf;

use ato_host_control::{
    NativeHost, OutputSink, ProcessSupervisor, RunnerHost, SpawnSpec, resolve_on_path,
};

fn host() -> NativeHost {
    NativeHost::with_path_lookup()
}

fn sleep_spec(seconds: &str) -> SpawnSpec {
    SpawnSpec {
        program: resolve_on_path("sleep").expect("sleep on PATH for the test"),
        args: vec![seconds.to_owned()],
        env: vec![],
        output: OutputSink::Null,
    }
}

#[test]
fn spawn_is_alive_until_reaped_and_shutdown_reaps_the_group() {
    let mut supervisor = ProcessSupervisor::new(host());
    let id = supervisor.spawn(&sleep_spec("30")).unwrap();
    assert_eq!(supervisor.supervised_count(), 1);
    // Spawn immediately after — the child must still be alive, so nothing reaps.
    assert_eq!(supervisor.reap(), 0);
    assert!(supervisor.contains(id));
    // Shutdown tears the whole process group down and forgets it.
    supervisor.shutdown().unwrap();
    assert_eq!(supervisor.supervised_count(), 0);
    // Idempotent — a second shutdown is a no-op success.
    supervisor.shutdown().unwrap();
}

#[test]
fn short_lived_child_is_reaped_with_its_exit_code() {
    let mut supervisor = ProcessSupervisor::new(host());
    supervisor.spawn(&sleep_spec("0")).unwrap();
    // Wait for the child to exit, then reap it with a captured exit code.
    let mut reaped = Vec::new();
    for _ in 0..200 {
        reaped = supervisor.reap_with_status();
        if !reaped.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(reaped.len(), 1);
    assert_eq!(reaped[0].1, Some(0));
    assert_eq!(supervisor.supervised_count(), 0);
}

#[test]
fn resolves_binary_and_runs_a_short_command() {
    let host = host();
    let shell = host.resolve_binary("sh").expect("sh on PATH for the test");
    let program: PathBuf = shell;
    let completed = host
        .run_to_completion(&ato_host_control::CommandSpec {
            program,
            args: vec!["-c".into(), "printf done".into()],
            env: vec![],
        })
        .unwrap();
    assert!(completed.success());
    assert_eq!(completed.stdout, b"done");
}
