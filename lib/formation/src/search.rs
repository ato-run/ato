//! Deterministic known-D exploration. Durable rows are the authority; this
//! bounded value is their versioned read model, not a second mutable store.
//! No clock, database, scheduler, executor or Contract evaluator lives here.
use crate::{
    authoring::BoundContract,
    browser::{BrowserContractV0, effective_contract_ref},
    decision::{
        Choice, ChoiceAction, DecisionOutcome, DecisionPolicy, DecisionRecord,
        InspectionEvidence, InspectionKind,
    },
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
pub const SEARCH_SCHEMA: &str = "ato.formation-search-state/1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrozenSearchV1 {
    pub contract_ref: String,
    pub base_contract_ref: String,
    pub base_contract: BoundContract,
    pub browser_contract: Option<BrowserContractV0>,
    pub policy: SearchPolicy,
    /// Input order is frozen, independently of Runtime availability/ranking.
    pub candidates: Vec<SearchCandidate>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchPolicy {
    pub mode: SearchMode,
    pub runtime_constraint: RuntimeConstraint,
    pub network: String,
    pub allow_managed: bool,
    pub bindings: BTreeMap<String, String>,
    pub budget: BudgetLimits,
    /// Stage 5a: absent means no provider, and the frozen bytes of a search
    /// without one are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<DecisionPolicy>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    FirstPass,
    All,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeConstraint {
    Any,
    Exact {
        runtime_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        environment_id: Option<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetLimits {
    pub max_attempts: u64,
    pub deadline_seconds: u64,
    pub max_transfer_bytes: u64,
    pub max_expanded_bytes: u64,
    pub max_stored_bytes: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchCandidate {
    pub derivation_ref: String,
    pub effects: String,
    pub requirements: Vec<Requirement>,
    pub provisions: Vec<String>,
    pub materialization: CandidateInput,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    pub fact: String,
    /// Absent: the fact only has to be present. Same wire shape as the
    /// Runtime Network requirement a requester computes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub one_of: Option<Vec<String>>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CandidateInput {
    Source {
        closure_ref: String,
        archive_digest: String,
    },
    Retained {
        retained_ref: String,
    },
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetCounters {
    pub attempts_used: u64,
    pub attempts_reserved: u64,
    pub transfer_used: u64,
    pub transfer_reserved: u64,
    pub expanded_used: u64,
    pub expanded_reserved: u64,
    pub stored_used: u64,
    pub stored_reserved: u64,
    /// Decision points opened for a provider, whatever their outcome: the
    /// provider decision budget (always equal to the recorded points).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub decisions_used: u64,
}
fn is_zero(n: &u64) -> bool {
    *n == 0
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchStateV1 {
    pub schema: String,
    pub search_id: String,
    pub owner_scope: String,
    /// Monotonic sequence advanced atomically with authoritative row changes.
    pub revision: u64,
    pub frozen: FrozenSearchV1,
    pub deadline_ms: u64,
    pub budget: BudgetCounters,
    pub attempts: Vec<SearchAttempt>,
    pub owner_stopped: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<DecisionRecord>,
    /// Durable typed evidence produced by chosen inspections (Stage 5a-b):
    /// append-only, write-once per (kind, target).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<InspectionEvidence>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchAttempt {
    pub attempt_id: String,
    pub derivation_ref: String,
    pub runtime_id: String,
    pub environment_id: String,
    pub status: DurableAttemptStatus,
    pub claimed: bool,
    pub record: Option<ExecutionRecord>,
    pub effects: Option<String>,
    pub failure_code: Option<String>,
    /// Only the existing physical-cessation-authorized durable resolution.
    pub unknown_resolved: bool,
    pub retained_ref: Option<String>,
    pub materialization_ref: Option<String>,
    /// Set only after accept_verified_route has accepted this exact attempt.
    pub route_accepted: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurableAttemptStatus {
    Pending,
    Claimed,
    Pass,
    Fail,
    Inconclusive,
    Expired,
    Unknown,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionRecord {
    NotStarted,
    Finished,
    StartedUnfinished,
    HistoryUnavailable,
    BlockedByUnknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    pub candidate_id: String,
    pub derivation_ref: String,
    pub runtime_id: String,
    pub environment_id: String,
    /// Coordinator authorization/capability hard-filter result, not D verdict.
    pub admissible: bool,
    pub transfer_bytes: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Termination {
    Verified,
    CandidatesExhausted,
    BudgetExhausted,
    /// An effect may have happened and its outcome is not known.
    EffectUnknown,
    /// The Runtime refused a D whose attested effect class may not run
    /// unattended, and proved it did not start: nothing happened.
    EffectPolicyRefused,
    /// The requester's DecisionProvider chose the offered stop: a decision,
    /// not a K failure and not an owner stop.
    DecisionStopped,
    OwnerStopped,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SearchAction {
    IssueAttempt {
        candidate_id: String,
        derivation_ref: String,
        runtime_id: String,
        environment_id: String,
    },
    ReplayRetained {
        candidate_id: String,
        derivation_ref: String,
        runtime_id: String,
        environment_id: String,
        retained_ref: String,
    },
    WaitForRuntime {
        derivation_ref: String,
    },
    WaitForAttempt {
        attempt_id: String,
    },
    WaitForUnknownResolution {
        attempt_id: String,
    },
    AcceptPendingRoute {
        attempt_id: String,
    },
    /// Stage 5a: offer `choices` to the requester's provider; `default_id` is
    /// what the search takes without a usable answer.
    OpenDecision {
        seq: u64,
        default_id: String,
        choices: Vec<Choice>,
    },
    WaitForDecision {
        seq: u64,
        expires_at_ms: u64,
    },
    RecordFallback {
        seq: u64,
        reason: DecisionOutcome,
    },
    /// Stage 5a-b: run the recorded bounded read-only inspection and store
    /// its typed evidence. Executed by the Coordinator over durable rows;
    /// never a shell, a network call or a provider-supplied argument.
    RunInspection {
        inspection: InspectionKind,
        target_ref: String,
    },
    Finish {
        reason: Termination,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SearchEvent {
    SearchCreated,
    CandidateAdmitted {
        derivation_ref: String,
    },
    CandidateRefused {
        derivation_ref: String,
    },
    AttemptIssued {
        attempt_id: String,
    },
    AttemptClaimed {
        attempt_id: String,
    },
    AttemptFinished {
        attempt_id: String,
    },
    AttemptUnknown {
        attempt_id: String,
    },
    UnknownResolved {
        attempt_id: String,
    },
    RetainedCandidateAvailable {
        derivation_ref: String,
        retained_ref: String,
    },
    RouteVerified {
        attempt_id: String,
    },
    BudgetExhausted,
    OwnerStopped,
    DecisionOpened {
        seq: u64,
    },
    DecisionMade {
        seq: u64,
        chosen_id: String,
    },
    DecisionFallback {
        seq: u64,
        reason: DecisionOutcome,
    },
    /// The recorded choice was no longer issuable; the default was taken.
    DecisionUnavailableAtIssue {
        seq: u64,
    },
    /// A chosen inspection produced its durable typed evidence.
    InspectionRecorded {
        inspection: InspectionKind,
        target_ref: String,
    },
}
#[derive(Debug, thiserror::Error)]
#[error("invalid search state: {0}")]
pub struct SearchError(pub &'static str);
impl FrozenSearchV1 {
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, SearchError> {
        if self
            .base_contract
            .contract_ref()
            .map_err(|_| SearchError("contract"))?
            != self.base_contract_ref
            || effective_contract_ref(&self.base_contract_ref, self.browser_contract.as_ref())
                != self.contract_ref
        {
            return Err(SearchError("frozen_contract_mismatch"));
        }
        if let Some(policy) = &self.policy.decision {
            policy.validate()?;
        }
        if self.candidates.is_empty() || self.candidates.len() > 64 {
            return Err(SearchError("candidate_count"));
        }
        let mut ids = BTreeSet::new();
        if self
            .candidates
            .iter()
            .any(|c| !ids.insert(&c.derivation_ref))
        {
            return Err(SearchError("duplicate_candidate"));
        }
        serde_jcs::to_vec(self).map_err(|_| SearchError("canonical"))
    }
}
impl SearchStateV1 {
    pub fn validate(&self) -> Result<(), SearchError> {
        if self.schema != SEARCH_SCHEMA
            || self.search_id.is_empty()
            || self.owner_scope.is_empty()
            || self.attempts.len() > 256
        {
            return Err(SearchError("schema_or_bounds"));
        }
        self.frozen.canonical_bytes()?;
        let mut ids = BTreeSet::new();
        for a in &self.attempts {
            if !ids.insert(&a.attempt_id)
                || !self
                    .frozen
                    .candidates
                    .iter()
                    .any(|d| d.derivation_ref == a.derivation_ref)
                || (a.unknown_resolved && a.status != DurableAttemptStatus::Unknown)
                || (a.route_accepted && a.status != DurableAttemptStatus::Pass)
            {
                return Err(SearchError("attempt_identity_or_state"));
            }
        }
        self.validate_decisions()
    }
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, SearchError> {
        self.validate()?;
        serde_jcs::to_vec(self).map_err(|_| SearchError("canonical"))
    }
}
pub(crate) fn safe(effects: &str) -> bool {
    matches!(effects, "pure" | "idempotent" | "record-substitutable")
}
fn finish(reason: Termination) -> SearchAction {
    SearchAction::Finish { reason }
}
pub(crate) fn available(max: u64, used: u64, reserved: u64) -> u64 {
    max.saturating_sub(used.saturating_add(reserved))
}
/// A deterministic decision over a revisioned snapshot. Its action is a proposal:
/// storage must CAS against this revision and recheck assignment/budget fences.
/// With a frozen decision policy, an issue may be replaced by the provider's
/// recorded choice among the same finite set (`decision::apply`).
pub fn decide_next(
    s: &SearchStateV1,
    placements: &[Placement],
    now_ms: u64,
) -> Result<SearchAction, SearchError> {
    let default = default_next(s, placements, now_ms)?;
    Ok(crate::decision::apply(s, placements, now_ms, default))
}
fn default_next(
    s: &SearchStateV1,
    placements: &[Placement],
    now_ms: u64,
) -> Result<SearchAction, SearchError> {
    s.validate()?;
    if placements.len() > 4096 {
        return Err(SearchError("placement_count"));
    }
    if s.owner_stopped {
        return Ok(finish(Termination::OwnerStopped));
    }
    if let Some(a) = s
        .attempts
        .iter()
        .find(|a| a.status == DurableAttemptStatus::Unknown && !a.unknown_resolved)
    {
        return Ok(SearchAction::WaitForUnknownResolution {
            attempt_id: a.attempt_id.clone(),
        });
    }
    if let Some(a) = s.attempts.iter().find(|a| {
        matches!(
            a.status,
            DurableAttemptStatus::Pending | DurableAttemptStatus::Claimed
        )
    }) {
        return Ok(SearchAction::WaitForAttempt {
            attempt_id: a.attempt_id.clone(),
        });
    }
    if let Some(a) = s
        .attempts
        .iter()
        .find(|a| a.status == DurableAttemptStatus::Pass && !a.route_accepted)
    {
        return Ok(SearchAction::AcceptPendingRoute {
            attempt_id: a.attempt_id.clone(),
        });
    }
    let passed = s.attempts.iter().any(|a| a.route_accepted);
    if passed && s.frozen.policy.mode == SearchMode::FirstPass {
        return Ok(finish(Termination::Verified));
    }
    if let Some(last) = s.attempts.last()
        && last.status != DurableAttemptStatus::Pass
    {
        if last.failure_code.as_deref().is_some_and(|c| {
            matches!(
                c,
                "search_transfer_budget_exceeded"
                    | "search_expanded_budget_exceeded"
                    | "search_stored_budget_exceeded"
            )
        }) {
            return Ok(finish(if passed {
                Termination::Verified
            } else {
                Termination::BudgetExhausted
            }));
        }
        // Policy refusal proven before start is not effect uncertainty.
        if !last.unknown_resolved
            && last.effects.as_deref().is_some_and(|e| !safe(e))
            && (!last.claimed || last.record == Some(ExecutionRecord::NotStarted))
        {
            return Ok(finish(Termination::EffectPolicyRefused));
        }
        let can_continue = last.unknown_resolved
            || ((last.effects.as_deref().is_none_or(safe))
                && (!last.claimed
                    || last.record == Some(ExecutionRecord::NotStarted)
                    || (last.record == Some(ExecutionRecord::Finished)
                        && last.effects.as_deref().is_some_and(safe))));
        if !can_continue {
            return Ok(finish(Termination::EffectUnknown));
        }
        if matches!(
            s.frozen.policy.runtime_constraint,
            RuntimeConstraint::Exact { .. }
        ) {
            return Ok(finish(if passed {
                Termination::Verified
            } else {
                Termination::CandidatesExhausted
            }));
        }
    }
    let b = &s.budget;
    let l = &s.frozen.policy.budget;
    if now_ms >= s.deadline_ms
        || available(l.max_attempts, b.attempts_used, b.attempts_reserved) == 0
        || available(l.max_expanded_bytes, b.expanded_used, b.expanded_reserved) == 0
    {
        return Ok(finish(if passed {
            Termination::Verified
        } else {
            Termination::BudgetExhausted
        }));
    }
    for d in &s.frozen.candidates {
        let history: Vec<_> = s
            .attempts
            .iter()
            .filter(|a| a.derivation_ref == d.derivation_ref)
            .collect();
        if !safe(&d.effects) {
            continue;
        }
        let next = placements.iter().find(|p| {
            p.derivation_ref == d.derivation_ref
                && p.admissible
                && !history
                    .iter()
                    .any(|a| a.runtime_id == p.runtime_id && a.environment_id == p.environment_id)
        });
        if let Some(p) = next {
            if p.transfer_bytes
                > available(l.max_transfer_bytes, b.transfer_used, b.transfer_reserved)
            {
                return Ok(finish(if passed {
                    Termination::Verified
                } else {
                    Termination::BudgetExhausted
                }));
            }
            return Ok(match &d.materialization {
                CandidateInput::Source { .. } => SearchAction::IssueAttempt {
                    candidate_id: p.candidate_id.clone(),
                    derivation_ref: d.derivation_ref.clone(),
                    runtime_id: p.runtime_id.clone(),
                    environment_id: p.environment_id.clone(),
                },
                CandidateInput::Retained { retained_ref } => SearchAction::ReplayRetained {
                    candidate_id: p.candidate_id.clone(),
                    derivation_ref: d.derivation_ref.clone(),
                    runtime_id: p.runtime_id.clone(),
                    environment_id: p.environment_id.clone(),
                    retained_ref: retained_ref.clone(),
                },
            });
        }
        // A missing Runtime is not a failed Derivation, even after restart.
        if history.is_empty() {
            return Ok(SearchAction::WaitForRuntime {
                derivation_ref: d.derivation_ref.clone(),
            });
        }
    }
    Ok(finish(if passed {
        Termination::Verified
    } else {
        Termination::CandidatesExhausted
    }))
}
/// Typed projection of durable facts for auditing; not a second event store.
pub fn events(s: &SearchStateV1, action: &SearchAction) -> Vec<SearchEvent> {
    let mut events = attempt_events(s, action);
    for d in &s.decisions {
        events.push(SearchEvent::DecisionOpened { seq: d.seq });
        match d.outcome {
            Some(DecisionOutcome::Chosen) => {
                if let Some(id) = &d.chosen_id {
                    events.push(SearchEvent::DecisionMade {
                        seq: d.seq,
                        chosen_id: id.clone(),
                    });
                }
                // The recorded attempt never landed and the frontier moved on
                // without it (a substitute was issued, or another issue is
                // being released now): the default was taken.
                if let Some(ChoiceAction::Attempt {
                    derivation_ref,
                    runtime_id,
                    environment_id,
                    ..
                }) = d.released()
                {
                    let landed = s.attempts.iter().any(|a| {
                        a.derivation_ref == *derivation_ref
                            && a.runtime_id == *runtime_id
                            && a.environment_id == *environment_id
                    });
                    let issuing_other = matches!(
                        action,
                        SearchAction::IssueAttempt {
                            derivation_ref: d2,
                            runtime_id: r2,
                            environment_id: e2,
                            ..
                        } | SearchAction::ReplayRetained {
                            derivation_ref: d2,
                            runtime_id: r2,
                            environment_id: e2,
                            ..
                        } if *d2 != *derivation_ref
                            || *r2 != *runtime_id
                            || *e2 != *environment_id
                    );
                    if !landed
                        && (issuing_other || s.attempts.len() as u64 > d.attempt_seq)
                    {
                        events.push(SearchEvent::DecisionUnavailableAtIssue { seq: d.seq });
                    }
                }
            }
            Some(reason) => events.push(SearchEvent::DecisionFallback {
                seq: d.seq,
                reason,
            }),
            None => {}
        }
    }
    for e in &s.evidence {
        events.push(SearchEvent::InspectionRecorded {
            inspection: e.kind,
            target_ref: e.target_ref.clone(),
        });
    }
    events
}
fn attempt_events(s: &SearchStateV1, action: &SearchAction) -> Vec<SearchEvent> {
    let mut events = vec![SearchEvent::SearchCreated];
    for d in &s.frozen.candidates {
        events.push(if safe(&d.effects) {
            SearchEvent::CandidateAdmitted {
                derivation_ref: d.derivation_ref.clone(),
            }
        } else {
            SearchEvent::CandidateRefused {
                derivation_ref: d.derivation_ref.clone(),
            }
        });
        if let CandidateInput::Retained { retained_ref } = &d.materialization {
            events.push(SearchEvent::RetainedCandidateAvailable {
                derivation_ref: d.derivation_ref.clone(),
                retained_ref: retained_ref.clone(),
            });
        }
    }
    for a in &s.attempts {
        let id = || a.attempt_id.clone();
        events.push(SearchEvent::AttemptIssued { attempt_id: id() });
        if a.claimed {
            events.push(SearchEvent::AttemptClaimed { attempt_id: id() });
        }
        match a.status {
            DurableAttemptStatus::Unknown => {
                events.push(SearchEvent::AttemptUnknown { attempt_id: id() });
                if a.unknown_resolved {
                    events.push(SearchEvent::UnknownResolved { attempt_id: id() });
                }
            }
            DurableAttemptStatus::Pass
            | DurableAttemptStatus::Fail
            | DurableAttemptStatus::Inconclusive => {
                events.push(SearchEvent::AttemptFinished { attempt_id: id() })
            }
            _ => {}
        }
        if a.route_accepted {
            events.push(SearchEvent::RouteVerified { attempt_id: id() });
        }
    }
    if matches!(
        action,
        SearchAction::Finish {
            reason: Termination::BudgetExhausted
        }
    ) {
        events.push(SearchEvent::BudgetExhausted);
    }
    if s.owner_stopped {
        events.push(SearchEvent::OwnerStopped);
    }
    events
}
