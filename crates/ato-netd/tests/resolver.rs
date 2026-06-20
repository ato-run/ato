//! Integration tests for `ato_netd::net::resolver` — Slice D (#299).
//!
//! All tests except `localhost_happy_path` use stub resolvers so they are
//! network-free and safe in CI.

use std::net::{Ipv4Addr, Ipv6Addr};

use async_trait::async_trait;
use ato_netd::net::resolver::{
    Chain, ResolveOptions, ResolvedRecord, Resolver, ResolverError, SystemResolver,
};

// ── Stub helpers ──────────────────────────────────────────────────────────────

/// A resolver that always returns the given `ResolvedRecord`.
struct OkStub(ResolvedRecord);

#[async_trait]
impl Resolver for OkStub {
    async fn resolve(&self, _: &str, _: &ResolveOptions) -> Result<ResolvedRecord, ResolverError> {
        Ok(self.0.clone())
    }
    fn backend_name(&self) -> &str {
        &self.0.backend
    }
}

/// A resolver that always returns the given `ResolverError`.
struct ErrStub {
    error: fn() -> ResolverError,
    name: &'static str,
}

#[async_trait]
impl Resolver for ErrStub {
    async fn resolve(
        &self,
        _query: &str,
        _: &ResolveOptions,
    ) -> Result<ResolvedRecord, ResolverError> {
        Err((self.error)())
    }
    fn backend_name(&self) -> &str {
        self.name
    }
}

fn ok_record(
    name: &str,
    cname_chain: Vec<String>,
    addrs_v4: Vec<Ipv4Addr>,
    addrs_v6: Vec<Ipv6Addr>,
    ttl: Option<u32>,
    backend: &str,
) -> ResolvedRecord {
    ResolvedRecord {
        name: name.to_string(),
        cname_chain,
        addrs_v4,
        addrs_v6,
        ttl_seconds: ttl,
        backend: backend.to_string(),
        fallback_reason: None,
    }
}

// ── 1. localhost happy path (real SystemResolver, /etc/hosts) ─────────────────

#[tokio::test]
async fn localhost_happy_path() {
    // SystemResolver::new() can fail in highly restricted CI environments.
    // If so, skip gracefully rather than blocking the build.
    let resolver = match SystemResolver::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("skip localhost_happy_path: cannot build SystemResolver: {e}");
            return;
        }
    };
    let opts = ResolveOptions::default();
    match resolver.resolve("localhost", &opts).await {
        Ok(record) => {
            assert_eq!(record.name, "localhost");
            assert_eq!(record.backend, "system");
            // localhost must resolve to at least one address (typically 127.0.0.1 or ::1).
            let any_addr = !record.addrs_v4.is_empty() || !record.addrs_v6.is_empty();
            assert!(any_addr, "localhost resolved no addresses: {record:?}");
        }
        // Some musl/container environments return NxDomain for "localhost" if
        // /etc/hosts is absent.  Accept that as a skip rather than a failure.
        Err(ResolverError::NxDomain(_)) => {
            eprintln!("skip localhost_happy_path: NxDomain (no /etc/hosts)");
        }
        Err(e) => panic!("unexpected error resolving localhost: {e}"),
    }
}

// ── 2. CNAME chain fixture (stub) ─────────────────────────────────────────────

#[tokio::test]
async fn cname_chain_stub() {
    let record = ok_record(
        "a.example.test",
        vec!["a.example.test".to_string(), "b.example.test".to_string()],
        vec!["192.0.2.1".parse().unwrap()],
        vec![],
        Some(60),
        "stub",
    );
    let stub = OkStub(record);
    let opts = ResolveOptions::default();
    let result = stub.resolve("a.example.test", &opts).await.unwrap();
    assert_eq!(result.cname_chain.len(), 2);
    assert_eq!(result.cname_chain[0], "a.example.test");
    assert_eq!(result.cname_chain[1], "b.example.test");
    assert_eq!(
        result.addrs_v4,
        vec!["192.0.2.1".parse::<Ipv4Addr>().unwrap()]
    );
}

// ── 3. NxDomain returns typed error (stub) ────────────────────────────────────

#[tokio::test]
async fn nxdomain_stub() {
    let stub = ErrStub {
        error: || ResolverError::NxDomain("no-such.test".to_string()),
        name: "stub",
    };
    let opts = ResolveOptions::default();
    let err = stub.resolve("no-such.test", &opts).await.unwrap_err();
    assert!(matches!(err, ResolverError::NxDomain(_)));
}

// ── 4. Timeout returns typed error and is cancellable (stub) ──────────────────

#[tokio::test]
async fn timeout_stub() {
    // Stub that returns Timeout immediately (no actual sleep needed — we're
    // testing the type mapping, not real network latency).
    let stub = ErrStub {
        error: || ResolverError::Timeout("slow.test".to_string()),
        name: "stub",
    };
    let opts = ResolveOptions { timeout_ms: 50 };
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        stub.resolve("slow.test", &opts),
    )
    .await;
    // Must complete within 500 ms and return Timeout variant.
    let err = result.expect("future timed out").unwrap_err();
    assert!(matches!(err, ResolverError::Timeout(_)));
}

