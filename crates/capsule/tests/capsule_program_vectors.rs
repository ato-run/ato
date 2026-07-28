//! Runner for the shared `ato.capsule-program/v1` test vectors (ADR-014 §9).
//!
//! The fixtures under `tests/fixtures/capsule_program_contract/` are the
//! cross-language source of truth for the canonical form (RFC 8785 JCS +
//! domain-separated BLAKE3) in three suites indexed by one `manifest.json`;
//! other implementations (Phase 1) consume the same files. Invariants
//! exercised here:
//!
//! **contract/** — `CapsuleProgramContractV1` JSON → canonical JCS bytes →
//! `capsule_program_id`:
//!
//! 1. The baseline contract produces the exact recorded canonical bytes and
//!    `capsule_program_id` (pins the domain separator and canonicalization).
//! 2. Field order never influences the canonical bytes or the id.
//! 3. Identity-bearing mutations (source digest, manifest intent) change the
//!    id, match their recorded ids exactly, and are pairwise distinct.
//! 4. Malformed identity input fails closed: unknown top-level or nested
//!    intent fields, a wrong schema string, a blake3-spelled source digest.
//! 5. Envelope metadata (generated_at/provenance/diagnostics, tolerated
//!    unknown fields) never influences the id; two envelopes differing only
//!    in metadata verify to the SAME verified id; a stored-id mismatch fails
//!    `verify()` closed.
//!
//! **manifest/** — `capsule.toml` text → expected `ProgramManifestIntentV1`
//! JSON via the real pipeline (`load_manifest` → `program_intent_from_v03`):
//! equivalent authored spellings share one expected file; rejection vectors
//! pin a distinctive error substring.
//!
//! **source/** — committed fixture tree → projected file set → expected
//! `ProgramSourceDigest`: the projection excludes exactly the selected root's
//! control files, the digest is a fixed point across {no lock, `capsule.lock`,
//! `ato.lock.json`}, and control-file NAMES at nested paths stay in the
//! projection and move the digest.
//!
//! Only the two non-portable source scenarios stay out of the committed tree —
//! a control-file-shaped symlink and an executable-bit flip do not survive
//! every platform/VCS checkout, so they remain tempdir unit tests in
//! `crates/capsule/src/contract/program_source_projection.rs`
//! (`symlink_named_capsule_lock_rejected_by_admissibility_pass`,
//! `executable_bit_flip_changes_projection_digest`,
//! `staged_copy_preserves_executable_bit`).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use capsule::blob::materialize_source_archive;
use capsule::capsule_program_contract::{
    CAPSULE_PROGRAM_V1_SCHEMA, CapsuleProgramContractV1, CapsuleProgramEnvelopeV1,
    CapsuleProgramError, CapsuleProgramId, ProgramSourceContract, ProgramSourceDigest,
    ProgramSourceProjectionSchemaV1,
};
use capsule::manifest::load_manifest;
use capsule::program_manifest_input::program_intent_from_v03;
use capsule::program_source_projection::{
    StagedCapsuleSource, VerifiedPinnedSourceMaterialization,
};
use serde::Deserialize;
use serde_json::Value;
use tempfile::TempDir;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: String,
    #[allow(dead_code)]
    description: String,
    domain_separator_utf8: String,
    #[allow(dead_code)]
    capsule_program_id_formula: String,
    #[allow(dead_code)]
    jcs: String,
    #[allow(dead_code)]
    manifest_suite_pipeline: String,
    #[allow(dead_code)]
    source_projection_suite: String,
    contract_baseline: String,
    source_baseline: String,
    contract_vectors: Vec<ContractVector>,
    manifest_vectors: Vec<ManifestVector>,
    source_vectors: Vec<SourceVector>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractVector {
    name: String,
    file: String,
    kind: ContractKind,
    expect: ContractExpect,
    capsule_program_id: Option<String>,
    relation: Option<Relation>,
    canonical_file: Option<String>,
    /// Recorded when the REASON a vector is rejected is the point of the
    /// vector (the `invalid-duplicate-*` pair), mirroring `ManifestVector`.
    error_substring: Option<String>,
    #[allow(dead_code)]
    notes: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ContractKind {
    Contract,
    Envelope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ContractExpect {
    CapsuleProgramId,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Relation {
    EqualsBaseline,
    DiffersFromBaseline,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestVector {
    name: String,
    file: String,
    expect: ManifestExpect,
    expected_file: Option<String>,
    error_substring: Option<String>,
    #[allow(dead_code)]
    notes: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ManifestExpect {
    Intent,
    Error,
}

/// One committed source-suite fixture tree: the projected file set a second
/// implementation must reproduce, and the digest of that projection.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceVector {
    name: String,
    dir: String,
    /// Paths relative to `dir`, `/`-joined, lexicographically sorted.
    projected_files: Vec<String>,
    source_digest: String,
    relation: Option<Relation>,
    #[allow(dead_code)]
    notes: Option<String>,
}

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/capsule_program_contract")
}

fn load_index() -> Manifest {
    let raw = fs::read_to_string(fixture_dir().join("manifest.json")).expect("read manifest");
    serde_json::from_str(&raw).expect("parse manifest")
}

fn compute_contract_id(raw: &str) -> Result<CapsuleProgramId, CapsuleProgramError> {
    let contract: CapsuleProgramContractV1 = serde_json::from_str(raw)
        .map_err(|error| CapsuleProgramError::Canonicalization(error.to_string()))?;
    contract.compute_capsule_program_id()
}

/// The full manifest-suite pipeline: tempdir + per-vector side files →
/// ordinary v0.3 normalizer (`load_manifest`, strict validation) → strict
/// gate + adapter (`program_intent_from_v03`) → intent JSON. Errors from
/// either stage collapse into one message for substring matching.
fn derive_intent(vector_name: &str, toml_text: &str) -> Result<Value, String> {
    let dir = tempfile::tempdir().expect("tempdir");
    manifest_vector_setup(vector_name, dir.path());
    let path = dir.path().join("capsule.toml");
    fs::write(&path, toml_text).expect("write manifest");
    let loaded = load_manifest(&path).map_err(|error| error.to_string())?;
    let intent = program_intent_from_v03(&loaded.model, &loaded.raw_text, dir.path())
        .map_err(|error| error.to_string())?;
    intent
        .validate()
        .expect("derived intent must pass IR validation");
    Ok(serde_json::to_value(&intent).expect("intent serializes"))
}

/// Side files a vector's manifest refers to (`SourceExistingPath` policy).
/// Keep in sync with the identical function in
/// `gen_capsule_program_vectors.rs`.
fn manifest_vector_setup(vector_name: &str, root: &Path) {
    if matches!(
        vector_name,
        "model-sha256-bare" | "model-sha256-prefixed" | "reject-engine-path"
    ) {
        fs::write(root.join("model.gguf"), b"gguf").expect("write model side file");
    }
}

#[test]
fn contract_vectors_pin_the_canonical_form() {
    let dir = fixture_dir();
    let index = load_index();
    assert_eq!(index.schema, CAPSULE_PROGRAM_V1_SCHEMA);
    assert_eq!(index.domain_separator_utf8, CAPSULE_PROGRAM_V1_SCHEMA);

    let baseline = index
        .contract_vectors
        .iter()
        .find(|vector| vector.name == index.contract_baseline)
        .expect("baseline vector present");
    let baseline_raw = fs::read_to_string(dir.join(&baseline.file)).expect("read baseline");
    let baseline_id = compute_contract_id(&baseline_raw).expect("baseline computes");
    assert_eq!(
        Some(baseline_id.as_str()),
        baseline.capsule_program_id.as_deref(),
        "baseline capsule_program_id drifted — canonicalization or domain separation changed"
    );

    let mut mutated_ids = BTreeSet::new();
    for vector in &index.contract_vectors {
        let raw = fs::read_to_string(dir.join(&vector.file))
            .unwrap_or_else(|error| panic!("read vector '{}': {error}", vector.name));

        if let Some(canonical_file) = &vector.canonical_file {
            let contract: CapsuleProgramContractV1 =
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
            ContractKind::Contract => compute_contract_id(&raw),
            ContractKind::Envelope => serde_json::from_str::<CapsuleProgramEnvelopeV1>(&raw)
                .map_err(|error| CapsuleProgramError::Canonicalization(error.to_string()))
                .and_then(|envelope| {
                    let verified = envelope.verified_capsule_program_id()?;
                    Ok(verified.as_capsule_program_id().clone())
                }),
        };

        match vector.expect {
            ContractExpect::Error => {
                let error = match outcome {
                    Err(error) => error.to_string(),
                    Ok(id) => panic!(
                        "vector '{}': expected fail-closed error, got {id}",
                        vector.name
                    ),
                };
                if let Some(substring) = &vector.error_substring {
                    assert!(
                        error.contains(substring),
                        "vector '{}': error '{error}' must contain '{substring}'",
                        vector.name
                    );
                }
            }
            ContractExpect::CapsuleProgramId => {
                let id =
                    outcome.unwrap_or_else(|error| panic!("vector '{}': {error}", vector.name));
                assert_eq!(
                    Some(id.as_str()),
                    vector.capsule_program_id.as_deref(),
                    "vector '{}': capsule_program_id drifted",
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
                    None => assert_eq!(vector.name, index.contract_baseline),
                }
            }
        }
    }
}

/// The two `envelope-non-identity-*` vectors carry the SAME contract with
/// different non-identity metadata (and a tolerated unknown envelope field on
/// `b`): `verify()` passes on both and both mint the same verified id.
#[test]
fn envelope_metadata_variants_verify_to_one_id() {
    let dir = fixture_dir();
    let verified_ids: Vec<CapsuleProgramId> =
        ["envelope-non-identity-a", "envelope-non-identity-b"]
            .iter()
            .map(|name| {
                let raw = fs::read_to_string(dir.join(format!("contract/vectors/{name}.json")))
                    .expect("read envelope vector");
                let envelope: CapsuleProgramEnvelopeV1 =
                    serde_json::from_str(&raw).expect("envelope parses (tolerant read)");
                envelope.verify().expect("metadata never breaks verify()");
                envelope
                    .verified_capsule_program_id()
                    .expect("verified id")
                    .as_capsule_program_id()
                    .clone()
            })
            .collect();
    assert_eq!(
        verified_ids[0], verified_ids[1],
        "envelopes differing only in non-identity metadata must verify to one id"
    );
}

#[test]
fn manifest_vectors_pin_the_normalized_intent() {
    let dir = fixture_dir();
    let index = load_index();

    for vector in &index.manifest_vectors {
        let toml_text = fs::read_to_string(dir.join(&vector.file))
            .unwrap_or_else(|error| panic!("read vector '{}': {error}", vector.name));
        let outcome = derive_intent(&vector.name, &toml_text);

        match vector.expect {
            ManifestExpect::Intent => {
                let derived =
                    outcome.unwrap_or_else(|error| panic!("vector '{}': {error}", vector.name));
                let expected_file = vector
                    .expected_file
                    .as_ref()
                    .unwrap_or_else(|| panic!("vector '{}': missing expected_file", vector.name));
                let expected: Value = serde_json::from_str(
                    &fs::read_to_string(dir.join(expected_file)).expect("read expected intent"),
                )
                .expect("parse expected intent");
                assert_eq!(
                    derived, expected,
                    "vector '{}': normalized intent drifted from {expected_file}",
                    vector.name
                );
            }
            ManifestExpect::Error => {
                let error = match outcome {
                    Err(error) => error,
                    Ok(intent) => panic!(
                        "vector '{}': expected rejection, derived {intent}",
                        vector.name
                    ),
                };
                let substring = vector
                    .error_substring
                    .as_ref()
                    .unwrap_or_else(|| panic!("vector '{}': missing error_substring", vector.name));
                assert!(
                    error.contains(substring),
                    "vector '{}': error '{error}' must contain '{substring}'",
                    vector.name
                );
            }
        }
    }
}

/// The headline Rule-4 spellings the manifest suite exists to pin, asserted
/// against the committed expected files so a regeneration cannot silently
/// drop the property the vector demonstrates.
#[test]
fn expected_intent_files_pin_the_headline_spellings() {
    let dir = fixture_dir();
    let expected = |stem: &str| -> Value {
        serde_json::from_str(
            &fs::read_to_string(dir.join(format!("manifest/expected/{stem}.intent.json")))
                .expect("read expected intent"),
        )
        .expect("parse expected intent")
    };

    // web-root-entrypoint: canonical Root spelling of a source-relative path.
    assert_eq!(
        expected("web-root-entrypoint")["targets"]["targets"]["site"]["working_dir"],
        serde_json::json!({ "source_relative": "." })
    );

    // model-sha256: one canonical IR spelling (bare lowercase hex) for both
    // authoring spellings, and the model path is a source-relative file ref.
    let model = &expected("model-sha256")["targets"]["targets"]["chat"];
    assert_eq!(model["model_sha256"], serde_json::json!("ab".repeat(32)));
    assert_eq!(model["model"], serde_json::json!("model.gguf"));

    // wasm-world-default: authored-absent world is default-expanded.
    assert_eq!(
        expected("wasm-world-default")["targets"]["targets"]["wasm"]["world"],
        serde_json::json!("wasi:cli/command")
    );

    // oci-user: a uid:gid ContainerUserSpec survives to the IR verbatim.
    assert_eq!(
        expected("oci-user")["targets"]["targets"]["app"]["user"],
        serde_json::json!("1000:1000")
    );

    // seal-at: the authored acceptance argv is identity-bearing and reaches the
    // IR with argument boundaries exact (spec §6.1) — an explicitly selected
    // shell, a space-containing argument as ONE element, a trailing empty
    // argument preserved — alongside its positive bounded timeout.
    let seal_at = &expected("seal-at")["seal_at"];
    assert_eq!(
        seal_at["command"],
        serde_json::json!([
            "sh",
            "-lc",
            "curl -fsS http://127.0.0.1:8080/ready",
            "--label",
            ""
        ])
    );
    assert_eq!(seal_at["timeout_seconds"], serde_json::json!(120));
}

/// Cross-suite determinism: the baseline-oci manifest vector's derived intent,
/// wrapped in a contract with a fixed source digest, derives the same
/// `capsule_program_id` across two fully independent computations.
#[test]
fn cross_suite_capsule_program_id_is_deterministic() {
    let dir = fixture_dir();
    let toml_text = fs::read_to_string(dir.join("manifest/vectors/baseline-oci.toml"))
        .expect("read baseline-oci vector");

    let contract_for = |intent_json: Value| -> CapsuleProgramContractV1 {
        let contract = CapsuleProgramContractV1 {
            schema: CAPSULE_PROGRAM_V1_SCHEMA.to_string(),
            source: ProgramSourceContract {
                digest: ProgramSourceDigest::new([0xAA; 32]),
                projection_schema: ProgramSourceProjectionSchemaV1,
            },
            manifest_intent: serde_json::from_value(intent_json).expect("intent round-trips"),
        };
        contract.validate().expect("contract validates");
        contract
    };

    let first = contract_for(derive_intent("baseline-oci", &toml_text).expect("first derivation"))
        .compute_capsule_program_id()
        .expect("first id");
    let second =
        contract_for(derive_intent("baseline-oci", &toml_text).expect("second derivation"))
            .compute_capsule_program_id()
            .expect("second id");
    assert_eq!(
        first, second,
        "manifest pipeline + contract hash must be deterministic end to end"
    );
}

/// The control-file names that are special **at the selected root only**. A
/// projection may never contain any of them at its root, whatever the vector.
const ROOT_CONTROL_FILE_NAMES: [&str; 3] = ["capsule.toml", "capsule.lock", "ato.lock.json"];

/// Runs the real projection over a committed fixture tree and returns
/// (projected file set, `sha256:<hex>` digest).
///
/// The fixture tree goes through the production pipeline exactly as a producer
/// would: `materialize_source_archive` freezes it into a content-addressed
/// `.tar.zst`, and `from_source_archive` extracts that archive into a
/// process-private root it owns. This crate is a separate compilation unit that
/// sees only `capsule`'s public API — the same surface a downstream producer
/// has — so there is no way to hand the projection a directory and assert it is
/// pinned, and no way to reach the projected tree between enumerating it and
/// hashing it: `projected_file_paths` returns the `/`-joined, lexicographically
/// sorted relative paths, never a root. The committed fixture tree itself is
/// only read.
///
/// Keep in sync with the identical function in
/// `gen_capsule_program_vectors.rs`.
fn project_source_vector(root: &Path) -> (Vec<String>, String) {
    let archive_dir = TempDir::new().expect("archive output directory");
    let archive = archive_dir.path().join("source.tar.zst");
    materialize_source_archive(root, &archive).expect("committed fixture tree materializes");
    let pinned = VerifiedPinnedSourceMaterialization::from_source_archive(&archive)
        .expect("the content-addressed archive extracts to a pinned materialization");
    let projected = StagedCapsuleSource::stage(&pinned)
        .expect("fixture tree stages")
        .into_projected()
        .expect("control files are excluded");
    let files = projected
        .projected_file_paths()
        .expect("projected tree enumerates");
    let digest = projected
        .source_contract()
        .expect("projected tree hashes")
        .digest
        .to_string();
    (files, digest)
}

#[test]
fn source_vectors_pin_the_projected_file_set_and_digest() {
    let dir = fixture_dir();
    let index = load_index();

    let baseline = index
        .source_vectors
        .iter()
        .find(|vector| vector.name == index.source_baseline)
        .expect("source baseline vector present");
    let (_, baseline_digest) = project_source_vector(&dir.join(&baseline.dir));
    assert_eq!(
        baseline_digest, baseline.source_digest,
        "source baseline digest drifted — the A1 tree hash or the control-file exclusion changed"
    );

    let mut differing_digests = BTreeSet::new();
    for vector in &index.source_vectors {
        let (files, digest) = project_source_vector(&dir.join(&vector.dir));

        assert_eq!(
            files, vector.projected_files,
            "vector '{}': projected file set drifted",
            vector.name
        );
        assert_eq!(
            digest, vector.source_digest,
            "vector '{}': ProgramSourceDigest drifted",
            vector.name
        );
        for name in ROOT_CONTROL_FILE_NAMES {
            assert!(
                !files.iter().any(|file| file == name),
                "vector '{}': root control file {name} must never reach the projection",
                vector.name
            );
        }

        match vector.relation {
            Some(Relation::EqualsBaseline) => assert_eq!(
                digest, baseline_digest,
                "vector '{}': the lock spelling must not move the digest",
                vector.name
            ),
            Some(Relation::DiffersFromBaseline) => {
                assert_ne!(
                    digest, baseline_digest,
                    "vector '{}': a source-bytes change must move the digest",
                    vector.name
                );
                assert!(
                    differing_digests.insert(digest),
                    "vector '{}': differing digests must be pairwise distinct",
                    vector.name
                );
            }
            None => assert_eq!(vector.name, index.source_baseline),
        }
    }
}

/// The exact-path rule the `nested-control-names` vector exists to pin: only
/// the SELECTED ROOT's control files are special, so control-file names at
/// nested paths are ordinary source — in the projected set, and digest-bearing.
#[test]
fn nested_control_file_names_stay_in_the_projected_set() {
    let index = load_index();
    let vector = index
        .source_vectors
        .iter()
        .find(|vector| vector.name == "nested-control-names")
        .expect("nested-control-names vector present");

    for nested in ["examples/capsule.toml", "fixtures/capsule.lock"] {
        assert!(
            vector.projected_files.iter().any(|file| file == nested),
            "{nested} must stay in the projection: exclusion is by exact path, not by file name"
        );
    }
    assert_eq!(vector.relation, Some(Relation::DiffersFromBaseline));
}

#[test]
fn every_source_vector_directory_is_listed_in_the_manifest() {
    let dir = fixture_dir();
    let index = load_index();

    let listed: BTreeSet<String> = index
        .source_vectors
        .iter()
        .map(|vector| {
            Path::new(&vector.dir)
                .file_name()
                .expect("vector directory name")
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let on_disk: BTreeSet<String> = fs::read_dir(dir.join("source/vectors"))
        .expect("read source vectors dir")
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        listed, on_disk,
        "manifest and source/vectors/ directory diverged — a fixture tree cannot be \
         added without an index entry that verifies it"
    );
}

#[test]
fn every_vector_file_is_listed_in_the_manifest() {
    let dir = fixture_dir();
    let index = load_index();

    let check = |sub: &str, listed: BTreeSet<String>| {
        let on_disk: BTreeSet<String> = fs::read_dir(dir.join(sub))
            .expect("read vectors dir")
            .map(|entry| {
                entry
                    .expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(listed, on_disk, "manifest and {sub}/ directory diverged");
    };

    check(
        "contract/vectors",
        index
            .contract_vectors
            .iter()
            .map(|vector| {
                Path::new(&vector.file)
                    .file_name()
                    .expect("vector file name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect(),
    );
    check(
        "manifest/vectors",
        index
            .manifest_vectors
            .iter()
            .map(|vector| {
                Path::new(&vector.file)
                    .file_name()
                    .expect("vector file name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect(),
    );
    check(
        "manifest/expected",
        index
            .manifest_vectors
            .iter()
            .filter_map(|vector| vector.expected_file.as_ref())
            .map(|file| {
                Path::new(file)
                    .file_name()
                    .expect("expected file name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect(),
    );
}
