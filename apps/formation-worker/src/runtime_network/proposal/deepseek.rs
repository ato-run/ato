//! Requester-only, one HTTP send. No repair, fallback, SDK parsing authority,
//! reasoning persistence or external credentials in a ProposalRequest.
use super::{budget::CallBudget, provenance::*};
use anyhow::{Result, ensure};
use ato_formation::proposal::{
    CandidateProducer, MAX_BATCH_BYTES, ProducerOutput, ProducerProvenance, ProposalRequestV2,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{io::Read, sync::Arc, time::Duration};

pub const PROMPT_VERSION: &str = "ato.formation-candidate-producer-prompt/1";
pub const PROMPT: &str = include_str!("prompt.txt");
pub fn prompt_sha256() -> String {
    format!("sha256:{:x}", Sha256::digest(PROMPT.as_bytes()))
}
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ThinkingMode {
    Disabled,
    Enabled { effort: ReasoningEffort },
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    Low,
    High,
    Max,
}
#[derive(Debug, Clone, Deserialize)]
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
                && self.prompt_version == PROMPT_VERSION
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
            config.max_output_tokens <= budget.plan().output_token_cap,
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
        })
    }
    pub fn identity(&self) -> ProviderIdentity {
        self.config.identity()
    }
    fn call(
        &self,
        request: &ProposalRequestV2,
        provenance: &mut Provenance,
    ) -> std::result::Result<ProducerOutput, ErrorClass> {
        let canonical = serde_jcs::to_string(request).map_err(|_| ErrorClass::MalformedResponse)?;
        // Conservative byte upper bound (plus framing reserve) rather than a
        // guessed chars/token ratio. D3 must preregister this input bound.
        let input_bound = (PROMPT.len() + canonical.len() + 1024) as u64;
        if input_bound > self.budget.plan().input_token_cap {
            return Err(ErrorClass::MalformedResponse);
        }
        self.budget
            .reserve(&request.search_id)
            .map_err(|_| ErrorClass::ProviderRefused)?;
        // Reservation is durable before this first credential read.
        let key = match &self.credentials {
            Credentials::Environment(name) => {
                std::env::var(name).map_err(|_| ErrorClass::TransportError)?
            }
            Credentials::Mock => "synthetic-mock-key".into(),
        };
        let mut body = json!({"model":self.config.model,"stream":false,"max_tokens":self.config.max_output_tokens,
            "response_format":{"type":"json_object"},"messages":[{"role":"system","content":PROMPT},{"role":"user","content":canonical}]});
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
        let response = self
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
            .json(&body)
            .send()
            .map_err(|e| {
                if e.is_timeout() {
                    ErrorClass::Timeout
                } else {
                    ErrorClass::TransportError
                }
            })?;
        if !response.status().is_success() {
            return Err(ErrorClass::ProviderRefused);
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
        let choice = envelope
            .choices
            .into_iter()
            .next()
            .ok_or(ErrorClass::MalformedResponse)?;
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
        ProducerOutput::new(
            raw,
            ProducerProvenance {
                provider: self.config.provider.clone(),
                model: Some(self.config.model.clone()),
            },
        )
        .map_err(|_| ErrorClass::MalformedResponse)
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
        match self.call(request, &mut provenance) {
            Ok(output) => Ok(GeneralOutput { output, provenance }),
            Err(class) => Err(GeneralFailure { class, provenance }),
        }
    }
}
#[derive(Deserialize)]
struct Envelope {
    choices: Vec<Choice>,
    usage: EnvelopeUsage,
}
#[derive(Deserialize)]
struct Choice {
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
