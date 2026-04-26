use std::time::Duration;

use async_trait::async_trait;
use tokio::time::{sleep, Instant};

use crate::error::{CapsuleError, Result};

#[derive(Debug, Clone)]
pub struct TsnetConfig {
    pub control_url: String,
    pub auth_key: String,
    pub hostname: String,
    pub socks_port: u16,
    pub allow_net: Vec<String>,
    pub endpoint: TsnetEndpoint,
}

#[derive(Debug, Clone)]
pub enum TsnetEndpoint {
    #[cfg(unix)]
    Uds(std::path::PathBuf),
    #[cfg(windows)]
    NamedPipe(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TsnetState {
    Stopped,
    Starting,
    Ready,
    Failed,
}

pub use client::TsnetClient;
pub use sidecar::{
    discover_sidecar, spawn_sidecar, SidecarBaseConfig, SidecarRequest, SidecarSpawnConfig,
};

#[derive(Debug, Clone)]
pub struct TsnetStatus {
    pub state: TsnetState,
    pub socks_port: Option<u16>,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TsnetServeStatus {
    pub running: bool,
    pub listen_port: Option<u16>,
    pub listen_addr: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TsnetWaitConfig {
    pub poll_interval: Duration,
    pub timeout: Duration,
}

impl TsnetWaitConfig {
    pub fn new(poll_interval: Duration, timeout: Duration) -> Self {
        Self {
            poll_interval,
            timeout,
        }
    }
}

#[async_trait]
pub trait TsnetHandle: Send + Sync {
    async fn start(&self, config: TsnetConfig) -> Result<TsnetStatus>;
    async fn stop(&self) -> Result<()>;
    async fn status(&self) -> Result<TsnetStatus>;
}

pub async fn wait_for_ready<H>(handle: &H, config: TsnetWaitConfig) -> Result<TsnetStatus>
where
    H: TsnetHandle + ?Sized,
{
    let started = Instant::now();
    loop {
        let status = handle.status().await?;
        match status.state {
            TsnetState::Ready => return Ok(status),
            TsnetState::Failed => {
                let message = status
                    .message
                    .unwrap_or_else(|| "sidecar reported failed state".to_string());
                return Err(CapsuleError::SidecarResponse(message));
            }
            TsnetState::Stopped | TsnetState::Starting => {}
        }

        if started.elapsed() >= config.timeout {
            return Err(CapsuleError::Timeout);
        }

        sleep(config.poll_interval).await;
    }
}

pub mod client;
#[cfg(test)]
pub mod integration_test;
pub mod ipc;
pub mod sidecar;

#[allow(clippy::all)]
pub mod proto {
    include!("tsnet.v1.rs");
}
