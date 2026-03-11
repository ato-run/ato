mod fail_closed_support;

use std::fs;
use std::process::Stdio;

use fail_closed_support::*;
use serde::Deserialize;
use serde_json::Value;
use tempfile::TempDir;

#[derive(Debug, Deserialize)]
struct PersistedProcessInfo {
    id: String,
    status: String,
    #[serde(default)]
    last_error: Option<String>,
    #[serde(default)]
    exit_code: Option<i32>,
    #[serde(default)]
    log_path: Option<std::path::PathBuf>,
}

fn read_single_process(home: &std::path::Path) -> PersistedProcessInfo {
    let run_dir = home.join(".ato").join("run");
    let pid_file = fs::read_dir(&run_dir)
        .expect("failed to read run dir")
        .filter_map(|entry| entry.ok().map(|value| value.path()))
        .find(|path| path.extension().and_then(|value| value.to_str()) == Some("pid"))
        .expect("expected a persisted process record");
    let raw = fs::read_to_string(&pid_file).expect("failed to read persisted process record");
    toml::from_str(&raw).expect("failed to parse persisted process record")
}

fn wait_for_single_process(home: &std::path::Path) -> PersistedProcessInfo {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let run_dir = home.join(".ato").join("run");
        if run_dir.exists() {
            let mut pid_files = fs::read_dir(&run_dir)
                .expect("failed to read run dir")
                .filter_map(|entry| entry.ok().map(|value| value.path()))
                .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("pid"))
                .collect::<Vec<_>>();
            pid_files.sort();
            if let Some(pid_file) = pid_files.into_iter().next() {
                let raw =
                    fs::read_to_string(&pid_file).expect("failed to read persisted process record");
                return toml::from_str(&raw).expect("failed to parse persisted process record");
            }
        }

        assert!(
            std::time::Instant::now() < deadline,
            "expected a persisted process record in {}",
            run_dir.display()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn reconcile_processes_via_ps(home: &std::path::Path) {
    let output = ato_cmd()
        .arg("ps")
        .arg("--all")
        .arg("--json")
        .env("HOME", home)
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .expect("failed to execute ps for reconciliation");

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let _parsed: Vec<Value> =
        serde_json::from_slice(&output.stdout).expect("ps --json must return valid json");
}

#[cfg(unix)]
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "unstable background process persistence under macOS test runner"
)]
fn background_native_run_waits_until_ready_and_persists_ready_state() {
    let (_workspace, fixture) = prepare_fixture_workspace("native-shell-capsule");
    let home = TempDir::new().expect("failed to create temporary HOME");

    let mock_dir = TempDir::new().expect("failed to create temp dir for mock nacelle");
    let nacelle_path = mock_dir.path().join("nacelle");
    let log_path = home.path().join("mock-ready.log");
    write_mock_nacelle_ready(&nacelle_path, &log_path);

    let output = ato_cmd()
        .arg("run")
        .arg("--yes")
        .arg("--sandbox")
        .arg("--background")
        .arg("--nacelle")
        .arg(&nacelle_path)
        .arg(&fixture)
        .env("HOME", home.path())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .expect("failed to execute background ready fixture");

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let persisted = wait_for_single_process(home.path());
    assert_eq!(persisted.status, "Ready");
    assert_eq!(persisted.log_path.as_deref(), Some(log_path.as_path()));

    let close_output = ato_cmd()
        .arg("close")
        .arg("--id")
        .arg(&persisted.id)
        .env("HOME", home.path())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .expect("failed to stop background ready fixture");

    assert!(
        close_output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&close_output.stderr)
    );

    let persisted = wait_for_single_process(home.path());
    assert_eq!(persisted.status, "Exited");
}

