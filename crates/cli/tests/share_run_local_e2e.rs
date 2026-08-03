//! E2E regression: `ato run <local share file>` on a clean ATO_HOME.
//!
//! The web share flow was removed and the hidden `ato decap` alias deleted;
//! the capsule ShareExecutor must now materialize via `ato workspace setup
//! --dev` (which runs captured `install_steps`), not via the removed
//! subcommand. This test pins that contract on a fresh ATO_HOME so the
//! materialization cache can never mask a regression to `decap`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use serial_test::serial;
use tempfile::TempDir;

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

/// Write a minimal share.spec.json + share.lock.json pair with one install step
/// and one runnable entry. `install_marker` is an absolute path the install
/// step touches, proving the step actually ran during materialization.
fn write_share_pair(dir: &Path, install_marker: &Path) {
    let spec = serde_json::json!({
        "schema_version": "1",
        "name": "demo",
        "root": "demo",
        "sources": [],
        "tool_requirements": [],
        "env_requirements": [],
        "install_steps": [{
            "id": "install",
            "cwd": ".",
            "run": format!("echo install-ran > {}", install_marker.display()),
            "depends_on": [],
            "evidence": ["test fixture"]
        }],
        "entries": [{
            "id": "demo",
            "label": "Demo",
            "cwd": ".",
            "run": "echo hello-from-entry",
            "kind": "command",
            "primary": true,
            "depends_on": [],
            "env": { "required": [], "optional": [], "files": [] },
            "evidence": []
        }],
        "services": [],
        "notes": { "team_notes": "" },
        "generated_from": {
            "root_path": "/tmp/demo",
            "captured_at": "2026-01-01T00:00:00Z",
            "host_os": "macos"
        }
    });
    let lock = serde_json::json!({
        "schema_version": "1",
        "spec_digest": "sha256:test-digest",
        "generated_guide_digest": "sha256:test-digest",
        "revision": 1,
        "created_at": "2026-01-01T00:00:00Z",
        "resolved_sources": [],
        "resolved_tools": []
    });
    fs::write(
        dir.join("share.spec.json"),
        serde_json::to_string_pretty(&spec).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.join("share.lock.json"),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();
}

fn run_share_input(input: &Path, home: &Path) -> std::process::Output {
    Command::cargo_bin("ato")
        .expect("ato binary")
        .arg("run")
        .arg(input)
        .arg("--compatibility-fallback")
        .arg("host")
        .env("ATO_HOME", home)
        .output()
        .expect("run ato")
}

/// Retired web share links (`ato.run/s/<id>`) must fail with the migration
/// error, without any network request — for both `ato run` and
/// `ato workspace setup`.
#[test]
#[serial]
fn retired_share_link_errors_are_actionable() {
    let tmp = workspace_tempdir("share-retired-");
    let home = tmp.path().join("home");
    fs::create_dir_all(&home).unwrap();

    for url in [
        "https://ato.run/s/abc123",
        "https://staging.ato.run/s/abc123",
        "ato.run/s/abc123@r3",
    ] {
        let run_out = Command::cargo_bin("ato")
            .expect("ato binary")
            .arg("run")
            .arg(url)
            .env("ATO_HOME", &home)
            .output()
            .expect("run ato");
        let run_text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&run_out.stdout),
            String::from_utf8_lossy(&run_out.stderr)
        );
        assert!(
            run_text.contains("retired") && run_text.contains("share.spec.json"),
            "ato run {url} must explain the migration, got:\n{run_text}"
        );
        assert!(
            !run_text.contains("decap"),
            "must not reference decap:\n{run_text}"
        );

        let setup_out = Command::cargo_bin("ato")
            .expect("ato binary")
            .args(["workspace", "setup", url, "--into"])
            .arg(tmp.path().join("into"))
            .env("ATO_HOME", &home)
            .output()
            .expect("run ato workspace setup");
        let setup_text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&setup_out.stdout),
            String::from_utf8_lossy(&setup_out.stderr)
        );
        assert!(
            setup_text.contains("retired") && setup_text.contains("share.spec.json"),
            "ato workspace setup {url} must explain the migration, got:\n{setup_text}"
        );
    }
}

/// `ato run <share.spec.json>` must materialize via `ato workspace setup --dev`
/// on a clean ATO_HOME: the install step runs and the entry produces output.
#[test]
#[serial]
fn ato_run_share_spec_runs_install_steps_and_entry_on_clean_home() {
    let tmp = workspace_tempdir("share-run-spec-");
    let share_dir = tmp.path().join("share");
    let home = tmp.path().join("home");
    let marker = tmp.path().join("install-marker.txt");
    fs::create_dir_all(&share_dir).unwrap();
    fs::create_dir_all(&home).unwrap();
    write_share_pair(&share_dir, &marker);

    let spec_input = share_dir.join("share.spec.json");
    let output = run_share_input(&spec_input, &home);

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "ato run share.spec.json failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("hello-from-entry"),
        "entry output missing on stdout:\n{stdout}"
    );
    assert!(
        !stderr.contains("decap"),
        "executor must not invoke the removed `decap` subcommand:\n{stderr}"
    );
    assert!(
        marker.exists(),
        "install step must run during materialization (--dev)"
    );
}

/// Same contract via `share.lock.json` as the input.
#[test]
#[serial]
fn ato_run_share_lock_runs_install_steps_and_entry_on_clean_home() {
    let tmp = workspace_tempdir("share-run-lock-");
    let share_dir = tmp.path().join("share");
    let home = tmp.path().join("home");
    let marker = tmp.path().join("install-marker.txt");
    fs::create_dir_all(&share_dir).unwrap();
    fs::create_dir_all(&home).unwrap();
    write_share_pair(&share_dir, &marker);

    let lock_input = share_dir.join("share.lock.json");
    let output = run_share_input(&lock_input, &home);

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "ato run share.lock.json failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("hello-from-entry"),
        "entry output missing on stdout:\n{stdout}"
    );
    assert!(
        !stderr.contains("decap"),
        "executor must not invoke the removed `decap` subcommand:\n{stderr}"
    );
    assert!(
        marker.exists(),
        "install step must run during materialization (--dev)"
    );
}

/// `ato run <share.spec.json>` must fail cleanly (before any nacelle work) when
/// `ato` is not on PATH and current_exe cannot be pinned — but the key guard is
/// that it never reaches a `decap` invocation. Runs with a bare PATH.
#[test]
#[serial]
fn ato_run_share_never_reaches_decap_even_without_pinned_binary() {
    let tmp = workspace_tempdir("share-run-nopath-");
    let share_dir = tmp.path().join("share");
    let home = tmp.path().join("home");
    let marker = tmp.path().join("install-marker.txt");
    fs::create_dir_all(&share_dir).unwrap();
    fs::create_dir_all(&home).unwrap();
    write_share_pair(&share_dir, &marker);

    let spec_input = share_dir.join("share.spec.json");
    let output = Command::cargo_bin("ato")
        .expect("ato binary")
        .arg("run")
        .arg(&spec_input)
        .arg("--compatibility-fallback")
        .arg("host")
        .env("ATO_HOME", &home)
        .env("PATH", "/nonexistent")
        .output()
        .expect("run ato");

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        !combined.contains("decap"),
        "must not invoke removed `decap`: {combined}"
    );
}
