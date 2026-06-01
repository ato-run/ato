//! DNS resolver abstraction — Slice D (#299).
//!
//! # Design
//!
//! - [`Resolver`] is the single async trait.  All backends implement it.
//! - [`ResolvedRecord`] carries the full resolution result: requested name,
//!   CNAME chain, A + AAAA addresses, per-record TTL, which backend answered,
//!   and (for `Chain` fallback) why fallback occurred.
//! - [`ResolverError`] is typed so callers can match `NxDomain` vs `Timeout`
//!   without parsing error strings.
//!
//! # Backends
//!
//! | Backend         | When to use |
//! |-----------------|-------------|
//! | [`SystemResolver`] | Default. Reads `/etc/resolv.conf` (or OS equivalent). |
//! | [`DohResolver`] | Optional. DNS-over-HTTPS fallback. Enabled at runtime by explicit construction; not used unless added to a [`Chain`]. |
//! | [`Chain`]       | Tries backends in order; falls back on `Timeout` / `TransportFailure` / `BackendUnavailable`. `NxDomain` / `Servfail` / `PolicyDenied` short-circuit immediately. |
//!
//! # Example
//!
//! ```rust,ignore
//! use ato_net::resolver::{Chain, ResolveOptions, Resolver, SystemResolver};
//!
//! let sys = SystemResolver::new().expect("system resolver");
//! let chain = Chain::new(vec![Box::new(sys)]);
//! let opts = ResolveOptions::default();
//! let record = chain.resolve("localhost", &opts).await.unwrap();
//! assert!(!record.addrs_v4.is_empty() || !record.addrs_v6.is_empty());
//! ```

use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use hickory_resolver::{
    TokioAsyncResolver,
    config::{ResolverConfig, ResolverOpts},
    error::{ResolveError, ResolveErrorKind},
    proto::rr::{RData, RecordType},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::time::{Duration, timeout};

// ── Public types ─────────────────────────────────────────────────────────────

/// Options passed to each [`Resolver::resolve`] call.
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

    /// If this record was produced by a fallback in [`Chain`], explains
    /// why the primary backend was skipped (e.g. `"fallback_from_system"`).
    pub fallback_reason: Option<String>,
}

/// Typed DNS resolution errors.
///
/// [`Chain`] treats [`Timeout`][ResolverError::Timeout],
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
    /// Returns true for errors that [`Chain`] treats as retryable.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Timeout(_) | Self::TransportFailure(_) | Self::BackendUnavailable(_)
        )
    }
}

// ── Resolver trait ────────────────────────────────────────────────────────────

#[async_trait]
pub trait Resolver: Send + Sync {
    /// Resolve `name` and return a structured record.
    async fn resolve(
        &self,
        name: &str,
        opts: &ResolveOptions,
    ) -> Result<ResolvedRecord, ResolverError>;

    /// Short identifier for this backend, used in receipts and `fallback_reason`.
    fn backend_name(&self) -> &str;
}

// ── SystemResolver ────────────────────────────────────────────────────────────

/// Resolver backed by the host's system DNS configuration.
///
/// Uses Hickory's [`TokioAsyncResolver`] configured from the OS resolver
/// settings (`/etc/resolv.conf` on Unix, system configuration on macOS /
/// Windows).  Provides CNAME chain extraction and per-record TTL — neither
/// of which is available via `tokio::net::lookup_host`.
pub struct SystemResolver {
    inner: Arc<TokioAsyncResolver>,
}

impl std::fmt::Debug for SystemResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SystemResolver").finish_non_exhaustive()
    }
}

impl SystemResolver {
    /// Create a `SystemResolver` using the host's system DNS configuration.
    pub fn new() -> Result<Self, ResolverError> {
        let resolver = TokioAsyncResolver::tokio_from_system_conf()
            .map_err(|e| ResolverError::BackendUnavailable(e.to_string()))?;
        Ok(Self {
            inner: Arc::new(resolver),
        })
    }

    /// Create a `SystemResolver` from explicit Hickory config (useful in tests).
    pub fn with_config(config: ResolverConfig, opts: ResolverOpts) -> Self {
        let resolver = TokioAsyncResolver::tokio(config, opts);
        Self {
            inner: Arc::new(resolver),
        }
    }
}

#[async_trait]
impl Resolver for SystemResolver {
    async fn resolve(
        &self,
        name: &str,
        opts: &ResolveOptions,
    ) -> Result<ResolvedRecord, ResolverError> {
        resolve_with_hickory(&self.inner, name, "system", opts).await
    }

