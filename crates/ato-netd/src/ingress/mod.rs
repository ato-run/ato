//! Ingress route manager — coordinates port allocation, per-route TCP
//! listeners, and connection lifecycle.
//!
//! # Route lifecycle
//!
//! 1. `register_or_swap(key, upstream_url)` is called from the control
//!    socket handler when a `RegisterIngress` request arrives.
//!    - Port is looked up or assigned by [`PortAllocator`].
//!    - A `TcpListener` is bound on `127.0.0.1:<port>`.
//!    - An accept loop task is spawned; it runs until the per-route
//!      `watch::Sender<bool>` fires `true`.
//!    - If the key already exists the upstream `Url` is swapped in-place
//!      (the listener keeps running, port unchanged).
//!
//! 2. `deregister(key)` cancels the accept loop and stops the listener, but
//!    keeps the port assignment in the persistent allocator so the next
//!    `register_or_swap` for the same key re-uses the same port.
//!    Use `purge(key)` to fully remove the port allocation (e.g., uninstall).
//!
//! 3. `shutdown_all()` cancels every accept loop and awaits all tasks.

pub mod allocator;
pub mod hop_by_hop;
pub mod proxy;

use std::{collections::HashMap, net::SocketAddr, path::PathBuf, sync::Arc};

use allocator::{AllocError, PortAllocator};
use anyhow::Context as _;
use ato_net::control::IngressInfo;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::{
    net::TcpListener,
    sync::{watch, RwLock},
    task::JoinSet,
};
use tracing::{debug, warn};
use url::Url;

/// Public error type for ingress operations.
#[derive(Debug, thiserror::Error)]
pub enum IngressError {
    #[error("port allocator error: {0}")]
    Alloc(#[from] AllocError),
    #[error("could not bind 127.0.0.1:{port}: {source}")]
    Bind {
        port: u16,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid upstream URL {url:?}: {source}")]
    InvalidUrl {
        url: String,
        #[source]
        source: url::ParseError,
    },
}

struct RouteHandle {
    port: u16,
    upstream: Arc<RwLock<Url>>,
    /// Send `true` to cancel the accept loop for this route.
    cancel_tx: watch::Sender<bool>,
    /// Set of active per-connection proxy tasks. Awaited on deregister /
    /// shutdown to ensure clean close.
    join_set: Arc<tokio::sync::Mutex<JoinSet<()>>>,
}

/// Manager for all active ingress routes.  Owned by `DaemonStateInner`
/// behind a `tokio::sync::Mutex`.
pub struct IngressManager {
    alloc: PortAllocator,
    routes: HashMap<String, RouteHandle>,
}

impl IngressManager {
    /// Load the allocator from `ato_home` and return a ready manager.
    pub async fn new(ato_home: &PathBuf) -> anyhow::Result<Self> {
        let json_path = ato_home.join("state/netd/stable_origin_ports.json");
        let alloc = PortAllocator::load(json_path)
            .await
            .context("loading stable_origin_ports.json")?;
        Ok(Self {
            alloc,
            routes: HashMap::new(),
        })
    }

    /// Register (or idempotently re-register) an ingress route.
    ///
    /// - Same key + same upstream → returns existing port, no-op.
    /// - Same key + different upstream → swaps upstream, returns same port.
    /// - New key → allocates port, binds listener, spawns accept loop.
    pub async fn register_or_swap(
        &mut self,
        key: &str,
        upstream_url_str: &str,
    ) -> Result<IngressInfo, IngressError> {
        let upstream_url = Url::parse(upstream_url_str).map_err(|e| IngressError::InvalidUrl {
            url: upstream_url_str.to_string(),
            source: e,
        })?;

        if let Some(handle) = self.routes.get(key) {
            // Existing route: just swap upstream and return the stable port.
            let mut upstream = handle.upstream.write().await;
            *upstream = upstream_url;
            return Ok(IngressInfo { port: handle.port });
        }

        // New route: allocate port, bind, and start accept loop.
        let port = self.alloc.get_or_assign(key).await?;

        let listener = {
            let addr = SocketAddr::from(([127, 0, 0, 1], port));
            let sock = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))
                .map_err(|e| IngressError::Bind { port, source: e })?;
            sock.set_reuse_address(true)
                .map_err(|e| IngressError::Bind { port, source: e })?;
            #[cfg(unix)]
            sock.set_reuse_port(true)
                .map_err(|e| IngressError::Bind { port, source: e })?;
            sock.set_nonblocking(true)
                .map_err(|e| IngressError::Bind { port, source: e })?;
            sock.bind(&addr.into())
                .map_err(|e| IngressError::Bind { port, source: e })?;
            sock.listen(128)
                .map_err(|e| IngressError::Bind { port, source: e })?;
            TcpListener::from_std(sock.into())
                .map_err(|e| IngressError::Bind { port, source: e })?
        };

        let upstream = Arc::new(RwLock::new(upstream_url));
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let join_set = Arc::new(tokio::sync::Mutex::new(JoinSet::<()>::new()));

        // Spawn the accept loop.
        {
            let upstream_clone = Arc::clone(&upstream);
            let join_set_clone = Arc::clone(&join_set);
            let key_clone = key.to_string();
            tokio::spawn(run_ingress_listener(
                listener,
                upstream_clone,
                join_set_clone,
                cancel_rx,
                key_clone,
            ));
        }

        self.routes.insert(
            key.to_string(),
            RouteHandle {
                port,
                upstream,
                cancel_tx,
                join_set,
            },
        );

        Ok(IngressInfo { port })
    }

