//! Deterministic generator for the shared `ato.execution-contract/v1` test
//! vectors.
//!
//! This is the single source of truth for every fixture under
//! `tests/fixtures/execution_contract/` **and** for `manifest.json`. It never
//! hand-authors an `execution_id`: every id is computed from the typed contract
//! with [`ExecutionContractV1::compute_execution_id`], and the generator fails
//! if any identity mutation collides with the baseline or another mutation.
//!
//! It is `#[ignore]`d so it never runs in normal CI; regenerate with:
//!
//! ```sh
//! cargo test -p capsule --test gen_execution_contract_vectors -- --ignored --exact regenerate_shared_vectors
//! ```
//!
//! then verify with the runner
//! (`cargo test -p capsule --test execution_contract_vectors`).

use std::collections::BTreeSet;
use std::fs;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};

use capsule::execution_contract::{
    ContentDigest, DigestAlgorithm, EXECUTION_CONTRACT_V1_SCHEMA, EnvironmentVariableContract,
    ExecutionContractV1, ExternalStateAccess, ExternalStateContract, GuestPath,
    GuestSurfaceContract, OpaqueContractDigestV1, ResolvedArtifactContract,
    ResolvedBuildOutputContract, ResolvedDependencyContract, ResolvedFilesystemContract,
    ResolvedLaunchContract, ResolvedPolicyContract, ResolvedSourceContract, ResolvedTargetContract,
    SnapshotExclusion,
};
use serde::Serialize;
use serde_json::{Value, json};

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/execution_contract")
}

fn digest(algorithm: DigestAlgorithm, byte: u8) -> ContentDigest {
    ContentDigest::new(algorithm, [byte; 32])
}

fn opaque(byte: u8) -> OpaqueContractDigestV1 {
    OpaqueContractDigestV1::new([byte; 32])
}

fn path(value: &str) -> GuestPath {
    GuestPath::parse(value).expect("canonical guest path")
}

fn baseline() -> ExecutionContractV1 {
    ExecutionContractV1 {
        schema: EXECUTION_CONTRACT_V1_SCHEMA.to_string(),
        source: ResolvedSourceContract {
            digest: digest(DigestAlgorithm::Sha256, 1),
            projection_digest: opaque(0x0c),
        },
        target: ResolvedTargetContract {
            os: "linux".to_string(),
            architecture: "x86_64".to_string(),
            abi: "gnu".to_string(),
            libc: Some("glibc-2.39".to_string()),
            observable_features: Default::default(),
        },
        runtime: ResolvedArtifactContract {
            kind: "node".to_string(),
            digest: digest(DigestAlgorithm::Sha256, 2),
            dynamic_contract_digest: opaque(0x0d),
        },
        dependencies: vec![ResolvedDependencyContract {
            name: "npm".to_string(),
            derivation_digest: digest(DigestAlgorithm::Blake3, 3),
            output_digest: digest(DigestAlgorithm::Blake3, 4),
        }],
        build_outputs: vec![ResolvedBuildOutputContract {
            name: "app".to_string(),
            digest: digest(DigestAlgorithm::Blake3, 5),
            projection_digest: opaque(0x0e),
        }],
        launch: ResolvedLaunchContract {
            argv: vec!["node".to_string(), "dist/server.js".to_string()],
            cwd: path("/workspace"),
            process_model_digest: opaque(0x0f),
            environment: vec![EnvironmentVariableContract {
                name: "NODE_ENV".to_string(),
                value_digest: opaque(6),
            }],
            environment_policy_digest: opaque(0x10),
            secret_bindings: vec!["API_TOKEN".to_string()],
        },
        filesystem: ResolvedFilesystemContract {
            view_digest: digest(DigestAlgorithm::Blake3, 7),
            topology_digest: opaque(0x11),
            readonly_layers: vec![digest(DigestAlgorithm::Blake3, 8)],
            writable_paths: vec![path("/tmp")],
        },
        policy: ResolvedPolicyContract {
            network_digest: opaque(9),
            capability_digest: opaque(10),
            filesystem_digest: opaque(11),
        },
        guest_surface: GuestSurfaceContract {
            bind_address: "0.0.0.0".to_string(),
            protocol: "ato-guest/v1".to_string(),
            port: Some(NonZeroU16::new(8080).unwrap()),
            features: vec!["bindings".to_string(), "exec".to_string()],
        },
        external_state: vec![ExternalStateContract {
            name: "data".to_string(),
            target: path("/data"),
            access: ExternalStateAccess::ReadWrite,
            schema: "1".to_string(),
            snapshot: SnapshotExclusion::Exclude,
        }],
    }
}

