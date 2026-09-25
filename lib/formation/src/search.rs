//! Deterministic known-D exploration. Durable rows are the authority; this
//! bounded value is their versioned read model, not a second mutable store.
//! No clock, database, scheduler, executor or Contract evaluator lives here.
use crate::{
    authoring::BoundContract,
    browser::{BrowserContractV0, effective_contract_ref},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
    pub one_of: Vec<String>,
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
    pub receipts: Vec<Value>,
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
    EffectUnknown,
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
        Ok(())
    }
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, SearchError> {
        self.validate()?;
        serde_jcs::to_vec(self).map_err(|_| SearchError("canonical"))
    }
}
fn safe(effects: &str) -> bool {
    matches!(effects, "pure" | "idempotent" | "record-substitutable")
}
fn finish(reason: Termination) -> SearchAction {
    SearchAction::Finish { reason }
}
fn available(max: u64, used: u64, reserved: u64) -> u64 {
    max.saturating_sub(used.saturating_add(reserved))
}
/// A deterministic decision over a revisioned snapshot. Its action is a proposal:
/// storage must CAS against this revision and recheck assignment/budget fences.
pub fn decide_next(
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
        if history.iter().any(|a| a.route_accepted) {
            continue;
        }
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
