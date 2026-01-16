use async_trait::async_trait;
use std::path::PathBuf;

use crate::runtime::{LaunchRequest, LaunchResult, RuntimeError};

/// Unified interface for runtime backends (Source-only in nacelle).
///
/// All UARC-compliant runtimes must implement this trait to provide a consistent
/// interface for launching, stopping, and monitoring Capsule workloads.
///
/// # Implementations
///
/// - [`crate::runtime::SourceRuntime`]: Interpreted languages with platform sandbox
#[async_trait]
pub trait Runtime: Send + Sync {
    /// Launch a workload with the given configuration.
    ///
    /// This method starts a new instance of the workload and returns information
    /// about the running process (PID, log path, etc.). The workload runs in its
    /// configured sandbox isolation.
    ///
    /// # Arguments
    ///
    /// * `request` - Launch request containing manifest, bundle, and workload metadata
    ///
    /// # Returns
    ///
    /// On success, returns a [`LaunchResult`] with:
    /// - PID of the main process (if applicable)
    /// - Path to the bundle directory
    /// - Path to the log file
    /// - Exposed port (for services)
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] on failure (invalid config, sandbox setup, etc.)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let result = runtime.launch(request).await?;
    /// println!("Started workload with PID: {:?}", result.pid);
    /// ```
    async fn launch(&self, request: LaunchRequest<'_>) -> Result<LaunchResult, RuntimeError>;

    /// Stop a running workload.
    ///
    /// This method gracefully terminates the workload by sending SIGTERM or equivalent.
    /// If the workload does not stop within a timeout, it may be forcefully killed.
    ///
    /// # Arguments
    ///
    /// * `workload_id` - Unique identifier of the workload to stop
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] if the workload cannot be found or stopped.
    /// This method is idempotent (stopping an already-stopped workload is OK).
    async fn stop(&self, workload_id: &str) -> Result<(), RuntimeError>;

    /// Get the log path for a workload.
    ///
    /// Returns the absolute path to the log file where the workload's stdout/stderr
    /// has been captured. Returns `None` if logs are not available or the workload
    /// doesn't exist.
    ///
    /// # Arguments
    ///
    /// * `workload_id` - Unique identifier of the workload
    ///
    /// # Example
    ///
    /// ```ignore
    /// if let Some(log_path) = runtime.get_log_path("my-workload") {
    ///     let content = std::fs::read_to_string(&log_path)?;
    ///     println!("Logs: {}", content);
    /// }
    /// ```
    fn get_log_path(&self, workload_id: &str) -> Option<PathBuf>;
}
