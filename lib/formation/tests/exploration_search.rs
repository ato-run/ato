//! Scheduler/compiler tests only; real provider/Coordinator gates are separate.
use ato_formation::{
    authoring::{BindingContext, bind},
    capsule_toml::parse_capsule_toml,
    exploration::{ExplorationPolicy, FormationConfig},
    proposal::*,
    requirements::{ExecutionPhase, ExecutionRequirements, NetworkRequirement},
    search::*,
};
use serde_json::json;
use std::collections::BTreeMap;

fn state() -> SearchStateV1 {
    let mut s: SearchStateV1 =
        serde_json::from_str(include_str!("fixtures/search-state/d1-failed.json")).unwrap();
    let source = format!("sha256:{}", "a".repeat(64));
    let (k, _) = bind(
        &parse_capsule_toml(include_str!("fixtures/proposal-python.toml")).unwrap(),
        &BindingContext {
            source_closure_ref: &source,
        },
    )
    .unwrap();
    s.frozen.base_contract_ref = k.contract_ref().unwrap();
    s.frozen.contract_ref = s.frozen.base_contract_ref.clone();
    s.frozen.base_contract = k;
    s.frozen.candidates.clear();
    s.frozen.initial_source = Some(InitialSource {
        closure_ref: source.clone(),
        archive_digest: source,
    });
    s.attempts.clear();
    s.source_archive_bytes = Some(64);
    s.budget = BudgetCounters::default();
    s.frozen.policy.exploration = Some(ExplorationPolicy {
        formation: FormationConfig::default(),
        ceiling: ExecutionRequirements::default(),
        max_provider_calls: 3,
        max_inspections: 3,
        max_provider_cost_usd_micros: 100_000,
        max_provider_input_tokens: 90_000,
        max_provider_output_tokens: 6144,
        max_network_transfer_bytes: 1048576,
        max_network_transfer_bytes_per_attempt: 524288,
        provider_configuration_ref: None,
        decision_provider_configuration_ref: None,
        provider_budget_binding_ref: None,
    });
    s.frozen.policy.proposal = Some(ProposalAuthorization {
        execution_plan: Some(PlanAuthorization {
            source_oci: None,
            files: BTreeMap::from([(
                "server".into(),
                VerifiedSourceFile {
                    path: "server.py".into(),
                    digest: format!("sha256:{}", "b".repeat(64)),
                },
            )]),
            toolchains: BTreeMap::from([("python".into(), "3.12.7".into())]),
        }),
        modifiable_derivation_refs: vec![],
        source_domain: SourceDomain {
            entrypoints: BTreeMap::new(),
            modules: BTreeMap::new(),
        },
        python_http_process: None,
        node_static_workspace: None,
        policy: CandidateProducerPolicy {
            max_proposal_rounds: 1,
            max_proposals: 1,
            timeout_ms: 5000,
            allow_source_text: false,
            max_source_bytes: 0,
        },
    });
    s
}

fn plan() -> serde_json::Value {
    json!({"runtime":{"name":"python","version":"3.12.7"},
        "entrypoint":{"file_id":"server","digest":format!("sha256:{}","b".repeat(64))},
        "argv":[],"cwd":".","guest_port":8000,"dependencies":[],"build_scripts":[],
        "requirements":{},"basis":[],"unknowns":[]})
}

fn compile(s: &SearchStateV1, plan: serde_json::Value) -> Vec<ProposalOutcome> {
    let bytes = serde_json::to_vec(&json!({"schema":PROPOSAL_SCHEMA,"proposals":[
        {"kind":"propose_derivation","operations":[{"operation":"execution_plan@1","plan":plan}]}]})).unwrap();
    CandidateRegistry::new(&s.frozen)
        .unwrap()
        .validate_batch(
            &BTreeMap::new(),
            &ProducerOutput::new(
                bytes,
                ProducerProvenance {
                    provider: "fixed".into(),
                    model: None,
                },
            )
            .unwrap(),
        )
        .unwrap()
}

#[test]
fn legacy_catalog_empty_does_not_prevent_a_typed_execution_plan() {
    let s = state();
    assert!(matches!(
        s.frozen
            .policy
            .proposal
            .as_ref()
            .unwrap()
            .catalog()
            .unwrap()
            .operations[0],
        OperationDomain::ExecutionPlan { .. }
    ));
    let outcomes = compile(&s, plan());
    let ProposalOutcome::Admitted(candidate) = &outcomes[0] else {
        panic!("{outcomes:?}")
    };
    assert_eq!(
        candidate.compiled().base_contract_ref,
        s.frozen.base_contract_ref
    );
    assert_eq!(
        candidate.compiled().derivation.steps[0].argv[1],
        "--version"
    );
    assert_eq!(
        candidate.compiled().derivation.steps[1].argv[1],
        "/app/server.py"
    );
}

