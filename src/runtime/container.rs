use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use oci_spec::runtime::Spec;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{debug, error, info, warn};

use crate::artifact::ArtifactManager;
use crate::process_supervisor::ProcessSupervisor;
use crate::runtime::traits::Runtime;
use crate::runtime::{LaunchRequest, LaunchResult, RuntimeConfig, RuntimeError};
use std::sync::Arc;

/// Container runtime wrapper that launches workloads using runc/youki.
/// UARC V1: Native runtime removed - only OCI containers supported
#[derive(Debug, Clone)]
pub struct ContainerRuntime {
    config: RuntimeConfig,
}

impl ContainerRuntime {
    pub fn new(
        config: RuntimeConfig,
        _artifact_manager: Option<Arc<ArtifactManager>>,
        _process_supervisor: Option<Arc<ProcessSupervisor>>,
        _egress_proxy_port: Option<u16>,
    ) -> Self {
        // UARC V1: Native runtime removed
        Self {
            config,
        }
    }

    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    async fn prepare_bundle(
        &self,
        workload_id: &str,
        spec: &Spec,
    ) -> Result<PathBuf, RuntimeError> {
        self.ensure_directory(&self.config.bundle_root).await?;
        let bundle_path = self.config.bundle_root.join(workload_id);
        self.ensure_directory(&bundle_path).await?;

        if let Some(root) = spec.root() {
            let path = root.path();
            if !path.exists() {
                return Err(RuntimeError::InvalidConfig(format!(
                    "root filesystem path {} does not exist",
                    path.display()
                )));
            }
        }

        let config_path = bundle_path.join("config.json");
        let spec_json = serde_json::to_vec_pretty(spec).map_err(RuntimeError::SpecSerialization)?;
        let mut file = fs::File::create(&config_path)
            .await
            .map_err(|source| RuntimeError::Io {
                path: config_path.clone(),
                source,
            })?;
        file.write_all(&spec_json)
            .await
            .map_err(|source| RuntimeError::Io {
                path: config_path.clone(),
                source,
            })?;
        file.flush().await.map_err(|source| RuntimeError::Io {
            path: config_path.clone(),
            source,
        })?;

        Ok(bundle_path)
    }

    async fn prepare_log_path(&self, workload_id: &str) -> Result<PathBuf, RuntimeError> {
        self.ensure_directory(&self.config.log_dir).await?;
        Ok(self.config.log_dir.join(format!("{}.log", workload_id)))
    }

    async fn write_manifest_snapshot(
        &self,
        workload_id: &str,
        manifest_json: Option<&str>,
    ) -> Result<(), RuntimeError> {
        if manifest_json.is_none() {
            return Ok(());
        }

        let snapshot_dir = self.config.bundle_root.join(workload_id);
        self.ensure_directory(&snapshot_dir).await?;
        let snapshot_path = snapshot_dir.join("manifest.json");
        let mut file =
            fs::File::create(&snapshot_path)
                .await
                .map_err(|source| RuntimeError::Io {
                    path: snapshot_path.clone(),
                    source,
                })?;
        file.write_all(manifest_json.unwrap().as_bytes())
            .await
            .map_err(|source| RuntimeError::Io {
                path: snapshot_path.clone(),
                source,
            })?;
        file.flush().await.map_err(|source| RuntimeError::Io {
            path: snapshot_path.clone(),
            source,
        })?;
        Ok(())
    }

    async fn create_and_start(
        &self,
        workload_id: &str,
        bundle_path: &Path,
        pid_file: &Path,
        log_path: &Path,
    ) -> Result<u32, RuntimeError> {
        match self
            .run_create(workload_id, bundle_path, pid_file, log_path)
            .await?
        {
            Ok(()) => {}
            Err(err) => return Err(err.into_error("create")),
        }

        match self.run_start(workload_id).await? {
            Ok(()) => {}
            Err(err) => return Err(err.into_error("start")),
        }

        let state = self.query_state(workload_id).await?;
        Ok(state.pid)
    }

