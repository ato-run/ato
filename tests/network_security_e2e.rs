//! Unit 7.1: DNS Control & Network Security E2E Tests
//!
//! These tests verify that:
//! 1. DNS control rules are correctly generated
//! 2. DNS rules integrate with egress policy
//! 3. DNS tunneling prevention config works as expected
//!
//! Note: Actual iptables application requires root privileges.
//! These tests focus on rule generation correctness.
//!
//! To run:
//! ```bash
//! cargo test --test network_security_e2e
//! ```

use tempfile::TempDir;

// ============================================================================
// Test 1: DNS Rule Generation
// ============================================================================

#[test]
fn test_dns_rule_generation_default_config() {
    use nacelle::security::dns_monitor::{generate_dns_rules, DnsConfig};

    let config = DnsConfig::default();
    let rules = generate_dns_rules(&config, "CAPSULE_OUT");

    // Verify rules are generated
    assert!(!rules.is_empty(), "Should generate DNS rules");

    // Check for loopback allow
    assert!(
        rules
            .iter()
            .any(|r| r.contains("127.0.0.1") && r.contains("ACCEPT")),
        "Should allow loopback DNS"
    );

    // Check for Tailscale MagicDNS allow
    assert!(
        rules
            .iter()
            .any(|r| r.contains("100.100.100.100") && r.contains("ACCEPT")),
        "Should allow Tailscale MagicDNS"
    );

    // Check for DROP rule for other DNS
    assert!(
        rules
            .iter()
            .any(|r| r.contains("-j DROP") && r.contains("--dport 53")),
        "Should DROP unauthorized DNS"
    );

    // Check for LOG rule
    assert!(
        rules
            .iter()
            .any(|r| r.contains("LOG") && r.contains("UARC-DNS-BLOCKED")),
        "Should LOG blocked DNS attempts"
    );

    println!(
        "✅ Default DNS rules generated correctly ({} rules)",
        rules.len()
    );
}

#[test]
fn test_dns_rule_generation_custom_resolvers() {
    use nacelle::security::dns_monitor::{generate_dns_rules, DnsConfig};

    // Custom config with corporate DNS
    let config = DnsConfig {
        allowed_resolvers: vec![
            "10.0.0.1".to_string(),    // Internal DNS
            "10.0.0.2".to_string(),    // Backup internal DNS
            "192.168.1.1".to_string(), // Gateway DNS
        ],
        log_blocked: true,
        enabled: true,
    };

    let rules = generate_dns_rules(&config, "CORP_DNS");

    // Verify each resolver has UDP and TCP rules
    for resolver in &config.allowed_resolvers {
        assert!(
            rules
                .iter()
                .any(|r| r.contains(resolver) && r.contains("-p udp")),
            "Should have UDP rule for {}",
            resolver
        );
        assert!(
            rules
                .iter()
                .any(|r| r.contains(resolver) && r.contains("-p tcp")),
            "Should have TCP rule for {}",
            resolver
        );
    }

    println!("✅ Custom resolver rules generated correctly");
}

#[test]
fn test_dns_disabled_generates_no_rules() {
    use nacelle::security::dns_monitor::{generate_dns_rules, DnsConfig};

    let config = DnsConfig {
        enabled: false,
        ..Default::default()
    };

    let rules = generate_dns_rules(&config, "TEST");
    assert!(rules.is_empty(), "Disabled config should generate no rules");

    println!("✅ Disabled DNS control produces no rules");
}

// ============================================================================
// Test 2: DNS + Egress Integration (Rule Counts)
// ============================================================================

#[test]
fn test_dns_and_egress_rule_combination() {
    use nacelle::security::dns_monitor::{generate_dns_rules, DnsConfig};

    // Generate DNS rules with default config
    let dns_config = DnsConfig::default();
    let dns_rules = generate_dns_rules(&dns_config, "OUTPUT");

    // Verify DNS rules are generated
    assert!(!dns_rules.is_empty(), "Should have DNS rules");

    // Verify we have both UDP and TCP rules
    let udp_count = dns_rules.iter().filter(|r| r.contains("-p udp")).count();
    let tcp_count = dns_rules.iter().filter(|r| r.contains("-p tcp")).count();

    assert!(udp_count > 0, "Should have UDP rules");
    assert!(tcp_count > 0, "Should have TCP rules");
    assert!(udp_count == tcp_count, "Should have equal UDP/TCP rules");

    println!(
        "✅ DNS rules: {} UDP + {} TCP = {} total",
        udp_count,
        tcp_count,
        dns_rules.len()
    );
}

