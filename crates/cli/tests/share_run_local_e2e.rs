//! E2E regression: `ato run <local share file>` on a clean ATO_HOME.
//!
//! The web share flow was removed and the hidden `ato decap` alias deleted;
//! the capsule ShareExecutor must now materialize via `ato workspace setup
//! --dev` (which runs captured `install_steps`), not via the removed
//! subcommand. This file pins the following contracts on a fresh ATO_HOME so
//! neither the materialization cache nor a stale fixture can mask a regression:
//!
//!   1. a digest-matching spec/lock pair runs install steps and the entry
//!   2. a spec/lock digest mismatch is rejected before the entry executes
//!   3. `./share.spec.json` from different projects never aliases each other
//!   4. updating the share files at the same path never reuses stale content

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use capsule::share::types::ShareSpec;
use serial_test::serial;
use sha2::{Digest, Sha256};
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

/// Compute the exact `spec_digest` the CLI records in the lock: SHA-256 over the
/// canonical typed `ShareSpec` serialization (mirrors `load_share_input`).
fn spec_digest(spec_value: &serde_json::Value) -> String {
    let spec: ShareSpec = serde_json::from_value(spec_value.clone()).expect("spec parses");
    let canonical = serde_json::to_vec(&spec).expect("spec serializes");
    format!("sha256:{:x}", Sha256::digest(&canonical))
}

/// Build a minimal valid spec value with one install step and one entry.
fn spec_value(entry_output: &str, install_marker: &Path) -> serde_json::Value {
    serde_json::json!({
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
            "run": format!("echo {entry_output}"),
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
    })
}

