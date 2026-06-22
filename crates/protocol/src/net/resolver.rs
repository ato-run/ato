//! DNS resolver wire DTOs — the transport-neutral data types shared by the
//! `ato-netd` resolver backends and any consumer that records resolution
//! results.
//!
//! The resolver *backends* (`SystemResolver`, `DohResolver`, `Chain`, the
//! `Resolver` trait) live in `ato-netd` because they pull in `hickory-resolver`
//! and a Tokio runtime. Only the pure DTOs that cross the process / receipt
//! boundary live here.

use std::net::{Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Options passed to each `Resolver::resolve` call.
#[derive(Debug, Clone)]
pub struct ResolveOptions {
    /// Per-lookup timeout in milliseconds. Defaults to 5 000 ms.
    pub timeout_ms: u64,
}

impl Default for ResolveOptions {
    fn default() -> Self {
        Self { timeout_ms: 5_000 }
    }
}

/// The result of a successful DNS resolution.
///
/// Both `addrs_v4` and `addrs_v6` may be empty if no records of that type
/// exist; a non-empty `addrs_v4` or `addrs_v6` constitutes success.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolvedRecord {
    /// The name that was originally requested.
    pub name: String,

    /// All names traversed via CNAME records, in query order.
    ///
    /// Empty when the name resolves directly (no CNAMEs).
    /// For `a.example → b.example → 1.2.3.4` the chain is
    /// `["a.example", "b.example"]`.
    pub cname_chain: Vec<String>,

    /// IPv4 addresses from A records.
    pub addrs_v4: Vec<Ipv4Addr>,

    /// IPv6 addresses from AAAA records.
    pub addrs_v6: Vec<Ipv6Addr>,

    /// TTL of the first A or AAAA record, in seconds.  `None` when no
    /// address records were returned (should not happen on success).
    pub ttl_seconds: Option<u32>,

    /// Which backend produced this answer (e.g. `"system"`, `"doh"`).
    pub backend: String,

    /// If this record was produced by a fallback in a resolver chain,
    /// explains why the primary backend was skipped
    /// (e.g. `"fallback_from_system"`).
    pub fallback_reason: Option<String>,
}

/// Typed DNS resolution errors.
///
/// A resolver chain treats [`Timeout`][ResolverError::Timeout],
/// [`TransportFailure`][ResolverError::TransportFailure], and
/// [`BackendUnavailable`][ResolverError::BackendUnavailable] as retryable
/// (falls through to the next backend).  All other variants short-circuit.
#[derive(Debug, Clone, Error)]
pub enum ResolverError {
    #[error("NXDOMAIN: {0}")]
    NxDomain(String),

    #[error("timeout resolving {0}")]
    Timeout(String),

    #[error("SERVFAIL: {0}")]
    Servfail(String),

    #[error("transport failure: {0}")]
    TransportFailure(String),

    #[error("policy denied: {0}")]
    PolicyDenied(String),

    #[error("backend unavailable: {0}")]
    BackendUnavailable(String),
}

impl ResolverError {
    /// Returns true for errors that a resolver chain treats as retryable.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Timeout(_) | Self::TransportFailure(_) | Self::BackendUnavailable(_)
        )
    }
}
