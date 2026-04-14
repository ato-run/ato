//! E2E interactive tests for `ato encap` primary entry selection via PTY.
//!
//! These tests drive `ato --json encap --save-only .` through a PTY session and verify
//! that primary-entry changes are persisted in the saved `share.spec.json`.
//!
//! Prompt patterns used:
//!  - Source/tool/env confirms: `"[Y/n] "` (from `confirm_with_fallback`)
//!  - Install step / entry / service: `"[Y/e/n] "` (from `prompt_editable_entry`)
//!  - Entry-edit sub-prompts: `"blank keeps current): "`
//!  - Primary confirm (inside edit): `"as primary? ["` → always `"[y/N] "`
//!  - Zero-primary chooser: `"Primary entry ["`
//!  - Workspace name: `"Workspace name ["`
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
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".tmp");
    fs::create_dir_all(&root).expect("create workspace .tmp");
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

/// Answer "y" to all `[Y/n]` and `[Y/e/n]` prompts until `before()` contains
/// the string `target_label`. When found, send `final_answer` and return.
///
/// This drains source, tool, (optional env), and install-step prompts before
/// reaching the target entry prompt.
#[cfg(unix)]
fn answer_yes_until(session: &mut impl Expect, target_label: &str, final_answer: &str) {
    loop {
        // All source/tool/env prompts end with "[Y/n] "; install/entry/service with "[Y/e/n] ".
        let caps = session
            .expect(expectrl::Any(["[Y/n] ", "[Y/e/n] "]))
            .unwrap_or_else(|e| panic!("waiting for prompt before '{}': {}", target_label, e));
        let before = String::from_utf8_lossy(caps.before()).to_string();
        if before.contains(target_label) {
            session.send_line(final_answer).expect("send final answer");
            return;
        }
        session.send_line("y").expect("answer yes");
    }
}

/// E2E-7: Edit `web-dev` and mark it as primary; verify saved spec has
/// `web-dev.primary=true`, `api-dev.primary=false`, and exactly one primary entry.
#[cfg(unix)]
#[test]
#[serial]
fn test_e2e_7_primary_entry_switch_persists() {
    let tmp = workspace_tempdir("e2e7-primary-switch-");
    let tmp_home = workspace_tempdir("e2e7-home-");
    setup_encap_workspace(tmp.path());

    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_ato"));
    cmd.args(["--json", "encap", "--save-only", "."])
        .current_dir(tmp.path())
        .env("HOME", tmp_home.path());

    let mut session = Session::spawn(cmd).expect("spawn encap PTY");
    session.set_expect_timeout(Some(Duration::from_secs(60)));

    // Phase 1: Keep api-dev (answer "y"), draining all earlier prompts.
    answer_yes_until(&mut session, "Run entry api-dev", "y");

    // Phase 2: web-dev prompt → edit.
    {
        let caps = session.expect("[Y/e/n] ").expect("web-dev entry prompt");
        let before = String::from_utf8_lossy(caps.before()).to_string();
        assert!(
            before.contains("Run entry web-dev"),
            "expected 'Run entry web-dev', got: {}",
            before
        );
        session.send_line("e").expect("edit web-dev");
    }

    // Phase 3: Sub-prompts — keep defaults (label, cwd, command).
    for _ in 0..3 {
        session
            .expect("blank keeps current): ")
            .expect("edit sub-prompt");
        session.send_line("").expect("keep default");
    }

    // Phase 4: Mark web-dev as primary → "y".
    session.expect("as primary? [").expect("primary prompt");
    session.send_line("y").expect("mark primary");

    // Phase 5: Service prompts ("y") then workspace name (keep default "").
    loop {
        let caps = session
            .expect(expectrl::Any(["Workspace name [", "[Y/e/n] "]))
            .expect("service or name prompt");
        let matched = String::from_utf8_lossy(caps.as_bytes()).to_string();
        if matched.contains("Workspace name") {
            session.send_line("").expect("keep workspace name");
            break;
        }
        session.send_line("y").expect("answer service prompt");
    }

    session.expect(Eof).ok();

    // Verify saved spec.
    let spec = read_spec(tmp.path());
    let entries = spec["entries"].as_array().expect("entries array");

    let api = entries
        .iter()
        .find(|e| e["id"].as_str() == Some("api-dev"))
        .expect("api-dev entry");
    let web = entries
        .iter()
        .find(|e| e["id"].as_str() == Some("web-dev"))
        .expect("web-dev entry");

    assert_eq!(
        web["primary"].as_bool(),
        Some(true),
        "web-dev should be primary after explicit switch"
    );
    assert_eq!(
        api["primary"].as_bool(),
        Some(false),
        "api-dev should no longer be primary"
    );

    let primary_count = entries.iter().filter(|e| e["primary"] == true).count();
    assert_eq!(primary_count, 1, "exactly one entry should be primary");
}

