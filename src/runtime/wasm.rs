//! Wasm Runtime implementation using Wasmtime Component Model.
//!
//! This module provides UARC V1.1.0 compliant WebAssembly execution via
//! Wasmtime's Component Model API. It supports `wasi:cli/command` world
//! with resource limiting, async execution, and log file redirection.
//!
//! # Key Features
//! - Component Model API (wasmtime::component::Component)
//! - `wasi:cli/command` world support via wasmtime-wasi preview2
//! - Resource limiting (512MB memory, 10k table elements by default)
//! - Async execution (Config::async_support(true))
//! - Log file redirection (stdout/stderr → log files, NOT inherit_stdio)

use crate::artifact::ArtifactManager;
use crate::runtime::{LaunchRequest, LaunchResult, Runtime, RuntimeError};
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use wasmtime::{
    component::{Component, Linker},
    Config, Engine, Store, StoreLimits, StoreLimitsBuilder,
};
use wasmtime_wasi::preview2::{
    command::{self, Command},
    pipe::MemoryOutputPipe,
    Table, WasiCtx, WasiCtxBuilder, WasiView,
};

/// Default memory limit for Wasm components: 512 MB
const DEFAULT_MEMORY_LIMIT: usize = 512 * 1024 * 1024;

/// Default table elements limit for Wasm components
const DEFAULT_TABLE_ELEMENTS: u32 = 10_000;

/// Default instance limit for Wasm components
const DEFAULT_INSTANCE_LIMIT: usize = 10;

/// Engine state for a single Wasm component execution.
///
/// Holds the WASI context, resource table, and store limits.
/// Implements `WasiView` for integration with wasmtime-wasi.
struct WasmEngineState {
    ctx: WasiCtx,
    table: Table,
    limits: StoreLimits,
}

impl WasiView for WasmEngineState {
    fn table(&self) -> &Table {
        &self.table
    }

    fn table_mut(&mut self) -> &mut Table {
        &mut self.table
    }

    fn ctx(&self) -> &WasiCtx {
        &self.ctx
    }

    fn ctx_mut(&mut self) -> &mut WasiCtx {
        &mut self.ctx
    }
}

/// Wasm Runtime using Wasmtime Component Model.
///
/// Provides UARC V1.1.0 compliant execution environment with:
/// - Component Model API (not legacy Module API)
/// - `wasi:cli/command` world support
/// - Resource limiting (memory, tables, instances)
/// - Async execution (non-blocking Tokio runtime)
/// - Log file redirection (stdout/stderr → log files)
pub struct WasmRuntime {
    engine: Engine,
    _artifact_manager: Option<Arc<ArtifactManager>>,
    log_dir: PathBuf,
    egress_proxy_port: Option<u16>,
}

impl WasmRuntime {
    /// Create a new WasmRuntime with Component Model support.
    ///
    /// # Arguments
    /// * `artifact_manager` - Optional artifact manager for CAS integration
    /// * `log_dir` - Directory for log files
    /// * `egress_proxy_port` - Optional egress proxy port for network access
    pub fn new(
        artifact_manager: Option<Arc<ArtifactManager>>,
        log_dir: PathBuf,
        egress_proxy_port: Option<u16>,
    ) -> Result<Self, RuntimeError> {
        // Create engine config with async and component model support
        let mut config = Config::new();
        config.async_support(true);
        config.wasm_component_model(true);

        let engine = Engine::new(&config).map_err(|e| {
            RuntimeError::Internal(format!("Failed to create Wasmtime engine: {}", e))
        })?;

        info!(
            "WasmRuntime initialized with Component Model support (log_dir: {:?}, egress_proxy: {:?})",
            log_dir, egress_proxy_port
        );

        Ok(Self {
            engine,
            _artifact_manager: artifact_manager,
            log_dir,
            egress_proxy_port,
        })
    }

