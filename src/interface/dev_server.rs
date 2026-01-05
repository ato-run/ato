//! Embedded DevServer for capsule-cli integration
//!
//! This module provides a lightweight, embeddable gRPC server that can be
//! spawned in-process by capsule-cli for `capsule dev` and `capsule run --engine`.
//!
//! ## Design Principles
//!
//! 1. **Zero Configuration**: Sensible defaults for development
//! 2. **Ephemeral Ports**: Auto-selects available ports
//! 3. **Graceful Shutdown**: Clean resource cleanup via `ServerHandle`
//! 4. **No tracing init**: Respects caller's tracing subscriber
//!
//! ## Usage
//!
//! ```ignore
//! use capsuled::dev_server::{DevServerConfig, DevServerHandle};
//!
//! let config = DevServerConfig::default();
//! let handle = DevServerHandle::start(config).await?;
//!
//! println!("Engine running at {}", handle.grpc_endpoint());
//!
//! // ... do work ...
//!
//! handle.shutdown().await;
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::oneshot;
use tracing::{info, warn};

use crate::artifact::{manager::ArtifactConfig, ArtifactManager};
use crate::capsule_manager::CapsuleManager;
use crate::interface::grpc;
use crate::hardware;
use crate::job_history::SqliteJobHistoryStore;
use crate::network::service_registry::ServiceRegistry;
use crate::process_supervisor::ProcessSupervisor;
use crate::runtime::{ContainerRuntime, RuntimeConfig, RuntimeKind};
use crate::security::audit::AuditLogger;
use crate::wasm_host::AdepLogicHost;

/// Configuration for the embedded DevServer
#[derive(Debug, Clone)]
pub struct DevServerConfig {
    /// gRPC server port (0 = auto-select)
    pub grpc_port: u16,

    /// HTTP API server port (0 = disabled, Some = specific port)
    pub http_port: Option<u16>,

    /// Data directory for job history, logs, etc.
    pub data_dir: PathBuf,

    /// Allowed host paths for bind mounts
    pub allowed_host_paths: Vec<String>,

    /// Models cache directory
    pub models_cache_dir: PathBuf,

    /// Enable verbose logging
    pub verbose: bool,

    /// Enable development mode (relaxed security, file watching support)
    pub dev_mode: bool,
}

impl Default for DevServerConfig {
    fn default() -> Self {
        let home = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/tmp"));

        Self {
            grpc_port: 0, // Auto-select
            http_port: None, // Disabled by default for embedded mode
            data_dir: home.join(".capsule").join("dev-engine"),
            allowed_host_paths: vec![],
            models_cache_dir: home.join(".capsule").join("models"),
            verbose: false,
            dev_mode: false, // Strict security by default
        }
    }
}

impl DevServerConfig {
    /// Create a new config with a specific gRPC port
    pub fn with_port(port: u16) -> Self {
        Self {
            grpc_port: port,
            ..Default::default()
        }
    }

    /// Add allowed host paths
    pub fn with_allowed_paths(mut self, paths: Vec<String>) -> Self {
        self.allowed_host_paths = paths;
        self
    }

    /// Set development mode (relaxed security)
    pub fn with_dev_mode(mut self, dev_mode: bool) -> Self {
        self.dev_mode = dev_mode;
        self
    }

    /// Set data directory
    pub fn with_data_dir(mut self, dir: PathBuf) -> Self {
        self.data_dir = dir;
        self
    }
}

/// Handle to a running DevServer
///
/// The server runs as background tokio tasks. Call `shutdown()` to stop it.
pub struct DevServerHandle {
    /// The actual gRPC endpoint (including auto-selected port)
    grpc_endpoint: String,

    /// HTTP API endpoint (if enabled)
    http_endpoint: Option<String>,

    /// Shutdown signal sender
    shutdown_tx: Option<oneshot::Sender<()>>,

    /// Join handle for the gRPC server task
    grpc_handle: Option<tokio::task::JoinHandle<()>>,
}

