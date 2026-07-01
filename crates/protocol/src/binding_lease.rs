//! Phase 8a BindingLease core model (#863; contract:
//! `docs/ready-state/binding-lease.md`).
//!
//! The session-scoped types by which a restored Ready-State microVM receives its
//! runtime bindings **after** restore and **before** user traffic — shared by the host
//! (`ato run`) and, in a later PR, the guest-agent over vsock. This module is **types
//! only**: no `ato run` behavior change, no guest-agent, no delivery.
//!
//! ## No secret in a log or a snapshot (hard invariants)
//! - The lease value is a [`SecretValue`]: its `Debug`/`Display` are **redacted**, so a
//!   lease can never leak into a log; the raw value is reachable only via
//!   [`SecretValue::expose`], called at the single wire-delivery point.
//! - The loggable/recordable form is [`BindingLeaseReceipt`], which carries **no**
//!   value at all — that is what gets serialized into records and traces.
//! - Nothing here is ever written into the sealed artifact (CAS/manifest/rootfs/memory/
//!   vmstate); a lease exists only at restore time. (The seal stays pre-bind + secret
//!   free per #831/#834.)

use serde::{Deserialize, Serialize};

/// Schema version for the BindingLease wire types. Bump on any breaking change.
pub const BINDING_LEASE_SCHEMA_VERSION: u32 = 1;

/// Max binding-name length (path-component sanity for `/run/ato/bindings/<name>`).
const MAX_BINDING_NAME_LEN: usize = 128;

/// A validated binding name — the `<name>` in `/run/ato/bindings/<name>`. Non-sensitive.
/// Charset is deliberately conservative (a safe single path component): lowercase
/// ASCII, digits, `_`, `-`, `.` (and never `.`/`..` alone).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BindingName(String);

/// Why a [`BindingName`] was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingNameError {
    Empty,
    TooLong(usize),
    /// A disallowed character (anything outside `[a-z0-9_.-]`).
    InvalidChar(char),
    /// `.` or `..` — not a usable path component.
    ReservedDotName,
}

impl std::fmt::Display for BindingNameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BindingNameError::Empty => write!(f, "binding name is empty"),
            BindingNameError::TooLong(n) => write!(f, "binding name too long ({n} > {MAX_BINDING_NAME_LEN})"),
            BindingNameError::InvalidChar(c) => write!(f, "binding name has invalid character {c:?} (allowed: a-z 0-9 _ - .)"),
            BindingNameError::ReservedDotName => write!(f, "binding name '.'/'..' is not a valid path component"),
        }
    }
}

impl std::error::Error for BindingNameError {}

