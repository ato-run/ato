//! Runner-owned lifecycle shared by the existing Pixel and Terminal gateways.
//!
//! Producer startup remains in the workload's isolated supervisor. This adapter
//! owns authentication, readiness through the public path, reconnect, and idle
//! shutdown. Its ready result is transport evidence; it never satisfies Contract K.

use std::net::SocketAddr;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::pixel_gateway::{PixelGatewayConfig, PixelGatewayError, PixelGatewayHandle};
use crate::rfb_probe::{
    PixelGatewayProbeRequest, PixelGatewayReadyError, RfbFrameInfo, start_ready_pixel_gateway,
};
use crate::surface_authorization::SurfaceAccessAuthorizer;
use crate::terminal_gateway::{TerminalGatewayConfig, TerminalGatewayError, TerminalGatewayHandle};
use crate::terminal_probe::{
    TerminalGatewayProbeRequest, TerminalGatewayReadyError, start_ready_terminal_gateway,
};

pub enum NativeSurfaceConfig {
    Pixel(PixelGatewayConfig),
    Terminal(TerminalGatewayConfig),
}

/// Capabilities of the pinned profiles. File exchange and clipboard need a
/// separately authorized channel and cannot be silently enabled by an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeSurfaceCapabilities {
    pub input: bool,
    pub resize: bool,
    pub reconnect: bool,
    pub upload: bool,
    pub download: bool,
    pub clipboard: bool,
}

pub const NATIVE_SURFACE_CAPABILITIES: NativeSurfaceCapabilities = NativeSurfaceCapabilities {
    input: true,
    resize: true,
    reconnect: true,
    upload: false,
    download: false,
    clipboard: false,
};

/// An authentication assertion is consumed once, including for this readiness
/// probe. The caller issues a fresh assertion for every later client connection.
/// Deliberately no Debug implementation: assertions are credential material.
pub struct NativeSurfaceProbe<'a> {
    pub connect_url: &'a str,
    pub origin: &'a str,
    pub assertion: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeSurfaceReady {
    Pixel(RfbFrameInfo),
    Terminal,
}

#[derive(Debug, Error)]
pub enum NativeSurfaceError {
    #[error("native surface idle timeout must be between 1 second and 24 hours")]
    InvalidIdleTimeout,
    #[error(transparent)]
    PixelReady(#[from] PixelGatewayReadyError),
    #[error(transparent)]
    TerminalReady(#[from] TerminalGatewayReadyError),
    #[error(transparent)]
    PixelStop(#[from] PixelGatewayError),
    #[error(transparent)]
    TerminalStop(#[from] TerminalGatewayError),
}

enum Gateway {
    Pixel(PixelGatewayHandle),
    Terminal(TerminalGatewayHandle),
}
impl Gateway {
    fn local_addr(&self) -> SocketAddr {
        match self {
            Self::Pixel(handle) => handle.local_addr(),
            Self::Terminal(handle) => handle.local_addr(),
        }
    }
    fn last_input(&self) -> u64 {
        match self {
            Self::Pixel(handle) => handle.last_input_activity_unix_millis(),
            Self::Terminal(handle) => handle.last_input_activity_unix_millis(),
        }
    }
    async fn stop(&self) -> Result<(), NativeSurfaceError> {
        match self {
            Self::Pixel(handle) => handle.stop().await?,
            Self::Terminal(handle) => handle.stop().await?,
        }
        Ok(())
    }
}

pub struct NativeSurfaceHandle {
    gateway: Arc<Gateway>,
    monitor: tokio::task::JoinHandle<()>,
    idle_expired: Arc<AtomicBool>,
    pub ready: NativeSurfaceReady,
}
impl NativeSurfaceHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.gateway.local_addr()
    }
    pub fn last_input_activity_unix_millis(&self) -> u64 {
        self.gateway.last_input()
    }
    pub fn idle_expired(&self) -> bool {
        self.idle_expired.load(Ordering::Acquire)
    }
    /// Idempotent; closes existing viewers as well as the listening socket.
    /// The owning Runner must also stop its workload through its existing lease.
    pub async fn stop(&self) -> Result<(), NativeSurfaceError> {
        self.monitor.abort();
        self.gateway.stop().await
    }
}
impl Drop for NativeSurfaceHandle {
    fn drop(&mut self) {
        self.monitor.abort();
        // Dropping the final gateway handle sends its existing cancel signal.
    }
}

pub async fn start_ready_native_surface(
    config: NativeSurfaceConfig,
    authorizer: Arc<dyn SurfaceAccessAuthorizer>,
    probe: NativeSurfaceProbe<'_>,
    readiness_timeout: Duration,
    idle_timeout: Duration,
) -> Result<NativeSurfaceHandle, NativeSurfaceError> {
    if !(Duration::from_secs(1)..=Duration::from_secs(86_400)).contains(&idle_timeout) {
        return Err(NativeSurfaceError::InvalidIdleTimeout);
    }
    let (gateway, ready) = match config {
        NativeSurfaceConfig::Pixel(config) => {
            let (handle, frame) = start_ready_pixel_gateway(
                config,
                authorizer,
                PixelGatewayProbeRequest::new(probe.connect_url, probe.origin, probe.assertion),
                readiness_timeout,
            )
            .await?;
            (Gateway::Pixel(handle), NativeSurfaceReady::Pixel(frame))
        }
        NativeSurfaceConfig::Terminal(config) => {
            let handle = start_ready_terminal_gateway(
                config,
                authorizer,
                TerminalGatewayProbeRequest::new(probe.connect_url, probe.origin, probe.assertion),
                readiness_timeout,
            )
            .await?;
            (Gateway::Terminal(handle), NativeSurfaceReady::Terminal)
        }
    };
    let gateway = Arc::new(gateway);
    let idle_expired = Arc::new(AtomicBool::new(false));
    let monitored = Arc::clone(&gateway);
    let expired = Arc::clone(&idle_expired);
    let monitor = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |value| value.as_millis() as u64);
            if idle_due(now, monitored.last_input(), idle_timeout) {
                expired.store(true, Ordering::Release);
                if let Err(error) = monitored.stop().await {
                    tracing::warn!(%error, "native surface idle cleanup failed");
                }
                break;
            }
        }
    });
    Ok(NativeSurfaceHandle {
        gateway,
        monitor,
        idle_expired,
        ready,
    })
}

