//! Egress policy: hostname and CIDR-based allow / deny rules.
//!
//! ## Semantics
//!
//! - **Deny wins over allow**: if a hostname or resolved IP matches a deny
//!   rule it is blocked regardless of any allow rules.
//! - **Empty policy = permissive-with-receipts**: an `EgressPolicy::permissive()`
//!   allows all outbound connections (useful for Slice E deployment before
//!   policy authoring is ready).
//! - **Hostname normalization**: all comparisons lower-case the hostname and
//!   strip a trailing dot so `EXAMPLE.COM.` matches `example.com`.

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ipnet::IpNet;

/// Bytes relayed through every tunnel of one policy, in both directions.
/// Once the limit is crossed the budget is exhausted: open tunnels are cut
/// and new CONNECTs are refused.
#[derive(Debug)]
pub struct TransferBudget {
    limit: u64,
    used: AtomicU64,
    exhausted: AtomicBool,
}

impl TransferBudget {
    pub fn new(limit: u64) -> Self {
        Self {
            limit,
            used: AtomicU64::new(0),
            exhausted: AtomicBool::new(false),
        }
    }

    /// Account `bytes`; false once the total would exceed the limit.
    pub fn consume(&self, bytes: u64) -> bool {
        let total = self.used.fetch_add(bytes, Ordering::AcqRel) + bytes;
        if total > self.limit {
            self.exhausted.store(true, Ordering::Release);
        }
        !self.exhausted()
    }

    pub fn used(&self) -> u64 {
        self.used.load(Ordering::Acquire)
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    pub fn exhausted(&self) -> bool {
        self.exhausted.load(Ordering::Acquire)
    }
}

/// The outcome of an egress policy check at one stage of the pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    /// Connection is allowed to proceed to the next stage.
    Allow,
    /// Hostname matched a deny rule; DNS must **not** be called.
    DenyHost,
    /// Resolved IP fell inside a denied CIDR range (DNS-rebinding guard).
    DenyCidr,
}

/// Hostname and CIDR policy for outbound CONNECT requests.
///
/// Construct with [`EgressPolicy::permissive`] for a no-restriction
/// baseline or build up deny lists manually for testing / production.
#[derive(Debug, Clone, Default)]
pub struct EgressPolicy {
    /// Exact hostname deny list (normalized: lower-case, trailing dot stripped).
    hostname_deny: Vec<String>,
    /// If non-empty, only hostnames in this list are allowed past the
    /// hostname stage. Deny still wins over allow when both apply.
    hostname_allow: Vec<String>,
    /// CIDR ranges that are denied after DNS resolution.
    cidr_deny: Vec<IpNet>,
    /// If non-empty, only these destination ports are allowed.
    port_allow: Vec<u16>,
    /// Optional limit on the bytes relayed through all tunnels.
    transfer_budget: Option<Arc<TransferBudget>>,
}

impl EgressPolicy {
    /// No hostname or CIDR restrictions — all connections are permitted.
    pub fn permissive() -> Self {
        Self::default()
    }

    /// Builder: add a hostname deny entry.
    #[allow(dead_code)]
    pub fn with_hostname_deny(mut self, host: impl Into<String>) -> Self {
        self.hostname_deny.push(normalize_hostname(&host.into()));
        self
    }

    /// Builder: add a hostname allow entry.
    #[allow(dead_code)]
    pub fn with_hostname_allow(mut self, host: impl Into<String>) -> Self {
        self.hostname_allow.push(normalize_hostname(&host.into()));
        self
    }

    /// Builder: add a CIDR deny entry.
    #[allow(dead_code)]
    pub fn with_cidr_deny(mut self, cidr: IpNet) -> Self {
        self.cidr_deny.push(cidr);
        self
    }

    /// Builder: allow only the listed destination ports.
    pub fn with_port_allow(mut self, port: u16) -> Self {
        self.port_allow.push(port);
        self
    }

    /// Builder: share one transfer budget across every tunnel.
    pub fn with_transfer_budget(mut self, budget: Arc<TransferBudget>) -> Self {
        self.transfer_budget = Some(budget);
        self
    }

    /// Whether `port` passes a non-empty port allow list.
    pub fn check_port(&self, port: u16) -> bool {
        self.port_allow.is_empty() || self.port_allow.contains(&port)
    }

    pub fn transfer_budget(&self) -> Option<&Arc<TransferBudget>> {
        self.transfer_budget.as_ref()
    }

    /// Evaluate the hostname *before* DNS resolution.
    ///
    /// Returns [`PolicyDecision::DenyHost`] if the hostname matches a deny
    /// rule, or if a non-empty allow list does not include the hostname.
    /// Returns [`PolicyDecision::Allow`] otherwise.
    ///
    /// IP-literal targets are checked too: with a non-empty allow list an
    /// IP literal is denied unless it is itself listed, so the allowlist
    /// cannot be bypassed by dialing the resolved IP directly.
    ///
    /// The resolver must **not** be called when this returns `DenyHost`.
    pub fn check_hostname(&self, host: &str) -> PolicyDecision {
        let h = normalize_hostname(host);

        // Deny always wins.
        if self.hostname_deny.contains(&h) {
            return PolicyDecision::DenyHost;
        }

        // Non-empty allow-list: host must be present.
        if !self.hostname_allow.is_empty() && !self.hostname_allow.contains(&h) {
            return PolicyDecision::DenyHost;
        }

        PolicyDecision::Allow
    }

