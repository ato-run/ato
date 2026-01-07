use crate::capsule_types::capsule_v1::{CapsuleManifestV1, EgressIdType};
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

pub const META_KEY_EGRESS_ALLOWLIST: &str = "gumball.egress_allowlist";
pub const ENV_KEY_EGRESS_TOKEN: &str = "GUMBALL_EGRESS_TOKEN";

#[derive(Debug, Clone)]
struct Policy {
    token: String,
    allowlist: Vec<String>,
}

#[derive(Debug, Default)]
pub struct EgressPolicyRegistry {
    policies: RwLock<HashMap<String, Policy>>,
}

impl EgressPolicyRegistry {
    pub fn global() -> &'static Self {
        static REGISTRY: OnceLock<EgressPolicyRegistry> = OnceLock::new();
        REGISTRY.get_or_init(EgressPolicyRegistry::default)
    }

    pub fn register(&self, workload_id: &str, token: String, allowlist: Vec<String>) {
        let mut policies = self.policies.write().expect("poisoned policies lock");
        policies.insert(workload_id.to_string(), Policy { token, allowlist });
    }

    pub fn unregister(&self, workload_id: &str) {
        let mut policies = self.policies.write().expect("poisoned policies lock");
        policies.remove(workload_id);
    }

    pub fn allowlist_for_basic_auth(&self, username: &str, password: &str) -> Option<Vec<String>> {
        let policies = self.policies.read().expect("poisoned policies lock");
        let policy = policies.get(username)?;
        if policy.token != password {
            return None;
        }
        Some(policy.allowlist.clone())
    }
}

/// Generate IPTables rules for L3 Egress Control based on Capsule Manifest
///
/// Policy:
/// - Default DROP (Fail-Closed)
/// - Allow Loopback
/// - Allow Established/Related
/// - Allow DNS (UDP/TCP 53) - Critical for resolving allowed domains (if any)
/// - Allow Local Gateway/Service Subnet (Implied/Configurable? For now, standard docker bridge is allowlisted via specific rules if needed, but strict mode blocks it unless specified)
/// - Allow explicitly listed IPs/CIDRs
pub fn generate_fw_rules(manifest: &CapsuleManifestV1) -> Vec<String> {
    let mut rules = vec![
        // 1. Basic Setup: Flush and Default DROP
        "iptables -P OUTPUT DROP".to_string(),
        "iptables -P INPUT DROP".to_string(),
        "iptables -P FORWARD DROP".to_string(),
        // 2. Allow Loopback
        "iptables -A OUTPUT -o lo -j ACCEPT".to_string(),
        "iptables -A INPUT -i lo -j ACCEPT".to_string(),
        // 3. Allow Established connections
        "iptables -A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT".to_string(),
        "iptables -A INPUT -m state --state ESTABLISHED,RELATED -j ACCEPT".to_string(),
        // 4. Critical Services (DNS)
        "iptables -A OUTPUT -p udp --dport 53 -j ACCEPT".to_string(),
        "iptables -A OUTPUT -p tcp --dport 53 -j ACCEPT".to_string(),
    ];

    // 5. Explicit Allowlist
    if let Some(net) = &manifest.network {
        for rule in &net.egress_id_allow {
            match rule.rule_type {
                EgressIdType::Ip | EgressIdType::Cidr => {
                    // Valid IP/CIDR check should be done ideally, but we pass through for now.
                    // Prevent command injection? Legacy deserialization handles structure, but value might be invalid.
                    // Simple check: characters allowed in IP/CIDR.
                    if is_safe_ip_string(&rule.value) {
                        rules.push(format!("iptables -A OUTPUT -d {} -j ACCEPT", rule.value));
                    }
                }
                EgressIdType::Spiffe => {
                    // TODO: SPIFFE ID resolution to IPs. For now, ignored/logged.
                }
            }
        }
    }

    // 6. Debug / Logging (Optional - log dropped packets)
    // rules.push("iptables -A OUTPUT -j LOG --log-prefix 'Dropped Output: '".to_string());

    rules
}

fn is_safe_ip_string(s: &str) -> bool {
    // Only allow digits, dots, slash, colon (IPv6). No spaces or shell metas.
    s.chars()
        .all(|c| c.is_ascii_digit() || c == '.' || c == '/' || c == ':')
}

pub fn parse_allowlist_csv(value: &str) -> Vec<String> {
    let mut out: Vec<String> = value
        .split(',')
        .filter_map(normalize_allowlist_entry)
        .collect();
    out.sort();
    out.dedup();
    out
}

