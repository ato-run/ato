//! Cross-language vectors for builder-issued Authoring Session evidence.
//!
//! Consumers verify the signature over the exact decoded payload bytes. They
//! must not reconstruct these bytes with a second canonicalization
//! implementation.

use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use snapshot::authoring_evidence::{CLEAN_REPLAY_RECEIPT_V1_SCHEMA, CleanReplayReceiptPayloadV1};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/authoring_evidence_v1")
}

#[test]
fn clean_replay_payload_vector_is_exact_jcs() {
    let mut expected =
        fs::read(fixture_dir().join("clean-replay-payload.canonical.json")).expect("read vector");
    assert_eq!(
        expected.pop(),
        Some(b'\n'),
        "repository JSON fixture uses one transport newline"
    );
    if expected.last() == Some(&b'\r') {
        expected.pop();
    }
    let payload: CleanReplayReceiptPayloadV1 =
        serde_json::from_slice(&expected).expect("parse vector");

    assert_eq!(payload.schema, CLEAN_REPLAY_RECEIPT_V1_SCHEMA);
    assert_eq!(
        serde_jcs::to_vec(&payload).expect("canonicalize typed payload"),
        expected,
        "Rust SSOT canonical payload drifted"
    );

    let wire = BASE64.encode(&expected);
    assert_eq!(
        BASE64.decode(wire).expect("decode wire payload"),
        expected,
        "wire envelope must preserve the exact signed bytes"
    );
}
