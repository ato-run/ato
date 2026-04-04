//! Source Runtime - Native execution for interpreted languages
//!
//! Provides fast development experience (3ms startup) using host toolchains
//! with sandbox isolation. OCI fallback is removed; ato-cli is responsible
//! for routing to other runtimes.
//!
//! Platform-specific sandbox implementations:
//! - Linux: bubblewrap (bwrap) namespace isolation
//! - macOS: sandbox-exec Seatbelt sandboxing
//! - Windows: Windows Sandbox (Pro/Enterprise) or Sandboxie Plus (all editions)

pub mod toolchain;
mod validator;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tracing::{info, warn};

use crate::launcher::{LaunchRequest, LaunchResult, Runtime, RuntimeError, SourceTarget};

pub use toolchain::{RuntimeFetcher, ToolchainInfo, ToolchainManager};
pub use validator::{validate_binary, validate_cmd};

/// Async child process handle for supervisor mode
pub type AsyncChild = tokio::process::Child;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeSandboxCapabilityReport {
    pub backends: Vec<String>,
    pub ipc_sandbox: bool,
}

/// Source runtime execution mode
#[derive(Debug, Clone)]
pub enum SourceRuntimeMode {
    /// Native execution with platform sandbox (fast, dev-friendly)
    Native,
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
    /// Sidecar (SOCKS5 proxy) configuration
    pub sidecar_config: Option<SidecarConfig>,
}

/// Sidecar proxy configuration
#[derive(Debug, Clone)]
pub struct SidecarConfig {
    /// SOCKS5 proxy port (e.g., 1080)
    pub socks_port: u16,
    /// Additional hosts to exclude from proxy (comma-separated)
    pub no_proxy: Vec<String>,
}

impl Default for SourceRuntimeConfig {
    fn default() -> Self {
        Self {
            dev_mode: std::env::var("ATO_DEV_MODE")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
            log_dir: PathBuf::from("/tmp/nacelle/logs"),
            state_dir: PathBuf::from("/tmp/nacelle/state"),
            sidecar_config: None,
        }
    }
}

/// Source Runtime supporting native sandbox execution
pub struct SourceRuntime {
    config: SourceRuntimeConfig,
    toolchain_manager: ToolchainManager,
    /// JIT Provisioning: Downloads runtimes on-demand if not available locally
    runtime_fetcher: Option<RuntimeFetcher>,
    /// Active workloads (workload_id -> pid)
    active_workloads: Mutex<HashMap<String, u32>>,
    /// Child process handles - keeps processes alive and allows management (sync, for sandbox modes)
    active_children: Arc<Mutex<HashMap<String, Child>>>,
    /// Async child process handles for dev mode (tokio::process::Child)
    async_children: Arc<tokio::sync::Mutex<HashMap<String, AsyncChild>>>,
}

impl SourceRuntime {
    pub fn supported_languages() -> Vec<String> {
        canonical_supported_languages()
    }

    #[cfg(target_os = "linux")]
    pub fn native_sandbox_capability_report() -> NativeSandboxCapabilityReport {
        let bubblewrap_available = linux::verify_bubblewrap_available().is_ok();
        linux_capability_report(
            bubblewrap_available,
            crate::system::sandbox::linux::is_landlock_supported(),
        )
    }

    #[cfg(target_os = "macos")]
    pub fn native_sandbox_capability_report() -> NativeSandboxCapabilityReport {
        macos_capability_report(macos::is_seatbelt_available())
    }