#[cfg(unix)]
#[test]
fn background_native_run_fails_closed_before_readiness() {
    let (_workspace, fixture) = prepare_fixture_workspace("native-shell-capsule");
    let home = TempDir::new().expect("failed to create temporary HOME");

    let mock_dir = TempDir::new().expect("failed to create temp dir for mock nacelle");
    let nacelle_path = mock_dir.path().join("nacelle");
    let log_path = home.path().join("mock-failed.log");
    write_mock_nacelle_fail_before_ready(&nacelle_path, &log_path);

    let output = ato_cmd()
        .arg("run")
        .arg("--yes")
        .arg("--sandbox")
        .arg("--background")
        .arg("--nacelle")
        .arg(&nacelle_path)
        .arg(&fixture)
        .env("HOME", home.path())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .expect("failed to execute background failure fixture");

    assert!(
        !output.status.success(),
        "stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed before readiness"),
        "stderr={stderr}"
    );

    let persisted = read_single_process(home.path());
    assert_eq!(persisted.status, "Failed");
    assert_eq!(persisted.exit_code, Some(42));
    assert!(
        persisted
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("before readiness"),
        "persisted={persisted:?}"
    );
}

#[cfg(unix)]
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "unstable background process persistence under macOS test runner"
)]
fn background_native_run_eventually_persists_exited_state_after_ready() {
    let (_workspace, fixture) = prepare_fixture_workspace("native-shell-capsule");
    let home = TempDir::new().expect("failed to create temporary HOME");

    let mock_dir = TempDir::new().expect("failed to create temp dir for mock nacelle");
    let nacelle_path = mock_dir.path().join("nacelle");
    let log_path = home.path().join("mock-ready-then-exit.log");
    write_mock_nacelle_ready_then_exit(&nacelle_path, &log_path, 17);

    let output = ato_cmd()
        .arg("run")
        .arg("--yes")
        .arg("--sandbox")
        .arg("--background")
        .arg("--nacelle")
        .arg(&nacelle_path)
        .arg(&fixture)
        .env("HOME", home.path())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .expect("failed to execute background ready-then-exit fixture");

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let initial = wait_for_single_process(home.path());
    assert_eq!(initial.status, "Ready");
    assert_eq!(initial.log_path.as_deref(), Some(log_path.as_path()));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        reconcile_processes_via_ps(home.path());
        let persisted = read_single_process(home.path());
        if persisted.status == "Exited" {
            assert_eq!(persisted.exit_code, Some(17));
            return;
        }

        assert!(
            std::time::Instant::now() < deadline,
            "process did not transition to Exited in time: {persisted:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(unix)]
#[test]
#[cfg_attr(
    target_os = "macos",
    ignore = "unstable background process persistence under macOS test runner"
)]
fn background_native_run_timeout_eventually_reconciles_to_failed() {
    let (_workspace, fixture) = prepare_fixture_workspace("native-shell-capsule");
    let home = TempDir::new().expect("failed to create temporary HOME");

    let mock_dir = TempDir::new().expect("failed to create temp dir for mock nacelle");
    let nacelle_path = mock_dir.path().join("nacelle");
    let log_path = home.path().join("mock-starting-then-exit.log");
    write_mock_nacelle_starting_then_exit(&nacelle_path, &log_path, 3, 23);

    let output = ato_cmd()
        .arg("run")
        .arg("--yes")
        .arg("--sandbox")
        .arg("--background")
        .arg("--nacelle")
        .arg(&nacelle_path)
        .arg(&fixture)
        .env("HOME", home.path())
        .env("ATO_BACKGROUND_READY_WAIT_TIMEOUT_SECS", "1")
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()
        .expect("failed to execute background timeout fixture");

    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let initial = wait_for_single_process(home.path());
    assert_eq!(initial.status, "Starting");
    assert_eq!(initial.log_path.as_deref(), Some(log_path.as_path()));

    std::thread::sleep(std::time::Duration::from_secs(4));
    reconcile_processes_via_ps(home.path());

    let persisted = read_single_process(home.path());
    assert_eq!(persisted.status, "Failed");
    assert!(persisted.exit_code.is_none(), "persisted={persisted:?}");
    assert!(
        persisted
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("before readiness"),
        "persisted={persisted:?}"
    );
}
