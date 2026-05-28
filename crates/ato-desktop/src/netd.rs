//! `ato-netd` lifecycle management and ingress registration for ato-desktop.
//!
//! Slice C of #294 / #298. Desktop calls [`register_stable_ingress`] instead of
//! the old `handle_stable_origin_proxy_request` path in `webview.rs`.
//!
//! # Control-plane contract
//! Desktop only talks to `ato-netd` through [`ato_net::control::SyncClient`].
//! The wire protocol (newline-delimited JSON over a Unix domain socket) is an
//! implementation detail of `ato-net` / `ato-netd` and must not be re-implemented
//! here.
//!
//! # Binary resolution order
//! Mirrors `orchestrator::resolve_ato_binary` for the `ato-netd` binary:
//!   1. `ATO_DESKTOP_NETD_BIN` env override
//!   2. `{exe_dir}/../Helpers/ato-netd`  (macOS app bundle)
//!   3. Monorepo dev target via `ATO_DESKTOP_DEV_HELPER_TARGET`
//!   4. `PATH` lookup
//!
//! # Platform note
//! `ato-netd` is Unix-only in slices A-C. On non-Unix hosts
//! [`register_stable_ingress`] returns [`IngressError::NotSupported`] and
//! callers fall back to the direct `local_url`.

use std::path::PathBuf;
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use ato_net::control::SyncClient;

use crate::state::GuestRoute;

/// Env-var override for the `ato-netd` binary path. Mirrors
/// `ATO_DESKTOP_ATO_BIN` from `orchestrator.rs`.
const NETD_BIN_ENV: &str = "ATO_DESKTOP_NETD_BIN";

// ---------------------------------------------------------------------------
// Public error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub(crate) enum IngressError {
    #[error(
        "ato-netd ingress port for key \"{key}\" is already claimed by another session (port {port}). \
         Stop that session before restarting this one."
    )]
    PersistedPortTaken { key: String, port: u16 },

    #[error("ato-netd binary was not found; install ato-netd or set {NETD_BIN_ENV}")]
    BinaryNotFound,

    #[error("failed to spawn ato-netd: {0}")]
    SpawnFailed(#[source] std::io::Error),

    #[error("timed out waiting for ato-netd to become ready")]
    Timeout,

    /// Generic control-plane error (not PersistedPortTaken).
    #[error("ato-netd control error: {0}")]
    Control(#[from] ato_net::control::Error),

    /// Platform does not support ato-netd (non-Unix in slices A-C).
    #[error("ato-netd ingress is not supported on this platform in the current release")]
    NotSupported,
}

// ---------------------------------------------------------------------------
// Route key derivation (moved from stable_origin_proxy.rs)
// ---------------------------------------------------------------------------

/// Derive the stable ingress key for a `GuestRoute`.
///
/// Returns `Some(key)` only for routes with a real capsule identity that
/// can be consistently re-derived across sessions:
/// - [`GuestRoute::CapsuleHandle`] → `handle:<handle>`
/// - [`GuestRoute::LocalManifest`] → `handle:<source_handle>`
/// - [`GuestRoute::Capsule`] → `session:<session_id>`
///
/// Returns `None` for:
/// - [`GuestRoute::CapsuleUrl`] — arbitrary URL, no stable identity
/// - [`GuestRoute::ExternalUrl`] — external site, no ingress needed
/// - [`GuestRoute::Terminal`] — terminal pane, not a web capsule
pub(crate) fn logical_key_for_route(route: &GuestRoute) -> Option<String> {
    match route {
        GuestRoute::CapsuleHandle { handle, .. } => {
            Some(ato_net::stable_origin::logical_key_for_handle(handle))
        }
        GuestRoute::LocalManifest(local) => {
            Some(ato_net::stable_origin::logical_key_for_handle(
                &local.source_handle,
            ))
        }
        GuestRoute::Capsule { session, .. } => {
            Some(ato_net::stable_origin::logical_key_for_session(session))
        }
        GuestRoute::CapsuleUrl { .. }
        | GuestRoute::ExternalUrl(_)
        | GuestRoute::Terminal { .. } => None,
    }
}

// ---------------------------------------------------------------------------
// Public registration API
// ---------------------------------------------------------------------------

/// Normalize an upstream URL so that wildcard bind addresses (`0.0.0.0` and
/// `[::]`) are replaced with the loopback address `127.0.0.1`.
///
/// Reverse proxies and plain TCP clients cannot connect to wildcard addresses.
/// If the upstream was started with `0.0.0.0:<port>` (e.g. a Docker container
/// that binds all interfaces), the ingress proxy must reach it via a concrete
/// address.  We canonicalize to `127.0.0.1` because the upstream is always
/// a local process.
pub(crate) fn normalize_upstream_url(url: &str) -> std::borrow::Cow<'_, str> {
    if url.contains("//0.0.0.0:") || url.contains("//[::]:") || url.contains("//[::]") {
        let normalized = url
            .replace("//0.0.0.0:", "//127.0.0.1:")
            .replace("//[::]:", "//127.0.0.1:")
            .replace("//[::]/", "//127.0.0.1/");
        std::borrow::Cow::Owned(normalized)
    } else {
        std::borrow::Cow::Borrowed(url)
    }
}

