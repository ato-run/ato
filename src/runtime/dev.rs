use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::process::Command;
use tracing::{info, warn};

use crate::process_supervisor::ProcessSupervisor;
use crate::runtime::traits::Runtime;
use crate::runtime::{LaunchRequest, LaunchResult, RuntimeError};

/// Development runtime that serves static files using python3 http.server.
pub struct DevRuntime {
    process_supervisor: Option<Arc<ProcessSupervisor>>,
    egress_proxy_port: Option<u16>,
}

impl DevRuntime {
    pub fn new(
        process_supervisor: Option<Arc<ProcessSupervisor>>,
        egress_proxy_port: Option<u16>,
    ) -> Self {
        Self {
            process_supervisor,
            egress_proxy_port,
        }
    }
}

#[async_trait]
impl Runtime for DevRuntime {
    async fn launch(&self, request: LaunchRequest<'_>) -> Result<LaunchResult, RuntimeError> {
        // Determine directory to serve
        let serve_dir = std::env::current_dir().unwrap_or_default();

        let candidate_paths = vec!["examples/apps", "../examples/apps", "../../examples/apps"];

        let mut app_dir = None;
        for path in candidate_paths {
            let candidate = serve_dir.join(path).join(request.workload_id);
            if candidate.exists() && candidate.join("index.html").exists() {
                app_dir = Some(candidate);
                break;
            }
        }

        let (working_dir, _is_temp) = if let Some(dir) = app_dir {
            info!("Serving custom app UI from {:?}", dir);
            (dir, false)
        } else {
            let temp_dir = std::env::temp_dir()
                .join("capsuled_mock_apps")
                .join(request.workload_id);
            if let Err(e) = std::fs::create_dir_all(&temp_dir) {
                warn!("Failed to create temp dir: {}", e);
                (serve_dir, false)
            } else {
                let index_html = format!(
                    "<html><body><h1>{}</h1><p>No custom UI found in examples/apps/{}.</p></body></html>",
                    request.workload_id, request.workload_id
                );
                if let Err(e) = std::fs::write(temp_dir.join("index.html"), index_html) {
                    warn!("Failed to write default index.html: {}", e);
                    (serve_dir, false)
                } else {
                    info!("Serving default UI from {:?}", temp_dir);
                    (temp_dir, true)
                }
            }
        };

        // Extract port from env vars in Spec
        let mut port = "8000".to_string();
        if let Some(process) = request.spec.process() {
            if let Some(env) = process.env() {
                for e in env {
                    if let Some((k, v)) = e.split_once('=') {
                        if k == "PORT" {
                            port = v.to_string();
                        }
                    }
                }
            }
        }

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

        info!(
            "Spawning dev runtime (python3 http.server) on port {}",
            port
        );
        let mut cmd = Command::new("python3");
        cmd.arg("-m")
            .arg("http.server")
            .arg(port)
            .current_dir(working_dir)
            .stdout(stdout)
            .stderr(stderr);

        if let Some(proxy_port) = self.egress_proxy_port {
            let mut token: Option<String> = None;
            if let Some(process) = request.spec.process() {
                if let Some(env) = process.env() {
                    for e in env {
                        if let Some((k, v)) = e.split_once('=') {
                            if k == crate::security::ENV_KEY_EGRESS_TOKEN {
                                token = Some(v.to_string());
                                break;
                            }
                        }
                    }
                }
            }

            let proxy_url = if let Some(token) = token {
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

                info!("Dev runtime started with PID {}", pid);

                // Write PID file
                let state_dir = std::env::temp_dir().join("capsuled").join("state");
                if let Err(e) = std::fs::create_dir_all(&state_dir) {
                    warn!("Failed to create state dir: {}", e);
                } else {
                    let pid_file = state_dir.join(format!("{}.pid", request.workload_id));
                    if let Err(e) = std::fs::write(&pid_file, pid.to_string()) {
                        warn!("Failed to write PID file: {}", e);
                    }
                }

                if let Some(_supervisor) = &self.process_supervisor {
                    // Supervisor registration issue (tokio vs std Child)
                    // For now, we skip registration or need to fix Supervisor.
                    // The original code used std::process::Command so it got std::process::Child.
                    // Here we use tokio::process::Command.
                    // We can use `into_std` but that consumes the child.
                    // But we need to return PID.
                    //
                    // Actually, `ProcessSupervisor::register` takes `u32` (PID) or `Child`?
                    // Let's check `process_supervisor.rs`.
                }

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
        // Similar to NativeRuntime, we rely on PID file or external kill.
        // But DevRuntime doesn't write PID file here.
        // `CapsuleManager` has the PID.
        // If `Runtime::stop` is called, we might need to kill by PID if we knew it.
        // But `Runtime` trait doesn't take PID.
        //
        // For now, we assume `CapsuleManager` handles the kill if `Runtime::stop` doesn't do it?
        // No, `CapsuleManager` calls `stop_capsule` which calls `Runtime::stop`.
        //
        // We should probably write a PID file for DevRuntime too to be consistent.
        let pid_file = std::env::temp_dir()
            .join("capsuled")
            .join("state")
            .join(format!("{}.pid", workload_id));
        if pid_file.exists() {
            if let Ok(content) = std::fs::read_to_string(&pid_file) {
                if let Ok(pid) = content.trim().parse::<i32>() {
                    #[cfg(unix)]
                    use nix::sys::signal::{self, Signal};
                    #[cfg(unix)]
                    use nix::unistd::Pid;

                    info!(
                        "Attempting to stop dev runtime PID {} [VERIFY_NEW_CODE]",
                        pid
                    );

                    // Check if process exists first (signal 0)
                    match signal::kill(Pid::from_raw(pid), None) {
                        Ok(_) => {
                            info!("Process {} exists, sending SIGTERM...", pid);
                            match signal::kill(Pid::from_raw(pid), Signal::SIGTERM) {
                                Ok(_) => {
                                    info!("Successfully sent SIGTERM to PID {}", pid);
                                }
                                Err(e) => {
                                    warn!("Failed to send SIGTERM to PID {}: {}", pid, e);
                                }
                            }
                        }
                        Err(e) => {
                            warn!(
                                "Process {} does not exist or cannot be signaled (check): {}",
                                pid, e
                            );
                            // If it doesn't exist (ESRCH), we can consider it stopped, but let's log it.
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn get_log_path(&self, workload_id: &str) -> Option<PathBuf> {
        let log_dir = std::env::temp_dir().join("capsuled").join("logs");
        Some(log_dir.join(format!("{}.log", workload_id)))
    }
}
