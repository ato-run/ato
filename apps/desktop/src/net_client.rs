//! Blocking transport client for the `ato-netd` control plane.
//!
//! The wire types ([`Request`], [`Response`], [`StatusReport`], [`Error`],
//! …) are single-sourced from `ato_ipc::net::control` and re-exported
//! here; this module owns only the blocking local-socket transport (a
//! Unix-domain socket on Unix, a named pipe on Windows).
//!
//! Desktop only ever talks to `ato-netd` synchronously (from the GPUI event
//! loop), so it carries just the blocking [`SyncClient`] — no async variant.
//! This was previously the shared `ato-net` crate; it was dissolved so each
//! caller carries the thin transport it needs over the single-sourced wire
//! types.

use std::path::{Path, PathBuf};

#[allow(unused_imports)]
pub use ato_ipc::net::control::{
    BootstrapTokenInfo, Error, ErrorPayload, IngressInfo, ListenerInfo, Request, Response,
    ResponseResult, StatusReport, default_socket_path,
};

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

/// Synchronous (blocking) client for the `ato-netd` control plane.
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

    /// Path this client connected to. Useful for diagnostics.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
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

fn unexpected(expected: &str, got: ResponseResult) -> Error {
    Error::DaemonError {
        code: "unexpected_response".into(),
        message: format!("expected {expected}, got: {got:?}"),
    }
}