/// Register a stable ingress route with `ato-netd` and return the allocated
/// port. If `ato-netd` is not running, it will be spawned automatically.
///
/// The same `key` always returns the same `stable_port` across restarts
/// (persisted in `${ATO_HOME}/state/netd/stable_origin_ports.json`).
///
/// The `upstream_url` is normalized before registration: wildcard bind
/// addresses (`0.0.0.0`, `[::]`) are replaced with `127.0.0.1` so the
/// ato-netd proxy can actually connect to the upstream.
///
/// # Errors
/// - [`IngressError::PersistedPortTaken`] — the stable port is held by
///   another process. The caller **must not** silently rebind.
/// - Other variants for binary-not-found, spawn failures, and timeouts.
#[cfg(unix)]
pub(crate) fn register_stable_ingress(key: &str, upstream_url: &str) -> Result<u16, IngressError> {
    let normalized = normalize_upstream_url(upstream_url);
    tracing::info!(
        key = %key,
        upstream_url = %normalized,
        "registering ato-netd stable ingress"
    );
    let mut client = ensure_netd_connected()?;
    match client.register_ingress(key, &normalized) {
        Ok(info) => Ok(info.port),
        Err(ato_net::control::Error::DaemonError { code, message }) => {
            Err(map_daemon_error(key, &code, &message))
        }
        Err(other) => Err(IngressError::Control(other)),
    }
}

#[cfg(not(unix))]
pub(crate) fn register_stable_ingress(
    _key: &str,
    _upstream_url: &str,
) -> Result<u16, IngressError> {
    Err(IngressError::NotSupported)
}

/// Deregister a stable ingress route. Best-effort: logs a warning on failure
/// but never panics. Safe to call if the daemon is already stopped.
pub(crate) fn deregister_stable_ingress(key: &str) {
    #[cfg(unix)]
    {
        match SyncClient::connect_default() {
            Ok(mut client) => match client.deregister_ingress(key) {
                Ok(()) => tracing::debug!(key = %key, "deregistered ato-netd ingress route"),
                Err(err) => tracing::warn!(
                    key = %key,
                    error = %err,
                    "failed to deregister ato-netd ingress route (best-effort)"
                ),
            },
            Err(ato_net::control::Error::NotRunning { .. }) => {
                // Daemon already stopped — nothing to deregister.
            }
            Err(err) => tracing::warn!(
                key = %key,
                error = %err,
                "failed to connect to ato-netd for deregistration (best-effort)"
            ),
        }
    }
    #[cfg(not(unix))]
    let _ = key;
}

