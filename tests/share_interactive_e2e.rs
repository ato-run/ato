#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use expectrl::{Eof, Expect, Session};
#[cfg(unix)]
use serial_test::serial;
#[cfg(unix)]
use tempfile::TempDir;

#[cfg(unix)]
fn workspace_tempdir(prefix: &str) -> TempDir {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".tmp");
    fs::create_dir_all(&root).expect("create workspace .tmp");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(root)
        .expect("create workspace tempdir")
}

#[cfg(unix)]
fn write_prompt_env_fixture(dir: &std::path::Path) {
    let spec = r#"{
  "schema_version": "1",
  "name": "test-share-prompt",
  "root": ".",
  "sources": [],
  "tool_requirements": [],
  "env_requirements": [],
  "install_steps": [],
  "services": [],
  "notes": {"team_notes": ""},
  "generated_from": {"root_path": ".", "captured_at": "2025-01-01T00:00:00Z", "host_os": "test"},
  "entries": [{
    "id": "dashboard",
    "label": "Dashboard",
    "cwd": ".",
    "run": "sh -c 'echo DEMO_TOKEN=$DEMO_TOKEN'",
    "kind": "task",
    "primary": true,
    "depends_on": [],
    "env": {"required": ["DEMO_TOKEN"], "optional": [], "files": []},
    "evidence": []
  }]
}"#;
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

#[cfg(unix)]
fn write_no_env_fixture(dir: &std::path::Path) {
    // Entry echoes each positional arg on its own line (supports trailing args test).
    // The run command is: sh -c 'for x in "$@"; do echo "$x"; done' --
    // With trailing args appended by ato via shell_words::join, this lets us
    // verify that each arg arrives as a distinct line (testing no word-splitting).
    let spec = r#"{
  "schema_version": "1",
  "name": "test-share-args",
  "root": ".",
  "sources": [],
  "tool_requirements": [],
  "env_requirements": [],
  "install_steps": [],
  "entries": [{
    "id": "main",
    "label": "Main",
    "cwd": ".",
    "run": "sh -c 'for x in \"$@\"; do echo \"$x\"; done' --",
    "kind": "task",
    "primary": true,
    "depends_on": [],
    "env": {"required": [], "optional": [], "files": []},
    "evidence": []
  }],
  "services": [],
  "notes": {"team_notes": ""},
  "generated_from": {"root_path": ".", "captured_at": "2025-01-01T00:00:00Z", "host_os": "test"}
}"#;
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

/// E2E-1: Save env values via prompt, then verify reuse on non-TTY re-run.
#[cfg(unix)]
#[test]
#[serial]
fn test_e2e_1_prompt_env_save_then_reuse() {
    let tmp = workspace_tempdir("e2e1-save-reuse-");
    let tmp_home = workspace_tempdir("e2e1-home-");
    write_prompt_env_fixture(tmp.path());
    let spec_path = tmp.path().join("share.spec.json");

    // --- Setup run via PTY: fill in env values and save them ---
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_ato"));
    cmd.arg("run")
        .arg(spec_path.to_str().unwrap())
        .env("HOME", tmp_home.path())
        .current_dir(tmp.path());

    let mut session = Session::spawn(cmd).expect("spawn PTY session for setup run");
    session.set_expect_timeout(Some(Duration::from_secs(30)));

    // "Enter values now? [Y/n] " because --prompt-env was NOT passed and env is missing.
    session
        .expect("Enter values now?")
        .expect("enter-values prompt");
    session.send_line("y").expect("confirm enter values");

    session
        .expect("DEMO_TOKEN:")
        .expect("DEMO_TOKEN key prompt");
    session
        .send_line("test-secret-value")
        .expect("send token value");

    session.expect("Save these values").expect("save prompt");
    session.send_line("y").expect("confirm save");

    // The entry echoes the token; wait for it to appear before draining.
    session
        .expect("DEMO_TOKEN=test-secret-value")
        .expect("echo output with saved token");
    session.expect(Eof).ok();

    // Verify that the env store now has a saved file.
    let env_store = tmp_home.path().join(".ato").join("env").join("targets");
    assert!(
        env_store.exists(),
        "env store directory should exist after saving"
    );
    let any_saved = fs::read_dir(&env_store)
        .map(|mut d| d.next().is_some())
        .unwrap_or(false);
    assert!(
        any_saved,
        "at least one env file should be written after save=yes"
    );

    // --- Reuse run: non-PTY, saved env must be loaded automatically ---
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .arg("run")
        .arg(spec_path.to_str().unwrap())
        .env("HOME", tmp_home.path())
        .current_dir(tmp.path())
        .output()
        .expect("reuse run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "reuse run should succeed (saved env loaded). stderr: {}",
        stderr
    );
    // The entry command echoes DEMO_TOKEN; verify the value appears somewhere.
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("DEMO_TOKEN=test-secret-value"),
        "saved token should appear in output. stdout: {stdout}, stderr: {stderr}"
    );
    // Must NOT have been asked for values again.
    assert!(
        !stderr.contains("Enter values now"),
        "reuse run must not re-prompt. stderr: {stderr}"
    );
}

