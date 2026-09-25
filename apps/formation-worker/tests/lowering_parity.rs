//! Semantic baseline captured before 2f; internal IR digests are not identities.
use ato_formation::{
    capsule_toml::parse_capsule_toml,
    detect::detect,
    source::{RESOLVER_CONTRACT_V1, SourceClosureRef},
};
use ato_formation_worker::{job::plan_candidate, runtime_network::derivation_requirements};
use std::collections::BTreeMap;

#[test]
fn lowering_preserves_canonical_identity_and_physical_behavior() {
    let mut actual = BTreeMap::new();
    for case in [
        "static",
        "python",
        "node",
        "pnpm",
        "yarn",
        "exec",
        "build-serve",
    ] {
        let tree = tempfile::tempdir().unwrap();
        std::fs::write(tree.path().join("app.py"), "print('fixture')").unwrap();
        std::fs::write(tree.path().join("index.html"), "fixture").unwrap();
        let mut text = String::from(
            "schema = \"ato.capsule/1\"\n[[input]]\nid = \"workspace\"\nuse = \"ato.workspace@1\"\npath = \".\"\n",
        );
        if case != "static" {
            let (runtime, version) = if case == "python" || case == "exec" {
                ("python", "3.12.7")
            } else {
                ("node", "22.14.0")
            };
            text += &format!("[[runtime]]\nname = {runtime:?}\nversion = {version:?}\n");
        }
        if case == "pnpm" || case == "yarn" {
            let version = if case == "pnpm" { "9.15.4" } else { "1.22.22" };
            std::fs::write(
                tree.path().join("package.json"),
                format!("{{\"packageManager\":\"{case}@{version}\"}}"),
            )
            .unwrap();
        }
        if case == "exec" || case == "build-serve" {
            text += "[[derive.step]]\nid = \"prepare\"\nuse = \"ato.process@1\"\nop = \"exec\"\nargv = [\"/bin/echo\", \"a b\"]\ncwd = \".\"\n[derive.step.env]\nBUILD_MODE = \"fixture\"\n";
        }
        if case == "static" {
            text += "[[derive.step]]\nid = \"site\"\nuse = \"ato.browser@1\"\nop = \"serve\"\nsource = \"workspace\"\nroot = \"\"\nentry = \"index.html\"\n";
        } else {
            let executable = if case == "python" || case == "exec" {
                "python3"
            } else {
                "node"
            };
            text += &format!(
                "[[derive.step]]\nid = \"site\"\nuse = \"ato.process@1\"\nop = \"serve\"\nargv = [{executable:?}, \"app.py\", \"a b\"]\n[derive.step.env]\nMODE = \"fixture\"\n"
            );
        }
        text += "[[port]]\nid = \"app.http\"\nuse = \"ato.http@1\"\nfrom = \"site\"\nguest_port = 8000\n[[contract.require]]\nid = \"http\"\nuse = \"ato.contract.http@1\"\nport = \"app.http\"\nmethod = \"GET\"\npath = \"/\"\n[contract.require.expect]\nstatus = 200\n";
        let draft = parse_capsule_toml(&text).unwrap();
        let source = SourceClosureRef::derive(
            &format!("sha256:{}", "1".repeat(64)),
            "",
            RESOLVER_CONTRACT_V1,
        )
        .unwrap();
        let planned = plan_candidate(
            &draft,
            &source,
            &detect(tree.path()).unwrap(),
            BTreeMap::new(),
            "/app",
            "x86_64-linux-gnu",
        )
        .unwrap();
        let (requirements, provisions) = derivation_requirements(&planned);
        actual.insert(
            case,
            serde_json::json!({
                "contract_ref":planned.contract_ref, "derivation_ref":planned.derivation_ref,
                "argv":planned.plan.serving(&planned.derivation).argv, "cwd":planned.plan.serving(&planned.derivation).cwd,
                "env":planned.plan.process_environment(&planned.derivation), "build_steps":planned.plan.steps(&planned.derivation).unwrap(),
                "toolchain_path":planned.plan.toolchain_path,
                "requirements":requirements, "provisions":provisions,
                "static_root":if planned.plan.lane.is_process() { None } else { Some(planned.plan.serving(&planned.derivation).root.clone().unwrap_or_default()) },
            }),
        );
    }
    let bytes = serde_json::to_vec_pretty(&actual).unwrap();
    if let Some(path) = std::env::var_os("ATO_TEST_CAPTURE_LOWERING_BASELINE") {
        std::fs::write(path, &bytes).unwrap();
    }
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/lowering-before-2f.json")).unwrap();
    assert_eq!(serde_json::to_value(actual).unwrap(), expected);
}

#[path = "support/legacy_plan.rs"]
mod legacy_plan;

