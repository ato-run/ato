//! Typed network receipt event structs — Slice D (#299).
//!
//! These types represent the "what happened on the network" side of Ato's
//! audit trail.  They are designed to be:
//!
//! - **Serialisable to JSON** so they can be written to receipt files or
//!   forwarded over the ato-netd control socket.
//! - **Composable** — a [`NetworkReceiptEvent`] is a tagged union so a
//!   receipt stream can carry DNS, egress, and future event kinds in one
//!   channel.
//! - **Wired up later** — connection to the running daemon and session
//!   recording is out of scope for this slice.  These are plain data types.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::resolver::ResolvedRecord;

// ── DnsResolutionRecord ───────────────────────────────────────────────────────

/// A complete record of one DNS resolution attempt, suitable for inclusion
/// in an Ato session receipt.
///
/// Mirrors [`crate::resolver::ResolvedRecord`] but drops non-serialisable
/// fields and adds receipt-specific metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DnsResolutionRecord {
    /// The name that was queried.
    pub name: String,

    /// CNAME traversal chain (may be empty).
    pub cname_chain: Vec<String>,

    /// Resolved IPv4 addresses.
    pub addrs_v4: Vec<std::net::Ipv4Addr>,

    /// Resolved IPv6 addresses.
    pub addrs_v6: Vec<std::net::Ipv6Addr>,

    /// TTL of the first address record, in seconds.
    pub ttl_seconds: Option<u32>,

    /// Which resolver backend answered (e.g. `"system"`, `"doh:https://…"`).
    pub backend: String,

    /// Present when a [`crate::resolver::Chain`] fell back to this backend.
    pub fallback_reason: Option<String>,

    /// Wall-clock timestamp (Unix seconds) at which the query was issued.
    pub queried_at_unix: u64,

    /// Resolution latency in milliseconds.
    pub latency_ms: u32,

    /// `true` when the resolution succeeded; `false` on error.
    pub success: bool,

    /// Human-readable error string when `success = false`.
    pub error: Option<String>,
}

impl DnsResolutionRecord {
    /// Construct a success record from a [`ResolvedRecord`] plus timing.
    pub fn from_resolved(
        record: &ResolvedRecord,
        queried_at_unix: u64,
        latency_ms: u32,
    ) -> Self {
        Self {
            name: record.name.clone(),
            cname_chain: record.cname_chain.clone(),
            addrs_v4: record.addrs_v4.clone(),
            addrs_v6: record.addrs_v6.clone(),
            ttl_seconds: record.ttl_seconds,
            backend: record.backend.clone(),
            fallback_reason: record.fallback_reason.clone(),
            queried_at_unix,
            latency_ms,
            success: true,
            error: None,
        }
    }

    /// Construct a failure record.
    pub fn from_error(
        name: &str,
        backend: &str,
        error: &str,
        queried_at_unix: u64,
        latency_ms: u32,
    ) -> Self {
        Self {
            name: name.to_string(),
            cname_chain: vec![],
            addrs_v4: vec![],
            addrs_v6: vec![],
            ttl_seconds: None,
            backend: backend.to_string(),
            fallback_reason: None,
            queried_at_unix,
            latency_ms,
            success: false,
            error: Some(error.to_string()),
        }
    }
}

// ── NetworkEgressDecision ─────────────────────────────────────────────────────

/// Outcome of one egress policy check for an outbound connection attempt.
///
/// This is a placeholder for Slice E (#300), which will implement the full
/// HTTP CONNECT egress proxy and policy engine.  The struct is defined here
/// in Slice D so receipt consumers can name the type without a
/// source-incompatible change later.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NetworkEgressDecision {
    /// Target hostname or IP.
    pub target: String,

    /// Target port.
    pub port: u16,

    /// Protocol (e.g. `"tcp"`, `"http"`, `"https"`).
    pub protocol: String,

    /// Policy decision.
    pub decision: EgressDecision,

    /// Resolved address used for the outbound connection, if allowed.
    pub resolved_addr: Option<IpAddr>,

    /// Wall-clock timestamp (Unix seconds) at the point of the decision.
    pub decided_at_unix: u64,
}

/// The outcome of an egress policy check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EgressDecision {
    /// Connection is allowed to proceed.
    Allow,
    /// Connection is blocked by policy.
    Deny,
    /// Connection is proxied through the ato-netd egress proxy.
    Proxy,
}

// ── NetworkReceiptEvent ───────────────────────────────────────────────────────

/// A single event in the network receipt stream for an Ato session.
///
/// The `#[serde(tag = "kind", rename_all = "snake_case")]` annotation means
/// JSON looks like: `{"kind": "dns_resolution", …fields…}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NetworkReceiptEvent {
    /// A DNS resolution was performed.
    DnsResolution(DnsResolutionRecord),

    /// An egress connection attempt was evaluated by policy.
    EgressDecision(NetworkEgressDecision),
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    #[test]
    fn receipt_event_dns_roundtrip() {
        let record = DnsResolutionRecord {
            name: "example.com".to_string(),
            cname_chain: vec!["www.example.com".to_string()],
            addrs_v4: vec!["1.2.3.4".parse().unwrap()],
            addrs_v6: vec![],
            ttl_seconds: Some(300),
            backend: "system".to_string(),
            fallback_reason: None,
            queried_at_unix: 1_700_000_000,
            latency_ms: 12,
            success: true,
            error: None,
        };
        let event = NetworkReceiptEvent::DnsResolution(record.clone());
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"kind\":\"dns_resolution\""));
        let back: NetworkReceiptEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn receipt_event_egress_roundtrip() {
        let decision = NetworkEgressDecision {
            target: "api.example.com".to_string(),
            port: 443,
            protocol: "https".to_string(),
            decision: EgressDecision::Allow,
            resolved_addr: Some("1.2.3.4".parse().unwrap()),
            decided_at_unix: 1_700_000_001,
        };
        let event = NetworkReceiptEvent::EgressDecision(decision);
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"kind\":\"egress_decision\""));
        let back: NetworkReceiptEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn egress_decision_deny_serde() {
        let d = EgressDecision::Deny;
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(json, r#""deny""#);
    }
}