    #[cfg(target_os = "windows")]
    pub fn native_sandbox_capability_report() -> NativeSandboxCapabilityReport {
        windows_capability_report(
            windows::is_windows_sandbox_available(),
            windows::is_sandboxie_available(),
        )
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    pub fn native_sandbox_capability_report() -> NativeSandboxCapabilityReport {
        NativeSandboxCapabilityReport::default()
    }

    /// Create a new SourceRuntime with the given configuration
    pub fn new(config: SourceRuntimeConfig) -> Self {
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
            active_workloads: Mutex::new(HashMap::new()),
            active_children: Arc::new(Mutex::new(HashMap::new())),
            async_children: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Register a child process for lifecycle management
    pub fn register_child(&self, workload_id: String, child: Child) {
        let mut children = self.active_children.lock().unwrap();
        children.insert(workload_id, child);
    }

    /// Register an async child process for dev mode
    pub async fn register_async_child(&self, workload_id: String, child: AsyncChild) {
        let mut children = self.async_children.lock().await;
        children.insert(workload_id, child);
    }

    /// Take an async child for waiting (removes from registry)
    pub async fn take_async_child(&self, workload_id: &str) -> Option<AsyncChild> {
        let mut children = self.async_children.lock().await;
        children.remove(workload_id)
    }

    pub fn take_child(&self, workload_id: &str) -> Option<Child> {
        let mut children = self.active_children.lock().unwrap();
        children.remove(workload_id)
    }

    /// Get a reference to active children for external management
    pub fn active_children(&self) -> Arc<Mutex<HashMap<String, Child>>> {
        Arc::clone(&self.active_children)
    }

    /// Apply sidecar (SOCKS5 proxy) environment variables to a command
    ///
    /// This ensures all network traffic from the child process goes through
    /// the sidecar proxy, enabling network isolation and monitoring.
    pub fn apply_sidecar_env(&self, cmd: &mut std::process::Command) {
        if let Some(ref sidecar) = self.config.sidecar_config {
            let proxy_url = format!("socks5h://127.0.0.1:{}", sidecar.socks_port);
            cmd.env("HTTP_PROXY", &proxy_url);
            cmd.env("HTTPS_PROXY", &proxy_url);
            cmd.env("ALL_PROXY", &proxy_url);

            // Build NO_PROXY list: localhost + configured exclusions + capsule internal hosts
            let mut no_proxy = vec![
                "localhost".to_string(),
                "127.0.0.1".to_string(),
                "::1".to_string(),
            ];
            no_proxy.extend(sidecar.no_proxy.clone());

            // Add capsule internal service endpoints
            no_proxy.push(".local".to_string());

            cmd.env("NO_PROXY", no_proxy.join(","));
            cmd.env("no_proxy", no_proxy.join(","));

            info!("Applied SOCKS5 proxy {} to command env", proxy_url);
        }
    }

    /// Check if sidecar is configured and active
    pub fn is_sidecar_configured(&self) -> bool {
        self.config.sidecar_config.is_some()
    }

    /// Determine the execution mode for a given source target
    pub fn determine_mode(&self, target: &SourceTarget) -> Result<SourceRuntimeMode, RuntimeError> {
        // Log toolchain availability for debugging
        if self
            .toolchain_manager
            .find_toolchain(&target.language, target.version.as_deref())
            .is_some()
        {
            info!("Using local toolchain for {}", target.language);
        } else if let Some(fetcher) = self.runtime_fetcher.as_ref() {
            if target
                .version
                .as_deref()
                .map(|v| fetcher.is_cached(&target.language, v))
                .unwrap_or(false)
            {
                info!("Using JIT-provisioned toolchain for {}", target.language);
            } else {
                info!(
                    "No local toolchain for {} {:?}, JIT provisioning may download on launch",
                    target.language, target.version
                );
            }
        } else {
            warn!(
                "No compatible toolchain found for {} {:?}; launch may fail",
                target.language, target.version
            );
        }

        if !Self::is_native_sandbox_available() {
            return Err(RuntimeError::SandboxSetupFailed(
                "Native sandbox not supported on this platform; OCI fallback removed".to_string(),
            ));
        }

        Ok(SourceRuntimeMode::Native)
    }

    /// Ensure a toolchain is available for the given target, downloading if necessary (JIT provisioning)
    ///
    /// Returns the path to the language binary (e.g., python3)
    pub async fn ensure_toolchain(&self, target: &SourceTarget) -> Result<PathBuf, RuntimeError> {
        info!(
            "ensure_toolchain: language={}, version={:?}",
            target.language, target.version
        );

        // First, check local toolchains
        if let Some(toolchain) = self
            .toolchain_manager
            .find_toolchain(&target.language, target.version.as_deref())
        {
            info!("Using local toolchain: {:?}", toolchain.path);
            return Ok(toolchain.path);
        }

        info!(
            "No local toolchain found for {} {:?}, attempting JIT provisioning",
            target.language, target.version
        );

        // Try JIT provisioning
        if let Some(ref fetcher) = self.runtime_fetcher {
            let result = match target.language.to_lowercase().as_str() {
                "python" => {
                    let version = target.version.as_deref().unwrap_or("3.11");
                    info!("JIT Provisioning: Downloading Python {}...", version);
                    fetcher.ensure_python(version).await
                }
                "node" | "nodejs" => {
                    let version = target.version.as_deref().unwrap_or("22");
                    info!("JIT Provisioning: Downloading Node {}...", version);
                    fetcher.ensure_node(version).await
                }
                "deno" => {
                    let version = match target.version.as_deref() {
                        Some(v) => v,
                        None => {
                            return Err(RuntimeError::ToolchainError {
                                message: "Deno version is required for JIT provisioning".to_string(),
                                technical_reason: Some("Version constraint is missing".to_string()),
                                cloud_upsell: Some(
                                    "💡 This app requires Deno runtime. Try specifying a version in capsule.toml, or run with a cloud environment (Pro plan).".to_string(),
                                ),
                            });
                        }
                    };
                    info!("JIT Provisioning: Downloading Deno {}...", version);
                    fetcher.ensure_deno(version).await
                }
                "bun" => {
                    let version = match target.version.as_deref() {
                        Some(v) => v,
                        None => {
                            return Err(RuntimeError::ToolchainError {
                                message: "Bun version is required for JIT provisioning".to_string(),
                                technical_reason: Some("Version constraint is missing".to_string()),
                                cloud_upsell: Some(
                                    "💡 This app requires Bun runtime. Try specifying a version in capsule.toml, or run with a cloud environment (Pro plan).".to_string(),
                                ),
                            });
                        }
                    };
                    info!("JIT Provisioning: Downloading Bun {}...", version);
                    fetcher.ensure_bun(version).await
                }
                _ => {
                    return Err(RuntimeError::ToolchainError {
                        message: format!(
                            "JIT provisioning not supported for language: {}",
                            target.language
                        ),
                        technical_reason: Some(
                            "Unsupported language for local JIT provisioning".to_string(),
                        ),
                        cloud_upsell: Some(
                            "💡 This app requires a runtime that needs cloud environment. \
                             Run with 'ato run --mode=cloud' (Pro plan) to execute in a managed Linux VM."
                                .to_string(),
                        ),
                    });
                }
            };

            return result.map_err(|e| {
                let error_msg = format!("{}", e);
                let technical_reason = if error_msg.contains("glibc") {
                    Some(format!(
                        "glibc version mismatch: {} - the runtime requires a newer glibc version than available on this system",
                        error_msg
                    ))
                } else if error_msg.contains("Unsupported") && error_msg.contains("platform") {
                    Some(format!(
                        "Platform not supported: {} - this runtime is not available for your OS/architecture",
                        error_msg
                    ))
                } else if error_msg.contains("network") || error_msg.contains("connection") {
                    Some("Network error: Unable to download runtime".to_string())
                } else if error_msg.contains("timeout") {
                    Some("Timeout: Runtime download took too long".to_string())
                } else {
                    Some(format!("JIT provisioning failed: {}", error_msg))
                };

                RuntimeError::ToolchainError {
                    message: format!(
                        "Failed to provision {} runtime",
                        target.language
                    ),
                    technical_reason,
                    cloud_upsell: Some(
                        "💡 This app requires a cloud environment. Run with '--mode=cloud' (Pro) to execute in a managed Linux VM with guaranteed compatibility."
                            .to_string(),
                    ),
                }
            });
        }

        Err(RuntimeError::ToolchainError {
            message: format!(
                "{} runtime not found on host",
                target.language
            ),
            technical_reason: Some(
                format!(
                    "No local {} installation found (version: {:?})",
                    target.language, target.version
                )
                .to_string(),
            ),
            cloud_upsell: Some(
                "💡 This app requires a cloud environment. Run with '--mode=cloud' (Pro) to execute in a managed Linux VM with guaranteed compatibility."
                    .to_string(),
            ),
        })
    }

    /// Check if native sandbox is available on this platform
    #[cfg(target_os = "linux")]
    fn is_native_sandbox_available() -> bool {
        !Self::native_sandbox_capability_report().backends.is_empty()
    }

    #[cfg(target_os = "macos")]
    fn is_native_sandbox_available() -> bool {
        !Self::native_sandbox_capability_report().backends.is_empty()
    }

    #[cfg(target_os = "windows")]
    fn is_native_sandbox_available() -> bool {
        !Self::native_sandbox_capability_report().backends.is_empty()
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
    /// - macOS: sandbox-exec
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
            "Native sandbox not supported on this platform.".to_string(),
        ))
    }

