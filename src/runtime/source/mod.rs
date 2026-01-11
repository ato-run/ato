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

pub mod toolchain;
mod validator;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows;

mod fallback;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tracing::{info, warn};

use crate::runtime::{LaunchRequest, LaunchResult, Runtime, RuntimeError, SourceTarget};

pub use toolchain::{RuntimeFetcher, ToolchainInfo, ToolchainManager};
pub use validator::{validate_binary, validate_cmd};

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
    /// JIT Provisioning: Downloads runtimes on-demand if not available locally
    runtime_fetcher: Option<RuntimeFetcher>,
    #[allow(dead_code)]
    oci_fallback: Option<()>, // Placeholder - OCI fallback removed in UARC V1.1.0
    /// Active workloads (workload_id -> pid)
    active_workloads: Mutex<HashMap<String, u32>>,
    /// Child process handles - keeps processes alive and allows management
    active_children: Arc<Mutex<HashMap<String, Child>>>,
}

impl SourceRuntime {
    /// Create a new SourceRuntime with the given configuration
    pub fn new(config: SourceRuntimeConfig, _oci_fallback: Option<()>) -> Self {
        // Try to initialize RuntimeFetcher for JIT provisioning
        let runtime_fetcher = match RuntimeFetcher::new() {
            Ok(fetcher) => {
                info!("JIT Provisioning enabled: {:?}", fetcher.cache_dir());
                Some(fetcher)
            }
            Err(e) => {
                warn!("JIT Provisioning disabled: {}", e);
                None
            }
        };

        Self {
            config,
            toolchain_manager: ToolchainManager::new(),
            runtime_fetcher,
            oci_fallback: None,
            active_workloads: Mutex::new(HashMap::new()),
            active_children: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a child process for lifecycle management
    pub fn register_child(&self, workload_id: String, child: Child) {
        let mut children = self.active_children.lock().unwrap();
        children.insert(workload_id, child);
    }

    /// Get a reference to active children for external management
    pub fn active_children(&self) -> Arc<Mutex<HashMap<String, Child>>> {
        Arc::clone(&self.active_children)
    }

    /// Determine the execution mode for a given source target
    pub fn determine_mode(&self, target: &SourceTarget) -> SourceRuntimeMode {
        // If not in dev mode, always use containerized
        if !self.config.dev_mode {
            return SourceRuntimeMode::Containerized;
        }

        // Check if we have a compatible toolchain (local or JIT-provisioned)
        let has_toolchain = self
            .toolchain_manager
            .find_toolchain(&target.language, target.version.as_deref())
            .is_some();

        let has_jit_cached = self
            .runtime_fetcher
            .as_ref()
            .map(|f| {
                f.is_cached(
                    &target.language,
                    target.version.as_deref().unwrap_or("3.11"),
                )
            })
            .unwrap_or(false);

        if has_toolchain || has_jit_cached {
            if has_toolchain {
                info!("Using local toolchain for {}", target.language);
            } else {
                info!("Using JIT-provisioned toolchain for {}", target.language);
            }

            // Check if native sandbox is available on this platform
            if Self::is_native_sandbox_available() {
                SourceRuntimeMode::Native
            } else {
                warn!("Native sandbox not available, falling back to OCI");
                SourceRuntimeMode::Containerized
            }
        } else if self.runtime_fetcher.is_some() {
            // JIT provisioning available - will download on demand
            info!(
                "No local toolchain for {} {:?}, JIT provisioning will download on launch",
                target.language, target.version
            );
            if Self::is_native_sandbox_available() {
                SourceRuntimeMode::Native
            } else {
                SourceRuntimeMode::Containerized
            }
        } else {
            warn!(
                "No compatible toolchain found for {} {:?}, falling back to OCI",
                target.language, target.version
            );
            SourceRuntimeMode::Containerized
        }
    }

    /// Ensure a toolchain is available for the given target, downloading if necessary (JIT provisioning)
    ///
    /// Returns the path to the language binary (e.g., python3)
    pub async fn ensure_toolchain(&self, target: &SourceTarget) -> Result<PathBuf, RuntimeError> {
        // First, check local toolchains
        if let Some(toolchain) = self
            .toolchain_manager
            .find_toolchain(&target.language, target.version.as_deref())
        {
            info!("Using local toolchain: {:?}", toolchain.path);
            return Ok(toolchain.path);
        }

        // Try JIT provisioning
        if let Some(ref fetcher) = self.runtime_fetcher {
            let version = target.version.as_deref().unwrap_or("3.11");

            match target.language.to_lowercase().as_str() {
                "python" => {
                    info!(
                        "JIT Provisioning: Ensuring Python {} is available...",
                        version
                    );
                    let python_path = fetcher.ensure_python(version).await.map_err(|e| {
                        RuntimeError::ToolchainNotFound {
                            language: target.language.clone(),
                            version: Some(format!("{} (JIT failed: {})", version, e)),
                        }
                    })?;
                    return Ok(python_path);
                }
                "node" | "nodejs" => {
                    // TODO: Implement Node.js JIT provisioning
                    return Err(RuntimeError::ToolchainNotFound {
                        language: target.language.clone(),
                        version: Some("JIT provisioning not yet implemented".to_string()),
                    });
                }
                _ => {
                    return Err(RuntimeError::ToolchainNotFound {
                        language: target.language.clone(),
                        version: Some("JIT provisioning not supported".to_string()),
                    });
                }
            }
        }

        Err(RuntimeError::ToolchainNotFound {
            language: target.language.clone(),
            version: target.version.clone(),
        })
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

        // Security validation for Generic Source Runtime
        if let Some(ref cmd) = target.cmd {
            info!("Using explicit command (Generic Source Runtime): {:?}", cmd);

            // Validate binary is in allowlist
            if let Some(binary) = cmd.first() {
                validate_binary(binary, target.dev_mode)
                    .map_err(|e| RuntimeError::SecurityViolation(e.to_string()))?;
            }

            // Validate command arguments (File-First + Dangerous Flags)
            validate_cmd(cmd, &target.source_dir, target.dev_mode)
                .map_err(|e| RuntimeError::SecurityViolation(e.to_string()))?;
        }

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

        // Remove from active workloads
        let _pid = {
            let mut workloads = self.active_workloads.lock().unwrap();
            workloads.remove(workload_id)
        };

        // Kill child process via handle (preferred method)
        let mut child = {
            let mut children = self.active_children.lock().unwrap();
            children.remove(workload_id)
        };

        if let Some(ref mut child) = child {
            info!("Killing child process for workload: {}", workload_id);
            if let Err(e) = child.kill() {
                warn!("Failed to kill child process for {}: {}", workload_id, e);
            }
            // Wait to collect status and prevent zombie
            let _ = child.wait();
            return Ok(());
        }

        // Fallback: signal by PID if we only have PID
        if let Some(_pid) = _pid {
            #[cfg(unix)]
            {
                use nix::sys::signal::{kill, Signal};
                use nix::unistd::Pid;

                if let Err(e) = kill(Pid::from_raw(_pid as i32), Signal::SIGTERM) {
                    warn!("Failed to send SIGTERM to PID {}: {}", _pid, e);
                }
            }
        } else {
            warn!("Workload {} not found", workload_id);
        }

        Ok(()) // Idempotent
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
            cmd: None,
            dev_mode: false,
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
