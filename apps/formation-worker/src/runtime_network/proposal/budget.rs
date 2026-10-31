//! Durable provider dollar reservation journal, separate from execution budgets.
//! A reservation is consumed before credentials are read. Never refunded on
//! timeout/unknown usage. Incomplete writes fail closed; no implicit reset.
use anyhow::{Result, ensure};
pub use ato_formation::provider_budget::{AUTHORIZED_USD_MICROS, BudgetPlan};
use fs2::FileExt;
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub struct CallBudget {
    path: PathBuf,
    plan: BudgetPlan,
}

/// Transport-independent provider ledger. Implementations must reserve before
/// credentials or network use, persist UNKNOWN without refund, and retain the
/// same frozen plan across restarts. A hosted implementation additionally
/// requires its Coordinator and driver fences at every committing operation.
pub trait ProviderBudget: Send + Sync {
    fn plan(&self) -> &BudgetPlan;
    fn reserve_request(&self, request: RequestEvidence) -> Result<()>;
    fn record_response(&self, response: ResponseEvidence) -> Result<()>;
    fn record_transport(&self, transport: TransportEvidence) -> Result<()>;
    fn charge_unknown(&self, cell: &str) -> Result<()>;
    fn settle_retry(&self, cell: &str, not_sent: bool) -> Result<()>;
    fn halt(&self) -> Result<()>;
    fn snapshot(&self) -> Result<BudgetSnapshot>;
    fn accounting(&self) -> Result<serde_json::Value>;
}

impl ProviderBudget for CallBudget {
    fn plan(&self) -> &BudgetPlan {
        CallBudget::plan(self)
    }
    fn reserve_request(&self, request: RequestEvidence) -> Result<()> {
        CallBudget::reserve_request(self, request)
    }
    fn record_response(&self, response: ResponseEvidence) -> Result<()> {
        CallBudget::record_response(self, response)
    }
    fn record_transport(&self, transport: TransportEvidence) -> Result<()> {
        CallBudget::record_transport(self, transport)
    }
    fn charge_unknown(&self, cell: &str) -> Result<()> {
        CallBudget::charge_unknown(self, cell)
    }
    fn settle_retry(&self, cell: &str, not_sent: bool) -> Result<()> {
        CallBudget::settle_retry(self, cell, not_sent)
    }
    fn halt(&self) -> Result<()> {
        CallBudget::halt(self)
    }
    fn snapshot(&self) -> Result<BudgetSnapshot> {
        CallBudget::snapshot(self)
    }
    fn accounting(&self) -> Result<serde_json::Value> {
        CallBudget::accounting(self)
    }
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

pub use ato_formation::provider_budget::{
    BudgetSnapshot, CallEvidence, FinishReason, JournalEvent, RequestEvidence, RequestInspection,
    ResponseEvidence, TransmittedContextEvidence, TransmittedSourceEvidence, TransportEvidence,
};
