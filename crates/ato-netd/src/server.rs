//! Control-socket server. Binds a UDS at the configured path, accepts
//! connections, and dispatches `ato_net::control::Request` verbs.
//!
//! The wire layer mirrors `Client`: newline-delimited JSON, one
//! request-response per line.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ato_net::control::{
    ErrorPayload, ListenerInfo, Request, Response, ResponseResult, StatusReport,
};
use ato_net::resolver::SystemResolver;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{debug, info, warn};

use crate::state::DaemonState;

/// Typed failures returned by [`Daemon::start`].
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// Another `ato-netd` already owns the control socket. Surfaced so
    /// a parent process (or `cargo run -p ato-netd` during a smoke
    /// test) can distinguish "I lost the start race" from generic
    /// startup failure.
    #[error("another ato-netd is already listening on {path}")]
    AlreadyRunning { pid: u32, path: PathBuf },
    /// Catch-all I/O during socket setup (bind, parent-dir creation,
    /// unlink-on-bind sequencing).
    #[error("control socket setup failed: {0}")]
    Io(#[from] std::io::Error),
    /// Daemon state initialization failed (e.g., loading the port
    /// allocator from disk).
    #[error("daemon state init failed: {0}")]
    State(#[from] anyhow::Error),
}

pub struct Daemon {
    listener: UnixListener,
    socket_path: PathBuf,
    state: DaemonState,
    /// Held so `Drop` can unlink the socket file on the normal exit
    /// path. The `Drop` is best-effort; SIGKILL / panics leave the
    /// file behind, but the next start's `AlreadyRunning` probe is
    /// designed to handle that case (see `start`).
    _socket_guard: SocketFileGuard,
}

impl Daemon {
    /// Bind the control socket and return a runnable daemon handle.
    ///
    /// `ato_home` is used to derive the path for the port allocator's
    /// JSON persistence file:
    /// `${ato_home}/state/netd/stable_origin_ports.json`.
    pub async fn start(socket_path: PathBuf, ato_home: PathBuf) -> Result<Self, StartError> {
        if let Some(parent) = socket_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Probe: if a socket file already exists, see whether a daemon
        // owns it. The cheapest check is to try connecting as a client
        // — if it accepts and answers `status`, someone is alive; we
        // surface `AlreadyRunning`. If the connect fails (ECONNREFUSED
        // / ENOENT), the file is stale and we unlink it before bind.
        if socket_path.exists() {
            match probe_existing_daemon(&socket_path).await {
                Some(pid) => {
                    return Err(StartError::AlreadyRunning {
                        pid,
                        path: socket_path,
                    })
                }
                None => {
                    // Stale socket file from a crashed predecessor.
                    // Unlink so `bind` can succeed.
                    if let Err(err) = tokio::fs::remove_file(&socket_path).await {
                        if err.kind() != std::io::ErrorKind::NotFound {
                            return Err(StartError::Io(err));
                        }
                    }
                }
            }
        }

        let listener = UnixListener::bind(&socket_path)?;
        info!(socket = %socket_path.display(), "ato-netd: control socket bound");

        let state = DaemonState::new(ato_home).await?;

        // Start the egress CONNECT proxy using the system DNS resolver.
        // Failure here is a hard startup error — the daemon is not useful
        // without an egress proxy for slice E+.
        let resolver = Arc::new(
            SystemResolver::new()
                .map_err(|e| anyhow::anyhow!("failed to create system resolver: {e}"))?,
        );
        state.init_egress(resolver).await?;

        Ok(Self {
            listener,
            socket_path: socket_path.clone(),
            state,
            _socket_guard: SocketFileGuard { path: socket_path },
        })
    }

    /// Run the accept loop until `Shutdown` is requested or the
    /// runtime is cancelled.
    pub async fn run(self) -> Result<(), std::io::Error> {
        let Daemon {
            listener,
            socket_path,
            state,
            _socket_guard,
        } = self;

        let shutdown_signal = state.clone();
        loop {
            tokio::select! {
                _ = shutdown_signal.wait_for_shutdown() => {
                    info!("ato-netd: shutdown signal received; exiting accept loop");
                    break;
                }
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, _addr)) => {
                            let state = state.clone();
                            tokio::spawn(async move {
                                if let Err(err) = handle_connection(stream, state).await {
                                    warn!(error = %err, "ato-netd: connection handler failed");
                                }
                            });
                        }
                        Err(err) => {
                            // Accept errors on UDS are typically benign
                            // (peer closed, EMFILE). Log and continue.
                            warn!(error = %err, "ato-netd: accept failed");
                        }
                    }
                }
            }
        }

        // Gracefully drain all ingress connections before exit.
        state.shutdown_egress().await;
        state.shutdown_ingress().await;

        info!(socket = %socket_path.display(), "ato-netd: daemon exiting cleanly");
        // _socket_guard is dropped here → unlink.
        Ok(())
    }
}