    /// Deregister a route. No-op if key is unknown.
    ///
    /// Stops the listener and drains active connections, but deliberately
    /// keeps the port assignment in the persistent allocator. This means
    /// calling `register` again for the same key will re-use the exact same
    /// port, preserving WebView origin and browser storage (IndexedDB,
    /// localStorage, Service Workers) across session stop/restart cycles.
    ///
    /// Use `purge` if you need to completely remove the port assignment
    /// (e.g., on app uninstall).
    pub async fn deregister(&mut self, key: &str) {
        if let Some(handle) = self.routes.remove(key) {
            // Cancel the accept loop.
            let _ = handle.cancel_tx.send(true);
            // Wait for all active proxy connections on this route.
            let mut js = handle.join_set.lock().await;
            while js.join_next().await.is_some() {}
            // Intentionally do NOT remove from allocator: the port stays
            // reserved in stable_origin_ports.json so the next register
            // call for this key returns the same port.
        }
    }

    /// Permanently remove a route and release its port allocation.
    /// Use only for app uninstall; prefer `deregister` for session stop.
    #[allow(dead_code)]
    pub async fn purge(&mut self, key: &str) {
        if let Some(handle) = self.routes.remove(key) {
            let _ = handle.cancel_tx.send(true);
            let mut js = handle.join_set.lock().await;
            while js.join_next().await.is_some() {}
        }
        if let Err(e) = self.alloc.remove(key).await {
            warn!("failed to remove key {key:?} from allocator: {e}");
        }
    }

    /// Snapshot of registered routes: key → port.
    pub fn listener_infos(&self) -> Vec<(String, u16)> {
        self.routes
            .iter()
            .map(|(k, v)| (k.clone(), v.port))
            .collect()
    }

    /// Cancel all accept loops and await all active connections.
    pub async fn shutdown_all(&mut self) {
        for (key, handle) in self.routes.drain() {
            let _ = handle.cancel_tx.send(true);
            let mut js = handle.join_set.lock().await;
            while js.join_next().await.is_some() {}
            debug!("ingress route {key:?} shut down");
        }
    }
}

// ── Accept loop ────────────────────────────────────────────────────────────

async fn run_ingress_listener(
    listener: TcpListener,
    upstream: Arc<RwLock<Url>>,
    join_set: Arc<tokio::sync::Mutex<JoinSet<()>>>,
    mut cancel_rx: watch::Receiver<bool>,
    key: String,
) {
    loop {
        tokio::select! {
            biased;
            _ = cancel_rx.changed() => {
                if *cancel_rx.borrow() {
                    debug!("ingress accept loop for {key:?} received cancel signal");
                    break;
                }
            }
            result = listener.accept() => {
                match result {
                    Ok((stream, client_addr)) => {
                        let upstream_clone = Arc::clone(&upstream);
                        let task = tokio::spawn(serve_connection(stream, upstream_clone, client_addr));
                        join_set.lock().await.spawn(async move {
                            let _ = task.await;
                        });
                    }
                    Err(e) => {
                        warn!("accept error on ingress route {key:?}: {e}");
                        // Brief back-off to avoid spinning on a transient error.
                        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                    }
                }
            }
        }
    }

    debug!("ingress accept loop for {key:?} exited");
}

async fn serve_connection(
    stream: tokio::net::TcpStream,
    upstream: Arc<RwLock<Url>>,
    client_addr: SocketAddr,
) {
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;

    let io = TokioIo::new(stream);

    let svc = service_fn(move |req| {
        let upstream = Arc::clone(&upstream);
        async move { proxy::proxy_request(req, upstream, client_addr).await }
    });

    if let Err(e) = hyper::server::conn::http1::Builder::new()
        .serve_connection(io, svc)
        .with_upgrades()
        .await
    {
        debug!("ingress connection error from {client_addr}: {e}");
    }
}
