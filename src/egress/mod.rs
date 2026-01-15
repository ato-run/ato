use anyhow::{Context, Result};

use crate::runtime_config::EgressRuleEntry;

pub const MAX_EGRESS_RULES: usize = 4096;

pub mod dns_bootstrap;
pub mod resolver;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_ipv4_and_ipv6() {
        let rules = vec![
            EgressRuleEntry {
                rule_type: "ip".to_string(),
                value: "1.1.1.1".to_string(),
            },
            EgressRuleEntry {
                rule_type: "ip".to_string(),
                value: "::1".to_string(),
            },
            EgressRuleEntry {
                rule_type: "cidr".to_string(),
                value: "10.0.0.0/8".to_string(),
            },
            EgressRuleEntry {
                rule_type: "cidr".to_string(),
                value: "2001:db8::/32".to_string(),
            },
        ];

        validate_egress_rules(&rules).unwrap();
    }

    #[test]
    fn rejects_over_limit() {
        let rules = (0..(MAX_EGRESS_RULES + 1))
            .map(|i| EgressRuleEntry {
                rule_type: "ip".to_string(),
                value: format!("10.0.{}.{}", i / 255, i % 255),
            })
            .collect::<Vec<_>>();

        let err = validate_egress_rules(&rules).unwrap_err();
        assert!(err.to_string().contains("fail-closed"));
    }
}