/// Probe an existing socket file to determine whether a live daemon
/// owns it. Returns the daemon's PID if so, `None` if the file is
/// stale or unreachable. Used during `start` to choose between
/// `AlreadyRunning` and "unlink + retry".
async fn probe_existing_daemon(socket_path: &Path) -> Option<u32> {
    // Use the same client every consumer uses — if it can't reach a
    // live daemon at this path, the file is treated as stale.
    let mut client = ato_net::control::Client::connect(socket_path).await.ok()?;
    let report = client.status().await.ok()?;
    Some(report.pid)
}

async fn handle_connection(stream: UnixStream, state: DaemonState) -> std::io::Result<()> {
    let (read_half, mut writer) = stream.into_split();
    let mut reader = BufReader::new(read_half).lines();

    while let Some(line) = reader.next_line().await? {
        let response: Response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => dispatch(req, &state).await,
            Err(err) => Response::Error {
                error: ErrorPayload {
                    code: "invalid_request".into(),
                    message: format!("could not parse control request: {err}"),
                },
            },
        };

        let serialized = match serde_json::to_string(&response) {
            Ok(s) => s,
            Err(err) => {
                warn!(error = %err, "ato-netd: failed to serialize response");
                return Err(std::io::Error::new(std::io::ErrorKind::Other, err));
            }
        };
        writer.write_all(serialized.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;

        // For `Shutdown` we want to ack the client before tearing
        // down the listener, but we don't need to wait for further
        // commands on this connection — the daemon is going away.
        if matches!(response, Response::Ok { .. })
            && matches!(
                serde_json::from_str::<Request>(&line),
                Ok(Request::Shutdown)
            )
        {
            debug!("ato-netd: shutdown ack flushed; signalling accept loop");
            state.signal_shutdown();
            break;
        }
    }
    Ok(())
}

async fn dispatch(request: Request, state: &DaemonState) -> Response {
    match request {
        Request::Status => Response::Ok {
            result: ResponseResult::Status(build_status_report(state).await),
        },
        Request::Shutdown => Response::Ok {
            result: ResponseResult::Empty {},
        },
        Request::RegisterIngress { key, upstream_url } => {
            match state
                .ingress()
                .lock()
                .await
                .register_or_swap(&key, &upstream_url)
                .await
            {
                Ok(info) => Response::Ok {
                    result: ResponseResult::IngressRegistered(info),
                },
                Err(e) => Response::Error {
                    error: ErrorPayload {
                        code: "ingress_register_failed".into(),
                        message: e.to_string(),
                    },
                },
            }
        }
        Request::DeregisterIngress { key } => {
            state.ingress().lock().await.deregister(&key).await;
            Response::Ok {
                result: ResponseResult::Empty {},
            }
        }
    }
}

async fn build_status_report(state: &DaemonState) -> StatusReport {
    let listener_infos = state.listener_infos().await;
    let listeners: Vec<ListenerInfo> = listener_infos
        .into_iter()
        .map(|(key, port)| ListenerInfo { key, port })
        .collect();
    let egress_proxy_port = state.egress_port().await;
    StatusReport {
        version: env!("CARGO_PKG_VERSION").to_string(),
        pid: std::process::id(),
        uptime_secs: state.uptime_secs(),
        listeners,
        egress_proxy_port,
    }
}

/// Best-effort cleanup of the socket file when the daemon exits via
/// normal `Drop`. SIGKILL bypasses this; the next start's probe handles
/// the stale-file case.
struct SocketFileGuard {
    path: PathBuf,
}

impl Drop for SocketFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
