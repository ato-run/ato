//! Runtime implementations for executing Capsules.
//!
//! nacelle is now a **Source Runtime** only. OCI/Wasm routing and execution
//! are handled by ato-cli. This module provides the minimal runtime
//! surface for source workloads.
//!
//! ## UARC V1.1.0 Compliance
//!
//! Runtimes must support the UARC execution interface:
//! - [`Runtime::launch`]: Start a workload with manifest and bundle
//! - [`Runtime::stop`]: Graceful termination with signal/timeout
//! - [`Runtime::get_log_path`]: Access structured logs

use std::path::PathBuf;
use std::sync::Arc;

pub mod source;
pub mod traits;
pub use source::SourceRuntime;
pub use traits::Runtime;

/// Runtime implementation to use for launching workloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeKind {
    /// Source code interpreter runtime (Python, Node.js, Ruby, etc.)
    Source,
}

impl RuntimeKind {
    #[allow(dead_code)]
    fn from_str(input: &str) -> Option<Self> {
        match input.to_ascii_lowercase().as_str() {
            "source" => Some(RuntimeKind::Source),
            _ => None,
        }
    }
}

/// High level request for launching a workload via Source runtime.
#[derive(Debug)]
pub struct LaunchRequest<'a> {
    pub workload_id: &'a str,
    /// Bundle root directory containing the workload files
    pub bundle_root: PathBuf,
    /// Environment variables to pass to the workload
    pub env: Option<Vec<(String, String)>>,
    /// Command line arguments for the workload
    pub args: Option<Vec<String>>,
    /// Source target configuration (for Source runtime)
    pub source_target: Option<SourceTarget>,
    /// Socket manager for socket activation (Phase 2)
    /// When provided, the runtime should pass the socket FD to the child process
    pub socket_manager: Option<Arc<crate::manager::socket::SocketManager>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectedMount {
    pub source: PathBuf,
    pub target: PathBuf,
    pub readonly: bool,
}

/// Source target configuration for Source runtime
#[derive(Debug, Clone)]
pub struct SourceTarget {
    /// Language runtime (python, node, etc.)
    pub language: String,
    /// Version constraint
    pub version: Option<String>,
    /// Entry point file
    pub entrypoint: String,
    /// Dependencies file
    pub dependencies: Option<String>,
    /// Runtime arguments
    pub args: Vec<String>,
    /// Source directory path
    pub source_dir: PathBuf,
    /// Requested process current_dir in the runtime namespace.
    pub requested_cwd: Option<PathBuf>,
    /// Explicit command (Generic Source Runtime)
    /// If specified, overrides language/entrypoint detection
    /// Example: ["ruby", "app.rb"] or ["deno", "run", "main.ts"]
    pub cmd: Option<Vec<String>>,
    /// Development mode flag
    /// When true, security validation is relaxed
    pub dev_mode: bool,
    /// Isolation/Sandbox configuration from capsule.toml
    pub isolation: Option<IsolationPolicy>,
    /// IPC socket paths injected by ato-cli (IPC Broker).
    /// Added to the Sandbox policy read-write allow-list.
    pub ipc_socket_paths: Vec<PathBuf>,
    /// Additional bind mounts injected by ato-cli for one-shot sandbox jobs.
    pub injected_mounts: Vec<InjectedMount>,
}

/// Isolation policy derived from capsule.toml [isolation] section
#[derive(Debug, Clone, Default)]
pub struct IsolationPolicy {
    /// Enable sandbox enforcement
    pub sandbox_enabled: bool,
    /// Paths with read-only access
    pub read_only_paths: Vec<PathBuf>,
    /// Paths with read-write access
    pub read_write_paths: Vec<PathBuf>,
    /// Enable network access
    pub network_enabled: bool,
    /// Allowed egress domains (for R3 domain filtering)
    pub egress_allow: Vec<String>,
}

/// Result details returned after successful launch.
#[derive(Debug, Clone)]
pub struct LaunchResult {
    /// Process ID (None for non-process runtimes)
    pub pid: Option<u32>,
    /// Bundle path (optional)
    pub bundle_path: Option<PathBuf>,
    /// Log file path (optional)
    pub log_path: Option<PathBuf>,
    /// Allocated port (optional, for network services)
    pub port: Option<u16>,
}

/// Errors produced by the runtime launcher.
#[derive(thiserror::Error, Debug)]
pub enum RuntimeError {
    #[error("runtime binary not found. tried: {tried:?}")]
    BinaryNotFound { tried: Vec<String> },

    #[error("invalid runtime configuration: {0}")]
    InvalidConfig(String),

    #[error("I/O error at {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to serialize spec: {0}")]
    SpecSerialization(serde_json::Error),

    #[error("runtime command failed: {operation} (exit={exit_code:?}): {stderr}")]
    CommandFailure {
        operation: String,
        exit_code: Option<i32>,
        stderr: String,
    },

    #[error("runtime command execution error ({operation}): {source}")]
    CommandExecution {
        operation: String,
        #[source]
        source: std::io::Error,
    },

    #[error("runtime state query failed: {source}")]
    StateQueryFailed {
        #[source]
        source: serde_json::Error,
    },

    #[error("internal runtime error: {0}")]
    Internal(String),

    #[error("execution failed: {0}")]
    ExecutionFailed(String),

    #[error("toolchain '{language}' not found on host (version constraint: {version:?})")]
    ToolchainNotFound {
        language: String,
        version: Option<String>,
    },

    #[error("toolchain error: {message}")]
    ToolchainError {
        message: String,
        technical_reason: Option<String>,
        cloud_upsell: Option<String>,
    },

    #[error("sandbox setup failed: {0}")]
    SandboxSetupFailed(String),

    #[error("source target not provided in launch request")]
    SourceTargetMissing,

    #[error("security violation: {0}")]
    SecurityViolation(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_kind_from_str() {
        assert_eq!(RuntimeKind::from_str("source"), Some(RuntimeKind::Source));
        assert_eq!(RuntimeKind::from_str("SOURCE"), Some(RuntimeKind::Source));
        assert_eq!(RuntimeKind::from_str("oci"), None);
    }
}
