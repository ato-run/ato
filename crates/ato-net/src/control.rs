//! Control plane between consumers (CLI, Desktop, runtime) and the
//! `ato-netd` daemon.
//!
//! Per #296 the wire protocol lives here so that the daemon
//! (`ato-netd`) and every consumer use the same types — no client-side
//! re-implementation.
//!
//! ## Transport
//!
//! Slice **A** uses **newline-delimited JSON over a local control
//! transport**. Unix uses the canonical `${ATO_HOME}/run/netd.sock`
//! Unix-domain socket; Windows uses the well-known
//! `\\.\pipe\ato-netd-control` named pipe. The wire types
//! ([`Request`], [`Response`], [`StatusReport`], [`Error`], etc.) stay
//! shared across both transports.
//!
//! ## Verbs in slice A
//!
//! - `status` — read-only liveness query. Returns the daemon's
//!   `{version, pid, uptime_secs, listeners}`.
//! - `shutdown` — graceful exit request.
//!
//! Follow-up slices extend [`Request`] / [`Response`] (route table
//! register/swap/deregister for **B**, policy ops for **E**, etc.).
//! Adding verbs in those slices is backwards-compatible because we
//! serialize via `serde_json` tagged enums; older clients ignore new
//! variants.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf, split};

#[cfg(unix)]
type AsyncTransport = tokio::net::UnixStream;
#[cfg(windows)]
type AsyncTransport = tokio::net::windows::named_pipe::NamedPipeClient;

#[cfg(unix)]
type SyncTransport = std::os::unix::net::UnixStream;
#[cfg(windows)]
type SyncTransport = std::fs::File;

/// Default path of the `ato-netd` control endpoint. On Unix this is the
/// canonical socket path inside `ATO_HOME`; on Windows this is the
/// well-known local named-pipe path used by `ato-netd`.
#[cfg(unix)]
pub fn default_socket_path() -> Result<PathBuf, Error> {
    capsule_core::common::paths::ato_path("run/netd.sock").map_err(|err| Error::PathResolve {
        message: err.to_string(),
    })
}

#[cfg(windows)]
pub fn default_socket_path() -> Result<PathBuf, Error> {
    Ok(PathBuf::from(r"\\.\pipe\ato-netd-control"))
}

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

/// Typed errors returned by [`Client`]. Matchable; consumers should
/// pattern-match (do not parse messages).
///
/// The variants are cross-platform so consumers can share portable
/// error-handling logic across Unix sockets and Windows named pipes.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Control socket exists / does not exist but no daemon is
    /// answering. Surfaced for `NotFound` (socket file absent — daemon
    /// never started or cleaned up cleanly) and `ConnectionRefused`
    /// (socket file present but no listener — daemon crashed leaving
    /// a stale file). Consumers branch on this to print
    /// `{"status":"not_running"}` without inspecting kernel error
    /// codes.
    ///
    /// Note: `PermissionDenied` is **not** folded into this variant —
    /// "daemon running but I cannot reach it" is a meaningfully
    /// different operator condition (typically a uid/gid mismatch
    /// against a daemon spawned by another user) and gets its own
    /// [`PermissionDenied`](Self::PermissionDenied) variant so
    /// Desktop / CLI diagnostics can surface an actionable hint.
    #[error("ato-netd is not running (control socket {path} unreachable: {source})")]
    NotRunning {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Control socket is reachable but the OS denied access (UDS
    /// permissions / ACL / SELinux). The daemon is likely running —
    /// the issue is the caller's credentials. Distinct from
    /// [`NotRunning`](Self::NotRunning) so consumers can present a
    /// `chmod` / wrong-user hint instead of a "start the daemon"
    /// hint.
    #[error(
        "control socket {path} refused access (daemon may be running as a different user): {source}"
    )]
    PermissionDenied {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to resolve control socket path: {message}")]
    PathResolve { message: String },
    #[error("control socket I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("control socket protocol error (could not parse daemon response): {0}")]
    Json(#[from] serde_json::Error),
    #[error("daemon returned error: [{code}] {message}")]
    DaemonError { code: String, message: String },
    #[error("daemon closed the control connection without responding")]
    PrematureClose,
}

/// Snapshot returned by the `status` verb. Fields are stable across
/// minor versions; new fields are additive and `#[serde(default)]`-safe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusReport {
    /// `CARGO_PKG_VERSION` of the running daemon.
    pub version: String,
    /// PID of the daemon process. Useful for `ps` / `kill -0` checks
    /// from consumers that want to verify liveness out-of-band.
    pub pid: u32,
    /// Daemon uptime in whole seconds.
    pub uptime_secs: u64,
    /// Active listeners the daemon owns. Empty in slice **A**;
    /// populated by **B** (ingress), **E** (egress CONNECT), etc.
    #[serde(default)]
    pub listeners: Vec<ListenerInfo>,
    /// Port the egress HTTP CONNECT proxy is listening on, if running.
    /// Added in slice **E** (#300). Absent in older daemons; defaults
    /// to `None` via `#[serde(default)]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress_proxy_port: Option<u16>,
    /// Stable UUID identifying this daemon installation. Added in
    /// slice **B** (#382). Absent in older daemons; `None` by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_id: Option<String>,
}

