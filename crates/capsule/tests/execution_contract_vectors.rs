//! Runner for the shared `ato.execution-contract/v1` test vectors.
//!
//! The fixtures under `tests/fixtures/execution_contract/` are the
//! cross-language source of truth for the canonical form (RFC 8785 JCS +
//! domain-separated BLAKE3). `manifest.json` lists every vector with its
//! expected outcome; other implementations (e.g. ato-api) consume the same
//! files. Invariants exercised here:
//!
//! 1. The baseline contract produces the exact recorded canonical bytes and
//!    `execution_id` (pins the domain separator and canonicalization version).
//! 2. Field order and input whitespace never influence the id.
//! 3. Every identity-bearing field mutation changes the id (and matches its
//!    recorded id exactly, pairwise distinct).
//! 4. Non-identity envelope data — provenance, diagnostics, evidence,
//!    timestamps, runner/session/snapshot/host facts, unknown fields — never
//!    influences the id.
//! 5. Malformed identity input (unknown fields, version mismatch, placeholder
//!    or non-canonical digests, unsorted lists, unresolved launch, and
//!    non-canonical spellings of absent optional fields — explicit `null` /
//!    empty optional collections) and stored `execution_id` mismatches fail
//!    closed.
//! 6. RFC 8785 string escaping is pinned for free-form fields: the
//!    `unicode-strings` vector records exact canonical bytes for non-ASCII
//!    (astral-plane emoji, CJK) and control-character content, whether the
//!    input spells them as `\uXXXX` escapes or literal UTF-8.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use capsule::execution_contract::{
    EXECUTION_CONTRACT_V1_SCHEMA, ExecutionContractEnvelopeV1, ExecutionContractError,
    ExecutionContractV1, ExecutionId,
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: String,
    #[allow(dead_code)]
    description: String,
    domain_separator_utf8: String,
    #[allow(dead_code)]
    execution_id_formula: String,
    #[allow(dead_code)]
    jcs: String,
    #[allow(dead_code)]
    numbers: String,
    #[allow(dead_code)]
    optional_fields: String,
    baseline: String,
    vectors: Vec<Vector>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Vector {
    name: String,
    file: String,
    kind: Kind,
    expect: Expect,
    execution_id: Option<String>,
    relation: Option<Relation>,
    canonical_file: Option<String>,
    #[allow(dead_code)]
    notes: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Kind {
    Contract,
    Envelope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Expect {
    ExecutionId,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Relation {
    EqualsBaseline,
    DiffersFromBaseline,
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/execution_contract")
}

fn load_manifest() -> Manifest {
    let raw = fs::read_to_string(fixture_dir().join("manifest.json")).expect("read manifest");
    serde_json::from_str(&raw).expect("parse manifest")
}

fn compute_contract_id(raw: &str) -> Result<ExecutionId, ExecutionContractError> {
    let contract: ExecutionContractV1 = serde_json::from_str(raw)
        .map_err(|error| ExecutionContractError::Canonicalization(error.to_string()))?;
    contract.compute_execution_id()
}

#[test]
fn shared_vectors_pin_the_canonical_form() {
    let dir = fixture_dir();
    let manifest = load_manifest();
    assert_eq!(manifest.schema, EXECUTION_CONTRACT_V1_SCHEMA);
    assert_eq!(manifest.domain_separator_utf8, EXECUTION_CONTRACT_V1_SCHEMA);

    let baseline = manifest
        .vectors
        .iter()
        .find(|vector| vector.name == manifest.baseline)
        .expect("baseline vector present");
    let baseline_raw = fs::read_to_string(dir.join(&baseline.file)).expect("read baseline");
    let baseline_id = compute_contract_id(&baseline_raw).expect("baseline computes");
    assert_eq!(
        Some(baseline_id.as_str()),
        baseline.execution_id.as_deref(),
        "baseline execution_id drifted — canonicalization or domain separation changed"
    );

    let mut mutated_ids = BTreeSet::new();
    for vector in &manifest.vectors {
        let raw = fs::read_to_string(dir.join(&vector.file))
            .unwrap_or_else(|error| panic!("read vector '{}': {error}", vector.name));

        if let Some(canonical_file) = &vector.canonical_file {
            let contract: ExecutionContractV1 =
                serde_json::from_str(&raw).expect("canonical vector parses");
            let expected = fs::read(dir.join(canonical_file)).expect("read canonical bytes");
            assert_eq!(
                contract.canonical_bytes().expect("canonical bytes"),
                expected,
                "vector '{}': canonical JCS bytes drifted",
                vector.name
            );
        }

        let outcome = match vector.kind {
            Kind::Contract => compute_contract_id(&raw),
            Kind::Envelope => serde_json::from_str::<ExecutionContractEnvelopeV1>(&raw)
                .map_err(|error| ExecutionContractError::Canonicalization(error.to_string()))
                .and_then(|envelope| {
                    envelope.verify()?;
                    envelope.execution_contract.compute_execution_id()
                }),
        };

        match vector.expect {
            Expect::Error => {
                assert!(
                    outcome.is_err(),
                    "vector '{}': expected fail-closed error, got {outcome:?}",
                    vector.name
                );
            }
            Expect::ExecutionId => {
                let id =
                    outcome.unwrap_or_else(|error| panic!("vector '{}': {error}", vector.name));
                assert_eq!(
                    Some(id.as_str()),
                    vector.execution_id.as_deref(),
                    "vector '{}': execution_id drifted",
                    vector.name
                );
                match vector.relation {
                    Some(Relation::EqualsBaseline) => assert_eq!(
                        id, baseline_id,
                        "vector '{}': must not change the id",
                        vector.name
                    ),
                    Some(Relation::DiffersFromBaseline) => {
                        assert_ne!(
                            id, baseline_id,
                            "vector '{}': identity-bearing mutation must change the id",
                            vector.name
                        );
                        assert!(
                            mutated_ids.insert(id.as_str().to_string()),
                            "vector '{}': mutation ids must be pairwise distinct",
                            vector.name
                        );
                    }
                    None => assert_eq!(vector.name, manifest.baseline),
                }
            }
        }
    }
}

#[test]
fn every_vector_file_is_listed_in_the_manifest() {
    let dir = fixture_dir().join("vectors");
    let manifest = load_manifest();
    let listed: BTreeSet<String> = manifest
        .vectors
        .iter()
        .map(|vector| {
            Path::new(&vector.file)
                .file_name()
                .expect("vector file name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let on_disk: BTreeSet<String> = fs::read_dir(&dir)
        .expect("read vectors dir")
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(listed, on_disk, "manifest and vectors/ directory diverged");
}
