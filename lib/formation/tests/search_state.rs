use ato_formation::search::*;
fn fixture(name: &str) -> SearchStateV1 {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/search-state")
        .join(format!("{name}.json"));
    let bytes = std::fs::read(path).unwrap();
    let state: SearchStateV1 = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(state.canonical_bytes().unwrap(), bytes);
    state
}
fn placements(s: &SearchStateV1) -> Vec<Placement> {
    s.frozen
        .candidates
        .iter()
        .enumerate()
        .map(|(n, d)| Placement {
            candidate_id: format!("candidate-{n}"),
            derivation_ref: d.derivation_ref.clone(),
            runtime_id: "runtime".into(),
            environment_id: "native".into(),
            admissible: true,
            transfer_bytes: 64,
        })
        .collect()
}
#[test]
fn canonical_states_have_deterministic_actions_after_restart() {
    for name in [
        "newly-created",
        "d1-failed",
        "d1-unknown",
        "d1-resolved-d2-pending",
        "retained-replay",
        "verified",
        "exhausted",
    ] {
        let s = fixture(name);
        let p = placements(&s);
        let action = decide_next(&s, &p, 1).unwrap();
        let restarted = serde_json::from_slice(&s.canonical_bytes().unwrap()).unwrap();
        assert_eq!(decide_next(&restarted, &p, 1).unwrap(), action);
        match name {
            "newly-created" => assert!(
                matches!(action,SearchAction::IssueAttempt{candidate_id,..} if candidate_id=="candidate-0")
            ),
            "d1-failed" | "d1-resolved-d2-pending" => assert!(
                matches!(action,SearchAction::IssueAttempt{candidate_id,..} if candidate_id=="candidate-1")
            ),
            "d1-unknown" => assert!(matches!(
                action,
                SearchAction::WaitForUnknownResolution { .. }
            )),
            "retained-replay" => assert!(matches!(action, SearchAction::ReplayRetained { .. })),
            "verified" => assert_eq!(
                action,
                SearchAction::Finish {
                    reason: Termination::Verified
                }
            ),
            "exhausted" => assert_eq!(
                action,
                SearchAction::Finish {
                    reason: Termination::CandidatesExhausted
                }
            ),
            _ => unreachable!(),
        }
    }
}
#[test]
fn unknown_dominates_late_receipt_and_budget_and_never_refunds() {
    let mut s = fixture("d1-unknown");
    s.budget.attempts_used = 4;
    s.attempts[0]
        .receipts
        .push(serde_json::json!({"fully_satisfied":true}));
    assert!(matches!(
        decide_next(&s, &placements(&s), 999999).unwrap(),
        SearchAction::WaitForUnknownResolution { .. }
    ));
    s.attempts[0].unknown_resolved = true;
    assert_eq!(
        decide_next(&s, &placements(&s), 1).unwrap(),
        SearchAction::Finish {
            reason: Termination::BudgetExhausted
        }
    );
    assert_eq!(s.budget.attempts_used, 4);
}
#[test]
fn runtime_unavailable_is_wait_not_d_failure_and_order_is_frozen() {
    let s = fixture("newly-created");
    let mut p = placements(&s);
    p[0].admissible = false;
    assert_eq!(
        decide_next(&s, &p, 1).unwrap(),
        SearchAction::WaitForRuntime {
            derivation_ref: s.frozen.candidates[0].derivation_ref.clone()
        }
    );
    p.reverse();
    p[1].admissible = true;
    assert!(
        matches!(decide_next(&s,&p,1).unwrap(),SearchAction::IssueAttempt{candidate_id,..} if candidate_id=="candidate-0")
    );
}
#[test]
fn uncertainty_budget_and_candidate_exhaustion_are_distinct() {
    let mut s = fixture("d1-failed");
    s.attempts[0].record = None;
    assert_eq!(
        decide_next(&s, &placements(&s), 1).unwrap(),
        SearchAction::Finish {
            reason: Termination::EffectUnknown
        }
    );
    s.attempts[0].record = Some(ExecutionRecord::Finished);
    s.budget.attempts_used = 4;
    assert_eq!(
        decide_next(&s, &placements(&s), 1).unwrap(),
        SearchAction::Finish {
            reason: Termination::BudgetExhausted
        }
    );
    s.owner_stopped = true;
    assert_eq!(
        decide_next(&s, &placements(&s), 1).unwrap(),
        SearchAction::Finish {
            reason: Termination::OwnerStopped
        }
    );
}
#[test]
fn pending_route_recovery_does_not_reexecute_or_use_receipt_alone() {
    let mut s = fixture("verified");
    s.attempts[0].route_accepted = false;
    assert_eq!(
        decide_next(&s, &placements(&s), 1).unwrap(),
        SearchAction::AcceptPendingRoute {
            attempt_id: "attempt-d1".into()
        }
    );
}
#[test]
fn frozen_identity_is_checked_and_policy_changes_change_canonical_freeze() {
    let s = fixture("newly-created");
    let mut altered = s.clone();
    altered.frozen.base_contract.requirements.clear();
    assert!(altered.validate().is_err());
    altered = s.clone();
    altered.frozen.candidates.reverse();
    assert_ne!(
        s.frozen.canonical_bytes().unwrap(),
        altered.frozen.canonical_bytes().unwrap()
    );
    altered = s.clone();
    altered.frozen.policy.budget.max_attempts += 1;
    assert_ne!(
        s.frozen.canonical_bytes().unwrap(),
        altered.frozen.canonical_bytes().unwrap()
    );
}
