use anyhow::{anyhow, Result};
use std::time::Duration;
use tracing::{error, info};

use crate::config::{CloudConfig, RcloneConfig};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct SkyPilotClient {
    client: Client,
    headscale_url: String,
    headscale_api_key: String,
    work_dir: PathBuf,
    rclone_config: Option<RcloneConfig>,
}

#[derive(Serialize)]
struct HeadscaleCreateKeyRequest {
    user: String,
    reusable: bool,
    ephemeral: bool,
    expiration: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    acl_tags: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct HeadscaleCreateKeyResponse {
    #[serde(rename = "preAuthKey")]
    pre_auth_key: PreAuthKey,
}

#[derive(Deserialize)]
struct PreAuthKey {
    key: String,
}

impl SkyPilotClient {
    pub fn new(config: &CloudConfig) -> Result<Self> {
        let headscale_url = std::env::var("HEADSCALE_URL")
            .unwrap_or_else(|_| "https://headscale.gumball.net".to_string());
        let headscale_api_key =
            std::env::var("HEADSCALE_API_KEY").unwrap_or_else(|_| "mock-key".to_string());

        // Use a temp dir for sky task files
        let work_dir = std::env::temp_dir().join("gumball_sky_tasks");
        std::fs::create_dir_all(&work_dir)?;

        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| anyhow!("Failed to create HTTP client: {}", e))?;

        Ok(Self {
            client,
            headscale_url,
            headscale_api_key,
            work_dir,
            rclone_config: config.rclone.clone(),
        })
    }

    pub async fn deploy(&self, manifest_str: &str) -> Result<String> {
        // 1. Generate Pre-Auth Key
        let auth_key = self.generate_pre_auth_key().await?;

        // 2. Generate task.yaml with Auto-Join
        let task_yaml = self.generate_task_yaml(manifest_str, &auth_key)?;

        // 3. Write to file
        let task_id = uuid::Uuid::new_v4().to_string();
        let file_path = self.work_dir.join(format!("{}.yaml", task_id));
        tokio::fs::write(&file_path, task_yaml).await?;

        info!("Generated SkyPilot task file: {:?}", file_path);

        // 4. Execute 'sky launch'
        // sky launch -c <cluster_name> <file> --detach
        let cluster_name = format!("gumball-{}", &task_id[..8]);

        info!("Launching SkyPilot cluster: {}", cluster_name);

        let output = Command::new("sky")
            .args([
                "launch",
                "-c",
                &cluster_name,
                file_path.to_str().unwrap(),
                "--detach",
                "-y",
            ])
            .output()
            .map_err(|e| anyhow!("Failed to execute sky launch: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("SkyPilot launch failed: {}", stderr);
            return Err(anyhow!("SkyPilot launch failed: {}", stderr));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        info!("SkyPilot launch success: {}", stdout);

        Ok(cluster_name)
    }

    async fn generate_pre_auth_key(&self) -> Result<String> {
        let url = format!("{}/api/v1/preauthkey", self.headscale_url);

        let request = HeadscaleCreateKeyRequest {
            user: "default".to_string(), // TODO: Make configurable
            reusable: false,
            ephemeral: true,
            expiration: "1h".to_string(),
            acl_tags: None,
        };

        info!("Requesting Pre-Auth Key from Headscale: {}", url);

        let response = self
            .client
            .post(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.headscale_api_key),
            )
            .json(&request)
            .send()
            .await
            .map_err(|e| anyhow!("Failed to send Headscale request: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            error!("Headscale API failed: {} - {}", status, text);
            return Err(anyhow!("Headscale API failed: {} - {}", status, text));
        }

        let key_response: HeadscaleCreateKeyResponse = response
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse Headscale response: {}", e))?;

        info!("Successfully generated Pre-Auth Key");
        Ok(key_response.pre_auth_key.key)
    }

    fn generate_task_yaml(&self, _manifest_str: &str, auth_key: &str) -> Result<String> {
        // Construct the YAML as per user request

        // Prepare Rclone Envs
        let mut envs_section = String::new();
        let mut mount_cmd = String::new();

        if let Some(rclone) = &self.rclone_config {
            envs_section = format!(
                r#"
envs:
  RCLONE_CONFIG_REMOTE_TYPE: {}
  RCLONE_CONFIG_REMOTE_PROVIDER: {}
  RCLONE_CONFIG_REMOTE_ACCESS_KEY_ID: {}
  RCLONE_CONFIG_REMOTE_SECRET_ACCESS_KEY: {}
  RCLONE_CONFIG_REMOTE_ENDPOINT: {}
"#,
                rclone.config_type,
                rclone.provider,
                rclone.access_key_id,
                rclone.secret_access_key,
                rclone.endpoint.as_deref().unwrap_or("")
            );

            mount_cmd = r#"
  echo "Mounting Rclone Remote..."
  mkdir -p /data/models
  # Mount in background with VFS cache full (Lazy Loading)
  # Using --daemon to keep it running
  rclone mount remote:/models /data/models \
    --vfs-cache-mode full \
    --daemon
            "#
            .to_string();
        }

        let yaml = format!(
            r#"
name: gumball-burst

{}

setup: |
  echo "Installing Capsuled & Tailscale..."
  curl -fsSL https://tailscale.com/install.sh | sh
  
  echo "Installing Rclone..."
  curl https://rclone.org/install.sh | sudo bash

  echo "Auto-Joining Mesh..."
  sudo tailscale up \
    --authkey={} \
    --login-server={} \
    --hostname=$(hostname) \
    --accept-routes

  echo "Starting Engine Proxy..."
  # For now, just a placeholder service or actual engine if we had the binary
  # tailscale serve --bg --https=443 localhost:4500

run: |
  {}
  
  echo "Hello from SkyPilot Cloud Node!"
  echo "Hostname: $(hostname)"
  if [ -d "/data/models" ]; then
    echo "Model mount point exists. Listing contents:"
    ls -F /data/models || echo "Empty or failed mount"
  fi
  # /usr/local/bin/capsuled-engine
"#,
            envs_section, auth_key, self.headscale_url, mount_cmd
        );

        Ok(yaml)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CloudConfig;

    #[tokio::test]
    #[ignore] // Ignored by default, run manually with mock server
    async fn test_deploy_integration() {
        let config = CloudConfig {
            enabled: true,
            api_endpoint: Some("http://localhost:8000".to_string()),
            api_key: Some("test-key".to_string()),
            rclone: None,
        };

        let client = SkyPilotClient::new(&config).unwrap();
        let result = client.deploy("test-manifest-content").await;

        match result {
            Ok(job_id) => println!("Deploy success: {}", job_id),
            Err(e) => panic!("Deploy failed: {}", e),
        }
    }
}