impl DevServerHandle {
    /// Start the DevServer with the given configuration
    ///
    /// This spawns background tasks for the gRPC server (and optionally HTTP API).
    /// The function returns immediately with a handle for control.
    pub async fn start(config: DevServerConfig) -> anyhow::Result<Self> {
        // Ensure directories exist
        std::fs::create_dir_all(&config.data_dir)?;
        std::fs::create_dir_all(&config.models_cache_dir)?;

        let logs_dir = config.data_dir.join("logs");
        std::fs::create_dir_all(&logs_dir)?;

        let keys_dir = config.data_dir.join("keys");
        std::fs::create_dir_all(&keys_dir)?;

        // Initialize AuditLogger
        let audit_logger = Arc::new(
            AuditLogger::new(
                logs_dir.join("audit.jsonl"),
                keys_dir.join("node_key.pem"),
                "capsuled-embedded".to_string(),
            )
            .map_err(|e| anyhow::anyhow!("Failed to initialize audit logger: {}", e))?,
        );

        // GPU detection
        let gpu_detector = hardware::create_gpu_detector();
        if let Ok(report) = gpu_detector.detect_gpus() {
            if !report.gpus.is_empty() {
                info!(
                    "GPU detected: {} ({:.2} GB)",
                    report.gpus[0].device_name,
                    report.total_vram_gb()
                );
            }
        }

        // Service Registry
        let service_registry = Arc::new(ServiceRegistry::new(None));

        // Job History
        let job_history_db = config.data_dir.join("job_history.sqlite");
        let job_history = Arc::new(
            SqliteJobHistoryStore::new(&job_history_db)
                .map_err(|e| anyhow::anyhow!("Failed to initialize job history: {}", e))?,
        );

        // Process Supervisor
        let process_supervisor = Arc::new(ProcessSupervisor::new());

        // Artifact Manager (minimal config for dev)
        let artifact_config = ArtifactConfig {
            registry_url: "".to_string(), // No remote registry in dev mode
            cache_path: config.data_dir.join("runtimes"),
            cas_root: None,
        };
        let artifact_manager = Arc::new(
            ArtifactManager::new(artifact_config)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to initialize artifact manager: {}", e))?,
        );

        // Runtime config (Source mode for dev - UARC V1 compliant)
        let runtime_config = RuntimeConfig {
            kind: RuntimeKind::Source,
            binary_path: PathBuf::from("/dev/null"),
            bundle_root: config.data_dir.join("bundles"),
            state_root: config.data_dir.join("state"),
            log_dir: logs_dir.clone(),
            hook_retry_attempts: 1,
        };

        let runtime = Arc::new(ContainerRuntime::new(
            runtime_config.clone(),
            Some(artifact_manager.clone()),
            Some(process_supervisor.clone()),
            None, // No egress proxy in dev mode
        ));

        // Manifest Verifier (permissive for dev)
        let verifier = Arc::new(crate::security::verifier::ManifestVerifier::new(None, false));

        // Metrics Collector
        let metrics_collector = Arc::new(crate::metrics::collector::MetricsCollector::new());

        // CapsuleManager
        let capsule_manager = Arc::new(CapsuleManager::new(
            audit_logger.clone(),
            config.allowed_host_paths.clone(),
            gpu_detector.clone(),
            Some(service_registry.clone()),
            None, // mDNS
            None, // Traefik
            Some(artifact_manager.clone()),
            Some(process_supervisor.clone()),
            None, // No egress proxy port
            verifier,
            Some(runtime_config),
            Some(metrics_collector),
            None, // Storage config
        ));

        // WasmHost (minimal)
        let wasm_bytes = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];
        let wasm_host = Arc::new(
            AdepLogicHost::new(&wasm_bytes)
                .map_err(|e| anyhow::anyhow!("Failed to initialize WASM host: {}", e))?,
        );

        // Tailscale Manager (no-op for dev)
        let tailscale_manager =
            Arc::new(crate::network::tailscale::TailscaleManager::start(None, None, None));

        // Bind to port (0 = auto-select)
        let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", config.grpc_port))
            .await
            .map_err(|e| anyhow::anyhow!("Failed to bind gRPC port: {}", e))?;

        let local_addr = listener.local_addr()?;
        let grpc_endpoint = format!("http://127.0.0.1:{}", local_addr.port());

        info!("DevServer starting on {}", grpc_endpoint);

        // Shutdown channel
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        // Spawn gRPC server with graceful shutdown
        let grpc_handle = {
            let capsule_manager = capsule_manager.clone();
            let wasm_host = wasm_host.clone();
            let runtime = runtime.clone();
            let allowed_host_paths = config.allowed_host_paths.clone();
            let models_cache_dir = config.models_cache_dir.clone();
            let tailscale_manager = tailscale_manager.clone();
            let service_registry = service_registry.clone();
            let gpu_detector = gpu_detector.clone();
            let artifact_manager = artifact_manager.clone();
            let job_history = job_history.clone();

            tokio::spawn(async move {
                let addr_str = format!("127.0.0.1:{}", local_addr.port());

                // Run server until shutdown signal
                tokio::select! {
                    result = grpc::start_grpc_server(
                        &addr_str,
                        capsule_manager,
                        wasm_host,
                        runtime,
                        allowed_host_paths,
                        models_cache_dir,
                        "native".to_string(),
                        tailscale_manager,
                        service_registry,
                        gpu_detector,
                        artifact_manager,
                        job_history,
                    ) => {
                        if let Err(e) = result {
                            warn!("DevServer gRPC error: {}", e);
                        }
                    }
                    _ = shutdown_rx => {
                        info!("DevServer shutdown requested");
                    }
                }
            })
        };

        Ok(Self {
            grpc_endpoint,
            http_endpoint: None,
            shutdown_tx: Some(shutdown_tx),
            grpc_handle: Some(grpc_handle),
        })
    }

    /// Get the gRPC endpoint URL (e.g., "http://127.0.0.1:50123")
    pub fn grpc_endpoint(&self) -> &str {
        &self.grpc_endpoint
    }

    /// Get the HTTP API endpoint URL (if enabled)
    pub fn http_endpoint(&self) -> Option<&str> {
        self.http_endpoint.as_deref()
    }

    /// Gracefully shutdown the server
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }

        if let Some(handle) = self.grpc_handle.take() {
            // Give it a moment to shutdown gracefully
            tokio::time::timeout(std::time::Duration::from_secs(5), handle)
                .await
                .ok();
        }

        info!("DevServer shutdown complete");
    }

    /// Check if the server is still running
    pub fn is_running(&self) -> bool {
        self.grpc_handle
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }
}

impl Drop for DevServerHandle {
    fn drop(&mut self) {
        // Send shutdown signal if not already done
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_dev_server_config_default() {
        let config = DevServerConfig::default();
        assert_eq!(config.grpc_port, 0);
        assert!(config.http_port.is_none());
    }

    #[tokio::test]
    async fn test_dev_server_config_builder() {
        let config = DevServerConfig::with_port(50051)
            .with_allowed_paths(vec!["/tmp".to_string()])
            .with_data_dir(PathBuf::from("/tmp/test-engine"));

        assert_eq!(config.grpc_port, 50051);
        assert_eq!(config.allowed_host_paths, vec!["/tmp".to_string()]);
        assert_eq!(config.data_dir, PathBuf::from("/tmp/test-engine"));
    }

    // Note: Full integration test requires all dependencies to be available
    // This is tested via capsule-cli e2e tests
}