/// E2E-2: Save=no means no persisted file; second run fails missing-env on non-TTY.
#[cfg(unix)]
#[test]
#[serial]
fn test_e2e_2_prompt_env_use_once() {
    let tmp = workspace_tempdir("e2e2-use-once-");
    let tmp_home = workspace_tempdir("e2e2-home-");
    write_prompt_env_fixture(tmp.path());
    let spec_path = tmp.path().join("share.spec.json");

    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_ato"));
    cmd.arg("run")
        .arg(spec_path.to_str().unwrap())
        .env("HOME", tmp_home.path())
        .current_dir(tmp.path());

    let mut session = Session::spawn(cmd).expect("spawn PTY session");
    session.set_expect_timeout(Some(Duration::from_secs(30)));

    session
        .expect("Enter values now?")
        .expect("enter-values prompt");
    session.send_line("y").expect("confirm");

    session.expect("DEMO_TOKEN:").expect("key prompt");
    session.send_line("once-only-value").expect("send value");

    session.expect("Save these values").expect("save prompt");
    session.send_line("n").expect("decline save");

    // The entry should still run and echo the token, even when save is declined.
    session
        .expect("DEMO_TOKEN=once-only-value")
        .expect("entry must echo the token on the first run");

    // Drain so the process can exit cleanly.
    session.expect(Eof).ok();

    // Verify no env file was persisted.
    let env_store = tmp_home.path().join(".ato").join("env").join("targets");
    let any_saved = if env_store.exists() {
        fs::read_dir(&env_store)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
    } else {
        false
    };
    assert!(
        !any_saved,
        "no env file should be persisted when user answers 'n'"
    );

    // Second non-TTY run should fail because the env is missing and stdin is not a TTY.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .arg("run")
        .arg(spec_path.to_str().unwrap())
        .env("HOME", tmp_home.path())
        .current_dir(tmp.path())
        .output()
        .expect("second run");

    assert!(
        !output.status.success(),
        "second run should fail (no saved env, no TTY)"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Missing required environment variables"),
        "expected missing-env error on second run. stderr: {stderr}"
    );
}

/// E2E-3: Cancel at "Enter values now?" prompt → fail-closed, no saved file.
#[cfg(unix)]
#[test]
#[serial]
fn test_e2e_3_prompt_env_cancel() {
    let tmp = workspace_tempdir("e2e3-cancel-");
    let tmp_home = workspace_tempdir("e2e3-home-");
    write_prompt_env_fixture(tmp.path());
    let spec_path = tmp.path().join("share.spec.json");

    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_ato"));
    cmd.arg("run")
        .arg(spec_path.to_str().unwrap())
        .env("HOME", tmp_home.path())
        .current_dir(tmp.path());

    let mut session = Session::spawn(cmd).expect("spawn PTY session");
    session.set_expect_timeout(Some(Duration::from_secs(30)));

    session
        .expect("Enter values now?")
        .expect("enter-values prompt");
    session.send_line("n").expect("cancel");

    // The process should print a cancellation message and exit non-zero.
    session.expect("Cancelled").expect("cancellation message");
    session.expect(Eof).ok();

    use expectrl::process::unix::WaitStatus;
    let status = session
        .get_process()
        .wait()
        .expect("wait for process after cancel");
    assert!(
        matches!(status, WaitStatus::Exited(_, code) if code != 0),
        "cancel should produce non-zero exit. got: {:?}",
        status
    );

    // No env file should exist.
    let env_store = tmp_home.path().join(".ato").join("env").join("targets");
    let any_saved = if env_store.exists() {
        fs::read_dir(&env_store)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
    } else {
        false
    };
    assert!(!any_saved, "no env file should be created after cancel");
}

