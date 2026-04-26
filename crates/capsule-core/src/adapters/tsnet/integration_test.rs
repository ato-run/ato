//! Integration test for tsnet sidecar egress filtering
//!
//! Tests the pattern matching logic used for egress allowlist filtering.
//! Mirrors the Go implementation in ato-tsnetd/sidecar.go

#[cfg(test)]
mod tests {
    #[test]
    fn test_pattern_matching_exact_domain() {
        // Exact match works
        assert!(matches_pattern("example.com", "example.com"));
        assert!(matches_pattern("example.com:443", "example.com"));
        assert!(!matches_pattern("test.example.com", "example.com"));
        assert!(!matches_pattern("example.com", "test.com"));
    }

    #[test]
    fn test_pattern_matching_wildcard() {
        // Wildcard subdomain matching (*.example.com)
        // Only matches if there's a dot prefix
        assert!(matches_pattern("test.example.com", "*.example.com"));
        assert!(matches_pattern("test.example.com:443", "*.example.com"));
        assert!(matches_pattern("a.b.example.com", "*.example.com"));
        assert!(matches_pattern("deep.nested.example.com", "*.example.com"));

        // Wildcard should NOT match the base domain itself (no dot prefix)
        assert!(!matches_pattern("example.com", "*.example.com"));
        assert!(!matches_pattern("notexample.com", "*.example.com"));

        // Wildcard should NOT match domains that just happen to end with the same suffix
        assert!(!matches_pattern("notgithub.com", "*.github.com"));
        assert!(!matches_pattern("evilgithub.com", "*.github.com"));
    }

    #[test]
    fn test_pattern_matching_cidr() {
        // Class A private network (10.0.0.0/8)
        assert!(matches_pattern("10.0.0.1", "10.0.0.0/8"));
        assert!(matches_pattern("10.0.0.1:443", "10.0.0.0/8"));
        assert!(matches_pattern("10.255.255.255", "10.0.0.0/8"));
        assert!(!matches_pattern("11.0.0.1", "10.0.0.0/8"));

        // Class B private network (172.16.0.0/12)
        assert!(matches_pattern("172.16.0.1", "172.16.0.0/12"));
        assert!(matches_pattern("172.31.255.255", "172.16.0.0/12"));
        assert!(!matches_pattern("172.32.0.1", "172.16.0.0/12"));

        // Class C private network (192.168.0.0/16)
        assert!(matches_pattern("192.168.1.100", "192.168.0.0/16"));
        assert!(matches_pattern("192.168.255.255", "192.168.0.0/16"));
        assert!(!matches_pattern("192.169.1.100", "192.168.0.0/16"));
    }

    #[test]
    fn test_pattern_matching_empty() {
        // Empty pattern only matches empty string
        assert!(matches_pattern("", ""));
        assert!(!matches_pattern("anything.com", ""));
    }

    #[test]
    fn test_pattern_matching_case_sensitivity() {
        // Go's net package is case-sensitive for domain names
        assert!(!matches_pattern("EXAMPLE.COM", "example.com"));
        assert!(!matches_pattern("example.com", "EXAMPLE.COM"));
        assert!(matches_pattern("example.com", "example.com"));
    }

    #[test]
    fn test_allowlist_examples() {
        // Common use cases from capsule.toml
        let allowlist = vec![
            "google.com".to_string(),   // Exact match
            "*.github.com".to_string(), // Wildcard match
            "10.0.0.0/8".to_string(),   // CIDR match
        ];

        // Should be allowed - exact domain match
        assert!(is_allowed("google.com", &allowlist));
        // Should be allowed - wildcard match
        assert!(is_allowed("www.github.com", &allowlist));
        assert!(is_allowed("api.github.com", &allowlist));
        assert!(is_allowed("api.github.com:443", &allowlist));
        // Should be allowed - CIDR match
        assert!(is_allowed("10.5.5.5", &allowlist));

        // Should be blocked - not in allowlist
        assert!(!is_allowed("example.com", &allowlist));
        assert!(!is_allowed("google.org", &allowlist));
        assert!(!is_allowed("notgithub.com", &allowlist));
        assert!(!is_allowed("172.16.0.1", &allowlist));

        // google.com with *.google.com in allowlist would NOT match
        // (no dot prefix, so it's not a proper subdomain match)
        let strict_wildcard_allowlist = vec!["*.google.com".to_string()];
        assert!(!is_allowed("google.com", &strict_wildcard_allowlist));
        assert!(is_allowed("www.google.com", &strict_wildcard_allowlist));
    }

    #[test]
    fn test_best_effort_mode() {
        // Empty allowlist = allow all (best_effort mode)
        let empty_allowlist: Vec<String> = vec![];
        assert!(is_allowed("google.com", &empty_allowlist));
        assert!(is_allowed("example.com", &empty_allowlist));
        assert!(is_allowed("anything.com", &empty_allowlist));
    }

    #[test]
    fn test_user_friendly_patterns() {
        // Users often want to write "google.com" and have it match "www.google.com"
        // This is NOT the default behavior, but we can document it

        let strict_allowlist = vec!["google.com".to_string()];
        let wildcard_allowlist = vec!["*.google.com".to_string()];

        // Strict: only exact match
        assert!(is_allowed("google.com", &strict_allowlist));
        assert!(!is_allowed("www.google.com", &strict_allowlist));

        // Wildcard: matches subdomains (NOT the base domain itself)
        assert!(!is_allowed("google.com", &wildcard_allowlist));
        assert!(is_allowed("www.google.com", &wildcard_allowlist));
        assert!(is_allowed("mail.google.com", &wildcard_allowlist));
    }

    // Core pattern matching function (mirrors Go implementation in sidecar.go)
    fn matches_pattern(addr: &str, pattern: &str) -> bool {
        let addr = addr.trim();
        let pattern = pattern.trim();

        let addr = if let Some((host, _)) = addr.rsplit_once(':') {
            host
        } else {
            addr
        };

        if addr == pattern {
            return true;
        }

        if let Some(domain) = pattern.strip_prefix("*.") {
            if addr.ends_with(domain) {
                let base_with_dot = format!(".{}", domain);
                if addr.contains(&base_with_dot) {
                    return true;
                }
            }
        }

        if pattern.contains('/') {
            let parts: Vec<&str> = pattern.split('/').collect();
            if parts.len() == 2 {
                if let (Ok(ip), Ok(mask)) = (
                    parts[0].parse::<std::net::Ipv4Addr>(),
                    parts[1].parse::<u8>(),
                ) {
                    if let Ok(addr_ip) = addr.parse::<std::net::Ipv4Addr>() {
                        let host_mask = !0u32 << (32 - mask);
                        return (addr_ip.to_bits() & host_mask) == (ip.to_bits() & host_mask);
                    }
                }
            }
        }

        false
    }

    // Helper function to check if an address is in the allowlist
    fn is_allowed(addr: &str, allowlist: &[String]) -> bool {
        if allowlist.is_empty() {
            return true; // Empty allowlist = allow all (best_effort mode)
        }
        allowlist.iter().any(|p| matches_pattern(addr, p))
    }
}