// ---------------------------------------------------------------------------
// Internal: connect or spawn + retry
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn ensure_netd_connected() -> Result<SyncClient, IngressError> {
    // Fast path: daemon already running.
    match SyncClient::connect_default() {
        Ok(client) => return Ok(client),
        Err(ato_net::control::Error::NotRunning { .. }) => {}
        Err(ato_net::control::Error::PermissionDenied { path, .. }) => {
            return Err(IngressError::Control(
                ato_net::control::Error::PermissionDenied {
                    path,
                    source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
                },
            ));
        }
        Err(other) => return Err(IngressError::Control(other)),
    }

    // Daemon not running — spawn it.
    let netd_bin = resolve_netd_binary()?;
    tracing::info!(
        bin = %netd_bin.display(),
        "spawning ato-netd"
    );
    std::process::Command::new(&netd_bin)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(IngressError::SpawnFailed)?;

    // Retry until the socket appears (up to ~2 s total).
    const RETRIES: u32 = 20;
    for i in 0..RETRIES {
        let delay_ms = 50 + 25 * i;
        thread::sleep(Duration::from_millis(u64::from(delay_ms)));
        match SyncClient::connect_default() {
            Ok(client) => {
                tracing::info!("ato-netd is ready (attempt {})", i + 1);
                return Ok(client);
            }
            Err(ato_net::control::Error::NotRunning { .. }) => continue,
            Err(other) => return Err(IngressError::Control(other)),
        }
    }
    Err(IngressError::Timeout)
}

// ---------------------------------------------------------------------------
// Internal: binary resolution
// ---------------------------------------------------------------------------

#[cfg(unix)]
pub(crate) fn resolve_netd_binary() -> Result<PathBuf, IngressError> {
    // 1. Explicit env override.
    if let Some(path) = std::env::var_os(NETD_BIN_ENV) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        // Override set but missing — surface as BinaryNotFound so caller
        // can show a clear message rather than silently falling through.
        return Err(IngressError::BinaryNotFound);
    }

    // 2. macOS app bundle: `{exe}/../Helpers/ato-netd`.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(macos_dir) = exe.parent() {
            let bundled = macos_dir
                .parent()
                .map(|contents| contents.join("Helpers").join("ato-netd"));
            if let Some(path) = bundled {
                if path.is_file() {
                    return Ok(path);
                }
            }
        }
    }

    // 3. Monorepo dev build: `{ATO_DESKTOP_DEV_HELPER_TARGET}/{profile}/ato-netd`.
    if let Some(path) = dev_workspace_netd_binary() {
        return Ok(path);
    }

    // 4. PATH lookup.
    if let Some(path) = which_in_path("ato-netd") {
        return Ok(path);
    }

    Err(IngressError::BinaryNotFound)
}

#[cfg(unix)]
fn dev_workspace_netd_binary() -> Option<PathBuf> {
    let target_root = option_env!("ATO_DESKTOP_DEV_HELPER_TARGET")?;
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let candidate = PathBuf::from(target_root).join(profile).join("ato-netd");
    candidate.is_file().then_some(candidate)
}

fn which_in_path(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|entry| entry.join(binary))
        .find(|candidate| candidate.is_file())
}

