//! Egress CONNECT proxy manager — Slice E (#300).
//!
//! [`EgressManager`] owns a TCP listener bound to `127.0.0.1:0` (ephemeral
//! port) and an accept loop that spawns [`handler::handle_connect`] for
//! each incoming connection.
//!
//! ## Lifecycle
//!
//! ```text
//! EgressManager::start(resolver, policy, receipt_tx) → EgressManager
//! EgressManager::port()     → u16   (egress proxy port to put in StatusReport)
//! EgressManager::shutdown() → ()    (cancels accept loop, awaits task)
//! ```
//!
//! The `DaemonState` stores an `Option<EgressManager>` under a `Mutex` and
//! calls `init_egress` once after construction.  `shutdown_egress` is called
//! alongside `shutdown_ingress` in the main run loop.
//!
//! ## Receipt channel
//!
//! `receipt_tx` is a bounded channel (capacity chosen by the caller).
//! Sends use `try_send` so a full or closed channel never blocks the relay.
//! This is intentional best-effort delivery; Slice E does not persist
//! receipts to disk.

pub mod handler;
pub mod policy;

use std::sync::Arc;

use crate::net::{receipt::NetworkEgressDecision, resolver::Resolver};
use tokio::{
    net::TcpListener,
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tracing::{info, warn};

use handler::handle_connect;
use policy::EgressPolicy;

/// Manages the egress HTTP CONNECT proxy listener.
pub struct EgressManager {
    port: u16,
    shutdown_tx: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl EgressManager {
    /// Bind an ephemeral port and start the accept loop.
    ///
    /// Fails immediately if the port cannot be bound.
    pub async fn start(
        resolver: Arc<dyn Resolver + Send + Sync>,
        policy: Arc<EgressPolicy>,
        receipt_tx: mpsc::Sender<NetworkEgressDecision>,
    ) -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let task = tokio::spawn(accept_loop(
            listener,
            resolver,
            policy,
            receipt_tx,
            shutdown_rx,
        ));

        info!(port, "ato-netd: egress CONNECT proxy started");
        Ok(Self {
            port,
            shutdown_tx,
            task,
        })
    }

    /// The `127.0.0.1` port the proxy is listening on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Cancel the accept loop and wait for it to exit.
    pub async fn shutdown(self) {
        self.shutdown_tx.send_replace(true);
        let _ = self.task.await;
        info!("ato-netd: egress CONNECT proxy stopped");
    }
}

// ── Accept loop ───────────────────────────────────────────────────────────────

async fn accept_loop(
    listener: TcpListener,
    resolver: Arc<dyn Resolver + Send + Sync>,
    policy: Arc<EgressPolicy>,
    receipt_tx: mpsc::Sender<NetworkEgressDecision>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            // Shutdown signal — sticky value: fires even if the sender
            // fired before this branch polled (watch semantics).
            shutdown = shutdown_rx.wait_for(|&v| v) => {
                if shutdown.is_ok() {
                    break;
                }
            }
            accept = listener.accept() => {
                match accept {
                    Ok((stream, _peer)) => {
                        let resolver = resolver.clone();
                        let policy = policy.clone();
                        let tx = receipt_tx.clone();
                        tokio::spawn(handle_connect(stream, policy, resolver, tx));
                    }
                    Err(e) => {
                        warn!("ato-netd: egress accept error: {e}");
                    }
                }
            }
        }
    }
}