    async fn run_create(
        &self,
        workload_id: &str,
        bundle_path: &Path,
        pid_file: &Path,
        log_path: &Path,
    ) -> Result<Result<(), CommandOutput>, RuntimeError> {
        let mut cmd = Command::new(&self.config.binary_path);
        cmd.arg("create")
            .arg("--root")
            .arg(&self.config.state_root)
            .arg("--bundle")
            .arg(bundle_path)
            .arg("--pid-file")
            .arg(pid_file)
            .arg("--log")
            .arg(log_path)
            .arg("--log-format")
            .arg("json")
            .arg(workload_id);

        let result = self.spawn_command(cmd, "create").await?;
        if let Err(ref output) = result {
            if !output.stderr.is_empty() {
                debug!(workload_id, "runtime create stderr: {}", output.stderr);
            }
        }
        Ok(result)
    }

    async fn run_start(
        &self,
        workload_id: &str,
    ) -> Result<Result<(), CommandOutput>, RuntimeError> {
        let mut cmd = Command::new(&self.config.binary_path);
        cmd.arg("start")
            .arg("--root")
            .arg(&self.config.state_root)
            .arg(workload_id);

        self.spawn_command(cmd, "start").await
    }

    async fn spawn_command(
        &self,
        mut cmd: Command,
        operation: &str,
    ) -> Result<Result<(), CommandOutput>, RuntimeError> {
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::piped());
        debug!(operation = %operation, "executing runtime command: {:?}", cmd);

        match cmd.output().await {
            Ok(output) => {
                if output.status.success() {
                    Ok(Ok(()))
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    Ok(Err(CommandOutput {
                        stderr,
                        exit_code: output.status.code(),
                    }))
                }
            }
            Err(source) => Err(RuntimeError::CommandExecution {
                operation: operation.to_string(),
                source,
            }),
        }
    }

    async fn query_state(&self, workload_id: &str) -> Result<RuntimeState, RuntimeError> {
        let mut cmd = Command::new(&self.config.binary_path);
        cmd.arg("state")
            .arg("--root")
            .arg(&self.config.state_root)
            .arg(workload_id)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = match cmd.output().await {
            Ok(output) => output,
            Err(source) => {
                return Err(RuntimeError::CommandExecution {
                    operation: "state".to_string(),
                    source,
                })
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            warn!(
                workload_id,
                "runtime state command failed: {}",
                stderr.trim()
            );
            return Err(RuntimeError::CommandFailure {
                operation: "state".to_string(),
                exit_code: output.status.code(),
                stderr,
            });
        }

        serde_json::from_slice(&output.stdout)
            .map_err(|source| RuntimeError::StateQueryFailed { source })
    }

    async fn cleanup_after_failure(&self, workload_id: &str) -> Result<(), RuntimeError> {
        let mut cmd = Command::new(&self.config.binary_path);
        cmd.arg("delete")
            .arg("--force")
            .arg("--root")
            .arg(&self.config.state_root)
            .arg(workload_id)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        match cmd.output().await {
            Ok(output) => {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    warn!(
                        workload_id,
                        "runtime delete failed during cleanup: {}",
                        stderr.trim()
                    );
                }
                Ok(())
            }
            Err(source) => {
                warn!(
                    workload_id,
                    error = %source,
                    "failed to execute runtime delete for cleanup"
                );
                Ok(())
            }
        }
    }

    async fn ensure_directory(&self, path: &Path) -> Result<(), RuntimeError> {
        if path.exists() {
            return Ok(());
        }
        fs::create_dir_all(path)
            .await
            .map_err(|source| RuntimeError::Io {
                path: path.to_path_buf(),
                source,
            })
    }
}

