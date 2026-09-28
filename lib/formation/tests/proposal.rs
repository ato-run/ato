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
        archive_digest: closure,
    };
    s.attempts[0].derivation_ref = base.derivation_ref.clone();
    s.source_archive_bytes = Some(64);
    s.frozen.policy.proposal = Some(ProposalAuthorization {
        base_derivation_ref: base.derivation_ref.clone(),
        entrypoints: BTreeMap::from([("entry_a".into(), "working.py".into())]),
        modules: BTreeMap::from([("module_a".into(), "pkg.server".into())]),
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
    json!({"kind":"propose_derivation","operations":[{"operation":"python_script@1","entrypoint_id":"entry_a"}]})
}
fn module() -> Value {
    json!({"kind":"propose_derivation","operations":[{"operation":"python_module@1","module_id":"module_a"}]})
}
fn batch(proposals: Vec<Value>) -> Vec<u8> {
    serde_json::to_vec(&json!({"schema":PROPOSAL_SCHEMA,"proposals":proposals})).unwrap()
}
fn admit(s: &mut SearchStateV1) {
    let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
    registry
        .validate_batch(
            s.frozen.policy.proposal.as_ref().unwrap(),
            BASE,
            &batch(vec![script(), module()]),
        )
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
        .validate_batch(
            s.frozen.policy.proposal.as_ref().unwrap(),
            BASE,
            &batch(vec![script(), module()]),
        )
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
            s.frozen.policy.proposal.as_ref().unwrap(),
            BASE,
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
                .validate_batch(
                    s.frozen.policy.proposal.as_ref().unwrap(),
                    BASE,
                    &batch(vec![bad, module()]),
                )
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
        .validate_batch(s.frozen.policy.proposal.as_ref().unwrap(), BASE, raw)
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
                s.frozen.policy.proposal.as_ref().unwrap(),
                BASE,
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
        vec![b' '; MAX_BATCH_BYTES + 1],
        batch(vec![script(); 5]),
        b"{\"schema\":\"wrong\",\"proposals\":[]}".to_vec(),
        b"{\"schema\":\"ato.formation-proposal/1\",\"proposals\":[],\"unknown\":true}".to_vec(),
    ] {
        let s = state();
        let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
        assert!(
            registry
                .validate_batch(s.frozen.policy.proposal.as_ref().unwrap(), BASE, &bytes)
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
            .validate_batch(
                s.frozen.policy.proposal.as_ref().unwrap(),
                BASE,
                &batch(vec![json!({"kind":"unsupported"})])
            )
            .unwrap(),
        vec![ProposalOutcome::Unsupported]
    );
    assert!(
        registry
            .validate_batch(
                s.frozen.policy.proposal.as_ref().unwrap(),
                BASE,
                &batch(vec![])
            )
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
                .validate_batch(
                    s.frozen.policy.proposal.as_ref().unwrap(),
                    BASE,
                    &batch(vec![proposal])
                )
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
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    let mut a = CandidateRegistry::new(&s.frozen).unwrap();
    let mut b = CandidateRegistry::new(&s.frozen).unwrap();
    let auth = s.frozen.policy.proposal.as_ref().unwrap();
    a.validate_batch(auth, BASE, &bytes).unwrap();
    b.validate_batch(auth, BASE, &serde_json::to_vec_pretty(&value).unwrap())
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
        bad.entrypoints.insert("entry_a".into(), path.into());
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
        bad.modules.insert("module_a".into(), module.into());
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
    bad = auth.clone();
    bad.entrypoints.insert("entry_a".into(), "other.py".into());
    assert!(
        CandidateRegistry::new(&s.frozen)
            .unwrap()
            .validate_batch(&bad, BASE, &batch(vec![script()]))
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
            .validate_batch(
                s.frozen.policy.proposal.as_ref().unwrap(),
                &text,
                &batch(vec![script()]),
            )
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
fn fixed_producer_returns_typed_batch_then_validator_compiles_without_decision_authority() {
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
        source_context: vec![],
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
    let before = request.clone();
    let provider = FixedCandidateProducer {
        batch: serde_json::from_slice(&batch(vec![script(), module()])).unwrap(),
    };
    let response = provider.propose(&request).unwrap();
    let mut registry = CandidateRegistry::new(&s.frozen).unwrap();
    registry
        .validate_batch(authorization, BASE, &serde_json::to_vec(&response).unwrap())
        .unwrap();
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
