//! Formation 6b-B: bounded workspace inventory and the typed
//! `node_static_workspace@1` operation. Pure inventory/compiler tests: nothing
//! here executes a Runtime or attests K (W9/W12 are actual Linux acceptance).
use ato_formation::{
    authoring::{BindingContext, BoundContract, bind},
    capsule_toml::parse_capsule_toml,
    preset::{AppPreset, candidate_authoring, synthesize_authoring},
    proposal::*,
    search::*,
    workspace::{Installation, InventoryError, Qualification, inventory},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path};

const CLOSURE: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}
fn static_ws(root: &Path, dir: &str, name: &str) {
    write(
        root,
        &format!("{dir}/package.json"),
        &json!({"name":name,"private":true,
            "scripts":{"build":"vite build","preview":"vite preview"}})
        .to_string(),
    );
    write(root, &format!("{dir}/index.html"), "<!doctype html><title>w</title>");
}
fn server_ws(root: &Path, dir: &str) {
    write(
        root,
        &format!("{dir}/package.json"),
        &json!({"name":"server","scripts":{"build":"vite build","preview":"vite preview"},
            "dependencies":{"express":"4.21.2"}})
        .to_string(),
    );
    write(root, &format!("{dir}/index.html"), "<!doctype html>");
}
/// Yarn 1 monorepo: one static frontend, one server-only package.
fn monorepo(root: &Path, workspaces: Value) {
    write(
        root,
        "package.json",
        &json!({"private":true,"name":"mono","packageManager":"yarn@1.22.22",
            "workspaces":workspaces,"scripts":{"build":"yarn --cwd ./apps/web build"}})
        .to_string(),
    );
    write(root, "yarn.lock", "# yarn lockfile v1\n\n\n");
    static_ws(root, "apps/web", "web");
    server_ws(root, "packages/server");
}

fn k() -> BoundContract {
    bind(
        &synthesize_authoring(AppPreset::NodeStatic),
        &BindingContext {
            source_closure_ref: CLOSURE,
        },
    )
    .unwrap()
    .0
}
fn frozen(auth: NodeStaticWorkspaceAuthorization) -> FrozenSearchV1 {
    let mut s: SearchStateV1 =
        serde_json::from_str(include_str!("fixtures/search-state/d1-failed.json")).unwrap();
    s.frozen.candidates.clear();
    s.frozen.base_contract = k();
    s.frozen.base_contract_ref = s.frozen.base_contract.contract_ref().unwrap();
    s.frozen.contract_ref = s.frozen.base_contract_ref.clone();
    s.frozen.initial_source = Some(InitialSource {
        closure_ref: CLOSURE.into(),
        archive_digest: format!("sha256:{}", "b".repeat(64)),
    });
    s.frozen.policy.proposal = Some(ProposalAuthorization {
        modifiable_derivation_refs: vec![],
        source_domain: SourceDomain {
            entrypoints: BTreeMap::new(),
            modules: BTreeMap::new(),
        },
        python_http_process: None,
        node_static_workspace: Some(auth),
        policy: CandidateProducerPolicy {
            max_proposal_rounds: 1,
            max_proposals: 4,
            timeout_ms: 5000,
            allow_source_text: false,
            max_source_bytes: 0,
        },
    });
    s.frozen
}
fn batch(proposals: Vec<Value>) -> ProducerOutput {
    raw(&serde_json::to_vec(&json!({"schema":PROPOSAL_SCHEMA,"proposals":proposals})).unwrap())
}
fn raw(bytes: &[u8]) -> ProducerOutput {
    ProducerOutput::new(
        bytes.to_vec(),
        ProducerProvenance {
            provider: "fixed".into(),
            model: None,
        },
    )
    .unwrap()
}
fn select(id: &str) -> Value {
    json!({"kind":"propose_derivation","operations":[{"operation":"node_static_workspace@1","workspace_id":id}]})
}
fn outcomes(frozen: &FrozenSearchV1, proposals: Vec<Value>) -> Vec<ProposalOutcome> {
    CandidateRegistry::new(frozen)
        .unwrap()
        .validate_batch(&BTreeMap::new(), &batch(proposals))
        .unwrap()
}
fn rejected(outcome: &ProposalOutcome) -> &'static str {
    match outcome {
        ProposalOutcome::Rejected(e) => e.0,
        other => panic!("expected rejection, got {other:?}"),
    }
}
fn qualified(root: &Path) -> NodeStaticWorkspaceAuthorization {
    inventory(root).unwrap().authorization().unwrap()
}