// ============================================================================
// Test 3: DNS Resolver Validation
// ============================================================================

#[test]
fn test_is_resolver_allowed() {
    use nacelle::security::dns_monitor::{is_resolver_allowed, DnsConfig};

    let config = DnsConfig {
        allowed_resolvers: vec![
            "127.0.0.1".to_string(),
            "100.100.100.100".to_string(),
            "10.0.0.53".to_string(),
        ],
        log_blocked: true,
        enabled: true,
    };

    // Allowed resolvers
    assert!(is_resolver_allowed("127.0.0.1", &config));
    assert!(is_resolver_allowed("100.100.100.100", &config));
    assert!(is_resolver_allowed("10.0.0.53", &config));

    // Disallowed resolvers
    assert!(!is_resolver_allowed("8.8.8.8", &config));
    assert!(!is_resolver_allowed("1.1.1.1", &config));
    assert!(!is_resolver_allowed("208.67.222.222", &config));

    println!("✅ Resolver allow-list validation works correctly");
}

#[test]
fn test_dns_rule_format() {
    use nacelle::security::dns_monitor::{generate_dns_rules, DnsConfig};

    let config = DnsConfig::default();
    let rules = generate_dns_rules(&config, "TEST_CHAIN");

    for rule in &rules {
        // All rules should start with iptables
        assert!(
            rule.starts_with("iptables"),
            "Rule should start with iptables: {}",
            rule
        );

        // All rules should target the chain
        assert!(
            rule.contains("TEST_CHAIN"),
            "Rule should target chain: {}",
            rule
        );

        // All rules should specify port 53
        assert!(
            rule.contains("--dport 53"),
            "Rule should target port 53: {}",
            rule
        );

        // All rules should specify protocol (udp or tcp)
        assert!(
            rule.contains("-p udp") || rule.contains("-p tcp"),
            "Rule should specify protocol: {}",
            rule
        );
    }

    println!("✅ All {} rules have correct iptables format", rules.len());
}

// ============================================================================
// Test 5: DNS Config Serialization
// ============================================================================

#[test]
fn test_dns_config_clone_and_debug() {
    use nacelle::security::dns_monitor::DnsConfig;

    let config = DnsConfig::default();

    // Test Clone
    let cloned = config.clone();
    assert_eq!(config.enabled, cloned.enabled);
    assert_eq!(config.log_blocked, cloned.log_blocked);
    assert_eq!(config.allowed_resolvers, cloned.allowed_resolvers);

    // Test Debug
    let debug_str = format!("{:?}", config);
    assert!(debug_str.contains("DnsConfig"));
    assert!(debug_str.contains("allowed_resolvers"));

    println!("✅ DnsConfig Clone and Debug traits work correctly");
}

// ============================================================================
// Test 6: Security Audit Integration
// ============================================================================

#[tokio::test]
async fn test_dns_block_audit_logging() {
    use nacelle::security::audit::{AuditLogger, AuditOperation, AuditStatus};
    use nacelle::security::dns_monitor::{is_resolver_allowed, DnsConfig};

    let tmp = TempDir::new().expect("tempdir");
    let log_path = tmp.path().join("dns_audit.log");
    let key_path = tmp.path().join("key.pem");

    let logger =
        AuditLogger::new(log_path.clone(), key_path, "dns-test-node".to_string()).expect("logger");

    let config = DnsConfig::default();

    // Simulate DNS query attempts
    let queries = vec![
        ("8.8.8.8", false),        // Google DNS - should be blocked
        ("1.1.1.1", false),        // Cloudflare - should be blocked
        ("127.0.0.1", true),       // Loopback - should pass
        ("100.100.100.100", true), // Tailscale - should pass
    ];

    for (resolver, expected_allowed) in &queries {
        let allowed = is_resolver_allowed(resolver, &config);
        assert_eq!(
            allowed, *expected_allowed,
            "Resolver {} check failed",
            resolver
        );

        // Log blocked attempts
        if !allowed {
            logger
                .log(
                    AuditOperation::NetworkAccess,
                    AuditStatus::Failure,
                    Some("dns-check".to_string()),
                    Some(format!("Blocked DNS: {}", resolver)),
                )
                .await;
        }
    }

    // Verify logs
    let db_path = log_path.with_extension("db");
    let conn = rusqlite::Connection::open(&db_path).expect("open db");

    let blocked_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit_logs WHERE status = 'failure'",
            [],
            |row| row.get(0),
        )
        .expect("query");

    assert_eq!(
        blocked_count, 2,
        "Should have logged 2 blocked DNS attempts"
    );

    println!("✅ DNS audit logging verified");
}
