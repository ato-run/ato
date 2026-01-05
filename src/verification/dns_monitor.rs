//! DNS Tunneling Prevention Module
//!
//! Generates iptables rules to prevent DNS-based data exfiltration by limiting
//! which DNS resolvers capsules can communicate with.
//!
//! Strategy:
//! - Block all outbound UDP/TCP port 53 by default
//! - Allow only explicitly permitted resolvers (e.g., Tailscale, internal)
//! - Log blocked DNS attempts for audit

use tracing::info;

/// Default allowed DNS resolvers for capsules
pub const DEFAULT_ALLOWED_RESOLVERS: &[&str] = &[
    "127.0.0.1",       // Loopback
    "100.100.100.100", // Tailscale MagicDNS
];

/// DNS tunneling detection config
#[derive(Debug, Clone)]
pub struct DnsConfig {
    /// List of allowed DNS resolver IPs
    pub allowed_resolvers: Vec<String>,
    /// Whether to log blocked attempts (for audit)
    pub log_blocked: bool,
    /// Whether DNS control is enabled
    pub enabled: bool,
}

impl Default for DnsConfig {
    fn default() -> Self {
        Self {
            allowed_resolvers: DEFAULT_ALLOWED_RESOLVERS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            log_blocked: true,
            enabled: true,
        }
    }
}

/// Generate iptables rules for DNS control
///
/// Returns a list of iptables command strings to be executed as prestart hooks.
/// These rules ensure capsules can only use approved DNS resolvers.
pub fn generate_dns_rules(config: &DnsConfig, chain_name: &str) -> Vec<String> {
    if !config.enabled {
        return vec![];
    }

    let mut rules = Vec::new();

    // Add allowed resolver rules (UDP and TCP for DNS)
    for resolver in &config.allowed_resolvers {
        // Allow UDP 53 to permitted resolver
        rules.push(format!(
            "iptables -A {} -p udp -d {} --dport 53 -j ACCEPT",
            chain_name, resolver
        ));
        // Allow TCP 53 (for large responses / DNS over TCP)
        rules.push(format!(
            "iptables -A {} -p tcp -d {} --dport 53 -j ACCEPT",
            chain_name, resolver
        ));
    }

    // Log blocked DNS attempts (if enabled)
    if config.log_blocked {
        rules.push(format!(
            "iptables -A {} -p udp --dport 53 -j LOG --log-prefix '[ADEP-DNS-BLOCKED] '",
            chain_name
        ));
        rules.push(format!(
            "iptables -A {} -p tcp --dport 53 -j LOG --log-prefix '[ADEP-DNS-BLOCKED] '",
            chain_name
        ));
    }

    // Block all other DNS traffic (both UDP and TCP port 53)
    rules.push(format!(
        "iptables -A {} -p udp --dport 53 -j DROP",
        chain_name
    ));
    rules.push(format!(
        "iptables -A {} -p tcp --dport 53 -j DROP",
        chain_name
    ));

    info!(
        "Generated {} DNS control rules for chain {} (allowed resolvers: {:?})",
        rules.len(),
        chain_name,
        config.allowed_resolvers
    );

    rules
}

/// Check if a DNS resolver is in the allowed list
pub fn is_resolver_allowed(resolver: &str, config: &DnsConfig) -> bool {
    config.allowed_resolvers.iter().any(|r| r == resolver)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = DnsConfig::default();
        assert!(config.enabled);
        assert!(config.log_blocked);
        assert!(config.allowed_resolvers.contains(&"127.0.0.1".to_string()));
        assert!(config
            .allowed_resolvers
            .contains(&"100.100.100.100".to_string()));
    }

    #[test]
    fn test_generate_dns_rules_default() {
        let config = DnsConfig::default();
        let rules = generate_dns_rules(&config, "OUTPUT");

        // Should have rules for each resolver (UDP + TCP) + log rules + drop rules
        // 2 resolvers * 2 protocols + 2 log + 2 drop = 8 rules
        assert_eq!(rules.len(), 8);

        // Check structure
        assert!(rules
            .iter()
            .any(|r| r.contains("127.0.0.1") && r.contains("udp")));
        assert!(rules
            .iter()
            .any(|r| r.contains("100.100.100.100") && r.contains("tcp")));
        assert!(rules
            .iter()
            .any(|r| r.contains("LOG") && r.contains("ADEP-DNS-BLOCKED")));
        assert!(rules
            .iter()
            .any(|r| r.contains("-j DROP") && r.contains("--dport 53")));
    }

    #[test]
    fn test_generate_dns_rules_disabled() {
        let config = DnsConfig {
            enabled: false,
            ..Default::default()
        };
        let rules = generate_dns_rules(&config, "OUTPUT");
        assert!(rules.is_empty());
    }

    #[test]
    fn test_generate_dns_rules_no_logging() {
        let config = DnsConfig {
            log_blocked: false,
            ..Default::default()
        };
        let rules = generate_dns_rules(&config, "OUTPUT");

        // No LOG rules should be present
        assert!(!rules.iter().any(|r| r.contains("LOG")));
        // But DROP rules should exist
        assert!(rules.iter().any(|r| r.contains("DROP")));
    }

    #[test]
    fn test_generate_dns_rules_custom_resolvers() {
        let config = DnsConfig {
            allowed_resolvers: vec!["8.8.8.8".to_string(), "1.1.1.1".to_string()],
            log_blocked: false,
            enabled: true,
        };
        let rules = generate_dns_rules(&config, "CAPSULE_DNS");

        assert!(rules.iter().any(|r| r.contains("8.8.8.8")));
        assert!(rules.iter().any(|r| r.contains("1.1.1.1")));
        assert!(rules.iter().any(|r| r.contains("CAPSULE_DNS")));
    }

    #[test]
    fn test_is_resolver_allowed() {
        let config = DnsConfig::default();
        assert!(is_resolver_allowed("127.0.0.1", &config));
        assert!(is_resolver_allowed("100.100.100.100", &config));
        assert!(!is_resolver_allowed("8.8.8.8", &config));
    }
}
