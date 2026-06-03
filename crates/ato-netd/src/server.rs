//! Control transport server. Binds the configured local control endpoint,
//! accepts connections, and dispatches `ato_net::control::Request` verbs.
//!
//! The wire layer mirrors `Client`: newline-delimited JSON, one
//! request-response per line.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ato_net::control::{
    BootstrapTokenInfo, ErrorPayload, ListenerInfo, Request, Response, ResponseResult, StatusReport,
};
use ato_net::resolver::SystemResolver;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, split};
#[cfg(unix)]
use tokio::net::UnixListener;
#[cfg(windows)]
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
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
    #[cfg(unix)]
    listener: UnixListener,
    #[cfg(windows)]
    listener: NamedPipeServer,
    socket_path: PathBuf,
    state: DaemonState,
    /// Held so `Drop` can unlink the socket file on the normal exit
    /// path. The `Drop` is best-effort; crashes leave the file behind,
    /// but the next start's `AlreadyRunning` probe is designed to
    /// handle that case.
    _socket_guard: SocketFileGuard,
}

impl Daemon {
    /// Bind the control socket and return a runnable daemon handle.
    ///
    /// `ato_home` is used to derive the path for the port allocator's
    /// JSON persistence file:
    /// `${ato_home}/state/netd/stable_origin_ports.json`.
    pub async fn start(socket_path: PathBuf, ato_home: PathBuf) -> Result<Self, StartError> {
        #[cfg(unix)]
        let listener = {
            if let Some(parent) = socket_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }

            if socket_path.exists() {
                match probe_existing_daemon(&socket_path).await {
                    Some(pid) => {
                        return Err(StartError::AlreadyRunning {
                            pid,
                            path: socket_path.clone(),
                        });
                    }
                    None => {
                        if let Err(err) = tokio::fs::remove_file(&socket_path).await
                            && err.kind() != std::io::ErrorKind::NotFound
                        {
                            return Err(StartError::Io(err));
                        }
                    }
                }
            }

            let listener = UnixListener::bind(&socket_path)?;
            info!(socket = %socket_path.display(), "ato-netd: control socket bound");
            listener
        };

        #[cfg(windows)]
        let listener = {
            if let Some(pid) = probe_existing_daemon(&socket_path).await {
                return Err(StartError::AlreadyRunning {
                    pid,
                    path: socket_path.clone(),
                });
            }

            let listener = create_named_pipe_listener(&socket_path, true)?;
            info!(socket = %socket_path.display(), "ato-netd: control socket bound");
            listener
        };

        let state = DaemonState::new(ato_home).await?;

        let resolver = Arc::new(
            SystemResolver::new()
                .map_err(|e| anyhow::anyhow!("failed to create system resolver: {e}"))?,
        );
        state.init_egress(resolver).await?;

        Ok(Self {
            listener,
            socket_path: socket_path.clone(),
            state,
            _socket_guard: SocketFileGuard::new(socket_path),
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

        #[cfg(unix)]
        {
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
                                warn!(error = %err, "ato-netd: accept failed");
                            }
                        }
                    }
                }
            }
        }

        #[cfg(windows)]
        {
            let mut listener = listener;
            loop {
                tokio::select! {
                    _ = shutdown_signal.wait_for_shutdown() => {
                        info!("ato-netd: shutdown signal received; exiting accept loop");
                        break;
                    }
                    accept = listener.connect() => {
                        match accept {
                            Ok(()) => {
                                let next_listener = create_named_pipe_listener(&socket_path, false)?;
                                let connected = listener;
                                listener = next_listener;
                                let state = state.clone();
                                tokio::spawn(async move {
                                    if let Err(err) = handle_connection(connected, state).await {
                                        warn!(error = %err, "ato-netd: connection handler failed");
                                    }
                                });
                            }
                            Err(err) => {
                                warn!(error = %err, "ato-netd: accept failed");
                            }
                        }
                    }
                }
            }
        }

        state.shutdown_egress().await;
        state.shutdown_ingress().await;

        info!(socket = %socket_path.display(), "ato-netd: daemon exiting cleanly");
        Ok(())
    }
}

/// Probe an existing control endpoint to determine whether a live daemon
/// owns it. Returns the daemon's PID if so, `None` if the endpoint is
/// stale or unreachable. Used during `start` to choose between
/// `AlreadyRunning` and rebind.
async fn probe_existing_daemon(socket_path: &Path) -> Option<u32> {
    let mut client = ato_net::control::Client::connect(socket_path).await.ok()?;
    let report = client.status().await.ok()?;
    Some(report.pid)
}

async fn handle_connection<T>(stream: T, state: DaemonState) -> std::io::Result<()>
where
    T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let (read_half, mut writer) = split(stream);
    let mut reader = BufReader::new(read_half).lines();

    while let Some(line) = reader.next_line().await? {
        let request = serde_json::from_str::<Request>(&line);
        let response: Response = match &request {
            Ok(req) => dispatch(req.clone(), &state).await,
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
                return Err(std::io::Error::other(err));
            }
        };
        writer.write_all(serialized.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;

        if matches!(request, Ok(Request::Shutdown)) && matches!(response, Response::Ok { .. }) {
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
        Request::RegisterEphemeralIngress {
            session_key,
            upstream_url,
        } => {
            match state
                .ingress()
                .lock()
                .await
                .register_ephemeral(&session_key, &upstream_url)
                .await
            {
                Ok(info) => Response::Ok {
                    result: ResponseResult::IngressRegistered(info),
                },
                Err(e) => Response::Error {
                    error: ErrorPayload {
                        code: "ephemeral_ingress_register_failed".into(),
                        message: e.to_string(),
                    },
                },
            }
        }
        Request::DeregisterEphemeralIngress { session_key } => {
            state
                .ingress()
                .lock()
                .await
                .deregister_ephemeral(&session_key)
                .await;
            Response::Ok {
                result: ResponseResult::Empty {},
            }
        }
        Request::BootstrapToken => {
            let identity = state.runtime_identity();
            Response::Ok {
                result: ResponseResult::BootstrapToken(BootstrapTokenInfo {
                    control_token: identity.control_token,
                }),
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
    let identity = state.runtime_identity();
    StatusReport {
        version: env!("CARGO_PKG_VERSION").to_string(),
        pid: std::process::id(),
        uptime_secs: state.uptime_secs(),
        listeners,
        egress_proxy_port,
        runtime_id: Some(identity.runtime_id),
    }
}

#[cfg(windows)]
fn create_named_pipe_listener(
    socket_path: &Path,
    first_instance: bool,
) -> std::io::Result<NamedPipeServer> {
    let mut options = ServerOptions::new();
    options.access_inbound(true);
    options.access_outbound(true);
    options.reject_remote_clients(true);
    if first_instance {
        options.first_pipe_instance(true);
    }
    options.create(socket_path)
}

/// Best-effort cleanup of the socket file when the daemon exits via
/// normal `Drop`. Windows named pipes disappear when their handles are
/// dropped, so cleanup is only needed on Unix.
struct SocketFileGuard {
    #[cfg(unix)]
    path: PathBuf,
}

impl SocketFileGuard {
    fn new(_path: PathBuf) -> Self {
        Self {
            #[cfg(unix)]
            path: _path,
        }
    }
}

impl Drop for SocketFileGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.path);
    }
}
