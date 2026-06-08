//! Golden-fixture contract tests for the launch-preparation bridge (#581 ↔ #593).
//!
//! These validate the committed fixtures under
//! `tests/fixtures/launch_preparation/` parse back into the typed
//! [`LaunchPreparationBridgeResult`] and honor the boundary guarantees the
//! control plane (ato-api) relies on. The fixtures themselves are generated from
//! a real `prepare_launch` decision — see
//! `regenerate_launch_preparation_bridge_golden_fixtures` in the crate's lib
//! tests. Mirrors the `ccp_fixtures.rs` pattern.

use std::fs;
use std::path::{Path, PathBuf};

use capsule_core::engine::launch_preparation_bridge::LaunchPreparationBridgeResult;
use capsule_core::foundation::install_lifecycle::launch_template::RunnerClass;

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/launch_preparation")
        .join(format!("{name}.json"))
}

fn read_fixture(name: &str) -> (String, LaunchPreparationBridgeResult) {
    let path = fixture_path(name);
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read fixture {}: {err}", path.display()));
    let parsed: LaunchPreparationBridgeResult = serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("parse {} as bridge result: {err}", path.display()));
    (raw, parsed)
}

const FIXTURE_NAMES: &[&str] = &["prepared_managed_runner", "not_prepared_standard_install"];

#[test]
fn every_bridge_fixture_parses() {
    for name in FIXTURE_NAMES {
        let _ = read_fixture(name);
    }
}

#[test]
fn no_bridge_fixture_carries_raw_secrets_or_observed_diagnostics() {
    for name in FIXTURE_NAMES {
        let (raw, _) = read_fixture(name);
        for forbidden in ["hunter2", "password", "swordfish"] {
            assert!(
                !raw.contains(forbidden),
                "fixture {name} must not contain raw secret {forbidden:?}"
            );
        }
        for forbidden in [
            "observed_status",
            "readiness_status",
            "dynamic_port",
            "process_id",
            "container_id",
            "log_cursor",
            "live_route",
        ] {
            assert!(
                !raw.contains(forbidden),
                "fixture {name} must not contain observed/runtime field {forbidden:?}"
            );
        }
    }
}

#[test]
fn prepared_fixture_is_managed_runner_prepare_session_only() {
    let (_, result) = read_fixture("prepared_managed_runner");
    let plan = match result {
        LaunchPreparationBridgeResult::Prepared { plan } => plan,
        other => panic!("expected prepared, got {other:?}"),
    };
    assert_eq!(plan.selected_runner_class, RunnerClass::ManagedRunner);
    assert_ne!(
        plan.requirement_graph_hash, plan.requirement_graph_snapshot_hash,
        "content hash and snapshot hash must stay distinct"
    );
    assert!(plan.install_revision_id.starts_with("rev_"));
    assert!(plan.capsule_instance_key.starts_with("cik_"));
    assert!(plan.execution_id.starts_with("exec_"));
    assert!(matches!(
        plan.prepare_command,
        capsule_core::engine::runner_command::RunnerCommandPayload::PrepareSession { .. }
    ));
}

#[test]
fn not_prepared_fixture_carries_stable_blocker_codes() {
    let (_, result) = read_fixture("not_prepared_standard_install");
    let blockers = match result {
        LaunchPreparationBridgeResult::NotPrepared { blockers } => blockers,
        other => panic!("expected not_prepared, got {other:?}"),
    };
    assert!(!blockers.is_empty(), "not_prepared must list at least one blocker");
    // Every code is from the documented vocabulary (docs/specs/launch-preparation-plan.md).
    const KNOWN: &[&str] = &[
        "reusable_inputs_invalid",
        "launch_template_not_reusable",
        "launch_materialization_failed",
        "prepare_session_command_failed",
        "launch_preparation_unavailable",
    ];
    for b in &blockers {
        assert!(
            KNOWN.contains(&b.code.as_str()),
            "unknown bridge blocker code {:?}",
            b.code
        );
    }
}
