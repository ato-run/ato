//! Stage 5a: a finite AllowedChoices DecisionProvider over the deterministic
//! search core. Choices are exploration actions — attempt, bounded read-only
//! inspection or stop — and the provider only picks one of the offered ids;
//! without a usable answer the search takes the deterministic default under
//! the same budget.
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
        max_decisions: 4,
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
fn attempt_action(c: &Choice) -> Option<(&str, &str, &str, &str)> {
    match &c.action {
        ChoiceAction::Attempt {
            candidate_id,
            derivation_ref,
            runtime_id,
            environment_id,
        } => Some((
            candidate_id,
            derivation_ref,
            runtime_id,
            environment_id,
        )),
        _ => None,
    }
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
        attempt_seq: s.attempts.len() as u64,
        opened_at_ms: at,
        default_id: default_id.clone(),
        choices: choices.clone(),
        outcome: None,
        chosen_id: None,
    });
    // Opening a point spends one provider decision, whatever its outcome.
    s.budget.decisions_used += 1;
    (choices, default_id)
}
fn submit(s: &SearchStateV1, body: serde_json::Value) -> Result<DecisionVerdict, DecisionRefusal> {
    // Before the deadline of points opened at 10 with a 5 s timeout.
    validate_decision(s, &serde_json::from_value(body).unwrap(), 11)
}
fn record(s: &mut SearchStateV1, v: &DecisionVerdict) {
    let d = s.decisions.iter_mut().find(|d| d.seq == v.seq).unwrap();
    d.outcome = Some(v.outcome);
    d.chosen_id = v.chosen_id.clone();
}
fn issue_attempt(s: &mut SearchStateV1, derivation_ref: &str) {
    let d = s
        .frozen
        .candidates
        .iter()
        .find(|d| d.derivation_ref == derivation_ref)
        .unwrap();
    s.attempts.push(SearchAttempt {
        attempt_id: format!("attempt-{}", s.attempts.len()),
        derivation_ref: d.derivation_ref.clone(),
        runtime_id: "runtime".into(),
        environment_id: "native".into(),
        status: DurableAttemptStatus::Fail,
        claimed: true,
        record: Some(ExecutionRecord::Finished),
        effects: Some(d.effects.clone()),
        failure_code: Some("http_status_mismatch".into()),
        unknown_resolved: false,
        retained_ref: None,
        materialization_ref: None,
        route_accepted: false,
    });
    s.budget.attempts_used += 1;
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
    // Two attempts, one refusal-inspection per D, then the stop.
    assert_eq!(a.len(), 5);
    assert_eq!(
        attempt_action(&a[0]).unwrap().0,
        "candidate-0"
    );
    assert!(attempt_action(&a[1]).is_some());
    assert!(matches!(
        a[2].action,
        ChoiceAction::Inspect {
            inspection: InspectionKind::CandidateRefusals,
            ..
        }
    ));
    assert!(matches!(
        a[4].action,
        ChoiceAction::Stop {
            reason_class: StopReasonClass::NoPromisingAction
        }
    ));
    assert!(
        a.iter()
            .all(|c| c.choice_id.len() == 17 && c.choice_id.starts_with('c'))
    );
    // Inadmissible, tried, over-budget and unsafe placements are never offered.
    let mut p = placements(&s);
    p[1].admissible = false;
    assert_eq!(
        allowed_choices(&s, &p)
            .iter()
            .filter(|c| attempt_action(c).is_some())
            .count(),
        1
    );
    let mut p = placements(&s);
    p[1].transfer_bytes = 1 << 40;
    assert_eq!(
        allowed_choices(&s, &p)
            .iter()
            .filter(|c| attempt_action(c).is_some())
            .count(),
        1
    );
    let mut unsafe_d = s.clone();
    unsafe_d.frozen.candidates[1].effects = "non-repeatable".into();
    assert_eq!(
        allowed_choices(&unsafe_d, &placements(&unsafe_d))
            .iter()
            .filter(|c| attempt_action(c).is_some())
            .count(),
        1
    );
    let failed = with_policy(fixture("d1-failed"));
    let offered = allowed_choices(&failed, &placements(&failed));
    let attempts: Vec<_> = offered.iter().filter_map(|c| attempt_action(c)).collect();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].0, "candidate-1");
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
    let bounded = allowed_choices(&s, &many);
    assert_eq!(bounded.len(), MAX_CHOICES);
    // The stop is offered even when the set is full.
    assert!(matches!(bounded.last().unwrap().action, ChoiceAction::Stop { .. }));
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
        matches!(&action, SearchAction::IssueAttempt{candidate_id,..} if candidate_id=="candidate-1"),
        "{action:?}"
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
        // A failed point still spent its provider decision.
        assert_eq!(s.budget.decisions_used, 1);
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
fn a_fallback_releases_the_default_once_then_the_frontier_moves_on() {
    let mut s = with_policy(fixture("newly-created"));
    let (_, default_id) = opened(&mut s, 10);
    let v = submit(&s, serde_json::json!({"seq":0,"fallback":"provider_error"})).unwrap();
    record(&mut s, &v);
    // The released action is the default issue.
    let issue = decide_next(&s, &placements(&s), 11).unwrap();
    assert!(
        matches!(&issue, SearchAction::IssueAttempt{candidate_id,..} if candidate_id=="candidate-0"),
        "{issue:?}"
    );
    // Once that attempt landed the point is consumed and a NEW point opens —
    // without issuing anything else, the decision sequence advanced.
    { let d = s.frozen.candidates[0].derivation_ref.clone(); issue_attempt(&mut s, &d); }
    let action = decide_next(&s, &placements(&s), 12).unwrap();
    assert!(
        matches!(&action, SearchAction::OpenDecision{seq:1,..}),
        "{action:?}"
    );
    let _ = default_id;
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
fn an_inspection_choice_runs_once_records_evidence_and_opens_a_new_point() {
    let mut s = with_policy(fixture("newly-created"));
    let (choices, _) = opened(&mut s, 10);
    let inspect = choices
        .iter()
        .find(|c| {
            matches!(
                c.action,
                ChoiceAction::Inspect {
                    inspection: InspectionKind::CandidateRefusals,
                    ..
                }
            )
        })
        .unwrap()
        .clone();
    let v = submit(&s, serde_json::json!({"seq":0,"choice_id":inspect.choice_id})).unwrap();
    record(&mut s, &v);
    // No attempt was issued: the action is a Coordinator-side inspection.
    let action = decide_next(&s, &placements(&s), 11).unwrap();
    let SearchAction::RunInspection {
        inspection,
        target_ref,
    } = &action
    else {
        panic!("expected RunInspection, got {action:?}")
    };
    assert_eq!(*inspection, InspectionKind::CandidateRefusals);
    // The recorded evidence consumes the point; the SAME durable state is
    // stable across a restart (the inspection is not re-run) and a new
    // decision point opens at the next decision sequence — attempt count did
    // not move.
    s.evidence.push(InspectionEvidence {
        schema: INSPECTION_SCHEMA.into(),
        kind: *inspection,
        target_ref: target_ref.clone(),
        decision_seq: 0,
        result: InspectionResult::CandidateRefusals {
            refusals: vec![],
        },
        recorded_at_ms: 11,
    });
    assert_eq!(s.attempts.len(), 0);
    let restarted: SearchStateV1 = serde_json::from_slice(&s.canonical_bytes().unwrap()).unwrap();
    let action = decide_next(&restarted, &placements(&restarted), 12).unwrap();
    assert!(
        matches!(&action, SearchAction::OpenDecision{seq:1,..}),
        "{action:?}"
    );
    assert!(events(&restarted, &action)
        .contains(&SearchEvent::InspectionRecorded {
            inspection: InspectionKind::CandidateRefusals,
            target_ref: target_ref.clone()
        }));
    // The collected evidence is no longer offered again.
    let next = allowed_choices(&s, &placements(&s));
    assert!(
        !next.iter().any(|c| matches!(
            &c.action,
            ChoiceAction::Inspect {
                inspection: InspectionKind::CandidateRefusals,
                target_ref: t
            } if t == target_ref
        )),
        "{next:?}"
    );
    // And the attempt choices are still there for the new point.
    assert!(next.iter().any(|c| attempt_action(c).is_some()));
}

#[test]
fn a_stop_choice_finishes_without_a_ticket_and_is_not_a_failure() {
    let mut s = with_policy(fixture("newly-created"));
    let (choices, _) = opened(&mut s, 10);
    let stop = choices
        .iter()
        .find(|c| matches!(c.action, ChoiceAction::Stop { .. }))
        .unwrap()
        .clone();
    let v = submit(&s, serde_json::json!({"seq":0,"choice_id":stop.choice_id})).unwrap();
    record(&mut s, &v);
    assert_eq!(
        decide_next(&s, &placements(&s), 11).unwrap(),
        SearchAction::Finish {
            reason: Termination::DecisionStopped
        }
    );
    // Stable after a restart; the termination is not a K failure.
    let restarted: SearchStateV1 = serde_json::from_slice(&s.canonical_bytes().unwrap()).unwrap();
    assert_eq!(
        decide_next(&restarted, &placements(&restarted), 11).unwrap(),
        SearchAction::Finish {
            reason: Termination::DecisionStopped
        }
    );
}

#[test]
fn attempt_failures_is_offered_only_when_a_failure_exists() {
    let s = with_policy(fixture("newly-created"));
    assert!(
        !allowed_choices(&s, &placements(&s)).iter().any(|c| matches!(
            c.action,
            ChoiceAction::Inspect {
                inspection: InspectionKind::AttemptFailures,
                ..
            }
        ))
    );
    let failed = with_policy(fixture("d1-failed"));
    let target = failed.frozen.candidates[0].derivation_ref.clone();
    let offered = allowed_choices(&failed, &placements(&failed));
    assert!(offered.iter().any(|c| matches!(
        &c.action,
        ChoiceAction::Inspect {
            inspection: InspectionKind::AttemptFailures,
            target_ref: t
        } if t == &target
    )));
}

#[test]
fn decision_seq_is_independent_of_the_attempt_count() {
    let mut s = with_policy(fixture("newly-created"));
    // Two points back to back with no attempt between: an inspection at 0,
    // then a second point at decision seq 1, still zero attempts.
    let (choices, _) = opened(&mut s, 10);
    let inspect = choices
        .iter()
        .find(|c| matches!(c.action, ChoiceAction::Inspect { .. }))
        .unwrap()
        .clone();
    let v = submit(&s, serde_json::json!({"seq":0,"choice_id":inspect.choice_id})).unwrap();
    record(&mut s, &v);
    let run = decide_next(&s, &placements(&s), 11).unwrap();
    let (inspection, target) = match &run {
        SearchAction::RunInspection {
            inspection,
            target_ref,
        } => (*inspection, target_ref.clone()),
        other => panic!("{other:?}"),
    };
    s.evidence.push(InspectionEvidence {
        schema: INSPECTION_SCHEMA.into(),
        kind: inspection,
        target_ref: target,
        decision_seq: 0,
        result: InspectionResult::CandidateRefusals {
            refusals: vec![],
        },
        recorded_at_ms: 11,
    });
    let action = decide_next(&s, &placements(&s), 12).unwrap();
    assert!(
        matches!(&action, SearchAction::OpenDecision{seq:1,..}),
        "{action:?}"
    );
    assert_eq!(s.attempts.len(), 0);
    assert!(s.validate().is_ok());
}

#[test]
fn decision_budget_exhaustion_is_the_default_not_a_termination() {
    // A provider that only ever fails still spends the budget: with
    // max_decisions 1, no second point opens after a failed first one.
    for failure in ["provider_error", "invalid", "timeout"] {
        let mut s = with_policy(fixture("newly-created"));
        s.frozen.policy.decision.as_mut().unwrap().max_decisions = 1;
        opened(&mut s, 10);
        let v = submit(&s, serde_json::json!({"seq":0,"fallback":failure})).unwrap();
        record(&mut s, &v);
        assert_eq!(s.budget.decisions_used, 1, "{failure}");
        // The default attempt then failed; two placements for D2 would warrant a point.
        let mut failed = fixture("d1-failed");
        failed.frozen = s.frozen.clone();
        failed.decisions = s.decisions.clone();
        failed.budget.decisions_used = 1;
        let mut p = placements(&failed);
        p.push(Placement {
            candidate_id: "candidate-2".into(),
            runtime_id: "runtime-b".into(),
            ..p[1].clone()
        });
        assert!(
            matches!(decide_next(&failed,&p,11).unwrap(), SearchAction::IssueAttempt{candidate_id,..} if candidate_id=="candidate-1"),
            "{failure}"
        );
    }
}

#[test]
fn the_deadline_is_authoritative_for_answers() {
    let mut s = with_policy(fixture("newly-created"));
    let (choices, _) = opened(&mut s, 10);
    let answer: DecisionSubmission =
        serde_json::from_value(serde_json::json!({"seq":0,"choice_id":choices[1].choice_id}))
            .unwrap();
    // One millisecond before the deadline the choice is accepted...
    assert_eq!(
        validate_decision(&s, &answer, 5_009).unwrap().outcome,
        DecisionOutcome::Chosen
    );
    // ...from the deadline on, only the timeout fallback may settle the point.
    for now in [5_010, 5_011, u64::MAX] {
        assert_eq!(
            validate_decision(&s, &answer, now).unwrap_err().code,
            "decision_expired"
        );
    }
    assert_eq!(
        decide_next(&s, &placements(&s), 5_011).unwrap(),
        SearchAction::RecordFallback {
            seq: 0,
            reason: DecisionOutcome::Timeout
        }
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
                    | SearchAction::RunInspection { .. }
            ),
            "{name}: {action:?}"
        );
    }
}

