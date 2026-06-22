//! Integration tests for `ato receipts diff <old> <new>` (#496).
//!
//! Builds synthetic v2 execution receipts via the public `capsule` API,
//! serializes them in the on-disk (bare) receipt shape, and drives the real
//! `ato` binary. No real launch is required.

use std::collections::BTreeMap;
use std::path::Path;

use assert_cmd::Command;
use capsule::execution_identity::{
    CaseSensitivity, DependencyIdentityV2, EnvOrigin, EnvironmentEntry, EnvironmentIdentityV2,
    EnvironmentMode, ExecutionIdentityInputV2, ExecutionReceiptV2, FdLayoutIdentity,
    FilesystemIdentityV2, FilesystemSemantics, LaunchArg, LaunchEntryPoint, LaunchIdentityV2,
    PlatformIdentity, PolicyIdentityV2, ReproducibilityClass, ReproducibilityIdentity,
    RuntimeCompleteness, RuntimeIdentityV2, SourceIdentityV2, SourceProvenance,
    SourceProvenanceKind, SymlinkPolicy, TmpPolicy, Tracked, UlimitIdentity,
    ValueNormalizationStatus,
};
use predicates::prelude::*;
use tempfile::tempdir;

fn base_v2() -> ExecutionReceiptV2 {
    let input = ExecutionIdentityInputV2::new(
        SourceIdentityV2 {
            source_tree_hash: Tracked::known("blake3:source".to_string()),
            manifest_path_role: Tracked::known("workspace:capsule.toml".to_string()),
        },
        SourceProvenance {
            kind: SourceProvenanceKind::Local,
            git_remote: None,
            git_commit: None,
            registry_ref: None,
        },
        DependencyIdentityV2 {
            derivation_hash: Tracked::known("blake3:deriv".to_string()),
            output_hash: Tracked::known("blake3:depout".to_string()),
            derivation_inputs: None,
        },
        RuntimeIdentityV2 {
            declared: Some("python@3".to_string()),
            resolved_ref: Tracked::known("python@3.12.1".to_string()),
            binary_hash: Tracked::known("sha256:uvbinary".to_string()),
            dynamic_linkage: Tracked::known("blake3:dyn".to_string()),
            completeness: RuntimeCompleteness::BinaryWithDynamicClosure,
            platform: PlatformIdentity {
                os: "macos".to_string(),
                arch: "aarch64".to_string(),
                libc: "unknown".to_string(),
            },
            native_inference: None,
        },
        EnvironmentIdentityV2 {
            entries: vec![EnvironmentEntry {
                key: "CONFIG".to_string(),
                value_hash: Tracked::known("blake3:config".to_string()),
                normalization: ValueNormalizationStatus::NoHostPath,
                origin: EnvOrigin::ManifestStatic,
            }],
            fd_layout: Tracked::known(FdLayoutIdentity {
                stdin: "inherited".to_string(),
                stdout: "inherited".to_string(),
                stderr: "inherited".to_string(),
            }),
            umask: Tracked::known("022".to_string()),
            ulimits: Tracked::known(UlimitIdentity {
                limits: BTreeMap::new(),
            }),
            mode: EnvironmentMode::Closed,
            ambient_untracked_keys: Vec::new(),
        },
        FilesystemIdentityV2 {
            view_hash: Tracked::known("blake3:fs".to_string()),
            partial_view_hash: None,
            source_root: Tracked::known("workspace:.".to_string()),
            working_directory: Tracked::known("workspace:.".to_string()),
            readonly_layers: Vec::new(),
            writable_dirs: Vec::new(),
            persistent_state: Vec::new(),
            semantics: FilesystemSemantics {
                case_sensitivity: Tracked::known(CaseSensitivity::Sensitive),
                symlink_policy: Tracked::known(SymlinkPolicy::Preserve),
                tmp_policy: Tracked::known(TmpPolicy::SessionLocal),
            },
        },
        PolicyIdentityV2 {
            network_policy_hash: Tracked::known("blake3:network".to_string()),
            capability_policy_hash: Tracked::known("blake3:capability".to_string()),
            sandbox_policy_hash: Tracked::known("blake3:sandbox".to_string()),
        },
        LaunchIdentityV2 {
            entry_point: LaunchEntryPoint::Command {
                name: "python".to_string(),
            },
            argv: vec![LaunchArg {
                value_hash: Tracked::known("blake3:argv-app".to_string()),
                normalization: ValueNormalizationStatus::NoHostPath,
            }],
            working_directory: Tracked::known("workspace:.".to_string()),
        },
        None,
        ReproducibilityIdentity {
            class: ReproducibilityClass::Pure,
            causes: Vec::new(),
        },
    );
    ExecutionReceiptV2::from_input(input, "2026-06-05T00:00:00Z".to_string()).expect("receipt")
}

