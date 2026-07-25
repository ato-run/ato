//! Deterministic generator for the shared `ato.capsule-program/v1` test
//! vectors (ADR-014 §9, contract/ + manifest/ + source/ suites).
//!
//! This is the single source of truth for every fixture under
//! `tests/fixtures/capsule_program_contract/` **and** for its `manifest.json`.
//! It never hand-authors a `capsule_program_id`, an expected intent JSON, or a
//! `ProgramSourceDigest`: every id is computed from the typed contract with
//! [`CapsuleProgramContractV1::compute_capsule_program_id`], every
//! `manifest/expected/*.intent.json` is derived by running the real pipeline
//! (`capsule::manifest::load_manifest` →
//! [`capsule::program_manifest_input::program_intent_from_v03`]) over the
//! committed `.toml` vector, and every source-suite file set + digest is
//! derived by running the real projection over the committed fixture tree.
//!
//! The `source/vectors/<name>/` trees are the one class of fixture this
//! generator does NOT write: they are hand-authored INPUTS (a second
//! implementation reads the same bytes), so the generator only recomputes
//! their recorded projected file set and digest.
//!
//! It is `#[ignore]`d so it never runs in normal CI; regenerate with:
//!
//! ```sh
//! cargo test -p capsule --test gen_capsule_program_vectors -- --ignored --exact regenerate_shared_vectors
//! ```
//!
//! then verify with the runner
//! (`cargo test -p capsule --test capsule_program_vectors`).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use capsule::blob::materialize_source_archive;
use capsule::capsule_program_contract::{
    CAPSULE_PROGRAM_V1_SCHEMA, CapsuleProgramContractV1, CapsuleProgramEnvelopeV1,
};
use capsule::manifest::load_manifest;
use capsule::program_manifest_input::program_intent_from_v03;
use capsule::program_source_projection::{
    StagedCapsuleSource, VerifiedPinnedSourceMaterialization,
};
use serde::Serialize;
use serde_json::{Value, json};
use tempfile::TempDir;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/capsule_program_contract")
}

/// (vector name, manifest.json note, baseline-value mutation).
type ValueMutation = (&'static str, &'static str, fn(&mut Value));

// ─────────────────────────────────────────────────────────────────────────────
// Suite 1 — contract/: baseline + reorder + mutations + fail-closed + envelope
// ─────────────────────────────────────────────────────────────────────────────

/// A small, valid contract: schema + pinned source (digest + projection
/// schema) + a minimal manifest intent (capsule_type, one state entry).
/// Built through the typed parser so the generator itself proves the JSON is
/// the canonical typed spelling.
fn baseline_value() -> Value {
    json!({
        "schema": "ato.capsule-program/v1",
        "source": {
            "digest": format!("sha256:{}", "11".repeat(32)),
            "projection_schema": "ato.capsule-program-source-projection/v1"
        },
        "manifest_intent": {
            "schema": "ato.capsule-program-manifest-intent/v1",
            "capsule_type": "web-app",
            "state": {
                "scratch": {
                    "kind": "filesystem",
                    "durability": "ephemeral",
                    "purpose": "run scratch"
                }
            }
        }
    })
}

fn typed(value: &Value) -> CapsuleProgramContractV1 {
    serde_json::from_value(value.clone()).expect("typed contract parses")
}

/// Envelope value with representative non-identity metadata. The stored id is
/// always the canonical hash of the embedded contract unless a caller
/// overwrites it.
fn envelope_value(contract: &Value, capsule_program_id: &str) -> Value {
    json!({
        "program_contract": contract,
        "capsule_program_id": capsule_program_id,
        "generated_at": "2026-07-24T00:00:00Z",
        "provenance": {
            "authoring_schema": "0.3",
            "name": "my-app",
            "version": "1.2.3"
        },
        "diagnostics": { "adapter_log": "normalized 1 target" }
    })
}

fn write_file(dir: &Path, rel: &str, contents: &str) {
    write_bytes(dir, rel, contents.as_bytes());
}

fn write_bytes(dir: &Path, rel: &str, contents: &[u8]) {
    let full = dir.join(rel);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(full, contents).unwrap();
}

fn pretty_value(value: &Value) -> String {
    let mut out = serde_json::to_string_pretty(value).unwrap();
    out.push('\n');
    out
}

