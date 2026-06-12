//! Egress allowlist policy for `ato://cli` REPL sessions.
//!
//! Philosophy: deny-by-default, localhost always allowed, everything else
//! must be explicitly granted via one of:
//!   1. `~/.ato/config.toml` `[cli.network] default_egress_allow = [...]`
//!   2. REPL meta-command `.allow <pattern>` (session-only)
//!   3. Interactive prompt when a child is blocked (future phase)
//!
//! This module is intentionally dependency-light: we parse patterns with
//! the stdlib only and avoid pulling `ipnet`/`toml` until Phase 3. CIDR
//! parsing is deferred to when the proxy enforcement layer lands.

use std::net::IpAddr;
use std::str::FromStr;

/// How a host/port pair should be treated by the REPL egress gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Proceed (allowlist match or localhost).
    Allow,
    /// Not allowed, but user can grant with `.allow` (phase 4 prompt).
    DenyAskUser,
    /// Hard deny; no amount of user grant can permit this.
    ///
    /// Emitted by [`EgressPolicy::decide_resolved`] when an allowed
    /// hostname resolves to a non-public address (DNS-rebinding guard).
    DenyFinal,
}

/// A single host pattern in the allowlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostPattern {
    /// Exact hostname match (case-insensitive).
    Exact(String),
    /// Suffix match, e.g. `.github.com` matches `api.github.com`
    /// but not `github.com` itself. A pattern starting with `*.`
    /// is normalised to a suffix.
    Suffix(String),
    /// Exact IPv4/IPv6 literal.
    Ip(IpAddr),
    /// Localhost family: `127.0.0.0/8`, `::1`, and the literal strings
    /// `localhost` / `localhost.localdomain`. Always present; cannot
    /// be removed.
    Localhost,
}

impl HostPattern {
    /// Parse a user-supplied pattern string.
    ///
    /// Accepted forms:
    ///   - `example.com`            → Exact
    ///   - `*.example.com`          → Suffix(".example.com")
    ///   - `.example.com`           → Suffix(".example.com")
    ///   - `1.2.3.4` / `::1`        → Ip
    ///   - `localhost`              → Localhost
    pub fn parse(raw: &str) -> Result<Self, String> {
        let s = raw.trim();
        if s.is_empty() {
            return Err("empty pattern".to_string());
        }
        if s.eq_ignore_ascii_case("localhost") || s.eq_ignore_ascii_case("localhost.localdomain") {
            return Ok(HostPattern::Localhost);
        }
        if let Ok(ip) = IpAddr::from_str(s) {
            return Ok(HostPattern::Ip(ip));
        }
        if let Some(rest) = s.strip_prefix("*.") {
            if rest.is_empty() {
                return Err("bare '*.' is not a valid pattern".to_string());
            }
            return Ok(HostPattern::Suffix(format!(
                ".{}",
                rest.to_ascii_lowercase()
            )));
        }
        if let Some(rest) = s.strip_prefix('.') {
            if rest.is_empty() {
                return Err("bare '.' is not a valid pattern".to_string());
            }
            return Ok(HostPattern::Suffix(format!(
                ".{}",
                rest.to_ascii_lowercase()
            )));
        }
        // Reject obvious garbage (whitespace, slashes, protocol).
        if s.contains(|c: char| c.is_whitespace() || c == '/' || c == ':' && !s.contains("::")) {
            return Err(format!("invalid host pattern: {s}"));
        }
        Ok(HostPattern::Exact(s.to_ascii_lowercase()))
    }

    /// Does this pattern match the given host string?
    ///
    /// The host is expected to be a DNS name or IP literal — the caller
    /// strips port / scheme / path before reaching us.
    pub fn matches(&self, host: &str) -> bool {
        let host_lc = host.to_ascii_lowercase();
        match self {
            HostPattern::Exact(name) => host_lc == *name,
            HostPattern::Suffix(suffix) => host_lc.ends_with(suffix),
            HostPattern::Ip(ip) => IpAddr::from_str(host).map(|h| h == *ip).unwrap_or(false),
            HostPattern::Localhost => match IpAddr::from_str(host) {
                Ok(IpAddr::V4(v4)) => v4.is_loopback(),
                Ok(IpAddr::V6(v6)) => v6.is_loopback(),
                Err(_) => host_lc == "localhost" || host_lc == "localhost.localdomain",
            },
        }
    }

