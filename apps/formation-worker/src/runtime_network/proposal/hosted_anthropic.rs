//! Anthropic candidate producer through an authenticated managed API proxy.
//! The proxy holds the provider key. Source and model output remain untrusted;
//! Coordinator/Rust validation and real Run receipts are still authoritative.
use super::{
    GeneralCandidateProducer,
    budget::{
        BudgetPlan, FinishReason, ProviderBudget, RequestEvidence, ResponseEvidence,
        TransmittedContextEvidence,
    },
    deepseek::{GeneralFailure, GeneralOutput, PROMPT_V3, PROMPT_VERSION_V3},
    hosted_budget::HostedProviderBudget,
    hosted_budget_http::{HostedJournalHttp, HostedJournalScope},
    provenance::{ErrorClass, ProviderIdentity},
};
use anyhow::{Result, ensure};
use ato_formation::proposal::{
    CandidateProducer, MAX_BATCH_BYTES, ProducerOutput, ProducerProvenance, ProposalRequestV2,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostedAnthropicConfig {
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub prompt_version: String,
    pub max_output_tokens: u64,
    pub timeout_ms: u64,
}
impl HostedAnthropicConfig {
    fn validate(&self, plan: &BudgetPlan) -> Result<()> {
        plan.validate()?;
        ensure!(
            ((self.provider == "anthropic"
                && self.model == "claude-sonnet-4-6"
                && self.endpoint == "https://api.anthropic.com")
                || (self.provider == "openrouter"
                    && self.model == "anthropic/claude-sonnet-4.6"
                    && self.endpoint == "https://openrouter.ai"
                    && plan.input_price >= 6_300_000
                    && plan.output_price >= 15_750_000))
                && self.prompt_version == PROMPT_VERSION_V3
                && (1..=2048).contains(&self.max_output_tokens)
                && self.max_output_tokens <= plan.output_token_cap
                && (1..=30000).contains(&self.timeout_ms)
                && plan.input_price >= 3_000_000
                && plan.output_price >= 15_000_000,
            "invalid hosted Anthropic configuration"
        );
        Ok(())
    }
    pub fn configuration_ref(&self, plan: &BudgetPlan) -> Result<String> {
        self.validate(plan)?;
        ato_formation::provider_budget::candidate_configuration_ref(
            self,
            plan,
            &format!("sha256:{:x}", Sha256::digest(PROMPT_V3.as_bytes())),
        )
    }
    fn identity(&self) -> ProviderIdentity {
        ProviderIdentity {
            provider: self.provider.clone(),
            model: self.model.clone(),
            prompt_version: self.prompt_version.clone(),
        }
    }
}

/// The implementation must be scoped to an authenticated managed worker,
/// owner/Search, driver lease and proposal round. It must never retry a send.
pub trait HostedInferenceTransport: Send + Sync {
    fn send(&self, cell: &str, round: u32, timeout_ms: u64, body: &[u8]) -> Result<Vec<u8>>;
}
pub struct HostedAnthropicCandidateProducer {
    config: HostedAnthropicConfig,
    budget: Arc<dyn ProviderBudget>,
    transport: Arc<dyn HostedInferenceTransport>,
}
impl HostedAnthropicCandidateProducer {
    pub fn new(
        config: HostedAnthropicConfig,
        budget: Arc<dyn ProviderBudget>,
        transport: Arc<dyn HostedInferenceTransport>,
    ) -> Result<Self> {
        config.validate(budget.plan())?;
        Ok(Self {
            config,
            budget,
            transport,
        })
    }
    fn call(
        &self,
        request: &ProposalRequestV2,
        cell: &str,
        round: u32,
        provenance: &mut super::provenance::Provenance,
    ) -> std::result::Result<ProducerOutput, ErrorClass> {
        let mut projected = request.clone();
        projected.remaining_budget.timeout_ms = projected
            .remaining_budget
            .timeout_ms
            .min(self.config.timeout_ms);
        let timeout = projected.remaining_budget.timeout_ms;
        if timeout == 0 {
            return Err(ErrorClass::Timeout);
        }
        let proposal =
            serde_jcs::to_string(&projected).map_err(|_| ErrorClass::MalformedResponse)?;
        if (PROMPT_V3.len() + proposal.len() + 1024) as u64 > self.budget.plan().input_token_cap {
            return Err(ErrorClass::MalformedResponse);
        }
        let mut provider_body = json!({"model":self.config.model,"max_tokens":self.config.max_output_tokens,
            "system":PROMPT_V3,"messages":[{"role":"user","content":proposal}],"stream":false});
        if self.config.provider == "openrouter" {
            provider_body["provider"] = json!({"only":["anthropic"],"allow_fallbacks":false,
                "max_price":{"prompt":3,"completion":15}});
        }
        let body = serde_json::to_vec(&provider_body).map_err(|_| ErrorClass::MalformedResponse)?;
        if body.len() > 65536 {
            return Err(ErrorClass::MalformedResponse);
        }
        let evidence = RequestEvidence {
            cell: cell.into(),
            proposal_request_sha256: format!("sha256:{:x}", Sha256::digest(proposal.as_bytes())),
            provider_body_sha256: format!("sha256:{:x}", Sha256::digest(&body)),
            timeout_ms: timeout,
            proposal_request_bytes: proposal.len() as u64,
            provider_body_bytes: body.len() as u64,
            transmitted_context: Some(TransmittedContextEvidence::from_request(&projected)),
        };
        self.budget
            .reserve_request(evidence)
            .map_err(|_| ErrorClass::ProviderRefused)?;
        let started = Instant::now();
        let response = self
            .transport
            .send(cell, round, timeout, &body)
            .map_err(|_| ErrorClass::TransportError)?;
        if response.len() > 128 * 1024 {
            return Err(ErrorClass::ResponseTooLarge);
        }
        let envelope: Value =
            serde_json::from_slice(&response).map_err(|_| ErrorClass::MalformedResponse)?;
        let content = envelope["content"]
            .as_array()
            .filter(|v| v.len() == 1)
            .ok_or(ErrorClass::MalformedResponse)?;
        if content[0]["type"] != "text" {
            return Err(ErrorClass::MalformedResponse);
        }
        let text = content[0]["text"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= MAX_BATCH_BYTES)
            .ok_or(ErrorClass::MalformedResponse)?;
        let input_tokens = envelope["usage"]["input_tokens"]
            .as_u64()
            .ok_or(ErrorClass::MalformedResponse)?;
        let output_tokens = envelope["usage"]["output_tokens"]
            .as_u64()
            .ok_or(ErrorClass::MalformedResponse)?;
        // This adapter never requests prompt caching; unexpected cache accounting
        // cannot be silently priced as ordinary input.
        if ["cache_creation_input_tokens", "cache_read_input_tokens"]
            .iter()
            .any(|key| {
                !envelope["usage"][*key].is_null() && envelope["usage"][*key].as_u64() != Some(0)
            })
        {
            return Err(ErrorClass::MalformedResponse);
        }
        let observed = ResponseEvidence {
            cell: cell.into(),
            finish_reason: match envelope["stop_reason"].as_str() {
                Some("end_turn") => FinishReason::Stop,
                Some("max_tokens") => FinishReason::Length,
                _ => FinishReason::Other,
            },
            model_matches: envelope["model"] == self.config.model,
            input_tokens,
            output_tokens,
            latency_ms: Some(started.elapsed().as_millis().min(u64::MAX as u128) as u64),
        };
        // Usage from a different model cannot be safely priced with this
        // pinned model's rate; leave it unresolved for the full UNKNOWN charge.
        if !observed.model_matches {
            return Err(ErrorClass::MalformedResponse);
        }
        if !observed.within(self.budget.plan()) {
            self.budget
                .record_response(observed)
                .map_err(|_| ErrorClass::TransportError)?;
            return Err(ErrorClass::MalformedResponse);
        }
        let output = ProducerOutput::new(
            text.as_bytes().to_vec(),
            ProducerProvenance {
                provider: self.config.provider.clone(),
                model: Some(self.config.model.clone()),
            },
        )
        .map_err(|_| ErrorClass::MalformedResponse)?;
        provenance.usage.input_tokens = Some(input_tokens);
        provenance.usage.output_tokens = Some(output_tokens);
        provenance.estimated_cost_usd_micros =
            self.budget.plan().cost(input_tokens, output_tokens).ok();
        provenance.latency_ms = observed.latency_ms;
        self.budget
            .record_response(observed)
            .map_err(|_| ErrorClass::TransportError)?;
        Ok(output)
    }
}
impl CandidateProducer<ProposalRequestV2, GeneralOutput, GeneralFailure>
    for HostedAnthropicCandidateProducer
{
    fn propose(
        &self,
        request: &ProposalRequestV2,
    ) -> std::result::Result<GeneralOutput, GeneralFailure> {
        let mut provenance = self.config.identity().unknown_usage();
        let Some(round) = request.round_seq else {
            return Err(GeneralFailure {
                class: ErrorClass::MalformedResponse,
                provenance,
            });
        };
        if round == 0 {
            return Err(GeneralFailure {
                class: ErrorClass::MalformedResponse,
                provenance,
            });
        }
        let cell = format!("{}_r{round}", request.search_id);
        match self.call(request, &cell, round, &mut provenance) {
            Ok(output) => Ok(GeneralOutput { output, provenance }),
            Err(class) => {
                if self
                    .budget
                    .snapshot()
                    .ok()
                    .is_some_and(|s| s.cells.get(&cell).is_some_and(|c| !c.is_settled()))
                {
                    let _ = self.budget.charge_unknown(&cell);
                }
                let _ = self.budget.halt();
                Err(GeneralFailure { class, provenance })
            }
        }
    }
}
impl GeneralCandidateProducer for HostedAnthropicCandidateProducer {
    fn configuration_ref(&self) -> Result<String> {
        self.config.configuration_ref(self.budget.plan())
    }
    fn identity(&self) -> ProviderIdentity {
        self.config.identity()
    }
}

/// The Coordinator creates the round fence as part of claim. Consequently a
/// hosted provider cannot be constructed with its final journal capability
/// before the claim response. This factory binds exactly once, after that
/// response is checked, and is discarded before another round.
pub struct ClaimBoundAnthropicProducer {
    config: HostedAnthropicConfig,
    plan: BudgetPlan,
    api: String,
    token: String,
    owner: String,
    search: String,
    holder: String,
    claimant: String,
    generation: u64,
    bound: Mutex<Option<Arc<HostedAnthropicCandidateProducer>>>,
    #[cfg(test)]
    mock: bool,
}
pub struct ClaimBoundScope<'a> {
    pub api: &'a str,
    pub token: &'a str,
    pub owner: &'a str,
    pub search: &'a str,
    pub holder: &'a str,
    pub claimant: &'a str,
    pub generation: u64,
}
impl ClaimBoundAnthropicProducer {
    pub fn new(
        config: HostedAnthropicConfig,
        plan: BudgetPlan,
        scope: ClaimBoundScope<'_>,
    ) -> Result<Self> {
        config.validate(&plan)?;
        Ok(Self {
            config,
            plan,
            api: scope.api.into(),
            token: scope.token.into(),
            owner: scope.owner.into(),
            search: scope.search.into(),
            holder: scope.holder.into(),
            claimant: scope.claimant.into(),
            generation: scope.generation,
            bound: Mutex::new(None),
            #[cfg(test)]
            mock: false,
        })
    }
    #[cfg(test)]
    pub(crate) fn for_test(
        config: HostedAnthropicConfig,
        plan: BudgetPlan,
        scope: ClaimBoundScope<'_>,
    ) -> Result<Self> {
        let mut producer = Self::new(config, plan, scope)?;
        producer.mock = true;
        Ok(producer)
    }
}
impl GeneralCandidateProducer for ClaimBoundAnthropicProducer {
    fn configuration_ref(&self) -> Result<String> {
        self.config.configuration_ref(&self.plan)
    }
    fn identity(&self) -> ProviderIdentity {
        self.config.identity()
    }
    fn bind_claim(&self, round: u64, fence: &str) -> Result<()> {
        let mut bound = self
            .bound
            .lock()
            .map_err(|_| anyhow::anyhow!("hosted producer lock unavailable"))?;
        ensure!(bound.is_none(), "hosted producer already bound");
        let scope = || HostedJournalScope {
            owner: &self.owner,
            search: &self.search,
            holder: &self.holder,
            claimant: &self.claimant,
            generation: self.generation,
            round,
            round_fence: fence,
        };
        #[cfg(test)]
        let transport = Arc::new(if self.mock {
            HostedJournalHttp::for_test(&self.api, &self.token, scope())?
        } else {
            HostedJournalHttp::new(&self.api, &self.token, scope())?
        });
        #[cfg(not(test))]
        let transport = Arc::new(HostedJournalHttp::new(&self.api, &self.token, scope())?);
        let budget = Arc::new(HostedProviderBudget::new(
            self.plan.clone(),
            transport.clone(),
        )?);
        *bound = Some(Arc::new(HostedAnthropicCandidateProducer::new(
            self.config.clone(),
            budget,
            transport,
        )?));
        Ok(())
    }
}
impl CandidateProducer<ProposalRequestV2, GeneralOutput, GeneralFailure>
    for ClaimBoundAnthropicProducer
{
    fn propose(
        &self,
        request: &ProposalRequestV2,
    ) -> std::result::Result<GeneralOutput, GeneralFailure> {
        let bound = self.bound.lock().ok().and_then(|guard| guard.clone());
        match bound {
            Some(producer) => producer.propose(request),
            None => Err(GeneralFailure {
                class: ErrorClass::ProviderRefused,
                provenance: self.config.identity().unknown_usage(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_network::proposal::budget::CallBudget;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    struct Transport {
        calls: AtomicUsize,
        body: Mutex<Vec<u8>>,
        response: Option<Vec<u8>>,
    }
    impl HostedInferenceTransport for Transport {
        fn send(&self, _cell: &str, _round: u32, _timeout_ms: u64, body: &[u8]) -> Result<Vec<u8>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.body.lock().unwrap() = body.to_vec();
            self.response
                .clone()
                .ok_or_else(|| anyhow::anyhow!("simulated connection loss"))
        }
    }
    fn config() -> HostedAnthropicConfig {
        HostedAnthropicConfig {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            endpoint: "https://api.anthropic.com".into(),
            prompt_version: PROMPT_VERSION_V3.into(),
            max_output_tokens: 256,
            timeout_ms: 1000,
        }
    }
    fn plan() -> BudgetPlan {
        BudgetPlan {
            max_calls: 1,
            input_token_cap: 32768,
            output_token_cap: 256,
            input_price: 3_000_000,
            output_price: 15_000_000,
            ceiling_usd_micros: 102144,
        }
    }
    fn request() -> ProposalRequestV2 {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../../tools/receipt-authority/fixtures/hosted-provider-request.json"
        ))
        .unwrap();
        serde_json::from_value(fixture["request"].clone()).unwrap()
    }
    fn response() -> Vec<u8> {
        serde_json::to_vec(&json!({"model":"claude-sonnet-4-6","stop_reason":"end_turn",
        "content":[{"type":"text","text":"{\"schema\":\"ato.formation-proposal/1\",\"proposals\":[]}"}],
        "usage":{"input_tokens":20,"output_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}})).unwrap()
    }
    #[test]
    fn sends_once_with_reserved_body_and_reports_actual_provider_usage() {
        let temp = tempfile::tempdir().unwrap();
        let budget =
            Arc::new(CallBudget::create(&temp.path().join("budget.jsonl"), plan()).unwrap());
        let transport = Arc::new(Transport {
            calls: AtomicUsize::new(0),
            body: Mutex::new(vec![]),
            response: Some(response()),
        });
        let producer =
            HostedAnthropicCandidateProducer::new(config(), budget.clone(), transport.clone())
                .unwrap();
        assert_eq!(
            producer.configuration_ref().unwrap(),
            "sha256:dae2f41abadfab12170807d1d82591e7290483b2de3a726e98425a9515686c61"
        );
        let output = producer.propose(&request()).unwrap();
        assert_eq!(output.provenance.provider, "anthropic");
        assert_eq!(output.provenance.usage.input_tokens, Some(20));
        assert_eq!(output.provenance.usage.output_tokens, Some(10));
        assert_eq!(output.provenance.estimated_cost_usd_micros, Some(210));
        let payload: Value = serde_json::from_slice(&transport.body.lock().unwrap()).unwrap();
        let projected: ProposalRequestV2 =
            serde_json::from_str(payload["messages"][0]["content"].as_str().unwrap()).unwrap();
        assert_eq!(projected.remaining_budget.timeout_ms, 1000);
        let snapshot = budget.snapshot().unwrap();
        assert!(snapshot.cells["source-search_r1"].response.is_some());
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        assert!(producer.propose(&request()).is_err());
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn router_request_pins_price_upstream_and_accounts_for_surcharge() {
        let mut p = plan();
        p.input_price = 6_300_000;
        p.output_price = 15_750_000;
        p.ceiling_usd_micros = 210_471;
        let mut c = config();
        c.provider = "openrouter".into();
        c.model = "anthropic/claude-sonnet-4.6".into();
        c.endpoint = "https://openrouter.ai".into();
        assert!(c.configuration_ref(&plan()).is_err());
        let temp = tempfile::tempdir().unwrap();
        let budget = Arc::new(CallBudget::create(&temp.path().join("budget.jsonl"), p).unwrap());
        let mut reply: Value = serde_json::from_slice(&response()).unwrap();
        reply["model"] = json!("anthropic/claude-sonnet-4.6");
        let transport = Arc::new(Transport {
            calls: AtomicUsize::new(0),
            body: Mutex::new(vec![]),
            response: Some(serde_json::to_vec(&reply).unwrap()),
        });
        let producer = HostedAnthropicCandidateProducer::new(c, budget, transport.clone()).unwrap();
        let output = producer.propose(&request()).unwrap();
        assert_eq!(output.provenance.provider, "openrouter");
        let payload: Value = serde_json::from_slice(&transport.body.lock().unwrap()).unwrap();
        assert_eq!(
            payload["provider"],
            json!({"only":["anthropic"],"allow_fallbacks":false,
            "max_price":{"prompt":3,"completion":15}})
        );
        assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn lost_or_invalid_response_is_charged_unknown_and_never_retried() {
        let mut wrong_model: Value = serde_json::from_slice(&response()).unwrap();
        wrong_model["model"] = json!("different-model");
        let mut invalid_cache: Value = serde_json::from_slice(&response()).unwrap();
        invalid_cache["usage"]["cache_creation_input_tokens"] = json!("unknown");
        for reply in [
            None,
            Some(b"{}".to_vec()),
            Some(serde_json::to_vec(&wrong_model).unwrap()),
            Some(serde_json::to_vec(&invalid_cache).unwrap()),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let budget =
                Arc::new(CallBudget::create(&temp.path().join("budget.jsonl"), plan()).unwrap());
            let transport = Arc::new(Transport {
                calls: AtomicUsize::new(0),
                body: Mutex::new(vec![]),
                response: reply,
            });
            let producer =
                HostedAnthropicCandidateProducer::new(config(), budget.clone(), transport.clone())
                    .unwrap();
            assert!(producer.propose(&request()).is_err());
            let snapshot = budget.snapshot().unwrap();
            assert!(snapshot.cells["source-search_r1"].charged_unknown);
            assert!(snapshot.stopped);
            assert!(producer.propose(&request()).is_err());
            assert_eq!(transport.calls.load(Ordering::SeqCst), 1);
        }
    }
    #[test]
    fn configuration_rejects_underpriced_or_untrusted_provider() {
        let mut p = plan();
        p.output_price = 1;
        assert!(config().configuration_ref(&p).is_err());
        let mut c = config();
        c.endpoint = "https://example.com".into();
        assert!(c.configuration_ref(&plan()).is_err());
    }
    #[test]
    fn managed_producer_is_bound_only_after_one_valid_claim() {
        let producer = ClaimBoundAnthropicProducer::new(
            config(),
            plan(),
            ClaimBoundScope {
                api: "https://example.com",
                token: "worker-token",
                owner: "owner_a",
                search: "source-search",
                holder: "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa",
                claimant: "bbbbbbbb-bbbb-4bbb-bbbb-bbbbbbbbbbbb",
                generation: 1,
            },
        )
        .unwrap();
        assert!(producer.propose(&request()).is_err());
        assert!(producer.bind_claim(0, "bad").is_err());
        assert!(
            producer
                .bind_claim(1, "cccccccc-cccc-4ccc-cccc-cccccccccccc")
                .is_ok()
        );
        assert!(
            producer
                .bind_claim(1, "cccccccc-cccc-4ccc-cccc-cccccccccccc")
                .is_err()
        );
    }
}
