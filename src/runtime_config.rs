use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeConfig {
    pub version: String,
    pub services: HashMap<String, ServiceConfig>,
    pub sandbox: SandboxConfig,
    #[serde(default)]
    pub metadata: Option<MetadataConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServiceConfig {
    pub executable: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: Option<HashMap<String, String>>,
    #[serde(default)]
    pub signals: Option<SignalsConfig>,
    #[serde(default)]
    pub depends_on: Option<Vec<String>>,
    #[serde(default)]
    pub health_check: Option<HealthCheck>,
    #[serde(default)]
    pub ports: Option<HashMap<String, u16>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SignalsConfig {
    #[serde(default)]
    pub stop: String,
    #[serde(default)]
    pub kill: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HealthCheck {
    #[serde(default)]
    pub http_get: Option<String>,
    #[serde(default)]
    pub tcp_connect: Option<String>,
    pub port: String,
    #[serde(default)]
    pub interval_secs: Option<u32>,
    #[serde(default)]
    pub timeout_secs: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SandboxConfig {
    pub enabled: bool,
    #[serde(default)]
    pub filesystem: Option<FilesystemConfig>,
    pub network: NetworkConfig,
    #[serde(default)]
    pub development_mode: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FilesystemConfig {
    #[serde(default)]
    pub read_only: Option<Vec<String>>,
    #[serde(default)]
    pub read_write: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NetworkConfig {
    pub enabled: bool,
    #[serde(default)]
    pub allow_domains: Option<Vec<String>>,
    pub enforcement: String,
    #[serde(default)]
    pub egress: Option<EgressConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EgressConfig {
    pub mode: String,
    #[serde(default)]
    pub rules: Option<Vec<EgressRuleEntry>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EgressRuleEntry {
    #[serde(rename = "type")]
    pub rule_type: String,
    pub value: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MetadataConfig {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub generated_by: Option<String>,
    #[serde(default)]
    pub source_manifest: Option<String>,
}

pub fn load_config(path: &Path) -> Result<RuntimeConfig> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config.json: {}", path.display()))?;
    let config: RuntimeConfig = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse config.json: {}", path.display()))?;
    Ok(config)
}
