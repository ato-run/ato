//! Stage 5a: an optional DecisionProvider picks the next exploration action
//! from a finite set the deterministic core enumerates — an attempt, a bounded
//! read-only inspection, or an explicit stop. It never decides K, never adds a
//! D, placement, argv, source change or probe argument, and its failure is
//! never a search failure: without a usable choice the search takes the
//! deterministic default.
//!
//! The provider runs on the requester side. The Coordinator only opens a
//! decision point, records the one answer it accepts for it (or the fallback),
//! executes the recorded action — an issue, a Coordinator-side inspection or a
//! stop — and reuses that durable record after any restart; no model is asked
//! twice.
use crate::search::{
    CandidateInput, Placement, SearchAction, SearchError, SearchStateV1, Termination, available,
    safe,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// At most this many choices are offered at one decision point.
pub const MAX_CHOICES: usize = 32;
/// Provider evidence kept with a decision (model, confidence, usage).
pub const MAX_EVIDENCE_BYTES: usize = 16 * 1024;
pub const MAX_DECISIONS: u32 = 64;
pub const MIN_DECISION_TIMEOUT_MS: u64 = 1_000;
pub const MAX_DECISION_TIMEOUT_MS: u64 = 120_000;

/// Schema of the durable evidence record of one inspection. Bounded,
/// read-only, deterministic input, typed output.
pub const INSPECTION_SCHEMA: &str = "ato.formation-inspection/1";
/// Inspection evidence a search may hold; kinds x candidates bound it too.
pub const MAX_INSPECTION_RECORDS: usize = 128;
pub const MAX_REFUSAL_ENTRIES: usize = 32;
pub const MAX_REFUSAL_REASONS: usize = 16;
/// One typed refusal-reason object, serialized.
pub const MAX_REASON_BYTES: usize = 1_024;
pub const MAX_FAILURE_ENTRIES: usize = 16;
pub const MAX_FAILURE_MESSAGE_BYTES: usize = 512;

/// Frozen with the search: whether, and how far, a provider may choose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionPolicy {
    pub provider: ProviderLocation,
    /// Decision points a provider may answer over the whole search.
    pub max_decisions: u32,
    /// How long an open decision point waits before the default is taken.
    pub decision_timeout_ms: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderLocation {
    /// The requester's provider answers; the Coordinator never calls a model.
    Requester,
}
impl DecisionPolicy {
    pub fn validate(&self) -> Result<(), SearchError> {
        if self.max_decisions == 0
            || self.max_decisions > MAX_DECISIONS
            || !(MIN_DECISION_TIMEOUT_MS..=MAX_DECISION_TIMEOUT_MS)
                .contains(&self.decision_timeout_ms)
        {
            return Err(SearchError("decision_policy_bounds"));
        }
        Ok(())
    }
}

// serde_json::Value is only PartialEq (a JSON Number cannot represent NaN),
// so the derived comparisons on the evidence types are total.
impl Eq for RefusalEntry {}
impl Eq for InspectionResult {}
impl Eq for InspectionEvidence {}


/// The bounded, pre-defined read-only inspections a provider may ask for.
/// Each maps to one Coordinator-side typed evidence producer; there are no
/// provider-supplied arguments and nothing here can reach a shell, the
/// network, secrets or the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectionKind {
    /// The typed hard-filter reasons that keep a D off its placements.
    CandidateRefusals,
    /// The typed failure summaries of the attempts a D already had.
    AttemptFailures,
}
/// The only stop a provider may take: a decision it made, not a free reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReasonClass {
    NoPromisingAction,
}

/// What an offered choice does. The payload is fixed by the deterministic
/// core; the provider answers with the choice's id only and can never rewrite
/// an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChoiceAction {
    /// Issue the next attempt: a frozen D on an admissible, untried placement.
    Attempt {
        candidate_id: String,
        derivation_ref: String,
        runtime_id: String,
        environment_id: String,
    },
    /// Run a bounded read-only inspection and record its typed evidence.
    Inspect {
        inspection: InspectionKind,
        /// The object the inspection reads: a frozen DerivationRef.
        target_ref: String,
    },
    /// End the search because no offered action looks promising. Not a K
    /// failure and not an owner stop.
    Stop { reason_class: StopReasonClass },
}