fn idle_due(now: u64, last_input: u64, timeout: Duration) -> bool {
    now.saturating_sub(last_input) >= timeout.as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn idle_policy_counts_user_input_and_tolerates_clock_rollback() {
        assert!(!idle_due(9_999, 9_000, Duration::from_secs(1)));
        assert!(idle_due(10_000, 9_000, Duration::from_secs(1)));
        assert!(!idle_due(8_000, 9_000, Duration::from_secs(1)));
    }
    #[tokio::test]
    async fn synthetic_rfb_uses_authenticated_first_frame_and_idle_closes_listener() {
        use crate::pixel_authorization::{HmacSurfaceAccessAuthorizer, SurfaceAssertionKeyring};
        use crate::surface_authorization::SurfaceGatewayScope;
        use std::collections::{BTreeMap, BTreeSet};
        let upstream = crate::rfb_probe::tests::fixture_server_with_input(vec![1], true).await;
        let reserved = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listen_addr = reserved.local_addr().unwrap();
        drop(reserved);
        let scope = SurfaceGatewayScope {
            session_id: "session-native".into(),
            surface_id: "surface-native".into(),
        };
        let keys = Arc::new(
            SurfaceAssertionKeyring::new(BTreeMap::from([("test".into(), "k".repeat(32))]))
                .unwrap(),
        );
        let assertion = keys.issue_readiness_assertion(&scope).unwrap();
        let url = format!("http://{listen_addr}/");
        let origin = "http://localhost:5173";
        let handle = start_ready_native_surface(
            NativeSurfaceConfig::Pixel(PixelGatewayConfig {
                listen_addr,
                private_rfb_addr: upstream,
                private_rfb_connect_timeout: Duration::from_secs(2),
                scope,
                allowed_origins: BTreeSet::from([origin.into()]),
            }),
            Arc::new(HmacSurfaceAccessAuthorizer::new(keys)),
            NativeSurfaceProbe {
                connect_url: &url,
                origin,
                assertion: assertion.as_str(),
            },
            Duration::from_secs(2),
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(
            handle.ready,
            NativeSurfaceReady::Pixel(RfbFrameInfo {
                width: 2,
                height: 1,
                bytes: 8
            })
        );
        assert!(!handle.idle_expired());
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(handle.idle_expired());
        assert!(tokio::net::TcpStream::connect(listen_addr).await.is_err());
        handle.stop().await.unwrap();
        handle.stop().await.unwrap();
    }
}