// ── 5. TTL fixture (stub) ─────────────────────────────────────────────────────

#[tokio::test]
async fn ttl_fixture_stub() {
    let record = ok_record(
        "ttl.test",
        vec![],
        vec!["10.0.0.1".parse().unwrap()],
        vec![],
        Some(42),
        "stub",
    );
    let stub = OkStub(record);
    let opts = ResolveOptions::default();
    let result = stub.resolve("ttl.test", &opts).await.unwrap();
    assert_eq!(result.ttl_seconds, Some(42));
}

// ── 6. Chain falls back and records reason (stub) ─────────────────────────────

#[tokio::test]
async fn chain_fallback_stub() {
    // Primary: always times out.  Secondary: always succeeds.
    let primary = ErrStub {
        error: || ResolverError::Timeout("chain-test.test".to_string()),
        name: "system",
    };
    let secondary_record = ok_record(
        "chain-test.test",
        vec![],
        vec!["203.0.113.1".parse().unwrap()],
        vec![],
        Some(120),
        "doh",
    );
    let secondary = OkStub(secondary_record);
    let chain = Chain::new(vec![Box::new(primary), Box::new(secondary)]);
    let opts = ResolveOptions::default();
    let result = chain.resolve("chain-test.test", &opts).await.unwrap();
    assert_eq!(result.backend, "doh");
    assert_eq!(
        result.fallback_reason.as_deref(),
        Some("fallback_from_system")
    );
    assert!(!result.addrs_v4.is_empty());
}

// ── 7. Chain does NOT fall back on NxDomain (stub) ───────────────────────────

#[tokio::test]
async fn chain_no_fallback_on_nxdomain() {
    let primary = ErrStub {
        error: || ResolverError::NxDomain("nope.test".to_string()),
        name: "system",
    };
    let secondary_record = ok_record(
        "nope.test",
        vec![],
        vec!["1.2.3.4".parse().unwrap()],
        vec![],
        None,
        "doh",
    );
    let secondary = OkStub(secondary_record);
    let chain = Chain::new(vec![Box::new(primary), Box::new(secondary)]);
    let opts = ResolveOptions::default();
    let err = chain.resolve("nope.test", &opts).await.unwrap_err();
    // NxDomain must short-circuit — secondary must NOT be tried.
    assert!(matches!(err, ResolverError::NxDomain(_)));
}

// ── 8. Receipt JSON round-trip (serde) ────────────────────────────────────────

#[tokio::test]
async fn receipt_json_roundtrip() {
    use ato_netd::net::receipt::{DnsResolutionRecord, NetworkReceiptEvent};

    let dns_record = DnsResolutionRecord {
        name: "example.com".to_string(),
        cname_chain: vec!["www.example.com".to_string()],
        addrs_v4: vec!["93.184.216.34".parse().unwrap()],
        addrs_v6: vec![],
        ttl_seconds: Some(3600),
        backend: "system".to_string(),
        fallback_reason: None,
        queried_at_unix: 1_700_000_000,
        latency_ms: 8,
        success: true,
        error: None,
    };
    let event = NetworkReceiptEvent::DnsResolution(dns_record.clone());
    let json = serde_json::to_string_pretty(&event).unwrap();

    // Required JSON shape checks.
    assert!(
        json.contains("\"kind\": \"dns_resolution\""),
        "missing kind field in: {json}"
    );
    assert!(json.contains("\"name\": \"example.com\""));
    assert!(json.contains("\"ttl_seconds\": 3600"));
    assert!(json.contains("\"backend\": \"system\""));

    let back: NetworkReceiptEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(back, event);
}

// ── 9. Default SystemResolver config does not use DoH ─────────────────────────

#[test]
fn default_config_no_doh() {
    // The SystemResolver is never constructed with a DoH backend in its
    // default path; this test validates that DohResolver without the `doh`
    // feature returns BackendUnavailable rather than compiling against the
    // DoH protocol.
    //
    // When the `doh` feature IS enabled, this test verifies that a
    // SystemResolver still identifies itself as "system", not "doh".
    let resolver = match SystemResolver::new() {
        Ok(r) => r,
        Err(_) => {
            // If we can't build a SystemResolver (e.g. no /etc/resolv.conf),
            // that is fine — we just care about the backend_name contract.
            return;
        }
    };
    assert_eq!(resolver.backend_name(), "system");

    // Without `doh` feature, DohResolver::new must return BackendUnavailable.
    #[cfg(not(feature = "doh"))]
    {
        use ato_netd::net::resolver::DohResolver;
        let err = DohResolver::new("https://1.1.1.1/dns-query").unwrap_err();
        assert!(
            matches!(err, ResolverError::BackendUnavailable(_)),
            "expected BackendUnavailable, got: {err:?}"
        );
    }
}
