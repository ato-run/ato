//! L4 Egress Policy Resolver
//!
//! Resolves domain names to IP addresses for egress allowlists.
//! This pre-resolution happens at pack time so nacelle doesn't need DNS.

use serde::{Deserialize, Serialize};
use std::net::ToSocketAddrs;
use tracing::{info, warn};

use crate::error::{CapsuleError, Result};

const MAX_EGRESS_RULES: usize = 4096;

/// Egress rule types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum EgressRule {
    /// Domain name (e.g., "api.example.com")
    Domain { value: String },
    /// IP address (e.g., "1.1.1.1")
    Ip { value: String },
    /// CIDR block (e.g., "10.0.0.0/8")
    Cidr { value: String },
}

/// Resolved egress policy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedEgressPolicy {
    /// Original rules from manifest
    pub rules: Vec<EgressRule>,
    /// Resolved IP addresses (domains → IPs)
    pub resolved_ips: Vec<String>,
}

/// Resolve egress policy by converting domains to IPs
///
/// # Arguments
/// * `allowed_domains` - List of allowed domains/IPs/CIDRs
///
/// # Returns
/// ResolvedEgressPolicy with all domains resolved to IPs
pub fn resolve_egress_policy(allowed_domains: &[String]) -> Result<ResolvedEgressPolicy> {
    let mut rules = Vec::new();
    let mut resolved_ips = Vec::new();

    let mut rule_count: usize = 0;

    for entry in allowed_domains {
        let entry = entry.trim();

        if entry.is_empty() {
            continue;
        }

        // Check if it's an IP address
        if entry.parse::<std::net::IpAddr>().is_ok() {
            rules.push(EgressRule::Ip {
                value: entry.to_string(),
            });
            resolved_ips.push(entry.to_string());
            rule_count += 1;
            if rule_count > MAX_EGRESS_RULES {
                return Err(CapsuleError::Config(format!(
                    "Egress allowlist exceeds {} entries (fail-closed)",
                    MAX_EGRESS_RULES
                )));
            }
            continue;
        }

        // Check if it's a CIDR block
        if entry.contains('/') {
            rules.push(EgressRule::Cidr {
                value: entry.to_string(),
            });
            rule_count += 1;
            if rule_count > MAX_EGRESS_RULES {
                return Err(CapsuleError::Config(format!(
                    "Egress allowlist exceeds {} entries (fail-closed)",
                    MAX_EGRESS_RULES
                )));
            }
            // Note: CIDRs are not resolved to individual IPs
            continue;
        }

        // Treat as domain name
        rules.push(EgressRule::Domain {
            value: entry.to_string(),
        });

        // Resolve domain to IP addresses
        match resolve_domain_to_ips(entry) {
            Ok(ips) => {
                info!("✅ Resolved {}: {} IPs", entry, ips.len());
                if ips.len() > 1 {
                    warn!(
                        "⚠️  Shared IP/CDN risk: domain {} resolved to {} IPs",
                        entry,
                        ips.len()
                    );
                }
                for ip in ips {
                    info!("   - {}", ip);
                    resolved_ips.push(ip);
                    rule_count += 1;
                    if rule_count > MAX_EGRESS_RULES {
                        return Err(CapsuleError::Config(format!(
                            "Egress allowlist exceeds {} entries (fail-closed)",
                            MAX_EGRESS_RULES
                        )));
                    }
                }
            }
            Err(e) => {
                warn!("⚠️  Failed to resolve {}: {}", entry, e);
                warn!("   Domain will be kept but may not work at runtime");
                // Continue without failing - nacelle can try at runtime
            }
        }
    }

    Ok(ResolvedEgressPolicy {
        rules,
        resolved_ips,
    })
}

