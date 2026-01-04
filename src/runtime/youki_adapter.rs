//! Youki Runtime Adapter
//!
//! This module adapts the libadep-runtime YoukiRuntime to the Engine's Runtime trait.
//! It provides a bridge between the ADEP capsule format and OCI container execution.

use async_trait::async_trait;
use capsule_runtime::{AdepContainerRuntime, YoukiConfig, YoukiRuntime as LibadepYoukiRuntime};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::runtime::{LaunchRequest, LaunchResult, Runtime, RuntimeError};

/// Adapter that wraps libadep-runtime's YoukiRuntime for use with the Engine
pub struct YoukiRuntimeAdapter {
    /// The underlying libadep YoukiRuntime (shared for pool usage)
    inner: Arc<LibadepYoukiRuntime>,

    /// Mapping from workload ID to container ID
    workload_to_container: Arc<RwLock<HashMap<String, String>>>,

    /// Log directory for container outputs
    log_dir: PathBuf,

    /// Bundle root directory
    bundle_root: PathBuf,
}

impl YoukiRuntimeAdapter {
    /// Creates a new YoukiRuntimeAdapter with default configuration
    pub fn new(log_dir: PathBuf, bundle_root: PathBuf) -> Self {
        Self {
            inner: Arc::new(LibadepYoukiRuntime::new()),
            workload_to_container: Arc::new(RwLock::new(HashMap::new())),
            log_dir,
            bundle_root,
        }
    }

    /// Creates a new YoukiRuntimeAdapter with custom configuration
    pub fn with_config(config: YoukiConfig, log_dir: PathBuf, bundle_root: PathBuf) -> Self {
        Self {
            inner: Arc::new(LibadepYoukiRuntime::with_config(config)),
            workload_to_container: Arc::new(RwLock::new(HashMap::new())),
            log_dir,
            bundle_root,
        }
    }

    /// Checks if youki is available on this system
    pub async fn is_available(&self) -> bool {
        self.inner.is_available().await
    }

    /// Returns a shared reference to the underlying LibadepYoukiRuntime
    /// This is used by PoolRegistry to create pool managers
    pub fn inner_arc(&self) -> Arc<LibadepYoukiRuntime> {
        Arc::clone(&self.inner)
    }

    /// Prepares a bundle directory with config.json from OCI spec
    fn prepare_bundle(
        &self,
        workload_id: &str,
        spec: &oci_spec::runtime::Spec,
    ) -> Result<PathBuf, RuntimeError> {
        let bundle_path = self.bundle_root.join(workload_id);
        fs::create_dir_all(&bundle_path).map_err(|e| RuntimeError::Io {
            path: bundle_path.clone(),
            source: e,
        })?;

        // Write config.json
        let config_path = bundle_path.join("config.json");
        let config_json =
            serde_json::to_string_pretty(spec).map_err(RuntimeError::SpecSerialization)?;

        fs::write(&config_path, config_json).map_err(|e| RuntimeError::Io {
            path: config_path,
            source: e,
        })?;

        debug!(bundle = %bundle_path.display(), "Bundle prepared");
        Ok(bundle_path)
    }
}