#[test]
fn durable_decision_records_are_checked() {
    let mut s = with_policy(fixture("newly-created"));
    let (choices, _) = opened(&mut s, 10);
    assert!(s.validate().is_ok());
    // decisions_used is the number of opened points, exactly.
    for used in [0, 2] {
        let mut bad = s.clone();
        bad.budget.decisions_used = used;
        assert!(bad.validate().is_err(), "decisions_used {used}");
    }
    let mut bad = s.clone();
    bad.budget.decisions_used = 1;
    bad.decisions[0].outcome = Some(DecisionOutcome::Chosen);
    bad.decisions[0].chosen_id = Some(choices[0].choice_id.clone());
    assert!(bad.validate().is_ok());
    bad.decisions[0].chosen_id = Some("c0000000000000000".into());
    assert!(bad.validate().is_err(), "chosen outside offered set");
    // The decision sequence is the record's position, not an attempt count.
    let mut bad_seq = s.clone();
    bad_seq.budget.decisions_used = 1;
    bad_seq.decisions[0].seq = 3;
    assert!(bad_seq.validate().is_err(), "seq is the ordinal");
    // attempt_seq is the issued count at open: it cannot exceed the attempts.
    let mut bad_attempt = s.clone();
    bad_attempt.budget.decisions_used = 1;
    bad_attempt.decisions[0].attempt_seq = 1;
    assert!(bad_attempt.validate().is_err(), "attempt_seq beyond attempts");
    // Only the last point may still be open.
    let mut two = s.clone();
    two.decisions.push(two.decisions[0].clone());
    two.decisions[1].seq = 1;
    two.budget.decisions_used = 2;
    assert!(two.validate().is_err(), "a non-last point cannot be open");
    let mut orphan = fixture("newly-created");
    orphan.decisions = s.decisions.clone();
    orphan.budget.decisions_used = 1;
    assert!(orphan.validate().is_err(), "decisions need a policy");
}