/// E2E-8: Edit `api-dev` and unset its primary, then use the zero-primary chooser
/// to pick `web-dev`. Verify saved spec has `web-dev.primary=true`, `api-dev.primary=false`.
#[cfg(unix)]
#[test]
#[serial]
fn test_e2e_8_zero_primary_choice_persists() {
    let tmp = workspace_tempdir("e2e8-zero-primary-");
    let tmp_home = workspace_tempdir("e2e8-home-");
    setup_encap_workspace(tmp.path());

    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_ato"));
    cmd.args(["--json", "encap", "--save-only", "."])
        .current_dir(tmp.path())
        .env("HOME", tmp_home.path());

    let mut session = Session::spawn(cmd).expect("spawn encap PTY");
    session.set_expect_timeout(Some(Duration::from_secs(60)));

    // Phase 1: Drain pre-entry prompts with "y", then send "e" for api-dev.
    answer_yes_until(&mut session, "Run entry api-dev", "e");

    // Phase 2: api-dev edit sub-prompts — keep defaults.
    for _ in 0..3 {
        session
            .expect("blank keeps current): ")
            .expect("api-dev edit sub-prompt");
        session.send_line("").expect("keep default");
    }

    // Phase 3: Mark api-dev as primary? → "n" to unset.
    session
        .expect("as primary? [")
        .expect("api-dev primary prompt");
    session.send_line("n").expect("unset primary");

    // Phase 4: web-dev entry prompt → "y" (keep, stays primary=false).
    {
        let caps = session.expect("[Y/e/n] ").expect("web-dev entry prompt");
        let before = String::from_utf8_lossy(caps.before()).to_string();
        assert!(
            before.contains("Run entry web-dev"),
            "expected 'Run entry web-dev', got: {}",
            before
        );
        session.send_line("y").expect("keep web-dev");
    }

    // Phase 5: Zero-primary chooser fires. Choose option 2 (web-dev).
    session
        .expect("Primary entry [")
        .expect("zero-primary chooser");
    session.send_line("2").expect("choose web-dev");

    // Phase 6: Service prompts ("y") then workspace name (keep default "").
    loop {
        let caps = session
            .expect(expectrl::Any(["Workspace name [", "[Y/e/n] "]))
            .expect("service or name prompt");
        let matched = String::from_utf8_lossy(caps.as_bytes()).to_string();
        if matched.contains("Workspace name") {
            session.send_line("").expect("keep workspace name");
            break;
        }
        session.send_line("y").expect("answer service prompt");
    }

    session.expect(Eof).ok();

    // Verify saved spec.
    let spec = read_spec(tmp.path());
    let entries = spec["entries"].as_array().expect("entries array");

    let api = entries
        .iter()
        .find(|e| e["id"].as_str() == Some("api-dev"))
        .expect("api-dev entry");
    let web = entries
        .iter()
        .find(|e| e["id"].as_str() == Some("web-dev"))
        .expect("web-dev entry");

    assert_eq!(
        web["primary"].as_bool(),
        Some(true),
        "web-dev should be primary (chosen via zero-primary prompt)"
    );
    assert_eq!(
        api["primary"].as_bool(),
        Some(false),
        "api-dev should not be primary (user explicitly unset it)"
    );

    let primary_count = entries.iter().filter(|e| e["primary"] == true).count();
    assert_eq!(primary_count, 1, "exactly one entry should be primary");
}