// ---------------------------------------------------------------------------
// Internal: error classification
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn map_daemon_error(key: &str, code: &str, message: &str) -> IngressError {
    if code == "ingress_register_failed" && message.contains("already claimed") {
        // Extract port from: "port allocator error: port N is already claimed by..."
        // Use windows(2) to find "port" followed by a parseable u16 number.
        let words: Vec<&str> = message.split_whitespace().collect();
        let port = words
            .windows(2)
            .find_map(|pair| {
                if pair[0] == "port" {
                    pair[1].parse::<u16>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0);
        return IngressError::PersistedPortTaken {
            key: key.to_string(),
            port,
        };
    }
    IngressError::Control(ato_net::control::Error::DaemonError {
        code: code.to_string(),
        message: message.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_key_capsule_handle() {
        let route = GuestRoute::CapsuleHandle {
            handle: "capsule://org/demo@1.0.0".to_string(),
            label: "demo".to_string(),
        };
        assert_eq!(
            logical_key_for_route(&route),
            Some("handle:capsule://org/demo@1.0.0".to_string())
        );
    }

    #[test]
    fn logical_key_local_manifest() {
        let local = crate::state::LocalManifestRoute {
            manifest_path: std::path::PathBuf::from("/path/to/capsule.toml"),
            source_handle: "capsule://org/demo@1.0.0".to_string(),
            label: "demo".to_string(),
            requested_ref: "main".to_string(),
            resolved_commit: "abc123".to_string(),
            manifest_source: crate::state::ManifestSource::Repo,
            manifest_hash: "def456".to_string(),
            draft_id: "".to_string(),
        };
        let route = GuestRoute::LocalManifest(local);
        assert_eq!(
            logical_key_for_route(&route),
            Some("handle:capsule://org/demo@1.0.0".to_string())
        );
    }

    #[test]
    fn logical_key_capsule_session() {
        let route = GuestRoute::Capsule {
            session: "session-abc-123".to_string(),
            entry_path: "/index.html".to_string(),
        };
        assert_eq!(
            logical_key_for_route(&route),
            Some("session:session-abc-123".to_string())
        );
    }

    #[test]
    fn logical_key_none_for_capsule_url() {
        let route = GuestRoute::CapsuleUrl {
            handle: "capsule://org/demo@1.0.0".to_string(),
            label: "demo".to_string(),
            url: url::Url::parse("http://127.0.0.1:3000").expect("url"),
        };
        assert_eq!(logical_key_for_route(&route), None);
    }

    #[test]
    fn logical_key_none_for_external_url() {
        let route =
            GuestRoute::ExternalUrl(url::Url::parse("https://example.com").expect("url"));
        assert_eq!(logical_key_for_route(&route), None);
    }

    #[test]
    fn logical_key_none_for_terminal() {
        let route = GuestRoute::Terminal {
            session_id: "term-1".to_string(),
        };
        assert_eq!(logical_key_for_route(&route), None);
    }

    #[test]
    fn handle_and_session_keys_do_not_collide_for_same_suffix() {
        let handle = "capsule://org/demo@1.0.0";
        let handle_route = GuestRoute::CapsuleHandle {
            handle: handle.to_string(),
            label: "demo".to_string(),
        };
        let session_route = GuestRoute::Capsule {
            session: handle.to_string(),
            entry_path: "/".to_string(),
        };
        assert_ne!(
            logical_key_for_route(&handle_route),
            logical_key_for_route(&session_route),
            "handle: and session: prefixes must not collide"
        );
    }

    #[cfg(unix)]
    #[test]
    fn map_daemon_error_detects_persisted_port_taken() {
        let err = map_daemon_error(
            "handle:test/demo@1.0.0",
            "ingress_register_failed",
            "port allocator error: port 19001 is already claimed by key \"other:key\" in the persisted allocation table",
        );
        match err {
            IngressError::PersistedPortTaken { key, port } => {
                assert_eq!(key, "handle:test/demo@1.0.0");
                assert_eq!(port, 19001);
            }
            other => panic!("expected PersistedPortTaken, got: {other}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn map_daemon_error_passes_through_other_errors() {
        let err = map_daemon_error(
            "handle:test/demo@1.0.0",
            "ingress_register_failed",
            "could not bind 127.0.0.1:19002: address already in use",
        );
        assert!(
            matches!(err, IngressError::Control(ato_net::control::Error::DaemonError { .. })),
            "non-claimed error should pass through as Control"
        );
    }

    #[test]
    fn normalize_upstream_url_wildcard_ipv4() {
        assert_eq!(
            normalize_upstream_url("http://0.0.0.0:8080/"),
            "http://127.0.0.1:8080/"
        );
    }

    #[test]
    fn normalize_upstream_url_wildcard_ipv6() {
        assert_eq!(
            normalize_upstream_url("http://[::]:8080/"),
            "http://127.0.0.1:8080/"
        );
    }

    #[test]
    fn normalize_upstream_url_loopback_unchanged() {
        let url = "http://127.0.0.1:8080/foo/bar?q=1";
        assert_eq!(normalize_upstream_url(url), url);
    }

    #[test]
    fn normalize_upstream_url_localhost_unchanged() {
        let url = "http://localhost:3000/";
        assert_eq!(normalize_upstream_url(url), url);
    }

    #[test]
    fn normalize_upstream_url_preserves_path_and_query() {
        assert_eq!(
            normalize_upstream_url("http://0.0.0.0:9000/app/ui?debug=1"),
            "http://127.0.0.1:9000/app/ui?debug=1"
        );
    }
}
