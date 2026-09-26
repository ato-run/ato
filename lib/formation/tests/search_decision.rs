//! Stage 5a: a finite AllowedChoices DecisionProvider over the deterministic
//! search core. The provider only reorders offered issues; without a usable
//! answer the search takes the deterministic default under the same budget.
use ato_formation::decision::*;
use ato_formation::search::*;

fn fixture(name: &str) -> SearchStateV1 {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/search-state")
        .join(format!("{name}.json"));
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn with_policy(mut s: SearchStateV1) -> SearchStateV1 {
    s.frozen.policy.decision = Some(DecisionPolicy {
        provider: ProviderLocation::Requester,
        max_decisions: 2,
        decision_timeout_ms: 5_000,
    });
    s
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
fn opened(s: &mut SearchStateV1, at: u64) -> (Vec<Choice>, String) {
    let action = decide_next(s, &placements(s), at).unwrap();
    let SearchAction::OpenDecision {
        seq,
        default_id,
        choices,
    } = action
    else {
        panic!("expected OpenDecision, got {action:?}")
    };
    s.decisions.push(DecisionRecord {
        seq,
        opened_at_ms: at,
        default_id: default_id.clone(),
        choices: choices.iter().map(|c| c.choice_id.clone()).collect(),
        outcome: None,
        chosen_id: None,
    });
    (choices, default_id)
}
fn submit(s: &SearchStateV1, body: serde_json::Value) -> Result<DecisionVerdict, DecisionRefusal> {
    validate_decision(s, &serde_json::from_value(body).unwrap())
}
fn record(s: &mut SearchStateV1, v: &DecisionVerdict) {
    let d = s.decisions.iter_mut().find(|d| d.seq == v.seq).unwrap();
    d.outcome = Some(v.outcome);
    d.chosen_id = v.chosen_id.clone();
    if v.outcome == DecisionOutcome::Chosen {
        s.budget.decisions_used += 1;
    }
}

#[test]
fn without_a_policy_nothing_changes_and_frozen_bytes_are_identical() {
    for name in ["newly-created", "d1-failed", "verified", "retained-replay"] {
        let s = fixture(name);
        let raw = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("tests/fixtures/search-state/{name}.json")),
        )
        .unwrap();
        assert_eq!(s.canonical_bytes().unwrap(), raw, "{name}");
        assert!(!matches!(
            decide_next(&s, &placements(&s), 1).unwrap(),
            SearchAction::OpenDecision { .. }
        ));
    }
    let s = fixture("newly-created");
    let enabled = with_policy(s.clone());
    assert_ne!(
        s.frozen.canonical_bytes().unwrap(),
        enabled.frozen.canonical_bytes().unwrap()
    );
}

#[test]
fn policy_bounds_are_enforced_at_freeze() {
    for (max, timeout) in [(0, 5_000), (65, 5_000), (1, 999), (1, 120_001)] {
        let mut s = with_policy(fixture("newly-created"));
        s.frozen.policy.decision.as_mut().unwrap().max_decisions = max;
        s.frozen
            .policy
            .decision
            .as_mut()
            .unwrap()
            .decision_timeout_ms = timeout;
        assert!(s.frozen.canonical_bytes().is_err(), "{max} {timeout}");
    }
}

