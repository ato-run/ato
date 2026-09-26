use ato_formation::{decision::*, generation::*, search::*};
use std::collections::BTreeMap;

fn state() -> SearchStateV1 {
    let mut s: SearchStateV1 =
        serde_json::from_str(include_str!("fixtures/search-state/d1-failed.json")).unwrap();
    s.frozen.candidates.truncate(1);
    s.source_archive_bytes = Some(64);
    s.frozen.policy.generation = Some(GenerationPolicy {
        schema: GENERATION_POLICY_SCHEMA.into(),
        base_derivation_ref: s.frozen.candidates[0].derivation_ref.clone(),
        entrypoints: BTreeMap::from([("entry_a".into(), "working.py".into())]),
        max_generations: 1,
        timeout_ms: 5_000,
    });
    s
}
fn open(s: &mut SearchStateV1) {
    let SearchAction::OpenGeneration {
        opened_at_ms,
        expires_at_ms,
    } = decide_next(s, &[], 10).unwrap()
    else {
        panic!("must open")
    };
    s.generation = Some(GenerationRecord {
        opened_at_ms,
        expires_at_ms,
        outcome: None,
        candidate: None,
    });
}
fn admit(s: &mut SearchStateV1) -> String {
    open(s);
    let mut candidate = s.frozen.candidates[0].clone();
    candidate.derivation_ref = format!("sha256:{}", "f".repeat(64));
    let reference = candidate.derivation_ref.clone();
    let g = s.generation.as_mut().unwrap();
    g.outcome = Some(GenerationOutcome::Admitted);
    g.candidate = Some(candidate);
    reference
}
fn placement(reference: &str) -> Placement {
    Placement {
        candidate_id: "generated".into(),
        derivation_ref: reference.into(),
        runtime_id: "runtime".into(),
        environment_id: "native".into(),
        admissible: true,
        transfer_bytes: 64,
    }
}

#[test]
fn generation_opens_only_after_known_candidates_are_exhausted() {
    let mut s = state();
    assert_eq!(
        decide_next(&s, &[], 10).unwrap(),
        SearchAction::OpenGeneration {
            opened_at_ms: 10,
            expires_at_ms: 5010
        }
    );
    s.attempts.clear();
    s.budget.attempts_used = 0;
    assert!(matches!(
        decide_next(&s, &[], 10).unwrap(),
        SearchAction::WaitForRuntime { .. }
    ));
    let p = placement(&s.frozen.candidates[0].derivation_ref);
    assert!(matches!(
        decide_next(&s, &[p], 10).unwrap(),
        SearchAction::IssueAttempt { .. }
    ));
}

#[test]
fn generation_does_not_bypass_unknown_running_stop_or_pending_acceptance() {
    for status in [
        DurableAttemptStatus::Unknown,
        DurableAttemptStatus::Claimed,
        DurableAttemptStatus::Pending,
        DurableAttemptStatus::Pass,
        DurableAttemptStatus::Inconclusive,
        DurableAttemptStatus::Expired,
    ] {
        let mut s = state();
        s.attempts[0].status = status;
        assert!(!matches!(
            decide_next(&s, &[], 10).unwrap(),
            SearchAction::OpenGeneration { .. }
        ));
    }
    let mut s = state();
    s.owner_stopped = true;
    assert_eq!(
        decide_next(&s, &[], 10).unwrap(),
        SearchAction::Finish {
            reason: Termination::OwnerStopped
        }
    );
    for record in [
        ExecutionRecord::NotStarted,
        ExecutionRecord::StartedUnfinished,
        ExecutionRecord::HistoryUnavailable,
        ExecutionRecord::BlockedByUnknown,
    ] {
        let mut s = state();
        s.attempts[0].record = Some(record);
        assert!(!matches!(
            decide_next(&s, &[], 10).unwrap(),
            SearchAction::OpenGeneration { .. }
        ));
    }
}

