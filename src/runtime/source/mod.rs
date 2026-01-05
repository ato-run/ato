//! Source Runtime - Hybrid Native/OCI execution for interpreted languages
//!
//! Provides fast development experience (3ms startup) using host toolchains
//! with sandbox isolation, falling back to OCI containers when toolchains
//! are unavailable or in production mode.
//!
//! Platform-specific sandbox implementations:
//! - Linux: bubblewrap (bwrap) namespace isolation
//! - macOS: Alcoholless (preferred) or sandbox-exec (fallback)
//! - Windows: Windows Sandbox (Pro/Enterprise) or Sandboxie Plus (all editions)

mod toolchain;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows;

mod fallback;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::{info, warn};

use crate::runtime::{
    LaunchRequest, LaunchResult, Runtime, RuntimeError, SourceTarget, YoukiRuntimeAdapter,
};

pub use toolchain::{ToolchainInfo, ToolchainManager};

/// Source runtime execution mode
#[derive(Debug, Clone)]
pub enum SourceRuntimeMode {
    /// Native execution with platform sandbox (fast, dev-friendly)
    Native,
    /// OCI container execution (secure, cross-platform fallback)
    Containerized,
}

/// Configuration for SourceRuntime
#[derive(Debug, Clone)]
pub struct SourceRuntimeConfig {
    /// Enable dev mode (prefer native execution)
    pub dev_mode: bool,
    /// Log directory for output capture
    pub log_dir: PathBuf,
    /// State directory for runtime data
    pub state_dir: PathBuf,
}

impl Default for SourceRuntimeConfig {
    fn default() -> Self {
        Self {
            dev_mode: std::env::var("ATO_DEV_MODE")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
            log_dir: PathBuf::from("/tmp/capsuled/logs"),
            state_dir: PathBuf::from("/tmp/capsuled/state"),
        }
    }
}

/// Hybrid Source Runtime supporting both native and containerized execution
pub struct SourceRuntime {
    config: SourceRuntimeConfig,
    toolchain_manager: ToolchainManager,
    oci_fallback: Option<Arc<YoukiRuntimeAdapter>>,
    /// Active workloads (workload_id -> pid)
    active_workloads: std::sync::Mutex<std::collections::HashMap<String, u32>>,
}