impl BindingName {
    /// Validate + construct a binding name.
    pub fn parse(s: impl Into<String>) -> Result<Self, BindingNameError> {
        let s = s.into();
        if s.is_empty() {
            return Err(BindingNameError::Empty);
        }
        if s.len() > MAX_BINDING_NAME_LEN {
            return Err(BindingNameError::TooLong(s.len()));
        }
        if s == "." || s == ".." {
            return Err(BindingNameError::ReservedDotName);
        }
        for c in s.chars() {
            if !(c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.')) {
                return Err(BindingNameError::InvalidChar(c));
            }
        }
        Ok(BindingName(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A session-scoped lease id (opaque; the host generates it, e.g. per restore).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BindingLeaseId(String);

impl BindingLeaseId {
    pub fn new(s: impl Into<String>) -> Self {
        BindingLeaseId(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Redacted secret material. Serializes to the wire (delivery to the guest-agent) but
/// `Debug`/`Display` are redacted, so it can never leak into a log; the raw value is
/// reachable only via [`SecretValue::expose`].
#[derive(Clone, Serialize, Deserialize)]
pub struct SecretValue(String);

impl SecretValue {
    pub fn new(value: impl Into<String>) -> Self {
        SecretValue(value.into())
    }

    /// Reveal the raw value. Call ONLY at the single wire-delivery point; never log or
    /// re-serialize the result.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretValue(***redacted***)")
    }
}

/// Lease status at a point in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingLeaseStatus {
    /// Within TTL and not revoked.
    Active,
    /// TTL elapsed with no renew.
    Expired,
    /// Explicitly revoked (host revoke or `ato stop`).
    Revoked,
}

/// A session-scoped binding lease: name + secret value + TTL window. Delivered to the
/// guest-agent over vsock (a later PR). Times are unix-millis (the caller supplies the
/// clock, so the model is deterministic + testable). `Debug` redacts the value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingLease {
    pub schema_version: u32,
    pub id: BindingLeaseId,
    pub name: BindingName,
    pub value: SecretValue,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    /// Set when revoked; `None` while the lease has never been revoked.
    #[serde(default)]
    pub revoked_at_ms: Option<u64>,
}

impl BindingLease {
    /// Issue a lease valid for `ttl_ms` from `issued_at_ms`.
    pub fn issue(
        id: BindingLeaseId,
        name: BindingName,
        value: SecretValue,
        issued_at_ms: u64,
        ttl_ms: u64,
    ) -> Self {
        BindingLease {
            schema_version: BINDING_LEASE_SCHEMA_VERSION,
            id,
            name,
            value,
            issued_at_ms,
            expires_at_ms: issued_at_ms.saturating_add(ttl_ms),
            revoked_at_ms: None,
        }
    }

    /// Mark the lease revoked at `at_ms` (idempotent — keeps the first revoke time).
    pub fn revoke(&mut self, at_ms: u64) {
        if self.revoked_at_ms.is_none() {
            self.revoked_at_ms = Some(at_ms);
        }
    }

    /// Status as of `now_ms`. Revoked dominates expiry.
    pub fn status(&self, now_ms: u64) -> BindingLeaseStatus {
        if let Some(r) = self.revoked_at_ms
            && now_ms >= r
        {
            return BindingLeaseStatus::Revoked;
        }
        if now_ms >= self.expires_at_ms {
            return BindingLeaseStatus::Expired;
        }
        BindingLeaseStatus::Active
    }

    pub fn is_active(&self, now_ms: u64) -> bool {
        self.status(now_ms) == BindingLeaseStatus::Active
    }

    /// The loggable receipt for this lease as of `now_ms` — **no secret value**.
    pub fn receipt(&self, now_ms: u64) -> BindingLeaseReceipt {
        BindingLeaseReceipt {
            schema_version: self.schema_version,
            id: self.id.clone(),
            name: self.name.clone(),
            status: self.status(now_ms),
            issued_at_ms: self.issued_at_ms,
            expires_at_ms: self.expires_at_ms,
            revoked_at_ms: self.revoked_at_ms,
        }
    }
}

/// The loggable / recordable record of a lease — carries **no** secret value, so it is
/// safe to serialize into records, receipts, and traces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingLeaseReceipt {
    pub schema_version: u32,
    pub id: BindingLeaseId,
    pub name: BindingName,
    pub status: BindingLeaseStatus,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    #[serde(default)]
    pub revoked_at_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "super-secret-token-DO-NOT-LEAK";

    fn lease(now: u64, ttl: u64) -> BindingLease {
        BindingLease::issue(
            BindingLeaseId::new("lease-1"),
            BindingName::parse("database_url").unwrap(),
            SecretValue::new(SECRET),
            now,
            ttl,
        )
    }

    #[test]
    fn binding_name_validation() {
        assert!(BindingName::parse("database_url").is_ok());
        assert!(BindingName::parse("api-key.v2").is_ok());
        assert_eq!(BindingName::parse(""), Err(BindingNameError::Empty));
        assert_eq!(BindingName::parse("."), Err(BindingNameError::ReservedDotName));
        assert_eq!(BindingName::parse(".."), Err(BindingNameError::ReservedDotName));
        assert_eq!(BindingName::parse("bad name"), Err(BindingNameError::InvalidChar(' ')));
        assert_eq!(BindingName::parse("UPPER"), Err(BindingNameError::InvalidChar('U')));
        assert!(matches!(BindingName::parse("x".repeat(200)), Err(BindingNameError::TooLong(_))));
    }

    #[test]
    fn secret_value_never_appears_in_debug() {
        let v = SecretValue::new(SECRET);
        assert!(!format!("{v:?}").contains(SECRET), "SecretValue Debug leaked the secret");
        // And a whole lease Debug'd must not leak it either.
        let l = lease(1000, 5000);
        assert!(!format!("{l:?}").contains(SECRET), "BindingLease Debug leaked the secret");
        assert_eq!(v.expose(), SECRET, "expose() still returns the raw value");
    }

    #[test]
    fn status_transitions_active_expired_revoked() {
        let mut l = lease(1000, 5000); // expires at 6000
        assert_eq!(l.status(1000), BindingLeaseStatus::Active);
        assert!(l.is_active(5999));
        assert_eq!(l.status(6000), BindingLeaseStatus::Expired);
        // revoke at 3000 dominates, even before expiry.
        l.revoke(3000);
        assert_eq!(l.status(3000), BindingLeaseStatus::Revoked);
        assert_eq!(l.status(6000), BindingLeaseStatus::Revoked);
        // revoke is idempotent (keeps the first time).
        l.revoke(9000);
        assert_eq!(l.revoked_at_ms, Some(3000));
    }

    #[test]
    fn receipt_carries_no_secret_and_serializes() {
        let l = lease(1000, 5000);
        let r = l.receipt(2000);
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains(SECRET), "receipt JSON leaked the secret");
        assert!(!format!("{r:?}").contains(SECRET), "receipt Debug leaked the secret");
        assert_eq!(r.status, BindingLeaseStatus::Active);
        assert_eq!(r.schema_version, BINDING_LEASE_SCHEMA_VERSION);
        // round-trip.
        let back: BindingLeaseReceipt = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn lease_wire_round_trips_including_value() {
        // The lease serializes WITH the value (wire delivery to the guest-agent).
        let l = lease(1000, 5000);
        let json = serde_json::to_string(&l).unwrap();
        assert!(json.contains(SECRET), "wire lease must carry the value for delivery");
        let back: BindingLease = serde_json::from_str(&json).unwrap();
        assert_eq!(back.value.expose(), SECRET);
        assert_eq!(back.name.as_str(), "database_url");
        assert_eq!(back.schema_version, BINDING_LEASE_SCHEMA_VERSION);
    }
}
