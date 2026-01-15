use anyhow::{Context, Result};
use std::collections::HashSet;
use std::net::ToSocketAddrs;

use crate::egress::MAX_EGRESS_RULES;
use crate::runtime_config::EgressRuleEntry;

/// Resolve allow_domains into IP/CIDR rules.
///
/// - IP literals are kept as-is.
/// - CIDR entries are kept as-is.
/// - Domains are resolved to IPs at startup.
pub fn resolve_allow_domains(allow_domains: &[String]) -> Result<Vec<EgressRuleEntry>> {
    let mut rules = Vec::new();
    let mut seen = HashSet::new();
    let mut count = 0usize;

    for entry in allow_domains {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }

        if entry.parse::<std::net::IpAddr>().is_ok() {
            if seen.insert(entry.to_string()) {
                rules.push(EgressRuleEntry {
                    rule_type: "ip".to_string(),
                    value: entry.to_string(),
                });
                count += 1;
            }
            if count > MAX_EGRESS_RULES {
                anyhow::bail!(
                    "Egress allowlist exceeds {} entries (fail-closed)",
                    MAX_EGRESS_RULES
                );
            }
            continue;
        }

        if entry.contains('/') {
            if seen.insert(entry.to_string()) {
                rules.push(EgressRuleEntry {
                    rule_type: "cidr".to_string(),
                    value: entry.to_string(),
                });
                count += 1;
            }
            if count > MAX_EGRESS_RULES {
                anyhow::bail!(
                    "Egress allowlist exceeds {} entries (fail-closed)",
                    MAX_EGRESS_RULES
                );
            }
            continue;
        }

        // Domain resolution (startup-only)
        let addr_string = format!("{}:443", entry);
        let addrs: Vec<_> = addr_string
            .to_socket_addrs()
            .with_context(|| format!("Failed to resolve domain: {}", entry))?
            .map(|addr| addr.ip().to_string())
            .collect();

        if addrs.is_empty() {
            anyhow::bail!("No IP addresses found for domain: {}", entry);
        }

        for ip in addrs {
            if seen.insert(ip.clone()) {
                rules.push(EgressRuleEntry {
                    rule_type: "ip".to_string(),
                    value: ip,
                });
                count += 1;
                if count > MAX_EGRESS_RULES {
                    anyhow::bail!(
                        "Egress allowlist exceeds {} entries (fail-closed)",
                        MAX_EGRESS_RULES
                    );
                }
            }
        }
    }

    Ok(rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_ip_and_cidr_only() {
        let rules =
            resolve_allow_domains(&["1.1.1.1".to_string(), "10.0.0.0/8".to_string()]).unwrap();

        assert_eq!(rules.len(), 2);
    }
}