#[test]
fn choices_are_finite_deterministic_and_default_first() {
    let s = with_policy(fixture("newly-created"));
    let a = allowed_choices(&s, &placements(&s));
    let b = allowed_choices(&s, &placements(&s));
    assert_eq!(a, b);
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].candidate_id, "candidate-0");
    assert!(
        a.iter()
            .all(|c| c.choice_id.len() == 17 && c.choice_id.starts_with('c'))
    );
    // Inadmissible, tried, over-budget and unsafe placements are never offered.
    let mut p = placements(&s);
    p[1].admissible = false;
    assert_eq!(allowed_choices(&s, &p).len(), 1);
    let mut p = placements(&s);
    p[1].transfer_bytes = 1 << 40;
    assert_eq!(allowed_choices(&s, &p).len(), 1);
    let mut unsafe_d = s.clone();
    unsafe_d.frozen.candidates[1].effects = "non-repeatable".into();
    assert_eq!(allowed_choices(&unsafe_d, &placements(&unsafe_d)).len(), 1);
    let failed = with_policy(fixture("d1-failed"));
    let offered = allowed_choices(&failed, &placements(&failed));
    assert_eq!(offered.len(), 1);
    assert_eq!(offered[0].candidate_id, "candidate-1");
    // Bounded, whatever the placement count.
    let many: Vec<Placement> = (0..100)
        .map(|n| Placement {
            candidate_id: format!("p{n}"),
            derivation_ref: s.frozen.candidates[n % 2].derivation_ref.clone(),
            runtime_id: format!("rt{n}"),
            environment_id: "native".into(),
            admissible: true,
            transfer_bytes: 1,
        })
        .collect();
    assert_eq!(allowed_choices(&s, &many).len(), MAX_CHOICES);
}

#[test]
fn a_recorded_choice_reorders_the_frozen_default_and_survives_restart() {
    let mut s = with_policy(fixture("newly-created"));
    let (choices, default_id) = opened(&mut s, 10);
    assert_eq!(default_id, choices[0].choice_id);
    assert_eq!(
        decide_next(&s, &placements(&s), 11).unwrap(),
        SearchAction::WaitForDecision {
            seq: 0,
            expires_at_ms: 5_010
        }
    );
    let v = submit(
        &s,
        serde_json::json!({"seq":0,"choice_id":choices[1].choice_id}),
    )
    .unwrap();
    assert_eq!(v.outcome, DecisionOutcome::Chosen);
    record(&mut s, &v);
    let action = decide_next(&s, &placements(&s), 12).unwrap();
    assert!(
        matches!(&action, SearchAction::IssueAttempt{candidate_id,..} if candidate_id=="candidate-1")
    );
    // Restart: the canonical durable state yields the same action; no new point.
    let restarted: SearchStateV1 = serde_json::from_slice(&s.canonical_bytes().unwrap()).unwrap();
    assert_eq!(
        decide_next(&restarted, &placements(&restarted), 12).unwrap(),
        action
    );
    assert!(events(&s, &action).contains(&SearchEvent::DecisionMade {
        seq: 0,
        chosen_id: choices[1].choice_id.clone()
    }));
    // A second answer is refused: the point is closed.
    assert_eq!(
        submit(
            &s,
            serde_json::json!({"seq":0,"choice_id":choices[0].choice_id})
        )
        .unwrap_err()
        .code,
        "decision_closed"
    );
}

#[test]
fn every_provider_failure_takes_the_default() {
    for body in [
        serde_json::json!({"seq":0,"choice_id":"c0000000000000000"}),
        serde_json::json!({"seq":0,"fallback":"invalid"}),
        serde_json::json!({"seq":0,"fallback":"provider_error"}),
        serde_json::json!({"seq":0,"fallback":"timeout"}),
    ] {
        let mut s = with_policy(fixture("newly-created"));
        opened(&mut s, 10);
        let v = submit(&s, body.clone()).unwrap();
        assert!(v.outcome.is_fallback(), "{body}");
        record(&mut s, &v);
        assert_eq!(s.budget.decisions_used, 0);
        assert!(
            matches!(decide_next(&s,&placements(&s),11).unwrap(), SearchAction::IssueAttempt{candidate_id,..} if candidate_id=="candidate-0"),
            "{body}"
        );
    }
    // Silence: the point times out into a durable fallback, then the default.
    let mut s = with_policy(fixture("newly-created"));
    opened(&mut s, 10);
    assert_eq!(
        decide_next(&s, &placements(&s), 5_010).unwrap(),
        SearchAction::RecordFallback {
            seq: 0,
            reason: DecisionOutcome::Timeout
        }
    );
    // Malformed submissions and oversized evidence are refused outright.
    for body in [
        serde_json::json!({"seq":0}),
        serde_json::json!({"seq":0,"choice_id":"x","fallback":"invalid"}),
        serde_json::json!({"seq":0,"fallback":"chosen"}),
        serde_json::json!({"seq":0,"fallback":"out_of_set"}),
        serde_json::json!({"seq":0,"fallback":"invalid","evidence":{"x":"y".repeat(20_000)}}),
        serde_json::json!({"seq":7,"fallback":"invalid"}),
    ] {
        assert!(submit(&s, body.clone()).is_err(), "{body}");
    }
}