#[test]
fn unsupported_toolchain_is_a_validator_diagnostic() {
    let mut p = plan();
    p["runtime"]["name"] = json!("go");
    assert!(
        matches!(&compile(&state(),p)[0],ProposalOutcome::Rejected(e) if e.0=="unsupported_toolchain")
    );
}

#[test]
fn stale_source_digest_is_not_lowered_to_a_process() {
    let mut p = plan();
    p["entrypoint"]["digest"] = json!(format!("sha256:{}", "c".repeat(64)));
    assert!(
        matches!(&compile(&state(),p)[0],ProposalOutcome::Rejected(e) if e.0=="proposal_source_digest_mismatch")
    );
}

#[test]
fn an_out_of_ceiling_requirement_is_saved_as_a_specific_rejection() {
    let mut p = plan();
    p["requirements"] = json!({"network":[{"phase":"dependencies","host":"pypi.org","port":443}]});
    assert!(
        matches!(&compile(&state(),p)[0],ProposalOutcome::Rejected(e) if e.0=="exploration_authority_exceeded")
    );
}

#[test]
fn default_three_failed_provider_rounds_exhaust_without_reset_on_restart() {
    let mut s = state();
    let frozen = s.frozen.canonical_bytes().unwrap();
    for round in 0..3 {
        let now = 100 + round * 6000;
        let SearchAction::OpenProposalRound {
            opened_at_ms,
            expires_at_ms,
        } = decide_next(&s, &[], now).unwrap()
        else {
            panic!("round {round}")
        };
        if let Some(previous) = s.proposal_round.take() {
            s.proposal_history.push(previous);
        }
        s.proposal_round = Some(ProposalRoundRecord {
            opened_at_ms,
            expires_at_ms,
            outcome: Some(ProposalRoundOutcome::ProviderError),
            candidates: vec![],
            derivations: vec![],
            diagnostics: vec![],
            inspection_requests: vec![],
        });
        s = serde_json::from_slice(&s.canonical_bytes().unwrap()).unwrap();
        assert_eq!(s.frozen.canonical_bytes().unwrap(), frozen);
    }
    assert_eq!(
        decide_next(&s, &[], 19_000).unwrap(),
        SearchAction::Finish {
            reason: Termination::RoundsExhausted
        }
    );
    assert_eq!(s.proposal_history.len(), 2);
}

#[test]
fn configured_one_round_keeps_its_effective_limit_after_serialization() {
    let mut s = state();
    s.frozen
        .policy
        .exploration
        .as_mut()
        .unwrap()
        .formation
        .max_rounds = std::num::NonZeroU32::MIN;
    s.proposal_round = Some(ProposalRoundRecord {
        opened_at_ms: 100,
        expires_at_ms: 5100,
        outcome: Some(ProposalRoundOutcome::Timeout),
        candidates: vec![],
        derivations: vec![],
        diagnostics: vec![],
        inspection_requests: vec![],
    });
    let restored: SearchStateV1 = serde_json::from_slice(&s.canonical_bytes().unwrap()).unwrap();
    assert_eq!(
        decide_next(&restored, &[], 6000).unwrap(),
        SearchAction::Finish {
            reason: Termination::RoundsExhausted
        }
    );
}