    /// Build environment variables for the Wasm component.
    fn build_env_vars(&self, request: &LaunchRequest<'_>) -> Vec<(String, String)> {
        let mut env_vars: Vec<(String, String)> = Vec::new();

        // Add user-specified environment variables
        if let Some(envs) = &request.env {
            for (k, v) in envs.iter() {
                env_vars.push((k.clone(), v.clone()));
            }
        }

        // Add egress proxy configuration if available
        if let Some(port) = self.egress_proxy_port {
            let proxy_url = format!("http://127.0.0.1:{}", port);
            env_vars.push(("HTTP_PROXY".to_string(), proxy_url.clone()));
            env_vars.push(("HTTPS_PROXY".to_string(), proxy_url));
        }

        env_vars
    }

    /// Create store limits for resource limiting.
    fn create_store_limits() -> StoreLimits {
        StoreLimitsBuilder::new()
            .memory_size(DEFAULT_MEMORY_LIMIT)
            .table_elements(DEFAULT_TABLE_ELEMENTS)
            .instances(DEFAULT_INSTANCE_LIMIT)
            .build()
    }
}

#[async_trait]
impl Runtime for WasmRuntime {
    async fn launch(&self, request: LaunchRequest<'_>) -> Result<LaunchResult, RuntimeError> {
        let workload_id = request.workload_id;
        let start_time = std::time::Instant::now();

        debug!(
            "WasmRuntime launching workload: {} (bundle_root: {:?})",
            workload_id, request.bundle_root
        );

        // 1. Resolve component path
        let component_path = if let Some(wasm_path) = &request.wasm_component_path {
            wasm_path.clone()
        } else {
            // Default: look for main.wasm in bundle root
            request.bundle_root.join("main.wasm")
        };

        if !component_path.exists() {
            return Err(RuntimeError::InvalidConfig(format!(
                "Wasm component not found: {:?}",
                component_path
            )));
        }

        info!(
            "Loading Wasm component: {:?} for workload: {}",
            component_path, workload_id
        );

        // 2. Load component from file
        let component = Component::from_file(&self.engine, &component_path).map_err(|e| {
            error!("Failed to load component {:?}: {}", component_path, e);
            RuntimeError::ExecutionFailed(format!("Failed to load Wasm component: {}", e))
        })?;

        // 3. Create linker with WASI command world
        let mut linker: Linker<WasmEngineState> = Linker::new(&self.engine);
        command::add_to_linker(&mut linker)
            .map_err(|e| RuntimeError::Internal(format!("Failed to add WASI to linker: {}", e)))?;

        // 4. Build environment variables
        let env_vars = self.build_env_vars(&request);

        // 5. Create log output pipe (stdout/stderr → memory, not inherit_stdio)
        let stdout_pipe = MemoryOutputPipe::new(1024 * 1024); // 1MB buffer
        let stderr_pipe = MemoryOutputPipe::new(1024 * 1024); // 1MB buffer

        // 6. Build WASI context with log redirection
        let mut wasi_builder = WasiCtxBuilder::new();

        // Add environment variables
        for (k, v) in &env_vars {
            wasi_builder.env(k, v);
        }

        // Add arguments (workload_id as argv[0])
        wasi_builder.arg(workload_id);
        if let Some(args) = &request.args {
            for arg in args {
                wasi_builder.arg(arg);
            }
        }

        // Redirect stdout/stderr to memory pipes (NOT inherit_stdio per spec)
        wasi_builder.stdout(stdout_pipe.clone());
        wasi_builder.stderr(stderr_pipe.clone());

        let wasi_ctx = wasi_builder.build();

        // 7. Create resource table and store limits
        let table = Table::new();
        let limits = Self::create_store_limits();

        // 8. Create engine state
        let state = WasmEngineState {
            ctx: wasi_ctx,
            table,
            limits: limits.clone(),
        };

        // 9. Create store with resource limiter
        let mut store = Store::new(&self.engine, state);
        store.limiter(|state| &mut state.limits);

        // 10. Instantiate and run the component
        let (command, _instance) = Command::instantiate_async(&mut store, &component, &linker)
            .await
            .map_err(|e| {
                error!("Failed to instantiate component: {}", e);
                RuntimeError::ExecutionFailed(format!("Component instantiation failed: {}", e))
            })?;

        debug!("Component instantiated, executing wasi:cli/command::run");

        // 11. Execute wasi:cli/command::run
        let run_result = command.wasi_cli_run().call_run(&mut store).await;

        let elapsed = start_time.elapsed();

        // 12. Collect output logs
        let stdout_bytes = stdout_pipe.contents();
        let stderr_bytes = stderr_pipe.contents();

        // Write logs to file
        let log_path = self.log_dir.join(format!("{}.log", workload_id));
        if let Err(e) = self
            .write_logs(&log_path, &stdout_bytes, &stderr_bytes)
            .await
        {
            warn!("Failed to write logs for {}: {}", workload_id, e);
        }

        // 13. Handle execution result
        match run_result {
            Ok(Ok(())) => {
                info!(
                    "Wasm component {} completed successfully in {:?}",
                    workload_id, elapsed
                );
                Ok(LaunchResult {
                    pid: None,         // Wasm components don't have traditional PIDs
                    bundle_path: None, // Wasm components don't use bundle directories
                    log_path: Some(log_path),
                    port: None,
                })
            }
            Ok(Err(())) => {
                // The component returned an error via wasi:cli/run
                warn!(
                    "Wasm component {} exited with error in {:?}",
                    workload_id, elapsed
                );
                Err(RuntimeError::ExecutionFailed(format!(
                    "Component {} exited with error",
                    workload_id
                )))
            }
            Err(e) => {
                // Runtime trap or other error
                error!(
                    "Wasm component {} trapped in {:?}: {}",
                    workload_id, elapsed, e
                );
                Err(RuntimeError::ExecutionFailed(format!(
                    "Component {} trapped: {}",
                    workload_id, e
                )))
            }
        }
    }