pub fn normalize_allowlist_entry(value: &str) -> Option<String> {
    let mut s = value.trim().to_lowercase();
    if s.is_empty() {
        return None;
    }

    if let Some(rest) = s.strip_prefix("*.") {
        s = rest.to_string();
    }
    if let Some(rest) = s.strip_prefix('.') {
        s = rest.to_string();
    }

    // If it looks like a URL, strip scheme and path.
    if let Some(idx) = s.find("://") {
        s = s[(idx + 3)..].to_string();
    }

    // Strip userinfo (e.g. user:pass@host)
    if let Some((_, host)) = s.rsplit_once('@') {
        s = host.to_string();
    }

    // Strip path
    if let Some(idx) = s.find('/') {
        s = s[..idx].to_string();
    }

    // Strip port (best-effort; handle bracketed IPv6)
    if s.starts_with('[') {
        if let Some(end) = s.find(']') {
            let host = &s[..=end];
            s = host.to_string();
        }
    } else if let Some((host, _port)) = s.rsplit_once(':') {
        // rsplit_once avoids breaking userinfo (already stripped) and keeps host part.
        s = host.to_string();
    }

    // Trim trailing dot (FQDN canonicalization)
    s = s.trim_end_matches('.').to_string();

    // Reject residual wildcards like "*" or "exa*mple.com".
    if s.contains('*') {
        return None;
    }

    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_allowlist_entry_accepts_domains_and_urls() {
        assert_eq!(
            normalize_allowlist_entry("example.com"),
            Some("example.com".into())
        );
        assert_eq!(
            normalize_allowlist_entry("example.com."),
            Some("example.com".into())
        );
        assert_eq!(
            normalize_allowlist_entry("https://api.example.com/v1"),
            Some("api.example.com".into())
        );
        assert_eq!(
            normalize_allowlist_entry("https://user:pass@api.example.com:8443/v1"),
            Some("api.example.com".into())
        );
        assert_eq!(
            normalize_allowlist_entry("https://example.com.:443/path"),
            Some("example.com".into())
        );
        assert_eq!(
            normalize_allowlist_entry("http://localhost:8080/health"),
            Some("localhost".into())
        );
        assert_eq!(
            normalize_allowlist_entry("http://127.0.0.1:8080"),
            Some("127.0.0.1".into())
        );
        assert_eq!(
            normalize_allowlist_entry("*.example.com"),
            Some("example.com".into())
        );
        assert_eq!(
            normalize_allowlist_entry(".example.com"),
            Some("example.com".into())
        );
        assert_eq!(normalize_allowlist_entry("*"), None);
        assert_eq!(normalize_allowlist_entry(""), None);
    }

    #[test]
    fn parse_allowlist_csv_normalizes_and_dedupes() {
        let v = parse_allowlist_csv(
            " https://api.example.com/v1,example.com,api.example.com:443, ,*.example.com ",
        );
        assert_eq!(
            v,
            vec!["api.example.com".to_string(), "example.com".to_string()]
        );
    }

    #[test]
    fn registry_authorizes_by_username_and_token() {
        let reg = EgressPolicyRegistry::default();
        reg.register("w1", "t1".to_string(), vec!["example.com".to_string()]);

        assert_eq!(
            reg.allowlist_for_basic_auth("w1", "t1"),
            Some(vec!["example.com".to_string()])
        );
        assert_eq!(reg.allowlist_for_basic_auth("w1", "wrong"), None);
        assert_eq!(reg.allowlist_for_basic_auth("missing", "t1"), None);
    }

    #[test]
    fn test_generate_fw_rules() {
        use crate::capsule_types::capsule_v1::{EgressIdRule, NetworkConfig};

        let mut manifest = CapsuleManifestV1::from_toml(
            r#"
schema_version = "1.0"
name = "test-cap"
version = "0.0.1"
type = "tool"
[execution]
runtime = "native"
entrypoint = "echo"
"#,
        )
        .unwrap();

        // 1. Empty Network -> Default Drop
        let rules = generate_fw_rules(&manifest);
        assert!(rules.len() >= 6); // Basics
        assert!(rules.iter().any(|r| r.contains("-P OUTPUT DROP")));

        // 2. With Allowlist
        manifest.network = Some(NetworkConfig {
            egress_allow: vec![],
            egress_id_allow: vec![
                EgressIdRule {
                    rule_type: EgressIdType::Ip,
                    value: "1.1.1.1".to_string(),
                },
                EgressIdRule {
                    rule_type: EgressIdType::Cidr,
                    value: "10.0.0.0/8".to_string(),
                },
                EgressIdRule {
                    rule_type: EgressIdType::Ip,
                    value: "bad; rm -rf /".to_string(),
                }, // Should be filtered
            ],
        });

        let rules = generate_fw_rules(&manifest);
        assert!(rules.iter().any(|r| r.contains("-d 1.1.1.1")));
        assert!(rules.iter().any(|r| r.contains("-d 10.0.0.0/8")));
        assert!(!rules.iter().any(|r| r.contains("bad;")));
    }
}