#[test]
fn independent_provider_cap_stops_before_receiver_refuses_another_round() {
    let mut s = state();
    let policy = s.frozen.policy.exploration.as_mut().unwrap();
    policy.formation.max_rounds = std::num::NonZeroU32::new(5).unwrap();
    policy.max_provider_calls = 4;
    for index in 0..4 {
        let now = 100 + index * 6000;
        let SearchAction::OpenProposalRound {
            opened_at_ms,
            expires_at_ms,
        } = decide_next(&s, &[], now).unwrap()
        else {
            panic!("{index}")
        };
        if let Some(previous) = s.proposal_round.take() {
            s.proposal_history.push(previous);
        }
        s.proposal_round = Some(ProposalRoundRecord {
            opened_at_ms,
            expires_at_ms,
            outcome: None,
            candidates: vec![],
            derivations: vec![],
            diagnostics: vec![],
            inspection_requests: vec![],
        });
        assert!(matches!(
            decide_next(&s, &[], now + 1).unwrap(),
            SearchAction::WaitForProposalRound { .. }
        ));
        s.proposal_round.as_mut().unwrap().outcome = Some(ProposalRoundOutcome::Completed);
        s = serde_json::from_slice(&s.canonical_bytes().unwrap()).unwrap();
    }
    assert_eq!(
        decide_next(&s, &[], 25000).unwrap(),
        SearchAction::Finish {
            reason: Termination::BudgetExhausted
        }
    );
    assert_eq!(s.proposal_history.len() + 1, 4);
    assert_eq!(
        s.frozen
            .policy
            .exploration
            .unwrap()
            .formation
            .max_rounds
            .get(),
        5
    );
}

#[test]
fn ceiling_changes_are_separate_from_frozen_contract() {
    let mut s = state();
    let before = s.frozen.canonical_bytes().unwrap();
    let k = s.frozen.contract_ref.clone();
    s.frozen
        .policy
        .exploration
        .as_mut()
        .unwrap()
        .ceiling
        .network
        .push(NetworkRequirement {
            phase: ExecutionPhase::Dependencies,
            host: "pypi.org".into(),
            port: 443,
        });
    assert_ne!(s.frozen.canonical_bytes().unwrap(), before);
    assert_eq!(s.frozen.contract_ref, k);
}

#[test]
fn module_state_and_public_environment_are_canonical_and_source_bound() {
    let mut s = state();
    s.frozen
        .policy
        .proposal
        .as_mut()
        .unwrap()
        .execution_plan
        .as_mut()
        .unwrap()
        .files
        .get_mut("server")
        .unwrap()
        .path = "demo/__main__.py".into();
    let mut p = plan();
    p["module"] = json!("demo");
    p["environment"] = json!({"HOME":"/data"});
    p["state"] = json!([{"id":"app.data","mount":"/data","access":"read-write"}]);
    let out = compile(&s, p.clone());
    let ProposalOutcome::Admitted(candidate) = &out[0] else {
        panic!("{out:?}");
    };
    assert_eq!(candidate.compiled().derivation.state[0].mount, "/data");
    assert_eq!(
        candidate.compiled().derivation.steps.last().unwrap().argv[1..],
        ["-m", "demo"]
    );
    p["module"] = json!("other");
    assert!(matches!(
        &compile(&s, p)[0],
        ProposalOutcome::Rejected(ProposalError("proposal_module_source_mismatch"))
    ));
}

#[test]
fn state_cannot_mask_shim_system_or_policy_and_env_cannot_inject_into_owner() {
    let s = state();
    for path in [
        "/",
        "/app",
        "/.ato/ingress",
        "/etc",
        "/opt/ato/toolchains",
        "/data/../etc",
    ] {
        let mut p = plan();
        p["state"] = json!([{"id":"app.data","mount":path,"access":"read-write"}]);
        assert!(matches!(
            &compile(&s, p)[0],
            ProposalOutcome::Rejected(ProposalError("execution_plan_bounds"))
        ));
    }
    for key in [
        "LD_PRELOAD",
        "DYLD_INSERT_LIBRARIES",
        "ATO_RUN_TOKEN",
        "HTTP_PROXY",
        "API_SECRET",
        "NODE_OPTIONS",
    ] {
        let mut p = plan();
        p["environment"] = json!({key: "ignored"});
        assert!(matches!(
            &compile(&s, p)[0],
            ProposalOutcome::Rejected(ProposalError("execution_plan_bounds"))
        ));
    }
}

#[test]
fn bounded_inspection_is_validated_and_consumed_without_authorizing_a_d() {
    let mut s = state();
    let policy = &mut s.frozen.policy.proposal.as_mut().unwrap().policy;
    policy.allow_source_text = true;
    policy.max_source_bytes = 16384;
    let source = json!({"file_id":"server","digest":format!("sha256:{}","b".repeat(64))});
    let batch = serde_json::to_vec(&json!({"schema":PROPOSAL_SCHEMA,"proposals":[{"kind":"inspect_source","sources":[source]}]})).unwrap();
    let output = ProducerOutput::new(
        batch,
        ProducerProvenance {
            provider: "fixed".into(),
            model: None,
        },
    )
    .unwrap();
    let mut registry = CandidateRegistry::new(&s.frozen)
        .unwrap()
        .with_inspection_budget(1)
        .unwrap();
    assert!(
        matches!(&registry.validate_batch(&BTreeMap::new(), &output).unwrap()[0], ProposalOutcome::InspectionRequested(sources) if sources.len() == 1)
    );
    assert!(registry.generated().is_empty());
    assert!(matches!(
        &registry.validate_batch(&BTreeMap::new(), &output).unwrap()[0],
        ProposalOutcome::Rejected(ProposalError("inspection_budget_exhausted"))
    ));
}