/// One offered action, as durably recorded. choice_id is a stable digest of
/// the point's sequence and the typed action; mutable availability and
/// descriptions are never part of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    pub choice_id: String,
    pub action: ChoiceAction,
}

/// The typed, bounded result of one inspection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InspectionResult {
    /// Why a D may not be tried on a placement, per environment.
    CandidateRefusals { refusals: Vec<RefusalEntry> },
    /// What the finished non-pass attempts of a D reported.
    AttemptFailures { failures: Vec<FailureEntry> },
}
/// One placement a D is refused on: the environment and every typed reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefusalEntry {
    pub runtime_id: String,
    pub environment_id: String,
    /// Typed hard-filter reasons ({code, ...} objects) from the candidate row.
    pub reasons: Vec<serde_json::Value>,
}
/// One finished non-pass attempt of a D, as typed durable evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureEntry {
    pub attempt_id: String,
    pub runtime_id: String,
    pub environment_id: String,
    /// fail, inconclusive, expired or unknown.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_stage: Option<String>,
    /// The bounded failure message; never raw logs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Durable evidence produced by a chosen Inspect action: append-only,
/// write-once per (kind, target).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspectionEvidence {
    pub schema: String,
    pub kind: InspectionKind,
    pub target_ref: String,
    /// The decision point whose chosen Inspect produced this evidence.
    pub decision_seq: u64,
    pub result: InspectionResult,
    pub recorded_at_ms: u64,
}
impl InspectionEvidence {
    pub(crate) fn validate(&self) -> Result<(), SearchError> {
        if self.schema != INSPECTION_SCHEMA {
            return Err(SearchError("evidence_schema"));
        }
        let matches = matches!(
            (&self.kind, &self.result),
            (
                InspectionKind::CandidateRefusals,
                InspectionResult::CandidateRefusals { .. }
            ) | (
                InspectionKind::AttemptFailures,
                InspectionResult::AttemptFailures { .. }
            )
        );
        if !matches {
            return Err(SearchError("evidence_kind_result_mismatch"));
        }
        match &self.result {
            InspectionResult::CandidateRefusals { refusals } => {
                if refusals.len() > MAX_REFUSAL_ENTRIES {
                    return Err(SearchError("evidence_bounds"));
                }
                for entry in refusals {
                    if entry.reasons.len() > MAX_REFUSAL_REASONS {
                        return Err(SearchError("evidence_bounds"));
                    }
                    for reason in &entry.reasons {
                        // Reasons are typed {code, ...} objects only.
                        let bytes =
                            serde_json::to_vec(reason).map_or(usize::MAX, |b| b.len());
                        if !reason.is_object()
                            || !reason["code"].is_string()
                            || bytes > MAX_REASON_BYTES
                        {
                            return Err(SearchError("evidence_reason"));
                        }
                    }
                }
            }
            InspectionResult::AttemptFailures { failures } => {
                if failures.len() > MAX_FAILURE_ENTRIES {
                    return Err(SearchError("evidence_bounds"));
                }
                for failure in failures {
                    if failure
                        .message
                        .as_deref()
                        .is_some_and(|m| m.len() > MAX_FAILURE_MESSAGE_BYTES)
                    {
                        return Err(SearchError("evidence_bounds"));
                    }
                }
            }
        }
        Ok(())
    }
}

