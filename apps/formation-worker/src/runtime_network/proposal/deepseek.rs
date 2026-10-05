//! Requester-only, one HTTP send. No repair, fallback, SDK parsing authority,
//! reasoning persistence or external credentials in a ProposalRequest.
use super::{
    budget::{CallBudget, FinishReason, RequestEvidence, ResponseEvidence},
    provenance::*,
};
use anyhow::{Context, Result, ensure};
use ato_formation::proposal::{
    CandidateProducer, MAX_BATCH_BYTES, ProducerOutput, ProducerProvenance, ProposalRequestV2,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const PROMPT_VERSION: &str = "ato.formation-candidate-producer-prompt/1";
pub const PROMPT: &str = include_str!("prompt.txt");
/// Adds `node_static_workspace@1`. Version 1 bytes stay pinned for its
/// preregistered evidence; a configuration selects exactly one version.
pub const PROMPT_VERSION_V2: &str = "ato.formation-candidate-producer-prompt/2";
pub const PROMPT_V2: &str = include_str!("prompt-v2.txt");
pub const PROMPT_VERSION_V3: &str = "ato.formation-candidate-producer-prompt/3";
pub const PROMPT_V3: &str = include_str!("prompt-v3.txt");
pub const PROMPT_VERSION_V4: &str = "ato.formation-candidate-producer-prompt/4";
pub const PROMPT_V4: &str = include_str!("prompt-v4.txt");
pub const PROMPT_VERSION_V5: &str = "ato.formation-candidate-producer-prompt/5";
pub const PROMPT_V5: &str = include_str!("prompt-v5.txt");
pub const PROMPT_VERSION_V6: &str = "ato.formation-candidate-producer-prompt/6";
pub const PROMPT_V6: &str = include_str!("prompt-v6.txt");
pub const PROMPT_VERSION_V7: &str = "ato.formation-candidate-producer-prompt/7";
pub const PROMPT_V7: &str = include_str!("prompt-v7.txt");
pub const PROMPT_VERSION_V8: &str = "ato.formation-candidate-producer-prompt/8";
pub const PROMPT_V8: &str = include_str!("prompt-v8.txt");
pub const PROMPT_VERSION_V9: &str = "ato.formation-candidate-producer-prompt/9";
pub const PROMPT_V9: &str = include_str!("prompt-v9.txt");
pub fn is_autonomous_prompt(version: &str) -> bool {
    matches!(
        version,
        PROMPT_VERSION_V6 | PROMPT_VERSION_V7 | PROMPT_VERSION_V8 | PROMPT_VERSION_V9
    )
}
pub fn prompt_sha256() -> String {
    format!("sha256:{:x}", Sha256::digest(PROMPT.as_bytes()))
}
pub fn prompt_for(version: &str) -> Option<&'static str> {
    match version {
        PROMPT_VERSION => Some(PROMPT),
        PROMPT_VERSION_V2 => Some(PROMPT_V2),
        PROMPT_VERSION_V3 => Some(PROMPT_V3),
        PROMPT_VERSION_V4 => Some(PROMPT_V4),
        PROMPT_VERSION_V5 => Some(PROMPT_V5),
        PROMPT_VERSION_V6 => Some(PROMPT_V6),
        PROMPT_VERSION_V7 => Some(PROMPT_V7),
        PROMPT_VERSION_V8 => Some(PROMPT_V8),
        PROMPT_VERSION_V9 => Some(PROMPT_V9),
        _ => None,
    }
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ThinkingMode {
    Disabled,
    Enabled { effort: ReasoningEffort },
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Low,
    High,
    Max,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeepSeekConfig {
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub prompt_version: String,
    pub max_output_tokens: u64,
    pub timeout_ms: u64,
    pub thinking: ThinkingMode,
}
impl DeepSeekConfig {
    pub fn configuration_ref(&self, budget: &super::budget::BudgetPlan) -> Result<String> {
        self.validate()?;
        budget.validate()?;
        let prompt = prompt_for(&self.prompt_version).context("unknown prompt version")?;
        let canonical = serde_jcs::to_vec(&(
            self,
            budget,
            format!("sha256:{:x}", Sha256::digest(prompt.as_bytes())),
        ))?;
        Ok(format!("sha256:{:x}", Sha256::digest(canonical)))
    }
    pub fn identity(&self) -> ProviderIdentity {
        ProviderIdentity {
            provider: self.provider.clone(),
            model: self.model.clone(),
            prompt_version: self.prompt_version.clone(),
        }
    }
    fn validate(&self) -> Result<()> {
        self.identity().validate()?;
        ensure!(
            self.provider == "deepseek"
                && prompt_for(&self.prompt_version).is_some()
                && (1..=2048).contains(&self.max_output_tokens)
                && (1..=30_000).contains(&self.timeout_ms),
            "invalid DeepSeek configuration"
        );
        Ok(())
    }
}
enum Credentials {
    Environment(String),
    Mock,
}
pub struct DeepSeekCandidateProducer {
    config: DeepSeekConfig,
    credentials: Credentials,
    budget: Arc<CallBudget>,
    http: reqwest::blocking::Client,
    #[cfg(test)]
    pub(super) credential_reads: std::sync::atomic::AtomicUsize,
}
pub struct GeneralOutput {
    pub output: ProducerOutput,
    pub provenance: Provenance,
}
#[derive(Debug)]
pub struct GeneralFailure {
    pub class: ErrorClass,
    pub provenance: Provenance,
}
impl DeepSeekCandidateProducer {
    pub fn configuration_ref(&self) -> Result<String> {
        self.config.configuration_ref(self.budget.plan())
    }
    pub fn new(
        config: DeepSeekConfig,
        key_environment: &str,
        budget: Arc<CallBudget>,
    ) -> Result<Self> {
        ensure!(
            config.endpoint == "https://api.deepseek.com",
            "production endpoint is fixed"
        );
        ensure!(
            !key_environment.is_empty()
                && key_environment
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'),
            "invalid credential variable name"
        );
        Self::build(
            config,
            Credentials::Environment(key_environment.into()),
            budget,
        )
    }
    /// Local synthetic transport only; never reads an environment credential.
    pub fn new_mock(config: DeepSeekConfig, budget: Arc<CallBudget>) -> Result<Self> {
        let url = reqwest::Url::parse(&config.endpoint)?;
        ensure!(
            url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "[::1]"))
                && url.path() == "/"
                && url.query().is_none()
                && url.fragment().is_none()
                && url.username().is_empty()
                && url.password().is_none(),
            "mock endpoint must be literal loopback root"
        );
        Self::build(config, Credentials::Mock, budget)
    }
    fn build(
        config: DeepSeekConfig,
        credentials: Credentials,
        budget: Arc<CallBudget>,
    ) -> Result<Self> {
        config.validate()?;
        budget.plan().validate()?;
        ensure!(
            budget.plan().output_price > 0
                && config.max_output_tokens <= budget.plan().output_token_cap,
            "output exceeds spend reservation"
        );
        let http = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .build()?;
        Ok(Self {
            config,
            credentials,
            budget,
            http,
            #[cfg(test)]
            credential_reads: std::sync::atomic::AtomicUsize::new(0),
        })
    }
    pub fn identity(&self) -> ProviderIdentity {
        self.config.identity()
    }
    /// Keep source text inside both its existing byte ceiling and the frozen
    /// input reservation. K, source IDs, D, requirements and failure facts are
    /// never rewritten. Truncated text has a new transmitted-text digest.
    fn bounded_request(&self, request: &ProposalRequestV2) -> Result<ProposalRequestV2> {
        let mut projected = request.clone();
        if !matches!(
            self.config.prompt_version.as_str(),
            PROMPT_VERSION_V3 | PROMPT_VERSION_V4
        ) {
            return Ok(projected);
        }
        let prompt = prompt_for(&self.config.prompt_version).context("prompt unavailable")?;
        loop {
            let bytes = serde_jcs::to_vec(&projected)?;
            if (bytes.len() + prompt.len() + 1024) as u64 <= self.budget.plan().input_token_cap {
                return Ok(projected);
            }
            let entry = projected
                .source_context
                .iter_mut()
                .max_by_key(|e| e.text.len())
                .context("non-text provider context exceeds input reservation")?;
            if entry.text.is_empty() {
                anyhow::bail!("non-text provider context exceeds input reservation");
            }
            let mut end = entry.text.len().saturating_sub(512);
            while !entry.text.is_char_boundary(end) {
                end -= 1;
            }
            entry.text.truncate(end);
            entry.truncated = true;
            entry.content_sha256 = format!("sha256:{:x}", Sha256::digest(entry.text.as_bytes()));
            projected.source_context.retain(|e| !e.text.is_empty());
        }
    }
    pub fn propose_reasoning(
        &self,
        input: &super::reasoning::ReasoningInput,
    ) -> std::result::Result<GeneralOutput, Box<GeneralFailure>> {
        let mut provenance = self.identity().unknown_usage();
        let started = std::time::Instant::now();
        let result = (|| {
            if !matches!(
                self.config.prompt_version.as_str(),
                PROMPT_VERSION_V5
                    | PROMPT_VERSION_V6
                    | PROMPT_VERSION_V7
                    | PROMPT_VERSION_V8
                    | PROMPT_VERSION_V9
            ) || input.schema != super::reasoning::INPUT_SCHEMA
            {
                return Err(ErrorClass::MalformedResponse);
            }
            let canonical =
                serde_jcs::to_string(input).map_err(|_| ErrorClass::MalformedResponse)?;
            self.call_exact(&input.request, &canonical, &input.call_id, &mut provenance)
        })();
        provenance.latency_ms = Some(started.elapsed().as_millis().min(u64::MAX as u128) as u64);
        match result {
            Ok(output) => Ok(GeneralOutput { output, provenance }),
            Err(class) => {
                if self
                    .budget
                    .snapshot()
                    .ok()
                    .is_some_and(|s| s.cells.get(&input.call_id).is_some_and(|c| !c.is_settled()))
                {
                    let _ = self.budget.charge_unknown(&input.call_id);
                }
                let _ = self.budget.halt();
                Err(Box::new(GeneralFailure { class, provenance }))
            }
        }
    }
    pub fn accounting_snapshot(&self) -> Result<super::budget::BudgetSnapshot> {
        self.budget.snapshot()
    }
    pub fn accounting(&self) -> Result<serde_json::Value> {
        self.budget.accounting()
    }
    /// A shared exchange owns a fixed set of actual call IDs. A lost sent call
    /// stays visible and conservatively charged; it is never an idempotent resend.
    pub fn propose_reasoning_recover(
        &self,
        input: &super::reasoning::ReasoningInput,
        expires: u64,
        max_retries: u32,
    ) -> std::result::Result<GeneralOutput, Box<GeneralFailure>> {
        if !is_autonomous_prompt(&self.config.prompt_version) {
            return self.propose_reasoning(input);
        }
        let mut final_class = ErrorClass::TransportError;
        let canonical = match serde_jcs::to_string(input) {
            Ok(c) => c,
            Err(_) => {
                return Err(Box::new(GeneralFailure {
                    class: ErrorClass::MalformedResponse,
                    provenance: self.identity().unknown_usage(),
                }));
            }
        };
        for retry in 0..=max_retries {
            let cell = format!("{}_t{retry}", input.call_id);
            let mut provenance = self.identity().unknown_usage();
            let snapshot = match self.budget.snapshot() {
                Ok(s) => s,
                Err(_) => break,
            };
            if snapshot.stopped {
                break;
            }
            if let Some(saved) = snapshot.cells.get(&cell) {
                final_class = saved
                    .transport
                    .as_ref()
                    .map_or(ErrorClass::Timeout, |t| t.error_class);
                if !saved.is_settled() {
                    let _ = self.budget.settle_retry(
                        &cell,
                        saved.transport.as_ref().is_some_and(|t| !t.possibly_sent),
                    );
                }
            } else {
                if retry > 0 {
                    let previous = format!("{}_t{}", input.call_id, retry - 1);
                    if let Some(transport) = snapshot
                        .cells
                        .get(&previous)
                        .and_then(|c| c.transport.as_ref())
                    {
                        let delay = transport
                            .retry_after_ms
                            .max(1000_u64.saturating_mul(1_u64 << retry.min(5)));
                        let resume = transport.recorded_at_ms.saturating_add(delay);
                        if resume >= expires {
                            final_class = ErrorClass::Timeout;
                            break;
                        }
                        while now_ms() < resume {
                            std::thread::sleep(Duration::from_millis((resume - now_ms()).min(100)));
                        }
                    }
                }
                if now_ms() >= expires {
                    final_class = ErrorClass::Timeout;
                    break;
                }
                let started = std::time::Instant::now();
                let mut request = input.request.clone();
                request.remaining_budget.timeout_ms = request
                    .remaining_budget
                    .timeout_ms
                    .min(expires.saturating_sub(now_ms()));
                match self.call_exact(&request, &canonical, &cell, &mut provenance) {
                    Ok(output) => {
                        provenance.latency_ms =
                            Some(started.elapsed().as_millis().min(u64::MAX as u128) as u64);
                        let snapshot = self.budget.snapshot().map_err(|_| {
                            Box::new(GeneralFailure {
                                class: ErrorClass::TransportError,
                                provenance: provenance.clone(),
                            })
                        })?;
                        let calls = snapshot
                            .cells
                            .iter()
                            .filter(|(name, _)| name.starts_with(&format!("{}_t", input.call_id)))
                            .map(|(_, c)| c)
                            .collect::<Vec<_>>();
                        provenance.estimated_cost_usd_micros = Some(
                            calls
                                .iter()
                                .map(|c| {
                                    c.response.as_ref().map_or_else(
                                        || {
                                            if c.charged_unknown {
                                                self.budget
                                                    .plan()
                                                    .cost(
                                                        self.budget.plan().input_token_cap,
                                                        self.budget.plan().output_token_cap,
                                                    )
                                                    .unwrap_or(u64::MAX)
                                            } else {
                                                0
                                            }
                                        },
                                        |r| {
                                            self.budget
                                                .plan()
                                                .cost(r.input_tokens, r.output_tokens)
                                                .unwrap_or(u64::MAX)
                                        },
                                    )
                                })
                                .fold(0u64, u64::saturating_add),
                        );
                        if calls.iter().any(|c| c.charged_unknown) {
                            provenance.usage = Usage {
                                input_tokens: None,
                                output_tokens: None,
                            };
                        }
                        return Ok(GeneralOutput { output, provenance });
                    }
                    Err(class) => {
                        final_class = class;
                        let snapshot = self.budget.snapshot().ok();
                        if snapshot
                            .as_ref()
                            .and_then(|s| s.cells.get(&cell))
                            .is_some_and(|c| !c.is_settled())
                        {
                            let _ = self
                                .budget
                                .settle_retry(&cell, class == ErrorClass::ConnectBeforeSend);
                        }
                    }
                }
            }
            if !matches!(
                final_class,
                ErrorClass::ConnectBeforeSend
                    | ErrorClass::ProviderUnavailable
                    | ErrorClass::Timeout
                    | ErrorClass::TransportError
            ) {
                break;
            }
        }
        let _ = self.budget.halt();
        Err(Box::new(GeneralFailure {
            class: final_class,
            provenance: self.identity().unknown_usage(),
        }))
    }
    fn call(
        &self,
        request: &ProposalRequestV2,
        provenance: &mut Provenance,
    ) -> std::result::Result<ProducerOutput, ErrorClass> {
        let canonical = serde_jcs::to_string(request).map_err(|_| ErrorClass::MalformedResponse)?;
        let cell = request.round_seq.map_or_else(
            || request.search_id.clone(),
            |seq| format!("{}_r{seq}", request.search_id),
        );
        self.call_exact(request, &canonical, &cell, provenance)
    }
    fn call_exact(
        &self,
        request: &ProposalRequestV2,
        canonical: &str,
        cell: &str,
        provenance: &mut Provenance,
    ) -> std::result::Result<ProducerOutput, ErrorClass> {
        // Conservative byte upper bound (plus framing reserve) rather than a
        // guessed chars/token ratio. D3 must preregister this input bound.
        let prompt =
            prompt_for(&self.config.prompt_version).ok_or(ErrorClass::MalformedResponse)?;
        let input_bound = (prompt.len() + canonical.len() + 1024) as u64;
        if input_bound > self.budget.plan().input_token_cap {
            return Err(ErrorClass::MalformedResponse);
        }
        let mut body = json!({"model":self.config.model,"stream":false,"max_tokens":self.config.max_output_tokens,
            "response_format":{"type":"json_object"},"messages":[{"role":"system","content":prompt},{"role":"user","content":canonical}]});
        match self.config.thinking {
            ThinkingMode::Disabled => body["thinking"] = json!({"type":"disabled"}),
            ThinkingMode::Enabled { ref effort } => {
                body["thinking"] = json!({"type":"enabled"});
                body["reasoning_effort"] = json!(match effort {
                    ReasoningEffort::Low => "low",
                    ReasoningEffort::High => "high",
                    ReasoningEffort::Max => "max",
                });
            }
        }
        // Serialize once. These exact bytes are both hashed and passed to reqwest.
        let provider_body = serde_json::to_vec(&body).map_err(|_| ErrorClass::MalformedResponse)?;
        let evidence = RequestEvidence {
            cell: cell.to_owned(),
            proposal_request_sha256: format!("sha256:{:x}", Sha256::digest(canonical.as_bytes())),
            provider_body_sha256: format!("sha256:{:x}", Sha256::digest(&provider_body)),
            timeout_ms: request.remaining_budget.timeout_ms,
            proposal_request_bytes: canonical.len() as u64,
            provider_body_bytes: provider_body.len() as u64,
            transmitted_context: Some(super::budget::TransmittedContextEvidence::from_request(
                request,
            )),
        };
        self.budget
            .reserve_request(evidence)
            .map_err(|_| ErrorClass::ProviderRefused)?;
        // The atomic request/reservation event is durable before any credential access.
        #[cfg(test)]
        self.credential_reads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let key = match &self.credentials {
            Credentials::Environment(name) => {
                std::env::var(name).map_err(|_| ErrorClass::TransportError)?
            }
            Credentials::Mock => "synthetic-mock-key".into(),
        };
        let dispatch_started = std::time::Instant::now();
        let sent = self
            .http
            .post(format!(
                "{}/chat/completions",
                self.config.endpoint.trim_end_matches('/')
            ))
            .bearer_auth(key)
            .timeout(Duration::from_millis(
                self.config
                    .timeout_ms
                    .min(request.remaining_budget.timeout_ms),
            ))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(provider_body)
            .send();
        let response = match sent {
            Ok(response) => response,
            Err(error) => {
                let class =
                    if error.is_connect() && is_autonomous_prompt(&self.config.prompt_version) {
                        ErrorClass::ConnectBeforeSend
                    } else if error.is_timeout() {
                        ErrorClass::Timeout
                    } else {
                        ErrorClass::TransportError
                    };
                let _ = self
                    .budget
                    .record_transport(super::budget::TransportEvidence {
                        cell: cell.into(),
                        possibly_sent: !error.is_connect(),
                        http_status: None,
                        retry_after_ms: 0,
                        recorded_at_ms: now_ms(),
                        error_class: class,
                        latency_ms: Some(
                            dispatch_started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                        ),
                    });
                return Err(class);
            }
        };
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let class = if !is_autonomous_prompt(&self.config.prompt_version) {
                ErrorClass::ProviderRefused
            } else {
                match status {
                    401 | 403 => ErrorClass::ProviderAuthentication,
                    400 | 404 => ErrorClass::ProviderConfiguration,
                    429 | 500..=599 => ErrorClass::ProviderUnavailable,
                    _ => ErrorClass::ProviderRefused,
                }
            };
            let retry_after_ms = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|h| h.to_str().ok())
                .and_then(|h| {
                    h.parse::<u64>()
                        .ok()
                        .map(|s| s.saturating_mul(1000))
                        .or_else(|| {
                            httpdate::parse_http_date(h).ok().map(|d| {
                                d.duration_since(SystemTime::now())
                                    .unwrap_or_default()
                                    .as_millis()
                                    .min(u64::MAX as u128) as u64
                            })
                        })
                })
                .unwrap_or(0);
            let _ = self
                .budget
                .record_transport(super::budget::TransportEvidence {
                    cell: cell.into(),
                    possibly_sent: true,
                    http_status: Some(status),
                    retry_after_ms,
                    recorded_at_ms: now_ms(),
                    error_class: class,
                    latency_ms: Some(
                        dispatch_started.elapsed().as_millis().min(u64::MAX as u128) as u64
                    ),
                });
            return Err(class);
        }
        // Envelope includes vendor fields/reasoning, which are never persisted.
        const ENVELOPE_CAP: u64 = 128 * 1024;
        let mut bytes = Vec::new();
        response
            .take(ENVELOPE_CAP + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::TimedOut
                    || e.get_ref()
                        .and_then(|e| e.downcast_ref::<reqwest::Error>())
                        .is_some_and(|e| e.is_timeout())
                {
                    ErrorClass::Timeout
                } else {
                    ErrorClass::TransportError
                }
            })?;
        if bytes.len() > ENVELOPE_CAP as usize {
            return Err(ErrorClass::ResponseTooLarge);
        }
        let envelope: Envelope =
            serde_json::from_slice(&bytes).map_err(|_| ErrorClass::MalformedResponse)?;
        if envelope.choices.len() != 1 {
            return Err(ErrorClass::MalformedResponse);
        }
        let choice = envelope
            .choices
            .into_iter()
            .next()
            .ok_or(ErrorClass::MalformedResponse)?;
        let evidence = ResponseEvidence {
            cell: cell.to_owned(),
            finish_reason: match choice.finish_reason.as_str() {
                "stop" => FinishReason::Stop,
                "length" => FinishReason::Length,
                _ => FinishReason::Other,
            },
            model_matches: envelope.model == self.config.model,
            input_tokens: envelope.usage.prompt_tokens,
            output_tokens: envelope.usage.completion_tokens,
            latency_ms: Some(dispatch_started.elapsed().as_millis().min(u64::MAX as u128) as u64),
        };
        if !evidence.within(self.budget.plan()) {
            self.budget
                .record_response(evidence)
                .map_err(|_| ErrorClass::TransportError)?;
            return Err(ErrorClass::MalformedResponse);
        }
        if envelope.usage.prompt_tokens > 9_007_199_254_740_991
            || envelope.usage.completion_tokens > 9_007_199_254_740_991
        {
            return Err(ErrorClass::MalformedResponse);
        }
        provenance.usage = Usage {
            input_tokens: Some(envelope.usage.prompt_tokens),
            output_tokens: Some(envelope.usage.completion_tokens),
        };
        // Cache-miss upper estimate, not a billing receipt; unknown is allowed.
        provenance.estimated_cost_usd_micros = self
            .budget
            .plan()
            .cost(
                envelope.usage.prompt_tokens,
                envelope.usage.completion_tokens,
            )
            .ok();
        if choice.index != 0 || choice.message.role != "assistant" {
            return Err(ErrorClass::MalformedResponse);
        }
        let raw = choice
            .message
            .content
            .ok_or(ErrorClass::MalformedResponse)?
            .into_bytes();
        if raw.is_empty() {
            return Err(ErrorClass::MalformedResponse);
        }
        if raw.len() > MAX_BATCH_BYTES {
            return Err(ErrorClass::ResponseTooLarge);
        }
        let output = ProducerOutput::new(
            raw,
            ProducerProvenance {
                provider: self.config.provider.clone(),
                model: Some(self.config.model.clone()),
            },
        )
        .map_err(|_| ErrorClass::MalformedResponse)?;
        // Release the next reservation only after the complete envelope passes.
        // Until then the prior response remains unresolved, including crashes.
        self.budget
            .record_response(evidence)
            .map_err(|_| ErrorClass::TransportError)?;
        Ok(output)
    }
}
impl CandidateProducer<ProposalRequestV2, GeneralOutput, GeneralFailure>
    for DeepSeekCandidateProducer
{
    fn propose(
        &self,
        request: &ProposalRequestV2,
    ) -> std::result::Result<GeneralOutput, GeneralFailure> {
        let mut provenance = self.identity().unknown_usage();
        let started = std::time::Instant::now();
        let result = self
            .bounded_request(request)
            .map_err(|_| ErrorClass::MalformedResponse)
            .and_then(|projected| self.call(&projected, &mut provenance));
        if self.config.prompt_version == PROMPT_VERSION_V3 {
            provenance.latency_ms =
                Some(started.elapsed().as_millis().min(u64::MAX as u128) as u64);
        }
        match result {
            Ok(output) => Ok(GeneralOutput { output, provenance }),
            Err(class) => {
                if self.config.prompt_version == PROMPT_VERSION_V3 {
                    let cell = request.round_seq.map_or_else(
                        || request.search_id.clone(),
                        |seq| format!("{}_r{seq}", request.search_id),
                    );
                    if self
                        .budget
                        .snapshot()
                        .ok()
                        .is_some_and(|s| s.cells.get(&cell).is_some_and(|c| !c.is_settled()))
                    {
                        let _ = self.budget.charge_unknown(&cell);
                    }
                }
                // Even across processes, a protocol/infrastructure error cannot
                // silently advance the next live cell. No refund or retry.
                let _ = self.budget.halt();
                Err(GeneralFailure { class, provenance })
            }
        }
    }
}
#[derive(Deserialize)]
struct Envelope {
    model: String,
    choices: Vec<Choice>,
    usage: EnvelopeUsage,
}
#[derive(Deserialize)]
struct Choice {
    finish_reason: String,
    index: u64,
    message: Message,
}
#[derive(Deserialize)]
struct Message {
    role: String,
    content: Option<String>,
}
#[derive(Deserialize)]
struct EnvelopeUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