    /// Evaluate a resolved IP address against the CIDR deny list.
    ///
    /// Returns [`PolicyDecision::DenyCidr`] if the address falls inside
    /// any denied network, [`PolicyDecision::Allow`] otherwise.
    pub fn check_addr(&self, addr: IpAddr) -> PolicyDecision {
        if self.cidr_deny.iter().any(|net| net.contains(&addr)) {
            PolicyDecision::DenyCidr
        } else {
            PolicyDecision::Allow
        }
    }
}

/// Normalise a hostname for policy comparisons: lowercase + strip trailing dot.
///
/// IP literals are canonicalised via parse-then-format so textual variants
/// of the same address (e.g. `0:0:0:0:0:0:0:1` and `::1`) match the same
/// policy entry.
pub(crate) fn normalize_hostname(host: &str) -> String {
    let h = host.trim_end_matches('.').to_lowercase();
    match h.parse::<IpAddr>() {
        Ok(ip) => ip.to_string(),
        Err(_) => h,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissive_allows_any_hostname() {
        let p = EgressPolicy::permissive();
        assert_eq!(p.check_hostname("example.com"), PolicyDecision::Allow);
        assert_eq!(p.check_hostname("evil.internal"), PolicyDecision::Allow);
    }

    #[test]
    fn hostname_deny_blocks() {
        let p = EgressPolicy::permissive().with_hostname_deny("denied.test");
        assert_eq!(p.check_hostname("denied.test"), PolicyDecision::DenyHost);
        assert_eq!(p.check_hostname("allowed.test"), PolicyDecision::Allow);
    }

    #[test]
    fn deny_wins_over_allow() {
        let p = EgressPolicy::permissive()
            .with_hostname_deny("conflict.test")
            .with_hostname_allow("conflict.test");
        // Deny must win even though host is also in the allow list.
        assert_eq!(p.check_hostname("conflict.test"), PolicyDecision::DenyHost);
    }

    #[test]
    fn allowlist_blocks_unlisted_hosts() {
        let p = EgressPolicy::permissive().with_hostname_allow("allowed.test");
        assert_eq!(p.check_hostname("allowed.test"), PolicyDecision::Allow);
        assert_eq!(p.check_hostname("other.test"), PolicyDecision::DenyHost);
    }

    #[test]
    fn allowlist_blocks_unlisted_ip_literals() {
        // Regression (#655): a non-empty allowlist must not be bypassed by
        // dialing an IP literal directly.
        let p = EgressPolicy::permissive().with_hostname_allow("allowed.test");
        assert_eq!(p.check_hostname("93.184.216.34"), PolicyDecision::DenyHost);
        assert_eq!(p.check_hostname("::1"), PolicyDecision::DenyHost);
    }

    #[test]
    fn allowlisted_ip_literal_is_allowed() {
        let p = EgressPolicy::permissive().with_hostname_allow("93.184.216.34");
        assert_eq!(p.check_hostname("93.184.216.34"), PolicyDecision::Allow);
    }

    #[test]
    fn ip_literal_normalization_matches_textual_variants() {
        // Parse-then-format canonicalisation: `0:0:0:0:0:0:0:1` ≡ `::1`.
        let p = EgressPolicy::permissive().with_hostname_deny("0:0:0:0:0:0:0:1");
        assert_eq!(p.check_hostname("::1"), PolicyDecision::DenyHost);
    }

    #[test]
    fn hostname_normalization_case_and_trailing_dot() {
        let p = EgressPolicy::permissive().with_hostname_deny("DENIED.TEST");
        // Case-insensitive
        assert_eq!(p.check_hostname("denied.test"), PolicyDecision::DenyHost);
        assert_eq!(p.check_hostname("DENIED.TEST"), PolicyDecision::DenyHost);
        // Trailing dot stripped
        assert_eq!(p.check_hostname("DENIED.TEST."), PolicyDecision::DenyHost);
    }

    #[test]
    fn cidr_deny_blocks_matching_ip() {
        let p = EgressPolicy::permissive().with_cidr_deny("192.168.0.0/16".parse().unwrap());
        let private: IpAddr = "192.168.1.1".parse().unwrap();
        let public: IpAddr = "8.8.8.8".parse().unwrap();
        assert_eq!(p.check_addr(private), PolicyDecision::DenyCidr);
        assert_eq!(p.check_addr(public), PolicyDecision::Allow);
    }

    #[test]
    fn port_allowlist_blocks_unlisted_ports() {
        let p = EgressPolicy::permissive();
        assert!(p.check_port(22));
        let p = p.with_port_allow(443).with_port_allow(80);
        assert!(p.check_port(443) && p.check_port(80));
        assert!(!p.check_port(22));
    }

    #[test]
    fn a_transfer_budget_is_exhausted_once_crossed() {
        let budget = TransferBudget::new(10);
        assert!(budget.consume(6));
        assert!(budget.consume(4));
        assert!(!budget.exhausted());
        assert!(!budget.consume(1));
        assert!(budget.exhausted());
        assert_eq!(budget.used(), 11);
    }

    #[test]
    fn permissive_allows_any_ip() {
        let p = EgressPolicy::permissive();
        let addr: IpAddr = "10.0.0.1".parse().unwrap();
        assert_eq!(p.check_addr(addr), PolicyDecision::Allow);
    }
}