/// One decision point, as durably recorded. choices are the offers made when
/// it opened; an answer is judged against exactly those.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRecord {
    /// The point's own sequence: the number of decision points opened before
    /// it. Independent of the attempt count, so an inspection can open a new
    /// point without issuing anything.
    pub seq: u64,
    /// How many attempts the search had issued when the point opened. It is
    /// what tells "the released action is still pending" from "the search
    /// moved on".
    pub attempt_seq: u64,
    pub opened_at_ms: u64,
    pub default_id: String,
    pub choices: Vec<Choice>,
    pub outcome: Option<DecisionOutcome>,
    pub chosen_id: Option<String>,
}
impl DecisionRecord {
    /// The instant from which only the timeout fallback may settle the point.
    pub fn expires_at_ms(&self, policy: &DecisionPolicy) -> u64 {
        self.opened_at_ms.saturating_add(policy.decision_timeout_ms)
    }
    /// The action a settled point released: its choice, or the default after
    /// any fallback outcome.
    pub fn released(&self) -> Option<&ChoiceAction> {
        let id = match self.outcome {
            Some(DecisionOutcome::Chosen) => self.chosen_id.as_ref()?,
            Some(_) => &self.default_id,
            None => return None,
        };
        self.choices
            .iter()
            .find(|c| &c.choice_id == id)
            .map(|c| &c.action)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOutcome {
    Chosen,
    /// No answer before the point's deadline.
    Timeout,
    /// The provider's answer was malformed.
    Invalid,
    /// The provider named something that was not offered.
    OutOfSet,
    /// The provider could not be reached or refused.
    ProviderError,
}
impl DecisionOutcome {
    pub fn is_fallback(self) -> bool {
        self != Self::Chosen
    }
}

/// A stable label for (decision seq, typed action): Jev answers with a label,
/// and the same point always yields the same labels.
pub fn choice_id(seq: u64, action: &ChoiceAction) -> String {
    let canonical = serde_jcs::to_vec(&serde_json::json!({
        "seq": seq,
        "action": action,
    }))
    .expect("plain JSON");
    let digest = Sha256::digest(canonical);
    let mut id = String::from("c");
    for byte in &digest[..8] {
        id.push_str(&format!("{byte:02x}"));
    }
    id
}
/// Every action the deterministic core could take now, in frozen order: the
/// issuable attempts first (frozen D order, then the Coordinator's ranking),
/// then the inspections still missing their evidence, then the stop. The
/// default is always the first, the same issue the core would take alone.
pub fn allowed_choices(s: &SearchStateV1, placements: &[Placement]) -> Vec<Choice> {
    let seq = s.decisions.len() as u64;
    let b = &s.budget;
    let l = &s.frozen.policy.budget;
    let transfer_left = available(l.max_transfer_bytes, b.transfer_used, b.transfer_reserved);
    let mut out = Vec::new();
    for d in s.frozen.candidates.iter().filter(|d| safe(&d.effects)) {
        for p in placements.iter().filter(|p| {
            p.derivation_ref == d.derivation_ref
                && p.admissible
                && p.transfer_bytes <= transfer_left
        }) {
            let tried = s.attempts.iter().any(|a| {
                a.derivation_ref == d.derivation_ref
                    && a.runtime_id == p.runtime_id
                    && a.environment_id == p.environment_id
            });
            let duplicate = out.iter().any(|c: &Choice| {
                matches!(
                    &c.action,
                    ChoiceAction::Attempt {
                        derivation_ref,
                        runtime_id,
                        environment_id,
                        ..
                    } if *derivation_ref == d.derivation_ref
                        && *runtime_id == p.runtime_id
                        && *environment_id == p.environment_id
                )
            });
            if tried || duplicate {
                continue;
            }
            let action = ChoiceAction::Attempt {
                candidate_id: p.candidate_id.clone(),
                derivation_ref: d.derivation_ref.clone(),
                runtime_id: p.runtime_id.clone(),
                environment_id: p.environment_id.clone(),
            };
            out.push(Choice {
                choice_id: choice_id(seq, &action),
                action,
            });
            if out.len() == MAX_CHOICES - 1 {
                break;
            }
        }
        if out.len() == MAX_CHOICES - 1 {
            break;
        }
    }
    // Inspections whose evidence is not recorded yet, in frozen D order.
    // attempt_failures exists only for a D with a finished non-pass attempt.
    if out.len() < MAX_CHOICES - 1 {
        'ds: for d in &s.frozen.candidates {
            for inspection in [
                InspectionKind::CandidateRefusals,
                InspectionKind::AttemptFailures,
            ] {
                if inspection == InspectionKind::AttemptFailures
                    && !s.attempts.iter().any(|a| {
                        a.derivation_ref == d.derivation_ref
                            && matches!(
                                a.status,
                                crate::search::DurableAttemptStatus::Fail
                                    | crate::search::DurableAttemptStatus::Inconclusive
                                    | crate::search::DurableAttemptStatus::Expired
                                    | crate::search::DurableAttemptStatus::Unknown
                            )
                    })
                {
                    continue;
                }
                if s.evidence
                    .iter()
                    .any(|e| e.kind == inspection && e.target_ref == d.derivation_ref)
                {
                    continue;
                }
                let action = ChoiceAction::Inspect {
                    inspection,
                    target_ref: d.derivation_ref.clone(),
                };
                out.push(Choice {
                    choice_id: choice_id(seq, &action),
                    action,
                });
                if out.len() == MAX_CHOICES - 1 {
                    break 'ds;
                }
            }
        }
    }
    // The stop is always offered and never squeezed out: the provider can
    // always decide to end the exploration.
    let stop = ChoiceAction::Stop {
        reason_class: StopReasonClass::NoPromisingAction,
    };
    out.push(Choice {
        choice_id: choice_id(seq, &stop),
        action: stop,
    });
    out
}

fn issue(s: &SearchStateV1, c: &ChoiceAction) -> SearchAction {
    let ChoiceAction::Attempt {
        candidate_id,
        derivation_ref,
        runtime_id,
        environment_id,
    } = c
    else {
        unreachable!("issue takes an attempt action")
    };
    let d = s
        .frozen
        .candidates
        .iter()
        .find(|d| &d.derivation_ref == derivation_ref)
        .expect("choices come from the frozen list");
    match &d.materialization {
        CandidateInput::Source { .. } => SearchAction::IssueAttempt {
            candidate_id: candidate_id.clone(),
            derivation_ref: derivation_ref.clone(),
            runtime_id: runtime_id.clone(),
            environment_id: environment_id.clone(),
        },
        CandidateInput::Retained { retained_ref } => SearchAction::ReplayRetained {
            candidate_id: candidate_id.clone(),
            derivation_ref: derivation_ref.clone(),
            runtime_id: runtime_id.clone(),
            environment_id: environment_id.clone(),
            retained_ref: retained_ref.clone(),
        },
    }
}

/// The provider's say over a deterministic default. The frontier is the last
/// recorded decision point: an open one must be answered or expire; a settled
/// one must release its recorded action — an issue, an inspection or a stop —
/// exactly once; only then does a new point open.
pub(crate) fn apply(
    s: &SearchStateV1,
    placements: &[Placement],
    now_ms: u64,
    default: SearchAction,
) -> SearchAction {
    let Some(policy) = &s.frozen.policy.decision else {
        return default;
    };
    if !matches!(
        default,
        SearchAction::IssueAttempt { .. } | SearchAction::ReplayRetained { .. }
    ) {
        return default;
    }
    if let Some(record) = s.decisions.last() {
        match record.outcome {
            None => {
                let expires_at_ms = record.expires_at_ms(policy);
                return if now_ms >= expires_at_ms {
                    SearchAction::RecordFallback {
                        seq: record.seq,
                        reason: DecisionOutcome::Timeout,
                    }
                } else {
                    SearchAction::WaitForDecision {
                        seq: record.seq,
                        expires_at_ms,
                    }
                };
            }
            Some(DecisionOutcome::Chosen) => {
                match record.released() {
                    Some(attempt @ ChoiceAction::Attempt {
                        derivation_ref,
                        runtime_id,
                        environment_id,
                        ..
                    }) => {
                        let issued = s.attempts.iter().any(|a| {
                            a.derivation_ref == *derivation_ref
                                && a.runtime_id == *runtime_id
                                && a.environment_id == *environment_id
                        });
                        if !issued && s.attempts.len() as u64 == record.attempt_seq {
                            let offered = allowed_choices(s, placements)
                                .into_iter()
                                .find(|c| match (&c.action, attempt) {
                                    (
                                        ChoiceAction::Attempt {
                                            derivation_ref: d1,
                                            runtime_id: r1,
                                            environment_id: e1,
                                            ..
                                        },
                                        ChoiceAction::Attempt {
                                            derivation_ref: d2,
                                            runtime_id: r2,
                                            environment_id: e2,
                                            ..
                                        },
                                    ) => d1 == d2 && r1 == r2 && e1 == e2,
                                    _ => false,
                                });
                            return match offered {
                                Some(choice) => issue(s, &choice.action),
                                // Offered then, no longer issuable now: the
                                // default, not a wait.
                                None => default,
                            };
                        }
                        // Issued, or substituted by the default: consumed.
                    }
                    Some(ChoiceAction::Inspect {
                        inspection,
                        target_ref,
                    }) => {
                        if !s.evidence.iter().any(|e| {
                            e.kind == *inspection && e.target_ref == *target_ref
                        }) {
                            return SearchAction::RunInspection {
                                inspection: *inspection,
                                target_ref: target_ref.clone(),
                            };
                        }
                        // Evidence recorded: consumed.
                    }
                    Some(ChoiceAction::Stop { .. }) => {
                        return SearchAction::Finish {
                            reason: Termination::DecisionStopped,
                        };
                    }
                    // A record the state validator would refuse: never derive
                    // an action from it.
                    None => return default,
                }
            }
            Some(_) => {
                // A fallback releases the deterministic default exactly once:
                // pending while no attempt was issued past the point.
                if s.attempts.len() as u64 == record.attempt_seq {
                    return default;
                }
                // Issued: consumed.
            }
        }
    }
    let seq = s.decisions.len() as u64;
    let choices = allowed_choices(s, placements);
    if choices.len() < 2 || s.budget.decisions_used >= u64::from(policy.max_decisions) {
        default
    } else {
        SearchAction::OpenDecision {
            seq,
            default_id: choices[0].choice_id.clone(),
            choices,
        }
    }
}

/// A requester's answer to an open decision point.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionSubmission {
    pub seq: u64,
    #[serde(default)]
    pub choice_id: Option<String>,
    /// A requester-side failure: invalid, provider_error or timeout.
    #[serde(default)]
    pub fallback: Option<DecisionOutcome>,
    #[serde(default)]
    pub evidence: Option<serde_json::Value>,
}
/// What the Coordinator records for a submission, or why it refuses it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionVerdict {
    pub seq: u64,
    pub outcome: DecisionOutcome,
    pub chosen_id: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionRefusal {
    pub code: &'static str,
}