#[test]
fn w0_no_workspace_publishes_no_operation() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "package.json", r#"{"name":"single"}"#);
    assert_eq!(inventory(dir.path()), Err(InventoryError::Absent));
    let mut auth = frozen(qualified_fixture()).policy.proposal.unwrap();
    auth.node_static_workspace = None;
    auth.source_domain
        .entrypoints
        .insert("entry_a".into(), "server.py".into());
    let catalog = serde_json::to_string(&auth.catalog().unwrap()).unwrap();
    assert!(!catalog.contains("node_static_workspace"));
}

fn qualified_fixture() -> NodeStaticWorkspaceAuthorization {
    let dir = tempfile::tempdir().unwrap();
    monorepo(dir.path(), json!(["apps/*", "packages/*"]));
    qualified(dir.path())
}

#[test]
fn w1_one_static_workspace_is_one_opaque_id_and_no_path_reaches_the_provider() {
    let dir = tempfile::tempdir().unwrap();
    monorepo(dir.path(), json!(["apps/*", "packages/*"]));
    let inv = inventory(dir.path()).unwrap();
    assert_eq!(inv.workspaces.len(), 2);
    let auth = inv.authorization().unwrap();
    assert_eq!(auth.workspaces.len(), 1);
    let (id, build) = auth.workspaces.iter().next().unwrap();
    assert_eq!(id, "w_0");
    assert_eq!(build.cwd, "apps/web");
    assert_eq!(build.output_root, "apps/web/dist");
    let frozen = frozen(auth);
    let catalog = frozen.policy.proposal.as_ref().unwrap().catalog().unwrap();
    assert_eq!(
        serde_json::to_value(&catalog).unwrap(),
        json!({"schema":CATALOG_SCHEMA,"operations":[
            {"operation":"node_static_workspace@1","workspace_ids":["w_0"]}]})
    );
    let request = serde_json::to_string(&ProposalRequest {
        schema: PROPOSAL_REQUEST_SCHEMA.into(),
        search_id: "s".into(),
        frozen_contract: frozen.base_contract.clone(),
        runtime_constraint: RuntimeConstraint::Any,
        known_derivations: vec![],
        failure_evidence: vec![],
        inspection_evidence: vec![],
        operation_catalog: catalog,
        remaining_budget: ProposalBudget {
            rounds_remaining: 1,
            max_proposals: 4,
            timeout_ms: 5000,
            attempts_remaining: 4,
        },
    })
    .unwrap();
    for private in ["apps/web", "dist", "yarn", "1.22.22", "20.20.2", "frozen-lockfile"] {
        assert!(!request.contains(private), "{private} leaked");
    }
}

#[test]
fn w1_fixed_selection_compiles_the_canonical_route_with_separate_scopes() {
    let frozen = frozen(qualified_fixture());
    let out = outcomes(&frozen, vec![select("w_0")]);
    let [ProposalOutcome::Admitted(candidate)] = out.as_slice() else {
        panic!("{out:?}")
    };
    let draft = parse_capsule_toml(&candidate.compiled().capsule_toml).unwrap();
    let steps: Vec<_> = draft
        .derivation
        .steps
        .iter()
        .map(|s| (s.id.as_str(), s.argv.clone(), s.cwd.as_str(), s.root.clone()))
        .collect();
    assert_eq!(
        steps,
        vec![
            ("install", vec!["yarn".into(), "install".into(), "--frozen-lockfile".into()], ".", None),
            ("build", vec!["yarn".into(), "run".into(), "build".into()], "apps/web", None),
            ("site", vec![], ".", Some("apps/web/dist".into())),
        ]
    );
    assert!(!candidate.compiled().capsule_toml.contains("--cwd"));
    // K is the frozen one; the route is only a candidate (W11: no verdict).
    assert_eq!(
        candidate.compiled().base_contract_ref,
        frozen.base_contract_ref
    );
    assert_eq!(candidate.candidate().effects, "pure");
    assert_eq!(
        candidate.candidate().provisions,
        vec!["toolchain.node.20.20.2", "toolchain.yarn.1.22.22"]
    );
    validate_candidate_scope(&frozen, &[candidate.candidate().clone()]).unwrap();
}

