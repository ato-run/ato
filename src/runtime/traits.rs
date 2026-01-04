use async_trait::async_trait;
use std::path::PathBuf;

use crate::runtime::{LaunchRequest, LaunchResult, RuntimeError};

/// Unified interface for different runtime backends (Container, Native, etc.)
#[async_trait]
pub trait Runtime: Send + Sync {
    /// Launch a workload
    async fn launch(&self, request: LaunchRequest<'_>) -> Result<LaunchResult, RuntimeError>;

    /// Stop a workload
    async fn stop(&self, workload_id: &str) -> Result<(), RuntimeError>;

    /// Get the log path for a workload
    fn get_log_path(&self, workload_id: &str) -> Option<PathBuf>;
}