    /// Render back to a user-facing string for `.egress` listings.
    pub fn render(&self) -> String {
        match self {
            HostPattern::Exact(name) => name.clone(),
            HostPattern::Suffix(suffix) => format!("*{suffix}"),
            HostPattern::Ip(ip) => ip.to_string(),
            HostPattern::Localhost => "localhost".to_string(),
        }
    }
}

/// Egress allowlist for a single REPL session.
///
/// `default_allow` comes from config at session creation and is never
/// mutated by runtime meta-commands. `session_allow` is populated by
/// `.allow` and discarded when the session closes.
#[derive(Debug, Clone)]
pub struct EgressPolicy {
    default_allow: Vec<HostPattern>,
    session_allow: Vec<HostPattern>,
}

impl EgressPolicy {
    /// Build a new policy. `Localhost` is always prepended to `default_allow`
    /// and cannot be removed by the user.
    pub fn new(defaults: Vec<HostPattern>) -> Self {
        let mut default_allow = vec![HostPattern::Localhost];
        for d in defaults {
            if d != HostPattern::Localhost && !default_allow.contains(&d) {
                default_allow.push(d);
            }
        }
        Self {
            default_allow,
            session_allow: Vec::new(),
        }
    }

    /// Convenience: policy with only the built-in localhost entry.
    pub fn localhost_only() -> Self {
        Self::new(Vec::new())
    }

    /// Evaluate a host/port combination.
    pub fn decide(&self, host: &str, _port: u16) -> Decision {
        for p in self.default_allow.iter().chain(self.session_allow.iter()) {
            if p.matches(host) {
                return Decision::Allow;
            }
        }
        Decision::DenyAskUser
    }

    /// Post-resolution guard against DNS-rebinding SSRF (CWE-918).
    ///
    /// [`decide`](Self::decide) evaluates the hostname *string* before DNS
    /// resolution, so an allowed name (e.g. `evil.example.com` under
    /// `*.example.com`) can still resolve to a loopback / private /
    /// link-local address — the cloud-metadata endpoint being the classic
    /// target. Callers must vet **every** resolved address with this
    /// method and only dial addresses that return [`Decision::Allow`].
    ///
    /// Rules, in order:
    ///   1. IP-literal targets were already evaluated as that exact
    ///      address by `decide`; resolution cannot diverge from it.
    ///   2. The localhost name family may only land on loopback.
    ///   3. An address covered by an explicit `Ip` allow pattern is a
    ///      deliberate user grant (e.g. a LAN device).
    ///   4. Loopback / link-local / RFC 1918 / ULA / unspecified /
    ///      broadcast / multicast addresses are [`Decision::DenyFinal`]
    ///      — no grant on the *hostname* can permit them.
    pub fn decide_resolved(&self, host: &str, resolved: IpAddr) -> Decision {
        // Normalise IPv4-mapped IPv6 (`::ffff:a.b.c.d`) so the v4 range
        // checks below cannot be bypassed in v6 clothing.
        let ip = resolved.to_canonical();

        if let Ok(lit) = IpAddr::from_str(host) {
            return if lit.to_canonical() == ip {
                Decision::Allow
            } else {
                Decision::DenyFinal
            };
        }

        if HostPattern::Localhost.matches(host) {
            return if ip.is_loopback() {
                Decision::Allow
            } else {
                Decision::DenyFinal
            };
        }

        if self
            .default_allow
            .iter()
            .chain(self.session_allow.iter())
            .any(|p| matches!(p, HostPattern::Ip(_)) && p.matches(&ip.to_string()))
        {
            return Decision::Allow;
        }

        if is_non_public(ip) {
            return Decision::DenyFinal;
        }
        Decision::Allow
    }

