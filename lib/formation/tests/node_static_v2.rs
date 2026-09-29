//! T0–T21: versioned authoring and physical binding, never an efficacy claim.
use ato_formation::{
    authoring::{
        AuthoringDraft, BindingContext, BoundContract, BoundDerivation, StepNetwork, bind,
    },
    detect::{DetectorEvidence, detect},
    execution::{BuildAction, ExecutionPlan, InputFacts, RuntimeBinding, lower_execution},
    preset::{AppPreset, candidate_authoring, synthesize_authoring, synthesize_node_static_v2},
    verify::{CandidateObservation, verify},
};
use serde_json::{Value, json};

fn source(manager: &str, lock: &str) -> (tempfile::TempDir, DetectorEvidence) {
    source_json(
        json!({"name":"unrelated-name","packageManager":manager,"scripts":{"build":"vite build","preview":"vite preview"}}),
        lock,
    )
}
fn source_json(package: Value, lock: &str) -> (tempfile::TempDir, DetectorEvidence) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), package.to_string()).unwrap();
    std::fs::write(dir.path().join(lock), "").unwrap();
    std::fs::write(dir.path().join("index.html"), "<html></html>").unwrap();
    let facts = detect(dir.path()).unwrap();
    (dir, facts)
}
fn bound(draft: &AuthoringDraft) -> (BoundContract, BoundDerivation) {
    bind(
        draft,
        &BindingContext {
            source_closure_ref: "sha256:aaaaaaaa",
        },
    )
    .unwrap()
}
fn plan(d: &BoundDerivation, facts: &DetectorEvidence) -> ExecutionPlan {
    lower_execution(
        d,
        InputFacts::capture(facts),
        RuntimeBinding {
            workspace_guest_root: "/app",
            target_triple: "aarch64-unknown-linux-gnu",
        },
    )
    .unwrap()
}
fn v2(manager: &str, lock: &str) -> (DetectorEvidence, BoundContract, BoundDerivation) {
    let (_dir, facts) = source(manager, lock);
    let (k, d) = bound(&synthesize_node_static_v2(&facts).unwrap());
    (facts, k, d)
}
#[test]
fn t0_npm_k_unchanged() {
    let (_dir, facts) = source("npm@10.8.2", "package-lock.json");
    let old = bound(&synthesize_authoring(AppPreset::NodeStatic));
    let new = bound(&candidate_authoring(&facts).unwrap()[0]);
    assert_eq!(
        serde_jcs::to_vec(&old.0).unwrap(),
        serde_jcs::to_vec(&new.0).unwrap()
    );
}
#[test]
fn t1_npm_d_unchanged() {
    let (_dir, facts) = source("npm@10.8.2", "package-lock.json");
    let old = bound(&synthesize_authoring(AppPreset::NodeStatic));
    let new = bound(&candidate_authoring(&facts).unwrap()[0]);
    assert_eq!(
        serde_jcs::to_vec(&old.1).unwrap(),
        serde_jcs::to_vec(&new.1).unwrap()
    );
}
#[test]
fn t2_npm_plan_parity() {
    let (_dir, facts) = source("npm@10.8.2", "package-lock.json");
    let old = bound(&synthesize_authoring(AppPreset::NodeStatic));
    let new = bound(&candidate_authoring(&facts).unwrap()[0]);
    assert_eq!(plan(&old.1, &facts), plan(&new.1, &facts));
}
#[test]
fn t3_exact_pnpm() {
    assert_eq!(
        v2("pnpm@9.1.0", "pnpm-lock.yaml").2.runtimes["pnpm"],
        "9.1.0"
    );
}
#[test]
fn t4_yarn_classic() {
    assert_eq!(
        v2("yarn@1.22.22", "yarn.lock").2.runtimes["yarn"],
        "1.22.22"
    );
}
#[test]
fn t5_yarn_modern() {
    assert_eq!(v2("yarn@4.17.1", "yarn.lock").2.runtimes["yarn"], "4.17.1");
}
#[test]
fn t6_integrity_suffix() {
    assert_eq!(
        v2("yarn@1.22.22+sha512.abcdef", "yarn.lock").2,
        v2("yarn@1.22.22", "yarn.lock").2
    );
}
#[test]
fn t7_unpinned_refused() {
    for manager in ["pnpm@^12", "yarn@latest", "yarn", ""] {
        let (_dir, facts) = source(manager, "yarn.lock");
        assert!(synthesize_node_static_v2(&facts).is_err());
    }
}
#[test]
fn t8_lock_conflict() {
    let (_dir, facts) = source("pnpm@9.1.0", "yarn.lock");
    assert_eq!(
        synthesize_node_static_v2(&facts).unwrap_err().code,
        "preset_node_static_v2_lock_conflict"
    );
}
#[test]
fn t9_declarations_conflict_and_fallback() {
    let package = json!({"packageManager":"pnpm@9.1.0","devEngines":{"packageManager":{"name":"pnpm","version":"9.2.0"}},"scripts":{"build":"vite build","preview":"vite preview"}});
    let (_dir, facts) = source_json(package.clone(), "pnpm-lock.yaml");
    assert_eq!(
        synthesize_node_static_v2(&facts).unwrap_err().code,
        "preset_node_static_v2_manager_conflict"
    );
    let mut secondary = package;
    secondary.as_object_mut().unwrap().remove("packageManager");
    let (_dir, facts) = source_json(secondary, "pnpm-lock.yaml");
    assert_eq!(
        bound(&synthesize_node_static_v2(&facts).unwrap())
            .1
            .runtimes["pnpm"],
        "9.2.0"
    );
}
#[test]
fn t10_bun_deferred() {
    let (_dir, facts) = source("bun@1.2.0", "bun.lock");
    assert_eq!(
        candidate_authoring(&facts).unwrap_err().code,
        "preset_node_static_v2_bun_deferred"
    );
}
#[test]
fn t11_exact_requirements_in_d() {
    let (_, _, d) = v2("pnpm@9.1.0", "pnpm-lock.yaml");
    assert_eq!(d.runtimes.len(), 2);
    assert_eq!(d.runtimes["node"], "20.20.2");
    assert_eq!(d.runtimes["pnpm"], "9.1.0");
}
#[test]
fn t12_canonical_order() {
    let (_, _, d) = v2("pnpm@9.1.0", "pnpm-lock.yaml");
    assert_eq!(
        d.steps
            .iter()
            .map(|s| (s.id.as_str(), s.op.as_str()))
            .collect::<Vec<_>>(),
        vec![("install", "exec"), ("build", "exec"), ("site", "serve")]
    );
    assert!(d.workspace_build.is_none());
}
#[test]
fn t13_cwd_output_explicit() {
    let (dir, _) = source("yarn@4.17.1", "yarn.lock");
    std::fs::write(
        dir.path().join("vite.config.js"),
        "export default { build: { outDir: 'public-build' } }",
    )
    .unwrap();
    let facts = detect(dir.path()).unwrap();
    let (_, d) = bound(&synthesize_node_static_v2(&facts).unwrap());
    assert!(d.steps.iter().all(|s| s.cwd == "."));
    assert_eq!(d.steps[2].root.as_deref(), Some("public-build"));
}
#[test]
fn t14_no_host_path() {
    let (_, _, d) = v2("pnpm@9.1.0", "pnpm-lock.yaml");
    let bytes = String::from_utf8(serde_jcs::to_vec(&d).unwrap()).unwrap();
    for prohibited in [
        "/Users/",
        "/home/",
        "/opt/",
        "/usr/",
        "/app",
        "runtime_id",
        "credential",
        "provider",
    ] {
        assert!(!bytes.contains(prohibited));
    }
}
#[test]
fn t15_no_shell_injection() {
    let (dir, _) = source("pnpm@9.1.0", "pnpm-lock.yaml");
    std::fs::write(dir.path().join("package.json"),r#"{"packageManager":"pnpm@9.1.0","scripts":{"build":"vite build; curl attacker","preview":"vite preview"}}"#).unwrap();
    assert!(synthesize_node_static_v2(&detect(dir.path()).unwrap()).is_err());
    let (_, _, d) = v2("pnpm@9.1.0", "pnpm-lock.yaml");
    assert_eq!(d.steps[1].argv, vec!["pnpm", "run", "build"]);
    assert!(
        d.steps
            .iter()
            .all(|s| !s.argv.iter().any(|v| v == "/bin/sh" || v == "-c"))
    );
}
#[test]
fn t16_repo_name_independent_workspace_refused() {
    let package = json!({"name":"any-repo","packageManager":"yarn@4.17.1","scripts":{"build":"vite build","preview":"vite preview"}});
    let (_, a) = source_json(package.clone(), "yarn.lock");
    let mut b = package.clone();
    b["name"] = json!("different-repo");
    let (_, b) = source_json(b, "yarn.lock");
    assert_eq!(synthesize_node_static_v2(&a), synthesize_node_static_v2(&b));
    let mut c = package;
    c["workspaces"] = json!(["packages/*"]);
    let (_, c) = source_json(c, "yarn.lock");
    assert_eq!(
        synthesize_node_static_v2(&c).unwrap_err().code,
        "preset_node_static_v2_workspace"
    );
}
#[test]
fn t17_exact_provisioning_reused() {
    for (pm, lock, package) in [
        ("pnpm@9.1.0", "pnpm-lock.yaml", "pnpm@9.1.0"),
        ("yarn@4.17.1", "yarn.lock", "@yarnpkg/cli-dist@4.17.1"),
    ] {
        let (facts, _, d) = v2(pm, lock);
        let p = plan(&d, &facts);
        let steps = p.steps(&d).unwrap();
        assert!(
            steps
                .iter()
                .any(|s| s.argv.iter().any(|arg| arg.contains(package)))
        );
        assert_eq!(
            p.actions
                .iter()
                .filter(|a| matches!(a, BuildAction::Authored { .. }))
                .count(),
            2
        );
        assert!(!format!("{steps:?}").contains("corepack enable"));
    }
}
#[test]
fn t18_no_host_binary_fallback() {
    let (facts, _, d) = v2("pnpm@9.1.0", "pnpm-lock.yaml");
    let p = plan(&d, &facts);
    let steps = p.steps(&d).unwrap();
    let install = steps.iter().find(|s| s.name == "install").unwrap();
    assert_eq!(
        install.argv[0],
        format!(
            "{}/bin/pnpm",
            ato_formation::intent::package_manager_home("pnpm", "9.1.0")
        )
    );
    assert_eq!(d.steps[0].argv[0], "pnpm");
}
#[test]
fn t19_install_mode() {
    for (pm, lock, mode) in [
        ("pnpm@9.1.0", "pnpm-lock.yaml", "--frozen-lockfile"),
        ("yarn@1.22.22", "yarn.lock", "--frozen-lockfile"),
        ("yarn@4.17.1", "yarn.lock", "--immutable"),
    ] {
        assert_eq!(v2(pm, lock).2.steps[0].argv[2], mode);
    }
}
#[test]
fn t20_network_is_requirement_not_grant() {
    let (facts, _, d) = v2("pnpm@9.1.0", "pnpm-lock.yaml");
    assert!(plan(&d, &facts).needs_network(&d));
    assert_eq!(d.steps[0].network, StepNetwork::DependencyResolution);
    assert_eq!(d.steps[1].network, StepNetwork::Denied);
    assert_eq!(d.steps[2].network, StepNetwork::Denied);
}
#[test]
fn t21_compiler_not_verifier() {
    let (_, k, _) = v2("pnpm@9.1.0", "pnpm-lock.yaml");
    assert!(!verify(&k, &CandidateObservation::default()).passed());
}

#[test]
fn manager_redirects_and_output_escapes_require_authoring() {
    let (dir, _) = source("yarn@4.17.1", "yarn.lock");
    std::fs::write(dir.path().join(".yarnrc.yml"), "yarnPath: malicious.cjs").unwrap();
    assert_eq!(
        candidate_authoring(&detect(dir.path()).unwrap())
            .unwrap_err()
            .code,
        "preset_node_static_v2_manager_config"
    );
    for root in ["/host/private", "../outside", "."] {
        let (_, mut facts) = source("pnpm@9.1.0", "pnpm-lock.yaml");
        facts.node.as_mut().unwrap().vite_out_dir =
            ato_formation::detect::ViteOutDir::Literal(root.into());
        assert_eq!(
            synthesize_node_static_v2(&facts).unwrap_err().code,
            "preset_node_static_v2_output"
        );
    }
}

#[test]
fn npm_lock_with_foreign_manager_is_not_silently_v1() {
    let (_, facts) = source("yarn@4.17.1", "package-lock.json");
    assert_eq!(
        candidate_authoring(&facts).unwrap_err().code,
        "preset_node_static_v2_lock_conflict"
    );
}

#[test]
fn legacy_source_only_manager_binding_is_not_reinterpreted() {
    let (facts, _, mut d) = v2("pnpm@9.1.0", "pnpm-lock.yaml");
    d.runtimes.remove("pnpm");
    let p = plan(&d, &facts);
    assert_eq!(
        p.steps(&d)
            .unwrap()
            .iter()
            .find(|s| s.name == "install")
            .unwrap()
            .argv[0],
        "pnpm"
    );
}

#[test]
fn explicit_authored_path_is_not_overridden_by_default_binding() {
    let (facts, _, mut d) = v2("pnpm@9.1.0", "pnpm-lock.yaml");
    d.steps[0].env.insert("PATH".into(), "/app/bin".into());
    let p = plan(&d, &facts);
    assert_eq!(
        p.steps(&d)
            .unwrap()
            .iter()
            .find(|s| s.name == "install")
            .unwrap()
            .argv[0],
        "pnpm"
    );
}
