//! Host the Coordinator in-process.
//!
//! The CLI's local workspace needs the Data Ports bridge without requiring a
//! separate `ato-coordinator` binary on PATH. This module owns the same hyper
//! accept loop the binary uses, bound to an ephemeral loopback port, and hands
//! the caller the address plus a shutdown handle.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use http_body_util::BodyExt;
use hyper::Request;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::Coordinator;
use crate::http::{ApiState, AppsConfig, WorkspaceApp, handle_request};

/// Everything the in-process server needs. The database is the authority.
pub struct ServeConfig {
    pub db: PathBuf,
    /// Loopback address to bind (`127.0.0.1:0` for an ephemeral port).
    pub listen: String,
    pub token: String,
    /// Root app served at `/` (the workspace shell), when present.
    pub apps: Option<AppsConfig>,
    /// Pane apps served under `/apps/<prefix>`.
    pub workspace: Vec<WorkspaceApp>,
}

pub struct ServerHandle {
    addr: SocketAddr,
    shutdown: watch::Sender<bool>,
    thread: Option<JoinHandle<()>>,
}

impl ServerHandle {
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Ask the server to stop and wait for its thread.
    pub fn shutdown(mut self) {
        let _ = self.shutdown.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Start the server on an ephemeral loopback port and return once it listens.
pub fn spawn(config: ServeConfig) -> std::io::Result<ServerHandle> {
    let coordinator = Coordinator::open(&config.db).map_err(std::io::Error::other)?;
    let (address_tx, address_rx) = std::sync::mpsc::sync_channel(1);
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let handle_shutdown = shutdown_tx.clone();
    let ServeConfig {
        listen,
        token,
        apps,
        workspace,
        ..
    } = config;
    let thread = std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        else {
            let _ = address_tx.send(Err(std::io::Error::other(
                "the Coordinator runtime could not start",
            )));
            return;
        };
        runtime.block_on(async move {
            let listener = match TcpListener::bind(&listen).await {
                Ok(listener) => listener,
                Err(error) => {
                    let _ = address_tx.send(Err(error));
                    return;
                }
            };
            let address = match listener.local_addr() {
                Ok(address) => address,
                Err(error) => {
                    let _ = address_tx.send(Err(error));
                    return;
                }
            };
            let _ = address_tx.send(Ok(address));
            let state = Arc::new(ApiState {
                coordinator: Mutex::new(coordinator),
                token,
                shutdown: shutdown_tx,
                apps,
                workspace,
            });
            loop {
                tokio::select! {
                    changed = shutdown_rx.changed() => {
                        if changed.is_ok() && *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else {
                            continue;
                        };
                        let state = Arc::clone(&state);
                        tokio::spawn(async move {
                            let service = service_fn(move |request: Request<Incoming>| {
                                let state = Arc::clone(&state);
                                async move {
                                    let (parts, body) = request.into_parts();
                                    let bytes = body
                                        .collect()
                                        .await
                                        .map(|collected| collected.to_bytes())
                                        .unwrap_or_default();
                                    let authorization = parts
                                        .headers
                                        .get("authorization")
                                        .and_then(|value| value.to_str().ok());
                                    let response = handle_request(
                                        &state,
                                        &parts.method,
                                        parts.uri.path(),
                                        authorization,
                                        &bytes,
                                    )
                                    .await;
                                    Ok::<_, std::convert::Infallible>(response)
                                }
                            });
                            let _ = hyper_util::server::conn::auto::Builder::new(
                                TokioExecutor::new(),
                            )
                            .serve_connection(TokioIo::new(stream), service)
                            .await;
                        });
                    }
                }
            }
        });
    });
    let address = address_rx
        .recv()
        .map_err(|_| std::io::Error::other("the Coordinator server did not start"))??;
    Ok(ServerHandle {
        addr: address,
        shutdown: handle_shutdown,
        thread: Some(thread),
    })
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    use super::{ServeConfig, spawn};

    #[test]
    fn an_in_process_coordinator_answers_health() {
        let directory = tempfile::tempdir().unwrap();
        let server = spawn(ServeConfig {
            db: directory.path().join("coordinator.db"),
            listen: "127.0.0.1:0".to_owned(),
            token: "test-token".to_owned(),
            apps: None,
            workspace: Vec::new(),
        })
        .unwrap();
        let address = server.base_url().trim_start_matches("http://").to_owned();
        let mut stream = TcpStream::connect(&address).unwrap();
        stream
            .write_all(b"GET /health HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(response.contains("\"status\":\"ok\""), "{response}");
        server.shutdown();
    }
}