/// E2E-9: Trailing args after `--` reach the entry command without corruption.
#[cfg(unix)]
#[test]
fn test_e2e_9_trailing_args() {
    let tmp = workspace_tempdir("e2e9-trailing-");
    let tmp_home = workspace_tempdir("e2e9-home-");
    write_no_env_fixture(tmp.path());
    let spec_path = tmp.path().join("share.spec.json");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_ato"))
        .arg("run")
        .arg(spec_path.to_str().unwrap())
        .arg("--")
        .arg("firstarg")
        .arg("arg with spaces")
        .env("HOME", tmp_home.path())
        .current_dir(tmp.path())
        .output()
        .expect("run with trailing args");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "run with trailing args should exit 0. stderr: {stderr}"
    );

    // Verify argv passthrough: each arg must appear as its own line.
    // If arg-splitting occurred, "arg with spaces" would become three separate lines.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines.contains(&"firstarg"),
        "firstarg should appear as its own output line; got lines: {lines:?}"
    );
    assert!(
        lines.contains(&"arg with spaces"),
        "'arg with spaces' should appear as a single line (not split into words); got lines: {lines:?}"
    );
}

#[cfg(unix)]
fn write_multi_entry_fixture(dir: &std::path::Path) {
    // Two entries, both primary=false so the interactive chooser is triggered.
    // First entry echoes "first-entry-ran"; second echoes "second-entry-ran".
    let spec = r#"{
  "schema_version": "1",
  "name": "test-multi-entry",
  "root": ".",
  "sources": [],
  "tool_requirements": [],
  "env_requirements": [],
  "install_steps": [],
  "entries": [
    {
      "id": "first-entry",
      "label": "First Entry",
      "cwd": ".",
      "run": "echo first-entry-ran",
      "kind": "task",
      "primary": false,
      "depends_on": [],
      "env": {"required": [], "optional": [], "files": []},
      "evidence": []
    },
    {
      "id": "second-entry",
      "label": "Second Entry",
      "cwd": ".",
      "run": "echo second-entry-ran",
      "kind": "task",
      "primary": false,
      "depends_on": [],
      "env": {"required": [], "optional": [], "files": []},
      "evidence": []
    }
  ],
  "services": [],
  "notes": {"team_notes": ""},
  "generated_from": {"root_path": ".", "captured_at": "2025-01-01T00:00:00Z", "host_os": "test"}
}"#;
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

/// E2E-6: Multi-entry chooser selects the intended entry via PTY interaction.
///
/// When a share spec has multiple entries with no single primary, `ato run`
/// must show a numbered chooser and run whichever entry the user picks.
#[cfg(unix)]
#[test]
#[serial]
fn test_e2e_6_multi_entry_chooser() {
    let tmp = workspace_tempdir("e2e6-chooser-");
    let tmp_home = workspace_tempdir("e2e6-home-");
    write_multi_entry_fixture(tmp.path());
    let spec_path = tmp.path().join("share.spec.json");

    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_ato"));
    cmd.arg("run")
        .arg(spec_path.to_str().unwrap())
        .env("HOME", tmp_home.path())
        .current_dir(tmp.path());

    let mut session = Session::spawn(cmd).expect("spawn PTY session for chooser");
    session.set_expect_timeout(Some(Duration::from_secs(30)));

    // The chooser prompt lists entries and asks for a selection.
    session
        .expect("Choose an entry [1-")
        .expect("chooser prompt");

    // Select entry 2 ("second-entry").
    session.send_line("2").expect("send entry selection");

    // Only the second entry's output should appear.
    session
        .expect("second-entry-ran")
        .expect("second entry output after selection");

    session.expect(Eof).ok();

    use expectrl::process::unix::WaitStatus;
    let status = session
        .get_process()
        .wait()
        .expect("wait for process after chooser");
    assert!(
        matches!(status, WaitStatus::Exited(_, 0)),
        "selected entry should exit 0. got: {:?}",
        status
    );
}
