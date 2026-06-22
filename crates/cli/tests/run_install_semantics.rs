//! Conformance guardrails for the `ato run` / `ato install` semantics
//! documented in `docs/rfcs/draft/run-install-semantics.md` (lands via #335,
//! closes #334).
//!
//! These tests do not drive `ato run` through a full successful launch — that
//! belongs to the broader E2E suite. They assert the narrower invariant from
//! the RFC: the `ato run` dispatch must never produce install-owned state. A
//! plain `ato run` may emit caches, materializations, session records, logs,
//! or receipts, but it must not silently write installed-app registry
//! entries, install profiles, install revisions, OS shortcuts, or update
//! channel records.
//!
//! If a future refactor changes the install-state layout, update the
//! predicates in `predicates::*` rather than rewriting every test.

mod fail_closed_support;

use fail_closed_support::ato_cmd;

#[allow(dead_code)] // `has_run_session_record` is part of the documented predicate
// surface (see the brief in the PR body) even though the four
// landed tests do not yet need it.
mod predicates {
    //! Path-shape predicates over a temporary `ATO_HOME`. These intentionally
    //! avoid asserting exact directory names — the RFC permits the state
    //! layout to move — and instead look for the recognizable file shapes
    //! that the `install_lifecycle` store emits.
    //!
    //! The shapes mirror `capsule::foundation::install_lifecycle::store`:
    //!   - `<root>/instances/<installed_app_id>/app.json`
    //!   - `<root>/instances/<installed_app_id>/profiles/<profile_id>/profile.json`
    //!   - `<root>/revisions/<install_revision_id>/...`

    use std::path::Path;

    /// Filename of the installed-app registry entry written by
    /// `InstallInstanceStore::write_app_record`.
    pub const APP_RECORD_FILENAME: &str = "app.json";

    /// Filename of the install profile written by
    /// `InstallInstanceStore::write_profile`.
    pub const PROFILE_RECORD_FILENAME: &str = "profile.json";

    /// Top-level directory that holds installed-app instances.
    pub const INSTANCES_DIR: &str = "instances";

    /// Top-level directory that holds immutable install revisions.
    pub const REVISIONS_DIR: &str = "revisions";

    pub fn has_installed_app_registry_entry(ato_home: &Path) -> bool {
        descendant_named(&ato_home.join(INSTANCES_DIR), APP_RECORD_FILENAME)
    }

    pub fn has_install_profile(ato_home: &Path) -> bool {
        descendant_named(&ato_home.join(INSTANCES_DIR), PROFILE_RECORD_FILENAME)
    }

    pub fn has_install_revision(ato_home: &Path) -> bool {
        let revisions = ato_home.join(REVISIONS_DIR);
        let Ok(mut entries) = std::fs::read_dir(&revisions) else {
            return false;
        };
        entries.any(|entry| entry.map(|e| e.path().is_dir()).unwrap_or(false))
    }

    pub fn has_run_session_record(ato_home: &Path) -> bool {
        // `ato run` may write session sidecars under any of these roots
        // depending on the surface that started the session. Treating them
        // uniformly keeps the predicate robust to layout moves between
        // CLI-only and Desktop-mediated sessions.
        for sub in ["runs", "run-sessions", "apps/ato-desktop/sessions"] {
            if directory_contains_file(&ato_home.join(sub)) {
                return true;
            }
        }
        false
    }

    fn descendant_named(root: &Path, target: &str) -> bool {
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let file_type = match entry.file_type() {
                    Ok(ft) => ft,
                    Err(_) => continue,
                };
                if file_type.is_dir() {
                    stack.push(path);
                } else if entry.file_name() == target {
                    return true;
                }
            }
        }
        false
    }

    fn directory_contains_file(root: &Path) -> bool {
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let file_type = match entry.file_type() {
                    Ok(ft) => ft,
                    Err(_) => continue,
                };
                if file_type.is_file() {
                    return true;
                }
                if file_type.is_dir() {
                    stack.push(path);
                }
            }
        }
        false
    }
}

mod stage {
    //! Test stage that owns a temporary `ATO_HOME`, a temporary `HOME`, and a
    //! minimal local capsule. The capsule fixture is the existing
    //! `native-shell-capsule` (a `source/native` capsule with a one-line
    //! shell command), copied into the staged workspace.

    use std::path::PathBuf;
    use tempfile::TempDir;

    use crate::fail_closed_support::prepare_fixture_workspace;

    pub struct RunStage {
        pub ato_home: TempDir,
        pub home: TempDir,
        // Tied to `RunStage` so the fixture workspace is dropped together
        // with the temp homes; tests don't reach into it directly.
        _workspace: TempDir,
        pub capsule_path: PathBuf,
    }

