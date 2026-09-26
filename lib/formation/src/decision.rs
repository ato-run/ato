//! Stage 5a: an optional DecisionProvider picks the next attempt from a finite
//! set the deterministic core enumerates. It never decides K, never adds a D,
//! placement, argv or source change, and its failure is never a search failure:
//! without a usable choice the search takes the deterministic default.
//!
//! The provider runs on the requester side. The Coordinator only opens a
//! decision point, records the one answer it accepts for it (or the fallback),
//! and reuses that durable record after any restart; no model is asked twice.
use crate::search::{
    CandidateInput, Placement, SearchAction, SearchError, SearchStateV1, available, safe,
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

/// One decision point, as durably recorded. `choices` are the ids offered
/// when it opened; an answer is judged against exactly those.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRecord {
    /// The number of attempts the search had issued when the point opened.
    pub seq: u64,
    pub opened_at_ms: u64,
    pub default_id: String,
    pub choices: Vec<String>,
    pub outcome: Option<DecisionOutcome>,
    pub chosen_id: Option<String>,
}
impl DecisionRecord {
    /// The instant from which only the timeout fallback may settle the point.
    pub fn expires_at_ms(&self, policy: &DecisionPolicy) -> u64 {
        self.opened_at_ms.saturating_add(policy.decision_timeout_ms)
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

/// One offered next attempt: a frozen D on an admissible, untried placement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    pub choice_id: String,
    pub candidate_id: String,
    pub derivation_ref: String,
    pub runtime_id: String,
    pub environment_id: String,
}

/// A stable label for (seq, D, placement): Jev answers with a label, and the
/// same point always yields the same labels.
pub fn choice_id(seq: u64, derivation_ref: &str, runtime_id: &str, environment_id: &str) -> String {
    let canonical = serde_jcs::to_vec(&serde_json::json!({
        "seq": seq,
        "derivation_ref": derivation_ref,
        "runtime_id": runtime_id,
        "environment_id": environment_id,
    }))
    .expect("plain JSON");
    let digest = Sha256::digest(canonical);
    let mut id = String::from("c");
    for byte in &digest[..8] {
        id.push_str(&format!("{byte:02x}"));
    }
    id
}

/// Every attempt the deterministic core could issue now, in frozen D order and
/// then the Coordinator's ranking; the default is always the first.
pub fn allowed_choices(s: &SearchStateV1, placements: &[Placement]) -> Vec<Choice> {
    let seq = s.attempts.len() as u64;
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
                c.derivation_ref == d.derivation_ref
                    && c.runtime_id == p.runtime_id
                    && c.environment_id == p.environment_id
            });
            if tried || duplicate {
                continue;
            }
            out.push(Choice {
                choice_id: choice_id(seq, &d.derivation_ref, &p.runtime_id, &p.environment_id),
                candidate_id: p.candidate_id.clone(),
                derivation_ref: d.derivation_ref.clone(),
                runtime_id: p.runtime_id.clone(),
                environment_id: p.environment_id.clone(),
            });
            if out.len() == MAX_CHOICES {
                return out;
            }
        }
    }
    out
}

fn issue(s: &SearchStateV1, c: &Choice) -> SearchAction {
    let d = s
        .frozen
        .candidates
        .iter()
        .find(|d| d.derivation_ref == c.derivation_ref)
        .expect("choices come from the frozen list");
    match &d.materialization {
        CandidateInput::Source { .. } => SearchAction::IssueAttempt {
            candidate_id: c.candidate_id.clone(),
            derivation_ref: c.derivation_ref.clone(),
            runtime_id: c.runtime_id.clone(),
            environment_id: c.environment_id.clone(),
        },
        CandidateInput::Retained { retained_ref } => SearchAction::ReplayRetained {
            candidate_id: c.candidate_id.clone(),
            derivation_ref: c.derivation_ref.clone(),
            runtime_id: c.runtime_id.clone(),
            environment_id: c.environment_id.clone(),
            retained_ref: retained_ref.clone(),
        },
    }
}

/// The provider's say over a deterministic default. Only an issue is ever
/// replaced, and only by another offered issue of the same point.
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
    let seq = s.attempts.len() as u64;
    let choices = allowed_choices(s, placements);
    match s.decisions.iter().find(|d| d.seq == seq) {
        None => {
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
        Some(record) => match (record.outcome, &record.chosen_id) {
            (Some(DecisionOutcome::Chosen), Some(id)) => choices
                .iter()
                .find(|c| &c.choice_id == id)
                .map(|c| issue(s, c))
                // Offered then, no longer issuable now: the default, not a wait.
                .unwrap_or(default),
            (Some(_), _) => default,
            (None, _) => {
                let expires_at_ms = record.expires_at_ms(policy);
                if now_ms >= expires_at_ms {
                    SearchAction::RecordFallback {
                        seq,
                        reason: DecisionOutcome::Timeout,
                    }
                } else {
                    SearchAction::WaitForDecision { seq, expires_at_ms }
                }
            }
        },
    }
}

/// A requester's answer to an open decision point.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionSubmission {
    pub seq: u64,
    #[serde(default)]
    pub choice_id: Option<String>,
    /// A requester-side failure: `invalid`, `provider_error` or `timeout`.
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
/// Coordinator's trusted `now_ms`. A named choice that was not offered is
/// recorded as `out_of_set` (the default is taken); a point that is already
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
    if record.outcome.is_some() || s.attempts.len() as u64 != record.seq {
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
    match (&submission.choice_id, submission.fallback) {
        (Some(id), None) if record.choices.contains(id) => {
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
        if self.decisions.is_empty() {
            return Ok(());
        }
        let policy = self
            .frozen
            .policy
            .decision
            .as_ref()
            .ok_or(SearchError("decision_without_policy"))?;
        let mut seen = std::collections::BTreeSet::new();
        if self.decisions.len() > 256 {
            return Err(SearchError("decision_bounds"));
        }
        if self.budget.decisions_used > u64::from(policy.max_decisions) {
            return Err(SearchError("decision_budget"));
        }
        for d in &self.decisions {
            let consistent = match d.outcome {
                Some(DecisionOutcome::Chosen) => d
                    .chosen_id
                    .as_ref()
                    .is_some_and(|id| d.choices.contains(id)),
                _ => d.chosen_id.is_none(),
            };
            if !seen.insert(d.seq)
                || d.seq > self.attempts.len() as u64
                || d.choices.len() < 2
                || d.choices.len() > MAX_CHOICES
                || !d.choices.contains(&d.default_id)
                || !consistent
            {
                return Err(SearchError("decision_record"));
            }
        }
        // A decision is taken before its attempt: an attempt that exists past a
        // decision point was issued after it was settled.
        if self
            .decisions
            .iter()
            .any(|d| d.outcome.is_none() && d.seq < self.attempts.len() as u64)
        {
            return Err(SearchError("decision_left_open"));
        }
        Ok(())
    }
}
