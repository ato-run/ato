use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::process::Command;
use tracing::{info, warn};

use crate::adep::{CapsuleManifestV1, RuntimeType};
use crate::artifact::ArtifactManager;
use crate::runtime::traits::Runtime;
use crate::runtime::{LaunchRequest, LaunchResult, RuntimeError};

/// Native runtime that executes binaries directly on the host.
#[derive(Debug)]
pub struct NativeRuntime {
    artifact_manager: Option<Arc<ArtifactManager>>,
    egress_proxy_port: Option<u16>,
}

impl NativeRuntime {
    pub fn new(
        artifact_manager: Option<Arc<ArtifactManager>>,
        egress_proxy_port: Option<u16>,
    ) -> Self {
        Self {
            artifact_manager,
            egress_proxy_port,
        }
    }
}

#[async_trait]
impl Runtime for NativeRuntime {
    async fn launch(&self, request: LaunchRequest<'_>) -> Result<LaunchResult, RuntimeError> {
        // Parse manifest to get native execution details if simpler than OCI spec extraction
        let execution = if let Some(json) = request.manifest_json {
            let manifest: CapsuleManifestV1 = serde_json::from_str(json).ok().ok_or_else(|| {
                RuntimeError::InvalidConfig("Failed to parse manifest JSON".to_string())
            })?;
            if manifest.execution.runtime == RuntimeType::Native {
                Some(manifest.execution)
            } else {
                None
            }
        } else {
            None
        }
        .ok_or_else(|| {
            RuntimeError::InvalidConfig("Missing native execution config".to_string())
        })?;

        // Entrypoint logic
        // "runtime_id" is essentially the binary/artifact
        let raw_entrypoint = execution.entrypoint;
        // Split args?
        // Note: oci/spec_builder logic was `shell_words::split`.
        // We do the same here to get the binary + args.
        // Or if entrypoint is just binary, where are args?
        // As discussed, we assume entrypoint contains command + args for Native.
        let parts =
            shell_words::split(&raw_entrypoint).unwrap_or_else(|_| vec![raw_entrypoint.clone()]);
        let (runtime_id, args) = if parts.is_empty() {
            return Err(RuntimeError::InvalidConfig("Empty entrypoint".to_string()));
        } else {
            (parts[0].clone(), parts[1..].to_vec())
        };

        // Parse version from runtime_id (e.g. "mlx-community/model@1.0")
        let (name_part, version_part) = if let Some(at_pos) = runtime_id.find('@') {
            (&runtime_id[..at_pos], &runtime_id[at_pos + 1..])
        } else {
            (runtime_id.as_str(), "latest")
        };

        info!("Ensuring runtime {}@{}", name_part, version_part);

        // Special handling for built-in runtimes (mlx, llama, vllm)
        let binary_path = match name_part {
            "mlx-mock" => {
                info!("Using mock MLX runtime (shell pass-through)");
                PathBuf::from("/usr/bin/env")
            }
            "mlx" => {
                // MLX runtime uses start.sh in ~/.gumball/runtimes/mlx
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                let mlx_runtime_path = PathBuf::from(&home)
                    .join(".gumball")
                    .join("runtimes")
                    .join("mlx")
                    .join("start.sh");

                // Fallback to development path
                if mlx_runtime_path.exists() {
                    info!("Using MLX runtime at: {:?}", mlx_runtime_path);
                    mlx_runtime_path
                } else {
                    // Try project path
                    let dev_path = std::env::current_dir()
                        .unwrap_or_default()
                        .parent()
                        .map(|p| p.join("desktop/mlx-runtime/start.sh"))
                        .unwrap_or_default();
                    if dev_path.exists() {
                        info!("Using MLX dev runtime at: {:?}", dev_path);
                        dev_path
                    } else {
                        return Err(RuntimeError::InvalidConfig(format!(
                            "MLX runtime not found. Expected at {:?} or {:?}",
                            mlx_runtime_path, dev_path
                        )));
                    }
                }
            }
            _ if std::path::Path::new(name_part).is_absolute()
                && std::path::Path::new(name_part).exists() =>
            {
                info!("Using direct path: {}", name_part);
                std::path::PathBuf::from(name_part)
            }
            _ => {
                // Use artifact manager for other runtimes if not absolute path
                if let Some(am) = &self.artifact_manager {
                    am.ensure_runtime(name_part, version_part, None)
                        .await
                        .map_err(|e| {
                            RuntimeError::InvalidConfig(format!("Failed to ensure runtime: {}", e))
                        })?
                } else {
                    return Err(RuntimeError::InvalidConfig(
                        "ArtifactManager not configured and runtime is not a direct path"
                            .to_string(),
                    ));
                }
            }
        };

        // Env vars from Request Spec (which includes logic from manifest)
        let mut env_vars = std::collections::HashMap::new();
        if let Some(process) = request.spec.process() {
            if let Some(env) = process.env() {
                for e in env {
                    if let Some((k, v)) = e.split_once('=') {
                        if k == "HOST" && (v.is_empty() || v.trim().is_empty()) {
                            continue;
                        }
                        env_vars.insert(k.to_string(), v.to_string());
                    }
                }
            }
        }

        let port = env_vars
            .get("PORT")
            .cloned()
            .unwrap_or_else(|| "0".to_string());

        // Define GUMBALL_MODELS_DIR
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let models_dir = PathBuf::from(&home).join(".gumball").join("models");
        let models_dir_str = models_dir.to_string_lossy();

        // Perform variable substitution on args
        // Prioritize Spec args if available (skipping argv[0] which is binary name)
        let effective_args = if let Some(process) = request.spec.process() {
            if let Some(spec_args) = process.args() {
                if !spec_args.is_empty() {
                    // spec_args[0] matches binary name usually, but for consistency with Command::new(binary).args(...)
                    // we usually pass the rest.
                    spec_args.iter().skip(1).cloned().collect()
                } else {
                    args
                }
            } else {
                args
            }
        } else {
            args
        };

        let final_args: Vec<String> = effective_args
            .iter()
            .map(|arg| {
                arg.replace("${PORT}", &port)
                    .replace("${GUMBALL_MODELS_DIR}", &models_dir_str)
            })
            .collect();

        // Create log file
        let log_dir = std::env::temp_dir().join("capsuled").join("logs");
        std::fs::create_dir_all(&log_dir).map_err(|e| RuntimeError::Io {
            path: log_dir.clone(),
            source: e,
        })?;
        let log_path_buf = log_dir.join(format!("{}.log", request.workload_id));
        let log_file = std::fs::File::create(&log_path_buf).map_err(|e| RuntimeError::Io {
            path: log_path_buf.clone(),
            source: e,
        })?;

        let (stdout, stderr) = {
            let stderr = log_file
                .try_clone()
                .ok()
                .map(Stdio::from)
                .unwrap_or(Stdio::null());
            (Stdio::from(log_file), stderr)
        };

        let mut cmd = Command::new(&binary_path);
        cmd.args(&final_args)
            .stdout(stdout)
            .stderr(stderr)
            .envs(&env_vars);

        if let Some(proxy_port) = self.egress_proxy_port {
            let proxy_url = if let Some(token) = env_vars.get(crate::security::ENV_KEY_EGRESS_TOKEN)
            {
                format!(
                    "http://{}:{}@127.0.0.1:{}",
                    request.workload_id, token, proxy_port
                )
            } else {
                format!("http://127.0.0.1:{}", proxy_port)
            };
            cmd.env("HTTP_PROXY", &proxy_url)
                .env("HTTPS_PROXY", &proxy_url)
                .env("ALL_PROXY", &proxy_url);
        }

        match cmd.spawn() {
            Ok(child) => {
                let pid = child.id().ok_or_else(|| RuntimeError::CommandFailure {
                    operation: "spawn".to_string(),
                    exit_code: None,
                    stderr: "Failed to get PID".to_string(),
                })?;

                info!("Native runtime started with PID {}", pid);

                // Write PID file
                let state_dir = std::env::temp_dir().join("capsuled").join("state");
                std::fs::create_dir_all(&state_dir).map_err(|e| RuntimeError::Io {
                    path: state_dir.clone(),
                    source: e,
                })?;
                let pid_file = state_dir.join(format!("{}.pid", request.workload_id));
                std::fs::write(&pid_file, pid.to_string()).map_err(|e| RuntimeError::Io {
                    path: pid_file.clone(),
                    source: e,
                })?;

                Ok(LaunchResult {
                    pid: Some(pid),
                    bundle_path: Some(PathBuf::from("/")),
                    log_path: Some(log_path_buf),
                    port: None,
                })
            }
            Err(e) => Err(RuntimeError::CommandExecution {
                operation: "spawn".to_string(),
                source: e,
            }),
        }
    }

    async fn stop(&self, workload_id: &str) -> Result<(), RuntimeError> {
        let pid_file = std::env::temp_dir()
            .join("capsuled")
            .join("state")
            .join(format!("{}.pid", workload_id));
        if pid_file.exists() {
            if let Ok(content) = std::fs::read_to_string(&pid_file) {
                if let Ok(pid) = content.trim().parse::<i32>() {
                    use nix::sys::signal::{self, Signal};
                    use nix::unistd::Pid;

                    info!("Stopping native process PID {}", pid);
                    match signal::kill(Pid::from_raw(pid), Signal::SIGTERM) {
                        Ok(_) => {}
                        Err(e) => warn!("Failed to stop native process {}: {}", pid, e),
                    }
                }
            }
            let _ = std::fs::remove_file(pid_file);
        } else {
            warn!("No PID file found for workload {}", workload_id);
        }
        Ok(())
    }

    fn get_log_path(&self, workload_id: &str) -> Option<PathBuf> {
        let log_dir = std::env::temp_dir().join("capsuled").join("logs");
        Some(log_dir.join(format!("{}.log", workload_id)))
    }
}