/// Judge a submission against the decision point as it was opened, at the
/// Coordinator's trusted now_ms. A named choice that was not offered is
/// recorded as out_of_set (the default is taken); a point that is already
/// decided, not open, or past its deadline is refused — past the deadline the
/// point belongs to the timeout fallback, never to a late answer.
pub fn validate_decision(
    s: &SearchStateV1,
    submission: &DecisionSubmission,
    now_ms: u64,
) -> Result<DecisionVerdict, DecisionRefusal> {
    s.validate().map_err(|_| DecisionRefusal {
        code: "search_state_invalid",
    })?;
    let Some(policy) = &s.frozen.policy.decision else {
        return Err(DecisionRefusal {
            code: "decision_not_enabled",
        });
    };
    let record = s
        .decisions
        .iter()
        .find(|d| d.seq == submission.seq)
        .ok_or(DecisionRefusal {
            code: "decision_not_open",
        })?;
    if record.outcome.is_some() {
        return Err(DecisionRefusal {
            code: "decision_closed",
        });
    }
    if now_ms >= record.expires_at_ms(policy) {
        return Err(DecisionRefusal {
            code: "decision_expired",
        });
    }
    if let Some(evidence) = &submission.evidence
        && serde_json::to_vec(evidence).map_or(true, |b| b.len() > MAX_EVIDENCE_BYTES)
    {
        return Err(DecisionRefusal {
            code: "decision_evidence_too_large",
        });
    }
    let verdict = |outcome, chosen_id| DecisionVerdict {
        seq: record.seq,
        outcome,
        chosen_id,
    };
    let offered = |id: &String| record.choices.iter().any(|c| &c.choice_id == id);
    match (&submission.choice_id, submission.fallback) {
        (Some(id), None) if offered(id) => {
            Ok(verdict(DecisionOutcome::Chosen, Some(id.clone())))
        }
        (Some(_), None) => Ok(verdict(DecisionOutcome::OutOfSet, None)),
        (
            None,
            Some(
                reason @ (DecisionOutcome::Invalid
                | DecisionOutcome::ProviderError
                | DecisionOutcome::Timeout),
            ),
        ) => Ok(verdict(reason, None)),
        _ => Err(DecisionRefusal {
            code: "decision_submission_invalid",
        }),
    }
}

