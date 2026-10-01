//! Pure compiler/scheduler tests: these do not execute a Runtime or attest PASS.
use ato_formation::{
    authoring::{BindingContext, bind},
    capsule_toml::parse_capsule_toml,
    decision::*,
    proposal::*,
    search::*,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const BASE: &str = include_str!("fixtures/proposal-python.toml");
fn state() -> SearchStateV1 {
    let mut s: SearchStateV1 =
        serde_json::from_str(include_str!("fixtures/search-state/d1-failed.json")).unwrap();
    let closure = format!("sha256:{}", "a".repeat(64));
    let (k, d) = bind(
        &parse_capsule_toml(BASE).unwrap(),
        &BindingContext {
            source_closure_ref: &closure,
        },
    )
    .unwrap();
    s.frozen.base_contract_ref = k.contract_ref().unwrap();
    s.frozen.contract_ref = s.frozen.base_contract_ref.clone();
    s.frozen.base_contract = k;
    s.frozen.candidates.truncate(1);
    let base = &mut s.frozen.candidates[0];
    base.derivation_ref = d.derivation_ref().unwrap();
    base.materialization = CandidateInput::Source {
        closure_ref: closure.clone(),
        archive_digest: closure.clone(),
    };
    s.attempts[0].derivation_ref = base.derivation_ref.clone();
    s.source_archive_bytes = Some(64);
    s.frozen.initial_source = Some(InitialSource {
        closure_ref: closure.clone(),
        archive_digest: closure,
    });
    s.frozen.policy.proposal = Some(ProposalAuthorization {
        execution_plan: None,
        modifiable_derivation_refs: vec![base.derivation_ref.clone()],
        python_http_process: Some(PythonHttpProcess {
            python_version: "3.12.7".into(),
            http_port: "app.http".into(),
            guest_port: 8000,
        }),
        node_static_workspace: None,
        source_domain: SourceDomain {
            entrypoints: BTreeMap::from([("entry_a".into(), "working.py".into())]),
            modules: BTreeMap::from([("module_a".into(), "pkg.server".into())]),
        },
        policy: CandidateProducerPolicy {
            max_proposal_rounds: 1,
            max_proposals: 4,
            timeout_ms: 5000,
            allow_source_text: false,
            max_source_bytes: 0,
        },
    });
    s
}
fn script() -> Value {
    json!({"kind":"modify_derivation","base_derivation_ref":base_ref(),"operations":[{"operation":"python_script@1","entrypoint_id":"entry_a"}]})
}
fn module() -> Value {
    json!({"kind":"modify_derivation","base_derivation_ref":base_ref(),"operations":[{"operation":"python_module@1","module_id":"module_a"}]})
}
fn batch(proposals: Vec<Value>) -> ProducerOutput {
    output(&serde_json::to_vec(&json!({"schema":PROPOSAL_SCHEMA,"proposals":proposals})).unwrap())
}
fn output(bytes: &[u8]) -> ProducerOutput {
    ProducerOutput::new(
        bytes.to_vec(),
        ProducerProvenance {
            provider: "fixed".into(),
            model: None,
        },
    )
    .unwrap()
}
fn base_ref() -> String {
    state().frozen.candidates[0].derivation_ref.clone()
}
fn recipes(text: &str) -> BTreeMap<String, String> {
    BTreeMap::from([(base_ref(), text.into())])
}
fn admit(s: &mut SearchStateV1) {
    let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
    registry
        .validate_batch(&recipes(BASE), &batch(vec![script(), module()]))
        .unwrap();
    let candidates = registry
        .generated()
        .iter()
        .map(|c| c.candidate().clone())
        .collect();
    let SearchAction::OpenProposalRound {
        opened_at_ms,
        expires_at_ms,
    } = decide_next(s, &[], 10).unwrap()
    else {
        panic!("expected proposal round")
    };
    s.proposal_round = Some(ProposalRoundRecord {
        opened_at_ms,
        expires_at_ms,
        outcome: Some(ProposalRoundOutcome::Completed),
        candidates,
        derivations: vec![],
        diagnostics: vec![],
        inspection_requests: vec![],
    });
}
fn placements(s: &SearchStateV1) -> Vec<Placement> {
    s.candidates()
        .map(|c| Placement {
            candidate_id: c.derivation_ref.clone(),
            derivation_ref: c.derivation_ref.clone(),
            runtime_id: "runtime".into(),
            environment_id: "native".into(),
            admissible: true,
            transfer_bytes: 64,
        })
        .collect()
}
#[test]
fn operation_and_typed_argument_compose_new_ds_without_precomputed_refs_or_k_change() {
    let s = state();
    let frozen = s.frozen.canonical_bytes().unwrap();
    let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
    let outcomes = registry
        .validate_batch(&recipes(BASE), &batch(vec![script(), module()]))
        .unwrap();
    assert!(
        outcomes
            .iter()
            .all(|o| matches!(o, ProposalOutcome::Admitted(_)))
    );
    assert_eq!(registry.candidates().count(), 3);
    for generated in registry.generated() {
        assert_eq!(
            generated.compiled().base_contract_ref,
            s.frozen.base_contract_ref
        );
        assert_ne!(
            generated.candidate().derivation_ref,
            s.frozen.candidates[0].derivation_ref
        );
        assert_eq!(
            generated.compiled().derivation.derivation_ref().unwrap(),
            generated.candidate().derivation_ref
        );
    }
    assert_eq!(
        registry.generated()[1].compiled().derivation.steps[0].argv[2..],
        ["-m", "pkg.server"]
    );
    assert_eq!(s.frozen.canonical_bytes().unwrap(), frozen);
}
#[test]
fn mixed_batch_admits_two_and_rejects_duplicate_and_unauthorized_individually() {
    let s = state();
    let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
    let mut bad = module();
    bad["operations"][0]["module_id"] = "unknown".into();
    let outcomes = registry
        .validate_batch(
            &recipes(BASE),
            &batch(vec![script(), module(), script(), bad]),
        )
        .unwrap();
    assert!(matches!(
        &outcomes[2],
        ProposalOutcome::Rejected(ProposalError("proposal_duplicate"))
    ));
    assert!(matches!(
        &outcomes[3],
        ProposalOutcome::Rejected(ProposalError("proposal_id_unauthorized"))
    ));
    assert_eq!(registry.generated().len(), 2);
}
#[test]
fn unknown_fields_cannot_smuggle_authority_into_proposals_or_arguments() {
    for field in [
        "K",
        "contract",
        "derivation_ref",
        "proposal_id",
        "runtime_constraint",
        "argv",
        "shell",
        "url",
        "path",
        "secret",
        "bindings",
        "network",
        "effects",
    ] {
        for in_operation in [false, true] {
            let s = state();
            let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
            let mut bad = script();
            if in_operation {
                bad["operations"][0][field] = "injected".into();
            } else {
                bad[field] = "injected".into();
            }
            let results = registry
                .validate_batch(&recipes(BASE), &batch(vec![bad, module()]))
                .unwrap();
            assert!(
                matches!(&results[0], ProposalOutcome::Rejected(_)),
                "{field}"
            );
            assert_eq!(registry.generated().len(), 1);
        }
    }
}
#[test]
fn duplicate_json_keys_are_rejected_in_original_member_bytes() {
    let s = state();
    let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
    let raw=br#"{"schema":"ato.formation-proposal/1","proposals":[{"kind":"propose_derivation","operations":[{"operation":"python_script@1","entrypoint_id":"bad","entrypoint_id":"entry_a"}]}]}"#;
    let results = registry
        .validate_batch(&recipes(BASE), &output(raw))
        .unwrap();
    assert!(matches!(results[0], ProposalOutcome::Rejected(_)));
    assert!(registry.generated().is_empty());
}
#[test]
fn unauthorized_shell_operation_and_wrong_argument_domain_cannot_compile() {
    for operation in [
        json!({"operation":"shell@1","entrypoint_id":"entry_a"}),
        json!({"operation":"python_script@1","entrypoint_id":"/app/working.py"}),
        json!({"operation":"python_module@1","module_id":"entry_a"}),
    ] {
        let s = state();
        let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
        let results = registry
            .validate_batch(
                &recipes(BASE),
                &batch(vec![
                    json!({"kind":"propose_derivation","operations":[operation]}),
                ]),
            )
            .unwrap();
        assert!(matches!(results[0], ProposalOutcome::Rejected(_)));
        assert!(registry.generated().is_empty());
    }
}
#[test]
fn batch_schema_size_and_count_are_bounded_before_admission() {
    for bytes in [
        batch(vec![script(); 5]).raw().to_vec(),
        b"{\"schema\":\"wrong\",\"proposals\":[]}".to_vec(),
        b"{\"schema\":\"ato.formation-proposal/1\",\"proposals\":[],\"unknown\":true}".to_vec(),
    ] {
        let s = state();
        let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
        assert!(
            registry
                .validate_batch(&recipes(BASE), &output(&bytes))
                .is_err()
        );
        assert!(registry.generated().is_empty());
    }
}
#[test]
fn unsupported_and_empty_batch_do_not_create_candidates_or_k_failure() {
    let s = state();
    let before = s.attempts.clone();
    let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
    assert_eq!(
        registry
            .validate_batch(&recipes(BASE), &batch(vec![json!({"kind":"unsupported"})]))
            .unwrap(),
        vec![ProposalOutcome::Unsupported]
    );
    assert!(
        registry
            .validate_batch(&recipes(BASE), &batch(vec![]))
            .unwrap()
            .is_empty()
    );
    assert!(registry.generated().is_empty());
    assert_eq!(s.attempts, before);
}
#[test]
fn modification_requires_authorized_base_and_exactly_one_operation() {
    for proposal in [
        json!({"kind":"modify_derivation","base_derivation_ref":"wrong","operations":[script()["operations"][0].clone()]}),
        json!({"kind":"propose_derivation","operations":[]}),
        json!({"kind":"propose_derivation","operations":[script()["operations"][0].clone(),module()["operations"][0].clone()]}),
    ] {
        let s = state();
        let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
        assert!(matches!(
            registry
                .validate_batch(&recipes(BASE), &batch(vec![proposal]))
                .unwrap()[0],
            ProposalOutcome::Rejected(_)
        ));
    }
}
#[test]
fn canonical_proposal_identity_is_ato_owned_and_whitespace_independent() {
    let s = state();
    let proposal = json!({"kind":"modify_derivation","base_derivation_ref":s.frozen.candidates[0].derivation_ref,"operations":script()["operations"].clone()});
    let bytes = batch(vec![proposal]);
    let value: Value = serde_json::from_slice(bytes.raw()).unwrap();
    let mut a = CandidateRegistry::new(&s.frozen).unwrap();
    let mut b = CandidateRegistry::new(&s.frozen).unwrap();
    a.validate_batch(&recipes(BASE), &bytes).unwrap();
    b.validate_batch(
        &recipes(BASE),
        &output(&serde_json::to_vec_pretty(&value).unwrap()),
    )
    .unwrap();
    assert_eq!(a.generated(), b.generated());
}
#[test]
fn owner_domain_policy_and_frozen_authorization_fail_closed() {
    let s = state();
    let auth = s.frozen.policy.proposal.as_ref().unwrap();
    for path in [
        "../x.py",
        "/x.py",
        ".secret.py",
        "https://example.test/x.py",
    ] {
        let mut bad = auth.clone();
        bad.source_domain
            .entrypoints
            .insert("entry_a".into(), path.into());
        assert!(bad.validate().is_err());
    }
    for module in [
        "os;cmd",
        "../server",
        "os/path",
        "-m",
        "pkg..main",
        "pkg.__main__",
    ] {
        let mut bad = auth.clone();
        bad.source_domain
            .modules
            .insert("module_a".into(), module.into());
        assert!(bad.validate().is_err());
    }
    let mut bad = auth.clone();
    bad.policy.max_proposal_rounds = 2;
    assert!(bad.validate().is_err());
    bad = auth.clone();
    bad.policy.timeout_ms = 30001;
    assert!(bad.validate().is_err());
    bad = auth.clone();
    bad.policy.allow_source_text = true;
    assert!(bad.validate().is_err());
    // There is no second authorization argument to the validator: only the
    // frozen search's owner authorization can be used.
    bad = auth.clone();
    bad.modifiable_derivation_refs = vec![format!("sha256:{}", "f".repeat(64))];
    let mut altered = s.frozen.clone();
    altered.policy.proposal = Some(bad);
    assert!(CandidateRegistry::new(&altered).is_err());
    assert!(
        ProducerOutput::new(
            vec![b' '; MAX_BATCH_BYTES + 1],
            ProducerProvenance {
                provider: "fixed".into(),
                model: None
            }
        )
        .is_err()
    );
}
#[test]
fn contract_or_base_recipe_changes_are_rejected_not_silently_recaptured() {
    let s = state();
    for text in [
        BASE.replace("status = 200", "status = 201"),
        BASE.replace("broken.py", "other.py"),
        BASE.replace("3.12.7", "3.13.0"),
    ] {
        let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
        let results = registry
            .validate_batch(&recipes(&text), &batch(vec![script()]))
            .unwrap();
        assert!(matches!(results[0], ProposalOutcome::Rejected(_)));
        assert!(registry.generated().is_empty());
    }
}
#[test]
fn operation_catalog_contains_only_opaque_ids_not_paths_modules_or_refs() {
    let s = state();
    let catalog = serde_json::to_string(
        &s.frozen
            .policy
            .proposal
            .as_ref()
            .unwrap()
            .catalog()
            .unwrap(),
    )
    .unwrap();
    assert!(catalog.contains("python_module@1"));
    assert!(catalog.contains("module_a"));
    for private in ["working.py", "pkg.server", "sha256:", "argv", "python3"] {
        assert!(!catalog.contains(private));
    }
}
#[test]
fn deterministic_and_decision_paths_share_generated_candidate_iterator() {
    let mut s = state();
    let frozen = s.frozen.canonical_bytes().unwrap();
    admit(&mut s);
    let placements = placements(&s);
    let refs: Vec<_> = s
        .candidates()
        .skip(1)
        .map(|c| c.derivation_ref.clone())
        .collect();
    assert!(
        matches!(decide_next(&s,&placements,11).unwrap(),SearchAction::IssueAttempt{derivation_ref,..} if derivation_ref==refs[0])
    );
    let offered = allowed_choices(&s, &placements);
    for reference in &refs {
        assert!(offered.iter().any(|c|matches!(&c.action,ChoiceAction::Attempt{derivation_ref,..} if derivation_ref==reference)));
    }
    assert_eq!(s.frozen.canonical_bytes().unwrap(), frozen);
    assert_eq!(s.candidates().count(), 3);
}
#[test]
fn completed_round_rehydrates_and_is_never_reopened() {
    let mut s = state();
    admit(&mut s);
    let restored: SearchStateV1 = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
    assert_eq!(
        decide_next(&s, &placements(&s), 11).unwrap(),
        decide_next(&restored, &placements(&restored), 11).unwrap()
    );
    assert_eq!(restored.candidates().count(), 3);
}
#[test]
fn open_round_waits_and_expires_without_fabricating_attempt_evidence() {
    let mut s = state();
    let before = s.attempts.clone();
    s.proposal_round = Some(ProposalRoundRecord {
        opened_at_ms: 10,
        expires_at_ms: 5010,
        outcome: None,
        candidates: vec![],
        derivations: vec![],
        diagnostics: vec![],
        inspection_requests: vec![],
    });
    assert_eq!(
        decide_next(&s, &[], 11).unwrap(),
        SearchAction::WaitForProposalRound {
            expires_at_ms: 5010
        }
    );
    assert_eq!(
        decide_next(&s, &[], 5010).unwrap(),
        SearchAction::ExpireProposalRound {}
    );
    assert_eq!(s.attempts, before);
}
#[test]
fn provider_error_and_timeout_exhaust_without_k_failure_or_retry() {
    for outcome in [
        ProposalRoundOutcome::ProviderError,
        ProposalRoundOutcome::Timeout,
        ProposalRoundOutcome::Completed,
    ] {
        let mut s = state();
        let before = s.attempts.clone();
        s.proposal_round = Some(ProposalRoundRecord {
            opened_at_ms: 10,
            expires_at_ms: 5010,
            outcome: Some(outcome),
            candidates: vec![],
            derivations: vec![],
            diagnostics: vec![],
            inspection_requests: vec![],
        });
        assert_eq!(
            decide_next(&s, &[], 11).unwrap(),
            SearchAction::Finish {
                reason: Termination::CandidatesExhausted
            }
        );
        assert_eq!(s.attempts, before);
    }
}
#[test]
fn unknown_owner_stop_and_cumulative_budgets_prevent_proposal_or_execution() {
    for generated in [false, true] {
        for condition in [
            "unknown", "stop", "attempts", "transfer", "stored", "expanded", "deadline",
        ] {
            let mut s = state();
            if generated {
                admit(&mut s);
            }
            let mut now = 11;
            match condition {
                "unknown" => s.attempts[0].status = DurableAttemptStatus::Unknown,
                "stop" => s.owner_stopped = true,
                "attempts" => s.budget.attempts_used = 4,
                "transfer" => s.budget.transfer_used = 4096,
                "stored" => s.budget.stored_used = 4096,
                "expanded" => s.budget.expanded_used = 4096,
                _ => now = s.deadline_ms,
            }
            // An inconsistent input row is rejected; a consistent barrier stops.
            let action = decide_next(&s, &placements(&s), now);
            assert!(
                !matches!(
                    action,
                    Ok(SearchAction::OpenProposalRound { .. } | SearchAction::IssueAttempt { .. })
                ),
                "{condition} {generated}"
            );
        }
    }
}
#[test]
fn no_proposal_policy_keeps_wire_bytes_and_no_new_actions() {
    let raw = include_str!("fixtures/search-state/d1-failed.json");
    let s: SearchStateV1 = serde_json::from_str(raw).unwrap();
    assert_eq!(
        serde_json::to_value(&s).unwrap(),
        serde_json::from_str::<Value>(raw).unwrap()
    );
    assert!(!matches!(
        decide_next(&s, &[], 10).unwrap(),
        SearchAction::OpenProposalRound { .. }
    ));
}
#[test]
fn exact_runtime_is_not_relaxed_by_new_candidates() {
    let mut s = state();
    s.frozen.policy.runtime_constraint = RuntimeConstraint::Exact {
        runtime_id: "runtime".into(),
        environment_id: Some("native".into()),
    };
    admit(&mut s);
    let mut wrong = placements(&s);
    for p in &mut wrong {
        p.runtime_id = "other".into();
    }
    assert!(matches!(
        decide_next(&s, &wrong, 11).unwrap(),
        SearchAction::WaitForRuntime { .. }
    ));
    assert!(
        !allowed_choices(&s, &wrong)
            .iter()
            .any(|c| matches!(c.action, ChoiceAction::Attempt { .. }))
    );
}
#[test]
fn forged_or_duplicate_registry_and_uncompleted_candidates_are_rejected() {
    for condition in ["duplicate", "scope", "outcome", "limit", "no_policy"] {
        let mut s = state();
        admit(&mut s);
        let round = s.proposal_round.as_mut().unwrap();
        match condition {
            "duplicate" => round.candidates[1] = round.candidates[0].clone(),
            "scope" => round.candidates[0].effects = "external".into(),
            "outcome" => round.outcome = None,
            "limit" => round.candidates = vec![round.candidates[0].clone(); 5],
            _ => s.frozen.policy.proposal = None,
        }
        assert!(s.validate().is_err(), "{condition}");
    }
}
#[test]
fn candidate_and_finite_choice_never_establish_verification() {
    let mut s = state();
    admit(&mut s);
    s.frozen.policy.decision = Some(DecisionPolicy {
        provider: ProviderLocation::Requester,
        max_decisions: 4,
        decision_timeout_ms: 1000,
    });
    let action = decide_next(&s, &placements(&s), 11).unwrap();
    assert!(matches!(action, SearchAction::OpenDecision { .. }));
    assert!(!s.attempts.iter().any(|a| a.route_accepted));
}

#[test]
fn fixed_producer_returns_raw_bytes_then_validator_compiles_without_decision_authority() {
    let s = state();
    let authorization = s.frozen.policy.proposal.as_ref().unwrap();
    let request = ProposalRequest {
        schema: PROPOSAL_REQUEST_SCHEMA.into(),
        search_id: s.search_id.clone(),
        frozen_contract: s.frozen.base_contract.clone(),
        runtime_constraint: s.frozen.policy.runtime_constraint.clone(),
        known_derivations: s
            .frozen
            .candidates
            .iter()
            .map(|c| c.derivation_ref.clone())
            .collect(),
        failure_evidence: vec![],
        inspection_evidence: vec![],
        operation_catalog: authorization.catalog().unwrap(),
        remaining_budget: ProposalBudget {
            rounds_remaining: 1,
            max_proposals: 4,
            timeout_ms: 5000,
            attempts_remaining: 3,
        },
    };
    let wire = serde_json::to_value(&request).unwrap();
    assert!(wire.get("source_context").is_none());
    assert!(!wire.to_string().contains("working.py"));
    let mut injected = wire;
    injected["source_context"] = json!([{"source_id":"source","text":"secret-canary"}]);
    assert!(serde_json::from_value::<ProposalRequest>(injected).is_err());
    let before = request.clone();
    let provider = FixedCandidateProducer {
        output: batch(vec![script(), module()]),
    };
    let response = provider.propose(&request).unwrap();
    let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
    registry.validate_batch(&recipes(BASE), &response).unwrap();
    assert_eq!(registry.generated().len(), 2);
    assert_eq!(request, before);
    assert!(!s.attempts.iter().any(|a| a.route_accepted));
}

#[test]
fn finite_decision_can_select_second_generated_d_but_only_issues_an_attempt() {
    let mut s = state();
    admit(&mut s);
    s.frozen.policy.decision = Some(DecisionPolicy {
        provider: ProviderLocation::Requester,
        max_decisions: 4,
        decision_timeout_ms: 1000,
    });
    let placements = placements(&s);
    let SearchAction::OpenDecision {
        seq,
        default_id,
        choices,
    } = decide_next(&s, &placements, 11).unwrap()
    else {
        panic!("decision point")
    };
    let selected_ref = s.candidates().nth(2).unwrap().derivation_ref.clone();
    let chosen=choices.iter().find(|c|matches!(&c.action,ChoiceAction::Attempt{derivation_ref,..} if *derivation_ref==selected_ref)).unwrap().choice_id.clone();
    s.decisions.push(DecisionRecord {
        seq,
        attempt_seq: s.attempts.len() as u64,
        proposal_seq: 0,
        opened_at_ms: 11,
        default_id,
        choices,
        outcome: None,
        chosen_id: None,
    });
    s.budget.decisions_used += 1;
    let submission = serde_json::from_value(json!({"seq":seq,"choice_id":chosen})).unwrap();
    let verdict = validate_decision(&s, &submission, 12).unwrap();
    s.decisions[0].outcome = Some(verdict.outcome);
    s.decisions[0].chosen_id = verdict.chosen_id;
    assert!(
        matches!(decide_next(&s,&placements,12).unwrap(),SearchAction::IssueAttempt{derivation_ref,..} if derivation_ref==selected_ref)
    );
    assert!(!s.attempts.iter().any(|a| a.route_accepted));
    // A later UNKNOWN row, while the original failure remains, is a valid
    // durable state and blocks both next execution and producer progress.
    let mut unknown = s.attempts[0].clone();
    unknown.attempt_id = "unknown-generated".into();
    unknown.derivation_ref = selected_ref;
    unknown.status = DurableAttemptStatus::Unknown;
    unknown.record = Some(ExecutionRecord::StartedUnfinished);
    s.attempts.push(unknown);
    s.budget.attempts_used += 1;
    assert_eq!(
        decide_next(&s, &placements, 13).unwrap(),
        SearchAction::WaitForUnknownResolution {
            attempt_id: "unknown-generated".into()
        }
    );
}

// Base-free acceptance starts from I + explicit K, NOT a bound fixture recipe
// or an invented base D. This DTO fixture contains no known derivation.
fn empty_frontier() -> SearchStateV1 {
    let mut s: SearchStateV1 =
        serde_json::from_str(include_str!("fixtures/search-state/d1-failed.json")).unwrap();
    s.frozen.candidates.clear();
    s.attempts.clear();
    s.budget = BudgetCounters::default();
    s.frozen.initial_source = Some(InitialSource {
        closure_ref: format!("sha256:{}", "a".repeat(64)),
        archive_digest: format!("sha256:{}", "b".repeat(64)),
    });
    s.source_archive_bytes = Some(64);
    s.frozen.policy.proposal = Some(ProposalAuthorization {
        execution_plan: None,
        modifiable_derivation_refs: vec![],
        source_domain: SourceDomain {
            entrypoints: BTreeMap::from([
                ("entry_a".into(), "server.py".into()),
                ("entry_b".into(), "alternate.py".into()),
            ]),
            modules: BTreeMap::from([("module_a".into(), "pkg.server".into())]),
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
            timeout_ms: 5000,
            allow_source_text: false,
            max_source_bytes: 0,
        },
    });
    s
}
fn propose(id: &str) -> Value {
    json!({"kind":"propose_derivation","operations":[{"operation":"python_http_process@1","entrypoint_id":id}]})
}
fn first_candidates(s: &SearchStateV1) -> Vec<ValidatedCandidate> {
    let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
    let result = registry
        .validate_batch(
            &BTreeMap::new(),
            &batch(vec![propose("entry_a"), propose("entry_b")]),
        )
        .unwrap();
    assert!(
        result
            .iter()
            .all(|r| matches!(r, ProposalOutcome::Admitted(_))),
        "{result:?}"
    );
    registry.generated().to_vec()
}
#[test]
fn b0_no_policy_preserves_old_canonical_bytes_and_rejects_empty_frontier() {
    let raw = include_str!("fixtures/search-state/d1-failed.json");
    let value: Value = serde_json::from_str(raw).unwrap();
    let mut s: SearchStateV1 = serde_json::from_str(raw).unwrap();
    assert_eq!(
        s.frozen.canonical_bytes().unwrap(),
        serde_jcs::to_vec(&value["frozen"]).unwrap()
    );
    s.frozen.candidates.clear();
    s.attempts.clear();
    assert_eq!(s.frozen.canonical_bytes().unwrap_err().0, "candidate_count");
}
#[test]
fn b1_zero_known_d_with_complete_i_k_authorization_opens_proposal_round() {
    let s = empty_frontier();
    assert_eq!(s.frozen.candidates.len(), 0);
    assert_eq!(
        decide_next(&s, &[], 10).unwrap(),
        SearchAction::OpenProposalRound {
            opened_at_ms: 10,
            expires_at_ms: 5010
        }
    );
    let wire = serde_json::to_vec(&s).unwrap();
    assert_eq!(serde_json::from_slice::<SearchStateV1>(&wire).unwrap(), s);
    for invalid in ["source", "digest", "K", "authorization"] {
        let mut bad = s.clone();
        match invalid {
            "source" => bad.frozen.initial_source = None,
            "digest" => bad.frozen.initial_source.as_mut().unwrap().archive_digest = "bad".into(),
            "K" => bad.frozen.base_contract.requirements[0].status = Some(201),
            _ => bad.frozen.policy.proposal = None,
        }
        assert!(bad.validate().is_err(), "{invalid}");
    }
}
#[test]
fn b2_base_free_propose_constructs_first_d_from_i_without_a_recipe() {
    let s = empty_frontier();
    let candidates = first_candidates(&s);
    let d = &candidates[0].compiled().derivation;
    assert_eq!(
        d.steps[0].argv,
        [
            "/opt/ato/toolchains/python/3.12.7/bin/python3",
            "-B",
            "/app/server.py"
        ]
    );
    assert_eq!(d.steps[0].cwd, ".");
    assert!(d.steps[0].network.is_denied());
    assert_eq!(d.effects, ato_formation::authoring::EffectClass::Pure);
    assert_eq!(d.ports[0].id, "app.http");
    assert_eq!(d.ports[0].guest_port, Some(8000));
    assert_eq!(
        candidates[0].candidate().materialization,
        s.frozen.initial_source.as_ref().unwrap().materialization()
    );
    assert_eq!(
        candidates[0].candidate().requirements,
        execution_requirements(true, false)
    );
    assert_eq!(
        candidates[0].candidate().provisions,
        ["toolchain.python.3.12.7"]
    );
    assert!(s.frozen.candidates.is_empty());
}
#[test]
fn b3_generated_k_is_byte_identical_to_frozen_k() {
    let s = empty_frontier();
    for candidate in first_candidates(&s) {
        let (k, d) = bind(
            &parse_capsule_toml(&candidate.compiled().capsule_toml).unwrap(),
            &BindingContext {
                source_closure_ref: &s.frozen.initial_source.as_ref().unwrap().closure_ref,
            },
        )
        .unwrap();
        assert_eq!(
            serde_jcs::to_vec(&k).unwrap(),
            serde_jcs::to_vec(&s.frozen.base_contract).unwrap()
        );
        assert_eq!(k.contract_ref().unwrap(), s.frozen.contract_ref);
        assert_eq!(
            d.derivation_ref().unwrap(),
            candidate.compiled().derivation_ref
        );
    }
}
#[test]
fn b4_modify_requires_explicit_authorized_known_base() {
    let s = state();
    let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
    let results = registry
        .validate_batch(&recipes(BASE), &batch(vec![script(), module()]))
        .unwrap();
    assert!(
        results
            .iter()
            .all(|r| matches!(r, ProposalOutcome::Admitted(_)))
    );
    let mut missing = script();
    missing
        .as_object_mut()
        .unwrap()
        .remove("base_derivation_ref");
    assert!(matches!(
        registry
            .validate_batch(&recipes(BASE), &batch(vec![missing]))
            .unwrap()[0],
        ProposalOutcome::Rejected(_)
    ));
}
#[test]
fn b5_propose_rejects_injected_base_ref() {
    let s = empty_frontier();
    let mut injected = propose("entry_a");
    injected["base_derivation_ref"] = base_ref().into();
    let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
    assert!(matches!(
        registry
            .validate_batch(&BTreeMap::new(), &batch(vec![injected]))
            .unwrap()[0],
        ProposalOutcome::Rejected(_)
    ));
    assert!(registry.generated().is_empty());
}
#[test]
fn b6_modify_rejects_known_but_unauthorized_base() {
    let mut s = state();
    s.frozen
        .policy
        .proposal
        .as_mut()
        .unwrap()
        .modifiable_derivation_refs
        .clear();
    let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
    assert_eq!(
        registry
            .validate_batch(&recipes(BASE), &batch(vec![script()]))
            .unwrap(),
        vec![ProposalOutcome::Rejected(ProposalError(
            "proposal_base_unauthorized"
        ))]
    );
}
#[test]
fn b7_fixed_provider_raw_duplicate_and_unknown_fields_reach_ato_validator() {
    let s = empty_frontier();
    for raw in [
        r#"{"schema":"ato.formation-proposal/1","proposals":[{"kind":"propose_derivation","operations":[{"operation":"python_http_process@1","entrypoint_id":"bad","entrypoint_id":"entry_a"}]}]}"#,
        r#"{"schema":"ato.formation-proposal/1","proposals":[{"kind":"propose_derivation","K":{},"operations":[{"operation":"python_http_process@1","entrypoint_id":"entry_a"}]}]}"#,
    ] {
        let provider = FixedCandidateProducer {
            output: output(raw.as_bytes()),
        };
        let request = ProposalRequest {
            schema: PROPOSAL_REQUEST_SCHEMA.into(),
            search_id: s.search_id.clone(),
            frozen_contract: s.frozen.base_contract.clone(),
            runtime_constraint: s.frozen.policy.runtime_constraint.clone(),
            known_derivations: vec![],
            failure_evidence: vec![],
            inspection_evidence: vec![],
            operation_catalog: s
                .frozen
                .policy
                .proposal
                .as_ref()
                .unwrap()
                .catalog()
                .unwrap(),
            remaining_budget: ProposalBudget {
                rounds_remaining: 1,
                max_proposals: 4,
                timeout_ms: 5000,
                attempts_remaining: 4,
            },
        };
        let response = provider.propose(&request).unwrap();
        let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
        assert!(matches!(
            registry
                .validate_batch(&BTreeMap::new(), &response)
                .unwrap()[0],
            ProposalOutcome::Rejected(_)
        ));
        assert!(registry.generated().is_empty());
        assert!(
            serde_json::to_value(request)
                .unwrap()
                .get("source_context")
                .is_none()
        );
    }
}
#[test]
fn b8_same_proposal_content_produces_same_search_scoped_id_and_d() {
    let s = empty_frontier();
    let a = first_candidates(&s);
    let b = first_candidates(&s);
    assert_eq!(a, b);
    assert!(a[0].proposal_id().starts_with("proposal-content:sha256:"));
    // Base recipes (even invalid ones) cannot influence ProposeDerivation.
    let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
    registry
        .validate_batch(&recipes("not a capsule"), &batch(vec![propose("entry_a")]))
        .unwrap();
    assert_eq!(registry.generated()[0], a[0]);
}
#[test]
fn b9_different_base_modifications_have_distinct_content_ids() {
    let mut s = state();
    let other_recipe = BASE.replace("guest_port = 8000", "guest_port = 8001");
    let (_, other) = bind(
        &parse_capsule_toml(&other_recipe).unwrap(),
        &BindingContext {
            source_closure_ref: &s.frozen.initial_source.as_ref().unwrap().closure_ref,
        },
    )
    .unwrap();
    let other_ref = other.derivation_ref().unwrap();
    let mut known = s.frozen.candidates[0].clone();
    known.derivation_ref = other_ref.clone();
    s.frozen.candidates.push(known);
    let auth = s.frozen.policy.proposal.as_mut().unwrap();
    auth.modifiable_derivation_refs.push(other_ref.clone());
    auth.modifiable_derivation_refs.sort();
    let mut bases = recipes(BASE);
    bases.insert(other_ref.clone(), other_recipe);
    let mut second = script();
    second["base_derivation_ref"] = other_ref.into();
    let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
    registry
        .validate_batch(&bases, &batch(vec![script(), second]))
        .unwrap();
    assert_eq!(registry.generated().len(), 2);
    assert_ne!(
        registry.generated()[0].proposal_id(),
        registry.generated()[1].proposal_id()
    );
    assert_ne!(
        registry.generated()[0].compiled().derivation_ref,
        registry.generated()[1].compiled().derivation_ref
    );
}
#[test]
fn b10_zero_known_d_unsupported_terminates_once_without_k_evidence() {
    let mut s = empty_frontier();
    let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
    assert_eq!(
        registry
            .validate_batch(
                &BTreeMap::new(),
                &batch(vec![json!({"kind":"unsupported"})])
            )
            .unwrap(),
        vec![ProposalOutcome::Unsupported]
    );
    s.proposal_round = Some(ProposalRoundRecord {
        opened_at_ms: 10,
        expires_at_ms: 5010,
        outcome: Some(ProposalRoundOutcome::Completed),
        candidates: vec![],
        derivations: vec![],
        diagnostics: vec![],
        inspection_requests: vec![],
    });
    assert_eq!(
        decide_next(&s, &[], 11).unwrap(),
        SearchAction::Finish {
            reason: Termination::CandidatesExhausted
        }
    );
    assert!(s.attempts.is_empty());
    assert_eq!(s.budget.attempts_used, 0);
}
#[test]
fn b11_generated_first_d_uses_same_deterministic_and_decision_frontier() {
    let mut s = empty_frontier();
    let candidates = first_candidates(&s);
    s.proposal_round = Some(ProposalRoundRecord {
        opened_at_ms: 10,
        expires_at_ms: 5010,
        outcome: Some(ProposalRoundOutcome::Completed),
        candidates: candidates.iter().map(|c| c.candidate().clone()).collect(),
        derivations: vec![],
        diagnostics: vec![],
        inspection_requests: vec![],
    });
    assert_eq!(s.candidates().count(), 2);
    assert!(
        matches!(decide_next(&s, &placements(&s), 11).unwrap(), SearchAction::IssueAttempt { derivation_ref, .. } if derivation_ref == candidates[0].compiled().derivation_ref)
    );
    s.frozen.policy.decision = Some(DecisionPolicy {
        provider: ProviderLocation::Requester,
        max_decisions: 4,
        decision_timeout_ms: 1000,
    });
    let SearchAction::OpenDecision { choices, .. } = decide_next(&s, &placements(&s), 11).unwrap()
    else {
        panic!("expected finite decision");
    };
    for c in &candidates {
        assert!(choices.iter().any(|choice| matches!(&choice.action, ChoiceAction::Attempt { derivation_ref, .. } if derivation_ref == &c.compiled().derivation_ref)));
    }
    assert!(s.frozen.candidates.is_empty());
}
#[test]
fn b12_compiler_output_does_not_establish_pass_or_modify_attempt_evidence() {
    let s = empty_frontier();
    let before = s.clone();
    assert_eq!(first_candidates(&s).len(), 2);
    assert_eq!(s, before);
    assert!(s.attempts.is_empty());
    assert!(!matches!(
        decide_next(&s, &[], 10).unwrap(),
        SearchAction::Finish {
            reason: Termination::Verified
        }
    ));
}
#[test]
fn zero_known_d_catalog_never_offers_modify_only_operations() {
    let mut s = empty_frontier();
    let auth = s.frozen.policy.proposal.as_mut().unwrap();
    assert!(matches!(
        auth.catalog().unwrap().operations.as_slice(),
        [OperationDomain::PythonHttpProcess { .. }]
    ));
    auth.python_http_process = None;
    assert!(auth.catalog().unwrap().operations.is_empty());
    let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
    assert!(matches!(
        registry
            .validate_batch(&BTreeMap::new(), &batch(vec![propose("entry_a")]))
            .unwrap()[0],
        ProposalOutcome::Rejected(_)
    ));
}
#[test]
fn known_frontier_is_explored_before_producer_and_inflight_blocks() {
    let mut s = state();
    s.attempts.clear();
    s.budget = BudgetCounters::default();
    assert!(matches!(
        decide_next(&s, &placements(&s), 10).unwrap(),
        SearchAction::IssueAttempt { .. }
    ));
    let mut running = state();
    running.attempts[0].status = DurableAttemptStatus::Claimed;
    running.attempts[0].record = Some(ExecutionRecord::StartedUnfinished);
    assert!(!matches!(
        decide_next(&running, &placements(&running), 10),
        Ok(SearchAction::OpenProposalRound { .. } | SearchAction::IssueAttempt { .. })
    ));
}
#[test]
fn base_free_constructor_rejects_incompatible_k_and_provider_controlled_parameters() {
    for field in [
        "python_version",
        "argv",
        "cwd",
        "port",
        "network",
        "path",
        "executable",
    ] {
        let s = empty_frontier();
        let mut bad = propose("entry_a");
        bad["operations"][0][field] = "injected".into();
        let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
        assert!(matches!(
            registry
                .validate_batch(&BTreeMap::new(), &batch(vec![bad]))
                .unwrap()[0],
            ProposalOutcome::Rejected(_)
        ));
    }
    for field in ["port", "method", "verifier", "digest"] {
        let mut s = empty_frontier();
        let r = &mut s.frozen.base_contract.requirements[0];
        match field {
            "port" => r.port = Some("other".into()),
            "method" => r.method = Some("POST".into()),
            "verifier" => r.verifier = "unknown".into(),
            _ => r.digest = Some("unrelated".into()),
        }
        s.frozen.base_contract_ref = s.frozen.base_contract.contract_ref().unwrap();
        s.frozen.contract_ref = s.frozen.base_contract_ref.clone();
        assert!(CandidateRegistry::new(&s.frozen).is_err(), "{field}");
    }
}

#[test]
fn raw_envelope_duplicate_fields_are_rejected_before_member_admission() {
    let s = empty_frontier();
    for raw in [
        r#"{"schema":"ato.formation-proposal/1","schema":"ato.formation-proposal/1","proposals":[]}"#,
        r#"{"schema":"ato.formation-proposal/1","proposals":[],"proposals":[]}"#,
    ] {
        let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
        assert!(
            registry
                .validate_batch(&BTreeMap::new(), &output(raw.as_bytes()))
                .is_err()
        );
        assert!(registry.generated().is_empty());
    }
}
#[test]
fn zero_known_d_respects_stop_deadline_budgets_and_single_round_error_outcomes() {
    for barrier in [
        "stop",
        "deadline",
        "attempt_budget",
        "transfer_budget",
        "expanded_budget",
        "stored_budget",
    ] {
        let mut s = empty_frontier();
        let mut now = 10;
        match barrier {
            "stop" => s.owner_stopped = true,
            "deadline" => now = s.deadline_ms,
            "attempt_budget" => s.frozen.policy.budget.max_attempts = 0,
            "transfer_budget" => s.frozen.policy.budget.max_transfer_bytes = 0,
            "expanded_budget" => s.frozen.policy.budget.max_expanded_bytes = 0,
            _ => s.frozen.policy.budget.max_stored_bytes = 0,
        }
        assert!(
            matches!(
                decide_next(&s, &[], now).unwrap(),
                SearchAction::Finish { .. }
            ),
            "{barrier}"
        );
    }
    for outcome in [
        ProposalRoundOutcome::Timeout,
        ProposalRoundOutcome::ProviderError,
    ] {
        let mut s = empty_frontier();
        s.proposal_round = Some(ProposalRoundRecord {
            opened_at_ms: 10,
            expires_at_ms: 5010,
            outcome: Some(outcome),
            candidates: vec![],
            derivations: vec![],
            diagnostics: vec![],
            inspection_requests: vec![],
        });
        assert_eq!(
            decide_next(&s, &[], 11).unwrap(),
            SearchAction::Finish {
                reason: Termination::CandidatesExhausted
            }
        );
        assert!(s.attempts.is_empty());
    }
}

fn source_request(auth: &ProposalAuthorization) -> ProposalRequest {
    let s = empty_frontier();
    ProposalRequest {
        schema: PROPOSAL_REQUEST_SCHEMA.into(),
        search_id: "source-search".into(),
        frozen_contract: s.frozen.base_contract,
        runtime_constraint: s.frozen.policy.runtime_constraint,
        known_derivations: vec![],
        failure_evidence: vec![],
        inspection_evidence: vec![],
        operation_catalog: auth.catalog().unwrap(),
        remaining_budget: ProposalBudget {
            rounds_remaining: 1,
            max_proposals: 4,
            timeout_ms: 5000,
            attempts_remaining: 4,
        },
    }
}
fn source_auth(bytes: usize) -> ProposalAuthorization {
    let mut auth = empty_frontier().frozen.policy.proposal.unwrap();
    auth.policy.allow_source_text = true;
    auth.policy.max_source_bytes = bytes;
    auth
}
#[test]
fn d2a_policy_matrix_and_old_frozen_bytes() {
    let s = empty_frontier();
    let old = s.frozen.canonical_bytes().unwrap();
    let mut policy = s.frozen.policy.proposal.as_ref().unwrap().policy.clone();
    for (allow, bytes, valid) in [
        (false, 0, true),
        (true, 1, true),
        (true, 16384, true),
        (true, 65536, true),
        (true, 0, false),
        (false, 1, false),
        (true, 65537, false),
        (false, 65536, false),
    ] {
        policy.allow_source_text = allow;
        policy.max_source_bytes = bytes;
        assert_eq!(policy.validate().is_ok(), valid);
    }
    assert_eq!(s.frozen.canonical_bytes().unwrap(), old);
    assert_eq!(
        serde_json::from_slice::<FrozenSearchV1>(&old)
            .unwrap()
            .canonical_bytes()
            .unwrap(),
        old
    );
}
#[test]
fn d2a_source_enabled_policy_does_not_change_compiled_d_or_k() {
    let mut s = empty_frontier();
    let before = first_candidates(&s);
    let k = s.frozen.contract_ref.clone();
    s.frozen.policy.proposal.as_mut().unwrap().policy = source_auth(16384).policy;
    assert_eq!(first_candidates(&s), before);
    assert_eq!(s.frozen.contract_ref, k);
    assert!(matches!(
        decide_next(&s, &[], 10).unwrap(),
        SearchAction::OpenProposalRound { .. }
    ));
    assert!(s.attempts.is_empty());
}
#[test]
fn d2a_v2_is_a_strict_superset_not_a_v1_reinterpretation() {
    let auth = source_auth(16384);
    let v1 = source_request(&auth);
    let old = serde_json::to_value(&v1).unwrap();
    assert!(old.get("source_context").is_none());
    let v2 = ProposalRequestV2::new(v1.clone(), &auth, &[]).unwrap();
    let mut wire = serde_json::to_value(&v2).unwrap();
    assert!(serde_json::from_value::<ProposalRequest>(wire.clone()).is_err());
    assert!(serde_json::from_value::<ProposalRequestV2>(old.clone()).is_err());
    wire.as_object_mut().unwrap().remove("source_context");
    wire["schema"] = json!(PROPOSAL_REQUEST_SCHEMA);
    assert_eq!(wire, old);
    let bytes = v2.canonical_bytes(&auth).unwrap();
    assert_eq!(
        serde_json::from_slice::<ProposalRequestV2>(&bytes).unwrap(),
        v2
    );
    let duplicate = String::from_utf8(bytes).unwrap().replacen(
        "{",
        "{\"schema\":\"ato.formation-proposal-request/2\",",
        1,
    );
    assert!(serde_json::from_str::<ProposalRequestV2>(&duplicate).is_err());
    let mut unknown = serde_json::to_value(&v2).unwrap();
    unknown["source_domain"] = json!({});
    assert!(serde_json::from_value::<ProposalRequestV2>(unknown).is_err());
}
#[test]
fn d2a_context_is_order_independent_opaque_and_utf8_bounded() {
    let auth = source_auth(19);
    let a = "あいうえおかき";
    let b = "abcdefghijklmno";
    let sources = [
        AuthorizedSourceText {
            kind: SourceContextKind::Entrypoint,
            logical_id: "entry_b",
            bytes: b.as_bytes(),
        },
        AuthorizedSourceText {
            kind: SourceContextKind::Entrypoint,
            logical_id: "entry_a",
            bytes: a.as_bytes(),
        },
    ];
    let first = ProposalRequestV2::new(source_request(&auth), &auth, &sources).unwrap();
    let reversed = [sources[1].bytes, sources[0].bytes];
    let second = ProposalRequestV2::new(
        source_request(&auth),
        &auth,
        &[
            AuthorizedSourceText {
                kind: SourceContextKind::Entrypoint,
                logical_id: "entry_a",
                bytes: reversed[0],
            },
            AuthorizedSourceText {
                kind: SourceContextKind::Entrypoint,
                logical_id: "entry_b",
                bytes: reversed[1],
            },
        ],
    )
    .unwrap();
    assert_eq!(
        first.canonical_bytes(&auth).unwrap(),
        second.canonical_bytes(&auth).unwrap()
    );
    assert_eq!(
        first.source_context_sha256(&auth).unwrap(),
        second.source_context_sha256(&auth).unwrap()
    );
    assert_eq!(first.source_context[0].text, "あいう");
    assert!(
        first
            .source_context
            .iter()
            .all(|s| s.truncated && s.source_id.len() == 66)
    );
    assert!(
        first
            .source_context
            .iter()
            .map(|s| s.text.len())
            .sum::<usize>()
            <= 19
    );
    let json = String::from_utf8(first.canonical_bytes(&auth).unwrap()).unwrap();
    for private in ["server.py", "alternate.py", "pkg.server", "source_domain"] {
        assert!(!json.contains(private));
    }
}
#[test]
fn d2a_invalid_utf8_and_binary_are_omitted_without_conversion() {
    let auth = source_auth(100);
    for bytes in [&b"a\xffb"[..], &b"a\0b"[..], &b"a\x01b"[..]] {
        let result = build_source_context(
            &auth,
            &[AuthorizedSourceText {
                kind: SourceContextKind::Entrypoint,
                logical_id: "entry_a",
                bytes,
            }],
        )
        .unwrap();
        assert!(result.is_empty());
    }
    let module = build_source_context(
        &auth,
        &[AuthorizedSourceText {
            kind: SourceContextKind::Module,
            logical_id: "module_a",
            bytes: b"print(1)\n",
        }],
    )
    .unwrap();
    assert_eq!(module[0].text, "print(1)\n");
}
#[test]
fn d2a_per_entry_cap_and_authorization_are_enforced() {
    let auth = source_auth(65536);
    let full = vec![b'a'; 65537];
    let result = build_source_context(
        &auth,
        &[AuthorizedSourceText {
            kind: SourceContextKind::Entrypoint,
            logical_id: "entry_a",
            bytes: &full,
        }],
    )
    .unwrap();
    assert_eq!(result[0].text.len(), MAX_SOURCE_ENTRY_BYTES);
    assert!(result[0].truncated);
    assert!(
        build_source_context(
            &auth,
            &[AuthorizedSourceText {
                kind: SourceContextKind::Entrypoint,
                logical_id: "../secret",
                bytes: b"x"
            }]
        )
        .is_err()
    );
    assert!(
        build_source_context(
            &auth,
            &[AuthorizedSourceText {
                kind: SourceContextKind::Module,
                logical_id: "entry_a",
                bytes: b"x"
            }]
        )
        .is_err()
    );
    assert!(
        build_source_context(
            &auth,
            &[
                AuthorizedSourceText {
                    kind: SourceContextKind::Entrypoint,
                    logical_id: "entry_a",
                    bytes: b"x"
                },
                AuthorizedSourceText {
                    kind: SourceContextKind::Entrypoint,
                    logical_id: "entry_a",
                    bytes: b"x"
                }
            ]
        )
        .is_err()
    );
    let disabled = empty_frontier().frozen.policy.proposal.unwrap();
    assert!(build_source_context(&disabled, &[]).is_err());
}
#[test]
fn d2a_mutated_hash_budget_or_context_fails_closed() {
    let auth = source_auth(30);
    let good = ProposalRequestV2::new(
        source_request(&auth),
        &auth,
        &[AuthorizedSourceText {
            kind: SourceContextKind::Entrypoint,
            logical_id: "entry_a",
            bytes: b"print(1)",
        }],
    )
    .unwrap();
    let mut bad = good.clone();
    bad.source_context[0].text.push('!');
    assert!(bad.validate(&auth).is_err());
    let mut bad = good.clone();
    bad.source_context[0].source_id = "s_/private/path".into();
    assert!(bad.validate(&auth).is_err());
    let mut bad = good.clone();
    bad.remaining_budget.max_proposals = 5;
    assert!(bad.validate(&auth).is_err());
    let mut bad = good.clone();
    bad.source_context.push(bad.source_context[0].clone());
    assert!(bad.validate(&auth).is_err());
    assert!(good.validate(&source_auth(1)).is_err());
    for (field, value) in [
        ("path", json!("secret.py")),
        ("encoding", json!("base64")),
        ("kind", json!("readme")),
    ] {
        let mut wire = serde_json::to_value(&good).unwrap();
        wire["source_context"][0][field] = value;
        assert!(serde_json::from_value::<ProposalRequestV2>(wire).is_err());
    }
}

fn adaptive(mut s: SearchStateV1) -> SearchStateV1 {
    s.frozen.policy.decision = Some(DecisionPolicy {
        provider: ProviderLocation::Requester,
        max_decisions: 8,
        decision_timeout_ms: 1000,
    });
    s
}
fn open_adaptive_point(s: &mut SearchStateV1) {
    let SearchAction::OpenDecision {
        seq,
        default_id,
        choices,
    } = decide_next(s, &placements(s), 10).unwrap()
    else {
        panic!("expected decision")
    };
    assert_eq!(
        choices[0].action,
        ChoiceAction::EscalateToCandidateProducer {}
    );
    assert_eq!(default_id, choices[0].choice_id);
    assert!(matches!(
        choices.last().unwrap().action,
        ChoiceAction::Stop { .. }
    ));
    assert!(
        choices[1..choices.len() - 1]
            .iter()
            .all(|c| matches!(c.action, ChoiceAction::Inspect { .. }))
    );
    s.decisions.push(DecisionRecord {
        seq,
        attempt_seq: s.attempts.len() as u64,
        proposal_seq: 0,
        opened_at_ms: 10,
        default_id,
        choices,
        outcome: None,
        chosen_id: None,
    });
    s.budget.decisions_used += 1;
}
fn settle_adaptive_point(s: &mut SearchStateV1, outcome: DecisionOutcome) {
    let r = s.decisions.last_mut().unwrap();
    r.outcome = Some(outcome);
    r.chosen_id = (outcome == DecisionOutcome::Chosen).then(|| r.default_id.clone());
}
#[test]
fn adaptive_escalation_has_no_arguments_and_stable_identity() {
    let action = ChoiceAction::EscalateToCandidateProducer {};
    let wire = json!({"kind":"escalate_to_candidate_producer"});
    assert_eq!(serde_json::to_value(&action).unwrap(), wire);
    assert_eq!(
        serde_json::from_value::<ChoiceAction>(wire.clone()).unwrap(),
        action
    );
    assert_eq!(choice_id(0, &action), choice_id(0, &action));
    assert_ne!(choice_id(0, &action), choice_id(1, &action));
    for field in [
        "prompt",
        "path",
        "operation",
        "runtime",
        "K",
        "source_id",
        "args",
    ] {
        let mut bad = wire.clone();
        bad[field] = json!("injected");
        assert!(
            serde_json::from_value::<ChoiceAction>(bad).is_err(),
            "{field}"
        );
    }
}
#[test]
fn adaptive_zero_and_failed_frontiers_offer_escalation_only_after_core_admission() {
    for mut s in [adaptive(empty_frontier()), adaptive(state())] {
        open_adaptive_point(&mut s);
        assert!(matches!(
            decide_next(&s, &placements(&s), 11).unwrap(),
            SearchAction::WaitForDecision { .. }
        ));
        settle_adaptive_point(&mut s, DecisionOutcome::Chosen);
        assert!(matches!(
            decide_next(&s, &placements(&s), 12).unwrap(),
            SearchAction::OpenProposalRound { .. }
        ));
        let restored: SearchStateV1 =
            serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
        assert_eq!(
            decide_next(&s, &placements(&s), 12).unwrap(),
            decide_next(&restored, &placements(&restored), 12).unwrap()
        );
    }
}
#[test]
fn adaptive_fallback_releases_proposal_then_next_generated_decision() {
    for outcome in [
        DecisionOutcome::Chosen,
        DecisionOutcome::Timeout,
        DecisionOutcome::ProviderError,
        DecisionOutcome::Invalid,
        DecisionOutcome::OutOfSet,
    ] {
        let mut s = adaptive(empty_frontier());
        open_adaptive_point(&mut s);
        settle_adaptive_point(&mut s, outcome);
        assert!(matches!(
            decide_next(&s, &[], 12).unwrap(),
            SearchAction::OpenProposalRound { .. }
        ));
        let candidates = first_candidates(&s);
        s.proposal_round = Some(ProposalRoundRecord {
            opened_at_ms: 12,
            expires_at_ms: 5012,
            outcome: Some(ProposalRoundOutcome::Completed),
            candidates: candidates.iter().map(|c| c.candidate().clone()).collect(),
            derivations: vec![],
            diagnostics: vec![],
            inspection_requests: vec![],
        });
        let SearchAction::OpenDecision { seq, choices, .. } =
            decide_next(&s, &placements(&s), 13).unwrap()
        else {
            panic!("generated decision missing after {outcome:?}")
        };
        assert_eq!(seq, 1);
        assert!(matches!(choices[0].action, ChoiceAction::Attempt { .. }));
        assert!(
            !choices
                .iter()
                .any(|c| matches!(c.action, ChoiceAction::EscalateToCandidateProducer {}))
        );
        assert!(s.attempts.is_empty()); // Compilation/decision never establish PASS.
    }
}
#[test]
fn adaptive_open_decision_timeout_uses_existing_fallback_record() {
    let mut s = adaptive(empty_frontier());
    open_adaptive_point(&mut s);
    assert_eq!(
        decide_next(&s, &[], 1010).unwrap(),
        SearchAction::RecordFallback {
            seq: 0,
            reason: DecisionOutcome::Timeout
        }
    );
}
#[test]
fn adaptive_inspect_and_stop_remain_effective_at_proposal_frontier() {
    for stop in [false, true] {
        let mut s = adaptive(state());
        open_adaptive_point(&mut s);
        let record = &mut s.decisions[0];
        let selected = record
            .choices
            .iter()
            .find(|c| {
                if stop {
                    matches!(c.action, ChoiceAction::Stop { .. })
                } else {
                    matches!(c.action, ChoiceAction::Inspect { .. })
                }
            })
            .unwrap();
        record.chosen_id = Some(selected.choice_id.clone());
        record.outcome = Some(DecisionOutcome::Chosen);
        let action = decide_next(&s, &placements(&s), 12).unwrap();
        if stop {
            assert_eq!(
                action,
                SearchAction::Finish {
                    reason: Termination::DecisionStopped
                }
            );
        } else {
            assert!(matches!(action, SearchAction::RunInspection { .. }));
        }
    }
}
#[test]
fn adaptive_proposal_fences_are_the_deterministic_fences() {
    for fence in [
        "owner",
        "deadline",
        "attempt_budget",
        "transfer",
        "stored",
        "unknown_source",
        "unknown",
        "inflight",
        "effect",
    ] {
        let mut s = state();
        match fence {
            "owner" => s.owner_stopped = true,
            "deadline" => s.deadline_ms = 10,
            "attempt_budget" => s.budget.attempts_used = s.frozen.policy.budget.max_attempts,
            "transfer" => s.budget.transfer_used = s.frozen.policy.budget.max_transfer_bytes,
            "stored" => s.budget.stored_used = s.frozen.policy.budget.max_stored_bytes,
            "unknown_source" => s.source_archive_bytes = None,
            "unknown" => {
                s.attempts[0].status = DurableAttemptStatus::Unknown;
                s.attempts[0].record = Some(ExecutionRecord::StartedUnfinished);
            }
            "inflight" => {
                s.attempts[0].status = DurableAttemptStatus::Claimed;
                s.attempts[0].record = None;
            }
            "effect" => s.attempts[0].record = Some(ExecutionRecord::HistoryUnavailable),
            _ => unreachable!(),
        }
        let expected = decide_next(&s, &placements(&s), 10).unwrap();
        assert!(
            !matches!(expected, SearchAction::OpenProposalRound { .. }),
            "{fence}"
        );
        let s = adaptive(s);
        assert_eq!(
            decide_next(&s, &placements(&s), 10).unwrap(),
            expected,
            "{fence}"
        );
    }
}