#[test]
fn public_catalog_is_bounded_and_does_not_export_the_path_map() {
    let mut s = state();
    let auth = s
        .frozen
        .policy
        .proposal
        .as_mut()
        .unwrap()
        .execution_plan
        .as_mut()
        .unwrap();
    for n in 0..100 {
        auth.files.insert(
            format!("file{n}"),
            VerifiedSourceFile {
                path: format!("nested/path/{n}.py"),
                digest: format!("sha256:{}", "b".repeat(64)),
            },
        );
    }
    let catalog = s
        .frozen
        .policy
        .proposal
        .as_ref()
        .unwrap()
        .catalog()
        .unwrap();
    let OperationDomain::ExecutionPlan { sources, .. } = &catalog.operations[0] else {
        panic!()
    };
    assert_eq!(sources.len(), 32);
    assert_eq!(sources[0].reference.file_id, "server");
    let bytes = serde_json::to_string(&catalog).unwrap();
    assert!(!bytes.contains("nested/path"));
    assert!(!bytes.contains("server.py"));
    let restored: OperationCatalog = serde_json::from_str(&bytes).unwrap();
    assert_eq!(restored, catalog);
}

#[test]
fn source_oci_proposal_is_canonical_source_bound_and_not_a_shell_plan() {
    let mut s = state();
    let auth = s
        .frozen
        .policy
        .proposal
        .as_mut()
        .unwrap()
        .execution_plan
        .as_mut()
        .unwrap();
    auth.files.get_mut("server").unwrap().path = "Dockerfile".into();
    auth.toolchains.insert("oci".into(), "1.0.0".into());
    auth.source_oci=Some(serde_json::from_value(json!({"schema":"ato.source-oci-recipe/1","dockerfile":"Dockerfile","platform":"linux/arm64","base_images":[],
        "build":{"memory_bytes":536870912,"cpu_limit_millis":1000,"pids_limit":128},"build_disk_bytes":536870912,
        "runtime":{"memory_bytes":268435456,"cpu_limit_millis":1000,"pids_limit":128},"build_timeout_seconds":60,"max_archive_bytes":1048576})).unwrap());
    let mut p = plan();
    p["runtime"] = json!({"name":"oci","version":"1.0.0"});
    let outcomes = compile(&s, p.clone());
    let ProposalOutcome::Admitted(c) = &outcomes[0] else {
        panic!("{outcomes:?}")
    };
    assert_eq!(c.compiled().derivation.steps.len(), 1);
    assert!(c.compiled().derivation.steps[0].argv.is_empty());
    assert!(c.compiled().derivation.source_oci.is_some());
    assert_eq!(c.compiled().base_contract_ref, s.frozen.base_contract_ref);
    p["argv"] = json!(["sh", "-c", "echo arbitrary"]);
    assert!(matches!(compile(&s, p)[0], ProposalOutcome::Rejected(_)));
}

#[test]
fn reusable_static_preset_authoring_roundtrips_without_changing_d_or_k() {
    use ato_formation::{
        capsule_toml::render_capsule_toml,
        preset::{AppPreset, synthesize_authoring},
    };
    let closure = format!("sha256:{}", "a".repeat(64));
    for preset in [AppPreset::StaticFiles, AppPreset::SingleHtml] {
        let before = synthesize_authoring(preset);
        let rendered = render_capsule_toml(&before).unwrap();
        let restored = parse_capsule_toml(&rendered).unwrap();
        assert_eq!(
            bind(
                &before,
                &BindingContext {
                    source_closure_ref: &closure
                }
            )
            .unwrap(),
            bind(
                &restored,
                &BindingContext {
                    source_closure_ref: &closure
                }
            )
            .unwrap()
        );
    }
    assert!(render_capsule_toml(&synthesize_authoring(AppPreset::NodeStatic)).is_err());
}