impl SearchStateV1 {
    pub(crate) fn validate_decisions(&self) -> Result<(), SearchError> {
        // Every opened point spends one provider decision, whatever its outcome.
        if self.budget.decisions_used != self.decisions.len() as u64 {
            return Err(SearchError("decision_budget_mismatch"));
        }
        if self.decisions.is_empty() && self.evidence.is_empty() {
            return Ok(());
        }
        let policy = self
            .frozen
            .policy
            .decision
            .as_ref()
            .ok_or(SearchError("decision_without_policy"))?;
        if self.decisions.len() > 256 || self.evidence.len() > MAX_INSPECTION_RECORDS {
            return Err(SearchError("decision_bounds"));
        }
        if self.budget.decisions_used > u64::from(policy.max_decisions) {
            return Err(SearchError("decision_budget"));
        }
        let last = self.decisions.len().saturating_sub(1);
        let mut previous_attempt_seq = 0;
        for (index, d) in self.decisions.iter().enumerate() {
            let consistent = match d.outcome {
                Some(DecisionOutcome::Chosen) => d
                    .chosen_id
                    .as_ref()
                    .is_some_and(|id| d.choices.iter().any(|c| &c.choice_id == id)),
                _ => d.chosen_id.is_none(),
            };
            let mut ids = std::collections::BTreeSet::new();
            if d.seq != index as u64
                || d.attempt_seq < previous_attempt_seq
                || d.attempt_seq > self.attempts.len() as u64
                || d.choices.len() < 2
                || d.choices.len() > MAX_CHOICES
                || d.choices.iter().any(|c| !ids.insert(&c.choice_id))
                || !d.choices.iter().any(|c| c.choice_id == d.default_id)
                || !consistent
                // A point must settle before the next one opens: only the
                // last record may still be open.
                || (d.outcome.is_none() && index != last)
            {
                return Err(SearchError("decision_record"));
            }
            previous_attempt_seq = d.attempt_seq;
        }
        // Inspection evidence is append-only and owned by exactly one point:
        // its decision_seq names a point that chose that very inspection.
        let mut inspected = std::collections::BTreeSet::new();
        for e in &self.evidence {
            e.validate()?;
            let produces = self
                .decisions
                .iter()
                .find(|d| d.seq == e.decision_seq)
                .is_some_and(|d| {
                    matches!(
                        d.released(),
                        Some(ChoiceAction::Inspect {
                            inspection,
                            target_ref
                        }) if *inspection == e.kind && *target_ref == e.target_ref
                    )
                });
            if !inspected.insert((e.kind, e.target_ref.clone())) || !produces {
                return Err(SearchError("evidence_record"));
            }
        }
        Ok(())
    }
}
