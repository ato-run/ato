//! Daemon runtime state. Slice **A** ships only the bookkeeping needed
//! to answer the `status` verb (start time, listener inventory).
//! Slice **B** (#297) adds `IngressManager` for the ingress reverse proxy.
//! Subsequent slices grow this with the resolver cache (**D**), the
//! egress policy (**E**), etc.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{watch, Mutex};

use crate::ingress::IngressManager;

/// Shared, cheaply-cloneable handle into the daemon's runtime state.
#[derive(Clone)]
pub struct DaemonState {
    inner: Arc<DaemonStateInner>,
}

struct DaemonStateInner {
    started_at: Instant,
    /// All active ingress routes. Replaces the old `listeners: RwLock<Vec<ListenerInfo>>`
    /// placeholder from slice A — listener info is now derived from the
    /// ingress manager.
    ingress: Mutex<IngressManager>,
    /// Non-lossy shutdown signal.
    ///
    /// The previous skeleton used `tokio::sync::Notify::notify_waiters`,
    /// which **silently drops** the wake-up if no future waiter is
    /// registered at the exact instant `notify_waiters` is called.
    /// With our two-loop shape (the accept loop drops its
    /// `wait_for_shutdown` future to spawn a connection handler, then
    /// re-creates it next iteration), there is a real window in which
    /// `signal_shutdown` from a connection handler races against the
    /// accept loop's re-subscribe and the wake is lost.
    ///
    /// `watch::channel<bool>` closes that race: the value is sticky,
    /// late receivers see it immediately via `borrow_and_update`, and
    /// `changed().await` covers the on-time case.
    shutdown_tx: watch::Sender<bool>,
}

impl DaemonState {
    /// Create a new `DaemonState`.  Loads the port allocator from
    /// `${ato_home}/state/netd/stable_origin_ports.json`.
    pub async fn new(ato_home: PathBuf) -> anyhow::Result<Self> {
        let ingress = IngressManager::new(&ato_home).await?;
        let (shutdown_tx, _) = watch::channel(false);
        Ok(Self {
            inner: Arc::new(DaemonStateInner {
                started_at: Instant::now(),
                ingress: Mutex::new(ingress),
                shutdown_tx,
            }),
        })
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
    /// if the call happened before this method was invoked. The
    /// no-race guarantee is the whole reason we picked
    /// `watch::channel` over `Notify`.
    pub async fn wait_for_shutdown(&self) {
        let mut rx = self.inner.shutdown_tx.subscribe();
        // Check the current value first — handles the "signal arrived
        // before we subscribed" case without sleeping.
        if *rx.borrow_and_update() {
            return;
        }
        // Wait until the sticky value flips. `changed()` returns `Err`
        // only when every `Sender` has been dropped; on our shape that
        // can only happen if the daemon is already going away, so
        // treat it as a shutdown signal too.
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
        // On-time case: subscriber registers first, signal arrives.
        // Notify-based code already handled this; the test guards
        // against a regression introducing a new ordering bug.
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
        // Regression guard for the `Notify::notify_waiters` race that
        // the reviewer flagged: with the old implementation, a signal
        // fired *before* the waiter subscribed was silently lost. With
        // `watch::channel<bool>` the sticky value lets a late
        // subscriber observe the signal immediately.
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
