//! Typed transport client for the `ato-netd` control plane.
//!
//! The wire types ([`Request`], [`Response`], [`StatusReport`], [`Error`],
//! …) are single-sourced from `ato_ipc::net::control` and re-exported
//! here; this module owns only the local-socket transport (a Unix-domain
//! socket on Unix, a named pipe on Windows).
//!
//! The CLI uses both flavours: the async [`Client`] for the
//! ingress-deregistration paths driven from async command handlers, and the
//! blocking [`SyncClient`] for the synchronous netd lifecycle helper in
//! [`crate::common::netd`]. This was previously the shared `ato-net` crate;
//! it was dissolved so each caller carries the thin transport it needs over
//! the single-sourced wire types.

use std::path::{Path, PathBuf};

#[allow(unused_imports)]
pub use ato_ipc::net::control::{
    BootstrapTokenInfo, Error, ErrorPayload, IngressInfo, ListenerInfo, Request, Response,
    ResponseResult, StatusReport, default_socket_path,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf, split};

#[cfg(unix)]
type AsyncTransport = tokio::net::UnixStream;
#[cfg(windows)]
type AsyncTransport = tokio::net::windows::named_pipe::NamedPipeClient;

#[cfg(unix)]
type SyncTransport = std::os::unix::net::UnixStream;
#[cfg(windows)]
type SyncTransport = std::fs::File;

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

fn connect_sync_transport(socket_path: &Path) -> Result<SyncTransport, Error> {
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(socket_path)
            .map_err(|err| map_connect_error(socket_path, err))
    }

    #[cfg(windows)]
    {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(socket_path)
            .map_err(|err| map_connect_error(socket_path, err))
    }
}

/// Async typed client for the `ato-netd` control plane.
///
/// Each [`Client`] owns one connection and is not `Clone`; consumers that need
/// concurrent access should construct multiple clients (the daemon accepts
/// many concurrent connections).
pub struct Client {
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

// Client exposes the full netd control surface; some methods are kept as API
// for callers/tests that don't exist yet in this build.
#[allow(dead_code)]
impl Client {
    /// Connect to the default control socket ([`default_socket_path`]).
    pub async fn connect_default() -> Result<Self, Error> {
        let path = default_socket_path()?;
        Self::connect(&path).await
    }

    /// Connect to a control socket at the given path.
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
            other => Err(unexpected("StatusReport", other)),
        }
    }

    /// Send a `Shutdown` request. Consumes the client because the connection
    /// is expected to close shortly after the ack.
    pub async fn shutdown(mut self) -> Result<(), Error> {
        let _ = self.call(Request::Shutdown).await?;
        Ok(())
    }

    /// Register (or idempotently re-register) an ingress route.
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
            other => Err(unexpected("IngressRegistered", other)),
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
            other => Err(unexpected("Empty", other)),
        }
    }

    /// Register a session-unique ephemeral ingress route (not persisted).
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
            other => Err(unexpected("IngressRegistered", other)),
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
            other => Err(unexpected("Empty", other)),
        }
    }

    /// Retrieve the bootstrap control token for this daemon.
    pub async fn query_bootstrap_token(&mut self) -> Result<BootstrapTokenInfo, Error> {
        match self.call(Request::BootstrapToken).await? {
            ResponseResult::BootstrapToken(info) => Ok(info),
            other => Err(unexpected("BootstrapToken", other)),
        }
    }

    /// Low-level request/response helper.
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
        parse_response(&buf)
    }
}

/// Synchronous (blocking) client for the `ato-netd` control plane.
///
/// Identical wire protocol to [`Client`] but uses blocking I/O so it can be
/// called from non-async contexts such as the synchronous netd lifecycle
/// helper.
pub struct SyncClient {
    socket_path: PathBuf,
    reader: std::io::BufReader<SyncTransport>,
    writer: SyncTransport,
}

impl std::fmt::Debug for SyncClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncClient")
            .field("socket_path", &self.socket_path)
            .finish_non_exhaustive()
    }
}

// SyncClient mirrors Client's control surface; some methods are kept as API
// for callers/tests that don't exist yet in this build.
#[allow(dead_code)]
impl SyncClient {
    /// Connect to the default control socket ([`default_socket_path`]).
    pub fn connect_default() -> Result<Self, Error> {
        let path = default_socket_path()?;
        Self::connect(&path)
    }