/// Resolve a domain name to IP addresses
fn resolve_domain_to_ips(domain: &str) -> Result<Vec<String>> {
    // Use port 443 as a hint (HTTPS is most common)
    let addr_string = format!("{}:443", domain);

    let addrs: Vec<_> = addr_string
        .to_socket_addrs()
        .map_err(CapsuleError::Io)?
        .map(|addr| addr.ip().to_string())
        .collect();

    if addrs.is_empty() {
        return Err(CapsuleError::NotFound(format!(
            "No IP addresses found for domain: {}",
            domain
        )));
    }

    // Deduplicate
    let mut unique_ips: Vec<String> = addrs.into_iter().collect();
    unique_ips.sort();
    unique_ips.dedup();

    Ok(unique_ips)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_ip_address() {
        let policy = resolve_egress_policy(&["1.1.1.1".to_string()]).unwrap();

        assert_eq!(policy.rules.len(), 1);
        assert_eq!(
            policy.rules[0],
            EgressRule::Ip {
                value: "1.1.1.1".to_string()
            }
        );
        assert_eq!(policy.resolved_ips, vec!["1.1.1.1"]);
    }

    #[test]
    fn test_resolve_cidr_block() {
        let policy = resolve_egress_policy(&["10.0.0.0/8".to_string()]).unwrap();

        assert_eq!(policy.rules.len(), 1);
        assert_eq!(
            policy.rules[0],
            EgressRule::Cidr {
                value: "10.0.0.0/8".to_string()
            }
        );
        // CIDRs are not resolved to IPs
        assert_eq!(policy.resolved_ips.len(), 0);
    }

    #[test]
    fn test_resolve_domain() {
        // Use a reliable public domain
        let policy = resolve_egress_policy(&["dns.google".to_string()]).unwrap();

        assert_eq!(policy.rules.len(), 1);
        assert!(matches!(policy.rules[0], EgressRule::Domain { .. }));

        // Should have resolved to at least one IP
        assert!(!policy.resolved_ips.is_empty());

        // All resolved entries should be valid IPs
        for ip in &policy.resolved_ips {
            assert!(ip.parse::<std::net::IpAddr>().is_ok());
        }
    }

    #[test]
    fn test_resolve_mixed_list() {
        let policy = resolve_egress_policy(&[
            "1.1.1.1".to_string(),
            "10.0.0.0/24".to_string(),
            "dns.google".to_string(),
        ])
        .unwrap();

        assert_eq!(policy.rules.len(), 3);

        // First rule is IP
        assert!(matches!(policy.rules[0], EgressRule::Ip { .. }));

        // Second rule is CIDR
        assert!(matches!(policy.rules[1], EgressRule::Cidr { .. }));

        // Third rule is Domain
        assert!(matches!(policy.rules[2], EgressRule::Domain { .. }));

        // Resolved IPs should include the direct IP + resolved domain IPs
        assert!(!policy.resolved_ips.is_empty());
        assert!(policy.resolved_ips.contains(&"1.1.1.1".to_string()));
    }

    #[test]
    fn test_resolve_ipv4_ipv6_literals() {
        let policy = resolve_egress_policy(&["127.0.0.1".to_string(), "::1".to_string()]).unwrap();

        assert!(policy.resolved_ips.contains(&"127.0.0.1".to_string()));
        assert!(policy.resolved_ips.contains(&"::1".to_string()));
    }

    #[test]
    fn test_fail_closed_on_limit() {
        let mut entries = Vec::new();
        for i in 0..(MAX_EGRESS_RULES + 1) {
            entries.push(format!("10.0.{}.{}", i / 255, i % 255));
        }

        let err = resolve_egress_policy(&entries).unwrap_err();
        assert!(err.to_string().contains("fail-closed"));
    }

    #[test]
    fn test_resolve_invalid_domain() {
        // Should not fail, but warn
        let policy =
            resolve_egress_policy(&["this-domain-definitely-does-not-exist-12345.com".to_string()])
                .unwrap();

        // Rule should still be added
        assert_eq!(policy.rules.len(), 1);
        assert!(matches!(policy.rules[0], EgressRule::Domain { .. }));

        // But no IPs resolved
        assert_eq!(policy.resolved_ips.len(), 0);
    }

    #[test]
    fn test_empty_list() {
        let policy = resolve_egress_policy(&[]).unwrap();
        assert_eq!(policy.rules.len(), 0);
        assert_eq!(policy.resolved_ips.len(), 0);
    }
}
