use super::*;
use ato_formation::proposal::ProposalRoundRecord;
use ato_formation::proposal::{CandidateProducerPolicy, PythonHttpProcess, SourceDomain};
use ato_formation::search::BudgetCounters;

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
        proposal_history:vec![],exploration_submission:None,proposal_round:Some(ProposalRoundRecord {opened_at_ms:now,expires_at_ms:now+30_000,outcome:None,candidates:vec![]}),
    },"proposal_point":{"round_seq":1,"revision":1,"claimed":false},"proposal_round":{"status":"open"}})
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
        None
    );
}
