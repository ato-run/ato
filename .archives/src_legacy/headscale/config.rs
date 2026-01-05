use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadscaleConfig {
    /// Headscale server URL (e.g., https://headscale.gumball.local:8443)
    pub server_url: String,

    /// Pre-authentication key for automatic registration
    pub auth_key: Option<String>,

    /// Path to Tailscale state directory
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,

    /// Hostname to register with (defaults to system hostname)
    pub hostname: Option<String>,

    /// Tags to apply to this node
    #[serde(default)]
    pub tags: Vec<String>,

    /// Enable exit node functionality
    #[serde(default)]
    pub exit_node: bool,
}

fn default_state_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("/var/lib"))
        .join("gumball")
        .join("tailscale")
}

impl Default for HeadscaleConfig {
    fn default() -> Self {
        Self {
            server_url: "https://localhost:8443".to_string(),
            auth_key: None,
            state_dir: default_state_dir(),
            hostname: None,
            tags: vec!["tag:engine".to_string()],
            exit_node: false,
        }
    }
}