#[test]
fn generation_spends_no_new_execution_permission_or_budget() {
    for counter in [
        "attempts_used",
        "expanded_used",
        "stored_used",
        "transfer_used",
    ] {
        let mut value = serde_json::to_value(state()).unwrap();
        value["budget"][counter] = 999999.into();
        let s = serde_json::from_value(value).unwrap();
        assert_eq!(
            decide_next(&s, &[], 10).unwrap(),
            SearchAction::Finish {
                reason: Termination::BudgetExhausted
            }
        );
    }
    let mut s = state();
    s.frozen.policy.runtime_constraint = RuntimeConstraint::Exact {
        runtime_id: "runtime".into(),
        environment_id: None,
    };
    assert_eq!(
        decide_next(&s, &[], 10).unwrap(),
        SearchAction::Finish {
            reason: Termination::CandidatesExhausted
        }
    );
    assert_eq!(
        decide_next(&state(), &[], 60000).unwrap(),
        SearchAction::Finish {
            reason: Termination::BudgetExhausted
        }
    );
}

#[test]
fn generation_waits_expires_and_restarts_without_reopening_or_changing_frozen_bytes() {
    let mut s = state();
    let frozen = s.frozen.canonical_bytes().unwrap();
    open(&mut s);
    assert_eq!(
        decide_next(&s, &[], 11).unwrap(),
        SearchAction::WaitForGeneration {
            expires_at_ms: 5010
        }
    );
    let restarted: SearchStateV1 = serde_json::from_slice(&s.canonical_bytes().unwrap()).unwrap();
    assert_eq!(
        decide_next(&restarted, &[], 5010).unwrap(),
        SearchAction::ExpireGeneration {}
    );
    for outcome in [
        GenerationOutcome::Timeout,
        GenerationOutcome::Invalid,
        GenerationOutcome::Duplicate,
        GenerationOutcome::ProviderError,
        GenerationOutcome::Declined,
    ] {
        s.generation.as_mut().unwrap().outcome = Some(outcome);
        assert_eq!(
            decide_next(&s, &[], 5011).unwrap(),
            SearchAction::Finish {
                reason: Termination::CandidatesExhausted
            }
        );
        assert_eq!(s.frozen.canonical_bytes().unwrap(), frozen);
    }
}

#[test]
fn admitted_d_joins_iteration_decisions_and_events_without_rewriting_frozen_candidates() {
    let mut s = state();
    let frozen = s.frozen.canonical_bytes().unwrap();
    let d = admit(&mut s);
    let p = placement(&d);
    assert!(
        matches!(decide_next(&s, std::slice::from_ref(&p), 11).unwrap(), SearchAction::IssueAttempt { derivation_ref, .. } if derivation_ref == d)
    );
    assert_eq!(s.candidates().count(), 2);
    s.frozen.policy.decision = Some(DecisionPolicy {
        provider: ProviderLocation::Requester,
        max_decisions: 4,
        decision_timeout_ms: 1000,
    });
    let SearchAction::OpenDecision {
        seq,
        choices,
        default_id,
    } = decide_next(&s, std::slice::from_ref(&p), 12).unwrap()
    else {
        panic!("decision")
    };
    s.decisions.push(DecisionRecord {
        seq,
        attempt_seq: 1,
        opened_at_ms: 12,
        default_id: default_id.clone(),
        choices,
        outcome: Some(DecisionOutcome::Chosen),
        chosen_id: Some(default_id),
    });
    s.budget.decisions_used = 1;
    let action = decide_next(&s, &[p], 13).unwrap();
    assert!(
        matches!(&action, SearchAction::IssueAttempt { derivation_ref, .. } if derivation_ref == &d)
    );
    assert!(events(&s, &action).contains(&SearchEvent::CandidateAdmitted { derivation_ref: d }));
    s.frozen.policy.decision = None;
    assert_eq!(s.frozen.canonical_bytes().unwrap(), frozen);
}