#[test]
fn a_choice_no_longer_issuable_takes_the_default_without_waiting() {
    let mut s = with_policy(fixture("newly-created"));
    let (choices, _) = opened(&mut s, 10);
    let v = submit(
        &s,
        serde_json::json!({"seq":0,"choice_id":choices[1].choice_id}),
    )
    .unwrap();
    record(&mut s, &v);
    let mut p = placements(&s);
    p[1].admissible = false;
    let action = decide_next(&s, &p, 11).unwrap();
    assert!(
        matches!(&action, SearchAction::IssueAttempt{candidate_id,..} if candidate_id=="candidate-0")
    );
    assert!(events(&s, &action).contains(&SearchEvent::DecisionUnavailableAtIssue { seq: 0 }));
}

#[test]
fn decision_budget_exhaustion_is_the_default_not_a_termination() {
    let mut s = with_policy(fixture("newly-created"));
    s.frozen.policy.decision.as_mut().unwrap().max_decisions = 1;
    let (choices, _) = opened(&mut s, 10);
    let v = submit(
        &s,
        serde_json::json!({"seq":0,"choice_id":choices[0].choice_id}),
    )
    .unwrap();
    record(&mut s, &v);
    // Pretend that attempt failed: the next point would open, but the budget is spent.
    let mut failed = fixture("d1-failed");
    failed.frozen = s.frozen.clone();
    failed.decisions = s.decisions.clone();
    failed.budget.decisions_used = 1;
    // Offer two placements for D2 so a point would be warranted.
    let mut p = placements(&failed);
    p.push(Placement {
        candidate_id: "candidate-2".into(),
        runtime_id: "runtime-b".into(),
        ..p[1].clone()
    });
    assert!(
        matches!(decide_next(&failed,&p,11).unwrap(), SearchAction::IssueAttempt{candidate_id,..} if candidate_id=="candidate-1")
    );
}

#[test]
fn only_issues_are_ever_replaced() {
    for name in ["d1-unknown", "verified", "exhausted"] {
        let s = with_policy(fixture(name));
        let action = decide_next(&s, &placements(&s), 1).unwrap();
        assert!(
            !matches!(
                action,
                SearchAction::OpenDecision { .. }
                    | SearchAction::WaitForDecision { .. }
                    | SearchAction::RecordFallback { .. }
            ),
            "{name}: {action:?}"
        );
    }
}

#[test]
fn durable_decision_records_are_checked() {
    let mut s = with_policy(fixture("newly-created"));
    let (choices, _) = opened(&mut s, 10);
    let mut bad = s.clone();
    bad.decisions[0].outcome = Some(DecisionOutcome::Chosen);
    bad.decisions[0].chosen_id = Some(choices[0].choice_id.clone());
    assert!(bad.validate().is_err(), "chosen without budget charge");
    bad.budget.decisions_used = 1;
    assert!(bad.validate().is_ok());
    bad.decisions[0].chosen_id = Some("c0000000000000000".into());
    assert!(bad.validate().is_err(), "chosen outside offered set");
    let mut orphan = fixture("newly-created");
    orphan.decisions = s.decisions.clone();
    assert!(orphan.validate().is_err(), "decisions need a policy");
}