#[async_trait]
impl Runtime for YoukiRuntimeAdapter {
    async fn launch(&self, request: LaunchRequest<'_>) -> Result<LaunchResult, RuntimeError> {
        debug!(
            workload_id = %request.workload_id,
            "Launching capsule via YoukiRuntime"
        );

        // Prepare bundle with config.json
        let bundle_path = self.prepare_bundle(request.workload_id, request.spec)?;
        let log_path = self.log_dir.join(format!("{}.log", request.workload_id));

        // Ensure log directory exists
        if let Some(parent) = log_path.parent() {
            fs::create_dir_all(parent).map_err(|e| RuntimeError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }

        // Launch using youki directly (we already have the OCI spec)
        let output = tokio::process::Command::new("youki")
            .args([
                "create",
                "--bundle",
                &bundle_path.to_string_lossy(),
                request.workload_id,
            ])
            .output()
            .await
            .map_err(|e| RuntimeError::CommandExecution {
                operation: "youki create".to_string(),
                source: e,
            })?;

        if !output.status.success() {
            return Err(RuntimeError::CommandFailure {
                operation: "youki create".to_string(),
                exit_code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }

        // Start the container
        let output = tokio::process::Command::new("youki")
            .args(["start", request.workload_id])
            .output()
            .await
            .map_err(|e| RuntimeError::CommandExecution {
                operation: "youki start".to_string(),
                source: e,
            })?;

        if !output.status.success() {
            // Cleanup created container
            let _ = tokio::process::Command::new("youki")
                .args(["delete", "--force", request.workload_id])
                .output()
                .await;

            return Err(RuntimeError::CommandFailure {
                operation: "youki start".to_string(),
                exit_code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }

        // Store mapping
        self.workload_to_container.write().await.insert(
            request.workload_id.to_string(),
            request.workload_id.to_string(),
        );

        // Get PID from youki state
        let pid = self
            .get_container_pid(request.workload_id)
            .await
            .unwrap_or(0);

        info!(
            workload_id = %request.workload_id,
            pid = pid,
            "Capsule launched successfully via YoukiRuntime"
        );

        Ok(LaunchResult {
            pid,
            bundle_path,
            log_path,
        })
    }

    async fn stop(&self, workload_id: &str) -> Result<(), RuntimeError> {
        debug!(workload_id = %workload_id, "Stopping capsule via YoukiRuntime");

        // Try to find the container ID
        let container_id = {
            let mapping = self.workload_to_container.read().await;
            mapping.get(workload_id).cloned()
        };

        let id_to_stop = container_id.as_deref().unwrap_or(workload_id);

        // Stop via libadep-runtime
        self.inner
            .stop(id_to_stop)
            .await
            .map_err(|e| RuntimeError::CommandFailure {
                operation: "youki stop".to_string(),
                exit_code: None,
                stderr: e.to_string(),
            })?;

        // Remove mapping
        self.workload_to_container.write().await.remove(workload_id);

        info!(workload_id = %workload_id, "Capsule stopped successfully via YoukiRuntime");
        Ok(())
    }

    fn get_log_path(&self, workload_id: &str) -> Option<PathBuf> {
        // Return log path based on workload ID
        Some(self.log_dir.join(format!("{}.log", workload_id)))
    }
}

impl YoukiRuntimeAdapter {
    /// Gets the PID of a running container from youki state
    async fn get_container_pid(&self, container_id: &str) -> Option<u32> {
        let output = tokio::process::Command::new("youki")
            .args(["state", container_id])
            .output()
            .await
            .ok()?;

        if !output.status.success() {
            warn!(container_id = %container_id, "Failed to get container state");
            return None;
        }

        // Parse JSON state to get PID
        #[derive(serde::Deserialize)]
        struct ContainerState {
            pid: Option<u32>,
        }

        let state: ContainerState = serde_json::from_slice(&output.stdout).ok()?;
        state.pid
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_youki_runtime_adapter_new() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path().join("logs");
        let bundle_dir = temp_dir.path().join("bundles");
        let adapter = YoukiRuntimeAdapter::new(log_dir, bundle_dir);
        assert!(adapter.log_dir.to_string_lossy().contains("logs"));
    }

    #[test]
    fn test_get_log_path() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path().join("logs");
        let bundle_dir = temp_dir.path().join("bundles");
        let adapter = YoukiRuntimeAdapter::new(log_dir, bundle_dir);

        let log_path = adapter.get_log_path("test-workload");
        assert!(log_path.is_some());
        assert!(log_path
            .unwrap()
            .to_string_lossy()
            .contains("test-workload.log"));
    }

    #[tokio::test]
    async fn test_is_available() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path().join("logs");
        let bundle_dir = temp_dir.path().join("bundles");
        let _adapter = YoukiRuntimeAdapter::new(log_dir, bundle_dir);

        // On non-Linux, should be false
        #[cfg(not(target_os = "linux"))]
        assert!(!_adapter.is_available().await);
    }

    #[test]
    fn test_prepare_bundle() {
        use oci_spec::runtime::{ProcessBuilder, RootBuilder, SpecBuilder};

        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path().join("logs");
        let bundle_dir = temp_dir.path().join("bundles");
        let adapter = YoukiRuntimeAdapter::new(log_dir, bundle_dir.clone());

        // Create a minimal OCI spec
        let process = ProcessBuilder::default()
            .args(vec!["sh".to_string()])
            .build()
            .unwrap();

        let root = RootBuilder::default().path("rootfs").build().unwrap();

        let spec = SpecBuilder::default()
            .version("1.0.2")
            .root(root)
            .process(process)
            .build()
            .unwrap();

        let result = adapter.prepare_bundle("test-container", &spec);
        assert!(result.is_ok());

        let bundle_path = result.unwrap();
        assert!(bundle_path.join("config.json").exists());
    }
}