/// Description of an ingress listener owned by the running daemon.
/// Slice **A** never produces any (no listeners exist yet); slice **B**
/// adds the first ones.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListenerInfo {
    /// Stable logical key for this route (opaque string, same as the
    /// `key` passed to `RegisterIngress`).
    pub key: String,
    /// `127.0.0.1` port the daemon is listening on for this route.
    pub port: u16,
}

/// Tagged-enum request envelope. New verbs added in follow-up slices
/// land as new variants; older daemons reject unknown variants with a
/// typed `unknown_method` error rather than panicking.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Request {
    /// Read-only liveness + listener inventory query.
    Status,
    /// Ask the daemon to exit gracefully. The daemon acks with `Ok`
    /// **before** beginning shutdown so clients can observe a successful
    /// reply even though the socket closes shortly after.
    Shutdown,
    /// Register (or idempotently re-register) an ingress reverse-proxy
    /// route. Same `key` → same stable port returned; the upstream is
    /// updated to `upstream_url`. This is the "register-or-swap"
    /// semantic: stable identity is preserved across upstream restarts.
    ///
    /// The daemon binds `127.0.0.1:<port>` and proxies all HTTP,
    /// SSE, long-poll, and WebSocket traffic to `upstream_url`. The
    /// assigned port is persisted to
    /// `${ATO_HOME}/state/netd/stable_origin_ports.json` so that the
    /// same key always gets the same port across daemon restarts.
    RegisterIngress {
        /// Opaque string that identifies this route persistently. Same
        /// key always maps to the same stable port for the lifetime of
        /// the daemon installation.
        key: String,
        /// Upstream base URL. Must be `http://` or `https://`. The proxy
        /// forwards requests to this origin, rewriting `Host` and
        /// `X-Forwarded-*` headers.
        upstream_url: String,
    },
    /// Remove a previously-registered ingress route. Idempotent:
    /// deregistering an unknown key is a no-op success.
    DeregisterIngress {
        /// Key previously passed to `RegisterIngress`.
        key: String,
    },
    /// Register a session-unique ephemeral ingress route.
    ///
    /// Unlike `RegisterIngress`, ephemeral routes are **not** persisted to
    /// `stable_origin_ports.json`. Ports are assigned in-memory only and
    /// cannot be reused by a different capsule within the same daemon
    /// lifetime (see [`DeregisterEphemeralIngress`]). Use this for
    /// transient capsule sessions where a stable origin is undesirable.
    RegisterEphemeralIngress {
        /// Session-unique key (e.g. `"ephemeral:<session_id>"`).
        session_key: String,
        /// Upstream base URL. Must be `http://` or `https://`.
        upstream_url: String,
    },
    /// Remove a previously-registered ephemeral ingress route. Idempotent.
    ///
    /// The released port is moved to a recently-freed set and will not be
    /// immediately reassigned to a new ephemeral route within the same
    /// daemon lifetime.
    DeregisterEphemeralIngress {
        /// Key previously passed to `RegisterEphemeralIngress`.
        session_key: String,
    },
    /// Retrieve the control token for this daemon installation. This is
    /// deliberately a separate verb (not included in `Status`) so that the
    /// token is never emitted in routine status dumps, logs, or diagnostics.
    BootstrapToken,
}

/// Tagged-enum response envelope mirroring [`Request`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok { result: ResponseResult },
    Error { error: ErrorPayload },
}

