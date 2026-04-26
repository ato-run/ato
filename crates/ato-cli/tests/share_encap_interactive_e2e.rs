//! E2E interactive tests for `ato encap` primary entry selection via PTY.
//!
//! These tests drive `ato --json encap --save-config .` through a PTY session and verify
//! that primary-entry changes are persisted in the saved `share.spec.json`.
//!
//! Prompt patterns used:
//!  - Bulk capture prompt: `"Accept all? [Enter]  or  skip <ids>:  "`
//!
//! Fixture: two sub-repos (`api/`, `web/`) each with a `package.json` + `dev` script.
//! Initial primary determined by `derive_entries`: `api-dev` (alphabetically first).

#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::path::{Path, PathBuf};
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
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".ato")
        .join("test-scratch");
    fs::create_dir_all(&root).expect("create workspace .ato/test-scratch");
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(root)
        .expect("create workspace tempdir")
}

/// Run a git command in `dir` with isolated config.
#[cfg(unix)]
fn git_in(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@test.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@test.com")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("git command");
    if !out.status.success() {
        eprintln!(
            "git {:?} in {} failed: {}",
            args,
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Create `api/` and `web/` sub-repos under `root`, each with a `package.json`
/// `dev` script and a standalone git repo with a remote.
/// The workspace root itself has no `.git` so it is not detected as a source.
#[cfg(unix)]
fn setup_encap_workspace(root: &Path) {
    for name in ["api", "web"] {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("package.json"),
            format!(
                r#"{{"name":"{}","version":"1.0.0","scripts":{{"dev":"echo {}-server"}}}}"#,
                name, name
            ),
        )
        .unwrap();
        git_in(&dir, &["init"]);
        git_in(
            &dir,
            &[
                "remote",
                "add",
                "origin",
                &format!("https://github.com/test/{}", name),
            ],
        );
        git_in(&dir, &["add", "."]);
        git_in(&dir, &["commit", "-m", "init"]);
    }
}

/// Parse `share.spec.json` from `.ato/share/` in the workspace.
#[cfg(unix)]
fn read_spec(workspace: &Path) -> serde_json::Value {
    let spec_path = workspace.join(".ato").join("share").join("share.spec.json");
    let contents = fs::read_to_string(&spec_path)
        .unwrap_or_else(|e| panic!("read share.spec.json at {}: {}", spec_path.display(), e));
    serde_json::from_str(&contents).expect("parse share.spec.json")
}

/// E2E-7: Accept all detected items; verify saved spec keeps exactly one primary entry.
#[cfg(unix)]
#[test]
#[serial]
fn test_e2e_7_bulk_accept_persists_single_primary() {
    let tmp = workspace_tempdir("e2e7-primary-switch-");
    let tmp_home = workspace_tempdir("e2e7-home-");
    setup_encap_workspace(tmp.path());

    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_ato"));
    cmd.args(["--json", "encap", "--save-config", "."])
        .current_dir(tmp.path())
        .env("HOME", tmp_home.path());

    let mut session = Session::spawn(cmd).expect("spawn encap PTY");
    session.set_expect_timeout(Some(Duration::from_secs(60)));

    session
        .expect("Accept all? [Enter]  or  skip <ids>:  ")
        .expect("bulk accept prompt");
    session.send_line("").expect("accept all");

    session.expect(Eof).ok();

    // Verify saved spec.
    let spec = read_spec(tmp.path());
    let entries = spec["entries"].as_array().expect("entries array");

    let primary_count = entries.iter().filter(|e| e["primary"] == true).count();
    assert_eq!(primary_count, 1, "exactly one entry should be primary");
    assert!(
        entries
            .iter()
            .any(|e| e["id"].as_str() == Some("api-dev") && e["primary"] == true),
        "api-dev should remain the default primary after accepting all"
    );
}

/// E2E-8: Skip the default primary entry and verify the remaining entry becomes primary.
#[cfg(unix)]
#[test]
#[serial]
fn test_e2e_8_skip_default_primary_promotes_remaining_entry() {
    let tmp = workspace_tempdir("e2e8-zero-primary-");
    let tmp_home = workspace_tempdir("e2e8-home-");
    setup_encap_workspace(tmp.path());

    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_ato"));
    cmd.args(["--json", "encap", "--save-config", "."])
        .current_dir(tmp.path())
        .env("HOME", tmp_home.path());

    let mut session = Session::spawn(cmd).expect("spawn encap PTY");
    session.set_expect_timeout(Some(Duration::from_secs(60)));

    session
        .expect("Accept all? [Enter]  or  skip <ids>:  ")
        .expect("bulk accept prompt");
    session.send_line("skip api-dev").expect("skip api-dev");

    session.expect(Eof).ok();

    // Verify saved spec.
    let spec = read_spec(tmp.path());
    let entries = spec["entries"].as_array().expect("entries array");

    let web = entries
        .iter()
        .find(|e| e["id"].as_str() == Some("web-dev"))
        .expect("web-dev entry");

    assert_eq!(
        web["primary"].as_bool(),
        Some(true),
        "web-dev should be primary after api-dev is skipped"
    );
    assert!(
        entries.iter().all(|e| e["id"].as_str() != Some("api-dev")),
        "api-dev should be removed by the skip filter"
    );

    let primary_count = entries.iter().filter(|e| e["primary"] == true).count();
    assert_eq!(primary_count, 1, "exactly one entry should be primary");
}