#[test]
fn preset_frontends_keep_k_d_and_build_commands_without_reconstructing_intent() {
    use ato_formation::preset::{AppPreset, synthesize_authoring};
    for preset in [
        AppPreset::SingleHtml,
        AppPreset::StaticFiles,
        AppPreset::NodeStatic,
        AppPreset::SingleJsx,
    ] {
        let tree = tempfile::tempdir().unwrap();
        match preset {
            AppPreset::SingleJsx => std::fs::write(
                tree.path().join("App.jsx"),
                "export default function App(){return <h1>hello</h1>}",
            )
            .unwrap(),
            AppPreset::NodeStatic => {
                std::fs::write(tree.path().join("package.json"),r#"{"scripts":{"build":"vite build"},"devDependencies":{"vite":"5.0.0"},"engines":{"node":">=20"}}"#).unwrap();
                std::fs::write(tree.path().join("package-lock.json"), "{}").unwrap();
                std::fs::write(tree.path().join("index.html"), "hello").unwrap();
            }
            _ => std::fs::write(tree.path().join("index.html"), "hello").unwrap(),
        }
        let evidence = detect(tree.path()).unwrap();
        let draft = synthesize_authoring(preset);
        let source = SourceClosureRef::derive(
            &format!("sha256:{}", "2".repeat(64)),
            "",
            RESOLVER_CONTRACT_V1,
        )
        .unwrap();
        let old = legacy_plan::plan_candidate(
            &draft,
            &source,
            &evidence,
            BTreeMap::new(),
            "/app",
            "x86_64-linux-gnu",
        )
        .unwrap();
        let new = plan_candidate(
            &draft,
            &source,
            &evidence,
            BTreeMap::new(),
            "/app",
            "x86_64-linux-gnu",
        )
        .unwrap();
        assert_eq!(old.contract_ref, new.contract_ref);
        assert_eq!(old.derivation_ref, new.derivation_ref);
        assert_eq!(
            old.plan.steps,
            new.plan
                .steps(&new.derivation)
                .unwrap()
                .into_iter()
                .map(|s| s.into_owned())
                .collect::<Vec<_>>()
        );
        assert_eq!(old.plan.runtime, new.plan.toolchains);
        assert_eq!(
            old.intent.static_output_root.as_deref(),
            Some(
                new.plan
                    .serving(&new.derivation)
                    .root
                    .as_deref()
                    .unwrap_or_default()
            )
        );
        assert_eq!(old.plan.toolchain_path, new.plan.toolchain_path);
        if matches!(preset, AppPreset::SingleHtml | AppPreset::StaticFiles) {
            // Same source, materializer and physical binding: same artifact.
            let first = tempfile::tempdir().unwrap();
            let second = tempfile::tempdir().unwrap();
            let a = legacy_execution::materialize_static(
                &old.intent,
                &old.plan,
                tree.path(),
                first.path(),
                "parity",
                &[],
            )
            .unwrap();
            let b = ato_runtime_attempt::static_lane::materialize_static(
                &new.derivation,
                &new.plan,
                tree.path(),
                second.path(),
                "parity",
                &[],
            )
            .unwrap();
            assert_eq!(a.manifest_digest, b.manifest_digest);
            assert_eq!(a.bundle.manifest_bytes, b.bundle.manifest_bytes);
        }
    }
}
#[path = "support/legacy_execution.rs"]
mod legacy_execution;

#[test]
fn canonical_arguments_are_not_roundtripped_through_a_command_string() {
    let tree = tempfile::tempdir().unwrap();
    std::fs::write(tree.path().join("app.py"), "print('fixture')").unwrap();
    let text = r#"
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
argv = ["python3", "app.py", "", "a b", "a'quote", 'a"quote', "$HOME", "a\\b"]
[[port]]
id = "app.http"
use = "ato.http@1"
from = "app"
guest_port = 8000
[[contract.require]]
id = "http"
use = "ato.contract.http@1"
port = "app.http"
method = "GET"
path = "/"
[contract.require.expect]
status = 200
"#;
    let draft = parse_capsule_toml(text).unwrap();
    let source = SourceClosureRef::derive(
        &format!("sha256:{}", "3".repeat(64)),
        "",
        RESOLVER_CONTRACT_V1,
    )
    .unwrap();
    let evidence = detect(tree.path()).unwrap();
    let planned = plan_candidate(
        &draft,
        &source,
        &evidence,
        BTreeMap::new(),
        "/app",
        "x86_64-linux-gnu",
    )
    .unwrap();
    assert_eq!(
        planned.plan.serving(&planned.derivation).argv,
        [
            "python3", "app.py", "", "a b", "a'quote", "a\"quote", "$HOME", "a\\b"
        ]
    );
    let error = plan_candidate(
        &draft,
        &source,
        &evidence,
        BTreeMap::from([("launch.argv".to_owned(), "python3 different.py".to_owned())]),
        "/app",
        "x86_64-linux-gnu",
    )
    .err()
    .expect("post-binding override refused");
    assert!(error.to_string().contains("AuthoringDraft"), "{error}");
}