#[test]
fn a_generated_failure_does_not_generate_again_and_a_pass_still_needs_acceptance() {
    let mut s = state();
    let d = admit(&mut s);
    let mut a = s.attempts[0].clone();
    a.attempt_id = "generated-attempt".into();
    a.derivation_ref = d;
    s.attempts.push(a);
    s.budget.attempts_used = 2;
    assert_eq!(
        decide_next(&s, &[], 11).unwrap(),
        SearchAction::Finish {
            reason: Termination::CandidatesExhausted
        }
    );
    s.attempts[1].status = DurableAttemptStatus::Pass;
    assert_eq!(
        decide_next(&s, &[], 11).unwrap(),
        SearchAction::AcceptPendingRoute {
            attempt_id: "generated-attempt".into()
        }
    );
    s.attempts[1].route_accepted = true;
    assert_eq!(
        decide_next(&s, &[], 11).unwrap(),
        SearchAction::Finish {
            reason: Termination::Verified
        }
    );
}

#[test]
fn generated_candidate_cannot_change_source_requirements_effects_or_duplicate_a_known_ref() {
    let mut s = state();
    admit(&mut s);
    for field in [
        "effects",
        "requirements",
        "provisions",
        "materialization",
        "derivation_ref",
    ] {
        let mut altered = s.clone();
        let c = altered
            .generation
            .as_mut()
            .unwrap()
            .candidate
            .as_mut()
            .unwrap();
        match field {
            "effects" => c.effects = "non-repeatable".into(),
            "requirements" => c.requirements.push(Requirement {
                fact: "secret".into(),
                one_of: None,
            }),
            "provisions" => c.provisions.push("new-toolchain".into()),
            "materialization" => {
                c.materialization = CandidateInput::Retained {
                    retained_ref: "other-source".into(),
                }
            }
            _ => c.derivation_ref = altered.frozen.candidates[0].derivation_ref.clone(),
        }
        assert!(altered.validate().is_err(), "{field}");
    }
}

#[test]
fn malformed_record_or_unauthorized_policy_is_rejected() {
    let mut s = state();
    open(&mut s);
    let mut altered = s.clone();
    altered.generation.as_mut().unwrap().expires_at_ms += 1;
    assert!(altered.validate().is_err());
    altered = s.clone();
    altered.generation.as_mut().unwrap().outcome = Some(GenerationOutcome::Admitted);
    assert!(altered.validate().is_err());
    altered = s.clone();
    altered.frozen.policy.generation = None;
    assert!(altered.validate().is_err());
    altered = s.clone();
    altered.attempts.clear();
    assert!(altered.validate().is_err());
    altered = s;
    altered
        .frozen
        .policy
        .bindings
        .insert("secret".into(), "grant".into());
    assert!(altered.validate().is_err());
}

#[test]
fn known_source_transfer_cost_and_search_deadline_bound_generation() {
    let mut s = state();
    let mut p = placement(&s.frozen.candidates[0].derivation_ref);
    p.transfer_bytes = 4097;
    s.source_archive_bytes = Some(4097);
    assert_eq!(
        decide_next(&s, &[p.clone()], 10).unwrap(),
        SearchAction::Finish {
            reason: Termination::BudgetExhausted
        }
    );
    p.transfer_bytes = 4096;
    s.source_archive_bytes = Some(4096);
    assert_eq!(
        decide_next(&s, &[p], 59999).unwrap(),
        SearchAction::OpenGeneration {
            opened_at_ms: 59999,
            expires_at_ms: 60000
        }
    );
}

