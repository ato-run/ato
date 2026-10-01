//! Durable provider dollar reservation journal, separate from execution budgets.
//! A reservation is consumed before credentials are read. Never refunded on
//! timeout/unknown usage. Incomplete writes fail closed; no implicit reset.
use anyhow::{Result, ensure};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const AUTHORIZED_USD_MICROS: u64 = 5_000_000;
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
pub struct CallBudget {
    path: PathBuf,
    plan: BudgetPlan,
}
impl CallBudget {
    /// Explicit initialization only. Existing journals are never overwritten.
    pub fn create(path: &Path, plan: BudgetPlan) -> Result<Self> {
        plan.validate()?;
        let mut f = OpenOptions::new().create_new(true).write(true).open(path)?;
        writeln!(f, "{}", serde_json::to_string(&plan)?)?;
        f.sync_all()?;
        Ok(Self {
            path: path.into(),
            plan,
        })
    }
    pub fn reopen(path: &Path, plan: BudgetPlan) -> Result<Self> {
        plan.validate()?;
        ensure!(path.is_file(), "reservation journal missing");
        let guard = Self {
            path: path.into(),
            plan,
        };
        guard.snapshot()?;
        Ok(guard)
    }
    pub fn plan(&self) -> &BudgetPlan {
        &self.plan
    }
    /// Historical reservation API. New model calls must use reserve_request.
    pub fn reserve(&self, cell: &str) -> Result<()> {
        self.transact(Some(JournalEvent::Cell(cell.into())))
            .map(|_| ())
    }
    /// Evidence IS the reservation: one locked append + fsync, before credentials.
    pub fn reserve_request(&self, request: RequestEvidence) -> Result<()> {
        self.transact(Some(JournalEvent::Request { request }))
            .map(|_| ())
    }
    /// Stop is operational and persistent across requester restarts.
    pub fn halt(&self) -> Result<()> {
        self.transact(Some(JournalEvent::Halt { halt: true }))
            .map(|_| ())
    }
    pub fn record_response(&self, response: ResponseEvidence) -> Result<()> {
        self.transact(Some(JournalEvent::Response { response }))
            .map(|_| ())
    }
    /// Final accounting of a provider call whose usage cannot be established.
    /// Charge the full reservation, halt this search's provider channel and
    /// never retry it. This does not resolve workload effect UNKNOWN.
    pub fn charge_unknown(&self, cell: &str) -> Result<()> {
        self.transact(Some(JournalEvent::ChargedUnknown {
            charged_unknown: cell.into(),
        }))
        .map(|_| ())
    }
    /// Controllers consume this validated view, never reinterpret journal JSON.
    pub fn record_transport(&self, transport: TransportEvidence) -> Result<()> {
        self.transact(Some(JournalEvent::Transport { transport }))
            .map(|_| ())
    }
    pub fn settle_retry(&self, cell: &str, not_sent: bool) -> Result<()> {
        self.transact(Some(JournalEvent::RetryCharge {
            retry_charge: cell.into(),
            not_sent,
        }))
        .map(|_| ())
    }
    pub fn snapshot(&self) -> Result<BudgetSnapshot> {
        self.transact(None)
    }
    /// One row per actual transport reservation, including sent calls whose
    /// response disappeared. Never substitute a reasoning exchange or round.
    pub fn accounting(&self) -> Result<serde_json::Value> {
        let snapshot = self.snapshot()?;
        let reservation = self
            .plan
            .cost(self.plan.input_token_cap, self.plan.output_token_cap)?;
        let mut spent = 0u64;
        let mut unsettled = 0u64;
        let mut calls = Vec::new();
        for (id, call) in &snapshot.cells {
            let (status, cost) = if let Some(response) = &call.response {
                (
                    "responded",
                    self.plan
                        .cost(response.input_tokens, response.output_tokens)?,
                )
            } else if call.not_sent {
                ("not_sent", 0)
            } else if call.charged_unknown {
                ("charged_unknown", reservation)
            } else {
                unsettled = unsettled.saturating_add(reservation);
                ("unsettled", 0)
            };
            spent = spent.saturating_add(cost);
            calls.push(serde_json::json!({"call_id":id,"status":status,
                "usage":call.response.as_ref().map(|r| serde_json::json!({
                    "input_tokens":r.input_tokens,"output_tokens":r.output_tokens})),
                "latency_ms":call.response.as_ref().and_then(|r|r.latency_ms)
                    .or_else(||call.transport.as_ref().and_then(|t|t.latency_ms)),
                "estimated_cost_usd_micros":cost,
                "unsettled_reservation_usd_micros":if call.is_settled(){0}else{reservation},
                "request":call.request,"transport":call.transport}));
        }
        Ok(
            serde_json::json!({"schema":"ato.provider-call-accounting/1", "calls":calls,
            "reserved_call_slots":snapshot.cells.len(),
            "remaining_call_slots":self.plan.max_calls.saturating_sub(snapshot.cells.len() as u32),
            "estimated_cost_usd_micros":spent,"unsettled_reservation_usd_micros":unsettled,
            "remaining_budget_usd_micros":self.plan.ceiling_usd_micros.saturating_sub(spent.saturating_add(unsettled)),
            "plan":self.plan,"stopped":snapshot.stopped}),
        )
    }
    pub fn inspect_request(&self, cell: &str) -> Result<RequestInspection> {
        let state = self.snapshot()?;
        let saved = state
            .cells
            .get(cell)
            .ok_or_else(|| anyhow::anyhow!("request absent"))?;
        Ok(RequestInspection {
            request: saved
                .request
                .clone()
                .ok_or_else(|| anyhow::anyhow!("historical reservation has no request evidence"))?,
            response_resolved: saved.is_settled(),
        })
    }
    fn transact(&self, event: Option<JournalEvent>) -> Result<BudgetSnapshot> {
        self.plan.validate()?;
        let mut f = OpenOptions::new()
            .read(true)
            .append(true)
            .open(&self.path)?;
        f.lock_exclusive()?;
        let mut bytes = String::new();
        Read::by_ref(&mut f)
            .take(64 * 1024 + 1)
            .read_to_string(&mut bytes)?;
        ensure!(
            bytes.len() <= 64 * 1024 && bytes.ends_with('\n'),
            "corrupt reservation journal"
        );
        let mut lines = bytes.lines();
        let saved: BudgetPlan = serde_json::from_str(
            lines
                .next()
                .ok_or_else(|| anyhow::anyhow!("missing plan"))?,
        )?;
        ensure!(saved == self.plan, "reservation plan changed");
        let mut state = BudgetSnapshot::default();
        for line in lines {
            state.apply(serde_json::from_str(line)?, &self.plan, true)?;
        }
        if let Some(event) = event {
            let encoded = serde_json::to_string(&event)?;
            state.apply(event, &self.plan, false)?;
            ensure!(bytes.len() + encoded.len() < 64 * 1024, "journal full");
            writeln!(f, "{encoded}")?;
            f.sync_all()?;
        }
        Ok(state) // File drop releases cross-process lock, including every error.
    }
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
    pub(super) fn from_request(request: &ato_formation::proposal::ProposalRequestV2) -> Self {
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
                            && e.text_bytes
                                <= ato_formation::proposal::MAX_SOURCE_ENTRY_BYTES as u64)
                    && context
                        .source_entries
                        .iter()
                        .map(|e| e.text_bytes)
                        .sum::<u64>()
                        <= ato_formation::proposal::MAX_SOURCE_BYTES as u64
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
#[derive(Debug, Default, Serialize)]
pub struct BudgetSnapshot {
    pub cells: BTreeMap<String, CallEvidence>,
    pub stopped: bool,
}
#[derive(Debug, Serialize)]
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
    fn apply(
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
#[derive(Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum JournalEvent {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportEvidence {
    pub cell: String,
    pub possibly_sent: bool,
    pub http_status: Option<u16>,
    pub retry_after_ms: u64,
    pub recorded_at_ms: u64,
    pub error_class: super::provenance::ErrorClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
}