    /// Add a session-only allow rule. Returns `true` if it was a new rule.
    ///
    /// Localhost is rejected because it is already built-in; returning
    /// `false` keeps `.allow localhost` idempotent.
    pub fn allow(&mut self, pattern: HostPattern) -> bool {
        if pattern == HostPattern::Localhost {
            return false;
        }
        if self.default_allow.contains(&pattern) || self.session_allow.contains(&pattern) {
            return false;
        }
        self.session_allow.push(pattern);
        true
    }

    /// Remove a session-only allow rule. Returns `true` if a rule was removed.
    ///
    /// `default_allow` entries (including Localhost) are never removed.
    pub fn revoke(&mut self, pattern: &HostPattern) -> bool {
        let before = self.session_allow.len();
        self.session_allow.retain(|p| p != pattern);
        self.session_allow.len() != before
    }

    /// Clear all session-only rules. `default_allow` is preserved.
    pub fn reset_session(&mut self) {
        self.session_allow.clear();
    }

    /// Snapshot for UI / `.egress` listing.
    pub fn snapshot(&self) -> EgressSnapshot {
        EgressSnapshot {
            defaults: self.default_allow.iter().map(HostPattern::render).collect(),
            session: self.session_allow.iter().map(HostPattern::render).collect(),
        }
    }
}

/// Is `ip` outside the publicly routable unicast space?
///
/// Plays the role of `ato-netd`'s post-resolution `check_addr` CIDR stage
/// with a fixed deny set: loopback, RFC 1918 private, link-local (incl.
/// the 169.254.169.254 cloud-metadata endpoint), IPv6 ULA, plus
/// unspecified / broadcast / multicast. Deliberately excludes CGNAT
/// 100.64.0.0/10 — Tailscale-style overlay networks legitimately resolve
/// hostnames into that range.
fn is_non_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_unspecified()
                || v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_multicast()
        }
        IpAddr::V6(v6) => {
            v6.is_unspecified()
                || v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
        }
    }
}

/// User-facing snapshot of the policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressSnapshot {
    pub defaults: Vec<String>,
    pub session: Vec<String>,
}