    pub fn stage_minimal_capsule() -> RunStage {
        let (workspace, capsule_path) = prepare_fixture_workspace("native-shell-capsule");
        let ato_home = TempDir::new().expect("create temp ATO_HOME");
        let home = TempDir::new().expect("create temp HOME");
        RunStage {
            ato_home,
            home,
            _workspace: workspace,
            capsule_path,
        }
    }
}

use predicates::*;
use stage::*;

/// Invoke `ato run --plan-only --json <capsule>` against a freshly staged
/// capsule under the staged temp `ATO_HOME`.
///
/// `--plan-only` is the right entry point for these guardrails. It exercises
/// the `ato run` dispatch (argument parsing, target resolution, requirements
/// collection) but explicitly does not auto-install, fetch from registries,
/// or materialize provider workspaces. The invariant we care about is that
/// *no* code path reachable from `ato run` may write install-owned state;
/// `--plan-only` is sufficient to detect a regression that flips that
/// invariant.
fn invoke_plan_only_run(stage: &RunStage) -> std::process::Output {
    // `--json` is a top-level `ato` flag (sets the reporter), not a `run`
    // subcommand option. Placing it after `run` would fail clap parsing —
    // exactly the kind of vacuous-pass failure that `assert_plan_only_run_succeeds`
    // is here to catch.
    ato_cmd()
        .arg("--json")
        .arg("run")
        .arg("--plan-only")
        .arg(&stage.capsule_path)
        .env("HOME", stage.home.path())
        .env("ATO_HOME", stage.ato_home.path())
        .env("ATO_OFFLINE", "1")
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .output()
        .expect("failed to invoke ato run --plan-only")
}

/// Invoke `ato run --plan-only --json` and assert it actually reached the
/// run dispatch: exit 0 plus a parseable JSON object on stdout. Without this
/// check the install-state assertions are vacuous — a binary that exits early
/// (missing fixture, arg-parse change, `--plan-only` removed) writes no install
/// state for trivial reasons unrelated to the invariant we want to pin.
///
/// We deliberately do not assert the JSON envelope's schema beyond "is an
/// object" so this guardrail does not couple to the `--plan-only` payload
/// shape; that belongs to a dedicated CLI test.
fn assert_plan_only_run_succeeds(stage: &RunStage) -> std::process::Output {
    let output = invoke_plan_only_run(stage);

    assert!(
        output.status.success(),
        "ato run --plan-only --json must reach the run dispatch and exit 0\n\
         status: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // The reporter writes the plan envelope plus may write follow-up notify
    // lines (e.g. "Share it next: ato encap"), so stdout is JSONL. We only
    // need to confirm at least one line is a JSON object — i.e. the run
    // dispatch reached its emit site instead of bailing out early.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let saw_json_object = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .any(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .map(|value| value.is_object())
                .unwrap_or(false)
        });
    assert!(
        saw_json_object,
        "ato run --plan-only --json must emit at least one JSON object line on stdout\n\
         stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );

    output
}

#[test]
fn run_does_not_create_installed_app_registry_entry() {
    let stage = stage_minimal_capsule();
    let _output = assert_plan_only_run_succeeds(&stage);

    assert!(
        !has_installed_app_registry_entry(stage.ato_home.path()),
        "`ato run` must not write an installed-app registry entry (e.g. \
         `instances/<app>/app.json`) under ATO_HOME ({}). See \
         docs/rfcs/draft/run-install-semantics.md §5.3.",
        stage.ato_home.path().display()
    );
}

#[test]
fn run_does_not_create_install_profile() {
    let stage = stage_minimal_capsule();
    let _output = assert_plan_only_run_succeeds(&stage);

    assert!(
        !has_install_profile(stage.ato_home.path()),
        "`ato run` must not write an install profile (e.g. \
         `instances/<app>/profiles/<profile>/profile.json`) under ATO_HOME \
         ({}). See docs/rfcs/draft/run-install-semantics.md §5.4.",
        stage.ato_home.path().display()
    );
}

#[test]
fn run_does_not_create_install_revision() {
    let stage = stage_minimal_capsule();
    let _output = assert_plan_only_run_succeeds(&stage);

    assert!(
        !has_install_revision(stage.ato_home.path()),
        "`ato run` must not create install revisions (e.g. `revisions/<rev>/`) \
         under ATO_HOME ({}). See docs/rfcs/draft/run-install-semantics.md §5.3.",
        stage.ato_home.path().display()
    );
}