#[test]
fn w2_two_static_workspaces_two_ids_and_no_auto_selection() {
    let dir = tempfile::tempdir().unwrap();
    monorepo(dir.path(), json!(["apps/*", "packages/*"]));
    static_ws(dir.path(), "apps/docs", "docs");
    let auth = qualified(dir.path());
    assert_eq!(
        auth.workspaces.keys().collect::<Vec<_>>(),
        vec!["w_0", "w_1"]
    );
    assert_eq!(auth.workspaces["w_0"].cwd, "apps/docs");
    // Known-D (CandidateProducer OFF) still refuses; no workspace is chosen.
    let evidence = ato_formation::detect::detect(dir.path()).unwrap();
    assert_eq!(
        candidate_authoring(&evidence).unwrap_err().code,
        "preset_node_static_v2_workspace"
    );
    let frozen = frozen(auth);
    // Both may be proposed; each is a separate candidate D.
    let out = outcomes(&frozen, vec![select("w_0"), select("w_1")]);
    assert!(
        out.iter()
            .all(|o| matches!(o, ProposalOutcome::Admitted(_)))
    );
}

#[test]
fn w3_unknown_workspace_id_is_rejected() {
    let frozen = frozen(qualified_fixture());
    for id in ["w_9", "apps/web", "web"] {
        assert_eq!(
            rejected(&outcomes(&frozen, vec![select(id)])[0]),
            "proposal_id_unauthorized"
        );
    }
}

#[test]
fn w4_provider_paths_argv_cwd_or_output_are_strict_schema_rejections() {
    let frozen = frozen(qualified_fixture());
    for extra in [
        json!({"path":"apps/web"}),
        json!({"cwd":"apps/web"}),
        json!({"argv":["yarn","build"]}),
        json!({"output_root":"apps/web/dist"}),
        json!({"network":"dependency-resolution"}),
        json!({"runtime_id":"local"}),
        json!({"manager_version":"4.0.0"}),
    ] {
        let mut op = json!({"operation":"node_static_workspace@1","workspace_id":"w_0"});
        op.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let proposal = json!({"kind":"propose_derivation","operations":[op]});
        assert_eq!(
            rejected(&outcomes(&frozen, vec![proposal])[0]),
            "proposal_schema"
        );
    }
    // Modify, multi-operation and cross-template forms are not this operation.
    let mut known = frozen.clone();
    known.policy.proposal.as_mut().unwrap().python_http_process = None;
    assert_eq!(
        rejected(
            &outcomes(
                &known,
                vec![json!({"kind":"propose_derivation","operations":[
                    {"operation":"node_static_workspace@1","workspace_id":"w_0"},
                    {"operation":"node_static_workspace@1","workspace_id":"w_0"}]})]
            )[0]
        ),
        "proposal_propose_operation"
    );
    assert_eq!(
        rejected(
            &outcomes(
                &known,
                vec![json!({"kind":"propose_derivation","operations":[
                    {"operation":"python_http_process@1","entrypoint_id":"w_0"}]})]
            )[0]
        ),
        "proposal_operation_unauthorized"
    );
}

#[test]
fn w5_path_escape_is_refused_by_the_inventory() {
    for pattern in ["../outside", "/abs/*", "apps/../../x"] {
        let dir = tempfile::tempdir().unwrap();
        monorepo(dir.path(), json!([pattern]));
        assert_eq!(
            inventory(dir.path()).unwrap_err().code(),
            "workspace_path_escape",
            "{pattern}"
        );
    }
    for pattern in ["apps/**", "apps/w*", "!apps/web", "{apps,packages}/*", "."] {
        let dir = tempfile::tempdir().unwrap();
        monorepo(dir.path(), json!([pattern]));
        assert_eq!(
            inventory(dir.path()).unwrap_err().code(),
            "workspace_pattern_unsupported",
            "{pattern}"
        );
    }
    #[cfg(unix)]
    {
        let outside = tempfile::tempdir().unwrap();
        static_ws(outside.path(), "web", "web");
        let dir = tempfile::tempdir().unwrap();
        monorepo(dir.path(), json!(["links/*"]));
        fs::create_dir_all(dir.path().join("links")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("web"), dir.path().join("links/web"))
            .unwrap();
        assert_eq!(
            inventory(dir.path()).unwrap_err().code(),
            "workspace_path_escape"
        );
    }
}

#[test]
fn w6_server_only_workspace_is_never_in_the_catalog() {
    let dir = tempfile::tempdir().unwrap();
    monorepo(dir.path(), json!(["packages/*"]));
    let inv = inventory(dir.path()).unwrap();
    assert_eq!(
        inv.workspaces[0].qualification,
        Qualification::Excluded {
            code: "workspace_server_dependency".into()
        }
    );
    assert!(inv.authorization().is_none());
}