    async fn stop(&self, workload_id: &str) -> Result<(), RuntimeError> {
        // Wasm components are typically short-lived and run synchronously in async context.
        // Stopping is handled by dropping the store/future.
        // For long-running components, we would need CancellationToken integration.
        info!("Stop requested for Wasm component: {}", workload_id);
        Ok(())
    }

    fn get_log_path(&self, workload_id: &str) -> Option<PathBuf> {
        Some(self.log_dir.join(format!("{}.log", workload_id)))
    }
}

impl WasmRuntime {
    /// Write stdout and stderr logs to a file.
    async fn write_logs(
        &self,
        log_path: &PathBuf,
        stdout: &[u8],
        stderr: &[u8],
    ) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;

        // Ensure log directory exists
        if let Some(parent) = log_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let mut file = tokio::fs::File::create(log_path).await?;

        if !stdout.is_empty() {
            file.write_all(b"=== STDOUT ===\n").await?;
            file.write_all(stdout).await?;
            file.write_all(b"\n").await?;
        }

        if !stderr.is_empty() {
            file.write_all(b"=== STDERR ===\n").await?;
            file.write_all(stderr).await?;
            file.write_all(b"\n").await?;
        }

        file.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_wasm_runtime_creation() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path().to_path_buf();

        let runtime = WasmRuntime::new(None, log_dir.clone(), None);
        assert!(runtime.is_ok());
    }

    #[test]
    fn test_wasm_runtime_with_egress_proxy() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path().to_path_buf();

        let runtime = WasmRuntime::new(None, log_dir.clone(), Some(8080));
        assert!(runtime.is_ok());
    }

    #[test]
    fn test_log_path() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path().to_path_buf();

        let runtime = WasmRuntime::new(None, log_dir.clone(), None).unwrap();
        let log_path = runtime.get_log_path("test-workload");

        assert!(log_path.is_some());
        assert_eq!(log_path.unwrap(), log_dir.join("test-workload.log"));
    }

    #[test]
    fn test_store_limits() {
        let limits = WasmRuntime::create_store_limits();
        // StoreLimits doesn't expose getters, but we can verify it was created
        // Using _ to silence unused variable warning
        let _ = limits;
    }

    #[test]
    fn test_default_constants() {
        assert_eq!(DEFAULT_MEMORY_LIMIT, 512 * 1024 * 1024);
        assert_eq!(DEFAULT_TABLE_ELEMENTS, 10_000);
        assert_eq!(DEFAULT_INSTANCE_LIMIT, 10);
    }
}
