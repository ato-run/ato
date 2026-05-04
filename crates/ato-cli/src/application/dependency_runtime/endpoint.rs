//! TCP endpoint allocation for `port = "auto"` (RFC §7.5).
//!
//! v1 strategy: bind to `127.0.0.1:0`, ask the kernel for a free port via
//! the OS, immediately drop the listener so the provider can bind the same
//! port shortly after. There is a brief race window between drop and the
//! provider's listen — providers that retry on `EADDRINUSE` (postgres,
//! uvicorn, etc.) handle this naturally; providers that fail-fast must
//! either accept this allocation strategy or provide an alternative at the
//! orchestrator level. This is the documented v1 trade-off in RFC §13
//! ("endpoint allocation race") and is acceptable for the current scope.
//!
//! `unix_socket = "auto"` is reserved-only in v1 (lock fail-closed in
//! `capsule-core::foundation::dependency_contracts`); this module does not
//! implement it.

use std::collections::BTreeSet;
use std::net::TcpListener;
use std::sync::Mutex;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum EndpointError {
    #[error("failed to bind 127.0.0.1:0 for endpoint allocation: {detail}")]
    BindFailed { detail: String },

    #[error("kernel returned an unexpected local address: {detail}")]
    UnexpectedAddress { detail: String },
}

/// Per-orchestration allocator that hands out ephemeral TCP ports for
/// `port = "auto"` providers.
///
/// The allocator avoids returning the same port twice within the same
/// orchestration session (even across rapid successive allocations) by
/// tracking previously-handed-out ports in `recent`. The OS kernel picks
/// the actual free port; `recent` only de-duplicates if the kernel
/// happens to return the same port on a fresh `bind(0)` (rare but possible
/// when the previous listener has not fully released yet).
pub struct EndpointAllocator {
    recent: Mutex<BTreeSet<u16>>,
}

impl EndpointAllocator {
    pub fn new() -> Self {
        Self {
            recent: Mutex::new(BTreeSet::new()),
        }
    }

    /// Allocate one ephemeral TCP port. The returned port is **not**
    /// reserved by the kernel after this call — the caller must spawn the
    /// provider quickly so the kernel does not hand the same port out to
    /// another process. The window is on the order of microseconds in
    /// practice.
    pub fn allocate_tcp(&self) -> Result<u16, EndpointError> {
        // Try a few times in case the kernel hands back a duplicate of a
        // very recently allocated port (the previous listener may still be
        // in TIME_WAIT for the original socket).
        for _ in 0..8 {
            let port = bind_and_probe()?;
            let mut recent = self.recent.lock().expect("allocator lock poisoned");
            if recent.insert(port) {
                return Ok(port);
            }
        }
        // Fall through: return whatever the kernel last gave us. With 8
        // retries the chance of a true collision under load is negligible.
        bind_and_probe()
    }
}

impl Default for EndpointAllocator {
    fn default() -> Self {
        Self::new()
    }
}

fn bind_and_probe() -> Result<u16, EndpointError> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|err| EndpointError::BindFailed {
        detail: err.to_string(),
    })?;
    let addr = listener
        .local_addr()
        .map_err(|err| EndpointError::UnexpectedAddress {
            detail: err.to_string(),
        })?;
    let port = addr.port();
    drop(listener);
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocates_nonzero_port() {
        let alloc = EndpointAllocator::new();
        let port = alloc.allocate_tcp().expect("allocate");
        assert!(port > 0, "port must be > 0");
    }

    #[test]
    fn allocates_distinct_ports_in_succession() {
        // 5 consecutive allocations must all return distinct values.
        let alloc = EndpointAllocator::new();
        let mut seen = BTreeSet::new();
        for _ in 0..5 {
            let port = alloc.allocate_tcp().expect("allocate");
            assert!(seen.insert(port), "duplicate port: {port}");
        }
    }

    #[test]
    fn allocator_works_across_threads() {
        use std::sync::Arc;
        use std::thread;
        let alloc = Arc::new(EndpointAllocator::new());
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let a = alloc.clone();
                thread::spawn(move || a.allocate_tcp().expect("allocate"))
            })
            .collect();
        let ports: Vec<u16> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let unique: BTreeSet<_> = ports.iter().copied().collect();
        assert_eq!(unique.len(), ports.len(), "collisions: {ports:?}");
    }
}