#[async_trait]
impl Runtime for ContainerRuntime {
    async fn launch(&self, request: LaunchRequest<'_>) -> Result<LaunchResult, RuntimeError> {
        // UARC V1: Only OCI containers supported
        let bundle_path = self
            .prepare_bundle(request.workload_id, request.spec)
            .await?;
        let log_path = self.prepare_log_path(request.workload_id).await?;
        let pid_file = self
            .config
            .state_root
            .join(format!("{}.pid", request.workload_id));

        self.ensure_directory(&self.config.state_root).await?;

        self.write_manifest_snapshot(request.workload_id, request.manifest_json)
            .await?;

        let mut attempts = 0;
        loop {
            attempts += 1;
            match self
                .create_and_start(request.workload_id, &bundle_path, &pid_file, &log_path)
                .await
            {
                Ok(pid) => {
                    info!(
                        workload_id = request.workload_id,
                        pid, "runtime launched container"
                    );
                    return Ok(LaunchResult {
                        pid: Some(pid),
                        bundle_path: Some(bundle_path),
                        log_path: Some(log_path),
                        port: None,
                    });
                }
                Err(err) => {
                    let (exit_code, stderr_text) = match &err {
                        RuntimeError::CommandFailure {
                            exit_code, stderr, ..
                        } => (*exit_code, Some(stderr.trim().to_string())),
                        _ => (None, None),
                    };

                    let should_retry = matches!(
                        &err,
                        RuntimeError::CommandFailure { stderr, .. }
                            if attempts <= self.config.hook_retry_attempts + 1
                                && hook_related_failure(stderr)
                    );

                    if should_retry {
                        warn!(
                            workload_id = request.workload_id,
                            attempt = attempts,
                            exit_code = ?exit_code,
                            stderr = stderr_text.as_deref().unwrap_or(""),
                            log_path = %log_path.display(),
                            "runtime reported hook failure; retrying"
                        );
                        self.cleanup_after_failure(request.workload_id).await.ok();
                        continue;
                    }

                    error!(
                        workload_id = request.workload_id,
                        attempts,
                        exit_code = ?exit_code,
                        stderr = stderr_text.as_deref().unwrap_or(""),
                        log_path = %log_path.display(),
                        ?err,
                        "runtime launch failed"
                    );

                    self.cleanup_after_failure(request.workload_id).await.ok();
                    return Err(err);
                }
            }
        }
    }

    async fn stop(&self, workload_id: &str) -> Result<(), RuntimeError> {
        // UARC V1: Only OCI containers supported
        let mut cmd = Command::new(&self.config.binary_path);
        cmd.arg("delete")
            .arg("--force")
            .arg("--root")
            .arg(&self.config.state_root)
            .arg(workload_id)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        match cmd.output().await {
            Ok(output) => {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    return Err(RuntimeError::CommandFailure {
                        operation: "delete".to_string(),
                        exit_code: output.status.code(),
                        stderr,
                    });
                }
                Ok(())
            }
            Err(source) => Err(RuntimeError::CommandExecution {
                operation: "delete".to_string(),
                source,
            }),
        }
    }

    fn get_log_path(&self, workload_id: &str) -> Option<PathBuf> {
        Some(self.config.log_dir.join(format!("{}.log", workload_id)))
    }
}

fn hook_related_failure(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("hook") || lower.contains("nvidia")
}

#[derive(Debug, serde::Deserialize)]
struct RuntimeState {
    pid: u32,
}

struct CommandOutput {
    stderr: String,
    exit_code: Option<i32>,
}

impl CommandOutput {
    fn into_error(self, operation: impl Into<String>) -> RuntimeError {
        RuntimeError::CommandFailure {
            operation: operation.into(),
            exit_code: self.exit_code,
            stderr: self.stderr,
        }
    }
}