#[test]
fn generation_requires_full_source_budget_even_without_eligible_placements() {
    for (source_bytes, used, reserved) in [
        (None, 0, 0),
        (Some(0), 0, 0),
        (Some(64), 4095, 0),
        (Some(64), 4000, 33),
        (Some(u64::MAX), 0, 0),
    ] {
        let mut s = state();
        s.source_archive_bytes = source_bytes;
        s.budget.transfer_used = used;
        s.budget.transfer_reserved = reserved;
        let frozen = s.frozen.canonical_bytes().unwrap();
        // A stale/understated placement must not override the archive cost.
        let mut p = placement(&s.frozen.candidates[0].derivation_ref);
        p.transfer_bytes = 0;
        for placements in [vec![], vec![p]] {
            assert_eq!(
                decide_next(&s, &placements, 10).unwrap(),
                SearchAction::Finish {
                    reason: Termination::BudgetExhausted
                },
                "source={source_bytes:?}, used={used}, reserved={reserved}"
            );
        }
        assert_eq!(s.frozen.canonical_bytes().unwrap(), frozen);
    }
    let mut s = state();
    s.budget.transfer_used = 4000;
    s.budget.transfer_reserved = 32;
    assert!(matches!(
        decide_next(&s, &[], 10).unwrap(),
        SearchAction::OpenGeneration { .. }
    ));
}

#[test]
fn source_size_roundtrips_without_changing_legacy_frozen_bytes() {
    let legacy: SearchStateV1 =
        serde_json::from_str(include_str!("fixtures/search-state/d1-failed.json")).unwrap();
    assert!(legacy.source_archive_bytes.is_none());
    assert!(
        serde_json::to_value(&legacy)
            .unwrap()
            .get("source_archive_bytes")
            .is_none()
    );
    let mut with_size = legacy.clone();
    with_size.source_archive_bytes = Some(64);
    assert_eq!(
        with_size.frozen.canonical_bytes().unwrap(),
        legacy.frozen.canonical_bytes().unwrap()
    );
    assert_eq!(
        decide_next(&with_size, &[], 10).unwrap(),
        decide_next(&legacy, &[], 10).unwrap()
    );
    let restored: SearchStateV1 =
        serde_json::from_slice(&with_size.canonical_bytes().unwrap()).unwrap();
    assert_eq!(restored.source_archive_bytes, Some(64));
}

#[test]
fn exhaustion_cannot_skip_an_open_decision_chosen_inspection_or_stop() {
    let mut s = state();
    s.frozen.policy.decision = Some(DecisionPolicy {
        provider: ProviderLocation::Requester,
        max_decisions: 4,
        decision_timeout_ms: 1000,
    });
    let choices = allowed_choices(&s, &[]);
    let inspection = choices
        .iter()
        .find(|c| matches!(c.action, ChoiceAction::Inspect { .. }))
        .unwrap()
        .clone();
    let stop = choices
        .iter()
        .find(|c| matches!(c.action, ChoiceAction::Stop { .. }))
        .unwrap()
        .clone();
    s.decisions.push(DecisionRecord {
        seq: 0,
        attempt_seq: 1,
        opened_at_ms: 0,
        default_id: choices[0].choice_id.clone(),
        choices,
        outcome: None,
        chosen_id: None,
    });
    s.budget.decisions_used = 1;
    assert_eq!(
        decide_next(&s, &[], 1).unwrap(),
        SearchAction::WaitForDecision {
            seq: 0,
            expires_at_ms: 1000
        }
    );
    assert_eq!(
        decide_next(&s, &[], 1000).unwrap(),
        SearchAction::RecordFallback {
            seq: 0,
            reason: DecisionOutcome::Timeout
        }
    );
    s.decisions[0].outcome = Some(DecisionOutcome::Chosen);
    s.decisions[0].chosen_id = Some(inspection.choice_id);
    assert!(matches!(
        decide_next(&s, &[], 1).unwrap(),
        SearchAction::RunInspection { .. }
    ));
    s.decisions[0].chosen_id = Some(stop.choice_id);
    assert_eq!(
        decide_next(&s, &[], 1).unwrap(),
        SearchAction::Finish {
            reason: Termination::DecisionStopped
        }
    );
}
