//! D1 operational evidence. Never used in proposal, D or K identity.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

pub const PROVENANCE_SCHEMA: &str = "ato.formation-proposal-provenance/1";
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderIdentity {
    pub provider: String,
    pub model: String,
    pub prompt_version: String,
}
fn identifier(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:/@+-".contains(&b))
}
impl ProviderIdentity {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            identifier(&self.provider, 32)
                && identifier(&self.model, 96)
                && identifier(&self.prompt_version, 64),
            "invalid provider identity"
        );
        Ok(())
    }
    pub fn unknown_usage(&self) -> Provenance {
        Provenance {
            schema: PROVENANCE_SCHEMA.into(),
            kind: "llm".into(),
            provider: self.provider.clone(),
            model: self.model.clone(),
            prompt_version: self.prompt_version.clone(),
            usage: Usage {
                input_tokens: None,
                output_tokens: None,
            },
            estimated_cost_usd_micros: None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub schema: String,
    pub kind: String,
    pub provider: String,
    pub model: String,
    pub prompt_version: String,
    pub usage: Usage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd_micros: Option<u64>,
}
impl Provenance {
    pub fn validate(&self, expected: &ProviderIdentity, success: bool) -> Result<()> {
        expected.validate()?;
        ensure!(
            self.schema == PROVENANCE_SCHEMA
                && self.kind == "llm"
                && self.provider == expected.provider
                && self.model == expected.model
                && self.prompt_version == expected.prompt_version,
            "provider identity mismatch"
        );
        ensure!(
            !success || (self.usage.input_tokens.is_some() && self.usage.output_tokens.is_some()),
            "missing usage"
        );
        ensure!(
            [
                self.usage.input_tokens,
                self.usage.output_tokens,
                self.estimated_cost_usd_micros
            ]
            .into_iter()
            .flatten()
            .all(|n| n <= 9_007_199_254_740_991),
            "operational integer bounds"
        );
        ensure!(serde_jcs::to_vec(self)?.len() <= 512, "provenance bounds");
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    TransportError,
    ProviderRefused,
    Timeout,
    MalformedResponse,
    ResponseTooLarge,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallStatus {
    Success,
    ProviderError,
    Timeout,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCall {
    pub provenance: Provenance,
    pub status: CallStatus,
    pub error_class: Option<ErrorClass>,
}
impl ProviderCall {
    pub fn validate(&self, expected: &ProviderIdentity) -> Result<()> {
        self.provenance
            .validate(expected, self.status == CallStatus::Success)?;
        ensure!(
            match self.status {
                CallStatus::Success => self.error_class.is_none(),
                CallStatus::Timeout => self.error_class == Some(ErrorClass::Timeout),
                CallStatus::ProviderError =>
                    self.error_class.is_some() && self.error_class != Some(ErrorClass::Timeout),
            },
            "provider call outcome mismatch"
        );
        Ok(())
    }
}