fn write_receipt(path: &Path, receipt: &ExecutionReceiptV2) {
    // On-disk receipts are the bare v2 object with a numeric schema_version,
    // which is exactly what the diff loader discriminates on.
    let bytes = serde_json::to_vec_pretty(receipt).expect("serialize receipt");
    std::fs::write(path, bytes).expect("write receipt");
}

#[test]
fn receipts_diff_reports_component_level_changes() {
    let temp = tempdir().expect("tempdir");
    let old_path = temp.path().join("old.json");
    let new_path = temp.path().join("new.json");

    let old = base_v2();
    let mut new = base_v2();
    // Declared change (entrypoint) + resolved change (runtime binary hash).
    new.launch.entry_point = LaunchEntryPoint::Command {
        name: "uvicorn".to_string(),
    };
    new.runtime.binary_hash = Tracked::known("sha256:uvbinary2".to_string());
    write_receipt(&old_path, &old);
    write_receipt(&new_path, &new);

    Command::cargo_bin("ato")
        .expect("binary")
        .args(["receipts", "diff"])
        .arg(&old_path)
        .arg(&new_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("Drift detected"))
        .stdout(predicate::str::contains("DeclaredDrift"))
        .stdout(predicate::str::contains("launch.entry_point"))
        .stdout(predicate::str::contains("ResolvedDrift"))
        .stdout(predicate::str::contains("runtime.binary_hash"));
}

#[test]
fn receipts_diff_identical_reports_no_drift() {
    let temp = tempdir().expect("tempdir");
    let old_path = temp.path().join("old.json");
    let new_path = temp.path().join("new.json");
    write_receipt(&old_path, &base_v2());
    write_receipt(&new_path, &base_v2());

    Command::cargo_bin("ato")
        .expect("binary")
        .args(["receipts", "diff"])
        .arg(&old_path)
        .arg(&new_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("No receipt drift detected"));
}

#[test]
fn receipts_diff_json_output_is_machine_readable() {
    let temp = tempdir().expect("tempdir");
    let old_path = temp.path().join("old.json");
    let new_path = temp.path().join("new.json");

    let old = base_v2();
    let mut new = base_v2();
    new.runtime.binary_hash = Tracked::known("sha256:uvbinary2".to_string());
    write_receipt(&old_path, &old);
    write_receipt(&new_path, &new);

    Command::cargo_bin("ato")
        .expect("binary")
        .args(["receipts", "diff", "--json"])
        .arg(&old_path)
        .arg(&new_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"has_drift\": true"))
        .stdout(predicate::str::contains("resolved-drift"))
        .stdout(predicate::str::contains("runtime.binary_hash"));
}

#[test]
fn receipts_diff_missing_path_fails_readably() {
    let temp = tempdir().expect("tempdir");
    let old_path = temp.path().join("old.json");
    write_receipt(&old_path, &base_v2());
    let missing = temp.path().join("does-not-exist.json");

    Command::cargo_bin("ato")
        .expect("binary")
        .args(["receipts", "diff"])
        .arg(&old_path)
        .arg(&missing)
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to read execution receipt"));
}

#[test]
fn receipts_diff_invalid_json_fails_readably() {
    let temp = tempdir().expect("tempdir");
    let old_path = temp.path().join("old.json");
    let bad_path = temp.path().join("bad.json");
    write_receipt(&old_path, &base_v2());
    std::fs::write(&bad_path, b"{ not valid json").expect("write bad json");

    Command::cargo_bin("ato")
        .expect("binary")
        .args(["receipts", "diff"])
        .arg(&old_path)
        .arg(&bad_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "failed to parse execution receipt",
        ));
}
