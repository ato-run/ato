//! Daemon runtime state. Slice **A** ships only the bookkeeping needed
//! to answer the `status` verb (start time, listener inventory).
//! Slice **B** (#297) adds `IngressManager` for the ingress reverse proxy.
//! Slice **E** (#300) adds `EgressManager` for the HTTP CONNECT proxy.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{Mutex, watch};

use crate::egress::{EgressManager, policy::EgressPolicy};
use crate::identity::RuntimeIdentity;
use crate::ingress::IngressManager;

/// Shared, cheaply-cloneable handle into the daemon's runtime state.
#[derive(Clone)]
pub struct DaemonState {
    inner: Arc<DaemonStateInner>,
}

struct DaemonStateInner {
    started_at: Instant,
    /// All active ingress routes.
    ingress: Mutex<IngressManager>,
    /// HTTP CONNECT egress proxy. `None` until `init_egress` is called.
    egress: Mutex<Option<EgressManager>>,
    /// Stable identity for this daemon installation.
    /// Loaded (or generated) once at startup; never mutated.
    runtime_identity: RuntimeIdentity,
    /// Non-lossy shutdown signal (see comment below).
    ///
    /// `watch::channel<bool>` closes the subscribe race: the value is sticky,
    /// late receivers see it immediately via `borrow_and_update`, and
    /// `changed().await` covers the on-time case.
    shutdown_tx: watch::Sender<bool>,
}

impl DaemonState {
    /// Create a new `DaemonState`.  Loads the port allocator from
    /// `${ato_home}/state/netd/stable_origin_ports.json` and the runtime
    /// identity from `${ATO_HOME}/state/netd/runtime_identity.json`.
    pub async fn new(ato_home: PathBuf) -> anyhow::Result<Self> {
        let ingress = IngressManager::new(&ato_home).await?;
        let runtime_identity = RuntimeIdentity::load_or_create(&ato_home)?;
        let (shutdown_tx, _) = watch::channel(false);
        Ok(Self {
            inner: Arc::new(DaemonStateInner {
                started_at: Instant::now(),
                ingress: Mutex::new(ingress),
                egress: Mutex::new(None),
                runtime_identity,
                shutdown_tx,
            }),
        })
    }

    /// Return a clone of the stable runtime identity for this daemon.
    pub fn runtime_identity(&self) -> RuntimeIdentity {
        self.inner.runtime_identity.clone()
    }

    /// Start the egress CONNECT proxy using the system DNS resolver.
    ///
    /// Called once by `Daemon::start` after `DaemonState::new`.
    /// Failure here is a hard daemon startup error.
    pub async fn init_egress(
        &self,
        resolver: Arc<dyn ato_net::resolver::Resolver + Send + Sync>,
    ) -> anyhow::Result<()> {
        let policy = Arc::new(EgressPolicy::permissive());
        // Bounded channel — receipts are best-effort; a closed or full
        // channel must never block the relay.  Capacity 1024 is generous
        // for a session-scoped daemon.
        let (receipt_tx, receipt_rx) = tokio::sync::mpsc::channel(1024);
        // Drain receipts in a background task.  Slice E does not persist
        // receipts to disk; that wiring lands in a follow-up.
        tokio::spawn(async move {
            let mut rx = receipt_rx;
            while rx.recv().await.is_some() {}
        });
        let mgr = EgressManager::start(resolver, policy, receipt_tx).await?;
        *self.inner.egress.lock().await = Some(mgr);
        Ok(())
    }

    /// The port the egress proxy is listening on, or `None` if not started.
    pub async fn egress_port(&self) -> Option<u16> {
        self.inner.egress.lock().await.as_ref().map(|m| m.port())
    }

    /// Shut down the egress proxy accept loop.
    pub async fn shutdown_egress(&self) {
        if let Some(mgr) = self.inner.egress.lock().await.take() {
            mgr.shutdown().await;
        }
    }

    /// Seconds elapsed since [`Self::new`].
    pub fn uptime_secs(&self) -> u64 {
        self.inner.started_at.elapsed().as_secs()
    }

    /// Snapshot the currently-registered ingress routes as
    /// `(key, port)` pairs.
    pub async fn listener_infos(&self) -> Vec<(String, u16)> {
        self.inner.ingress.lock().await.listener_infos()
    }

    /// Lock the ingress manager for mutation (register / deregister).
    pub fn ingress(&self) -> &Mutex<IngressManager> {
        &self.inner.ingress
    }

    /// Mark the daemon as shutting down. Safe to call from any task
    /// (control-socket handler, signal hook, etc.); never blocks.
    pub fn signal_shutdown(&self) {
        // `send_replace` (not `send`) is required: `send` fails when
        // there are no current receivers, which on our shape happens
        // any time `signal_shutdown` fires before the accept loop has
        // subscribed (or after it has temporarily dropped its
        // receiver to spawn a connection handler). The sticky-value
        // guarantee depends on the cell actually being updated, so we
        // use the receiver-count-independent setter.
        self.inner.shutdown_tx.send_replace(true);
    }

    /// Resolves when [`Self::signal_shutdown`] has been called, even
    /// if the call happened before this method was invoked.
    pub async fn wait_for_shutdown(&self) {
        let mut rx = self.inner.shutdown_tx.subscribe();
        // Check the current value first — handles the "signal arrived
        // before we subscribed" case without sleeping.
        if *rx.borrow_and_update() {
            return;
        }
        let _ = rx.changed().await;
    }

    /// Shut down all ingress accept loops and await active connections.
    /// Called by the main loop once `wait_for_shutdown` resolves.
    pub async fn shutdown_ingress(&self) {
        self.inner.ingress.lock().await.shutdown_all().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    async fn make_state() -> DaemonState {
        let dir = tempdir().unwrap();
        DaemonState::new(dir.path().to_path_buf()).await.unwrap()
    }

    #[tokio::test]
    async fn wait_for_shutdown_observes_signal_after_subscribe() {
        let state = make_state().await;
        let waiter = state.clone();
        let task = tokio::spawn(async move { waiter.wait_for_shutdown().await });
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        state.signal_shutdown();
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .expect("waiter should observe shutdown within 1s")
            .expect("waiter task should not panic");
    }

    #[tokio::test]
    async fn wait_for_shutdown_returns_immediately_if_signal_already_fired() {
        let state = make_state().await;
        state.signal_shutdown();
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            state.wait_for_shutdown(),
        )
        .await
        .expect("late subscriber must observe a sticky shutdown signal");
    }
}
