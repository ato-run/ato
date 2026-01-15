use std::net::IpAddr;

use anyhow::{Context, Result};

use crate::runtime_config::EgressRuleEntry;

/// Parse resolv.conf and return DNS server IPs.
pub fn read_dns_servers() -> Result<Vec<IpAddr>> {
    let content =
        std::fs::read_to_string("/etc/resolv.conf").context("Failed to read /etc/resolv.conf")?;
    Ok(parse_resolv_conf(&content))
}

/// Convert DNS server IPs into temporary egress allowlist rules (TCP/UDP 53).
pub fn dns_bootstrap_rules() -> Result<Vec<EgressRuleEntry>> {
    let servers = read_dns_servers()?;
    let mut rules = Vec::new();

    for ip in servers {
        rules.push(EgressRuleEntry {
            rule_type: "ip".to_string(),
            value: ip.to_string(),
        });
    }

    Ok(rules)
}

fn parse_resolv_conf(content: &str) -> Vec<IpAddr> {
    let mut servers = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }

        let mut parts = line.split_whitespace();
        let keyword = parts.next();
        let value = parts.next();

        if keyword == Some("nameserver") {
            if let Some(addr) = value {
                if let Ok(ip) = addr.parse::<IpAddr>() {
                    servers.push(ip);
                }
            }
        }
    }

    servers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_resolv_conf() {
        let content = r#"
nameserver 1.1.1.1
nameserver 2606:4700:4700::1111
"#;
        let servers = parse_resolv_conf(content);
        assert_eq!(servers.len(), 2);
    }
}