/// The `unicode-strings` contract carries the RFC 8785 string-canonicalization
/// content in the free-form identity fields that survive: `launch.argv` (any
/// bytes) and `external_state[].target` (a non-ASCII, control-free path).
fn unicode_contract() -> ExecutionContractV1 {
    let mut contract = baseline();
    // é (literal), U+0007 BEL (control), CJK, U+001F (control), astral emoji.
    contract.launch.argv = vec![
        "node".to_string(),
        "dist/server.js".to_string(),
        "--label=café\u{7}日本語\u{1f}🚀".to_string(),
    ];
    // Kana in a canonical guest path (no control chars allowed here).
    contract.external_state[0].target = path("/data/ユーザー");
    contract
}

#[derive(Serialize)]
struct GenVector {
    name: String,
    file: String,
    kind: &'static str,
    expect: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    relation: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    canonical_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
}

#[derive(Serialize)]
struct GenManifest {
    schema: String,
    description: String,
    domain_separator_utf8: String,
    execution_id_formula: String,
    jcs: String,
    numbers: String,
    optional_fields: String,
    opaque_digests: String,
    guest_path_ordering: String,
    baseline: String,
    vectors: Vec<GenVector>,
}

fn write_file(dir: &Path, rel: &str, contents: &str) {
    let full = dir.join(rel);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(full, contents).unwrap();
}

