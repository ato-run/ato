use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

pub const MAX_EGRESS_RULES: usize = 4096;

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeConfig {
    pub version: String,
    pub services: HashMap<String, ServiceConfig>,
    pub sandbox: SandboxConfig,
    #[serde(default)]
    pub metadata: Option<MetadataConfig>,
    #[serde(default)]
    pub sidecar: Option<SidecarConfig>,
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

#[derive(Debug, Clone, Deserialize)]
pub struct SidecarConfig {
    pub tsnet: TsnetSidecarConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TsnetSidecarConfig {
    pub enabled: bool,
    pub control_url: String,
    pub auth_key: String,
    pub hostname: String,
    pub socks_port: u16,
    pub allow_net: Vec<String>,
}

pub fn load_config(path: &Path) -> Result<RuntimeConfig> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read config.json: {}", path.display()))?;
    let config: RuntimeConfig = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse config.json: {}", path.display()))?;
    Ok(config)
}

pub fn validate_egress_rules(rules: &[EgressRuleEntry]) -> Result<()> {
    if rules.len() > MAX_EGRESS_RULES {
        anyhow::bail!(
            "Egress allowlist exceeds {} entries (fail-closed)",
            MAX_EGRESS_RULES
        );
    }

    for rule in rules {
        match rule.rule_type.as_str() {
            "ip" => {
                rule.value
                    .parse::<std::net::IpAddr>()
                    .with_context(|| format!("Invalid IP address: {}", rule.value))?;
            }
            "cidr" => {
                validate_cidr(&rule.value)?;
            }
            other => {
                anyhow::bail!("Unsupported egress rule type: {}", other);
            }
        }
    }

    Ok(())
}

fn validate_cidr(cidr: &str) -> Result<()> {
    let (addr, prefix) = cidr
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("Invalid CIDR (missing '/'): {cidr}"))?;

    let ip: std::net::IpAddr = addr
        .parse()
        .with_context(|| format!("Invalid CIDR address: {cidr}"))?;
    let prefix: u32 = prefix
        .parse()
        .with_context(|| format!("Invalid CIDR prefix: {cidr}"))?;

    match ip {
        std::net::IpAddr::V4(_) => {
            if prefix > 32 {
                anyhow::bail!("Invalid IPv4 CIDR prefix: {cidr}");
            }
        }
        std::net::IpAddr::V6(_) => {
            if prefix > 128 {
                anyhow::bail!("Invalid IPv6 CIDR prefix: {cidr}");
            }
        }
    }

    Ok(())
}