#[test]
fn w7_ambiguous_output_or_unproven_static_build_is_excluded() {
    let dir = tempfile::tempdir().unwrap();
    monorepo(dir.path(), json!(["apps/*"]));
    write(
        dir.path(),
        "apps/web/vite.config.ts",
        "export default { build: { outDir: dir } }",
    );
    static_ws(dir.path(), "apps/compound", "compound");
    write(
        dir.path(),
        "apps/compound/package.json",
        &json!({"scripts":{"build":"yarn build:app && yarn build:version","preview":"vite preview"}})
            .to_string(),
    );
    static_ws(dir.path(), "apps/noentry", "noentry");
    fs::remove_file(dir.path().join("apps/noentry/index.html")).unwrap();
    static_ws(dir.path(), "apps/escape", "escape");
    write(
        dir.path(),
        "apps/escape/vite.config.ts",
        "export default { build: { outDir: '../../public' } }",
    );
    let inv = inventory(dir.path()).unwrap();
    let codes: BTreeMap<_, _> = inv
        .workspaces
        .iter()
        .map(|w| (w.relative_root.as_str(), w.qualification.clone()))
        .collect();
    let excluded = |code: &str| Qualification::Excluded { code: code.into() };
    assert_eq!(codes["apps/web"], excluded("workspace_output_ambiguous"));
    assert_eq!(codes["apps/escape"], excluded("workspace_output_ambiguous"));
    assert_eq!(codes["apps/compound"], excluded("workspace_static_unproven"));
    assert_eq!(codes["apps/noentry"], excluded("workspace_entry_missing"));
    assert!(inv.authorization().is_none());
}