/// Bootstrap token info returned by the `BootstrapToken` verb.
///
/// This is kept separate from [`StatusReport`] so that the control token
/// never appears in routine status dumps, logs, or diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapTokenInfo {
    /// Opaque bearer token for authenticating remote callers. A 32-byte
    /// random value encoded as 64 lowercase hex characters.
    pub control_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseResult {
    Status(StatusReport),
    /// Result from `RegisterIngress`. Contains the stable port on
    /// `127.0.0.1` that the daemon is listening on for this route.
    IngressRegistered(IngressInfo),
    /// Result from `BootstrapToken`. Contains the control token.
    BootstrapToken(BootstrapTokenInfo),
    /// Empty `{}` body, used for verbs whose only useful signal is
    /// "succeeded" (e.g. `Shutdown`, `DeregisterIngress`).
    Empty {},
}

/// Information about a registered ingress route, returned by
/// `RegisterIngress`. The port is stable: same key always maps to the
/// same port across daemon restarts (persisted to
/// `${ATO_HOME}/state/netd/stable_origin_ports.json`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IngressInfo {
    /// The `127.0.0.1` port the daemon is listening on for this route.
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorPayload {
    pub code: String,
    pub message: String,
}

/// Typed client for the `ato-netd` control plane.
///
/// Each [`Client`] owns one UDS connection. The client is not `Clone`
/// because the connection is single-threaded request/response —
/// consumers that need concurrent access should construct multiple
/// clients (the daemon accepts many concurrent connections).
///
/// The transport is a Unix domain socket on Unix and a local named
/// pipe on Windows. Each [`Client`] owns one connection and is not
/// `Clone`; consumers that need concurrent access should construct
/// multiple clients.
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

