use std::process::Stdio;
use tokio::process::Command;
use tracing::{info, warn};

use super::HeadscaleConfig;

#[derive(Debug, thiserror::Error)]
pub enum HeadscaleError {
    #[error("Tailscale not installed")]
    NotInstalled,

    #[error("Failed to connect: {0}")]
    ConnectionFailed(String),

    #[error("Command execution failed: {0}")]
    CommandFailed(#[from] std::io::Error),
}

pub struct HeadscaleClient {
    config: HeadscaleConfig,
}

impl HeadscaleClient {
    pub fn new(config: HeadscaleConfig) -> Self {
        Self { config }
    }

    /// Check if Tailscale CLI is available
    pub async fn check_installation(&self) -> Result<(), HeadscaleError> {
        let output = Command::new("tailscale").arg("version").output().await?;

        if !output.status.success() {
            return Err(HeadscaleError::NotInstalled);
        }

        let version = String::from_utf8_lossy(&output.stdout);
        info!("Tailscale version: {}", version.trim());
        Ok(())
    }

    /// Connect to Headscale server
    pub async fn connect(&self) -> Result<TailnetInfo, HeadscaleError> {
        self.check_installation().await?;

        let mut cmd = Command::new("tailscale");
        cmd.arg("up")
            .arg("--login-server")
            .arg(&self.config.server_url)
            .arg("--hostname")
            .arg(self.get_hostname());

        // Add auth key if provided
        if let Some(ref key) = self.config.auth_key {
            cmd.arg("--authkey").arg(key);
        }

        // Add tags
        if !self.config.tags.is_empty() {
            cmd.arg("--advertise-tags").arg(self.config.tags.join(","));
        }

        info!("Connecting to Headscale at {}", self.config.server_url);

        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(HeadscaleError::ConnectionFailed(stderr.to_string()));
        }

        // Get assigned IP
        self.get_status().await
    }

    /// Get current Tailscale status
    pub async fn get_status(&self) -> Result<TailnetInfo, HeadscaleError> {
        let output = Command::new("tailscale")
            .arg("status")
            .arg("--json")
            .output()
            .await?;

        if !output.status.success() {
            return Err(HeadscaleError::CommandFailed(std::io::Error::other(
                "status command failed",
            )));
        }

        let status: TailscaleStatus = serde_json::from_slice(&output.stdout).map_err(|e| {
            HeadscaleError::CommandFailed(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
        })?;

        Ok(TailnetInfo {
            ip: status
                .self_node
                .tailscale_ips
                .first()
                .cloned()
                .unwrap_or_default(),
            hostname: status.self_node.host_name,
            online: status.self_node.online,
            peers: status
                .peer
                .into_values()
                .map(|p| PeerInfo {
                    ip: p.tailscale_ips.first().cloned().unwrap_or_default(),
                    hostname: p.host_name,
                    online: p.online,
                    tags: p.tags,
                })
                .collect(),
        })
    }

    /// Disconnect from Tailnet
    #[allow(dead_code)]
    pub async fn disconnect(&self) -> Result<(), HeadscaleError> {
        let output = Command::new("tailscale").arg("down").output().await?;

        if !output.status.success() {
            warn!("Failed to disconnect cleanly");
        }

        Ok(())
    }

    fn get_hostname(&self) -> String {
        self.config.hostname.clone().unwrap_or_else(|| {
            hostname::get()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_else(|_| "gumball-engine".to_string())
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TailnetInfo {
    pub ip: String,
    pub hostname: String,
    pub online: bool,
    pub peers: Vec<PeerInfo>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PeerInfo {
    pub ip: String,
    pub hostname: String,
    pub online: bool,
    pub tags: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct TailscaleStatus {
    #[serde(rename = "Self")]
    self_node: TailscaleNode,
    #[serde(default)]
    peer: std::collections::HashMap<String, TailscaleNode>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
struct TailscaleNode {
    host_name: String,
    #[serde(rename = "TailscaleIPs")]
    tailscale_ips: Vec<String>,
    online: bool,
    #[serde(default)]
    tags: Vec<String>,
}