    // OCI fallback removed; only native sandbox is supported.
}

fn canonical_supported_languages() -> Vec<String> {
    vec![
        "bun".to_string(),
        "deno".to_string(),
        "node".to_string(),
        "python".to_string(),
    ]
}

#[cfg(any(test, target_os = "linux"))]
fn linux_capability_report(
    bubblewrap_available: bool,
    landlock_supported: bool,
) -> NativeSandboxCapabilityReport {
    if !bubblewrap_available {
        return NativeSandboxCapabilityReport::default();
    }

    let mut backends = vec!["linux-bwrap".to_string()];
    if landlock_supported {
        backends.push("linux-landlock".to_string());
    }

    NativeSandboxCapabilityReport {
        backends,
        ipc_sandbox: true,
    }
}

#[cfg(any(test, target_os = "macos"))]
fn macos_capability_report(seatbelt_available: bool) -> NativeSandboxCapabilityReport {
    if !seatbelt_available {
        return NativeSandboxCapabilityReport::default();
    }

    NativeSandboxCapabilityReport {
        backends: vec!["macos-seatbelt".to_string()],
        ipc_sandbox: seatbelt_available,
    }
}

#[cfg(any(test, target_os = "windows"))]
fn windows_capability_report(
    windows_sandbox_available: bool,
    sandboxie_available: bool,
) -> NativeSandboxCapabilityReport {
    if !windows_sandbox_available && !sandboxie_available {
        return NativeSandboxCapabilityReport::default();
    }

    let mut backends = Vec::new();
    if windows_sandbox_available {
        backends.push("windows-sandbox".to_string());
    }
    if sandboxie_available {
        backends.push("windows-sandboxie".to_string());
    }

    NativeSandboxCapabilityReport {
        backends,
        ipc_sandbox: false,
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
        let mode = self.determine_mode(target)?;
        info!("Selected execution mode: {:?}", mode);

        match mode {
            SourceRuntimeMode::Native => self.launch_native(&request, target).await,
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
    use std::collections::BTreeSet;

    #[test]
    fn test_default_config() {
        let config = SourceRuntimeConfig::default();
        // dev_mode should be false by default (unless env var set)
        assert!(!config.dev_mode || std::env::var("ATO_DEV_MODE").is_ok());
    }

    #[test]
    fn test_workload_log_path() {
        let config = SourceRuntimeConfig {
            log_dir: PathBuf::from("/var/log/nacelle"),
            ..Default::default()
        };
        let runtime = SourceRuntime::new(config);

        let path = runtime.workload_log_path("test-123");
        assert_eq!(path, PathBuf::from("/var/log/nacelle/test-123.log"));
    }

    #[test]
    fn supported_languages_match_current_contract() {
        let languages = SourceRuntime::supported_languages()
            .into_iter()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            languages,
            BTreeSet::from([
                "bun".to_string(),
                "deno".to_string(),
                "node".to_string(),
                "python".to_string(),
            ])
        );
    }

    #[test]
    fn linux_capability_report_fails_closed_without_bwrap() {
        assert_eq!(
            linux_capability_report(false, true),
            NativeSandboxCapabilityReport::default()
        );
    }

    #[test]
    fn linux_capability_report_includes_landlock_only_when_supported() {
        assert_eq!(
            linux_capability_report(true, false),
            NativeSandboxCapabilityReport {
                backends: vec!["linux-bwrap".to_string()],
                ipc_sandbox: true,
            }
        );
        assert_eq!(
            linux_capability_report(true, true),
            NativeSandboxCapabilityReport {
                backends: vec!["linux-bwrap".to_string(), "linux-landlock".to_string()],
                ipc_sandbox: true,
            }
        );
    }

    #[test]
    fn macos_capability_report_requires_real_backend() {
        assert_eq!(
            macos_capability_report(false),
            NativeSandboxCapabilityReport::default()
        );
        assert_eq!(
            macos_capability_report(true),
            NativeSandboxCapabilityReport {
                backends: vec!["macos-seatbelt".to_string()],
                ipc_sandbox: true,
            }
        );
    }

    #[test]
    fn windows_capability_report_requires_real_backend() {
        assert_eq!(
            windows_capability_report(false, false),
            NativeSandboxCapabilityReport::default()
        );
        assert_eq!(
            windows_capability_report(true, false),
            NativeSandboxCapabilityReport {
                backends: vec!["windows-sandbox".to_string()],
                ipc_sandbox: false,
            }
        );
        assert_eq!(
            windows_capability_report(false, true),
            NativeSandboxCapabilityReport {
                backends: vec!["windows-sandboxie".to_string()],
                ipc_sandbox: false,
            }
        );
    }
}