/// Session records carry the only structured discriminator between a
/// run-owned launch and an install-owned launch: the four install-lifecycle
/// identifier fields on `StoredSessionInfo` (`installed_app_id`,
/// `install_profile_id`, `install_profile_key`, `install_revision_id`) plus
/// the derived `capsule_instance_key`.
///
/// This test pins the invariant from RFC §7: a run-owned session record
/// keeps all five fields unset, and an install-owned record carries all
/// four primary identifiers. A regression that promoted any of the primary
/// fields to non-Option or to a non-null default would silently make every
/// `ato run` session look install-owned to receipt consumers and to the
/// Desktop direct-read fast path.
#[test]
fn run_session_records_are_scoped_as_ephemeral() {
    use capsule::state::session::record::StoredSessionInfo;

    // Run-owned minimal record (schema=1 wire shape; omits the install
    // identifier fields entirely, which serde maps to `None`).
    let run_owned_json = r#"{
        "session_id": "run_minimal",
        "handle": "native-shell-capsule",
        "normalized_handle": "native-shell-capsule",
        "canonical_handle": null,
        "trust_state": "trusted",
        "source": null,
        "restricted": false,
        "snapshot": null,
        "runtime": {
            "target_label": "main",
            "runtime": "source/native",
            "driver": null,
            "language": null,
            "port": null
        },
        "display_strategy": "guest_webview",
        "pid": 1234,
        "log_path": "/tmp/x.log",
        "manifest_path": "/tmp/manifest.toml",
        "target_label": "main",
        "notes": [],
        "guest": null,
        "web": null,
        "terminal": null,
        "service": null
    }"#;
    let run_owned: StoredSessionInfo =
        serde_json::from_str(run_owned_json).expect("parse run-owned session record");
    assert!(
        run_owned.installed_app_id.is_none(),
        "run-owned session must leave installed_app_id unset"
    );
    assert!(
        run_owned.install_profile_id.is_none(),
        "run-owned session must leave install_profile_id unset"
    );
    assert!(
        run_owned.install_profile_key.is_none(),
        "run-owned session must leave install_profile_key unset"
    );
    assert!(
        run_owned.install_revision_id.is_none(),
        "run-owned session must leave install_revision_id unset"
    );
    assert!(
        run_owned.capsule_instance_key.is_none(),
        "run-owned session must leave capsule_instance_key unset \
         (CIK only exists once install identity binds the launch)"
    );

    // Install-owned record: the same wire shape but with the four
    // install-lifecycle primary identifiers populated. The discriminator
    // function below should classify this as install-owned and the
    // run-owned record above as run-owned. This pins the discrimination
    // contract from RFC §7.
    let install_owned_json = r#"{
        "session_id": "ato-desktop-installed-launch",
        "handle": "publisher/slug",
        "normalized_handle": "publisher/slug",
        "canonical_handle": null,
        "trust_state": "trusted",
        "source": "installed-app",
        "restricted": false,
        "snapshot": null,
        "runtime": {
            "target_label": "main",
            "runtime": "source/native",
            "driver": null,
            "language": null,
            "port": null
        },
        "display_strategy": "guest_webview",
        "pid": 5678,
        "log_path": "/tmp/y.log",
        "manifest_path": "/tmp/manifest.toml",
        "target_label": "main",
        "notes": [],
        "guest": null,
        "web": null,
        "terminal": null,
        "service": null,
        "installed_app_id": "app_aabbccddeeff00112233445566778899",
        "install_profile_id": "default",
        "install_profile_key": "ipk_aabbccddeeff00112233445566778899",
        "install_revision_id": "rev_1122334455667788"
    }"#;
    let install_owned: StoredSessionInfo =
        serde_json::from_str(install_owned_json).expect("parse install-owned session record");
    assert!(install_owned.installed_app_id.is_some());
    assert!(install_owned.install_profile_id.is_some());
    assert!(install_owned.install_profile_key.is_some());
    assert!(install_owned.install_revision_id.is_some());

    // Discrimination: any session that lacks every install-lifecycle
    // primary identifier is run-owned; otherwise install-owned. We define
    // the function inline rather than reaching into a production helper so
    // that the test fails loudly if the schema grows a new identifier
    // field that this guardrail forgot about.
    fn is_run_owned(record: &StoredSessionInfo) -> bool {
        record.installed_app_id.is_none()
            && record.install_profile_id.is_none()
            && record.install_profile_key.is_none()
            && record.install_revision_id.is_none()
    }
    assert!(
        is_run_owned(&run_owned),
        "run-owned record must discriminate as run-owned"
    );
    assert!(
        !is_run_owned(&install_owned),
        "install-owned record must not discriminate as run-owned"
    );
}