    fn backend_name(&self) -> &str {
        "system"
    }
}

// ── DohResolver ──────────────────────────────────────────────────────────────

/// Resolver that uses DNS-over-HTTPS (RFC 8484).
///
/// Requires the `doh` cargo feature.  Without it the struct exists and
/// compiles, but [`DohResolver::new`] will return
/// [`ResolverError::BackendUnavailable`].
pub struct DohResolver {
    inner: Arc<TokioAsyncResolver>,
    upstream: String,
}

impl std::fmt::Debug for DohResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DohResolver")
            .field("upstream", &self.upstream)
            .finish_non_exhaustive()
    }
}

impl DohResolver {
    /// Create a `DohResolver` pointing at `upstream_url`
    /// (e.g. `"https://1.1.1.1/dns-query"`).
    ///
    /// Requires the `doh` cargo feature; without it this always returns an
    /// error so callers can detect the missing feature at runtime.
    pub fn new(upstream_url: &str) -> Result<Self, ResolverError> {
        #[cfg(feature = "doh")]
        {
            use hickory_resolver::config::{NameServerConfigGroup, ResolverConfig, ResolverOpts};

            let upstream = upstream_url.to_string();
            // Parse the upstream URL to extract hostname and IP.
            let url = url::Url::parse(upstream_url)
                .map_err(|e| ResolverError::BackendUnavailable(format!("invalid DoH URL: {e}")))?;
            let host = url
                .host_str()
                .ok_or_else(|| {
                    ResolverError::BackendUnavailable("DoH URL missing host".to_string())
                })?
                .to_string();
            // Resolve the host IP — for well-known providers (1.1.1.1, 8.8.8.8)
            // the host *is* the IP.  A full bootstrap resolver is out of scope
            // for Slice D; callers using non-IP hostnames should provide a
            // pre-resolved address.
            let ip: std::net::IpAddr = host.parse().map_err(|_| {
                ResolverError::BackendUnavailable(format!(
                    "DoH upstream host is not an IP address: {host}. \
                     Bootstrap resolution is not supported in Slice D."
                ))
            })?;
            let port = url.port().unwrap_or(443);
            let ns_group = NameServerConfigGroup::from_ips_https(&[ip], port, host.clone(), true);
            let config = ResolverConfig::from_parts(None, vec![], ns_group);
            let resolver = TokioAsyncResolver::tokio(config, ResolverOpts::default());
            Ok(Self {
                inner: Arc::new(resolver),
                upstream,
            })
        }
        #[cfg(not(feature = "doh"))]
        {
            Err(ResolverError::BackendUnavailable(format!(
                "DohResolver is not available: recompile ato-net with the `doh` feature. \
                 upstream_url={upstream_url}"
            )))
        }
    }
}

#[async_trait]
impl Resolver for DohResolver {
    async fn resolve(
        &self,
        name: &str,
        opts: &ResolveOptions,
    ) -> Result<ResolvedRecord, ResolverError> {
        let backend = format!("doh:{}", self.upstream);
        resolve_with_hickory(&self.inner, name, &backend, opts).await
    }

    fn backend_name(&self) -> &str {
        "doh"
    }
}

// ── Chain ─────────────────────────────────────────────────────────────────────

/// Resolver combinator that tries backends in order.
///
/// Fallback occurs on [`ResolverError::Timeout`],
/// [`ResolverError::TransportFailure`], or
/// [`ResolverError::BackendUnavailable`].
/// [`ResolverError::NxDomain`], [`ResolverError::Servfail`], and
/// [`ResolverError::PolicyDenied`] short-circuit immediately — they are
/// definitive answers, not transient failures.
///
/// When a fallback backend succeeds, the returned [`ResolvedRecord`] has
/// its [`fallback_reason`][ResolvedRecord::fallback_reason] field set to
/// `"fallback_from_<previous_backend>"`.
pub struct Chain {
    resolvers: Vec<Box<dyn Resolver>>,
}

impl Chain {
    pub fn new(resolvers: Vec<Box<dyn Resolver>>) -> Self {
        Self { resolvers }
    }
}

