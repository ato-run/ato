//! Shared Rust/TypeScript vectors for the source receipts.
//!
//! The digest is over exact bytes and the verifier lives in another repository
//! and another language. Describing the format ("sorted keys", "RFC 8785
//! compatible") cannot establish that two implementations agree — only the same
//! bytes, checked by both, can. So these fixtures record the input object, the
//! expected canonical bytes and the expected digest, and `apps/ato-api` runs
//! its own test over the identical files.
//!
//! A vector corpus that lives in two repositories drifts. That has already
//! happened once here: the execution-contract corpus went 85 -> 78 vectors
//! between the two repos with CI green throughout, because the guard checked
//! only the manifest rather than the set. `manifest.json` therefore carries a
//! `vector_count` and both sides assert it.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use snapshot::source_receipt::{SourceMaterializationReceiptV1, SourceReceiptV1};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/source_receipt")
}

#[derive(Debug, Deserialize)]
struct Manifest {
    schema: String,
    digest_algorithm: String,
    domain_separator: Option<String>,
    vector_count: usize,
    vectors: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Vector {
    description: String,
    kind: String,
    receipt: serde_json::Value,
    canonical_json: String,
    digest: String,
}

fn load_manifest() -> Manifest {
    let raw = fs::read_to_string(fixtures().join("manifest.json")).expect("read manifest");
    serde_json::from_str(&raw).expect("parse manifest")
}

#[test]
fn every_vector_reproduces_its_recorded_bytes_and_digest() {
    let manifest = load_manifest();
    assert_eq!(
        manifest.vectors.len(),
        manifest.vector_count,
        "manifest vector_count disagrees with the list — the guard that stops \
         the corpus silently shrinking"
    );

    for name in &manifest.vectors {
        let raw = fs::read_to_string(fixtures().join(format!("{name}.json")))
            .unwrap_or_else(|e| panic!("read vector {name}: {e}"));
        let vector: Vector =
            serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse vector {name}: {e}"));

        let (canonical, digest) = match vector.kind.as_str() {
            "source-receipt" => {
                let receipt: SourceReceiptV1 = serde_json::from_value(vector.receipt.clone())
                    .unwrap_or_else(|e| panic!("{name}: {e}"));
                (receipt.canonical_json(), receipt.digest())
            }
            "source-materialization-receipt" => {
                let receipt: SourceMaterializationReceiptV1 =
                    serde_json::from_value(vector.receipt.clone())
                        .unwrap_or_else(|e| panic!("{name}: {e}"));
                (receipt.canonical_json(), receipt.digest())
            }
            other => panic!("{name}: unknown vector kind {other}"),
        };

        assert_eq!(
            canonical, vector.canonical_json,
            "{name} ({}): canonical bytes differ",
            vector.description
        );
        assert_eq!(
            digest, vector.digest,
            "{name} ({}): digest differs",
            vector.description
        );
    }
}

/// The manifest states the digest rule, and the rule is that there is NO domain
/// separator. Pinned here so a future reader cannot assume the repo-wide
/// `schema_domained_blake3_id` habit applies.
#[test]
fn the_manifest_records_an_undomained_blake3_digest() {
    let manifest = load_manifest();
    assert_eq!(manifest.schema, "ato.source-receipt-vectors/v1");
    assert_eq!(manifest.digest_algorithm, "blake3");
    assert_eq!(
        manifest.domain_separator, None,
        "these digests are NOT domain-separated; the merged TypeScript verifier \
         does not domain-separate either"
    );
}

/// Every vector file on disk is listed. Catches a vector added to the directory
/// but never registered — which would be a case nobody runs.
#[test]
fn no_vector_file_is_unlisted() {
    let manifest = load_manifest();
    let mut found: Vec<String> = fs::read_dir(fixtures())
        .expect("read fixtures dir")
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.strip_suffix(".json")
                .filter(|n| *n != "manifest")
                .map(|n| n.to_string())
        })
        .collect();
    found.sort();
    let mut listed = manifest.vectors.clone();
    listed.sort();
    assert_eq!(found, listed, "a vector file is not listed in the manifest");
}