impl From<RuntimeError> for CommandOutput {
    fn from(err: RuntimeError) -> Self {
        match err {
            RuntimeError::CommandFailure {
                stderr, exit_code, ..
            } => CommandOutput { stderr, exit_code },
            _ => CommandOutput {
                stderr: err.to_string(),
                exit_code: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::RuntimeKind;
    use oci_spec::runtime::{ProcessBuilder, RootBuilder, SpecBuilder};
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Helper to create a minimal valid OCI spec for testing
    fn create_test_spec(root_path: &str) -> Spec {
        let root = RootBuilder::default()
            .path(root_path)
            .build()
            .expect("Failed to build root");

        let process = ProcessBuilder::default()
            .args(vec![
                "sh".to_string(),
                "-c".to_string(),
                "sleep 1".to_string(),
            ])
            .build()
            .expect("Failed to build process");

        SpecBuilder::default()
            .root(root)
            .process(process)
            .build()
            .expect("Failed to build spec")
    }

    #[test]
    fn test_hook_related_failure() {
        assert!(hook_related_failure("Error: hook failed"));
        assert!(hook_related_failure("nvidia hook error"));
        assert!(hook_related_failure("HOOK: failed to execute"));
        assert!(!hook_related_failure("Container failed to start"));
        assert!(!hook_related_failure("Unknown error"));
    }

    #[tokio::test]
    async fn test_container_runtime_new() {
        // Create a minimal runtime config for testing
        let temp_dir = TempDir::new().unwrap();
        let config = RuntimeConfig {
            kind: RuntimeKind::Runc,
            binary_path: PathBuf::from("/usr/bin/runc"),
            bundle_root: temp_dir.path().join("bundles"),
            state_root: temp_dir.path().join("state"),
            log_dir: temp_dir.path().join("logs"),
            hook_retry_attempts: 1,
        };

        let runtime = ContainerRuntime::new(config.clone(), None, None, None);
        assert_eq!(runtime.config().kind, RuntimeKind::Runc);
        assert_eq!(runtime.config().hook_retry_attempts, 1);
    }

    #[tokio::test]
    async fn test_prepare_bundle_creates_directories() {
        let temp_dir = TempDir::new().unwrap();
        let root_path = temp_dir.path().join("rootfs");
        tokio::fs::create_dir_all(&root_path).await.unwrap();

        let config = RuntimeConfig {
            kind: RuntimeKind::Runc,
            binary_path: PathBuf::from("/usr/bin/runc"),
            bundle_root: temp_dir.path().join("bundles"),
            state_root: temp_dir.path().join("state"),
            log_dir: temp_dir.path().join("logs"),
            hook_retry_attempts: 1,
        };

        let runtime = ContainerRuntime::new(config, None, None, None);
        let spec = create_test_spec(root_path.to_str().unwrap());

        let result = runtime.prepare_bundle("test-workload", &spec).await;
        assert!(result.is_ok());

        let bundle_path = result.unwrap();
        assert!(bundle_path.exists());
        assert!(bundle_path.join("config.json").exists());
    }

    #[tokio::test]
    async fn test_prepare_bundle_invalid_rootfs() {
        let temp_dir = TempDir::new().unwrap();
        let nonexistent_root = temp_dir.path().join("nonexistent");

        let config = RuntimeConfig {
            kind: RuntimeKind::Runc,
            binary_path: PathBuf::from("/usr/bin/runc"),
            bundle_root: temp_dir.path().join("bundles"),
            state_root: temp_dir.path().join("state"),
            log_dir: temp_dir.path().join("logs"),
            hook_retry_attempts: 1,
        };

        let runtime = ContainerRuntime::new(config, None, None, None);
        let spec = create_test_spec(nonexistent_root.to_str().unwrap());

        let result = runtime.prepare_bundle("test-workload", &spec).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not exist"));
    }

    #[tokio::test]
    async fn test_prepare_log_path() {
        let temp_dir = TempDir::new().unwrap();
        let config = RuntimeConfig {
            kind: RuntimeKind::Runc,
            binary_path: PathBuf::from("/usr/bin/runc"),
            bundle_root: temp_dir.path().join("bundles"),
            state_root: temp_dir.path().join("state"),
            log_dir: temp_dir.path().join("logs"),
            hook_retry_attempts: 1,
        };

        let runtime = ContainerRuntime::new(config, None, None, None);
        let path = runtime.prepare_log_path("test-workload").await.unwrap();
        assert!(path.to_string_lossy().ends_with("test-workload.log"));
        assert!(path.parent().unwrap().exists());
    }
}
