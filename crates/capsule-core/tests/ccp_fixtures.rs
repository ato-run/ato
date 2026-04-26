//! Consumer-side golden test for the CCP envelopes that `ato-cli` emits.
//!
//! These fixtures are the *only* place the wire shape is recorded. The
//! producer (`crates/ato-cli/src/app_control`) compares its serialized
//! output against the same JSON bytes via `assert_snapshot`, and this
//! test verifies the consumer-facing invariants the Desktop relies on:
//!
//! 1. Every envelope's top-level `schema_version` is the canonical
//!    [`SCHEMA_VERSION`] — i.e., the CLI did not silently downgrade or
//!    bump the wire version.
//! 2. The payload-agnostic [`CcpHeader`] extractor still parses cleanly
//!    against each fixture, which is the contract Desktop's classifier
//!    chain ([`classify_schema_version`] → [`enforce_ccp_compat`])
//!    relies on.
//!
//! Adding a new envelope variant means adding both a producer snapshot
//! test and a fixture filename below; the producer build will fail if
//! the fixture is missing, and this test will fail if its
//! `schema_version` differs from the canonical constant.

use std::fs;
use std::path::Path;

use capsule_core::ccp::{
    classify_schema_version, enforce_ccp_compat, CcpCompat, CcpHeader, HasSchemaVersion,
    SCHEMA_VERSION,
};

const FIXTURE_NAMES: &[&str] = &["bootstrap", "status", "repair"];

fn fixture_path(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ccp")
        .join(format!("{name}.json"))
}

#[test]
fn every_fixture_carries_canonical_schema_version() {
    for name in FIXTURE_NAMES {
        let path = fixture_path(name);
        let raw = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("read fixture {}: {err}", path.display()));
        let header: CcpHeader = serde_json::from_str(&raw)
            .unwrap_or_else(|err| panic!("parse {} as CcpHeader: {err}", path.display()));
        assert_eq!(
            header.schema_version.as_deref(),
            Some(SCHEMA_VERSION),
            "fixture {} carries a non-canonical schema_version",
            path.display()
        );
    }
}

#[test]
fn every_fixture_classifies_as_native_v1() {
    for name in FIXTURE_NAMES {
        let path = fixture_path(name);
        let raw = fs::read_to_string(&path).expect("fixture readable");
        let header: CcpHeader = serde_json::from_str(&raw).expect("CcpHeader parse");
        assert_eq!(
            classify_schema_version(header.schema_version.as_deref()),
            CcpCompat::NativeV1,
            "fixture {} did not classify as NativeV1",
            path.display()
        );
    }
}

#[test]
fn enforce_accepts_every_fixture() {
    struct HeaderEnvelope(CcpHeader);
    impl HasSchemaVersion for HeaderEnvelope {
        fn schema_version(&self) -> Option<&str> {
            self.0.schema_version.as_deref()
        }
    }

    for name in FIXTURE_NAMES {
        let path = fixture_path(name);
        let raw = fs::read_to_string(&path).expect("fixture readable");
        let header: CcpHeader = serde_json::from_str(&raw).expect("CcpHeader parse");
        enforce_ccp_compat(&HeaderEnvelope(header), "fixture")
            .unwrap_or_else(|err| panic!("enforce rejected fixture {}: {err}", path.display()));
    }
}
