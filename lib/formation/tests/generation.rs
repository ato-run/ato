use ato_formation::authoring::{BindingContext, BoundContract, BoundDerivation, bind};
use ato_formation::capsule_toml::parse_capsule_toml;
use ato_formation::generation::*;
use std::collections::BTreeMap;

const BASE: &str = r#"
schema = "ato.capsule/1"
[[input]]
id = "workspace"
use = "ato.workspace@1"
path = "."
[[runtime]]
name = "python"
version = "3.12.7"
[[derive.step]]
id = "app"
use = "ato.process@1"
op = "serve"
argv = ["/opt/ato/toolchains/python/3.12.7/bin/python3", "-B", "/app/broken.py"]
[[port]]
id = "app.http"
use = "ato.http@1"
from = "app"
guest_port = 8000
[[contract.require]]
id = "root"
use = "ato.contract.http@1"
port = "app.http"
method = "GET"
path = "/"
[contract.require.expect]
status = 200
[[contract.require]]
id = "source"
use = "ato.contract.workspace@1"
input = "workspace"
[contract.require.expect]
digest = "capture"
"#;

fn closure() -> String {
    format!("sha256:{}", "a".repeat(64))
}
fn bound(text: &str) -> (BoundContract, BoundDerivation) {
    bind(
        &parse_capsule_toml(text).unwrap(),
        &BindingContext {
            source_closure_ref: &closure(),
        },
    )
    .unwrap()
}
fn policy(text: &str) -> GenerationPolicy {
    GenerationPolicy {
        schema: GENERATION_POLICY_SCHEMA.into(),
        base_derivation_ref: bound(text).1.derivation_ref().unwrap(),
        entrypoints: BTreeMap::from([("entry_a".into(), "working.py".into())]),
        max_generations: 1,
        timeout_ms: 5_000,
    }
}
fn draft() -> GenerationDraft {
    GenerationDraft {
        schema: GENERATION_DRAFT_SCHEMA.into(),
        operation: GenerationOperation::PythonScript,
        entrypoint_id: "entry_a".into(),
    }
}
fn generate(text: &str, scope: &GenerationPolicy) -> Result<CompiledGeneration, GenerationError> {
    compile(
        scope,
        text,
        &closure(),
        &bound(text).0.contract_ref().unwrap(),
        &draft(),
    )
}

#[test]
fn a_field_choice_forms_a_new_canonical_d_with_exactly_the_same_k_and_scope() {
    let (k, original) = bound(BASE);
    let result = generate(BASE, &policy(BASE)).unwrap();
    let (new_k, new_d) = bound(&result.capsule_toml);
    assert_eq!(new_k, k);
    assert_eq!(new_d, result.derivation);
    assert_eq!(result.derivation_ref, new_d.derivation_ref().unwrap());
    assert_ne!(result.derivation_ref, original.derivation_ref().unwrap());
    let mut expected = original;
    expected.steps[0].argv[2] = "/app/working.py".into();
    assert_eq!(new_d, expected);
    assert_eq!(result.base_contract_ref, k.contract_ref().unwrap());
    assert_eq!(generate(BASE, &policy(BASE)).unwrap(), result);
}

#[test]
fn logical_labels_and_toml_format_do_not_change_d_identity() {
    let result = generate(BASE, &policy(BASE)).unwrap();
    let mut scope = policy(BASE);
    scope.entrypoints = BTreeMap::from([("renamed".into(), "working.py".into())]);
    let mut d = draft();
    d.entrypoint_id = "renamed".into();
    let with_comment = format!("# author provenance\n{BASE}");
    let renamed = compile(
        &scope,
        &with_comment,
        &closure(),
        &result.base_contract_ref,
        &d,
    )
    .unwrap();
    assert_eq!(result, renamed);
}

#[test]
fn same_d_is_a_duplicate_even_when_its_logical_label_is_different() {
    let mut scope = policy(BASE);
    scope
        .entrypoints
        .insert("entry_a".into(), "broken.py".into());
    assert_eq!(
        generate(BASE, &scope).unwrap_err().code(),
        "generation_duplicate"
    );
}

#[test]
fn private_paths_and_model_strings_cannot_be_submitted_as_fields() {
    for field in [
        "argv",
        "env",
        "network",
        "bindings",
        "state",
        "source",
        "contract",
        "runtime",
        "permissions",
    ] {
        let mut value = serde_json::to_value(draft()).unwrap();
        value[field] = serde_json::json!("SECRET_CANARY");
        assert!(
            serde_json::from_value::<GenerationDraft>(value).is_err(),
            "{field}"
        );
    }
    for operation in [
        "shell",
        "python_module",
        "source_patch",
        "python_script;curl",
    ] {
        let mut value = serde_json::to_value(draft()).unwrap();
        value["operation"] = operation.into();
        assert!(serde_json::from_value::<GenerationDraft>(value).is_err());
    }
    let scope = policy(BASE);
    let k = bound(BASE).0.contract_ref().unwrap();
    for id in ["working.py", "unknown", "none"] {
        let mut d = draft();
        d.entrypoint_id = id.into();
        assert!(compile(&scope, BASE, &closure(), &k, &d).is_err());
    }
}

#[test]
fn source_and_contract_and_parent_refs_cannot_be_rebound() {
    let scope = policy(BASE);
    let k = bound(BASE).0.contract_ref().unwrap();
    let other = format!("sha256:{}", "b".repeat(64));
    assert!(compile(&scope, BASE, &other, &k, &draft()).is_err());
    assert!(compile(&scope, BASE, &closure(), &other, &draft()).is_err());
    let changed_k = BASE.replace("status = 200", "status = 201");
    assert_eq!(
        compile(&scope, &changed_k, &closure(), &k, &draft())
            .unwrap_err()
            .code(),
        "generation_contract_mismatch"
    );
    let changed_d = BASE.replace("guest_port = 8000", "guest_port = 9000");
    assert_eq!(
        compile(&scope, &changed_d, &closure(), &k, &draft())
            .unwrap_err()
            .code(),
        "generation_base_mismatch"
    );
}

