use std::fs;
use std::path::Path;
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;

fn ato(ato_home: &Path) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("ato"));
    command.env("ATO_HOME", ato_home);
    command
}

fn repository(root: &Path) {
    #[cfg(windows)]
    {
        fs::write(
            root.join("capsule.toml"),
            "[workspace]\ntoolchain = 'cmd'\nentrypoint = ['cmd', '/C', 'if defined UNDECLARED_HOST_VALUE exit /b 9 & mkdir build-output & exit /b 0']\n",
        )
        .unwrap();
    }
    #[cfg(not(windows))]
    {
        fs::write(
            root.join("capsule.toml"),
            "[workspace]\ntoolchain = 'shell'\nentrypoint = ['sh', 'main.sh']\n",
        )
        .unwrap();
        fs::write(
            root.join("main.sh"),
            "#!/bin/sh\ntest -z \"$UNDECLARED_HOST_VALUE\"\nmkdir build-output\n",
        )
        .unwrap();
    }
}

#[test]
fn lock_run_and_sealed_workspace_fail_closed_end_to_end() {
    let repository_directory = tempfile::tempdir().unwrap();
    let ato_home = tempfile::tempdir().unwrap();
    repository(repository_directory.path());

    ato(ato_home.path())
        .args(["lock", repository_directory.path().to_str().unwrap()])
        .assert()
        .success();
    assert!(repository_directory.path().join("capsule.lock").is_file());
    assert!(!repository_directory.path().join("ato.lock.json").exists());

    ato(ato_home.path())
        .env("UNDECLARED_HOST_VALUE", "must-not-leak")
        .args([
            "run",
            repository_directory.path().to_str().unwrap(),
            "--no-sandbox",
        ])
        .assert()
        .success();
    assert!(!repository_directory.path().join("build-output").exists());
    assert!(
        fs::read_dir(ato_home.path().join("runs/run"))
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("workspace-"))
    );

    fs::write(
        repository_directory.path().join("source-mutation"),
        "changed",
    )
    .unwrap();
    ato(ato_home.path())
        .args([
            "run",
            repository_directory.path().to_str().unwrap(),
            "--no-sandbox",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "capsule.lock is stale; run `ato lock`",
        ));

    fs::write(repository_directory.path().join("capsule.lock"), "not json").unwrap();
    ato(ato_home.path())
        .args([
            "run",
            repository_directory.path().to_str().unwrap(),
            "--no-sandbox",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("capsule.lock is malformed"));

    fs::remove_file(repository_directory.path().join("capsule.lock")).unwrap();
    fs::write(repository_directory.path().join("ato.lock.json"), "{}").unwrap();
    ato(ato_home.path())
        .args([
            "run",
            repository_directory.path().to_str().unwrap(),
            "--no-sandbox",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("deprecated ato.lock.json found"));
}

#[cfg(unix)]
#[test]
fn detached_stop_terminates_the_isolated_process_tree() {
    let repository_directory = tempfile::tempdir().unwrap();
    let ato_home = tempfile::tempdir().unwrap();
    fs::write(
        repository_directory.path().join("capsule.toml"),
        "[workspace]\ntoolchain = 'shell'\nentrypoint = ['sh', 'main.sh']\n",
    )
    .unwrap();
    fs::write(
        repository_directory.path().join("main.sh"),
        "#!/bin/sh\nsleep 60 &\necho $! > \"$HOME/child.pid\"\nwait\n",
    )
    .unwrap();

    ato(ato_home.path())
        .args([
            "run",
            repository_directory.path().to_str().unwrap(),
            "--no-sandbox",
            "--detach",
            "--name",
            "tree",
        ])
        .assert()
        .success();

    let record_path = ato_home.path().join("runs/tree/run.json");
    wait_until(|| record_path.is_file());
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    let worker = record["pid"].as_u64().unwrap() as u32;
    let child_file = || {
        fs::read_dir(ato_home.path().join("runs/tree"))
            .ok()?
            .filter_map(Result::ok)
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("workspace-")
            })
            .map(|entry| entry.path().join(".ato/child.pid"))
    };
    wait_until(|| child_file().is_some_and(|path| path.is_file()));
    let child = fs::read_to_string(child_file().unwrap())
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();

    ato(ato_home.path())
        .args(["stop", "tree"])
        .assert()
        .success();
    wait_until(|| !pid_exists(worker) && !pid_exists(child));
    assert!(!pid_exists(worker));
    assert!(!pid_exists(child));
}

#[cfg(unix)]
fn pid_exists(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(unix)]
fn wait_until(mut predicate: impl FnMut() -> bool) {
    for _ in 0..100 {
        if predicate() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
