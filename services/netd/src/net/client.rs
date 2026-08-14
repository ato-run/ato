//! Async control-plane client for the `ato-netd` daemon.
//!
//! The wire types ([`Request`], [`Response`], [`StatusReport`], [`Error`],
//! …) are single-sourced from `ato_ipc::net::control`; this module owns
//! only the Tokio transport that exchanges them over the local control socket
//! (a Unix-domain socket on Unix, a named pipe on Windows).
//!
//! `ato-netd` uses this client for self-probes (`--status`, `--shutdown`,
//! stale-socket detection) and the integration tests drive the full verb set
//! through it.

use std::path::{Path, PathBuf};

use ato_ipc::net::control::{
    BootstrapTokenInfo, Error, IngressInfo, Request, Response, ResponseResult, StatusReport,
    default_socket_path,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf, split};

#[cfg(unix)]
type AsyncTransport = tokio::net::UnixStream;
#[cfg(windows)]
type AsyncTransport = tokio::net::windows::named_pipe::NamedPipeClient;

fn map_connect_error(socket_path: &Path, err: std::io::Error) -> Error {
    #[cfg(windows)]
    if err.raw_os_error() == Some(231) {
        return Error::NotRunning {
            path: socket_path.to_path_buf(),
            source: err,
        };
    }

    match err.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => Error::NotRunning {
            path: socket_path.to_path_buf(),
            source: err,
        },
        std::io::ErrorKind::PermissionDenied => Error::PermissionDenied {
            path: socket_path.to_path_buf(),
            source: err,
        },
        _ => Error::Io(err),
    }
}

async fn connect_async_transport(socket_path: &Path) -> Result<AsyncTransport, Error> {
    #[cfg(unix)]
    {
        tokio::net::UnixStream::connect(socket_path)
            .await
            .map_err(|err| map_connect_error(socket_path, err))
    }

    #[cfg(windows)]
    {
        tokio::net::windows::named_pipe::ClientOptions::new()
            .open(socket_path)
            .map_err(|err| map_connect_error(socket_path, err))
    }
}

/// Typed async client for the `ato-netd` control plane.
///
/// Each [`Client`] owns one connection. The client is not `Clone` because the
/// connection is single-threaded request/response — consumers that need
/// concurrent access should construct multiple clients (the daemon accepts
/// many concurrent connections).
pub struct Client {
    /// Path the client connected to. Recorded so error messages can
    /// surface the path the consumer actually used.
    socket_path: PathBuf,
    reader: BufReader<ReadHalf<AsyncTransport>>,
    writer: WriteHalf<AsyncTransport>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("socket_path", &self.socket_path)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Connect to the default control socket ([`default_socket_path`]).
    pub async fn connect_default() -> Result<Self, Error> {
        let path = default_socket_path()?;
        Self::connect(&path).await
    }

    /// Connect to a control socket at the given path. Returns
    /// [`Error::NotRunning`] when the control endpoint is absent or
    /// refuses connections and [`Error::PermissionDenied`] when the OS
    /// denied access while the daemon is likely running.
    pub async fn connect(socket_path: &Path) -> Result<Self, Error> {
        let stream = connect_async_transport(socket_path).await?;
        let (read_half, writer) = split(stream);
        Ok(Self {
            socket_path: socket_path.to_path_buf(),
            reader: BufReader::new(read_half),
            writer,
        })
    }

    /// Path this client connected to. Useful for diagnostics.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Send a `Status` request and parse the response.
    pub async fn status(&mut self) -> Result<StatusReport, Error> {
        match self.call(Request::Status).await? {
            ResponseResult::Status(s) => Ok(s),
            other => Err(Error::DaemonError {
                code: "unexpected_response".into(),
                message: format!("expected StatusReport, got: {other:?}"),
            }),
        }
    }

    /// Send a `Shutdown` request. Consumes the client because the
    /// connection is expected to close shortly after the ack.
    pub async fn shutdown(mut self) -> Result<(), Error> {
        let _ = self.call(Request::Shutdown).await?;
        Ok(())
    }

