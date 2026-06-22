//! `ato-netd` lifecycle management for the CLI.
//!
//! This module mirrors `ato-desktop/src/netd.rs` for the CLI path. It
//! provides:
//!
//! - [`ensure_egress_proxy`]: start (or reuse) `ato-netd` and return the
//!   ephemeral egress proxy port.
//! - [`try_shutdown_if_last_session`]: best-effort daemon shutdown when the
//!   last tracked session is removed.
//!
//! Like the Desktop variant, the entire implementation is `#[cfg(unix)]`
//! because `ato-netd` uses Unix domain sockets for control. Non-Unix builds
//! compile cleanly but `ensure_egress_proxy` returns
//! [`EgressProxyError::NotSupported`].

use std::path::PathBuf;

#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use crate::net_client::SyncClient;

/// Environment variable that overrides the `ato-netd` binary path.
const NETD_BIN_ENV: &str = "ATO_NETD_BIN";

// ---------------------------------------------------------------------------
// Public error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)]
pub enum EgressProxyError {
    #[error("ato-netd binary was not found; install ato-netd or set {NETD_BIN_ENV}")]
    BinaryNotFound,

    #[error("failed to spawn ato-netd: {0}")]
    SpawnFailed(#[source] std::io::Error),

    #[error("timed out waiting for ato-netd to become ready")]
    Timeout,

    #[error("ato-netd did not report an egress proxy port")]
    EgressPortNotAvailable,

    #[error("ato-netd control error: {0}")]
    Control(#[from] crate::net_client::Error),

    #[error("ato-netd egress proxy is not supported on this platform")]
    NotSupported,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Ensure `ato-netd` is running for the current `ATO_HOME` and return the
/// ephemeral port of its HTTP CONNECT egress proxy.
///
/// Fast path: if `ato-netd` is already running, connect and read
/// `StatusReport.egress_proxy_port`.
///
/// Slow path: spawn the `ato-netd` binary and retry until the socket
/// appears (up to ~2 s total).
///
/// Returns [`EgressProxyError::NotSupported`] on non-Unix platforms.
#[cfg(unix)]
pub fn ensure_egress_proxy() -> Result<u16, EgressProxyError> {
    let mut client = ensure_netd_connected()?;
    let status = client.status()?;
    status
        .egress_proxy_port
        .ok_or(EgressProxyError::EgressPortNotAvailable)
}

#[cfg(not(unix))]
pub fn ensure_egress_proxy() -> Result<u16, EgressProxyError> {
    Err(EgressProxyError::NotSupported)
}

/// Best-effort shutdown: stop `ato-netd` if there are no remaining active
/// sessions in `${ATO_HOME}/apps/ato-desktop/sessions/`.
///
/// Called from `stop_session` after the session record is removed. All errors
/// are silently discarded — a lingering daemon is always preferable to a
/// panicking or error-propagating cleanup path.
pub fn try_shutdown_if_last_session() {
    #[cfg(unix)]
    {
        let remaining = match capsule::state::session::store::session_root()
            .ok()
            .and_then(|root| capsule::state::session::store::read_session_records(&root).ok())
        {
            Some(records) => records.len(),
            None => return, // can't determine count, leave daemon running
        };
        if remaining == 0 {
            tracing::debug!("last session removed — shutting down ato-netd");
            if let Ok(client) = SyncClient::connect_default() {
                let _ = client.shutdown();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers (Unix only)
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn ensure_netd_connected() -> Result<SyncClient, EgressProxyError> {
    // Fast path: daemon already running.
    match SyncClient::connect_default() {
        Ok(client) => return Ok(client),
        Err(crate::net_client::Error::NotRunning { .. }) => {}
        Err(crate::net_client::Error::PermissionDenied { path, .. }) => {
            return Err(EgressProxyError::Control(
                crate::net_client::Error::PermissionDenied {
                    path,
                    source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
                },
            ));
        }
        Err(other) => return Err(EgressProxyError::Control(other)),
    }

    // Slow path: spawn the daemon then retry until the socket appears.
    let netd_bin = resolve_netd_binary()?;
    tracing::info!(bin = %netd_bin.display(), "spawning ato-netd");
    std::process::Command::new(&netd_bin)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(EgressProxyError::SpawnFailed)?;

    // Retry up to ~2 s (20 attempts, backoff from 50 ms to 525 ms).
    const RETRIES: u32 = 20;
    for i in 0..RETRIES {
        let delay_ms = 50 + 25 * i;
        thread::sleep(Duration::from_millis(u64::from(delay_ms)));
        match SyncClient::connect_default() {
            Ok(client) => {
                tracing::info!("ato-netd is ready (attempt {})", i + 1);
                return Ok(client);
            }
            Err(crate::net_client::Error::NotRunning { .. }) => continue,
            Err(other) => return Err(EgressProxyError::Control(other)),
        }
    }
    Err(EgressProxyError::Timeout)
}

/// Resolve the path to the `ato-netd` binary.
///
/// Resolution order:
/// 1. `ATO_NETD_BIN` env override
/// 2. `{exe_dir}/ato-netd` (same directory as the running `ato` binary)
/// 3. `PATH` lookup
#[cfg(unix)]
fn resolve_netd_binary() -> Result<PathBuf, EgressProxyError> {
    // 1. Explicit env override.
    if let Some(path) = std::env::var_os(NETD_BIN_ENV) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(EgressProxyError::BinaryNotFound);
    }

    // 2. Same directory as the current executable.
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join("ato-netd");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    // 3. PATH lookup.
    if let Some(path) = which_in_path("ato-netd") {
        return Ok(path);
    }

    Err(EgressProxyError::BinaryNotFound)
}

fn which_in_path(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|entry| entry.join(binary))
        .find(|candidate| candidate.is_file())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(unix))]
    fn non_unix_returns_not_supported() {
        let result = ensure_egress_proxy();
        assert!(
            matches!(result, Err(EgressProxyError::NotSupported)),
            "expected NotSupported on non-Unix, got {result:?}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn missing_binary_returns_binary_not_found() {
        // Point ATO_NETD_BIN at a path that doesn't exist so resolution
        // fails predictably without relying on PATH state.
        let absent = "/nonexistent/path/to/ato-netd-slice-f-test";
        unsafe {
            std::env::set_var(NETD_BIN_ENV, absent);
        }
        let result = resolve_netd_binary();
        unsafe {
            std::env::remove_var(NETD_BIN_ENV);
        }
        assert!(
            matches!(result, Err(EgressProxyError::BinaryNotFound)),
            "expected BinaryNotFound, got {result:?}"
        );
    }
}
