//! Daemon runtime state. Slice **A** ships only the bookkeeping needed
//! to answer the `status` verb (start time, listener inventory).
//! Subsequent slices grow this with the ingress route table (**B**),
//! the resolver cache (**D**), the egress policy (**E**), etc.

use std::sync::Arc;
use std::time::Instant;

use ato_net::control::ListenerInfo;
use tokio::sync::RwLock;

/// Shared, cheaply-cloneable handle into the daemon's runtime state.
#[derive(Clone)]
pub struct DaemonState {
    inner: Arc<DaemonStateInner>,
}

struct DaemonStateInner {
    started_at: Instant,
    listeners: RwLock<Vec<ListenerInfo>>,
    /// Signals graceful shutdown to the accept loop. `Some` while the
    /// daemon is meant to run; replaced by `None` after `shutdown`
    /// arrives so the next accept iteration breaks out.
    shutdown: tokio::sync::Notify,
}

impl DaemonState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(DaemonStateInner {
                started_at: Instant::now(),
                listeners: RwLock::new(Vec::new()),
                shutdown: tokio::sync::Notify::new(),
            }),
        }
    }

    /// Seconds elapsed since [`Self::new`].
    pub fn uptime_secs(&self) -> u64 {
        self.inner.started_at.elapsed().as_secs()
    }

    /// Snapshot the currently-registered listeners. Slice **A** never
    /// pushes one; the inventory exists so the wire format is stable
    /// when **B** does.
    pub async fn listeners(&self) -> Vec<ListenerInfo> {
        self.inner.listeners.read().await.clone()
    }

    /// Notify any task awaiting [`Self::wait_for_shutdown`].
    pub fn signal_shutdown(&self) {
        self.inner.shutdown.notify_waiters();
    }

    /// Resolves when [`Self::signal_shutdown`] has been called.
    pub async fn wait_for_shutdown(&self) {
        self.inner.shutdown.notified().await;
    }
}

impl Default for DaemonState {
    fn default() -> Self {
        Self::new()
    }
}
