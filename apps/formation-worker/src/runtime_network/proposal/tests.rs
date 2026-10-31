use super::*;
use ato_formation::proposal::ProposalRoundRecord;
use ato_formation::proposal::{CandidateProducerPolicy, PythonHttpProcess, SourceDomain};
use ato_formation::search::BudgetCounters;

#[test]
fn successful_native_audit_does_not_hide_later_http_verification_failure() {
    let attempt = json!({
        "failure":{"code":"http_status_mismatch","stage":"verification",
            "message":"root: GET / on app.http returned 302; expected 200"},
        "exploration_evidence":[{"kind":"exploration_execution_facts","facts":[{
            "exit_code":0,"log_tail":"native audit completed","stage":"build","step":"native-4"
        }]}]
    });
    let log = failure_log_tail(&attempt);
    assert!(log.contains("native audit completed"));
    assert!(log.contains("returned 302; expected 200"));
    let unsafe_message = json!({"failure":{"message":"secret=private-value"}});
    assert!(!failure_log_tail(&unsafe_message).contains("private-value"));
}

fn authorization() -> ProposalAuthorization {
    ProposalAuthorization {
        execution_plan: None,
        modifiable_derivation_refs: vec![],
        source_domain: SourceDomain {
            entrypoints: BTreeMap::from([("entry".into(), "app.py".into())]),
            modules: BTreeMap::new(),
        },
        python_http_process: Some(PythonHttpProcess {
            python_version: "3.12.7".into(),
            http_port: "app.http".into(),
            guest_port: 8000,
        }),
        node_static_workspace: None,
        policy: CandidateProducerPolicy {
            max_proposal_rounds: 1,
            max_proposals: 4,
            timeout_ms: 30_000,
            allow_source_text: false,
            max_source_bytes: 0,
        },
    }
}
fn prepared(edit: impl FnOnce(&Path)) -> (tempfile::TempDir, Submission) {
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join(".tmp");
    std::fs::create_dir_all(&scratch).unwrap();
    let root = tempfile::tempdir_in(scratch).unwrap();
    let source = root.path().join("input");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("app.py"), "SOURCE_SECRET_CANARY").unwrap();
    edit(&source);
    let k = serde_json::from_value(json!({"schema":"ato.contract/1","requirements":[{
        "id":"health","verifier":"ato.contract.http@1","port":"app.http","method":"GET","path":"/health","status":200}]})).unwrap();
    let submission = prepare_submission_inner(
        &source,
        &[],
        None,
        &root.path().join("work"),
        RuntimeConstraintWire::Any,
        SatisfyPolicy {
            network: "denied".into(),
            allow_managed: false,
            decision: None,
            generation: None,
            proposal: None,
            exploration: None,
        },
        SatisfyBudget::ceilings(4, "first_pass"),
        "proposal_test",
        Some(k),
    )
    .unwrap();
    (root, submission)
}
fn status(sub: &Submission) -> Value {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    json!({"search_state":SearchStateV1 {
        schema:ato_formation::search::SEARCH_SCHEMA.into(),search_id:sub.request.search_id.clone(),owner_scope:"owner".into(),
        revision:1,frozen:frozen_request(&sub.request).unwrap(),deadline_ms:now+60_000,budget:BudgetCounters::default(),
        source_archive_bytes:sub.request.source.archive_bytes,attempts:vec![],owner_stopped:false,decisions:vec![],evidence:vec![],generation:None,
        proposal_history:vec![],exploration_submission:None,proposal_round:Some(ProposalRoundRecord {opened_at_ms:now,expires_at_ms:now+30_000,outcome:None,candidates:vec![],derivations:vec![],diagnostics:vec![],inspection_requests:vec![]}),
    },"proposal_point":{"round_seq":1,"revision":1,"claimed":false},"proposal_round":{"round_seq":1,"status":"open"}})
}
fn output() -> ProducerOutput {
    ProducerOutput::new(br#"{"schema":"ato.formation-proposal/1","proposals":[{"kind":"propose_derivation","operations":[{"operation":"python_http_process@1","entrypoint_id":"entry"}]}]}"#.to_vec(),ProducerProvenance {provider:"fixed".into(),model:None}).unwrap()
}
fn completed(sub: &Submission) -> Value {
    let mut status = status(sub);
    let frozen = frozen_request(&sub.request).unwrap();
    let mut registry = CandidateRegistry::new(&frozen).unwrap();
    let raw = output();
    let outcomes = registry.validate_batch(&BTreeMap::new(), &raw).unwrap();
    status["proposal_round"] = json!({"round_seq":1,"status":"completed","raw_output_base64":BASE64.encode(raw.raw()),
        "raw_output_digest":format!("sha256:{:x}",Sha256::digest(raw.raw())),"provenance":{"provider":"fixed"},
        "outcomes":outcomes.iter().map(outcome_wire).collect::<Vec<_>>(),"error_class":null});
    status["search_state"]["proposal_round"]["outcome"] = json!("completed");
    status["search_state"]["proposal_round"]["candidates"] = json!(
        registry
            .generated()
            .iter()
            .map(|c| c.candidate())
            .collect::<Vec<_>>()
    );
    status["proposal_point"] = Value::Null;
    status
}
#[test]
fn zero_d_has_no_preset_and_source_is_independent() {
    let (_root, mut sub) = prepared(|_| {});
    assert!(sub.request.authorized_derivations.is_empty());
    assert!(sub.contracts.is_empty());
    sub.enable_candidate_producer(authorization()).unwrap();
    assert_eq!(
        sub.request.initial_source.as_ref().unwrap().closure_ref,
        sub.request.source.closure_ref
    );
    assert!(sub.enable_candidate_producer(authorization()).is_err());
}
#[test]
fn source_inventory_ignores_later_original_writes() {
    let (root, mut sub) = prepared(|_| {});
    std::fs::write(root.path().join("input/later.py"), "pass").unwrap();
    let mut auth = authorization();
    auth.source_domain
        .entrypoints
        .insert("later".into(), "later.py".into());
    assert!(sub.enable_candidate_producer(auth).is_err());
    assert!(sub.request.policy.proposal.is_none());
    sub.enable_candidate_producer(authorization()).unwrap();
}
#[test]
fn module_requires_frozen_files_and_package_parents() {
    for module in ["os", "site", "absent.module"] {
        let (_root, mut sub) = prepared(|_| {});
        let mut auth = authorization();
        auth.source_domain
            .modules
            .insert("module".into(), module.into());
        assert!(sub.enable_candidate_producer(auth).is_err());
    }
    let (_root, mut sub) = prepared(|p| {
        std::fs::create_dir(p.join("pkg")).unwrap();
        std::fs::write(p.join("pkg/__init__.py"), "").unwrap();
        std::fs::write(p.join("pkg/server.py"), "pass").unwrap();
        std::fs::write(p.join("pkg/__main__.py"), "pass").unwrap();
    });
    let mut auth = authorization();
    auth.source_domain.modules = BTreeMap::from([
        ("file".into(), "pkg.server".into()),
        ("package".into(), "pkg".into()),
    ]);
    sub.enable_candidate_producer(auth).unwrap();
}
#[test]
fn source_refs_cannot_be_replaced_by_receiver_syntax() {
    let (_root, mut sub) = prepared(|_| {});
    sub.request.source.closure_ref = format!("sha256:{}", "a".repeat(64));
    assert!(sub.enable_candidate_producer(authorization()).is_err());
}
#[cfg(unix)]
#[test]
fn symlink_file_and_directory_never_authorize_entrypoint() {
    for path in ["link.py", "linked/app.py"] {
        let (_root, mut sub) = prepared(|p| {
            std::fs::create_dir(p.join("dir")).unwrap();
            std::fs::write(p.join("dir/app.py"), "pass").unwrap();
            std::os::unix::fs::symlink("app.py", p.join("link.py")).unwrap();
            std::os::unix::fs::symlink("dir", p.join("linked")).unwrap();
        });
        let mut auth = authorization();
        auth.source_domain
            .entrypoints
            .insert("link".into(), path.into());
        assert!(sub.enable_candidate_producer(auth).is_err());
    }
}
#[test]
fn request_projection_contains_no_private_paths_source_or_receipts() {
    let (_root, mut sub) = prepared(|_| {});
    sub.enable_candidate_producer(authorization()).unwrap();
    let mut status = status(&sub);
    status["logs"] = json!("SECRET_LOG");
    status["receipt"] = json!("SECRET_RECEIPT");
    let request = sub.proposal_request(&status).unwrap();
    let bytes = serde_json::to_string(&request).unwrap();
    for forbidden in [
        "app.py",
        "SOURCE_SECRET_CANARY",
        "SECRET_LOG",
        "SECRET_RECEIPT",
        "source_domain",
        "source_context",
        "bindings",
        "archive_digest",
    ] {
        assert!(!bytes.contains(forbidden), "{forbidden}");
    }
    assert_eq!(request.schema, PROPOSAL_REQUEST_SCHEMA);
    assert_eq!(request.remaining_budget.attempts_remaining, 4);
}
#[test]
fn completed_batch_recompiles_idempotently_but_is_not_a_pass() {
    let (_root, mut sub) = prepared(|_| {});
    sub.enable_candidate_producer(authorization()).unwrap();
    let frozen = serde_json::to_value(&sub.request).unwrap();
    let status = completed(&sub);
    sub.accept_proposal_round(&status).unwrap();
    sub.accept_proposal_round(&status).unwrap();
    assert_eq!(sub.proposal_recipes().count(), 1);
    assert_eq!(sub.contracts.len(), 1);
    assert_eq!(serde_json::to_value(&sub.request).unwrap(), frozen);
    assert!(accept_verified_routes(&sub, "id", &status).0.is_empty());
    assert!(
        sub.contracts
            .values()
            .all(|k| k == &sub.request.base_contract)
    );
}
#[test]
fn corruption_is_rejected_before_any_receipt_or_recipe_admission() {
    for field in [
        "proposal_id",
        "derivation_ref",
        "capsule_toml",
        "candidate",
        "derivation",
        "status",
    ] {
        let (_root, mut sub) = prepared(|_| {});
        sub.enable_candidate_producer(authorization()).unwrap();
        let mut status = completed(&sub);
        status["proposal_round"]["outcomes"][0][field] = json!("forged");
        assert!(sub.accept_proposal_round(&status).is_err(), "{field}");
        assert!(sub.contracts.is_empty());
        assert_eq!(sub.proposal_recipes().count(), 0);
    }
}
#[test]
fn raw_provenance_scope_and_member_count_corruption_fail_closed() {
    for field in [
        "raw_output_base64",
        "raw_output_digest",
        "provenance",
        "outcomes",
    ] {
        let (_root, mut sub) = prepared(|_| {});
        sub.enable_candidate_producer(authorization()).unwrap();
        let mut status = completed(&sub);
        status["proposal_round"][field] = json!("forged");
        assert!(sub.accept_proposal_round(&status).is_err(), "{field}");
        assert!(sub.contracts.is_empty());
    }
    let (_root, mut sub) = prepared(|_| {});
    sub.enable_candidate_producer(authorization()).unwrap();
    let mut status = completed(&sub);
    status["search_state"]["frozen"]["base_contract_ref"] = json!("forged");
    assert!(sub.accept_proposal_round(&status).is_err());
}

#[test]
fn policy_cannot_bypass_enablement_or_change_after_source_verification() {
    let (_root, mut sub) = prepared(|_| {});
    sub.request.policy.proposal = Some(authorization());
    assert!(sub.validate_proposal_submission().is_err());
    sub.request.policy.proposal = None;
    sub.enable_candidate_producer(authorization()).unwrap();
    sub.validate_proposal_submission().unwrap();
    sub.request.source.archive_digest = format!("sha256:{}", "a".repeat(64));
    assert!(sub.validate_proposal_submission().is_err());
}

mod general;

fn monorepo(p: &Path) {
    std::fs::write(
        p.join("package.json"),
        r#"{"private":true,"packageManager":"yarn@1.22.22","workspaces":["apps/*"]}"#,
    )
    .unwrap();
    std::fs::write(p.join("yarn.lock"), "# yarn lockfile v1\n").unwrap();
    std::fs::create_dir_all(p.join("apps/web")).unwrap();
    std::fs::write(
        p.join("apps/web/package.json"),
        r#"{"name":"web","scripts":{"build":"vite build","preview":"vite preview"}}"#,
    )
    .unwrap();
    std::fs::write(p.join("apps/web/index.html"), "<!doctype html>").unwrap();
}

/// Workspace IDs are authorized only as the inventory of the digest-verified
/// frozen extraction; a mutated private mapping is refused before submission.
#[test]
fn workspace_domain_must_equal_the_frozen_source_inventory() {
    let (root, mut sub) = prepared(monorepo);
    let domain = ato_formation::workspace::inventory(&root.path().join("input"))
        .unwrap()
        .authorization()
        .unwrap();
    let mut auth = authorization();
    auth.python_http_process = None;
    auth.source_domain.entrypoints.clear();
    auth.node_static_workspace = Some(domain.clone());
    let mut tampered = auth.clone();
    tampered
        .node_static_workspace
        .as_mut()
        .unwrap()
        .workspaces
        .get_mut("w_0")
        .unwrap()
        .output_root = "apps/web/build".into();
    assert!(
        sub.enable_candidate_producer(tampered)
            .unwrap_err()
            .to_string()
            .contains("workspace domain differs")
    );
    sub.enable_candidate_producer(auth).unwrap();
    let catalog = serde_json::to_string(
        &sub.request
            .policy
            .proposal
            .as_ref()
            .unwrap()
            .catalog()
            .unwrap(),
    )
    .unwrap();
    assert!(catalog.contains("node_static_workspace@1") && !catalog.contains("apps/web"));
}

#[test]
fn prompt_v1_bytes_stay_pinned_and_v2_names_the_workspace_operation() {
    use super::deepseek::*;
    assert_eq!(
        prompt_sha256(),
        "sha256:a3217b28b4242fdc03e11fe5dee7d91fbbc47b79358c4ae31601002b7a81ea50"
    );
    assert!(!PROMPT.contains("node_static_workspace@1"));
    assert!(PROMPT_V2.contains("node_static_workspace@1"));
    assert!(PROMPT_V2.contains("never a path or package name"));
    assert_eq!(prompt_for(PROMPT_VERSION_V2), Some(PROMPT_V2));
    assert_eq!(
        prompt_for("ato.formation-candidate-producer-prompt/3"),
        Some(PROMPT_V3)
    );
}

#[test]
fn auto_plan_inventory_comes_from_verified_source_and_excludes_credentials() {
    let (_root, mut sub) = prepared(|source| {
        std::fs::write(source.join("package.json"), "{\"main\":\"app.py\"}").unwrap();
        std::fs::write(source.join(".dev.vars"), "PROVIDER_KEY=must_not_project").unwrap();
    });
    let source = sub.frozen_source.root.clone();
    let files = source_file_inventory(&source).unwrap();
    assert!(files.contains("app.py"));
    assert!(!ato_formation::proposal::source_file_allowed(".dev.vars"));
    // The compiler refuses a private credential path even if a requester
    // tries to supply it in a hand-written authorization.
    let mut auth = authorization();
    auth.execution_plan = Some(ato_formation::proposal::PlanAuthorization {
        runtime_port_operations: false,
        source_oci: None,
        toolchains: BTreeMap::from([("python".into(), "3.12.7".into())]),
        files: BTreeMap::from([(
            "private".into(),
            ato_formation::proposal::VerifiedSourceFile {
                path: ".dev.vars".into(),
                digest: format!("sha256:{}", "a".repeat(64)),
            },
        )]),
    });
    assert!(sub.enable_candidate_producer(auth).is_err());
}

#[test]
fn auto_source_priority_subset_is_valid_canonical_provider_context() {
    let (root, sub) = prepared(|source| {
        std::fs::write(source.join("package.json"), "{\"main\":\"app.py\"}").unwrap();
        std::fs::write(source.join("README.md"), "untrusted source documentation").unwrap();
        std::fs::write(source.join("package-lock.json"), "{\"lockfileVersion\":3}").unwrap();
        std::fs::write(source.join(".dev.vars"), "PROVIDER_KEY=excluded").unwrap();
    });
    let mut policy = sub.request.policy.clone();
    policy.exploration = Some(
        serde_json::from_value(json!({"ceiling":{},
        "max_provider_calls":3,"max_inspections":4,"max_provider_cost_usd_micros":100000,
        "max_provider_input_tokens":90000,"max_provider_output_tokens":6144}))
        .unwrap(),
    );
    let fresh = prepare_exploration_submission_auto(
        &sub.frozen_source.root,
        &[],
        sub.request.base_contract.clone(),
        &root.path().join("auto"),
        RuntimeConstraintWire::Any,
        policy,
        SatisfyBudget::ceilings(4, "first_pass"),
        "auto_context_test",
        None,
        BTreeMap::from([("python".into(), "3.12.7".into())]),
    )
    .unwrap();
    let request = fresh.proposal_request_v2(&status(&fresh)).unwrap();
    let repository = request
        .repository_evidence
        .as_ref()
        .expect("automatic repository evidence");
    assert!(repository.validate());
    assert!(!repository.runtime_verified);
    assert!(
        repository
            .facts
            .iter()
            .any(|fact| fact.file == "package.json")
    );
    let encoded_evidence = serde_json::to_string(repository).unwrap();
    assert!(!encoded_evidence.contains("PROVIDER_KEY") && !encoded_evidence.contains("excluded"));
    assert_eq!(request.frozen_contract, fresh.request.base_contract);

    let mut view = status(&fresh);
    let mut old = view["search_state"]["proposal_round"].clone();
    old["outcome"] = json!("completed");
    let inspected = request
        .source_context
        .iter()
        .find(|e| e.text.contains("untrusted source documentation"))
        .unwrap();
    let auth = fresh
        .request
        .policy
        .proposal
        .as_ref()
        .unwrap()
        .execution_plan
        .as_ref()
        .unwrap();
    old["inspection_requests"] =
        json!([{"file_id":inspected.logical_id,"digest":auth.files[&inspected.logical_id].digest}]);
    old["diagnostics"] = json!(["source_inspection_requested"]);
    view["search_state"]["proposal_history"] = json!([old]);
    let after = fresh.proposal_request_v2(&view).unwrap();
    assert!(
        after.source_context.iter().any(|e| e.text.contains("main")),
        "manifest lost after a one-file inspection"
    );
    assert!(
        after
            .source_context
            .iter()
            .any(|e| e.logical_id == inspected.logical_id)
    );
    assert_eq!(after.source_context.len(), 4);
    assert_eq!(request.source_context.len(), 4);
    assert!(
        request
            .source_context
            .windows(2)
            .all(|w| (w[0].kind, &w[0].logical_id) < (w[1].kind, &w[1].logical_id))
    );
    assert!(
        !serde_json::to_string(&request)
            .unwrap()
            .contains("PROVIDER_KEY=excluded")
    );
}

#[test]
fn session_inspection_is_inside_one_round_and_restart_reuses_the_final_answer() {
    use reasoning::{
        ReasoningInput, ReasoningProducer, ReasoningProviderConfig, SESSION_RESPONSE_SCHEMA,
        SessionConfig,
    };
    let (root, mut sub) = prepared(|path| {
        std::fs::create_dir(path.join("src")).unwrap();
        for (name, text) in [
            (
                "setup.py",
                "# explicitly referenced entrypoint: app.py\n# explicitly referenced config: src/requirements.txt\n",
            ),
            ("README.md", "# application\n"),
            ("settings.py", "PORT=8000\n"),
            ("src/requirements.txt", "# requirements\n"),
        ] {
            std::fs::write(path.join(name), text).unwrap();
        }
    });
    sub.request.policy.exploration=Some(serde_json::from_value(json!({"ceiling":{},"max_provider_calls":6,"max_inspections":4,
        "max_provider_cost_usd_micros":1000000,"max_provider_input_tokens":200000,"max_provider_output_tokens":100000,
        "reasoning":{"round_timeout_ms":60000,"inspection_timeout_ms":50,"inspection_source_bytes":32768}})).unwrap());
    let source = root.path().join("input");
    let files = [
        ("entry", "app.py"),
        ("manifest", "setup.py"),
        ("readme", "README.md"),
        ("config", "settings.py"),
        ("requirements", "src/requirements.txt"),
    ]
    .into_iter()
    .map(|(id, path)| {
        (
            id.into(),
            ato_formation::proposal::VerifiedSourceFile {
                path: path.into(),
                digest: format!(
                    "sha256:{:x}",
                    Sha256::digest(std::fs::read(source.join(path)).unwrap())
                ),
            },
        )
    })
    .collect();
    let domain = ato_formation::proposal::PlanAuthorization {
        runtime_port_operations: false,
        files,
        toolchains: BTreeMap::from([("python".into(), "3.12.7".into())]),
        source_oci: None,
    };
    let mut a = authorization();
    a.execution_plan = Some(domain);
    a.python_http_process = None;
    a.source_domain.entrypoints.clear();
    a.policy.allow_source_text = true;
    a.policy.max_source_bytes = 16384;
    a.policy.max_proposals = 1;
    sub.enable_candidate_producer(a).unwrap();
    let mut status = status(&sub);
    let opened = status["search_state"]["proposal_round"]["opened_at_ms"]
        .as_u64()
        .unwrap();
    status["search_state"]["proposal_round"]["expires_at_ms"] = json!(opened + 60000);
    let request = sub.proposal_request_v2(&status).unwrap();
    let config = ReasoningProviderConfig::Session(SessionConfig {
        provider: "codex_session".into(),
        model: "codex-session".into(),
        prompt_version: deepseek::PROMPT_VERSION_V5.into(),
        agent: None,
    });
    let budget = budget::BudgetPlan {
        max_calls: 6,
        input_token_cap: 24576,
        output_token_cap: 2048,
        input_price: 300000,
        output_price: 1200000,
        ceiling_usd_micros: 100000,
    };
    let dir = root.path().join("reasoning");
    let producer =
        ReasoningProducer::new(config.clone(), budget.clone(), dir.clone(), None).unwrap();
    let thread_dir = dir.clone();
    let private_root = root.path().to_string_lossy().to_string();
    let responder = std::thread::spawn(move || {
        for step in 1..=3 {
            let name = format!("r001_s{step:03}");
            let input_path = thread_dir.join(format!("{name}.input.json"));
            let end = std::time::Instant::now() + Duration::from_secs(10);
            while !input_path.exists() {
                assert!(std::time::Instant::now() < end);
                std::thread::sleep(Duration::from_millis(10));
            }
            let bytes = std::fs::read(&input_path).unwrap();
            let input: ReasoningInput = serde_json::from_slice(&bytes).unwrap();
            assert!(!std::str::from_utf8(&bytes).unwrap().contains(&private_root));
            assert!(
                input
                    .request
                    .source_context
                    .iter()
                    .any(|e| e.logical_id == "manifest")
            );
            assert!(
                input
                    .request
                    .source_context
                    .iter()
                    .any(|e| e.logical_id == "entry")
            );
            let output = if step == 1 {
                json!({"schema":"ato.formation-proposal/1","proposals":[{"kind":"unsupported","reason":"invalid-reason"}]})
            } else if step == 2 {
                assert!(!input.inspection_feedback.is_empty());
                let source = input
                    .inventory
                    .iter()
                    .find(|s| s.source_relative_path == "src/requirements.txt")
                    .unwrap();
                json!({"schema":"ato.formation-proposal/1","proposals":[{"kind":"inspect_source","sources":[source.reference]}]})
            } else {
                assert!(
                    input
                        .request
                        .source_context
                        .iter()
                        .any(|e| e.logical_id == "requirements")
                );
                assert_eq!(input.request.round_seq, Some(1));
                assert_eq!(input.rounds_remaining, 3);
                assert_eq!(input.calls_remaining, 4);
                let entry = input
                    .inventory
                    .iter()
                    .find(|s| s.source_relative_path == "app.py")
                    .unwrap();
                json!({"schema":"ato.formation-proposal/1","proposals":[{"kind":"propose_derivation","operations":[{"operation":"execution_plan@1","plan":{
                    "runtime":{"name":"python","version":"3.12.7"},"entrypoint":entry.reference,"argv":[],"cwd":".","guest_port":8000,
                    "dependencies":[],"build_scripts":[],"requirements":{},"basis":[],"unknowns":[]}}]}]})
            };
            // Inference/wait time belongs to the round, not the separate inspection clock.
            std::thread::sleep(Duration::from_millis(80));
            let response = json!({"schema":SESSION_RESPONSE_SCHEMA,"input_sha256":format!("sha256:{:x}",Sha256::digest(&bytes)),"output":output});
            reasoning::save(
                &thread_dir.join(format!("{name}.response.json")),
                &serde_jcs::to_vec(&response).unwrap(),
            )
            .unwrap();
        }
    });
    let answer = producer
        .run_round(&sub, &status, request.clone(), opened + 60000)
        .unwrap();
    responder.join().unwrap();
    let outcomes = CandidateRegistry::new(&frozen_request(&sub.request).unwrap())
        .unwrap()
        .validate_batch(&BTreeMap::new(), &answer.output)
        .unwrap();
    assert!(matches!(&outcomes[0],ProposalOutcome::InspectionRequested(refs) if refs.len()==1));
    assert!(matches!(&outcomes[1], ProposalOutcome::Admitted(_)));
    assert_eq!(producer.accounting().unwrap()["call_count"], 3);
    assert_eq!(producer.accounting().unwrap()["API_calls"], 0);
    let restarted = ReasoningProducer::new(config, budget, dir, None).unwrap();
    let cached = restarted
        .run_round(&sub, &status, request, opened + 60000)
        .unwrap();
    assert_eq!(answer.output.raw(), cached.output.raw());
    assert_eq!(restarted.accounting().unwrap()["call_count"], 3);
}

#[test]
fn native_helper_bytes_do_not_block_failure_feedback_or_success_context() {
    use ato_formation::authoring::{BindingContext, bind};
    use ato_formation::preset::{AppPreset, synthesize_authoring};
    let (_, mut d) = bind(
        &synthesize_authoring(AppPreset::StaticFiles),
        &BindingContext {
            source_closure_ref: &format!("sha256:{}", "a".repeat(64)),
        },
    )
    .unwrap();
    let helper =
        include_str!("../../../../../lib/formation/src/proposal/node-native-dependencies.cjs");
    d.steps[0].argv = vec![helper.repeat(4)];
    let identity = d.derivation_ref().unwrap();
    let manifest = json!({"file_id":"manifest","digest":format!("sha256:{}","b".repeat(64))});
    let plan = json!({"runtime":{"name":"node","version":"22.23.4"},"entrypoint":manifest,
        "static_output":"build","argv":[],"cwd":".","guest_port":0,"dependencies":[],
        "build_scripts":["build"],"requirements":{},"basis":[],"unknowns":[]});
    let mut context: ato_formation::proposal::ExplorationContext = serde_json::from_value(json!({
        "effective_max_rounds":3,"ceiling":{},"previous_derivations":[d],"previous_plan":plan,
        "failures":[{"derivation_ref":identity,"stage":"build","code":"npm_lifecycle_plan_required",
            "exit_code":1,"log_tail":"required native lifecycle was not completed","artifacts":[]}],
        "successful_derivation_ref":identity,"successful_derivation":d,"proposal_diagnostics":[]
    }))
    .unwrap();
    assert!(serde_jcs::to_vec(&context).unwrap().len() > 16 * 1024);
    bound_exploration_context(&mut context).unwrap();
    assert!(serde_jcs::to_vec(&context).unwrap().len() <= 16 * 1024);
    assert_eq!(context.omitted_derivation_refs, vec![identity.clone()]);
    assert_eq!(context.successful_derivation_ref, Some(identity));
    assert_eq!(serde_json::to_value(context.previous_plan).unwrap(), plan);
    assert_eq!(context.failures[0].code, "npm_lifecycle_plan_required");
}

fn raw_github_source(root: &Path) -> (std::path::PathBuf, Vec<u8>, String, String, String) {
    use std::io::Write;
    let mut tar = tar::Builder::new(Vec::new());
    let mut directory = tar::Header::new_gnu();
    directory.set_entry_type(tar::EntryType::Directory);
    directory.set_size(0);
    directory.set_mode(0o755);
    directory.set_mtime(123);
    directory.set_cksum();
    tar.append_data(&mut directory, "repository-0123456789abcdef/", &b""[..])
        .unwrap();
    for (name, bytes) in [
        (
            "app.py",
            &b"raise RuntimeError('must never execute during intake')\n"[..],
        ),
        ("README.md", &b"Python application: python app.py\n"[..]),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(123);
        header.set_cksum();
        tar.append_data(
            &mut header,
            format!("repository-0123456789abcdef/{name}"),
            bytes,
        )
        .unwrap();
    }
    let tar = tar.into_inner().unwrap();
    let mut gzip = flate2::GzBuilder::new()
        .mtime(123)
        .write(Vec::new(), flate2::Compression::default());
    gzip.write_all(&tar).unwrap();
    let bytes = gzip.finish().unwrap();
    let digest = format!("sha256:{:x}", Sha256::digest(&bytes));
    let path = root.join("github.tar.gz");
    std::fs::write(&path, &bytes).unwrap();
    let verified = FileVerifiedArchive::verify(
        File::open(&path).unwrap(),
        &digest,
        bytes.len() as u64,
        SourceLimits::default(),
    )
    .unwrap();
    (
        path,
        bytes,
        digest,
        verified.tree_digest().to_owned(),
        verified.closure_ref("").unwrap().as_str().to_owned(),
    )
}

#[test]
fn original_github_wrapper_archive_reaches_search_without_retar_or_semantic_relabel() {
    use crate::job::source_intake::{ArchiveFileFetcher, prepare_source_proposal};
    let (root, mut template) = prepared(|_| {});
    let mut explicit_authorization = authorization();
    explicit_authorization.policy.allow_source_text = true;
    explicit_authorization.policy.max_source_bytes = 4096;
    template
        .enable_candidate_producer(explicit_authorization)
        .unwrap();
    let (file, bytes, digest, tree, closure) = raw_github_source(root.path());
    let mut request = template.request.clone();
    request.source = SourceTransport {
        archive_base64: None,
        content_ref: Some(digest.clone()),
        archive_bytes: Some(bytes.len() as u64),
        archive_digest: digest.clone(),
        closure_ref: closure.clone(),
    };
    request.initial_source = Some(InitialSource {
        archive_digest: digest.clone(),
        closure_ref: closure.clone(),
    });
    let k = request.base_contract.clone();
    let frozen = frozen_request(&request).unwrap().canonical_bytes().unwrap();
    let job:ato_ipc::formation::FormationJobV1=serde_json::from_value(json!({
        "protocol":"ato.formation-job.v1","job_id":"fjob_wrapper_search","request_idempotency_key":"wrapper-search-explicit",
        "source":{"kind":"uploaded_archive","archive_ref":"formation-source-archive:fjob_source","expected_archive_digest":digest,"expected_source_tree_digest":tree,"subdirectory":""},
        "requested_outputs":["process_workspace"],"target":{"triple":"x86_64-linux-gnu","workspace_guest_root":"/app"},"policy":{"policy_version":"formation-v1","network":"denied","publish_enabled":false}
    })).unwrap();
    let sub = prepare_source_proposal(
        &ArchiveFileFetcher(file),
        &job,
        request.clone(),
        &root.path().join("raw-search"),
    )
    .unwrap();
    assert_eq!(sub.request.base_contract, k);
    assert_eq!(sub.request.base_contract_ref, k.contract_ref().unwrap());
    assert_eq!(
        frozen_request(&sub.request)
            .unwrap()
            .canonical_bytes()
            .unwrap(),
        frozen
    );
    let mut transported = Vec::new();
    sub.source_file()
        .unwrap()
        .read_to_end(&mut transported)
        .unwrap();
    assert_eq!(transported, bytes);
    let verified = FileVerifiedArchive::verify(
        sub.source_file().unwrap(),
        &digest,
        bytes.len() as u64,
        SourceLimits::default(),
    )
    .unwrap();
    println!(
        "raw_github_vector={}",
        json!({
            "archive_base64":BASE64.encode(&bytes),"archive_digest":digest,"size_bytes":bytes.len(),
            "source_tree_digest":tree,"source_closure_ref":closure,
            "observed_file_count":verified.expanded_file_count(),
            "observed_uncompressed_bytes":verified.expanded_bytes(),
        "resolver_contract_version":verified.resolver_contract(),
            "job":job,"request":request,"canonical_frozen_json":String::from_utf8(frozen.clone()).unwrap()
        })
    );
    assert_eq!(verified.tree_digest(), tree);
    assert_eq!(
        verified.archive_format(),
        ato_formation::source::RAW_TAR_GZIP_FORMAT
    );
    let runtime = local::freeze_verified_file(
        verified,
        &root.path().join("runtime"),
        SourceLimits::default(),
    )
    .unwrap();
    assert_eq!(runtime.closure_ref.as_str(), closure);
    assert!(runtime.root.join("app.py").is_file());
    assert!(!runtime.root.join("repository-0123456789abcdef").exists());
    let proposal = sub.proposal_request_v2(&status(&sub)).unwrap();
    assert!(proposal.repository_evidence.is_some());
    assert_eq!(proposal.frozen_contract, k);
    println!(
        "raw_github_proof archive={digest} tree={tree} closure={closure} bytes={}",
        bytes.len()
    );
    for corruption in [
        "archive",
        "tree",
        "subdir",
        "closure",
        "budget",
        "expanded_budget",
        "transfer_budget",
        "K",
    ] {
        let mut bad_job = job.clone();
        let mut bad = request.clone();
        match corruption {
            "archive" => {
                bad.source.archive_digest = format!("sha256:{}", "0".repeat(64));
            }
            "tree" => {
                if let ato_ipc::formation::FormationSourceV1::UploadedArchive(s) =
                    &mut bad_job.source
                {
                    s.expected_source_tree_digest = Some(format!("sha256:{}", "0".repeat(64)));
                }
            }
            "subdir" => {
                if let ato_ipc::formation::FormationSourceV1::UploadedArchive(s) =
                    &mut bad_job.source
                {
                    s.subdirectory = "child".into();
                }
            }
            "closure" => {
                bad.source.closure_ref = format!("sha256:{}", "0".repeat(64));
            }
            "budget" => {
                bad.budget.max_attempts = 0;
            }
            "expanded_budget" => {
                bad.budget.max_expanded_bytes = 1;
            }
            "transfer_budget" => {
                bad.budget.max_transfer_bytes = 1;
            }
            "K" => {
                bad.base_contract_ref = format!("sha256:{}", "0".repeat(64));
            }
            _ => unreachable!(),
        }
        assert!(
            prepare_source_proposal(
                &ArchiveFileFetcher(root.path().join("github.tar.gz")),
                &bad_job,
                bad,
                &root.path().join(format!("bad-{corruption}"))
            )
            .is_err(),
            "{corruption}"
        );
    }
}

#[test]
fn verified_github_archive_prepares_unstarted_zero_d_search_with_fixed_contract() {
    let (root, template) = prepared(|_| {});
    let (file, bytes, digest, _tree, closure) = raw_github_source(root.path());
    let verify = || {
        FileVerifiedArchive::verify(
            File::open(&file).unwrap(),
            &digest,
            bytes.len() as u64,
            SourceLimits::default(),
        )
        .unwrap()
    };
    let mut policy = template.request.policy.clone();
    policy.exploration=Some(serde_json::from_value(json!({"ceiling":{},"max_provider_calls":1,"max_inspections":1,
        "max_provider_cost_usd_micros":1,"max_provider_input_tokens":1,"max_provider_output_tokens":1})).unwrap());
    let mut too_small = template.request.budget.clone();
    too_small.max_transfer_bytes = 1;
    assert!(
        prepare_verified_exploration_submission(
            verify(),
            template.request.base_contract.clone(),
            &root.path().join("small"),
            RuntimeConstraintWire::Any,
            policy.clone(),
            too_small,
            "proposal_test",
            BTreeMap::new()
        )
        .is_err()
    );
    let sub = prepare_verified_exploration_submission(
        verify(),
        template.request.base_contract.clone(),
        &root.path().join("verified"),
        RuntimeConstraintWire::Any,
        policy,
        template.request.budget.clone(),
        "proposal_test",
        BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(sub.request.base_contract, template.request.base_contract);
    assert_eq!(
        sub.request.base_contract_ref,
        template.request.base_contract_ref
    );
    assert_eq!(sub.request.source.archive_digest, digest);
    assert_eq!(sub.request.source.closure_ref, closure);
    assert!(sub.request.authorized_derivations.is_empty());
    assert!(
        sub.request
            .policy
            .proposal
            .as_ref()
            .unwrap()
            .execution_plan
            .as_ref()
            .unwrap()
            .files
            .values()
            .any(|f| f.path == "app.py")
    );
    let mut copied = Vec::new();
    sub.source_file().unwrap().read_to_end(&mut copied).unwrap();
    assert_eq!(copied, bytes);
    let proposal = sub.proposal_request_v2(&status(&sub)).unwrap();
    assert_eq!(proposal.frozen_contract, template.request.base_contract);
}

#[test]
fn source_exploration_intake_requires_measured_tree_and_network_denial() {
    use crate::job::source_intake::{ArchiveFileFetcher, prepare_source_exploration};
    let (root, template) = prepared(|_| {});
    let (file, bytes, digest, tree, closure) = raw_github_source(root.path());
    let mut job:ato_ipc::formation::FormationJobV1=serde_json::from_value(json!({
        "protocol":"ato.formation-job.v1","job_id":"fjob_wrapper_search","request_idempotency_key":"wrapper-search-explicit",
        "source":{"kind":"uploaded_archive","archive_ref":"formation-source-archive:fjob_source","expected_archive_digest":digest,"expected_source_tree_digest":tree,"subdirectory":""},
        "requested_outputs":["process_workspace"],"target":{"triple":"x86_64-linux-gnu","workspace_guest_root":"/app"},"policy":{"policy_version":"formation-v1","network":"denied","publish_enabled":false}
    })).unwrap();
    let mut policy = template.request.policy.clone();
    policy.exploration=Some(serde_json::from_value(json!({"ceiling":{},"max_provider_calls":1,"max_inspections":1,
        "max_provider_cost_usd_micros":1,"max_provider_input_tokens":1,"max_provider_output_tokens":1})).unwrap());
    let prepare = |job: &ato_ipc::formation::FormationJobV1, p: SatisfyPolicy| {
        prepare_source_exploration(
            &ArchiveFileFetcher(file.clone()),
            job,
            template.request.base_contract.clone(),
            &root.path().join("intake"),
            RuntimeConstraintWire::Any,
            p,
            template.request.budget.clone(),
            "proposal_test",
            BTreeMap::new(),
        )
    };
    let sub = prepare(&job, policy.clone()).unwrap();
    assert_eq!(sub.request.source.archive_digest, digest);
    assert_eq!(sub.request.source.archive_bytes, Some(bytes.len() as u64));
    assert_eq!(sub.request.source.closure_ref, closure);
    assert!(sub.request.authorized_derivations.is_empty());
    let mut network = policy.clone();
    network.network = "allowed".into();
    assert!(prepare(&job, network).is_err());
    if let ato_ipc::formation::FormationSourceV1::UploadedArchive(s) = &mut job.source {
        s.expected_source_tree_digest = Some(format!("sha256:{}", "0".repeat(64)));
    }
    assert!(prepare(&job, policy).is_err());
}

#[test]
fn explicit_zero_derivation_archive_never_enables_source_manifest_or_known_preset_fallback() {
    let (root, mut template) =
        prepared(|p| std::fs::write(p.join("capsule.toml"), "not valid TOML !!!").unwrap());
    template.enable_candidate_producer(authorization()).unwrap();
    let mut request = template.request.clone();
    request
        .policy
        .proposal
        .as_mut()
        .unwrap()
        .policy
        .max_proposals = 1;
    request.policy.exploration=Some(serde_json::from_value(json!({
        "ceiling":{},"max_provider_calls":1,"max_inspections":1,"max_provider_cost_usd_micros":1,
        "max_provider_input_tokens":1,"max_provider_output_tokens":1
    })).unwrap());
    let expected = frozen_request(&request).unwrap().canonical_bytes().unwrap();
    let verified = FileVerifiedArchive::verify(
        template.source_file().unwrap(),
        &request.source.archive_digest,
        request.source.archive_bytes.unwrap(),
        SourceLimits::default(),
    )
    .unwrap();
    let sub = prepare_pinned_proposal_submission(
        verified,
        request,
        &root.path().join("explicit-no-fallback"),
    )
    .unwrap();
    assert!(sub.request.authorized_derivations.is_empty());
    assert_eq!(
        frozen_request(&sub.request)
            .unwrap()
            .canonical_bytes()
            .unwrap(),
        expected
    );
}

fn source_handoff_fixture() -> (
    tempfile::TempDir,
    Submission,
    super::super::source_handoff::FormationSourcePointer,
    super::super::source_handoff::FormationSourceOrigin,
) {
    use super::super::source_handoff::{FormationSourceOrigin, FormationSourcePointer};
    use crate::source_archive::{ArchiveDescriptor, Binding, SourceArchiveOutput};
    let (root, mut template) = prepared(|_| {});
    template.enable_candidate_producer(authorization()).unwrap();
    let (file, bytes, digest, _, closure) = raw_github_source(root.path());
    let verified = FileVerifiedArchive::verify(
        File::open(file).unwrap(),
        &digest,
        bytes.len() as u64,
        SourceLimits::default(),
    )
    .unwrap();
    let descriptor = ArchiveDescriptor {
        archive_format_version: verified.archive_format().into(),
        source_archive_digest: digest.clone(),
        size_bytes: bytes.len() as u64,
        source_tree_digest: verified.tree_digest().into(),
        resolver_contract_version: verified.resolver_contract().into(),
        subdirectory: "".into(),
        source_closure_ref: closure.clone(),
        observed_file_count: verified.expanded_file_count() as u64,
        observed_uncompressed_bytes: verified.expanded_bytes(),
    };
    let mut request = template.request;
    request.source = SourceTransport {
        archive_base64: None,
        content_ref: Some(digest.clone()),
        archive_bytes: Some(bytes.len() as u64),
        archive_digest: digest.clone(),
        closure_ref: closure.clone(),
    };
    request.initial_source = Some(InitialSource {
        archive_digest: digest,
        closure_ref: closure,
    });
    let submission =
        prepare_pinned_proposal_submission(verified, request, &root.path().join("handoff"))
            .unwrap();
    let binding = Binding {
        job_id: "fjob_source".into(),
        attempt_id: "fatt_source".into(),
        attempt_fence: 1,
        job_digest: format!("sha256:{}", "1".repeat(64)),
        archive_descriptor: descriptor,
    };
    let pointer = FormationSourcePointer {
        source_archive_job_id: binding.job_id.clone(),
        binding_digest: binding.digest().unwrap(),
    };
    let origin = FormationSourceOrigin {
        result: SourceArchiveOutput { kind: "source_archive".into(), status: "source_materialized".into(), binding_digest: pointer.binding_digest.clone(), source_revision_id: "srev_fixture".into(), source_materialization_id: "smat_fixture".into(), job_id: binding.job_id, attempt_id: binding.attempt_id, attempt_fence: binding.attempt_fence, job_digest: binding.job_digest, archive_descriptor: binding.archive_descriptor },
        source_selection: serde_json::from_value(json!({"kind":"git_hub","owner":"owner","repository":"repo","requested_ref":"main","resolved_commit_sha":"a".repeat(40),"subdirectory":""})).unwrap(),
    };
    (root, submission, pointer, origin)
}

fn source_handoff_accept(listener: &std::net::TcpListener) -> std::net::TcpStream {
    use std::time::Instant;
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(e) => panic!("expected request before bounded deadline: {e}"),
        }
    }
}
fn source_handoff_request(stream: &mut std::net::TcpStream) -> (String, Vec<String>, Value) {
    use std::io::{BufRead, BufReader};
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    let mut headers = Vec::new();
    let mut length = 0;
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
        headers.push(line);
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).unwrap();
    (first, headers, serde_json::from_slice(&bytes).unwrap())
}
fn source_handoff_reply(stream: &mut std::net::TcpStream, status: u16, body: &Value) {
    let bytes = serde_json::to_vec(body).unwrap();
    write!(stream, "HTTP/1.1 {status} fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
    stream.write_all(&bytes).unwrap();
}
fn source_handoff_no_more_requests(listener: std::net::TcpListener) {
    use std::time::Instant;
    listener.set_nonblocking(true).unwrap();
    let until = Instant::now() + Duration::from_millis(75);
    while Instant::now() < until {
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock,
            "unexpected retry, fallback, or legacy upload"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn source_handoff_posts_exact_pointer_then_unchanged_request_without_reupload_or_budget_reset() {
    let (_root, sub, pointer, origin) = source_handoff_fixture();
    let mut archive = Vec::new();
    sub.source_file()
        .unwrap()
        .read_to_end(&mut archive)
        .unwrap();
    let mut stopped = status(&sub);
    stopped["status"] = json!("stopped");
    stopped["proposal_point"] = Value::Null;
    stopped["search_state"]["owner_stopped"] = json!(true);
    println!(
        "source_handoff_vector={}",
        json!({"request":sub.request,"pointer":pointer,"origin":origin,
        "archive_base64":BASE64.encode(archive),"stopped_status":stopped})
    );
    let before = serde_jcs::to_vec(&sub.request).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let client = Client::new(
        &format!("http://{}", listener.local_addr().unwrap()),
        "owner-fake-token",
    )
    .unwrap();
    let (p, o, source, request) = (
        json!(pointer),
        json!(origin),
        json!(sub.request.source),
        json!(sub.request),
    );
    let expected_query = format!(
        "POST /v1/runtime-network/satisfy?source_archive_job_id=fjob_source&binding_digest={} HTTP/1.1\r\n",
        pointer.binding_digest.replace(':', "%3A")
    );
    let server = std::thread::spawn(move || {
        for replay in [false, true] {
            let mut stream = source_handoff_accept(&listener);
            let (line, headers, body) = source_handoff_request(&mut stream);
            assert_eq!(
                line,
                "POST /v1/runtime-network/sources/from-formation HTTP/1.1\r\n"
            );
            assert!(
                headers
                    .iter()
                    .any(|h| h.eq_ignore_ascii_case("authorization: Bearer owner-fake-token\r\n"))
            );
            assert_eq!(body, p);
            source_handoff_reply(
                &mut stream,
                201,
                &json!({"source":source,"source_origin":o,"formation_started":false}),
            );
            let mut stream = source_handoff_accept(&listener);
            let (line, _, body) = source_handoff_request(&mut stream);
            assert_eq!(line, expected_query);
            assert_eq!(body, request);
            assert!(body.get("source_archive_job_id").is_none());
            assert!(body.get("binding_digest").is_none());
            source_handoff_reply(
                &mut stream,
                if replay { 200 } else { 201 },
                &json!({"search_id":request["search_id"],"satisfy_id":"satisfy_exact","existing":replay}),
            );
        }
        source_handoff_no_more_requests(listener);
    });
    for replay in [false, true] {
        let answer = client
            .submit_from_formation_source(&sub, &pointer, &origin)
            .unwrap();
        assert_eq!(answer["existing"], replay);
        assert_eq!(answer["satisfy_id"], "satisfy_exact");
    }
    server.join().unwrap();
    assert_eq!(serde_jcs::to_vec(&sub.request).unwrap(), before);
}

#[test]
fn source_handoff_refuses_changed_transfer_before_satisfy() {
    for field in [
        "source",
        "attempt",
        "binding",
        "selection",
        "started",
        "unknown",
        "oversize",
    ] {
        let (_root, sub, pointer, origin) = source_handoff_fixture();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = Client::new(
            &format!("http://{}", listener.local_addr().unwrap()),
            "fixture",
        )
        .unwrap();
        let mut reply =
            json!({"source":sub.request.source,"source_origin":origin,"formation_started":false});
        match field {
            "source" => reply["source"]["archive_bytes"] = json!(1),
            "attempt" => reply["source_origin"]["result"]["attempt_fence"] = json!(2),
            "binding" => {
                reply["source_origin"]["result"]["binding_digest"] =
                    json!(format!("sha256:{}", "a".repeat(64)))
            }
            "selection" => {
                reply["source_origin"]["source_selection"]["resolved_commit_sha"] =
                    json!("b".repeat(40))
            }
            "started" => reply["formation_started"] = json!(true),
            "unknown" => reply["fully_satisfied"] = json!(true),
            "oversize" => reply["padding"] = json!("x".repeat(65536)),
            _ => unreachable!(),
        }
        let server = std::thread::spawn(move || {
            let mut stream = source_handoff_accept(&listener);
            assert!(
                source_handoff_request(&mut stream)
                    .0
                    .contains("/sources/from-formation ")
            );
            source_handoff_reply(&mut stream, 201, &reply);
            source_handoff_no_more_requests(listener);
        });
        assert!(
            client
                .submit_from_formation_source(&sub, &pointer, &origin)
                .is_err(),
            "{field}"
        );
        server.join().unwrap();
    }
}

#[test]
fn source_handoff_rejects_local_drift_and_expired_deadline_before_any_post() {
    for field in ["pointer", "request", "measurement", "deadline"] {
        let (_root, mut sub, mut pointer, mut origin) = source_handoff_fixture();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = Client::new(
            &format!("http://{}", listener.local_addr().unwrap()),
            "fixture",
        )
        .unwrap();
        match field {
            "pointer" => pointer.source_archive_job_id = "fjob_another".into(),
            "request" => sub.request.source.archive_bytes = Some(1),
            "measurement" => {
                origin.result.archive_descriptor.observed_file_count += 1;
                pointer.binding_digest = crate::source_archive::Binding {
                    job_id: origin.result.job_id.clone(),
                    attempt_id: origin.result.attempt_id.clone(),
                    attempt_fence: origin.result.attempt_fence,
                    job_digest: origin.result.job_digest.clone(),
                    archive_descriptor: origin.result.archive_descriptor.clone(),
                }
                .digest()
                .unwrap();
                origin.result.binding_digest = pointer.binding_digest.clone();
            }
            "deadline" => client = client.with_deadline(1),
            _ => unreachable!(),
        }
        assert!(
            client
                .submit_from_formation_source(&sub, &pointer, &origin)
                .is_err(),
            "{field}"
        );
        source_handoff_no_more_requests(listener);
    }
}

#[test]
fn source_handoff_refused_transfer_or_satisfy_is_not_retried_or_fallen_back() {
    for stage in ["transfer", "satisfy", "redirect"] {
        let (_root, sub, pointer, origin) = source_handoff_fixture();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let client = Client::new(&base, "fixture").unwrap();
        let transfer =
            json!({"source":sub.request.source,"source_origin":origin,"formation_started":false});
        let server = std::thread::spawn(move || {
            let mut stream = source_handoff_accept(&listener);
            source_handoff_request(&mut stream);
            if stage == "redirect" {
                write!(stream,"HTTP/1.1 307 redirect\r\nLocation: {base}/forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            } else if stage == "transfer" {
                source_handoff_reply(
                    &mut stream,
                    409,
                    &json!({"error":"source_archive_cancelled"}),
                );
            } else {
                source_handoff_reply(&mut stream, 201, &transfer);
                let mut stream = source_handoff_accept(&listener);
                source_handoff_request(&mut stream);
                source_handoff_reply(
                    &mut stream,
                    409,
                    &json!({"error":"formation_effect_unknown"}),
                );
            }
            source_handoff_no_more_requests(listener);
        });
        assert!(
            client
                .submit_from_formation_source(&sub, &pointer, &origin)
                .is_err(),
            "{stage}"
        );
        server.join().unwrap();
    }
}

#[test]
fn legacy_requester_submit_keeps_existing_upload_and_plain_satisfy() {
    let (_root, sub, _, _) = source_handoff_fixture();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let client = Client::new(
        &format!("http://{}", listener.local_addr().unwrap()),
        "fixture",
    )
    .unwrap();
    let server = std::thread::spawn(move || {
        let mut stream = source_handoff_accept(&listener);
        assert_eq!(
            source_handoff_request(&mut stream).0,
            "POST /v1/runtime-network/sources HTTP/1.1\r\n"
        );
        source_handoff_reply(
            &mut stream,
            200,
            &json!({"upload_id":"upload_existing","status":"ready"}),
        );
        let mut stream = source_handoff_accept(&listener);
        assert_eq!(
            source_handoff_request(&mut stream).0,
            "POST /v1/runtime-network/satisfy HTTP/1.1\r\n"
        );
        source_handoff_reply(&mut stream, 201, &json!({"satisfy_id":"legacy"}));
    });
    assert_eq!(client.submit(&sub).unwrap()["satisfy_id"], "legacy");
    server.join().unwrap();
}
