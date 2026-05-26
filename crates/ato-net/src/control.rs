//! Control plane between consumers (CLI, Desktop, runtime) and the
//! `ato-netd` daemon.
//!
//! Per #296 the wire protocol lives here so that the daemon
//! (`ato-netd`) and every consumer use the same types — no client-side
//! re-implementation. The transport is newline-delimited JSON over a
//! Unix domain socket at `${ATO_HOME}/run/netd.sock`.
//!
//! Slice **A** ships the minimal verb set:
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
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Default path of the `ato-netd` control socket inside the user's
/// `ATO_HOME` tree. The same path is computed by the daemon when it
/// binds, so clients constructed via [`Client::connect_default`] and
/// daemons started without an explicit override always meet.
pub fn default_socket_path() -> Result<PathBuf, Error> {
    capsule_core::common::paths::ato_path("run/netd.sock").map_err(|err| Error::PathResolve {
        message: err.to_string(),
    })
}

/// Typed errors returned by [`Client`]. Matchable; consumers should
/// pattern-match (do not parse messages).
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("ato-netd is not running (control socket {path} unreachable: {source})")]
    NotRunning {
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
}

/// Description of a listener owned by the running daemon. Slice **A**
/// never produces any (no listeners exist yet); the type is shipped now
/// so the wire format does not need a breaking change when **B** adds
/// the first listener.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListenerInfo {
    /// Stable short name (e.g. `"ingress"`, `"egress-connect"`).
    pub name: String,
    /// `host:port` or path form, suitable for human display only.
    pub address: String,
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
}

/// Tagged-enum response envelope mirroring [`Request`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Ok { result: ResponseResult },
    Error { error: ErrorPayload },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseResult {
    Status(StatusReport),
    /// Empty `{}` body, used for verbs whose only useful signal is
    /// "succeeded" (e.g. `Shutdown`).
    Empty {},
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
pub struct Client {
    /// Path the client connected to. Recorded so error messages can
    /// surface the path the consumer actually used.
    socket_path: PathBuf,
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
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
    /// [`Error::NotRunning`] when the socket is absent or refuses
    /// connections — consumers can branch on that variant to print
    /// `{"status":"not_running"}` without needing to inspect kernel
    /// error codes.
    pub async fn connect(socket_path: &Path) -> Result<Self, Error> {
        let stream = UnixStream::connect(socket_path).await.map_err(|err| {
            // ENOENT, ECONNREFUSED, and "permission denied" all map to
            // "daemon not running from this consumer's perspective":
            // the consumer cannot reach a daemon at the canonical path,
            // and that is exactly the user-facing meaning of
            // not_running. Other I/O errors (EMFILE etc.) are passed
            // through as the generic Io variant by the std::io::Error
            // construction below — but `connect()` failure on UDS is
            // overwhelmingly one of the not-running shapes.
            match err.kind() {
                std::io::ErrorKind::NotFound
                | std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::PermissionDenied => Error::NotRunning {
                    path: socket_path.to_path_buf(),
                    source: err,
                },
                _ => Error::Io(err),
            }
        })?;
        let (read_half, writer) = stream.into_split();
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
            ResponseResult::Empty {} => Err(Error::DaemonError {
                code: "unexpected_empty_response".into(),
                message: "daemon returned empty result for status".into(),
            }),
        }
    }

    /// Send a `Shutdown` request. Consumes the client because the
    /// connection is expected to close shortly after the ack.
    pub async fn shutdown(mut self) -> Result<(), Error> {
        let _ = self.call(Request::Shutdown).await?;
        Ok(())
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
}