/// Synchronous (blocking) client for the `ato-netd` control plane.
///
/// Identical wire protocol to [`Client`] but uses blocking I/O so it can
/// be called from non-async contexts such as the GPUI event loop in
/// `ato-desktop`.
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

    /// Register (or idempotently re-register) an ingress route.
    ///
    /// Same `key` → same stable port returned, upstream updated.
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
            other => Err(Error::DaemonError {
                code: "unexpected_response".into(),
                message: format!("expected IngressRegistered, got: {other:?}"),
            }),
        }
    }

    /// Remove a previously-registered ingress route. Idempotent.
    pub fn deregister_ingress(&mut self, key: &str) -> Result<(), Error> {
        match self.call(Request::DeregisterIngress {
            key: key.to_string(),
        })? {
            ResponseResult::Empty {} => Ok(()),
            other => Err(Error::DaemonError {
                code: "unexpected_response".into(),
                message: format!("expected Empty, got: {other:?}"),
            }),
        }
    }

    /// Register a session-unique ephemeral ingress route (not persisted).
    ///
    /// Returns [`IngressInfo`] with the assigned port.
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
            other => Err(Error::DaemonError {
                code: "unexpected_response".into(),
                message: format!("expected IngressRegistered, got: {other:?}"),
            }),
        }
    }

    /// Remove a previously-registered ephemeral ingress route. Idempotent.
    pub fn deregister_ephemeral_ingress(&mut self, session_key: &str) -> Result<(), Error> {
        match self.call(Request::DeregisterEphemeralIngress {
            session_key: session_key.to_string(),
        })? {
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
    pub fn query_bootstrap_token(&mut self) -> Result<BootstrapTokenInfo, Error> {
        match self.call(Request::BootstrapToken)? {
            ResponseResult::BootstrapToken(info) => Ok(info),
            other => Err(Error::DaemonError {
                code: "unexpected_response".into(),
                message: format!("expected BootstrapToken, got: {other:?}"),
            }),
        }
    }

    /// Send a `Status` request and parse the response.
    pub fn status(&mut self) -> Result<StatusReport, Error> {
        match self.call(Request::Status)? {
            ResponseResult::Status(s) => Ok(s),
            other => Err(Error::DaemonError {
                code: "unexpected_response".into(),
                message: format!("expected StatusReport, got: {other:?}"),
            }),
        }
    }

    /// Send a `Shutdown` request. Consumes the client because the
    /// connection is expected to close shortly after the ack.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serializes_with_method_tag() {
        let json = serde_json::to_string(&Request::Status).unwrap();
        assert_eq!(json, r#"{"method":"status"}"#);
        let json = serde_json::to_string(&Request::Shutdown).unwrap();
        assert_eq!(json, r#"{"method":"shutdown"}"#);
    }

    #[test]
    fn ok_response_round_trips_through_serde() {
        let resp = Response::Ok {
            result: ResponseResult::Status(StatusReport {
                version: "0.5.2".into(),
                pid: 12345,
                uptime_secs: 7,
                listeners: vec![],
                egress_proxy_port: None,
                runtime_id: None,
            }),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: Response = serde_json::from_str(&json).unwrap();
        match back {
            Response::Ok {
                result: ResponseResult::Status(s),
            } => {
                assert_eq!(s.version, "0.5.2");
                assert_eq!(s.pid, 12345);
                assert_eq!(s.uptime_secs, 7);
                assert!(s.listeners.is_empty());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn error_response_round_trips_through_serde() {
        let resp = Response::Error {
            error: ErrorPayload {
                code: "unknown_method".into(),
                message: "no such method".into(),
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: Response = serde_json::from_str(&json).unwrap();
        match back {
            Response::Error { error } => {
                assert_eq!(error.code, "unknown_method");
                assert_eq!(error.message, "no such method");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn empty_result_round_trips() {
        // Shutdown's success response has no payload of interest.
        let resp = Response::Ok {
            result: ResponseResult::Empty {},
        };
        let json = serde_json::to_string(&resp).unwrap();
        // Should parse back without losing the variant.
        let back: Response = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            Response::Ok {
                result: ResponseResult::Empty {}
            }
        ));
    }

    #[cfg(unix)]
    mod sync_client_tests {
        use super::*;
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixListener;

        /// Spawn a minimal fake daemon that reads one newline-delimited JSON
        /// request and writes back a single JSON response, then exits.
        fn spawn_fake_daemon(
            socket_path: &std::path::Path,
            respond_with: Response,
        ) -> std::thread::JoinHandle<()> {
            let listener = UnixListener::bind(socket_path).expect("bind failed in test");
            let respond_with = serde_json::to_string(&respond_with).unwrap();
            std::thread::spawn(move || {
                let (stream, _) = listener.accept().expect("accept failed in test");
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut writer = stream;
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                // Drop the parsed request — we just need to consume it.
                let _: Request = serde_json::from_str(line.trim()).unwrap();
                writer.write_all((respond_with + "\n").as_bytes()).unwrap();
            })
        }

        #[test]
        fn sync_client_status_round_trip() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test-status.sock");
            let resp = Response::Ok {
                result: ResponseResult::Status(StatusReport {
                    version: "0.5.2".into(),
                    pid: 99,
                    uptime_secs: 3,
                    listeners: vec![],
                    egress_proxy_port: None,
                    runtime_id: None,
                }),
            };
            let handle = spawn_fake_daemon(&path, resp);
            let mut client = SyncClient::connect(&path).unwrap();
            let result = client.call(Request::Status).unwrap();
            handle.join().unwrap();
            match result {
                ResponseResult::Status(s) => {
                    assert_eq!(s.version, "0.5.2");
                    assert_eq!(s.pid, 99);
                }
                other => panic!("unexpected: {other:?}"),
            }
        }

        #[test]
        fn sync_client_register_ingress_round_trip() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test-register.sock");
            let resp = Response::Ok {
                result: ResponseResult::IngressRegistered(IngressInfo { port: 19000 }),
            };
            let handle = spawn_fake_daemon(&path, resp);
            let mut client = SyncClient::connect(&path).unwrap();
            let info = client
                .register_ingress("handle:test/demo@1.0.0", "http://127.0.0.1:8080")
                .unwrap();
            handle.join().unwrap();
            assert_eq!(info.port, 19000);
        }

        #[test]
        fn sync_client_daemon_error_maps_to_error() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("test-error.sock");
            let resp = Response::Error {
                error: ErrorPayload {
                    code: "ingress_register_failed".into(),
                    message: "port 19001 is already claimed by key \"other:key\"".into(),
                },
            };
            let handle = spawn_fake_daemon(&path, resp);
            let mut client = SyncClient::connect(&path).unwrap();
            let err = client.call(Request::Status).unwrap_err();
            handle.join().unwrap();
            match err {
                Error::DaemonError { code, .. } => assert_eq!(code, "ingress_register_failed"),
                other => panic!("unexpected error: {other}"),
            }
        }

        #[test]
        fn sync_client_connect_missing_socket_returns_not_running() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("nonexistent.sock");
            let err = SyncClient::connect(&path).unwrap_err();
            assert!(
                matches!(err, Error::NotRunning { .. }),
                "expected NotRunning, got {err}"
            );
        }
    }
}