/// Emit a pretty JSON string with every object's keys in reverse-sorted order,
/// used only for the `field-order` vector (JCS must erase the reordering).
fn reverse_ordered(value: &Value, indent: usize) -> String {
    let pad = "  ".repeat(indent);
    let pad1 = "  ".repeat(indent + 1);
    match value {
        Value::Object(map) => {
            if map.is_empty() {
                return "{}".to_string();
            }
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            keys.reverse();
            let items: Vec<String> = keys
                .into_iter()
                .map(|key| {
                    format!(
                        "{pad1}{}: {}",
                        serde_json::to_string(key).unwrap(),
                        reverse_ordered(&map[key.as_str()], indent + 1)
                    )
                })
                .collect();
            format!("{{\n{}\n{pad}}}", items.join(",\n"))
        }
        Value::Array(items) => {
            if items.is_empty() {
                return "[]".to_string();
            }
            let rendered: Vec<String> = items
                .iter()
                .map(|item| format!("{pad1}{}", reverse_ordered(item, indent + 1)))
                .collect();
            format!("[\n{}\n{pad}]", rendered.join(",\n"))
        }
        scalar => serde_json::to_string(scalar).unwrap(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Suite 2 — manifest/: capsule.toml text → expected ProgramManifestIntentV1
// ─────────────────────────────────────────────────────────────────────────────

/// The full derivation pipeline the runner mirrors: tempdir + side files →
/// `load_manifest` (ordinary v0.3 normalizer, strict validation) →
/// `program_intent_from_v03` (strict gate + adapter).
fn derive_intent(vector_name: &str, toml_text: &str) -> Result<Value, String> {
    let dir = tempfile::tempdir().expect("tempdir");
    manifest_vector_setup(vector_name, dir.path());
    let path = dir.path().join("capsule.toml");
    fs::write(&path, toml_text).expect("write manifest");
    let loaded = load_manifest(&path).map_err(|error| error.to_string())?;
    let intent = program_intent_from_v03(&loaded.model, &loaded.raw_text, dir.path())
        .map_err(|error| error.to_string())?;
    intent.validate().expect("derived intent is canonical");
    Ok(serde_json::to_value(&intent).expect("intent serializes"))
}

/// Side files a vector's manifest refers to (`SourceExistingPath` policy).
/// Keep in sync with the identical function in `capsule_program_vectors.rs`.
fn manifest_vector_setup(vector_name: &str, root: &Path) {
    if matches!(
        vector_name,
        "model-sha256-bare" | "model-sha256-prefixed" | "reject-engine-path"
    ) {
        fs::write(root.join("model.gguf"), b"gguf").expect("write model side file");
    }
}

const BASELINE_OCI_TOML: &str = r#"schema_version = "0.3"
name = "baseline-oci"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:1"
port = 8080
"#;

/// Differs from `baseline-oci` ONLY in the non-identity top level: `name`,
/// `version`, and display `[metadata]` — plus the excluded `[routing]` and
/// `[pool]` sections. Must produce the byte-identical expected intent file.
const EQUIVALENT_METADATA_CHANGE_TOML: &str = r#"schema_version = "0.3"
name = "renamed-elsewhere"
version = "9.9.9"
type = "app"
default_target = "app"

[metadata]
display_name = "Renamed"
description = "entirely different display metadata"

[routing]
weight = "heavy"

[pool]
enabled = true

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:1"
port = 8080
"#;

/// Alias + explicit-default authored spelling: target `run` (alias of
/// `run_command`), service `command` (alias of `entrypoint`), and authored
/// explicit-default sections (`[snapshot] mode = "none"`, empty `[network]` /
/// `[pack]`).
const SOURCE_RUN_ALIAS_TOML: &str = r#"schema_version = "0.3"
name = "source-run"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "node"
run = "node server.js"
port = 8082

[services.main]
command = "node server.js"

[snapshot]
mode = "none"

[network]

[pack]
"#;

/// The canonical spelling of the same declaration as `source-run-alias`.
const SOURCE_RUN_CANONICAL_TOML: &str = r#"schema_version = "0.3"
name = "source-run"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "source"
driver = "node"
run_command = "node server.js"
port = 8082

[services.main]
entrypoint = "node server.js"
"#;

/// Web static target whose `working_dir = "."` pins the canonical Root
/// spelling in the IR (`{"source_relative": "."}`).
const WEB_ROOT_ENTRYPOINT_TOML: &str = r#"schema_version = "0.3"
name = "web-root"
version = "0.1.0"
type = "app"
default_target = "site"

[targets.site]
runtime = "web"
driver = "static"
run = "index.html"
working_dir = "."
port = 8081
"#;

fn model_sha256_toml(pin: &str) -> String {
    format!(
        r#"schema_version = "0.3"
name = "model-pin"
version = "0.1.0"
type = "app"
default_target = "chat"

[targets.chat]
runtime = "native-inference"
engine = "llama.cpp"
engine_version = "b9754"
model = "model.gguf"
model_sha256 = "{pin}"
port = 8080
"#
    )
}

/// Structured `[targets.wasm]` with `world` authored absent: the adapter must
/// default-expand it to `wasi:cli/command` before hashing.
fn wasm_world_default_toml() -> String {
    format!(
        r#"schema_version = "0.3"
name = "wasm-default"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.wasm]
digest = "sha256:{digest}"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:1"
port = 8080
"#,
        digest = "0c".repeat(32)
    )
}

const OCI_USER_TOML: &str = r#"schema_version = "0.3"
name = "oci-user"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:1"
user = "1000:1000"
port = 8080
"#;

const REJECT_WORKSPACE_TOML: &str = r#"schema_version = "0.3"
name = "workspace-reject"
version = "0.1.0"
type = "app"
default_target = "app"

[workspace]
default_app = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:1"
port = 8080
"#;

const REJECT_ENGINE_PATH_TOML: &str = r#"schema_version = "0.3"
name = "engine-path-reject"
version = "0.1.0"
type = "app"
default_target = "chat"

[targets.chat]
runtime = "native-inference"
engine_path = "/usr/local/bin/llama-server"
model = "model.gguf"
port = 8080
"#;

const REJECT_UNKNOWN_TOP_LEVEL_TOML: &str = r#"schema_version = "0.3"
name = "unknown-top-level"
version = "0.1.0"
type = "app"
default_target = "app"
description = "marketing copy the tolerant model parser silently drops"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:1"
port = 8080
"#;

fn reject_source_digest_bare_toml() -> String {
    format!(
        r#"schema_version = "0.3"
name = "bare-source-digest"
version = "0.1.0"
type = "app"
default_target = "app"

[targets]
source_digest = "{}"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:1"
port = 8080
"#,
        "ab".repeat(32)
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// Suite 3 — source/: fixture tree → projected file set → ProgramSourceDigest
// ─────────────────────────────────────────────────────────────────────────────

/// (vector directory name, relation to the source baseline, manifest.json note).
///
/// Regular files only, by design: a committed symlink or executable-bit fixture
/// does not survive every platform/VCS checkout, so those two scenarios stay in
/// the tempdir unit tests named in the fixture README.
type SourceVectorSpec = (&'static str, Option<&'static str>, &'static str);

const SOURCE_VECTORS: [SourceVectorSpec; 5] = [
    (
        "baseline",
        None,
        "manifest + two ordinary source files, no lock: the root capsule.toml is excluded and \
         everything else is hashed",
    ),
    (
        "with-canonical-lock",
        Some("equals-baseline"),
        "baseline's source bytes plus a root capsule.lock: the resolved lock never reaches the \
         preimage, so the digest is the baseline's exactly (fixed point)",
    ),
    (
        "with-deprecated-alias-lock",
        Some("equals-baseline"),
        "baseline's source bytes plus a root ato.lock.json: the deprecated alias resolves to the \
         same single excluded lock path, so the digest survives the lock-file rename",
    ),
    (
        "nested-control-names",
        Some("differs-from-baseline"),
        "baseline's source bytes plus examples/capsule.toml and fixtures/capsule.lock: only the \
         SELECTED ROOT's control files are special, so both nested files are ordinary source, stay \
         in the projected set, and change the digest (exact-path rule, no content sniffing)",
    ),
    (
        "nested-dir-tree",
        Some("differs-from-baseline"),
        "nested directories plus sibling names that sort across the '/' boundary (a.txt / a/ / \
         ab/): pins recursion and the A1 child-ordering rule",
    ),
];

/// Runs the real projection over a committed fixture tree and returns
/// (projected file set, `sha256:<hex>` digest).
///
/// Same production pipeline as the checker: freeze the tree with
/// `materialize_source_archive`, then mint the proof by extracting that
/// content-addressed archive, and read the recorded file set from
/// `projected_file_paths` (the `/`-joined, lexicographically sorted relative
/// paths) rather than by walking a projected root the harness could mutate. The
/// recorded value is therefore what a producer on the public API observes, not
/// what a self-attested directory would give.
///
/// Keep in sync with the identical function in `capsule_program_vectors.rs`.
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

// ─────────────────────────────────────────────────────────────────────────────
// manifest.json index shapes
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct GenContractVector {
    name: String,
    file: String,
    kind: &'static str,
    expect: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    capsule_program_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    relation: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    canonical_file: Option<String>,
    /// Recorded for `expect: "error"` vectors whose rejection REASON is the
    /// point of the vector (same role as `GenManifestVector::error_substring`).
    #[serde(skip_serializing_if = "Option::is_none")]
    error_substring: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
}

#[derive(Serialize)]
struct GenManifestVector {
    name: String,
    file: String,
    expect: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_substring: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
}

#[derive(Serialize)]
struct GenSourceVector {
    name: String,
    dir: String,
    projected_files: Vec<String>,
    source_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    relation: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
}

#[derive(Serialize)]
struct GenManifest {
    schema: String,
    description: String,
    domain_separator_utf8: String,
    capsule_program_id_formula: String,
    jcs: String,
    manifest_suite_pipeline: String,
    source_projection_suite: String,
    contract_baseline: String,
    source_baseline: String,
    contract_vectors: Vec<GenContractVector>,
    manifest_vectors: Vec<GenManifestVector>,
    source_vectors: Vec<GenSourceVector>,
}

#[test]
#[ignore = "regenerates committed fixtures; run explicitly"]
fn regenerate_shared_vectors() {
    let dir = fixture_dir();

    // Clean regeneration so obsolete vectors cannot linger and desync the
    // completeness checks. `source/vectors` is NOT cleaned: those trees are
    // hand-authored inputs, and the generator only re-derives their index entry.
    for sub in [
        "contract/vectors",
        "contract/expected",
        "manifest/vectors",
        "manifest/expected",
    ] {
        let path = dir.join(sub);
        if let Ok(entries) = fs::read_dir(&path) {
            for entry in entries.flatten() {
                let is_fixture = entry
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "json" || ext == "toml");
                if is_fixture {
                    fs::remove_file(entry.path()).unwrap();
                }
            }
        }
    }

    let mut contract_vectors: Vec<GenContractVector> = Vec::new();
    let mut seen_ids: BTreeSet<String> = BTreeSet::new();

    // ── contract/: baseline ─────────────────────────────────────────────
    let base_value = baseline_value();
    let base = typed(&base_value);
    let baseline_id = base
        .compute_capsule_program_id()
        .unwrap()
        .as_str()
        .to_string();
    write_file(
        &dir,
        "contract/vectors/baseline.json",
        &pretty_value(&base_value),
    );
    write_bytes(
        &dir,
        "contract/expected/baseline.canonical.json",
        &base.canonical_bytes().unwrap(),
    );
    seen_ids.insert(baseline_id.clone());
    contract_vectors.push(GenContractVector {
        name: "baseline".to_string(),
        file: "contract/vectors/baseline.json".to_string(),
        kind: "contract",
        expect: "capsule_program_id",
        capsule_program_id: Some(baseline_id.clone()),
        relation: None,
        canonical_file: Some("contract/expected/baseline.canonical.json".to_string()),
        error_substring: None,
        notes: None,
    });

    // ── contract/: field-order (equals baseline, same canonical bytes) ──
    write_file(
        &dir,
        "contract/vectors/field-order.json",
        &format!("{}\n", reverse_ordered(&base_value, 0)),
    );
    contract_vectors.push(GenContractVector {
        name: "field-order".to_string(),
        file: "contract/vectors/field-order.json".to_string(),
        kind: "contract",
        expect: "capsule_program_id",
        capsule_program_id: Some(baseline_id.clone()),
        relation: Some("equals-baseline"),
        canonical_file: Some("contract/expected/baseline.canonical.json".to_string()),
        error_substring: None,
        notes: Some(
            "same contract with every object's keys reverse-sorted; JCS erases key order, so \
             canonical bytes and id equal the baseline's exactly"
                .to_string(),
        ),
    });

    // ── contract/: identity mutations ───────────────────────────────────
    let mutations: [ValueMutation; 2] = [
        (
            "mutate-source-digest",
            "one hex digit of the pinned A1 source digest differs; the id must change",
            |value| {
                value["source"]["digest"] = json!(format!("sha256:{}12", "11".repeat(31)));
            },
        ),
        (
            "mutate-intent",
            "manifest_intent.capsule_type differs; declaration intent is identity-bearing",
            |value| value["manifest_intent"]["capsule_type"] = json!("tool"),
        ),
    ];
    for (name, note, mutate) in mutations {
        let mut value = base_value.clone();
        mutate(&mut value);
        let id = typed(&value)
            .compute_capsule_program_id()
            .unwrap_or_else(|error| panic!("mutation '{name}' must stay valid: {error}"))
            .as_str()
            .to_string();
        assert_ne!(id, baseline_id, "mutation '{name}' must change the id");
        assert!(
            seen_ids.insert(id.clone()),
            "mutation '{name}' id collides with another vector"
        );
        let file = format!("contract/vectors/{name}.json");
        write_file(&dir, &file, &pretty_value(&value));
        contract_vectors.push(GenContractVector {
            name: name.to_string(),
            file,
            kind: "contract",
            expect: "capsule_program_id",
            capsule_program_id: Some(id),
            relation: Some("differs-from-baseline"),
            canonical_file: None,
            error_substring: None,
            notes: Some(note.to_string()),
        });
    }

    // ── contract/: fail-closed invalid vectors ──────────────────────────
    let invalid: [ValueMutation; 4] = [
        (
            "invalid-unknown-top-level",
            "unknown top-level identity-bearing field fails closed (deny_unknown_fields)",
            |value| {
                value
                    .as_object_mut()
                    .unwrap()
                    .insert("runner".to_string(), json!("local"));
            },
        ),
        (
            "invalid-unknown-intent-field",
            "unknown field inside manifest_intent fails closed — non-identity manifest data \
             (e.g. name) must never deserialize into the intent",
            |value| {
                value["manifest_intent"]
                    .as_object_mut()
                    .unwrap()
                    .insert("name".to_string(), json!("my-app"));
            },
        ),
        (
            "invalid-schema-string",
            "contract schema version mismatch fails validate() closed",
            |value| value["schema"] = json!("ato.capsule-program/v2"),
        ),
        (
            "invalid-source-digest-blake3",
            "a blake3-spelled ProgramSourceDigest is rejected on typed parse: the A1 source \
             tree hash is sha256-only",
            |value| {
                value["source"]["digest"] = json!(format!("blake3:{}", "11".repeat(32)));
            },
        ),
    ];
    for (name, note, mutate) in invalid {
        let mut value = base_value.clone();
        mutate(&mut value);
        let file = format!("contract/vectors/{name}.json");
        write_file(&dir, &file, &pretty_value(&value));
        contract_vectors.push(GenContractVector {
            name: name.to_string(),
            file,
            kind: "contract",
            expect: "error",
            capsule_program_id: None,
            relation: None,
            canonical_file: None,
            error_substring: None,
            notes: Some(note.to_string()),
        });
    }

    // ── contract/: duplicate-key vectors (literal text, not a Value) ─────
    // `serde_json::Value` cannot hold a duplicate key — it is last-wins like
    // every stock JSON parser — so these are injected TEXTUALLY into the
    // rendered baseline: the repeated key must survive in the fixture bytes
    // and reach the deserializer, where the identity maps' UniqueBTreeMap
    // rejects it instead of silently collapsing two byte-distinct documents
    // onto one capsule_program_id.
    let mut duplicate: Vec<(&str, String, &str, &str)> = Vec::new();

    // A SECOND "scratch" entry, spelled at the baseline's own indentation.
    let repeated_state_entry = r#"      "scratch": {
        "durability": "persistent",
        "kind": "database",
        "purpose": "app data"
      },
"#;
    let state_key = pretty_value(&base_value).replacen(
        "    \"state\": {\n",
        &format!("    \"state\": {{\n{repeated_state_entry}"),
        1,
    );
    assert_eq!(
        state_key.matches("\"scratch\"").count(),
        2,
        "duplicate state key injection missed its anchor"
    );
    duplicate.push((
        "invalid-duplicate-state-key",
        state_key,
        "duplicate identity map key 'scratch'",
        "a duplicate key in a top-level identity map (state) is rejected by the unique-map \
         deserializer: BTreeMap last-wins would give two byte-distinct documents one id",
    ));

    let mut nested = base_value.clone();
    nested["manifest_intent"]["targets"] = json!({
        "targets": { "app": { "env": { "PORT": "8080" } } }
    });
    typed(&nested);
    let nested_env_key = pretty_value(&nested).replacen(
        "          \"env\": {\n",
        "          \"env\": {\n            \"PORT\": \"9090\",\n",
        1,
    );
    assert_eq!(
        nested_env_key.matches("\"PORT\"").count(),
        2,
        "duplicate nested env key injection missed its anchor"
    );
    duplicate.push((
        "invalid-duplicate-nested-env-key",
        nested_env_key,
        "duplicate identity map key 'PORT'",
        "a duplicate key in a NESTED identity map (targets.<label>.env) is rejected too — the \
         fail-closed rule is not only top level",
    ));

    for (name, text, error_substring, note) in duplicate {
        let error = serde_json::from_str::<CapsuleProgramContractV1>(&text)
            .expect_err("a duplicate identity map key must fail closed");
        assert!(
            error.to_string().contains(error_substring),
            "vector '{name}': error '{error}' must contain '{error_substring}'"
        );
        let file = format!("contract/vectors/{name}.json");
        write_file(&dir, &file, &text);
        contract_vectors.push(GenContractVector {
            name: name.to_string(),
            file,
            kind: "contract",
            expect: "error",
            capsule_program_id: None,
            relation: None,
            canonical_file: None,
            error_substring: Some(error_substring.to_string()),
            notes: Some(note.to_string()),
        });
    }

    // ── contract/: envelopes ────────────────────────────────────────────
    // Two envelopes over the SAME contract differing only in non-identity
    // metadata (generated_at / provenance / diagnostics, plus a tolerated
    // unknown envelope field on `b`): verify() passes on both and both yield
    // the same verified id.
    let envelope_a = envelope_value(&base_value, &baseline_id);
    write_file(
        &dir,
        "contract/vectors/envelope-non-identity-a.json",
        &pretty_value(&envelope_a),
    );
    contract_vectors.push(GenContractVector {
        name: "envelope-non-identity-a".to_string(),
        file: "contract/vectors/envelope-non-identity-a.json".to_string(),
        kind: "envelope",
        expect: "capsule_program_id",
        capsule_program_id: Some(baseline_id.clone()),
        relation: Some("equals-baseline"),
        canonical_file: None,
        error_substring: None,
        notes: Some(
            "envelope metadata (generated_at/provenance/diagnostics) is non-identity".to_string(),
        ),
    });

    let mut envelope_b = envelope_value(&base_value, &baseline_id);
    {
        let object = envelope_b.as_object_mut().unwrap();
        object.insert("generated_at".to_string(), json!("2027-01-01T12:34:56Z"));
        object.insert(
            "provenance".to_string(),
            json!({
                "authoring_schema": "0.3",
                "name": "renamed-elsewhere",
                "version": "9.9.9"
            }),
        );
        object.insert(
            "diagnostics".to_string(),
            json!({ "adapter_log": "a completely different diagnostic trail" }),
        );
        // The envelope is tolerant of unknown non-identity fields by design.
        object.insert("registry_row".to_string(), json!("row-77"));
    }
    write_file(
        &dir,
        "contract/vectors/envelope-non-identity-b.json",
        &pretty_value(&envelope_b),
    );
    contract_vectors.push(GenContractVector {
        name: "envelope-non-identity-b".to_string(),
        file: "contract/vectors/envelope-non-identity-b.json".to_string(),
        kind: "envelope",
        expect: "capsule_program_id",
        capsule_program_id: Some(baseline_id.clone()),
        relation: Some("equals-baseline"),
        canonical_file: None,
        error_substring: None,
        notes: Some(
            "same contract as envelope-non-identity-a with different generated_at/provenance/\
             diagnostics and a tolerated unknown envelope field; verify() passes and the \
             verified id is identical"
                .to_string(),
        ),
    });

    let mut mismatch = envelope_value(&base_value, &baseline_id);
    mismatch["capsule_program_id"] = json!(format!("blake3:{}", "0".repeat(64)));
    // Prove the tampered envelope still PARSES and only verify() rejects it.
    serde_json::from_value::<CapsuleProgramEnvelopeV1>(mismatch.clone())
        .expect("mismatched envelope parses")
        .verify()
        .expect_err("stored-id mismatch must fail verification");
    write_file(
        &dir,
        "contract/vectors/envelope-id-mismatch.json",
        &pretty_value(&mismatch),
    );
    contract_vectors.push(GenContractVector {
        name: "envelope-id-mismatch".to_string(),
        file: "contract/vectors/envelope-id-mismatch.json".to_string(),
        kind: "envelope",
        expect: "error",
        capsule_program_id: None,
        relation: None,
        canonical_file: None,
        error_substring: None,
        notes: Some(
            "stored capsule_program_id disagrees with the canonical hash of program_contract; \
             readers fail closed"
                .to_string(),
        ),
    });

    // ── manifest/: derived-intent vectors ───────────────────────────────
    let mut manifest_vectors: Vec<GenManifestVector> = Vec::new();
    let mut expected_written: BTreeSet<String> = BTreeSet::new();

    // (name, toml, expected file stem shared across equal spellings, notes)
    let intent_vectors: Vec<(&str, String, &str, &str)> = vec![
        (
            "baseline-oci",
            BASELINE_OCI_TOML.to_string(),
            "baseline-oci",
            "the BASE-style app manifest: one named OCI target",
        ),
        (
            "equivalent-metadata-change",
            EQUIVALENT_METADATA_CHANGE_TOML.to_string(),
            "baseline-oci",
            "differs from baseline-oci only in name/version/[metadata]/[routing]/[pool] \
             (non-identity + excluded sections): shares baseline-oci's expected intent file",
        ),
        (
            "source-run-alias",
            SOURCE_RUN_ALIAS_TOML.to_string(),
            "source-run",
            "aliases (target run=, service command=) plus authored explicit-default sections \
             ([snapshot] mode=none, empty [network]/[pack]): canonicalizes into the same IR \
             as the canonical spelling",
        ),
        (
            "source-run-canonical",
            SOURCE_RUN_CANONICAL_TOML.to_string(),
            "source-run",
            "the canonical spelling of the same declaration as source-run-alias",
        ),
        (
            "web-root-entrypoint",
            WEB_ROOT_ENTRYPOINT_TOML.to_string(),
            "web-root-entrypoint",
            "web static target with working_dir = \".\": the IR pins the canonical Root \
             spelling {\"source_relative\": \".\"}",
        ),
        (
            "model-sha256-bare",
            model_sha256_toml(&"ab".repeat(32)),
            "model-sha256",
            "bare-hex model_sha256 authoring spelling; model is a SourceExistingPath (the \
             harness materializes model.gguf)",
        ),
        (
            "model-sha256-prefixed",
            model_sha256_toml(&format!("sha256:{}", "ab".repeat(32))),
            "model-sha256",
            "sha256:-prefixed model_sha256 authoring spelling; normalizes into the same IR \
             (bare lowercase hex) as model-sha256-bare",
        ),
        (
            "wasm-world-default",
            wasm_world_default_toml(),
            "wasm-world-default",
            "structured [targets.wasm] with world authored absent: the IR pins the \
             default-expanded \"wasi:cli/command\"",
        ),
        (
            "oci-user",
            OCI_USER_TOML.to_string(),
            "oci-user",
            "user = \"1000:1000\" is a valid ContainerUserSpec and stays visible in the IR",
        ),
    ];

    for (name, toml_text, expected_stem, notes) in &intent_vectors {
        let file = format!("manifest/vectors/{name}.toml");
        write_file(&dir, &file, toml_text);
        let derived = derive_intent(name, toml_text)
            .unwrap_or_else(|error| panic!("vector '{name}' must derive an intent: {error}"));
        let expected_file = format!("manifest/expected/{expected_stem}.intent.json");
        if expected_written.insert(expected_file.clone()) {
            write_file(&dir, &expected_file, &pretty_value(&derived));
        } else {
            let existing: Value =
                serde_json::from_str(&fs::read_to_string(dir.join(&expected_file)).unwrap())
                    .unwrap();
            assert_eq!(
                derived, existing,
                "vector '{name}' must derive the same intent as its shared expected file"
            );
        }
        manifest_vectors.push(GenManifestVector {
            name: name.to_string(),
            file,
            expect: "intent",
            expected_file: Some(expected_file),
            error_substring: None,
            notes: Some(notes.to_string()),
        });
    }

    // ── manifest/: rejection vectors ────────────────────────────────────
    let reject_vectors: Vec<(&str, String, &str, &str)> = vec![
        (
            "reject-workspace",
            REJECT_WORKSPACE_TOML.to_string(),
            "does not support field 'workspace'",
            "workspace manifests fail Program Identity issuance closed (ADR-014 Phase 0)",
        ),
        (
            "reject-engine-path",
            REJECT_ENGINE_PATH_TOML.to_string(),
            "does not support field 'engine_path'",
            "engine_path names a host-local binary; it can never be part of a portable \
             declaration (Rule 3)",
        ),
        (
            "reject-unknown-top-level",
            REJECT_UNKNOWN_TOP_LEVEL_TOML.to_string(),
            "unknown field `description`",
            "the tolerant model parser silently drops unknown top-level keys; the strict \
             identity gate rejects them instead of hashing around dropped authoring",
        ),
        (
            "reject-source-digest-bare",
            reject_source_digest_bare_toml(),
            "source_digest must start with 'sha256:'",
            "targets.source_digest requires the sha256: prefix (existing validator); the \
             bare spelling is rejected before the adapter runs",
        ),
    ];
    for (name, toml_text, substring, notes) in &reject_vectors {
        let file = format!("manifest/vectors/{name}.toml");
        write_file(&dir, &file, toml_text);
        let error =
            derive_intent(name, toml_text).expect_err(&format!("vector '{name}' must be rejected"));
        assert!(
            error.contains(substring),
            "vector '{name}': error '{error}' must contain '{substring}'"
        );
        manifest_vectors.push(GenManifestVector {
            name: name.to_string(),
            file,
            expect: "error",
            expected_file: None,
            error_substring: Some(substring.to_string()),
            notes: Some(notes.to_string()),
        });
    }

    // ── source/: committed fixture trees → file set + digest ────────────
    let mut source_vectors: Vec<GenSourceVector> = Vec::new();
    let mut source_baseline_digest: Option<String> = None;
    let mut source_digests: BTreeSet<String> = BTreeSet::new();
    for (name, relation, notes) in SOURCE_VECTORS {
        let vector_dir = format!("source/vectors/{name}");
        let (projected_files, source_digest) = project_source_vector(&dir.join(&vector_dir));
        assert!(
            !projected_files.is_empty(),
            "source vector '{name}': the projected tree must not be empty"
        );
        match relation {
            None => {
                assert!(
                    source_baseline_digest
                        .replace(source_digest.clone())
                        .is_none(),
                    "source suite must declare exactly one baseline"
                );
            }
            Some("equals-baseline") => assert_eq!(
                Some(&source_digest),
                source_baseline_digest.as_ref(),
                "source vector '{name}': the lock spelling must not move the digest"
            ),
            Some("differs-from-baseline") => {
                assert_ne!(
                    Some(&source_digest),
                    source_baseline_digest.as_ref(),
                    "source vector '{name}': a source-bytes change must move the digest"
                );
                assert!(
                    source_digests.insert(source_digest.clone()),
                    "source vector '{name}': differing digests must be pairwise distinct"
                );
            }
            Some(other) => panic!("source vector '{name}': unknown relation '{other}'"),
        }
        source_vectors.push(GenSourceVector {
            name: name.to_string(),
            dir: vector_dir,
            projected_files,
            source_digest,
            relation,
            notes: Some(notes.to_string()),
        });
    }

    // ── manifest.json ───────────────────────────────────────────────────
    let manifest = GenManifest {
        schema: CAPSULE_PROGRAM_V1_SCHEMA.to_string(),
        description: "Shared cross-language test vectors for the ato.capsule-program/v1 \
                      canonical form (ADR-014 §9: contract/ + manifest/ + source/ suites). \
                      Consumed by crates/capsule/tests/capsule_program_vectors.rs and reusable \
                      verbatim by a second-language implementation (Phase 1)."
            .to_string(),
        domain_separator_utf8: CAPSULE_PROGRAM_V1_SCHEMA.to_string(),
        capsule_program_id_formula: "\"blake3:\" + lowercase_hex(BLAKE3(UTF8(domain_separator_utf8) || 0x00 || JCS(contract)))".to_string(),
        jcs: "RFC 8785 (JSON Canonicalization Scheme)".to_string(),
        manifest_suite_pipeline: "Each manifest/vectors/*.toml is written to an otherwise-empty \
                                  source root (plus the side files listed in the harness setup: \
                                  model-sha256-* and reject-engine-path materialize model.gguf), \
                                  loaded with the ordinary v0.3 normalizer (load_manifest, strict \
                                  validation), then adapted with program_intent_from_v03. \
                                  expect=intent vectors must equal their expected_file JSON \
                                  exactly; vectors sharing one expected_file are equivalent \
                                  authored spellings of the same declaration. expect=error \
                                  vectors must fail (at either pipeline stage) with a message \
                                  containing error_substring."
            .to_string(),
        source_projection_suite: "Each source/vectors/<name>/ directory is a committed, \
                                  regular-files-only fixture tree. Freeze it with \
                                  materialize_source_archive into a content-addressed .tar.zst \
                                  and mint the pinned source materialization by extracting that \
                                  archive with from_source_archive (the only public mint; no \
                                  directory is self-attested), run the ProgramSourceProjectionV1 \
                                  derivation over the extracted root (A1v2 admissibility, \
                                  staging copy, resolve the control files at the selected root, \
                                  exclude exactly those paths), and compare: the remaining files \
                                  must equal projected_files (paths relative to the vector root, \
                                  '/'-joined, lexicographically sorted) and the A1 tree hash of \
                                  that projection must equal source_digest. relation is measured \
                                  against source_baseline. Symlink and executable-bit scenarios \
                                  are deliberately NOT committed here (they do not survive every \
                                  platform/VCS checkout) — see README.md for the unit tests that \
                                  cover them."
            .to_string(),
        contract_baseline: "baseline".to_string(),
        source_baseline: "baseline".to_string(),
        contract_vectors,
        manifest_vectors,
        source_vectors,
    };
    let mut manifest_json = serde_json::to_string_pretty(&manifest).unwrap();
    manifest_json.push('\n');
    fs::write(dir.join("manifest.json"), manifest_json).unwrap();

    println!(
        "regenerated {} contract vectors + {} manifest vectors + {} source vectors",
        manifest.contract_vectors.len(),
        manifest.manifest_vectors.len(),
        manifest.source_vectors.len()
    );
    println!("baseline capsule_program_id = {baseline_id}");
    if let Some(digest) = source_baseline_digest {
        println!("baseline ProgramSourceDigest = {digest}");
    }
}