impl SourceRuntime {
    /// Create a new SourceRuntime with the given configuration
    pub fn new(config: SourceRuntimeConfig, oci_fallback: Option<Arc<YoukiRuntimeAdapter>>) -> Self {
        Self {
            config,
            toolchain_manager: ToolchainManager::new(),
            oci_fallback,
            active_workloads: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Determine the execution mode for a given source target
    pub fn determine_mode(&self, target: &SourceTarget) -> SourceRuntimeMode {
        // If not in dev mode, always use containerized
        if !self.config.dev_mode {
            return SourceRuntimeMode::Containerized;
        }

        // Check if we have a compatible toolchain
        match self.toolchain_manager.find_toolchain(&target.language, target.version.as_deref()) {
            Some(toolchain) => {
                info!(
                    "Found compatible toolchain: {} {} at {:?}",
                    toolchain.language, toolchain.version, toolchain.path
                );

                // Check if native sandbox is available on this platform
                if Self::is_native_sandbox_available() {
                    SourceRuntimeMode::Native
                } else {
                    warn!("Native sandbox not available, falling back to OCI");
                    SourceRuntimeMode::Containerized
                }
            }
            None => {
                warn!(
                    "No compatible toolchain found for {} {:?}, falling back to OCI",
                    target.language, target.version
                );
                SourceRuntimeMode::Containerized
            }
        }
    }

    /// Check if native sandbox is available on this platform
    #[cfg(target_os = "linux")]
    fn is_native_sandbox_available() -> bool {
        // Check for bubblewrap
        which::which("bwrap").is_ok()
    }

    #[cfg(target_os = "macos")]
    fn is_native_sandbox_available() -> bool {
        // Alcoholless preferred, sandbox-exec always available as fallback
        macos::is_native_available()
    }

    #[cfg(target_os = "windows")]
    fn is_native_sandbox_available() -> bool {
        // Windows Sandbox (Pro/Enterprise) or Sandboxie Plus
        windows::is_native_available()
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    fn is_native_sandbox_available() -> bool {
        false
    }

    /// Get log path for a workload
    fn workload_log_path(&self, workload_id: &str) -> PathBuf {
        self.config.log_dir.join(format!("{}.log", workload_id))
    }

    /// Launch using native sandbox
    /// - Linux: bubblewrap
    /// - macOS: Alcoholless or sandbox-exec
    /// - Windows: Windows Sandbox or Sandboxie Plus
    #[cfg(target_os = "linux")]
    async fn launch_native(
        &self,
        request: &LaunchRequest<'_>,
        target: &SourceTarget,
    ) -> Result<LaunchResult, RuntimeError> {
        linux::launch_with_bubblewrap(self, request, target).await
    }

    #[cfg(target_os = "macos")]
    async fn launch_native(
        &self,
        request: &LaunchRequest<'_>,
        target: &SourceTarget,
    ) -> Result<LaunchResult, RuntimeError> {
        macos::launch_native_macos(self, request, target).await
    }

    #[cfg(target_os = "windows")]
    async fn launch_native(
        &self,
        request: &LaunchRequest<'_>,
        target: &SourceTarget,
    ) -> Result<LaunchResult, RuntimeError> {
        windows::launch_native_windows(self, request, target).await
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    async fn launch_native(
        &self,
        _request: &LaunchRequest<'_>,
        _target: &SourceTarget,
    ) -> Result<LaunchResult, RuntimeError> {
        Err(RuntimeError::SandboxSetupFailed(
            "Native sandbox not supported on this platform. Use OCI fallback.".to_string(),
        ))
    }

    /// Launch using OCI container fallback
    async fn launch_containerized(
        &self,
        request: &LaunchRequest<'_>,
        target: &SourceTarget,
    ) -> Result<LaunchResult, RuntimeError> {
        fallback::launch_with_oci(self, request, target).await
    }
}

#[async_trait]
impl Runtime for SourceRuntime {
    async fn launch(&self, request: LaunchRequest<'_>) -> Result<LaunchResult, RuntimeError> {
        let target = request
            .source_target
            .as_ref()
            .ok_or(RuntimeError::SourceTargetMissing)?;

        info!(
            "Launching source workload: {} (language={}, entrypoint={})",
            request.workload_id, target.language, target.entrypoint
        );

        // Determine execution mode
        let mode = self.determine_mode(target);
        info!("Selected execution mode: {:?}", mode);

        match mode {
            SourceRuntimeMode::Native => self.launch_native(&request, target).await,
            SourceRuntimeMode::Containerized => self.launch_containerized(&request, target).await,
        }
    }

    async fn stop(&self, workload_id: &str) -> Result<(), RuntimeError> {
        info!("Stopping source workload: {}", workload_id);

        // Try to get PID from active workloads
        let pid = {
            let mut workloads = self.active_workloads.lock().unwrap();
            workloads.remove(workload_id)
        };

        if let Some(pid) = pid {
            // Send SIGTERM
            #[cfg(unix)]
            {
                use nix::sys::signal::{kill, Signal};
                use nix::unistd::Pid;
                
                if let Err(e) = kill(Pid::from_raw(pid as i32), Signal::SIGTERM) {
                    warn!("Failed to send SIGTERM to PID {}: {}", pid, e);
                }
            }
            Ok(())
        } else {
            // Workload not found in native tracking, try OCI fallback
            if let Some(ref oci) = self.oci_fallback {
                oci.stop(workload_id).await
            } else {
                warn!("Workload {} not found", workload_id);
                Ok(()) // Idempotent
            }
        }
    }

    fn get_log_path(&self, workload_id: &str) -> Option<PathBuf> {
        let path = self.workload_log_path(workload_id);
        if path.exists() {
            Some(path)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = SourceRuntimeConfig::default();
        // dev_mode should be false by default (unless env var set)
        assert!(!config.dev_mode || std::env::var("ATO_DEV_MODE").is_ok());
    }

    #[test]
    fn test_determine_mode_without_dev_mode() {
        let config = SourceRuntimeConfig {
            dev_mode: false,
            ..Default::default()
        };
        let runtime = SourceRuntime::new(config, None);

        let target = SourceTarget {
            language: "python".to_string(),
            version: Some("3.11".to_string()),
            entrypoint: "main.py".to_string(),
            dependencies: None,
            args: vec![],
            source_dir: PathBuf::from("/tmp"),
        };

        // Without dev_mode, should always be Containerized
        assert!(matches!(
            runtime.determine_mode(&target),
            SourceRuntimeMode::Containerized
        ));
    }

    #[test]
    fn test_workload_log_path() {
        let config = SourceRuntimeConfig {
            log_dir: PathBuf::from("/var/log/capsuled"),
            ..Default::default()
        };
        let runtime = SourceRuntime::new(config, None);

        let path = runtime.workload_log_path("test-123");
        assert_eq!(path, PathBuf::from("/var/log/capsuled/test-123.log"));
    }
}