fn pretty_json(contract: &ExecutionContractV1) -> String {
    let mut out = serde_json::to_string_pretty(contract).unwrap();
    out.push('\n');
    out
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

type Mutation = (
    &'static str,
    Option<&'static str>,
    fn(&mut ExecutionContractV1),
);

fn identity_mutations() -> Vec<Mutation> {
    vec![
        ("mutate-source-digest", None, |c| {
            c.source.digest = digest(DigestAlgorithm::Sha256, 0x21);
        }),
        (
            "mutate-source-projection-digest",
            Some(
                "opaque source projection-rules sub-contract digest is identity-bearing (RFC §4.2 Source)",
            ),
            |c| c.source.projection_digest = opaque(0xac),
        ),
        ("mutate-target-os", None, |c| {
            c.target.os = "windows".to_string()
        }),
        ("mutate-target-architecture", None, |c| {
            c.target.architecture = "aarch64".to_string();
        }),
        ("mutate-target-abi", None, |c| {
            c.target.abi = "musl".to_string()
        }),
        ("mutate-target-libc", None, |c| {
            c.target.libc = Some("musl-1.2.5".to_string());
        }),
        (
            "mutate-target-observable-features",
            Some("baseline omits observable_features entirely; introducing one changes the id"),
            |c| {
                c.target
                    .observable_features
                    .insert("avx512".to_string(), "1".to_string());
            },
        ),
        ("mutate-runtime-kind", None, |c| {
            c.runtime.kind = "python".to_string()
        }),
        ("mutate-runtime-artifact-digest", None, |c| {
            c.runtime.digest = digest(DigestAlgorithm::Sha256, 0x22);
        }),
        (
            "mutate-runtime-dynamic-contract-digest",
            Some(
                "opaque dynamic-runtime sub-contract digest is identity-bearing (RFC §4.2 Runtime)",
            ),
            |c| c.runtime.dynamic_contract_digest = opaque(0xad),
        ),
        ("mutate-dependency-name", None, |c| {
            c.dependencies[0].name = "pnpm".to_string();
        }),
        ("mutate-dependency-derivation-digest", None, |c| {
            c.dependencies[0].derivation_digest = digest(DigestAlgorithm::Blake3, 0x23);
        }),
        ("mutate-dependency-output-digest", None, |c| {
            c.dependencies[0].output_digest = digest(DigestAlgorithm::Blake3, 0x24);
        }),
        ("mutate-build-output-name", None, |c| {
            c.build_outputs[0].name = "bundle".to_string();
        }),
        ("mutate-build-output-digest", None, |c| {
            c.build_outputs[0].digest = digest(DigestAlgorithm::Blake3, 0x25);
        }),
        (
            "mutate-build-output-projection-digest",
            Some(
                "opaque build-output projection sub-contract digest is identity-bearing (RFC §4.2 Build outputs)",
            ),
            |c| c.build_outputs[0].projection_digest = opaque(0xae),
        ),
        ("mutate-launch-argv", None, |c| {
            c.launch.argv = vec!["node".to_string(), "dist/main.js".to_string()];
        }),
        ("mutate-launch-cwd", None, |c| c.launch.cwd = path("/srv")),
        (
            "mutate-launch-process-model-digest",
            Some("opaque process-model sub-contract digest is identity-bearing (RFC §4.2 Launch)"),
            |c| c.launch.process_model_digest = opaque(0xaf),
        ),
        (
            "mutate-launch-environment-policy-digest",
            Some(
                "opaque environment requirements/normalization/inheritance policy sub-contract digest is identity-bearing (RFC §4.2 Environment)",
            ),
            |c| c.launch.environment_policy_digest = opaque(0xb0),
        ),
        ("mutate-launch-secret-bindings", None, |c| {
            c.launch.secret_bindings = vec!["API_TOKEN".to_string(), "DB_PASSWORD".to_string()];
        }),
        ("mutate-environment-name", None, |c| {
            c.launch.environment[0].name = "RUST_LOG".to_string();
        }),
        ("mutate-environment-value-digest", None, |c| {
            c.launch.environment[0].value_digest = opaque(0x26);
        }),
        ("mutate-filesystem-view-digest", None, |c| {
            c.filesystem.view_digest = digest(DigestAlgorithm::Blake3, 0x27);
        }),
        (
            "mutate-filesystem-topology-digest",
            Some(
                "opaque mount-topology and access-modes sub-contract digest is identity-bearing (RFC §4.2 Filesystem)",
            ),
            |c| c.filesystem.topology_digest = opaque(0xb1),
        ),
        ("mutate-filesystem-readonly-layers", None, |c| {
            c.filesystem.readonly_layers = vec![digest(DigestAlgorithm::Blake3, 0x28)];
        }),
        ("mutate-filesystem-writable-paths", None, |c| {
            c.filesystem.writable_paths = vec![path("/var/tmp")];
        }),
        (
            "mutate-network-policy-digest",
            Some(
                "opaque network-policy sub-contract digest is identity-bearing (RFC §4.2 Network; domain ato.network-policy/v1)",
            ),
            |c| c.policy.network_digest = opaque(0x29),
        ),
        (
            "mutate-capability-policy-digest",
            Some(
                "opaque capability-policy sub-contract digest is identity-bearing (RFC §4.2 Capabilities; domain ato.capability-policy/v1)",
            ),
            |c| c.policy.capability_digest = opaque(0x2a),
        ),
        (
            "mutate-filesystem-policy-digest",
            Some(
                "opaque filesystem-policy sub-contract digest is identity-bearing (RFC §4.2 Capabilities; domain ato.filesystem-policy/v1)",
            ),
            |c| c.policy.filesystem_digest = opaque(0x2b),
        ),
        ("mutate-guest-surface-bind-address", None, |c| {
            c.guest_surface.bind_address = "127.0.0.1".to_string();
        }),
        ("mutate-guest-surface-protocol", None, |c| {
            c.guest_surface.protocol = "ato-guest/v2".to_string();
        }),
        ("mutate-guest-surface-port", None, |c| {
            c.guest_surface.port = Some(NonZeroU16::new(9090).unwrap());
        }),
        ("mutate-guest-surface-features", None, |c| {
            c.guest_surface.features =
                vec!["bindings".to_string(), "exec".to_string(), "fs".to_string()];
        }),
        ("mutate-external-state-name", None, |c| {
            c.external_state[0].name = "cache".to_string();
        }),
        ("mutate-external-state-target", None, |c| {
            c.external_state[0].target = path("/var/data");
        }),
        ("mutate-external-state-schema", None, |c| {
            c.external_state[0].schema = "2".to_string();
        }),
        ("mutate-external-state-access", None, |c| {
            c.external_state[0].access = ExternalStateAccess::ReadOnly;
        }),
        (
            "guest-path-utf8-order",
            Some(
                "GuestPath lists sort segment-wise by UTF-8 byte order: U+E000 precedes U+10000 under UTF-8/code-point order (they invert under UTF-16). This correctly-ordered pair is accepted; TypeScript implementations MUST compare UTF-8 bytes, not UTF-16 code units. See invalid-guest-path-utf16-order for the rejected reverse.",
            ),
            |c| c.filesystem.writable_paths = vec![path("/\u{e000}"), path("/\u{10000}")],
        ),
    ]
}

/// Baseline envelope value with representative non-identity fields (tolerated
/// unknown keys plus provenance/diagnostics/evidence). `resolved_refs` and
/// `execution_id` are filled by each caller.
fn envelope_value(baseline_id: &str) -> Value {
    json!({
        "execution_contract": serde_json::to_value(baseline()).unwrap(),
        "execution_id": baseline_id,
        "generated_at": "2026-07-21T00:00:00Z",
        "provenance": {
            "builder": "runner-a",
            "machine_id": "machine-77",
            "display_url": "https://example.invalid/builds/123"
        },
        "diagnostics": { "resolver_log": "resolved node 22.14.0 in 1.2s" },
        "evidence": { "readiness_probe": "http-200" },
        "runner_id": "runner-a",
        "session_id": "sess-42",
        "snapshot_id": "snap-9",
        "host_port": 54321
    })
}

#[test]
#[ignore = "regenerates committed fixtures; run explicitly"]
fn regenerate_shared_vectors() {
    let dir = fixture_dir();

    // Clean regeneration: remove any stale *.json so obsolete vectors (e.g. the
    // removed source/runtime ref mutations) can't linger and desync the
    // manifest completeness check.
    for sub in ["vectors", "expected"] {
        let path = dir.join(sub);
        if let Ok(entries) = fs::read_dir(&path) {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|ext| ext == "json") {
                    fs::remove_file(entry.path()).unwrap();
                }
            }
        }
    }

    let mut vectors: Vec<GenVector> = Vec::new();
    let mut seen_ids: BTreeSet<String> = BTreeSet::new();

    // --- baseline -----------------------------------------------------------
    let base = baseline();
    let baseline_id = base.compute_execution_id().unwrap().as_str().to_string();
    write_file(&dir, "vectors/baseline.json", &pretty_json(&base));
    fs::write(
        dir.join("expected/baseline.canonical.json"),
        base.canonical_bytes().unwrap(),
    )
    .unwrap();
    seen_ids.insert(baseline_id.clone());
    vectors.push(GenVector {
        name: "baseline".to_string(),
        file: "vectors/baseline.json".to_string(),
        kind: "contract",
        expect: "execution_id",
        execution_id: Some(baseline_id.clone()),
        relation: None,
        canonical_file: Some("expected/baseline.canonical.json".to_string()),
        notes: None,
    });

    // --- field-order + whitespace (equals-baseline robustness) --------------
    let base_value = serde_json::to_value(&base).unwrap();
    write_file(
        &dir,
        "vectors/field-order.json",
        &format!("{}\n", reverse_ordered(&base_value, 0)),
    );
    vectors.push(GenVector {
        name: "field-order".to_string(),
        file: "vectors/field-order.json".to_string(),
        kind: "contract",
        expect: "execution_id",
        execution_id: Some(baseline_id.clone()),
        relation: Some("equals-baseline"),
        canonical_file: None,
        notes: Some(
            "same contract with every object's keys reverse-sorted; JCS erases key order"
                .to_string(),
        ),
    });

    write_file(
        &dir,
        "vectors/whitespace.json",
        &format!("{}\n", serde_json::to_string(&base).unwrap()),
    );
    vectors.push(GenVector {
        name: "whitespace".to_string(),
        file: "vectors/whitespace.json".to_string(),
        kind: "contract",
        expect: "execution_id",
        execution_id: Some(baseline_id.clone()),
        relation: Some("equals-baseline"),
        canonical_file: None,
        notes: Some(
            "same contract minified; input whitespace never reaches the canonical form".to_string(),
        ),
    });

    // --- unicode-strings ----------------------------------------------------
    let unicode = unicode_contract();
    let unicode_id = unicode.compute_execution_id().unwrap().as_str().to_string();
    assert_ne!(unicode_id, baseline_id, "unicode vector must differ");
    assert!(seen_ids.insert(unicode_id.clone()), "unicode id collision");
    write_file(&dir, "vectors/unicode-strings.json", &pretty_json(&unicode));
    fs::write(
        dir.join("expected/unicode-strings.canonical.json"),
        unicode.canonical_bytes().unwrap(),
    )
    .unwrap();
    vectors.push(GenVector {
        name: "unicode-strings".to_string(),
        file: "vectors/unicode-strings.json".to_string(),
        kind: "contract",
        expect: "execution_id",
        execution_id: Some(unicode_id),
        relation: Some("differs-from-baseline"),
        canonical_file: Some("expected/unicode-strings.canonical.json".to_string()),
        notes: Some("pins RFC 8785 string canonicalization for free-form fields: input escapes (surrogate pair, control chars) and literal UTF-8 (é-acute, CJK, kana, astral-plane emoji) canonicalize identically — control characters as lowercase \\u00xx / two-char escapes, everything else as literal UTF-8".to_string()),
    });

    // --- identity mutations -------------------------------------------------
    for (name, notes, mutate) in identity_mutations() {
        let mut contract = baseline();
        mutate(&mut contract);
        let id = contract.compute_execution_id().unwrap_or_else(|error| {
            panic!("mutation '{name}' produced an invalid contract: {error}")
        });
        let id = id.as_str().to_string();
        assert_ne!(id, baseline_id, "mutation '{name}' must change the id");
        assert!(
            seen_ids.insert(id.clone()),
            "mutation '{name}' id collides with another vector"
        );
        let file = format!("vectors/{name}.json");
        write_file(&dir, &file, &pretty_json(&contract));
        vectors.push(GenVector {
            name: name.to_string(),
            file,
            kind: "contract",
            expect: "execution_id",
            execution_id: Some(id),
            relation: Some("differs-from-baseline"),
            canonical_file: None,
            notes: notes.map(str::to_string),
        });
    }

    // --- envelope: non-identity equals-baseline -----------------------------
    write_file(
        &dir,
        "vectors/envelope-non-identity.json",
        &pretty_value(&envelope_value(&baseline_id)),
    );
    vectors.push(GenVector {
        name: "envelope-non-identity".to_string(),
        file: "vectors/envelope-non-identity.json".to_string(),
        kind: "envelope",
        expect: "execution_id",
        execution_id: Some(baseline_id.clone()),
        relation: Some("equals-baseline"),
        canonical_file: None,
        notes: Some("provenance/diagnostics/evidence/timestamps/runner/session/snapshot/host facts are non-identity and ignored".to_string()),
    });

    // --- envelope: resolved-ref aliases are non-identity (equals-baseline) ---
    let alias_vectors: [(&str, Value, &str); 3] = [
        (
            "source-kind-alias",
            json!({ "source_kind": "archive" }),
            "source.kind is a non-identity resolved-ref alias; it must not change the id",
        ),
        (
            "source-immutable-ref-alias",
            json!({ "source_immutable_ref": "https://mirror.invalid/repo@012345" }),
            "source.immutable_ref is a non-identity resolved-ref alias; it must not change the id",
        ),
        (
            "runtime-resolved-ref-alias",
            json!({ "runtime_resolved_ref": "node@lts" }),
            "runtime.resolved_ref is a non-identity resolved-ref alias; it must not change the id",
        ),
    ];
    for (name, resolved_refs, note) in alias_vectors {
        let mut value = envelope_value(&baseline_id);
        value
            .as_object_mut()
            .unwrap()
            .insert("resolved_refs".to_string(), resolved_refs);
        let file = format!("vectors/{name}.json");
        write_file(&dir, &file, &pretty_value(&value));
        vectors.push(GenVector {
            name: name.to_string(),
            file,
            kind: "envelope",
            expect: "execution_id",
            execution_id: Some(baseline_id.clone()),
            relation: Some("equals-baseline"),
            canonical_file: None,
            notes: Some(note.to_string()),
        });
    }

    // --- envelope: stored id mismatch fails closed --------------------------
    {
        let mut value = envelope_value(&baseline_id);
        value.as_object_mut().unwrap().insert(
            "execution_id".to_string(),
            json!(format!("blake3:{}", "0".repeat(64))),
        );
        write_file(
            &dir,
            "vectors/envelope-execution-id-mismatch.json",
            &pretty_value(&value),
        );
        vectors.push(error_vector(
            "envelope-execution-id-mismatch",
            "envelope",
            "stored execution_id disagrees with the canonical hash; readers fail closed",
        ));
    }

    // --- invalid vectors via baseline-value injection -----------------------
    let mut invalid = |name: &str, note: &str, mutate: &dyn Fn(&mut Value)| {
        let mut value = base_value.clone();
        mutate(&mut value);
        write_file(&dir, &format!("vectors/{name}.json"), &pretty_value(&value));
        vectors.push(error_vector_owned(
            name.to_string(),
            "contract",
            note.to_string(),
        ));
    };

    invalid(
        "invalid-unknown-identity-field",
        "unknown top-level identity-bearing field fails closed",
        &|v| {
            v.as_object_mut()
                .unwrap()
                .insert("runner".to_string(), json!("local"));
        },
    );
    invalid(
        "invalid-unknown-nested-identity-field",
        "unknown nested identity-bearing field fails closed",
        &|v| {
            v["target"]
                .as_object_mut()
                .unwrap()
                .insert("machine_id".to_string(), json!("machine-77"));
        },
    );
    invalid(
        "invalid-schema-version",
        "contract version mismatch fails closed",
        &|v| v["schema"] = json!("ato.execution-contract/v2"),
    );
    invalid(
        "invalid-placeholder-digest",
        "placeholder digest values are rejected before an id is issued",
        &|v| v["runtime"]["digest"] = json!("unknown"),
    );
    invalid(
        "invalid-uppercase-digest",
        "uppercase hex is non-canonical and rejected",
        &|v| v["runtime"]["digest"] = json!(format!("blake3:{}", "A".repeat(64))),
    );
    invalid(
        "invalid-unsorted-dependencies",
        "identity lists must be pre-sorted and duplicate-free",
        &|v| {
            v["dependencies"] = json!([
                {
                    "name": "zlib",
                    "derivation_digest": format!("blake3:{}", "1c".repeat(32)),
                    "output_digest": format!("blake3:{}", "1d".repeat(32))
                },
                {
                    "name": "npm",
                    "derivation_digest": format!("blake3:{}", "03".repeat(32)),
                    "output_digest": format!("blake3:{}", "04".repeat(32))
                }
            ]);
        },
    );
    invalid(
        "invalid-empty-argv",
        "unresolved launch argv fails closed",
        &|v| v["launch"]["argv"] = json!([]),
    );
    invalid(
        "invalid-null-libc",
        "explicit null is a non-canonical spelling of an absent target.libc; only key omission is canonical",
        &|v| v["target"]["libc"] = Value::Null,
    );
    invalid(
        "invalid-null-port",
        "explicit null is a non-canonical spelling of an absent guest_surface.port; only key omission is canonical",
        &|v| v["guest_surface"]["port"] = Value::Null,
    );
    invalid(
        "invalid-empty-observable-features",
        "explicit empty object is a non-canonical spelling of an absent target.observable_features; only key omission is canonical",
        &|v| v["target"]["observable_features"] = json!({}),
    );
    invalid(
        "invalid-empty-secret-bindings",
        "explicit empty array is a non-canonical spelling of an absent launch.secret_bindings; only key omission is canonical",
        &|v| v["launch"]["secret_bindings"] = json!([]),
    );
    invalid(
        "invalid-relative-cwd",
        "a relative launch.cwd is not a canonical guest path (must be absolute)",
        &|v| v["launch"]["cwd"] = json!("workspace"),
    );
    invalid(
        "invalid-dotdot-target",
        "a '..' segment in external_state[].target is not a canonical guest path",
        &|v| v["external_state"][0]["target"] = json!("/data/../data"),
    );
    invalid(
        "invalid-trailing-slash-target",
        "a trailing slash in external_state[].target is not a canonical guest path",
        &|v| v["external_state"][0]["target"] = json!("/data/"),
    );
    invalid(
        "invalid-zero-port",
        "guest_surface.port 0 is never a valid declared surface and is rejected (NonZeroU16)",
        &|v| v["guest_surface"]["port"] = json!(0),
    );

    // --- opaque digests are BLAKE3-only: a sha256 spelling fails closed -------
    // One per opaque `*_digest` / `value_digest` facet field. The algorithm is
    // fixed by v1 (blake3(UTF8(domain) || 0x00 || JCS(payload))); a
    // producer-supplied sha256 value is not a valid opaque sub-contract digest.
    let sha256 = || json!(format!("sha256:{}", "ab".repeat(32)));
    let sha256_note = |facet: &str| {
        format!(
            "a sha256-spelled {facet} opaque digest is rejected: opaque sub-contract digests are BLAKE3-only"
        )
    };
    invalid(
        "invalid-sha256-source-projection-digest",
        &sha256_note("source projection"),
        &|v| v["source"]["projection_digest"] = sha256(),
    );
    invalid(
        "invalid-sha256-runtime-dynamic-contract-digest",
        &sha256_note("runtime dynamic-contract"),
        &|v| v["runtime"]["dynamic_contract_digest"] = sha256(),
    );
    invalid(
        "invalid-sha256-build-output-projection-digest",
        &sha256_note("build-output projection"),
        &|v| v["build_outputs"][0]["projection_digest"] = sha256(),
    );
    invalid(
        "invalid-sha256-process-model-digest",
        &sha256_note("process-model"),
        &|v| v["launch"]["process_model_digest"] = sha256(),
    );
    invalid(
        "invalid-sha256-environment-policy-digest",
        &sha256_note("environment-policy"),
        &|v| v["launch"]["environment_policy_digest"] = sha256(),
    );
    invalid(
        "invalid-sha256-environment-value-digest",
        &sha256_note("environment-value"),
        &|v| v["launch"]["environment"][0]["value_digest"] = sha256(),
    );
    invalid(
        "invalid-sha256-filesystem-topology-digest",
        &sha256_note("filesystem-topology"),
        &|v| v["filesystem"]["topology_digest"] = sha256(),
    );
    invalid(
        "invalid-sha256-network-policy-digest",
        &sha256_note("network-policy"),
        &|v| v["policy"]["network_digest"] = sha256(),
    );
    invalid(
        "invalid-sha256-capability-policy-digest",
        &sha256_note("capability-policy"),
        &|v| v["policy"]["capability_digest"] = sha256(),
    );
    invalid(
        "invalid-sha256-filesystem-policy-digest",
        &sha256_note("filesystem-policy"),
        &|v| v["policy"]["filesystem_digest"] = sha256(),
    );

    // --- guest-path ordering + control-character canonicalization ------------
    invalid(
        "invalid-guest-path-utf16-order",
        "filesystem.writable_paths sorted by UTF-16 code units is non-canonical: U+10000 before U+E000 is the reverse of the normative UTF-8 byte order (see guest-path-utf8-order)",
        &|v| v["filesystem"]["writable_paths"] = json!(["/\u{10000}", "/\u{e000}"]),
    );
    invalid(
        "invalid-c1-control-target",
        "a C1 control character (U+0085 NEL) in external_state[].target is not a canonical guest path: all Unicode control characters are rejected, not only C0/DEL",
        &|v| v["external_state"][0]["target"] = json!("/data\u{85}x"),
    );

    // --- invalid vectors that require literal duplicate keys ----------------
    // serde_json::Value cannot represent a duplicate key, so these are injected
    // textually into the compact baseline so the duplicate survives in the
    // fixture bytes and reaches the deserializer.
    let compact = serde_json::to_string(&base_value).unwrap();

    let dup_top = compact.replacen(
        '{',
        &format!("{{\"schema\":\"{EXECUTION_CONTRACT_V1_SCHEMA}\","),
        1,
    );
    write_file(
        &dir,
        "vectors/invalid-duplicate-top-level-field.json",
        &format!("{dup_top}\n"),
    );
    vectors.push(error_vector(
        "invalid-duplicate-top-level-field",
        "contract",
        "a duplicate top-level key (last-wins) is rejected by the struct deserializer",
    ));

    let dup_nested = compact.replacen("\"target\":{", "\"target\":{\"os\":\"linux\",", 1);
    write_file(
        &dir,
        "vectors/invalid-duplicate-nested-field.json",
        &format!("{dup_nested}\n"),
    );
    vectors.push(error_vector(
        "invalid-duplicate-nested-field",
        "contract",
        "a duplicate key inside a nested identity object is rejected by the struct deserializer",
    ));

    let dup_feature = compact.replacen(
        "\"target\":{",
        "\"target\":{\"observable_features\":{\"avx512\":\"1\",\"avx512\":\"0\"},",
        1,
    );
    write_file(
        &dir,
        "vectors/invalid-duplicate-observable-feature.json",
        &format!("{dup_feature}\n"),
    );
    vectors.push(error_vector(
        "invalid-duplicate-observable-feature",
        "contract",
        "a duplicate observable-feature map key (BTreeMap last-wins) is rejected by the unique-map visitor",
    ));

    // --- manifest -----------------------------------------------------------
    let manifest = GenManifest {
        schema: EXECUTION_CONTRACT_V1_SCHEMA.to_string(),
        description: "Shared cross-language test vectors for the ato.execution-contract/v1 canonical form. Consumed by crates/capsule/tests/execution_contract_vectors.rs and reusable verbatim by ato-api.".to_string(),
        domain_separator_utf8: EXECUTION_CONTRACT_V1_SCHEMA.to_string(),
        execution_id_formula: "\"blake3:\" + lowercase_hex(BLAKE3(UTF8(domain_separator_utf8) || 0x00 || JCS(contract)))".to_string(),
        jcs: "RFC 8785 (JSON Canonicalization Scheme)".to_string(),
        numbers: "v1 contracts contain only integer numbers: the sole JSON number is guest_surface.port, a nonzero integer in 1..=65535 (0 is rejected fail-closed). No fractional or exponent literals ever appear in a valid contract, so implementations face no RFC 8785 number-serialization ambiguity beyond small integers.".to_string(),
        optional_fields: "Optional identity fields (target.libc, target.observable_features, launch.secret_bindings, guest_surface.port) have exactly one canonical spelling for absence: the key is omitted from the JSON entirely. Explicit null and explicit-empty collections ({} / []) are non-canonical spellings of absence and MUST be rejected before hashing; implementations must never silently normalize them to the omitted form, or semantically identical input would derive different execution_ids across languages. Pinned by the invalid-null-* and invalid-empty-* vectors.".to_string(),
        opaque_digests: "The opaque sub-contract digest fields (source.projection_digest, runtime.dynamic_contract_digest, build_outputs[].projection_digest, launch.process_model_digest, launch.environment_policy_digest, launch.environment[].value_digest, filesystem.topology_digest, policy.network_digest, policy.capability_digest, policy.filesystem_digest) are ALWAYS blake3: their preimage is fixed by v1 as blake3(UTF8(domain) || 0x00 || JCS(payload)) under one of the frozen domains ato.source-projection-contract/v1, ato.runtime-dynamic-contract/v1, ato.build-output-projection/v1, ato.process-model-contract/v1, ato.environment-policy/v1, ato.environment-value/v1, ato.filesystem-topology/v1, ato.network-policy/v1, ato.capability-policy/v1, ato.filesystem-policy/v1. The algorithm is not a producer choice: a sha256-spelled value is NOT a valid opaque sub-contract digest and MUST be rejected before hashing. Only the payload schema behind each digest is versioned separately from v1 (RFC 4.5); the algorithm, domain, and preimage rule are frozen. Pinned by the invalid-sha256-*-digest vectors.".to_string(),
        guest_path_ordering: "Guest-path lists (filesystem.writable_paths) are sorted segment-wise, and WITHIN each segment by UTF-8 byte lexicographic order (equivalently Unicode code-point order). TypeScript implementations MUST compare UTF-8 bytes, NOT UTF-16 code units — the two orders diverge for astral-plane characters: e.g. U+E000 precedes U+10000 by UTF-8/code point, but follows it by UTF-16 (the astral char's lead surrogate U+D800 sorts before the single unit U+E000). Guest paths also reject every Unicode control character (char::is_control: the C0 range incl. NUL, DEL, and the C1 range U+0080..U+009F), not only C0/DEL. Pinned by guest-path-utf8-order (valid, correctly ordered), invalid-guest-path-utf16-order (same pair reversed into UTF-16 order), and invalid-c1-control-target (U+0085 NEL).".to_string(),
        baseline: "baseline".to_string(),
        vectors,
    };
    let mut manifest_json = serde_json::to_string_pretty(&manifest).unwrap();
    manifest_json.push('\n');
    fs::write(dir.join("manifest.json"), manifest_json).unwrap();

    println!("regenerated {} vectors", manifest.vectors.len());
    println!("baseline execution_id = {baseline_id}");
}

fn error_vector(name: &'static str, kind: &'static str, note: &'static str) -> GenVector {
    error_vector_owned(name.to_string(), kind, note.to_string())
}

fn error_vector_owned(name: String, kind: &'static str, note: String) -> GenVector {
    GenVector {
        name: name.clone(),
        file: format!("vectors/{name}.json"),
        kind,
        expect: "error",
        execution_id: None,
        relation: None,
        canonical_file: None,
        notes: Some(note),
    }
}
