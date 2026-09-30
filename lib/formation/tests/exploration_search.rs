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
    });
    s.frozen.policy.proposal = Some(ProposalAuthorization {
        execution_plan: Some(PlanAuthorization {
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