#[async_trait]
impl Resolver for Chain {
    async fn resolve(
        &self,
        name: &str,
        opts: &ResolveOptions,
    ) -> Result<ResolvedRecord, ResolverError> {
        let mut last_retryable: Option<ResolverError> = None;
        let mut prev_backend: Option<String> = None;

        for resolver in &self.resolvers {
            match resolver.resolve(name, opts).await {
                Ok(mut record) => {
                    if let Some(prev) = &prev_backend {
                        record.fallback_reason = Some(format!("fallback_from_{prev}"));
                    }
                    return Ok(record);
                }
                Err(e) if e.is_retryable() => {
                    prev_backend = Some(resolver.backend_name().to_string());
                    last_retryable = Some(e);
                    continue;
                }
                Err(e) => return Err(e),
            }
        }

        Err(last_retryable
            .unwrap_or_else(|| ResolverError::BackendUnavailable("no resolvers".to_string())))
    }

    fn backend_name(&self) -> &str {
        "chain"
    }
}

// ── Hickory shared resolution logic ──────────────────────────────────────────

/// Resolve `name` using the given Hickory resolver and annotate the result
/// with `backend`.  A and AAAA records are queried sequentially; AAAA
/// absence (NxDomain-equivalent, no AAAA records) is treated as an empty
/// list rather than an error.
async fn resolve_with_hickory(
    resolver: &TokioAsyncResolver,
    name: &str,
    backend: &str,
    opts: &ResolveOptions,
) -> Result<ResolvedRecord, ResolverError> {
    let deadline = Duration::from_millis(opts.timeout_ms);
    let start = Instant::now();

    // ── A records ────────────────────────────────────────────────────────
    let a_lookup = timeout(deadline, resolver.lookup(name, RecordType::A))
        .await
        .map_err(|_| ResolverError::Timeout(name.to_string()))?
        .map_err(|e| map_hickory_error(e, name))?;

    let mut cname_chain: Vec<String> = Vec::new();
    let mut addrs_v4: Vec<Ipv4Addr> = Vec::new();
    let mut ttl_seconds: Option<u32> = None;

    for record in a_lookup.records() {
        match record.data() {
            Some(RData::CNAME(cname)) => {
                // Push the owner name (source of the alias).
                cname_chain.push(record.name().to_string());
                // Push the target so the chain includes both sides of each hop.
                cname_chain.push(cname.0.to_string());
            }
            Some(RData::A(a)) => {
                addrs_v4.push(a.0);
                if ttl_seconds.is_none() {
                    ttl_seconds = Some(record.ttl());
                }
            }
            _ => {}
        }
    }
    // Deduplicate CNAME chain entries (adjacent duplicates from multi-hop
    // building) while preserving order.
    cname_chain.dedup();

    // ── AAAA records ─────────────────────────────────────────────────────
    let remaining = deadline.saturating_sub(start.elapsed());
    let mut addrs_v6: Vec<Ipv6Addr> = Vec::new();
    if remaining > Duration::ZERO {
        match timeout(remaining, resolver.lookup(name, RecordType::AAAA)).await {
            Ok(Ok(aaaa_lookup)) => {
                for record in aaaa_lookup.records() {
                    if let Some(RData::AAAA(aaaa)) = record.data() {
                        addrs_v6.push(aaaa.0);
                        if ttl_seconds.is_none() {
                            ttl_seconds = Some(record.ttl());
                        }
                    }
                }
            }
            // No AAAA records is normal — treat as empty, not an error.
            Ok(Err(e)) if is_no_records(&e) => {}
            Ok(Err(e)) => return Err(map_hickory_error(e, name)),
            Err(_) => {} // AAAA timeout is non-fatal
        }
    }

    Ok(ResolvedRecord {
        name: name.to_string(),
        cname_chain,
        addrs_v4,
        addrs_v6,
        ttl_seconds,
        backend: backend.to_string(),
        fallback_reason: None,
    })
}

// ── Error mapping ─────────────────────────────────────────────────────────────

fn map_hickory_error(e: ResolveError, name: &str) -> ResolverError {
    match e.kind() {
        ResolveErrorKind::NoRecordsFound { response_code, .. } => {
            use hickory_resolver::proto::op::ResponseCode;
            match response_code {
                ResponseCode::NXDomain => ResolverError::NxDomain(name.to_string()),
                ResponseCode::ServFail => ResolverError::Servfail(e.to_string()),
                _ => ResolverError::TransportFailure(e.to_string()),
            }
        }
        ResolveErrorKind::Io(_) => ResolverError::TransportFailure(e.to_string()),
        ResolveErrorKind::Timeout => ResolverError::Timeout(name.to_string()),
        ResolveErrorKind::Proto(_) => ResolverError::TransportFailure(e.to_string()),
        _ => ResolverError::BackendUnavailable(e.to_string()),
    }
}

fn is_no_records(e: &ResolveError) -> bool {
    matches!(e.kind(), ResolveErrorKind::NoRecordsFound { .. })
}