impl EgressSnapshot {
    /// Render the snapshot as a human-readable multi-line block for
    /// the REPL `.egress` command.
    pub fn render_human(&self) -> String {
        let mut out = String::new();
        out.push_str("egress policy (session-only; deny by default):\n");
        out.push_str("  defaults:\n");
        for d in &self.defaults {
            out.push_str(&format!("    - {d}\n"));
        }
        if self.session.is_empty() {
            out.push_str("  session allows: (none — use `.allow <host>` to add)\n");
        } else {
            out.push_str("  session allows:\n");
            for s in &self.session {
                out.push_str(&format!("    - {s}\n"));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_exact() {
        assert_eq!(
            HostPattern::parse("api.github.com").unwrap(),
            HostPattern::Exact("api.github.com".into())
        );
    }

    #[test]
    fn parse_suffix_star() {
        assert_eq!(
            HostPattern::parse("*.github.com").unwrap(),
            HostPattern::Suffix(".github.com".into())
        );
    }

    #[test]
    fn parse_suffix_dot() {
        assert_eq!(
            HostPattern::parse(".anthropic.com").unwrap(),
            HostPattern::Suffix(".anthropic.com".into())
        );
    }

    #[test]
    fn parse_ip_v4() {
        assert_eq!(
            HostPattern::parse("1.2.3.4").unwrap(),
            HostPattern::Ip("1.2.3.4".parse().unwrap())
        );
    }

    #[test]
    fn parse_ip_v6() {
        assert_eq!(
            HostPattern::parse("::1").unwrap(),
            HostPattern::Ip("::1".parse().unwrap())
        );
    }

    #[test]
    fn parse_localhost_ci() {
        assert_eq!(
            HostPattern::parse("LOCALHOST").unwrap(),
            HostPattern::Localhost
        );
    }

    #[test]
    fn parse_empty_fails() {
        assert!(HostPattern::parse("").is_err());
        assert!(HostPattern::parse("   ").is_err());
    }

    #[test]
    fn parse_rejects_whitespace() {
        assert!(HostPattern::parse("foo bar").is_err());
        assert!(HostPattern::parse("foo/bar").is_err());
    }

    #[test]
    fn exact_matches_case_insensitive() {
        let p = HostPattern::parse("API.Github.COM").unwrap();
        assert!(p.matches("api.github.com"));
        assert!(p.matches("API.github.com"));
        assert!(!p.matches("github.com"));
    }

    #[test]
    fn suffix_matches_subdomain() {
        let p = HostPattern::parse("*.github.com").unwrap();
        assert!(p.matches("api.github.com"));
        assert!(p.matches("raw.githubusercontent.github.com"));
        assert!(!p.matches("github.com"));
        assert!(!p.matches("notgithub.com"));
    }

    #[test]
    fn ip_literal_matches_exact() {
        let p = HostPattern::parse("1.2.3.4").unwrap();
        assert!(p.matches("1.2.3.4"));
        assert!(!p.matches("1.2.3.5"));
        // DNS-name input should not accidentally match an IP pattern.
        assert!(!p.matches("example.com"));
    }

    #[test]
    fn localhost_matches_loopback_forms() {
        let p = HostPattern::Localhost;
        assert!(p.matches("127.0.0.1"));
        assert!(p.matches("127.1.2.3"));
        assert!(p.matches("::1"));
        assert!(p.matches("localhost"));
        assert!(p.matches("LocalHost"));
        assert!(!p.matches("10.0.0.1"));
        assert!(!p.matches("example.com"));
    }

    #[test]
    fn new_always_prepends_localhost() {
        let pol = EgressPolicy::new(vec![HostPattern::Exact("example.com".into())]);
        let snap = pol.snapshot();
        assert_eq!(snap.defaults[0], "localhost");
        assert!(snap.defaults.iter().any(|d| d == "example.com"));
    }

    #[test]
    fn new_deduplicates_localhost() {
        let pol = EgressPolicy::new(vec![HostPattern::Localhost, HostPattern::Localhost]);
        assert_eq!(pol.snapshot().defaults, vec!["localhost"]);
    }

    #[test]
    fn decide_localhost_always_allowed() {
        let pol = EgressPolicy::localhost_only();
        assert_eq!(pol.decide("127.0.0.1", 443), Decision::Allow);
        assert_eq!(pol.decide("::1", 443), Decision::Allow);
        assert_eq!(pol.decide("localhost", 443), Decision::Allow);
    }

    #[test]
    fn decide_default_denies_public() {
        let pol = EgressPolicy::localhost_only();
        assert_eq!(pol.decide("example.com", 443), Decision::DenyAskUser);
        assert_eq!(pol.decide("1.2.3.4", 443), Decision::DenyAskUser);
    }

    #[test]
    fn allow_then_decide_allows() {
        let mut pol = EgressPolicy::localhost_only();
        assert!(pol.allow(HostPattern::parse("example.com").unwrap()));
        assert_eq!(pol.decide("example.com", 443), Decision::Allow);
        assert_eq!(pol.decide("other.com", 443), Decision::DenyAskUser);
    }

    #[test]
    fn allow_is_idempotent() {
        let mut pol = EgressPolicy::localhost_only();
        assert!(pol.allow(HostPattern::parse("example.com").unwrap()));
        assert!(!pol.allow(HostPattern::parse("example.com").unwrap()));
    }

    #[test]
    fn allow_localhost_noop() {
        let mut pol = EgressPolicy::localhost_only();
        assert!(!pol.allow(HostPattern::Localhost));
        assert_eq!(pol.snapshot().session.len(), 0);
    }

    #[test]
    fn revoke_session_only() {
        let mut pol = EgressPolicy::new(vec![HostPattern::parse("builtin.com").unwrap()]);
        pol.allow(HostPattern::parse("runtime.com").unwrap());
        assert!(pol.revoke(&HostPattern::parse("runtime.com").unwrap()));
        assert_eq!(pol.decide("runtime.com", 443), Decision::DenyAskUser);
        // default entries cannot be revoked
        assert!(!pol.revoke(&HostPattern::parse("builtin.com").unwrap()));
        assert_eq!(pol.decide("builtin.com", 443), Decision::Allow);
    }

    #[test]
    fn reset_session_clears_runtime_grants_only() {
        let mut pol = EgressPolicy::new(vec![HostPattern::parse("builtin.com").unwrap()]);
        pol.allow(HostPattern::parse("a.com").unwrap());
        pol.allow(HostPattern::parse("b.com").unwrap());
        pol.reset_session();
        assert!(pol.snapshot().session.is_empty());
        assert_eq!(pol.decide("builtin.com", 443), Decision::Allow);
    }

    #[test]
    fn decide_resolved_blocks_rebinding_to_non_public() {
        // DNS rebinding: the hostname is allowed, but the attacker's
        // resolver answers with a non-public address. Hard deny.
        let mut pol = EgressPolicy::localhost_only();
        pol.allow(HostPattern::parse("*.example.com").unwrap());
        for ip in [
            "127.0.0.1",          // loopback
            "10.1.2.3",           // RFC 1918
            "172.16.0.1",         // RFC 1918
            "192.168.1.1",        // RFC 1918
            "169.254.169.254",    // link-local / cloud metadata
            "0.0.0.0",            // unspecified
            "255.255.255.255",    // broadcast
            "224.0.0.1",          // multicast
            "::1",                // v6 loopback
            "::",                 // v6 unspecified
            "fe80::1",            // v6 link-local
            "fd00::1",            // v6 ULA
            "::ffff:127.0.0.1",   // v4-mapped loopback
            "::ffff:192.168.1.1", // v4-mapped private
        ] {
            assert_eq!(
                pol.decide_resolved("evil.example.com", ip.parse().unwrap()),
                Decision::DenyFinal,
                "{ip} must be a hard deny"
            );
        }
    }

    #[test]
    fn decide_resolved_allows_public_addrs() {
        let mut pol = EgressPolicy::localhost_only();
        pol.allow(HostPattern::parse("*.example.com").unwrap());
        for ip in ["93.184.216.34", "2606:2800:220:1::1"] {
            assert_eq!(
                pol.decide_resolved("ok.example.com", ip.parse().unwrap()),
                Decision::Allow,
                "{ip} must be allowed"
            );
        }
    }

    #[test]
    fn decide_resolved_localhost_names_pin_to_loopback() {
        let pol = EgressPolicy::localhost_only();
        assert_eq!(
            pol.decide_resolved("localhost", "127.0.0.1".parse().unwrap()),
            Decision::Allow
        );
        assert_eq!(
            pol.decide_resolved("localhost", "::1".parse().unwrap()),
            Decision::Allow
        );
        // `localhost` rebinding away from loopback is a hard deny.
        assert_eq!(
            pol.decide_resolved("localhost", "10.0.0.1".parse().unwrap()),
            Decision::DenyFinal
        );
    }

    #[test]
    fn decide_resolved_allows_ip_literal_targets() {
        // An IP-literal target only reaches the dial when `decide`
        // matched an explicit Ip grant; resolution cannot diverge.
        let pol = EgressPolicy::new(vec![HostPattern::parse("192.168.1.50").unwrap()]);
        assert_eq!(
            pol.decide_resolved("192.168.1.50", "192.168.1.50".parse().unwrap()),
            Decision::Allow
        );
    }

    #[test]
    fn decide_resolved_honours_explicit_ip_grant_for_hostname() {
        // The user deliberately allowed the LAN address: a hostname
        // resolving to it is a grant, not a rebinding.
        let mut pol = EgressPolicy::localhost_only();
        pol.allow(HostPattern::parse("*.lan.example").unwrap());
        pol.allow(HostPattern::parse("192.168.1.50").unwrap());
        assert_eq!(
            pol.decide_resolved("nas.lan.example", "192.168.1.50".parse().unwrap()),
            Decision::Allow
        );
        assert_eq!(
            pol.decide_resolved("nas.lan.example", "192.168.1.51".parse().unwrap()),
            Decision::DenyFinal
        );
    }

    #[test]
    fn snapshot_renders_human_readable() {
        let mut pol = EgressPolicy::new(vec![HostPattern::parse("builtin.com").unwrap()]);
        pol.allow(HostPattern::parse("*.example.com").unwrap());
        let text = pol.snapshot().render_human();
        assert!(text.contains("localhost"));
        assert!(text.contains("builtin.com"));
        assert!(text.contains("*.example.com"));
    }
}
