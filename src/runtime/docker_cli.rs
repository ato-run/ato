//! Docker CLI Runtime for macOS Docker Desktop.
//!
//! This runtime uses the `docker` CLI directly to run containers,
//! which works on macOS where low-level OCI runtimes (runc/youki) are not available.

use std::path::PathBuf;
use std::process::Stdio;

use async_trait::async_trait;
use tokio::process::Command;
use tracing::{error, info, warn};

use crate::runtime::traits::Runtime;
use crate::runtime::{LaunchRequest, LaunchResult, RuntimeError};

/// Runtime that uses Docker CLI (`docker run`) to execute containers.
pub struct DockerCliRuntime {
    egress_proxy_port: Option<u16>,
}

impl DockerCliRuntime {
    pub fn new(egress_proxy_port: Option<u16>) -> Self {
        Self { egress_proxy_port }
    }
}

impl Default for DockerCliRuntime {
    fn default() -> Self {
        Self::new(None)
    }
}

#[async_trait]
impl Runtime for DockerCliRuntime {
    async fn launch(&self, request: LaunchRequest<'_>) -> Result<LaunchResult, RuntimeError> {
        let workload_id = request.workload_id;

        // Extract image and ports from spec/manifest.
        // Container port: where the app listens inside the container.
        // Host port: where the port is published on the host via `-p HOST:CONTAINER`.
        let mut image = String::new();
        let mut container_port: u16 = 80;
        let mut host_port: u16 = 0;
        let mut command_args: Vec<String> = Vec::new();
        let mut env_vars: Vec<(String, String)> = Vec::new();

        // Parse from spec.process
        if let Some(process) = request.spec.process() {
            // Get command/args
            if let Some(args) = process.args() {
                command_args = args.to_vec();
            }

            // Get env vars including PORT/HOST_PORT
            if let Some(env) = process.env() {
                for e in env {
                    if let Some((k, v)) = e.split_once('=') {
                        if k == "PORT" {
                            if let Ok(p) = v.parse::<u16>() {
                                container_port = p;
                                info!("[DockerCliRuntime] Got container PORT={} from OCI spec", p);
                            }
                        }

                        if k == "HOST_PORT" && host_port == 0 {
                            if let Ok(p) = v.parse::<u16>() {
                                if p > 1024 {
                                    host_port = p;
                                    info!("[DockerCliRuntime] Got HOST_PORT={} from OCI spec", p);
                                }
                            }
                        }
                        env_vars.push((k.to_string(), v.to_string()));
                    }
                }
            }
        }

        // Try to get image, port, and PORT from manifest JSON
        if let Some(manifest_json) = request.manifest_json {
            if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(manifest_json) {
                // Legacy schema: compute.image / compute.port
                if let Some(img) = manifest
                    .get("compute")
                    .and_then(|c| c.get("image"))
                    .and_then(|i| i.as_str())
                {
                    image = img.to_string();
                }

                if let Some(port) = manifest
                    .get("compute")
                    .and_then(|c| c.get("port"))
                    .and_then(|p| p.as_u64())
                {
                    container_port = port as u16;
                    info!(
                        "[DockerCliRuntime] Got container port={} from legacy manifest",
                        container_port
                    );
                }

                // Canonical schema (CapsuleManifestV1): execution.entrypoint / execution.port
                if image.is_empty() {
                    if let Some(img) = manifest
                        .get("execution")
                        .and_then(|e| e.get("entrypoint"))
                        .and_then(|i| i.as_str())
                    {
                        image = img.to_string();
                    }
                }

                if let Some(port) = manifest
                    .get("execution")
                    .and_then(|e| e.get("port"))
                    .and_then(|p| p.as_u64())
                {
                    container_port = port as u16;
                    info!(
                        "[DockerCliRuntime] Got container port={} from canonical manifest",
                        container_port
                    );
                }

                // Get container port from docker.ports array (prefer this over env PORT)
                if let Some(ports) = manifest
                    .get("docker")
                    .and_then(|d| d.get("ports"))
                    .and_then(|p| p.as_array())
                {
                    if let Some(first_port) = ports.first() {
                        if let Some(cp) = first_port.get("containerPort").and_then(|v| v.as_u64()) {
                            container_port = cp as u16;
                            info!(
                                "[DockerCliRuntime] Got container port={} from docker.ports",
                                container_port
                            );
                        }
                    }
                }

                // NOTE: We skip using execution.env.PORT as container port since it is
                // intended to tell the app what port to listen on, not the actual exposed port.
                // For images like code-server, the app ignores PORT and uses its own config.
                // Instead, we will auto-detect from Docker image EXPOSE if container_port is still 80.

                if host_port == 0 {
                    if let Some(p) = manifest
                        .get("execution")
                        .and_then(|e| e.get("env"))
                        .and_then(|env| env.get("HOST_PORT"))
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<u16>().ok())
                    {
                        host_port = p;
                        info!(
                            "[DockerCliRuntime] Got HOST_PORT={} from canonical manifest env",
                            p
                        );
                    }
                }

                // Legacy env vars list in compute.env (string list). Support PORT (container port) and HOST_PORT.
                if host_port == 0 || container_port == 80 {
                    if let Some(env_list) = manifest
                        .get("compute")
                        .and_then(|c| c.get("env"))
                        .and_then(|e| e.as_array())
                    {
                        for e in env_list {
                            if let Some(s) = e.as_str() {
                                if let Some((k, v)) = s.split_once('=') {
                                    if k == "PORT" {
                                        if let Ok(p) = v.parse::<u16>() {
                                            container_port = p;
                                            info!(
                                                "[DockerCliRuntime] Got container PORT={} from manifest JSON",
                                                p
                                            );
                                        }
                                    }

                                    if k == "HOST_PORT" && host_port == 0 {
                                        if let Ok(p) = v.parse::<u16>() {
                                            host_port = p;
                                            info!(
                                                "[DockerCliRuntime] Got HOST_PORT={} from manifest JSON",
                                                p
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Validate required fields
        if image.is_empty() {
            return Err(RuntimeError::InvalidConfig(
                "No Docker image specified in manifest".to_string(),
            ));
        }

        // If container_port is still default (80), try to detect from Docker image's EXPOSED ports
        if container_port == 80 && !image.is_empty() {
            if let Ok(output) = std::process::Command::new("docker")
                .args([
                    "image",
                    "inspect",
                    "--format",
                    "{{json .Config.ExposedPorts}}",
                    &image,
                ])
                .output()
            {
                if output.status.success() {
                    let exposed_json = String::from_utf8_lossy(&output.stdout);
                    // Parse JSON like {"8080/tcp":{}}
                    if let Ok(exposed) =
                        serde_json::from_str::<serde_json::Value>(exposed_json.trim())
                    {
                        if let Some(obj) = exposed.as_object() {
                            for key in obj.keys() {
                                // key format: "8080/tcp"
                                if let Some(port_str) = key.split('/').next() {
                                    if let Ok(p) = port_str.parse::<u16>() {
                                        if p != 80 {
                                            container_port = p;
                                            info!(
                                                "[DockerCliRuntime] Auto-detected container port={} from image EXPOSE",
                                                container_port
                                            );
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if host_port == 0 {
            // If no explicit host port is provided, avoid publishing privileged ports like 80.
            // In our default stack, host :80 is owned by Caddy, so publishing :80 will fail.
            if container_port <= 1024 {
                warn!(
                    "[DockerCliRuntime] HOST_PORT not found; refusing to publish privileged port {} on host",
                    container_port
                );
                return Err(RuntimeError::InvalidConfig(format!(
                    "HOST_PORT is required for container_port {} (privileged) to avoid host port collisions",
                    container_port
                )));
            }

            warn!(
                "[DockerCliRuntime] HOST_PORT not found; defaulting to host_port = container_port {}",
                container_port
            );
            host_port = container_port;
        }

        // Create log directory and file
        let log_dir = std::env::temp_dir().join("nacelle").join("logs");
        std::fs::create_dir_all(&log_dir).map_err(|e| RuntimeError::Io {
            path: log_dir.clone(),
            source: e,
        })?;
        let log_path = log_dir.join(format!("{}.log", workload_id));

        // Build docker run command
        // docker run -d --name <WORKLOAD_ID> -p <HOST_PORT>:<CONTAINER_PORT> [-e K=V ...] <IMAGE> [CMD...]
        let mut cmd = Command::new("docker");
        cmd.arg("run")
            .arg("-d")
            .arg("--name")
            .arg(workload_id)
            .arg("-p")
            .arg(format!("{}:{}", host_port, container_port));

        // Ensure host.docker.internal resolves on Linux.
        #[cfg(target_os = "linux")]
        {
            cmd.arg("--add-host")
                .arg("host.docker.internal:host-gateway");
        }

        // Add environment variables
        for (k, v) in &env_vars {
            cmd.arg("-e").arg(format!("{}={}", k, v));
        }

        // If a per-capsule egress token exists, inject authenticated proxy env vars.
        if let Some(proxy_port) = self.egress_proxy_port {
            let token = env_vars.iter().find_map(|(k, v)| {
                if k == crate::security::ENV_KEY_EGRESS_TOKEN {
                    Some(v.clone())
                } else {
                    None
                }
            });

            if let Some(token) = token {
                let proxy_url = format!(
                    "http://{}:{}@host.docker.internal:{}",
                    workload_id, token, proxy_port
                );
                cmd.arg("-e").arg(format!("HTTP_PROXY={}", proxy_url));
                cmd.arg("-e").arg(format!("HTTPS_PROXY={}", proxy_url));
                cmd.arg("-e").arg(format!("ALL_PROXY={}", proxy_url));
            }
        }

        // Add volumes
        if let Some(manifest_json) = request.manifest_json {
            if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(manifest_json) {
                if let Some(volumes) = manifest
                    .get("storage")
                    .and_then(|s| s.get("volumes"))
                    .and_then(|v| v.as_array())
                {
                    for vol in volumes {
                        if let (Some(name), Some(mount_path)) = (
                            vol.get("name").and_then(|n| n.as_str()),
                            vol.get("mount_path").and_then(|p| p.as_str()),
                        ) {
                            // Use named volume: gumball_<capsule_id>_<volume_name>
                            let volume_name = format!("gumball_{}_{}", workload_id, name);
                            cmd.arg("-v").arg(format!("{}:{}", volume_name, mount_path));
                            info!(
                                "[DockerCliRuntime] Mounting volume: {} -> {}",
                                volume_name, mount_path
                            );
                        }
                    }
                }
            }
        }

        // Add image
        cmd.arg(&image);

        // Add command arguments if present
        for arg in &command_args {
            cmd.arg(arg);
        }

        // Log the exact command for debugging
        info!(
            "[DockerCliRuntime] Executing: docker run -d --name {} -p {}:{} {} {:?}",
            workload_id, host_port, container_port, image, command_args
        );

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        // Execute docker run
        let output = cmd
            .output()
            .await
            .map_err(|e| RuntimeError::CommandExecution {
                operation: "docker run".to_string(),
                source: e,
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            error!(
                "[DockerCliRuntime] docker run failed for {}: {}",
                workload_id, stderr
            );
            return Err(RuntimeError::CommandFailure {
                operation: "docker run".to_string(),
                exit_code: output.status.code(),
                stderr,
            });
        }

        let container_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        info!(
            "[DockerCliRuntime] Container {} started with ID: {}",
            workload_id, container_id
        );

        // Get container PID via docker inspect
        let inspect_output = Command::new("docker")
            .arg("inspect")
            .arg("--format")
            .arg("{{.State.Pid}}")
            .arg(&container_id)
            .output()
            .await
            .ok();

        let pid: u32 = inspect_output
            .and_then(|o| {
                if o.status.success() {
                    String::from_utf8_lossy(&o.stdout).trim().parse().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0);

        // Write container ID to state file for stop operation
        let state_dir = std::env::temp_dir().join("nacelle").join("state");
        std::fs::create_dir_all(&state_dir).ok();
        let container_id_file = state_dir.join(format!("{}.container_id", workload_id));
        if let Err(e) = std::fs::write(&container_id_file, &container_id) {
            warn!("Failed to write container ID file: {}", e);
        }

        // Write log marker
        let log_content = format!(
            "[DockerCliRuntime] Container started: {}\nImage: {}\nPorts: {}:{}\n",
            container_id, image, host_port, container_port
        );
        std::fs::write(&log_path, log_content).ok();

        Ok(LaunchResult {
            pid: Some(pid),
            bundle_path: Some(PathBuf::from("/")),
            log_path: Some(log_path),
            port: Some(host_port),
        })
    }

    async fn stop(&self, workload_id: &str) -> Result<(), RuntimeError> {
        info!("[DockerCliRuntime] Stopping container: {}", workload_id);

        // First try to stop by container name (workload_id)
        let output = Command::new("docker")
            .arg("stop")
            .arg(workload_id)
            .output()
            .await;

        match output {
            Ok(o) if o.status.success() => {
                info!(
                    "[DockerCliRuntime] Container {} stopped via name",
                    workload_id
                );
            }
            _ => {
                // Fallback: try container ID from state file
                let state_dir = std::env::temp_dir().join("nacelle").join("state");
                let container_id_file = state_dir.join(format!("{}.container_id", workload_id));

                if let Ok(container_id) = std::fs::read_to_string(&container_id_file) {
                    let container_id = container_id.trim();
                    let _ = Command::new("docker")
                        .arg("stop")
                        .arg(container_id)
                        .output()
                        .await;
                    info!(
                        "[DockerCliRuntime] Container {} stopped via ID {}",
                        workload_id, container_id
                    );

                    // Remove container
                    let _ = Command::new("docker")
                        .arg("rm")
                        .arg("-f")
                        .arg(container_id)
                        .output()
                        .await;
                }
            }
        }

        // Also try to remove by name
        let _ = Command::new("docker")
            .arg("rm")
            .arg("-f")
            .arg(workload_id)
            .output()
            .await;

        Ok(())
    }

    fn get_log_path(&self, workload_id: &str) -> Option<PathBuf> {
        let log_dir = std::env::temp_dir().join("nacelle").join("logs");
        Some(log_dir.join(format!("{}.log", workload_id)))
    }
}
