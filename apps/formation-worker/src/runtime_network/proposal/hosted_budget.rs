//! Adapter for a Coordinator-owned provider ledger. The transport is a narrow
//! capability: it must use authenticated, owner/search/lease/round-bound
//! requests, canonical event identities and the shared Rust journal authority.
//! This module does not itself mint credentials or expose an HTTP route.
use super::budget::{
    BudgetPlan, BudgetSnapshot, JournalEvent, ProviderBudget, RequestEvidence, ResponseEvidence,
    TransportEvidence, accounting_from_snapshot,
};
use anyhow::{Result, ensure};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    Appended,
    AlreadyPresent,
}

pub trait HostedJournalTransport: Send + Sync {
    /// A transport error is ambiguous; implementations must not retry a provider
    /// call. The same operation must reconcile as AlreadyPresent if committed.
    fn append(&self, event: &JournalEvent) -> Result<AppendOutcome>;
    fn snapshot(&self) -> Result<BudgetSnapshot>;
}

pub struct HostedProviderBudget {
    plan: BudgetPlan,
    transport: Arc<dyn HostedJournalTransport>,
}
impl HostedProviderBudget {
    pub fn new(plan: BudgetPlan, transport: Arc<dyn HostedJournalTransport>) -> Result<Self> {
        plan.validate()?;
        Ok(Self { plan, transport })
    }
    fn evidence(&self, event: JournalEvent) -> Result<()> {
        self.transport.append(&event)?;
        Ok(())
    }
}
impl ProviderBudget for HostedProviderBudget {
    fn plan(&self) -> &BudgetPlan {
        &self.plan
    }
    fn reserve_request(&self, request: RequestEvidence) -> Result<()> {
        // A previously committed reservation does not authorize a second send,
        // even when the prior append response was lost before the first send.
        ensure!(
            self.transport.append(&JournalEvent::Request { request })? == AppendOutcome::Appended,
            "hosted reservation already exists; provider call must not be sent"
        );
        Ok(())
    }
    fn record_response(&self, response: ResponseEvidence) -> Result<()> {
        self.evidence(JournalEvent::Response { response })
    }
    fn record_transport(&self, transport: TransportEvidence) -> Result<()> {
        self.evidence(JournalEvent::Transport { transport })
    }
    fn charge_unknown(&self, cell: &str) -> Result<()> {
        self.evidence(JournalEvent::ChargedUnknown {
            charged_unknown: cell.into(),
        })
    }
    fn settle_retry(&self, cell: &str, not_sent: bool) -> Result<()> {
        self.evidence(JournalEvent::RetryCharge {
            retry_charge: cell.into(),
            not_sent,
        })
    }
    fn halt(&self) -> Result<()> {
        self.evidence(JournalEvent::Halt { halt: true })
    }
    fn snapshot(&self) -> Result<BudgetSnapshot> {
        self.transport.snapshot()
    }
    fn accounting(&self) -> Result<serde_json::Value> {
        accounting_from_snapshot(&self.plan, &self.snapshot()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ato_formation::provider_budget::replay_hosted_budget;
    use std::sync::Mutex;

    struct Ledger {
        plan: BudgetPlan,
        events: Mutex<Vec<JournalEvent>>,
        lose_first_reply: Mutex<bool>,
    }
    impl HostedJournalTransport for Ledger {
        fn append(&self, event: &JournalEvent) -> Result<AppendOutcome> {
            let mut events = self.events.lock().unwrap();
            let bytes = serde_jcs::to_vec(event)?;
            if events
                .iter()
                .any(|old| serde_jcs::to_vec(old).unwrap() == bytes)
            {
                return Ok(AppendOutcome::AlreadyPresent);
            }
            replay_hosted_budget(&self.plan, &events, Some(event.clone()))?;
            events.push(event.clone());
            let mut lost = self.lose_first_reply.lock().unwrap();
            if *lost {
                *lost = false;
                anyhow::bail!("response lost after commit");
            }
            Ok(AppendOutcome::Appended)
        }
        fn snapshot(&self) -> Result<BudgetSnapshot> {
            replay_hosted_budget(&self.plan, &self.events.lock().unwrap(), None)
        }
    }
    fn plan() -> BudgetPlan {
        BudgetPlan {
            max_calls: 2,
            input_token_cap: 100,
            output_token_cap: 20,
            input_price: 1,
            output_price: 1,
            ceiling_usd_micros: 4,
        }
    }
    fn request(cell: &str) -> RequestEvidence {
        RequestEvidence {
            cell: cell.into(),
            proposal_request_sha256: format!("sha256:{}", "a".repeat(64)),
            provider_body_sha256: format!("sha256:{}", "b".repeat(64)),
            timeout_ms: 1000,
            proposal_request_bytes: 1,
            provider_body_bytes: 1,
            transmitted_context: None,
        }
    }
    #[test]
    fn lost_reservation_reply_never_authorizes_a_send_or_another_call() {
        let journal = Arc::new(Ledger {
            plan: plan(),
            events: Mutex::new(vec![]),
            lose_first_reply: Mutex::new(true),
        });
        let budget = HostedProviderBudget::new(plan(), journal).unwrap();
        assert!(budget.reserve_request(request("call_1")).is_err());
        assert!(budget.reserve_request(request("call_1")).is_err());
        assert!(budget.reserve_request(request("call_2")).is_err());
        assert_eq!(
            budget.accounting().unwrap()["calls"][0]["status"],
            "unsettled"
        );
        budget.charge_unknown("call_1").unwrap();
        assert!(budget.reserve_request(request("call_2")).is_err());
        assert_eq!(
            budget.accounting().unwrap()["calls"][0]["status"],
            "charged_unknown"
        );
    }
    #[test]
    fn repeated_evidence_is_safe_but_repeated_reservation_is_not() {
        let journal = Arc::new(Ledger {
            plan: plan(),
            events: Mutex::new(vec![]),
            lose_first_reply: Mutex::new(false),
        });
        let budget = HostedProviderBudget::new(plan(), journal).unwrap();
        budget.reserve_request(request("call_1")).unwrap();
        assert!(budget.reserve_request(request("call_1")).is_err());
        budget.charge_unknown("call_1").unwrap();
        budget.charge_unknown("call_1").unwrap();
        assert!(budget.snapshot().unwrap().stopped);
    }
}
