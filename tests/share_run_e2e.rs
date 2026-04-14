use std::fs;
use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn workspace_tempdir(prefix: &str) -> TempDir {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".tmp");
    fs::create_dir_all(&root).expect("create workspace .tmp");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(root)
        .expect("create workspace tempdir")
}

fn write_simple_fixture(dir: &std::path::Path, entry_cmd: &str, require_env: bool) {
    let required_env = if require_env {
        r#""required": ["DEMO_TOKEN"]"#
    } else {
        r#""required": []"#
    };
    let spec = format!(
        r#"{{
  "schema_version": "1",
  "name": "test-share",
  "root": ".",
  "sources": [],
  "tool_requirements": [],
  "env_requirements": [],
  "install_steps": [],
  "services": [],
  "notes": {{"team_notes": ""}},
  "generated_from": {{"root_path": ".", "captured_at": "2025-01-01T00:00:00Z", "host_os": "test"}},
  "entries": [{{
    "id": "dashboard",
    "label": "Dashboard",
    "cwd": ".",
    "run": "{}",
    "kind": "task",
    "primary": true,
    "depends_on": [],
    "env": {{{}, "optional": [], "files": []}},
    "evidence": []
  }}]
}}"#,
        entry_cmd, required_env
    );
    let lock = r#"{
  "schema_version": "1",
  "spec_digest": "sha256:dummy",
  "generated_guide_digest": "sha256:dummy",
  "revision": 1,
  "created_at": "2025-01-01T00:00:00Z",
  "resolved_sources": [],
  "resolved_tools": []
}"#;
    fs::write(dir.join("share.spec.json"), spec).unwrap();
    fs::write(dir.join("share.lock.json"), lock).unwrap();
}

#[test]
fn test_share_run_watch_reject() {
    let tmp = workspace_tempdir("share-run-watch-");
    write_simple_fixture(tmp.path(), "sh -c 'echo ok'", false);
    let spec_path = tmp.path().join("share.spec.json");

    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.arg("run")
        .arg(spec_path.to_str().unwrap())
        .arg("--watch")
        .env("HOME", tmp.path());
    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("does not support --watch"));
}

#[test]
fn test_share_run_background_reject() {
    let tmp = workspace_tempdir("share-run-bg-");
    write_simple_fixture(tmp.path(), "sh -c 'echo ok'", false);
    let spec_path = tmp.path().join("share.spec.json");

    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.arg("run")
        .arg(spec_path.to_str().unwrap())
        .arg("--background")
        .env("HOME", tmp.path());
    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("does not support --background"));
}

#[test]
fn test_non_tty_missing_required_env_fails_closed() {
    let tmp = workspace_tempdir("share-run-env-nontty-");
    write_simple_fixture(tmp.path(), "sh -c 'echo ok'", true);
    let spec_path = tmp.path().join("share.spec.json");

    // Command::output() uses non-TTY stdin, so missing required env should fail closed.
    let mut cmd = Command::cargo_bin("ato").unwrap();
    cmd.arg("run")
        .arg(spec_path.to_str().unwrap())
        .env("HOME", tmp.path());
    cmd.assert().failure().stderr(predicate::str::contains(
        "Missing required environment variables",
    ));
}

#[test]
fn test_e2e_5_failed_run_then_rerun_no_stale_dir_error() {
    let tmp = workspace_tempdir("share-run-fail-rerun-");
    write_simple_fixture(tmp.path(), "sh -c 'echo boom; exit 1'", false);
    let spec_path = tmp.path().join("share.spec.json");

    // First run: entry exits 1, so ato run should also exit non-zero.
    let output1 = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .arg("run")
        .arg(spec_path.to_str().unwrap())
        .env("HOME", tmp.path())
        .current_dir(tmp.path())
        .output()
        .expect("first run");

    assert!(
        !output1.status.success(),
        "first run should fail (entry exits 1)"
    );
    let stderr1 = String::from_utf8_lossy(&output1.stderr);
    assert!(
        !stderr1.contains("non-empty target directory"),
        "first run should not get non-empty dir error: {}",
        stderr1
    );

    // Second run: must NOT get a stale-directory error; should fail the same way as run 1.
    let output2 = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .arg("run")
        .arg(spec_path.to_str().unwrap())
        .env("HOME", tmp.path())
        .current_dir(tmp.path())
        .output()
        .expect("second run");

    let stderr2 = String::from_utf8_lossy(&output2.stderr);
    assert!(
        !stderr2.contains("non-empty target directory"),
        "second run must not get stale dir error. stderr: {}",
        stderr2
    );
    // Entry still exits 1 so the overall run should still fail.
    assert!(
        !output2.status.success(),
        "second run should fail (entry exits 1). stderr: {}",
        stderr2
    );
}
