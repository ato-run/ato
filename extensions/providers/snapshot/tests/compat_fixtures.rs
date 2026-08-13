//! Snapshot v1 Compatibility Suite — KVM-free eligibility enforcement.
//!
//! Walks every fixture under `tools/snapshot-builder/fixtures/compat/` and
//! asserts the `derive_build_spec` verdict against the fixture's
//! `expected.json`. This is the CI-enforced half of the contract in
//! `docs/snapshot-v1-compatibility.md` §4; the seal-side expectations
//! (`seal`, `seal_failure_stage`) are consumed by the staging API E2E.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use capsule::foundation::types::manifest::CapsuleManifest;
use serde::Deserialize;
use snapshot::rootfs_builder::{SourceProbe, derive_build_spec};

/// The contract's fixture matrix (docs/snapshot-v1-compatibility.md §4),
/// verbatim. Completeness is asserted both ways: a fixture dir without a
/// contract row, or a contract row without a dir, fails this suite.
const CONTRACT_FIXTURES: &[&str] = &[
    // positive
    "static-web-basic",
    "python-stdlib-explicit",
    "python-bare-port-only",
    "python-requirements-flask",
    "node-express-basic",
    "node-port-only",
    "store-recipe-manifest-only",
    "real-store-receipt-to-csv",
    // negative
    "missing-port",
    "missing-run",
    "secret-required",
    "user-files-binding",
    "oauth-binding",
    "external-db-required",
    "gpu-required",
    "localhost-only-bind",
    "synthesized-root-404",
    "pem-marker-in-library",
    "planted-builder-token",
];

#[derive(Debug, Deserialize)]
struct Expected {
    class: String,
    eligibility: String,
    #[serde(default)]
    eligibility_reason_contains: Option<String>,
    #[serde(default)]
    runtime: Option<String>,
    #[serde(default)]
    probe_synthesized: Option<bool>,
    #[serde(default)]
    start_cmd: Option<String>,
    seal: String,
    #[serde(default)]
    seal_failure_stage: Option<String>,
    manifest_source: String,
}

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tools/snapshot-builder/fixtures/compat")
}

fn load_expected(dir: &Path) -> Expected {
    let raw = std::fs::read_to_string(dir.join("expected.json"))
        .unwrap_or_else(|e| panic!("{}: expected.json unreadable: {e}", dir.display()));
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("{}: expected.json invalid: {e}", dir.display()))
}

/// The fixture's manifest text: `capsule.toml`, or `store-recipe.toml` for the
/// recipe-as-manifest shape (the claim carries it; the builder writes it as
/// `capsule.toml`, so eligibility-wise they are the same input).
fn manifest_text(dir: &Path) -> Option<String> {
    for name in ["capsule.toml", "store-recipe.toml"] {
        if let Ok(s) = std::fs::read_to_string(dir.join(name)) {
            return Some(s);
        }
    }
    None
}

#[test]
fn contract_table_and_fixture_dirs_match_exactly() {
    let root = fixtures_root();
    let on_disk: BTreeSet<String> = std::fs::read_dir(&root)
        .unwrap_or_else(|e| panic!("{}: {e}", root.display()))
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let contract: BTreeSet<String> = CONTRACT_FIXTURES.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        on_disk, contract,
        "fixture dirs and the contract matrix drifted (extra dirs need a contract \
         row in docs/snapshot-v1-compatibility.md; removed rows need the dir deleted)"
    );
}

#[test]
fn every_fixture_matches_its_eligibility_expectation() {
    for name in CONTRACT_FIXTURES {
        let dir = fixtures_root().join(name);
        let expected = load_expected(&dir);

        assert!(
            matches!(expected.class.as_str(), "positive" | "negative"),
            "{name}: bad class {:?}",
            expected.class
        );
        assert!(
            matches!(expected.seal.as_str(), "sealed" | "failed"),
            "{name}: bad seal {:?}",
            expected.seal
        );

        // External anchors carry no in-tree manifest; the API E2E owns them.
        if expected.manifest_source == "external" {
            assert!(
                manifest_text(&dir).is_none(),
                "{name}: external fixture must not ship a manifest"
            );
            continue;
        }

        let toml = manifest_text(&dir)
            .unwrap_or_else(|| panic!("{name}: no capsule.toml / store-recipe.toml"));
        let manifest = CapsuleManifest::from_toml(&toml)
            .unwrap_or_else(|e| panic!("{name}: manifest parse failed: {e}"));
        let probe = SourceProbe::scan(&dir);

        match (
            expected.eligibility.as_str(),
            derive_build_spec(&manifest, &probe),
        ) {
            ("pass", Ok(spec)) => {
                if let Some(runtime) = &expected.runtime {
                    let got = serde_json::to_value(spec.runtime).unwrap();
                    assert_eq!(
                        got.as_str(),
                        Some(runtime.as_str()),
                        "{name}: runtime detection"
                    );
                }
                if let Some(synth) = expected.probe_synthesized {
                    assert_eq!(spec.probe_synthesized, synth, "{name}: probe_synthesized");
                }
                if let Some(cmd) = &expected.start_cmd {
                    assert_eq!(&spec.start_cmd, cmd, "{name}: start_cmd");
                }
                // A fixture that passes eligibility but expects a failed seal
                // must name the later stage the API E2E asserts on.
                if expected.seal == "failed" {
                    assert!(
                        expected
                            .seal_failure_stage
                            .as_deref()
                            .is_some_and(|s| s != "eligibility"),
                        "{name}: eligible fixture with seal=failed must pin a post-eligibility stage"
                    );
                }
            }
            ("fail", Err(reason)) => {
                let needle = expected
                    .eligibility_reason_contains
                    .as_deref()
                    .unwrap_or_else(|| {
                        panic!("{name}: eligibility=fail needs eligibility_reason_contains")
                    });
                assert!(
                    reason.contains(needle),
                    "{name}: rejection {reason:?} does not contain {needle:?} — \
                     the contract's documented reason drifted"
                );
                assert_eq!(
                    expected.seal_failure_stage.as_deref(),
                    Some("eligibility"),
                    "{name}: eligibility-failing fixture must expect failure_stage=eligibility"
                );
            }
            ("pass", Err(reason)) => panic!("{name}: expected eligible, rejected: {reason}"),
            ("fail", Ok(_)) => panic!("{name}: expected rejection, but derive_build_spec passed"),
            (other, _) => panic!("{name}: bad eligibility {other:?}"),
        }
    }
}