#[test]
fn paths_ids_schema_and_domain_sizes_are_bounded() {
    for path in [
        "/app/a.py",
        "../a.py",
        "x/../a.py",
        "./a.py",
        ".secret.py",
        "x/.secret/a.py",
        "x//a.py",
        "x\\a.py",
        "a.py\0",
        "https://host/a.py",
        "a;curl.py",
        "$(id).py",
        "a.py/",
        "a.txt",
    ] {
        let mut scope = policy(BASE);
        scope.entrypoints.insert("entry_a".into(), path.into());
        assert!(scope.validate().is_err(), "{path:?}");
    }
    for id in [
        "none",
        "",
        "a/b",
        "a.b",
        "a-b",
        "secret\n",
        "abcdefghijklmnopqrstuvwxyz1234567",
    ] {
        let mut scope = policy(BASE);
        scope.entrypoints = BTreeMap::from([(id.into(), "working.py".into())]);
        assert!(scope.validate().is_err());
    }
    let mut scope = policy(BASE);
    scope.entrypoints = (0..17)
        .map(|i| (format!("e{i}"), format!("e{i}.py")))
        .collect();
    assert!(scope.validate().is_err());
    scope = policy(BASE);
    scope
        .entrypoints
        .insert("alias".into(), "working.py".into());
    assert!(scope.validate().is_err());
    for timeout in [0, 30_001, u64::MAX] {
        scope = policy(BASE);
        scope.timeout_ms = timeout;
        assert!(scope.validate().is_err());
    }
    scope = policy(BASE);
    scope.max_generations = 2;
    assert!(scope.validate().is_err());
    scope = policy(BASE);
    scope.schema.push_str("-unknown");
    assert!(scope.validate().is_err());
}

#[test]
fn even_owner_supplied_unsafe_parent_shapes_are_outside_this_language() {
    let variants = [
        BASE.replace("op = \"serve\"", "op = \"serve\"\nenv = { TOKEN = \"SECRET_CANARY\" }"),
        BASE.replace("schema = \"ato.capsule/1\"", "schema = \"ato.capsule/1\"\n[effects]\ndefault = \"non-repeatable\""),
        BASE.replace("\"-B\", \"/app/broken.py\"", "\"-c\", \"print(1)\""),
        BASE.replace("/opt/ato/toolchains/python/3.12.7/bin/python3", "/usr/bin/python3"),
        BASE.replace("op = \"serve\"", "op = \"serve\"\ncwd = \"../outside\""),
        BASE.replace("version = \"3.12.7\"", "version = \">=3.12\""),
        BASE.replace("version = \"3.12.7\"", "version = \"3.12.7+untrusted\""),
        format!("{BASE}\n[[state]]\nid = \"data\"\nuse = \"ato.state.filesystem@1\"\nmount = \"/data\"\naccess = \"read-write\"\n"),
        BASE.replace("[[derive.step]]", "[[derive.step]]\nid = \"prep\"\nuse = \"ato.process@1\"\nop = \"exec\"\nargv = [\"/bin/sh\", \"-c\", \"true\"]\nnetwork = \"dependency-resolution\"\n[[derive.step]]"),
    ];
    for text in variants {
        let error = generate(&text, &policy(&text)).unwrap_err();
        assert_eq!(error.code(), "generation_base_unsupported");
        assert!(!error.to_string().contains("SECRET_CANARY"));
    }
}

#[test]
fn parser_failures_never_return_secret_source_diagnostics() {
    let scope = policy(BASE);
    let k = bound(BASE).0.contract_ref().unwrap();
    for text in [
        BASE.replace(
            "op = \"serve\"",
            "op = \"serve\"\nnetwork = \"SECRET_CANARY\"",
        ),
        BASE.replace(
            "op = \"serve\"",
            "op = \"serve\"\nbindings = [\"SECRET_CANARY\"]",
        ),
        BASE.replace("schema = \"ato.capsule/1\"", "schema = \"SECRET_CANARY\""),
        "SECRET_CANARY = [".into(),
    ] {
        let error = compile(&scope, &text, &closure(), &k, &draft()).unwrap_err();
        assert_eq!(error.code(), "generation_base_invalid");
        assert!(!error.to_string().contains("SECRET_CANARY"));
    }
}

#[cfg(feature = "planning")]
#[test]
fn generated_d_uses_the_same_existing_lowering_and_pinned_toolchain() {
    use ato_formation::detect::DetectorEvidence;
    use ato_formation::execution::{InputFacts, RuntimeBinding, lower_execution};
    let result = generate(BASE, &policy(BASE)).unwrap();
    let facts = DetectorEvidence {
        present_files: vec!["broken.py".into(), "working.py".into()],
        ..Default::default()
    };
    let plan = lower_execution(
        &result.derivation,
        InputFacts::capture(&facts),
        RuntimeBinding {
            workspace_guest_root: "/app",
            target_triple: "x86_64-unknown-linux-gnu",
        },
    )
    .unwrap();
    assert_eq!(
        plan.toolchains.get("python").map(String::as_str),
        Some("3.12.7")
    );
    assert_eq!(plan.serving(&result.derivation).argv[2], "/app/working.py");
    // Platform-owned provisioning is unchanged, not a new permission from the model.
    assert!(plan.needs_network(&result.derivation));
}
