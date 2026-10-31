//! Provider spend-plan validation shared by local and hosted authorities.
//! These limits are operational; they are never Capsule or Contract identity.
use crate::exploration::ExplorationPolicy;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const AUTHORIZED_USD_MICROS: u64 = 5_000_000;

/// Content-address the public candidate-provider configuration and spend plan.
/// The caller validates the configuration and prompt; this does not grant a
/// call or accept an untrusted prompt as a known version.
pub fn candidate_configuration_ref<T: Serialize>(
    config: &T,
    plan: &BudgetPlan,
    prompt_digest: &str,
) -> Result<String> {
    ensure!(
        crate::generation::is_sha256(prompt_digest),
        "invalid prompt digest"
    );
    plan.validate()?;
    let canonical = serde_jcs::to_vec(&(config, plan, prompt_digest))?;
    ensure!(
        canonical.len() <= 16 * 1024,
        "provider configuration bounds"
    );
    Ok(format!("sha256:{:x}", Sha256::digest(canonical)))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetPlan {
    pub max_calls: u32,
    pub input_token_cap: u64,
    pub output_token_cap: u64,
    /// Explicit peak/cache-miss price snapshot, micros per million tokens.
    pub input_price: u64,
    pub output_price: u64,
    pub ceiling_usd_micros: u64,
}

impl BudgetPlan {
    pub fn cost(&self, input: u64, output: u64) -> Result<u64> {
        let input = (u128::from(input) * u128::from(self.input_price)).div_ceil(1_000_000);
        let output = (u128::from(output) * u128::from(self.output_price)).div_ceil(1_000_000);
        Ok(u64::try_from(
            input
                .checked_add(output)
                .ok_or_else(|| anyhow::anyhow!("cost overflow"))?,
        )?)
    }

    pub fn validate(&self) -> Result<u64> {
        ensure!(
            (1..=64).contains(&self.max_calls)
                && self.input_token_cap > 0
                && (1..=if self.output_price == 0 { 65536 } else { 2048 })
                    .contains(&self.output_token_cap)
                && self.input_price > 0
                && self.ceiling_usd_micros <= AUTHORIZED_USD_MICROS,
            "invalid spend plan"
        );
        let per_call = self.cost(self.input_token_cap, self.output_token_cap)?;
        ensure!(
            per_call
                .checked_mul(u64::from(self.max_calls))
                .is_some_and(|n| n <= self.ceiling_usd_micros),
            "spend ceiling exceeded"
        );
        Ok(per_call)
    }
}

/// Validate a candidate and optional decision provider together against the
/// already-frozen Search ceilings. This validates limits only: callers must
/// separately bind each plan to its validated provider configuration.
pub fn validate_combined(plans: &[BudgetPlan], exploration: &ExplorationPolicy) -> Result<()> {
    ensure!(
        (1..=2).contains(&plans.len()),
        "invalid provider plan count"
    );
    exploration
        .validate()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let mut calls = 0_u64;
    let mut input = 0_u64;
    let mut output = 0_u64;
    let mut cost = 0_u64;
    for plan in plans {
        let count = u64::from(plan.max_calls);
        let reserved = plan.validate()?;
        calls = calls
            .checked_add(count)
            .ok_or_else(|| anyhow::anyhow!("calls overflow"))?;
        input = input
            .checked_add(
                plan.input_token_cap
                    .checked_mul(count)
                    .ok_or_else(|| anyhow::anyhow!("token overflow"))?,
            )
            .ok_or_else(|| anyhow::anyhow!("token overflow"))?;
        output = output
            .checked_add(
                plan.output_token_cap
                    .checked_mul(count)
                    .ok_or_else(|| anyhow::anyhow!("token overflow"))?,
            )
            .ok_or_else(|| anyhow::anyhow!("token overflow"))?;
        cost = cost
            .checked_add(
                reserved
                    .checked_mul(count)
                    .ok_or_else(|| anyhow::anyhow!("spend overflow"))?,
            )
            .ok_or_else(|| anyhow::anyhow!("spend overflow"))?;
    }
    ensure!(
        calls <= u64::from(exploration.max_provider_calls)
            && input <= exploration.max_provider_input_tokens
            && output <= exploration.max_provider_output_tokens
            && cost <= exploration.max_provider_cost_usd_micros,
        "combined provider spend plan exceeds frozen exploration budget"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn costs_round_up_and_enforce_the_full_reserved_plan() {
        let mut plan = BudgetPlan {
            max_calls: 6,
            input_token_cap: 262_144,
            output_token_cap: 2048,
            input_price: 300_000,
            output_price: 1_200_000,
            ceiling_usd_micros: 5_000_000,
        };
        assert_eq!(plan.validate().unwrap(), 81_102);
        plan.ceiling_usd_micros = 486_611;
        assert!(plan.validate().is_err());
        plan.ceiling_usd_micros = 5_000_000;
        plan.max_calls = 65;
        assert!(plan.validate().is_err());
    }

    #[test]
    fn configuration_binding_preserves_the_existing_canonical_wire() {
        let plan = BudgetPlan {
            max_calls: 1,
            input_token_cap: 100,
            output_token_cap: 20,
            input_price: 1,
            output_price: 1,
            ceiling_usd_micros: 2,
        };
        let config = serde_json::json!({"provider":"deepseek","model":"pinned"});
        let digest = format!("sha256:{}", "c".repeat(64));
        let prior = format!(
            "sha256:{:x}",
            Sha256::digest(serde_jcs::to_vec(&(&config, &plan, digest.clone())).unwrap())
        );
        assert_eq!(
            candidate_configuration_ref(&config, &plan, &digest).unwrap(),
            prior
        );
    }

    #[test]
    fn combined_plans_cannot_exceed_any_frozen_search_ceiling() {
        let plan = BudgetPlan {
            max_calls: 2,
            input_token_cap: 100,
            output_token_cap: 20,
            input_price: 1,
            output_price: 1,
            ceiling_usd_micros: 4,
        };
        let policy: ExplorationPolicy = serde_json::from_value(serde_json::json!({
            "ceiling":{}, "max_provider_calls":4, "max_inspections":0,
            "max_provider_cost_usd_micros":8, "max_provider_input_tokens":400,
            "max_provider_output_tokens":80
        }))
        .unwrap();
        assert!(validate_combined(&[plan.clone(), plan.clone()], &policy).is_ok());
        let mut limited = policy.clone();
        limited.max_provider_calls -= 1;
        assert!(validate_combined(&[plan.clone(), plan.clone()], &limited).is_err());
        limited = policy.clone();
        limited.max_provider_input_tokens -= 1;
        assert!(validate_combined(&[plan.clone(), plan.clone()], &limited).is_err());
        limited = policy.clone();
        limited.max_provider_output_tokens -= 1;
        assert!(validate_combined(&[plan.clone(), plan.clone()], &limited).is_err());
        limited = policy.clone();
        limited.max_provider_cost_usd_micros -= 1;
        assert!(validate_combined(&[plan.clone(), plan], &limited).is_err());
        assert!(validate_combined(&[], &policy).is_err());
    }

    #[test]
    fn hosted_replay_never_retries_an_unsettled_or_unknown_call() {
        let plan = BudgetPlan {
            max_calls: 2,
            input_token_cap: 100,
            output_token_cap: 20,
            input_price: 1,
            output_price: 1,
            ceiling_usd_micros: 4,
        };
        let request = |cell: &str| JournalEvent::Request {
            request: RequestEvidence {
                cell: cell.into(),
                proposal_request_sha256: format!("sha256:{}", "a".repeat(64)),
                provider_body_sha256: format!("sha256:{}", "b".repeat(64)),
                timeout_ms: 1000,
                proposal_request_bytes: 1,
                provider_body_bytes: 1,
                transmitted_context: None,
            },
        };
        let first = request("call_1");
        assert!(replay_hosted_budget(&plan, &[], Some(first.clone())).is_ok());
        assert!(
            replay_hosted_budget(&plan, std::slice::from_ref(&first), Some(request("call_2")))
                .is_err()
        );
        let unknown = JournalEvent::ChargedUnknown {
            charged_unknown: "call_1".into(),
        };
        let state =
            replay_hosted_budget(&plan, std::slice::from_ref(&first), Some(unknown.clone()))
                .unwrap();
        assert!(state.stopped);
        assert!(
            replay_hosted_budget(&plan, &[first.clone(), unknown], Some(request("call_2")))
                .is_err()
        );
        let response = JournalEvent::Response {
            response: ResponseEvidence {
                cell: "call_1".into(),
                finish_reason: FinishReason::Stop,
                model_matches: true,
                input_tokens: 10,
                output_tokens: 5,
                latency_ms: None,
            },
        };
        assert!(replay_hosted_budget(&plan, &[first, response], Some(request("call_2"))).is_ok());
        assert!(
            replay_hosted_budget(&plan, &[], Some(JournalEvent::Cell("legacy".into()))).is_err()
        );
    }

    #[test]
    fn hosted_snapshot_decoding_rejects_unknown_fields() {
        let snapshot = BudgetSnapshot::default();
        let value = serde_json::to_value(&snapshot).unwrap();
        let decoded: BudgetSnapshot = serde_json::from_value(value.clone()).unwrap();
        assert!(decoded.cells.is_empty());
        assert!(!decoded.stopped);
        let mut invalid = value;
        invalid["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<BudgetSnapshot>(invalid).is_err());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    ConnectBeforeSend,
    ProviderUnavailable,
    ProviderAuthentication,
    ProviderConfiguration,
    TransportError,
    ProviderRefused,
    Timeout,
    MalformedResponse,
    ResponseTooLarge,
}

/// Exact transmitted request digests plus optional safe projection metadata.
/// Never persist source text, logs, environment values or credential headers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestEvidence {
    pub cell: String,
    pub proposal_request_sha256: String,
    pub provider_body_sha256: String,
    /// Final provider-visible request timeout, after Requester completion reserve.
    pub timeout_ms: u64,
    pub proposal_request_bytes: u64,
    pub provider_body_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transmitted_context: Option<TransmittedContextEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransmittedContextEvidence {
    pub source_entries: Vec<TransmittedSourceEvidence>,
    pub failure_codes: Vec<String>,
    pub previous_derivation_refs: Vec<String>,
    pub proposal_diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransmittedSourceEvidence {
    pub logical_id: String,
    pub content_sha256: String,
    pub text_bytes: u64,
    pub truncated: bool,
}

impl TransmittedContextEvidence {
    pub fn from_request(request: &crate::proposal::ProposalRequestV2) -> Self {
        let context = request.exploration_context.as_ref();
        Self {
            source_entries: request
                .source_context
                .iter()
                .map(|e| TransmittedSourceEvidence {
                    logical_id: e.logical_id.clone(),
                    content_sha256: e.content_sha256.clone(),
                    text_bytes: e.text.len() as u64,
                    truncated: e.truncated,
                })
                .collect(),
            failure_codes: context
                .map(|c| c.failures.iter().map(|f| f.code.clone()).collect())
                .unwrap_or_default(),
            previous_derivation_refs: context
                .map(|c| {
                    c.previous_derivations
                        .iter()
                        .filter_map(|d| d.derivation_ref().ok())
                        .collect()
                })
                .unwrap_or_default(),
            proposal_diagnostics: context
                .map(|c| c.proposal_diagnostics.clone())
                .unwrap_or_default(),
        }
    }
}
impl RequestEvidence {
    fn validate(&self) -> Result<()> {
        let valid_digest = |hash: &str| {
            hash.strip_prefix("sha256:").is_some_and(|s| {
                s.len() == 64
                    && s.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        };
        for hash in [&self.proposal_request_sha256, &self.provider_body_sha256] {
            ensure!(valid_digest(hash), "invalid request digest");
        }
        ensure!(
            (1..=30_000).contains(&self.timeout_ms)
                && self.proposal_request_bytes > 0
                && self.provider_body_bytes > 0,
            "invalid request evidence"
        );
        if let Some(context) = &self.transmitted_context {
            let identifier = |s: &str| {
                !s.is_empty()
                    && s.len() <= 96
                    && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            };
            ensure!(
                context.source_entries.len() <= 2048
                    && context
                        .source_entries
                        .iter()
                        .all(|e| identifier(&e.logical_id)
                            && valid_digest(&e.content_sha256)
                            && e.text_bytes <= crate::proposal::MAX_SOURCE_ENTRY_BYTES as u64)
                    && context
                        .source_entries
                        .iter()
                        .map(|e| e.text_bytes)
                        .sum::<u64>()
                        <= crate::proposal::MAX_SOURCE_BYTES as u64
                    && context.failure_codes.len() <= 4
                    && context.failure_codes.iter().all(|s| identifier(s))
                    && context.previous_derivation_refs.len() <= 3
                    && context
                        .previous_derivation_refs
                        .iter()
                        .all(|s| valid_digest(s))
                    && context.proposal_diagnostics.len() <= 4
                    && context.proposal_diagnostics.iter().all(|s| identifier(s)),
                "invalid transmitted context evidence"
            );
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct RequestInspection {
    #[serde(flatten)]
    pub request: RequestEvidence,
    pub response_resolved: bool,
}
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetSnapshot {
    pub cells: BTreeMap<String, CallEvidence>,
    pub stopped: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallEvidence {
    pub transport: Option<TransportEvidence>,
    pub not_sent: bool,
    /// None only for a historical Cell reservation. Not acceptable for D3 v2.
    pub request: Option<RequestEvidence>,
    pub response: Option<ResponseEvidence>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub charged_unknown: bool,
}
impl CallEvidence {
    pub fn is_settled(&self) -> bool {
        self.response.is_some() || self.charged_unknown || self.not_sent
    }
}
impl BudgetSnapshot {
    fn reserve(
        &mut self,
        cell: String,
        request: Option<RequestEvidence>,
        plan: &BudgetPlan,
    ) -> Result<()> {
        ensure!(!self.stopped, "provider protocol violation: run stopped");
        ensure!(
            self.cells.values().all(CallEvidence::is_settled),
            "prior call response unresolved"
        );
        ensure!(
            !cell.is_empty()
                && cell.len() <= 128
                && cell
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
            "invalid cell"
        );
        ensure!(
            !self.cells.contains_key(&cell) && self.cells.len() < plan.max_calls as usize,
            "cell already consumed or call budget exhausted"
        );
        self.cells.insert(
            cell,
            CallEvidence {
                request,
                response: None,
                charged_unknown: false,
                transport: None,
                not_sent: false,
            },
        );
        Ok(())
    }
    pub fn apply(
        &mut self,
        event: JournalEvent,
        plan: &BudgetPlan,
        historical_read: bool,
    ) -> Result<()> {
        match event {
            JournalEvent::Cell(cell) => self.reserve(cell, None, plan)?,
            JournalEvent::Request { request } => {
                request.validate()?;
                self.reserve(request.cell.clone(), Some(request), plan)?;
            }
            JournalEvent::Transport { transport } => {
                let call = self
                    .cells
                    .get_mut(&transport.cell)
                    .ok_or_else(|| anyhow::anyhow!("provider reservation missing"))?;
                ensure!(call.transport.is_none(), "provider transport write once");
                call.transport = Some(transport);
            }
            JournalEvent::RetryCharge {
                retry_charge,
                not_sent,
            } => {
                let call = self
                    .cells
                    .get_mut(&retry_charge)
                    .ok_or_else(|| anyhow::anyhow!("provider reservation missing"))?;
                ensure!(!call.is_settled(), "provider accounting already settled");
                ensure!(
                    !not_sent || call.transport.as_ref().is_some_and(|t| !t.possibly_sent),
                    "unproved unsent call"
                );
                call.not_sent = not_sent;
                call.charged_unknown = !not_sent;
            }
            JournalEvent::Halt { halt } => {
                ensure!(halt, "invalid halt");
                self.stopped = true;
            }
            JournalEvent::ChargedUnknown { charged_unknown } => {
                let call = self
                    .cells
                    .get_mut(&charged_unknown)
                    .ok_or_else(|| anyhow::anyhow!("provider reservation missing"))?;
                ensure!(!call.is_settled(), "provider accounting already settled");
                call.charged_unknown = true;
                self.stopped = true;
            }
            JournalEvent::Response { response } => {
                let call = self
                    .cells
                    .get_mut(&response.cell)
                    .ok_or_else(|| anyhow::anyhow!("unclaimed provider response"))?;
                // Existing Cell/Response journals remain readable. New responses
                // require atomic Request reservations; legacy writes cannot opt in.
                ensure!(
                    historical_read || call.request.is_some(),
                    "response without request evidence"
                );
                ensure!(!call.is_settled(), "duplicate provider response");
                self.stopped |= !response.within(plan);
                call.response = Some(response);
            }
        }
        Ok(())
    }
}

/// Only closed finish categories, usage and a model-match boolean are stored;
/// never arbitrary vendor strings, reasoning, headers or raw error bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    Other,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseEvidence {
    pub cell: String,
    pub finish_reason: FinishReason,
    pub model_matches: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
}
impl ResponseEvidence {
    pub fn within(&self, plan: &BudgetPlan) -> bool {
        self.finish_reason == FinishReason::Stop
            && self.model_matches
            && self.input_tokens <= plan.input_token_cap
            && self.output_tokens <= plan.output_token_cap
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum JournalEvent {
    Transport {
        transport: TransportEvidence,
    },
    RetryCharge {
        retry_charge: String,
        not_sent: bool,
    },
    Cell(String),
    Request {
        request: RequestEvidence,
    },
    Halt {
        halt: bool,
    },
    ChargedUnknown {
        charged_unknown: String,
    },
    Response {
        response: ResponseEvidence,
    },
}

/// Revalidate a hosted journal and one proposed append with the same state
/// machine as the local file ledger. The storage layer must compare-and-swap
/// the validated history and all relevant owner/lease/round fences atomically;
/// this pure result alone never authorizes a provider call.
pub fn replay_hosted_budget(
    plan: &BudgetPlan,
    history: &[JournalEvent],
    next: Option<JournalEvent>,
) -> Result<BudgetSnapshot> {
    plan.validate()?;
    ensure!(history.len() <= 512, "provider journal bounds");
    ensure!(
        serde_json::to_vec(&(plan, history, &next))?.len() <= 64 * 1024,
        "provider journal bounds"
    );
    let mut state = BudgetSnapshot::default();
    for event in history {
        ensure!(
            !matches!(event, JournalEvent::Cell(_)),
            "legacy reservation unsupported"
        );
        state.apply(event.clone(), plan, true)?;
    }
    if let Some(event) = next {
        ensure!(
            !matches!(event, JournalEvent::Cell(_)),
            "legacy reservation unsupported"
        );
        state.apply(event, plan, false)?;
    }
    Ok(state)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportEvidence {
    pub cell: String,
    pub possibly_sent: bool,
    pub http_status: Option<u16>,
    pub retry_after_ms: u64,
    pub recorded_at_ms: u64,
    pub error_class: ErrorClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
}
