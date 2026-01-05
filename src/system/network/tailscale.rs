use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

/// Manages the tailscaled child process and VPN connection
pub struct TailscaleManager {
    process_running: Arc<AtomicBool>,
    vpn_ip: Arc<Mutex<Option<String>>>,
    _monitor_task: Option<JoinHandle<()>>,
}

impl TailscaleManager {
    /// Start tailscaled and attempt to join the network
    pub fn start(
        headscale_url: Option<String>,
        auth_key: Option<String>,
        state_dir: Option<String>,
    ) -> Self {
        let process_running = Arc::new(AtomicBool::new(true));
        let vpn_ip = Arc::new(Mutex::new(None));

        // 1. Start tailscaled process (if not already running as system service)
        // Note: For this implementation, we assume we might need to start it or just control it via CLI.
        // If we are running in a container, we likely need to start `tailscaled`.
        // If running on host, it might already be there.
        // For "Soft-Spatial OS", we want to be self-contained, so we try to start it if we have config.

        let _state_dir = state_dir.unwrap_or_else(|| "/var/lib/tailscale".to_string());

        // Spawn background task to manage connection
        let running_clone = process_running.clone();
        let vpn_ip_clone = vpn_ip.clone();
        let headscale_url = headscale_url.clone();
        let auth_key = auth_key.clone();

        let monitor_task = tokio::spawn(async move {
            info!("Starting Tailscale manager loop...");

            // Ensure tailscaled is up (simplified: we assume `tailscaled` binary is in PATH)
            // In a real implementation, we would spawn `tailscaled` as a child process here and keep it alive.
            // For now, we'll assume we control it via `tailscale up`.

            // Wait a bit for process to stabilize if we were starting it
            tokio::time::sleep(Duration::from_secs(2)).await;

            // Attempt to join if auth key provided
            if let (Some(url), Some(key)) = (headscale_url, auth_key) {
                info!("Attempting to join Headscale at {}", url);
                let status = Command::new("tailscale")
                    .arg("up")
                    .arg("--login-server")
                    .arg(&url)
                    .arg("--authkey")
                    .arg(&key)
                    .arg("--accept-routes")
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .status();

                match status {
                    Ok(s) if s.success() => info!("Successfully executed 'tailscale up'"),
                    Ok(s) => warn!("'tailscale up' failed with exit code: {:?}", s.code()),
                    Err(_e) => error!("Failed to execute 'tailscale up': {}", _e),
                }
            }

            // Monitoring loop for IP address
            while running_clone.load(Ordering::Relaxed) {
                match get_tailscale_ip() {
                    Ok(ip) => {
                        let mut lock = vpn_ip_clone.lock().unwrap();
                        if lock.as_ref() != Some(&ip) {
                            info!("Tailscale VPN IP assigned: {}", ip);
                            *lock = Some(ip);
                        }
                    }
                    Err(_e) => {
                        // Only log error occasionally to avoid spam
                        // warn!("Failed to get Tailscale IP: {}", e);
                    }
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });

        Self {
            process_running,
            vpn_ip,
            _monitor_task: Some(monitor_task),
        }
    }

    /// Get the current VPN IP address
    pub fn get_vpn_ip(&self) -> Option<String> {
        self.vpn_ip.lock().unwrap().clone()
    }
}

impl Drop for TailscaleManager {
    fn drop(&mut self) {
        info!("Stopping Tailscale process...");
        self.process_running.store(false, Ordering::Relaxed);

        // Explicitly run tailscale down to cleanup network
        let _ = Command::new("tailscale")
            .arg("down")
            .status()
            .map_err(|e| error!("Failed to run tailscale down: {}", e));
    }
}

/// Helper to get IP from `tailscale ip -4`
fn get_tailscale_ip() -> anyhow::Result<String> {
    let output = Command::new("tailscale").arg("ip").arg("-4").output()?;

    if output.status.success() {
        let ip = String::from_utf8(output.stdout)?.trim().to_string();
        if !ip.is_empty() {
            return Ok(ip);
        }
    }

    Err(anyhow::anyhow!("No IP found"))
}
