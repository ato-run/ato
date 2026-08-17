//! Control-plane wire types between consumers (CLI, Desktop, runtime) and the
//! `ato-netd` daemon.
//!
//! Per #296 the wire protocol lives here so that the daemon (`ato-netd`) and
//! every consumer use the same types — no client-side re-implementation. The
//! typed transport client (`Client` / `SyncClient`) is not part of this wire
//! surface: it pulls in a Tokio runtime and lives next to each caller that
//! needs it (`ato-netd`, `ato-cli`, `ato-desktop`).
//!
//! ## Transport
//!
//! Newline-delimited JSON over a local control transport. Unix uses the
//! canonical `${ATO_HOME}/run/netd.sock` Unix-domain socket; Windows uses the
//! well-known `\\.\pipe\ato-netd-control` named pipe. The wire types
//! ([`Request`], [`Response`], [`StatusReport`], [`Error`], etc.) stay shared
//! across both transports.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const ENV_ATO_HOME: &str = "ATO_HOME";

/// Resolve the canonical `ATO_HOME` root.
///
/// Mirrors the path resolution used by every other ato binary: honour the
/// `ATO_HOME` environment variable (absolutizing a relative value against the
/// current working directory), otherwise fall back to `~/.ato`.
///
/// This is intentionally dependency-light (only `dirs`) so the wire crate
/// stays free of the runtime / CLI dependency graph.
pub fn ato_home_dir() -> Result<PathBuf, Error> {
    if let Some(path) = std::env::var_os(ENV_ATO_HOME) {
        let path = PathBuf::from(path);
        if !path.as_os_str().is_empty() {
            if path.is_absolute() {
                return Ok(path);
            }
            let cwd = std::env::current_dir().map_err(|err| Error::PathResolve {
                message: format!(
                    "failed to resolve relative ATO_HOME against current directory: {err}"
                ),
            })?;
            return Ok(cwd.join(path));
        }
    }

    let home = dirs::home_dir().ok_or_else(|| Error::PathResolve {
        message: "failed to determine home directory".to_string(),
    })?;
    Ok(home.join(".ato"))
}

/// Default path of the `ato-netd` control endpoint. On Unix this is the
/// canonical socket path inside `ATO_HOME`; on Windows this is the
/// well-known local named-pipe path used by `ato-netd`.
#[cfg(unix)]
pub fn default_socket_path() -> Result<PathBuf, Error> {
    Ok(ato_home_dir()?.join("run/netd.sock"))
}

#[cfg(windows)]
pub fn default_socket_path() -> Result<PathBuf, Error> {
    Ok(PathBuf::from(r"\\.\pipe\ato-netd-control"))
}

/// Typed errors returned by the control client. Matchable; consumers should
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
}
