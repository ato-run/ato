//! Frozen external exploration limits; no grant or approval is attached to D.
use crate::authoring::BoundDerivation;
use crate::verify::ContractVerificationReceipt;
use crate::{requirements::ExecutionRequirements, search::SearchError};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;

fn default_rounds() -> NonZeroU32 {
    NonZeroU32::MIN.saturating_add(2)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FormationConfig {
    #[serde(default = "default_rounds")]
    pub max_rounds: NonZeroU32,
}
impl Default for FormationConfig {
    fn default() -> Self {
        Self {
            max_rounds: default_rounds(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplorationPolicy {
    /// Effective config captured before execution, never re-read on restart.
    #[serde(default)]
    pub formation: FormationConfig,
    pub ceiling: ExecutionRequirements,
    pub max_provider_calls: u32,
    pub max_inspections: u32,
    /// Opt-in shared reasoning protocol. Legacy one-call rounds retain their
    /// original timeout and wire bytes; inspection exchanges are not D rounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningLimits>,
    pub max_provider_cost_usd_micros: u64,
    pub max_provider_input_tokens: u64,
    pub max_provider_output_tokens: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub max_network_transfer_bytes: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub max_network_transfer_bytes_per_attempt: u64,
    /// Canonical operational provider config + prompt + spend-plan digest.
    /// Frozen once, distinct from K/D and never a credential value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_configuration_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_provider_configuration_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_budget_binding_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningLimits {
    /// Owner-authored acceptance protocol, frozen before search; grants no rights.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    pub round_timeout_ms: u64,
    pub inspection_timeout_ms: u64,
    pub inspection_source_bytes: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

impl ExplorationPolicy {
    pub fn validate(&self) -> Result<(), SearchError> {
        self.ceiling.validate().map_err(|e| SearchError(e.0))?;
        if self.reasoning.as_ref().is_some_and(|r| {
            !(1..=900_000).contains(&r.round_timeout_ms)
                || !(1..=r.round_timeout_ms).contains(&r.inspection_timeout_ms)
                || !(1..=64 * 1024).contains(&r.inspection_source_bytes)
                || r.goal.as_ref().is_some_and(|g| {
                    g.is_empty() || g.len() > 512 || g.chars().any(char::is_control)
                })
        }) {
            return Err(SearchError("reasoning_budget_invalid"));
        }
        if self.max_provider_calls == 0
            || self.max_provider_cost_usd_micros == 0
            || self.max_provider_input_tokens == 0
            || self.max_provider_output_tokens == 0
            || self.max_network_transfer_bytes_per_attempt > self.max_network_transfer_bytes
            || (!self.ceiling.network.is_empty()
                && self.max_network_transfer_bytes_per_attempt == 0)
        {
            return Err(SearchError("exploration_budget_invalid"));
        }
        if [
            &self.provider_configuration_ref,
            &self.decision_provider_configuration_ref,
            &self.provider_budget_binding_ref,
        ]
        .iter()
        .any(|ref_| {
            ref_.as_ref()
                .is_some_and(|r| !crate::generation::is_sha256(r))
        }) {
            return Err(SearchError("exploration_provider_configuration_invalid"));
        }
        Ok(())
    }

    pub fn round_timeout_ms(&self, legacy: u64) -> u64 {
        self.reasoning
            .as_ref()
            .map_or(legacy, |r| r.round_timeout_ms)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionStatus {
    KReachedAwaitingAssessment,
}

/// A successful search submission, not permission to Run, publish or deploy.
/// The authenticated Runtime receipt and exact D are retained together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExplorationSubmission {
    pub status: SubmissionStatus,
    pub contract: crate::authoring::BoundContract,
    pub contract_ref: String,
    pub derivation: BoundDerivation,
    pub derivation_ref: String,
    pub attempt_id: String,
    pub receipt: ContractVerificationReceipt,
}

impl ExplorationSubmission {
    pub fn validate(&self, state: &crate::search::SearchStateV1) -> Result<(), SearchError> {
        let policy = state
            .frozen
            .policy
            .exploration
            .as_ref()
            .ok_or(SearchError("submission_without_exploration"))?;
        self.derivation
            .requirements
            .within(&policy.ceiling)
            .map_err(|e| SearchError(e.0))?;
        if self.contract != state.frozen.base_contract
            || self.contract_ref != state.frozen.contract_ref
        {
            return Err(SearchError("submission_contract_mismatch"));
        }
        if self
            .derivation
            .derivation_ref()
            .map_err(|_| SearchError("submission_derivation_invalid"))?
            != self.derivation_ref
            || !state.attempts.iter().any(|a| {
                a.attempt_id == self.attempt_id
                    && a.derivation_ref == self.derivation_ref
                    && a.route_accepted
                    && a.status == crate::search::DurableAttemptStatus::Pass
            })
            || !self.receipt.fully_satisfied
        {
            return Err(SearchError("submission_attempt_mismatch"));
        }
        crate::receipt::accept_receipt(
            &crate::receipt::ReceiptAssignment {
                contract: &state.frozen.base_contract,
                contract_ref: &state.frozen.contract_ref,
                derivation_ref: &self.derivation_ref,
                attempt_id: &self.attempt_id,
                request_id: None,
            },
            &self.receipt,
        )
        .map_err(|_| SearchError("submission_receipt_invalid"))
    }

    /// Only requirements may be reduced; unchanged executable semantics.
    pub fn admits_reduction(&self, candidate: &BoundDerivation) -> Result<(), SearchError> {
        candidate
            .requirements
            .within(&self.derivation.requirements)
            .map_err(|_| SearchError("reduction_expands_requirements"))?;
        if candidate.requirements == self.derivation.requirements {
            return Err(SearchError("no_progress"));
        }
        let mut compared = candidate.clone();
        compared.requirements = self.derivation.requirements.clone();
        if compared != self.derivation {
            return Err(SearchError("reduction_changes_execution"));
        }
        Ok(())
    }
}