/// Write a digest-consistent `share.spec.json` + `share.lock.json` pair.
fn write_share_pair(dir: &Path, entry_output: &str, install_marker: &Path) {
    let spec = spec_value(entry_output, install_marker);
    let digest = spec_digest(&spec);
    let lock = serde_json::json!({
        "schema_version": "1",
        "spec_digest": digest,
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

/// Write a pair whose lock digest matches `entry_output_a`, then overwrite the
/// spec with `entry_output_b` so the pair is now inconsistent (stale lock).
fn write_mismatched_pair(dir: &Path, entry_output_a: &str, entry_output_b: &str, marker: &Path) {
    let spec = spec_value(entry_output_a, marker);
    let digest = spec_digest(&spec);
    let lock = serde_json::json!({
        "schema_version": "1",
        "spec_digest": digest,
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
    // Now replace the spec with different content — the lock digest is stale.
    let spec_b = spec_value(entry_output_b, marker);
    fs::write(
        dir.join("share.spec.json"),
        serde_json::to_string_pretty(&spec_b).unwrap(),
    )
    .unwrap();
}

fn run_share_input_from(input: &str, cwd: &Path, home: &Path) -> std::process::Output {
    Command::cargo_bin("ato")
        .expect("ato binary")
        .current_dir(cwd)
        .arg("run")
        .arg(input)
        .arg("--compatibility-fallback")
        .arg("host")
        .env("ATO_HOME", home)
        .output()
        .expect("run ato")
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
    write_share_pair(&share_dir, "hello-from-spec", &marker);

    let output = run_share_input_from(
        &share_dir.join("share.spec.json").display().to_string(),
        tmp.path(),
        &home,
    );

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "ato run share.spec.json failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("hello-from-spec"),
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
    write_share_pair(&share_dir, "hello-from-lock", &marker);

    let output = run_share_input_from(
        &share_dir.join("share.lock.json").display().to_string(),
        tmp.path(),
        &home,
    );

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "ato run share.lock.json failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("hello-from-lock"),
        "entry output missing on stdout:\n{stdout}"
    );
    assert!(
        marker.exists(),
        "install step must run during materialization (--dev)"
    );
}

/// A spec/lock digest mismatch must be rejected BEFORE any entry execution —
/// for both `share.spec.json` and `share.lock.json` inputs. No install step
/// runs and no entry output is produced.
#[test]
#[serial]
fn mismatched_spec_digest_is_rejected_before_entry() {
    for input_name in ["share.spec.json", "share.lock.json"] {
        let tmp = workspace_tempdir(&format!("share-mismatch-{input_name}"));
        let share_dir = tmp.path().join("share");
        let home = tmp.path().join("home");
        let marker = tmp.path().join("install-marker.txt");
        fs::create_dir_all(&share_dir).unwrap();
        fs::create_dir_all(&home).unwrap();
        // Lock digest matches "GOOD"; spec is then replaced with "EVIL".
        write_mismatched_pair(&share_dir, "GOOD", "EVIL", &marker);

        let input = share_dir.join(input_name);
        let output = run_share_input_from(&input.display().to_string(), tmp.path(), &home);

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let combined = format!("{stdout}\n{stderr}");
        assert!(
            !output.status.success(),
            "{input_name} must fail on digest mismatch"
        );
        assert!(
            combined.contains("does not match") && combined.contains("refusing"),
            "{input_name} mismatch must be rejected explicitly:\n{combined}"
        );
        assert!(
            !combined.contains("EVIL"),
            "{input_name} must not execute the mismatched spec:\n{combined}"
        );
        assert!(
            !marker.exists(),
            "{input_name} must fail before any install step runs"
        );
    }
}

/// `./share.spec.json` from two different projects with the same ATO_HOME must
/// never alias each other — the second run must execute the second project.
#[test]
#[serial]
fn different_cwd_share_files_do_not_alias() {
    let tmp = workspace_tempdir("share-aliasing-");
    let dir_a = tmp.path().join("a");
    let dir_b = tmp.path().join("b");
    let home = tmp.path().join("home");
    fs::create_dir_all(&dir_a).unwrap();
    fs::create_dir_all(&dir_b).unwrap();
    fs::create_dir_all(&home).unwrap();
    write_share_pair(&dir_a, "OUTPUT-A", &tmp.path().join("marker-a.txt"));
    write_share_pair(&dir_b, "OUTPUT-B", &tmp.path().join("marker-b.txt"));

    let run_a = run_share_input_from("./share.spec.json", &dir_a, &home);
    assert!(
        run_a.status.success(),
        "project A must run: {}",
        String::from_utf8_lossy(&run_a.stderr)
    );
    assert!(String::from_utf8_lossy(&run_a.stdout).contains("OUTPUT-A"));

    let run_b = run_share_input_from("./share.spec.json", &dir_b, &home);
    let stdout_b = String::from_utf8_lossy(&run_b.stdout).to_string();
    let stderr_b = String::from_utf8_lossy(&run_b.stderr).to_string();
    assert!(
        run_b.status.success(),
        "project B must run: {stdout_b}\n{stderr_b}"
    );
    assert!(
        stdout_b.contains("OUTPUT-B"),
        "second run must execute project B, not a cached A:\n{stdout_b}\n{stderr_b}"
    );
}

/// Updating the share files at the same path must not reuse a stale cache — the
/// second run must execute the new entry and install step.
#[test]
#[serial]
fn same_path_share_update_is_not_stale() {
    let tmp = workspace_tempdir("share-update-");
    let share_dir = tmp.path().join("share");
    let home = tmp.path().join("home");
    let marker = tmp.path().join("install-marker.txt");
    fs::create_dir_all(&share_dir).unwrap();
    fs::create_dir_all(&home).unwrap();

    write_share_pair(&share_dir, "VERSION-1", &marker);
    let input = share_dir.join("share.spec.json").display().to_string();
    let run1 = run_share_input_from(&input, tmp.path(), &home);
    assert!(run1.status.success(), "run 1 failed");
    assert!(String::from_utf8_lossy(&run1.stdout).contains("VERSION-1"));

    // Replace the pair with new content (fresh digest).
    fs::remove_file(&marker).ok();
    write_share_pair(&share_dir, "VERSION-2", &marker);
    let run2 = run_share_input_from(&input, tmp.path(), &home);
    let stdout2 = String::from_utf8_lossy(&run2.stdout).to_string();
    let stderr2 = String::from_utf8_lossy(&run2.stderr).to_string();
    assert!(
        run2.status.success(),
        "run 2 must succeed: {stdout2}\n{stderr2}"
    );
    assert!(
        stdout2.contains("VERSION-2"),
        "run 2 must execute the updated spec, not a stale cache:\n{stdout2}\n{stderr2}"
    );
    assert!(
        marker.exists(),
        "install step must run for the updated spec"
    );
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