#[test]
fn durable_evidence_records_are_checked() {
    let mut s = with_policy(fixture("newly-created"));
    let (choices, _) = opened(&mut s, 10);
    let inspect = choices
        .iter()
        .find(|c| matches!(c.action, ChoiceAction::Inspect { .. }))
        .unwrap()
        .clone();
    let v = submit(&s, serde_json::json!({"seq":0,"choice_id":inspect.choice_id})).unwrap();
    record(&mut s, &v);
    let Some(ChoiceAction::Inspect {
        inspection,
        target_ref,
    }) = Some(inspect.action.clone())
    else {
        unreachable!()
    };
    let evidence = InspectionEvidence {
        schema: INSPECTION_SCHEMA.into(),
        kind: inspection,
        target_ref: target_ref.clone(),
        decision_seq: 0,
        result: InspectionResult::CandidateRefusals {
            refusals: vec![RefusalEntry {
                runtime_id: "runtime".into(),
                environment_id: "native".into(),
                reasons: vec![serde_json::json!({"code": "requirement_unmet", "fact": "x"})],
            }],
        },
        recorded_at_ms: 11,
    };
    s.evidence.push(evidence.clone());
    assert!(s.validate().is_ok());
    // Evidence must come from the point that chose it.
    let mut bad = s.clone();
    bad.evidence[0].result = InspectionResult::AttemptFailures { failures: vec![] };
    assert!(bad.validate().is_err(), "result kind must match the record kind");
    let mut bad = s.clone();
    bad.evidence[0].target_ref = "other".into();
    assert!(bad.validate().is_err(), "the point chose another target");
    let mut bad = s.clone();
    bad.evidence[0].decision_seq = 9;
    assert!(bad.validate().is_err(), "no such decision point");
    let mut bad = s.clone();
    bad.evidence[0].schema = "other".into();
    assert!(bad.validate().is_err(), "schema");
    let mut bad = s.clone();
    bad.evidence.push(bad.evidence[0].clone());
    assert!(bad.validate().is_err(), "evidence is write-once per (kind,target)");
    let mut bad = s.clone();
    bad.evidence[0] = InspectionEvidence {
        schema: INSPECTION_SCHEMA.into(),
        kind: inspection,
        target_ref,
        decision_seq: 0,
        result: InspectionResult::CandidateRefusals {
            refusals: vec![RefusalEntry {
                runtime_id: "runtime".into(),
                environment_id: "native".into(),
                reasons: vec![serde_json::json!("not-an-object")],
            }],
        },
        recorded_at_ms: 11,
    };
    assert!(bad.validate().is_err(), "reasons must be typed objects");
}