#[test]
fn w8_manager_or_version_conflicts_are_excluded() {
    // Workspace-level conflicting manager declaration.
    let dir = tempfile::tempdir().unwrap();
    monorepo(dir.path(), json!(["apps/*"]));
    write(
        dir.path(),
        "apps/web/package.json",
        &json!({"packageManager":"pnpm@9.15.4","scripts":{"build":"vite build","preview":"vite preview"}})
            .to_string(),
    );
    static_ws(dir.path(), "apps/node", "node");
    write(dir.path(), "apps/node/.nvmrc", "22.14.0\n");
    static_ws(dir.path(), "apps/lock", "lock");
    write(dir.path(), "apps/lock/package-lock.json", "{}");
    static_ws(dir.path(), "apps/rc", "rc");
    write(dir.path(), "apps/rc/.npmrc", "registry=https://example.invalid\n");
    let inv = inventory(dir.path()).unwrap();
    let codes: BTreeMap<_, _> = inv
        .workspaces
        .iter()
        .map(|w| (w.relative_root.clone(), w.qualification.clone()))
        .collect();
    let excluded = |code: &str| Qualification::Excluded { code: code.into() };
    assert_eq!(codes["apps/web"], excluded("workspace_manager_conflict"));
    assert_eq!(codes["apps/node"], excluded("workspace_node_version_conflict"));
    assert_eq!(codes["apps/lock"], excluded("workspace_nested_lock"));
    assert_eq!(codes["apps/rc"], excluded("workspace_manager_config"));
    assert!(inv.authorization().is_none());

    // Root: lock conflict or manager configuration refuses the install scope,
    // so no workspace becomes a candidate at all.
    for (file, code) in [
        ("package-lock.json", "preset_node_static_v2_lock_conflict"),
        (".yarnrc.yml", "preset_node_static_v2_manager_config"),
        (".npmrc", "preset_node_static_v2_manager_config"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        monorepo(dir.path(), json!(["apps/*"]));
        write(dir.path(), file, "{}");
        let inv = inventory(dir.path()).unwrap();
        assert_eq!(
            inv.installation,
            Installation::Refused { code: code.into() },
            "{file}"
        );
        assert!(
            matches!(
                inv.workspaces[0].qualification,
                Qualification::StaticQualified { .. }
            ),
            "discovery still records the static workspace fact"
        );
        assert!(inv.authorization().is_none());
    }
}

#[test]
fn w8_pnpm_workspace_configuration_is_a_separate_slice() {
    let dir = tempfile::tempdir().unwrap();
    monorepo(dir.path(), json!(["apps/*"]));
    write(dir.path(), "pnpm-workspace.yaml", "packages:\n  - apps/*\n");
    assert_eq!(
        inventory(dir.path()).unwrap_err().code(),
        "workspace_pnpm_deferred"
    );
}

#[test]
fn w10_k_mutation_is_rejected_and_k_is_never_a_proposal_field() {
    let auth = qualified_fixture();
    // A producer cannot carry K.
    let frozen_search = frozen(auth.clone());
    let with_k = json!({"kind":"propose_derivation","contract":{"require":[]},
        "operations":[{"operation":"node_static_workspace@1","workspace_id":"w_0"}]});
    assert_eq!(
        rejected(&outcomes(&frozen_search, vec![with_k])[0]),
        "proposal_schema"
    );
    // A frozen K outside the static-surface shape is refused before any call.
    let mut other = frozen_search.clone();
    other.base_contract.requirements[0].port = Some("other.http".into());
    other.base_contract_ref = other.base_contract.contract_ref().unwrap();
    other.contract_ref = other.base_contract_ref.clone();
    assert_eq!(
        auth.validate_contract(&other.base_contract, other.initial_source.as_ref().unwrap())
            .unwrap_err()
            .0,
        "proposal_contract_unsupported"
    );
    // Such a search cannot even be frozen for a producer.
    assert!(CandidateRegistry::new(&other).is_err());
    // The same frozen K is compiled into every admitted D, never rewritten.
    let out = outcomes(&frozen_search, vec![select("w_0")]);
    let ProposalOutcome::Admitted(candidate) = &out[0] else {
        panic!()
    };
    let (k2, _) = bind(
        &parse_capsule_toml(&candidate.compiled().capsule_toml).unwrap(),
        &BindingContext {
            source_closure_ref: CLOSURE,
        },
    )
    .unwrap();
    assert_eq!(k2, frozen_search.base_contract);
}

#[test]
fn w9_policy_scope_is_not_widened_by_the_operation() {
    // Proposal searches stay network-denied; the D *requires* dependency
    // resolution for install, which admission compares with policy.
    let mut search = frozen(qualified_fixture());
    search.policy.network = "dependency-resolution".into();
    assert_eq!(
        search
            .policy
            .proposal
            .as_ref()
            .unwrap()
            .validate_search(&search)
            .unwrap_err()
            .0,
        "proposal_search_scope"
    );
    assert!(CandidateRegistry::new(&search).is_err());
    let denied = frozen(qualified_fixture());
    let out = outcomes(&denied, vec![select("w_0")]);
    let ProposalOutcome::Admitted(candidate) = &out[0] else {
        panic!()
    };
    let text = &candidate.compiled().capsule_toml;
    assert_eq!(text.matches("dependency-resolution").count(), 1);
    assert!(!text.contains("PATH") && !text.contains("/opt/ato"));
}

#[test]
fn authorization_bounds_and_grammar_are_validated() {
    let good = qualified_fixture();
    good.validate().unwrap();
    let mut bad = good.clone();
    bad.workspaces.get_mut("w_0").unwrap().cwd = "../web".into();
    assert!(bad.validate().is_err());
    let mut bad = good.clone();
    bad.workspaces.get_mut("w_0").unwrap().output_root = "elsewhere/dist".into();
    assert!(bad.validate().is_err());
    let mut bad = good.clone();
    bad.install.install_mode = "--immutable".into();
    assert!(bad.validate().is_err());
    let mut bad = good.clone();
    bad.install.package_manager_version = "^1.22.0".into();
    assert!(bad.validate().is_err());
    let mut bad = good.clone();
    let build = bad.workspaces["w_0"].clone();
    for n in 1..=MAX_WORKSPACE_CANDIDATES {
        bad.workspaces.insert(format!("w_x{n}"), WorkspaceStaticBuild {
            cwd: format!("{}{n}", build.cwd),
            output_root: format!("{}{n}/dist", build.cwd),
        });
    }
    assert_eq!(bad.validate().unwrap_err().0, "proposal_domain_bounds");
}

#[test]
fn candidate_bound_is_enforced_by_the_inventory() {
    let dir = tempfile::tempdir().unwrap();
    monorepo(dir.path(), json!(["apps/*"]));
    for n in 0..=MAX_WORKSPACE_CANDIDATES {
        static_ws(dir.path(), &format!("apps/w{n:03}"), "w");
    }
    assert_eq!(
        inventory(dir.path()).unwrap_err().code(),
        "workspace_inventory_bounds"
    );
}

#[test]
fn legacy_authorization_bytes_are_unchanged() {
    let json = json!({"modifiable_derivation_refs":[],
        "source_domain":{"entrypoints":{"entry_a":"server.py"},"modules":{}},
        "python_http_process":null,
        "policy":{"max_proposal_rounds":1,"max_proposals":4,"timeout_ms":5000,
            "allow_source_text":false,"max_source_bytes":0}});
    let auth: ProposalAuthorization = serde_json::from_value(json.clone()).unwrap();
    assert!(auth.node_static_workspace.is_none());
    assert_eq!(serde_json::to_value(&auth).unwrap(), json);
}