    /// Connect to a control socket at the given path.
    pub fn connect(socket_path: &Path) -> Result<Self, Error> {
        let stream = connect_sync_transport(socket_path)?;
        let writer = stream.try_clone().map_err(Error::Io)?;
        Ok(Self {
            socket_path: socket_path.to_path_buf(),
            reader: std::io::BufReader::new(stream),
            writer,
        })
    }

    /// Register (or idempotently re-register) an ingress route.
    pub fn register_ingress(
        &mut self,
        key: &str,
        upstream_url: &str,
    ) -> Result<IngressInfo, Error> {
        match self.call(Request::RegisterIngress {
            key: key.to_string(),
            upstream_url: upstream_url.to_string(),
        })? {
            ResponseResult::IngressRegistered(info) => Ok(info),
            other => Err(unexpected("IngressRegistered", other)),
        }
    }

    /// Remove a previously-registered ingress route. Idempotent.
    pub fn deregister_ingress(&mut self, key: &str) -> Result<(), Error> {
        match self.call(Request::DeregisterIngress {
            key: key.to_string(),
        })? {
            ResponseResult::Empty {} => Ok(()),
            other => Err(unexpected("Empty", other)),
        }
    }

    /// Register a session-unique ephemeral ingress route (not persisted).
    pub fn register_ephemeral_ingress(
        &mut self,
        session_key: &str,
        upstream_url: &str,
    ) -> Result<IngressInfo, Error> {
        match self.call(Request::RegisterEphemeralIngress {
            session_key: session_key.to_string(),
            upstream_url: upstream_url.to_string(),
        })? {
            ResponseResult::IngressRegistered(info) => Ok(info),
            other => Err(unexpected("IngressRegistered", other)),
        }
    }

    /// Remove a previously-registered ephemeral ingress route. Idempotent.
    pub fn deregister_ephemeral_ingress(&mut self, session_key: &str) -> Result<(), Error> {
        match self.call(Request::DeregisterEphemeralIngress {
            session_key: session_key.to_string(),
        })? {
            ResponseResult::Empty {} => Ok(()),
            other => Err(unexpected("Empty", other)),
        }
    }

    /// Retrieve the bootstrap control token for this daemon.
    pub fn query_bootstrap_token(&mut self) -> Result<BootstrapTokenInfo, Error> {
        match self.call(Request::BootstrapToken)? {
            ResponseResult::BootstrapToken(info) => Ok(info),
            other => Err(unexpected("BootstrapToken", other)),
        }
    }

    /// Send a `Status` request and parse the response.
    pub fn status(&mut self) -> Result<StatusReport, Error> {
        match self.call(Request::Status)? {
            ResponseResult::Status(s) => Ok(s),
            other => Err(unexpected("StatusReport", other)),
        }
    }

    /// Send a `Shutdown` request. Consumes the client because the connection
    /// is expected to close shortly after the ack.
    pub fn shutdown(mut self) -> Result<(), Error> {
        let _ = self.call(Request::Shutdown)?;
        Ok(())
    }

    /// Low-level request/response helper.
    pub fn call(&mut self, request: Request) -> Result<ResponseResult, Error> {
        use std::io::{BufRead, Write};
        let mut line = serde_json::to_string(&request)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).map_err(Error::Io)?;
        self.writer.flush().map_err(Error::Io)?;
        let mut buf = String::new();
        let n = self.reader.read_line(&mut buf).map_err(Error::Io)?;
        if n == 0 {
            return Err(Error::PrematureClose);
        }
        parse_response(&buf)
    }
}

fn parse_response(buf: &str) -> Result<ResponseResult, Error> {
    let parsed: Response = serde_json::from_str(buf.trim_end())?;
    match parsed {
        Response::Ok { result } => Ok(result),
        Response::Error { error } => Err(Error::DaemonError {
            code: error.code,
            message: error.message,
        }),
    }
}

fn unexpected(expected: &str, got: ResponseResult) -> Error {
    Error::DaemonError {
        code: "unexpected_response".into(),
        message: format!("expected {expected}, got: {got:?}"),
    }
}