    /// Register (or idempotently re-register) an ingress route.
    ///
    /// Same `key` → same stable port returned, upstream updated.
    /// Returns [`IngressInfo`] with the assigned port.
    pub async fn register_ingress(
        &mut self,
        key: &str,
        upstream_url: &str,
    ) -> Result<IngressInfo, Error> {
        match self
            .call(Request::RegisterIngress {
                key: key.to_string(),
                upstream_url: upstream_url.to_string(),
            })
            .await?
        {
            ResponseResult::IngressRegistered(info) => Ok(info),
            other => Err(Error::DaemonError {
                code: "unexpected_response".into(),
                message: format!("expected IngressRegistered, got: {other:?}"),
            }),
        }
    }

    /// Remove a previously-registered ingress route. Idempotent.
    pub async fn deregister_ingress(&mut self, key: &str) -> Result<(), Error> {
        match self
            .call(Request::DeregisterIngress {
                key: key.to_string(),
            })
            .await?
        {
            ResponseResult::Empty {} => Ok(()),
            other => Err(Error::DaemonError {
                code: "unexpected_response".into(),
                message: format!("expected Empty, got: {other:?}"),
            }),
        }
    }

    /// Register a session-unique ephemeral ingress route (not persisted).
    ///
    /// Returns [`IngressInfo`] with the assigned port. The port is
    /// in-memory only and will not be reused immediately after
    /// [`deregister_ephemeral_ingress`](Self::deregister_ephemeral_ingress).
    pub async fn register_ephemeral_ingress(
        &mut self,
        session_key: &str,
        upstream_url: &str,
    ) -> Result<IngressInfo, Error> {
        match self
            .call(Request::RegisterEphemeralIngress {
                session_key: session_key.to_string(),
                upstream_url: upstream_url.to_string(),
            })
            .await?
        {
            ResponseResult::IngressRegistered(info) => Ok(info),
            other => Err(Error::DaemonError {
                code: "unexpected_response".into(),
                message: format!("expected IngressRegistered, got: {other:?}"),
            }),
        }
    }

    /// Remove a previously-registered ephemeral ingress route. Idempotent.
    pub async fn deregister_ephemeral_ingress(&mut self, session_key: &str) -> Result<(), Error> {
        match self
            .call(Request::DeregisterEphemeralIngress {
                session_key: session_key.to_string(),
            })
            .await?
        {
            ResponseResult::Empty {} => Ok(()),
            other => Err(Error::DaemonError {
                code: "unexpected_response".into(),
                message: format!("expected Empty, got: {other:?}"),
            }),
        }
    }

    /// Retrieve the bootstrap control token for this daemon.
    ///
    /// This is a separate verb from `status` so the token is never
    /// included in routine status dumps or logs.
    pub async fn query_bootstrap_token(&mut self) -> Result<BootstrapTokenInfo, Error> {
        match self.call(Request::BootstrapToken).await? {
            ResponseResult::BootstrapToken(info) => Ok(info),
            other => Err(Error::DaemonError {
                code: "unexpected_response".into(),
                message: format!("expected BootstrapToken, got: {other:?}"),
            }),
        }
    }

    /// Low-level request/response helper. Public for tests and future
    /// verbs; consumers should prefer the verb-specific methods above.
    pub async fn call(&mut self, request: Request) -> Result<ResponseResult, Error> {
        let mut line = serde_json::to_string(&request)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;

        let mut buf = String::new();
        let n = self.reader.read_line(&mut buf).await?;
        if n == 0 {
            return Err(Error::PrematureClose);
        }
        let parsed: Response = serde_json::from_str(buf.trim_end())?;
        match parsed {
            Response::Ok { result } => Ok(result),
            Response::Error { error } => Err(Error::DaemonError {
                code: error.code,
                message: error.message,
            }),
        }
    }
}
