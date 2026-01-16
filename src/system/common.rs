use crate::config::EgressRuleEntry;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct IsolationRule {
    pub allow_rules: Vec<EgressRuleEntry>,
    pub dns_rules: Vec<EgressRuleEntry>,
    pub job_id: String,
}

#[derive(Error, Debug)]
pub enum SystemError {
    #[error("unsupported operation: {0}")]
    Unsupported(String),

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error(transparent)]
    Anyhow(#[from] anyhow::Error),
}
