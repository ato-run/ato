//! Runtime implementations for executing Capsules.
//!
//! This module provides multiple runtime backends for executing Capsule workloads:
//! - **WasmRuntime**: WebAssembly execution via Wasmtime Component Model
//! - **SourceRuntime**: Interpreted languages (Python, Node.js, Ruby) with native sandbox
//! - **ContainerRuntime**: OCI container execution (runc, youki)
//! - **DockerCliRuntime**: Docker CLI wrapper (development)
//! - **DevRuntime**: In-process development runtime with hot-reload
//!
//! ## Runtime Selection
//!
//! The [`resolve_runtime`] function selects the appropriate runtime based on the
//! workload target. For best performance and compatibility:
//! - Wasm components → WasmRuntime
//! - Python/Node/Ruby → SourceRuntime (dev mode) or ContainerRuntime (production)
//! - OCI images → ContainerRuntime or DockerCliRuntime
//!
//! ## Security Model
//!
//! All runtimes implement the [`Runtime`] trait with strict isolation:
//! - **Wasm**: Component Model sandbox, memory/table limits, WASI preview2
//! - **Source**: Platform-specific sandbox (bubblewrap/sandbox-exec/Windows Sandbox)
//! - **Container**: OCI runtime isolation (cgroups, namespaces)
//!
//! ## UARC V1.1.0 Compliance
//!
//! Runtimes must support the UARC execution interface:
//! - [`Runtime::launch`]: Start a workload with manifest and bundle
//! - [`Runtime::stop`]: Graceful termination with signal/timeout
//! - [`Runtime::get_log_path`]: Access structured logs
//!
//! Native runtime (local binary execution) is NOT supported in UARC V1.1.0.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use oci_spec::runtime::Spec;
use tracing::info;

use crate::config::RuntimeSection;

pub mod container;
pub mod dev;
pub mod docker_cli;
pub mod resolver;
pub mod source;
pub mod traits;
pub mod wasm;
// pub mod youki_adapter; // Disabled: requires capsule_runtime dependency

pub use container::ContainerRuntime;
pub use dev::DevRuntime;
pub use docker_cli::DockerCliRuntime;
pub use resolver::{resolve_runtime, ResolveContext, ResolveError, ResolvedTarget};
pub use source::SourceRuntime;
pub use traits::Runtime;
pub use wasm::WasmRuntime;
// pub use youki_adapter::YoukiRuntimeAdapter; // Disabled

const DEFAULT_HOOK_RETRY_ATTEMPTS: u32 = 1;

/// Runtime implementation to use for launching containers.
///
/// Note: `Native` runtime has been removed as it's not part of UARC V1.
/// Use `Source` runtime for interpreted languages instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeKind {
    /// Youki OCI runtime (preferred for production)
    Youki,
    /// Runc OCI runtime (standard, widely available)
    Runc,
    /// WebAssembly runtime (Wasmtime Component Model)
    Wasm,
    /// Source code interpreter runtime (Python, Node.js, Ruby, etc.)
    Source,
}

impl RuntimeKind {
    fn from_str(input: &str) -> Option<Self> {
        match input.to_ascii_lowercase().as_str() {
            "youki" => Some(RuntimeKind::Youki),
            "runc" => Some(RuntimeKind::Runc),
            "wasm" => Some(RuntimeKind::Wasm),
            "source" => Some(RuntimeKind::Source),
            _ => None,
        }
    }

    fn binary_candidates(self) -> &'static [&'static str] {
        match self {
            RuntimeKind::Youki => &["youki"],
            RuntimeKind::Runc => &["runc"],
            RuntimeKind::Wasm => &[],   // Internal
            RuntimeKind::Source => &[], // Internal (uses host toolchains)
        }
    }
}

/// Concrete runtime configuration after resolving defaults and binary detection.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub kind: RuntimeKind,
    pub binary_path: PathBuf,
    pub bundle_root: PathBuf,
    pub state_root: PathBuf,
    pub log_dir: PathBuf,
    pub hook_retry_attempts: u32,
}

impl RuntimeConfig {
    pub fn from_section(section: Option<&RuntimeSection>) -> Result<Self, RuntimeError> {
        if let Some(s) = section {
            info!(
                "Runtime config section: preferred={:?}, binary_path={:?}",
                s.preferred, s.binary_path
            );
        } else {
            info!("Runtime config section is None");
        }

        let preferred_kind = section
            .and_then(|s| s.preferred.as_deref())
            .and_then(RuntimeKind::from_str);

        let explicit_binary = section
            .and_then(|s| s.binary_path.as_ref())
            .map(PathBuf::from);

        let (kind, binary_path) = match (preferred_kind, explicit_binary) {
            (Some(kind), Some(path)) => {
                if path.exists() {
                    (kind, path)
                } else {
                    return Err(RuntimeError::BinaryNotFound {
                        tried: vec![path.to_string_lossy().into_owned()],
                    });
                }
            }
            (Some(kind), None) => {
                let path = find_binary(kind.binary_candidates())?;
                (kind, path)
            }
            (None, Some(path)) => {
                if !path.exists() {
                    return Err(RuntimeError::BinaryNotFound {
                        tried: vec![path.to_string_lossy().into_owned()],
                    });
                }
                let inferred_kind = infer_kind_from_path(&path).ok_or_else(|| {
                    RuntimeError::InvalidConfig(format!(
                        "Failed to infer runtime kind from binary path {:?}. Specify runtime.preferred",
                        path
                    ))
                })?;
                (inferred_kind, path)
            }
            (None, None) => {
                let order = [RuntimeKind::Youki, RuntimeKind::Runc];
                let mut attempts = Vec::new();
                let mut found = None;
                for kind in order {
                    match find_binary(kind.binary_candidates()) {
                        Ok(path) => {
                            found = Some((kind, path));
                            break;
                        }
                        Err(RuntimeError::BinaryNotFound { tried }) => {
                            attempts.extend(tried);
                        }
                        Err(err) => return Err(err),
                    }
                }
                match found {
                    Some(result) => result,
                    None => {
                        return Err(RuntimeError::BinaryNotFound { tried: attempts });
                    }
                }
            }
        };

        let base_dir = resolve_base_dir(section);
        let bundle_root = section
            .and_then(|s| s.bundle_root.as_ref())
            .map(PathBuf::from)
            .unwrap_or_else(|| base_dir.join("bundles"));
        let state_root = section
            .and_then(|s| s.state_root.as_ref())
            .map(PathBuf::from)
            .unwrap_or_else(|| base_dir.join("state"));
        let log_dir = section
            .and_then(|s| s.log_dir.as_ref())
            .map(PathBuf::from)
            .unwrap_or_else(|| base_dir.join("logs"));

        let hook_retry_attempts = section
            .and_then(|s| s.hook_retry_attempts)
            .unwrap_or(DEFAULT_HOOK_RETRY_ATTEMPTS);

        Ok(Self {
            kind,
            binary_path,
            bundle_root,
            state_root,
            log_dir,
            hook_retry_attempts,
        })
    }
}

fn resolve_base_dir(section: Option<&RuntimeSection>) -> PathBuf {
    if let Some(dir) = section.and_then(|s| s.bundle_root.as_ref()) {
        if let Some(parent) = Path::new(dir).parent() {
            return parent.to_path_buf();
        }
    }

    std::env::var("CAPSULED_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("capsuled"))
}

fn find_binary(candidates: &[&str]) -> Result<PathBuf, RuntimeError> {
    let mut tried = Vec::new();
    for candidate in candidates {
        match which::which(candidate) {
            Ok(path) => return Ok(path),
            Err(_) => tried.push(candidate.to_string()),
        }
    }
    Err(RuntimeError::BinaryNotFound { tried })
}

fn infer_kind_from_path(path: &Path) -> Option<RuntimeKind> {
    let file_name = path.file_stem().and_then(OsStr::to_str)?;

    RuntimeKind::from_str(file_name)
}

/// High level request for launching a workload via OCI runtime.
#[derive(Debug)]
pub struct LaunchRequest<'a> {
    pub workload_id: &'a str,
    pub spec: &'a Spec,
    pub manifest_json: Option<&'a str>,
    /// Bundle root directory containing the workload files
    pub bundle_root: PathBuf,
    /// Environment variables to pass to the workload
    pub env: Option<Vec<(String, String)>>,
    /// Command line arguments for the workload
    pub args: Option<Vec<String>>,
    /// Path to the Wasm component file (for Wasm runtime)
    pub wasm_component_path: Option<PathBuf>,
    /// Source target configuration (for Source runtime)
    pub source_target: Option<SourceTarget>,
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
    /// Explicit command (Generic Source Runtime)
    /// If specified, overrides language/entrypoint detection
    /// Example: ["ruby", "app.rb"] or ["deno", "run", "main.ts"]
    pub cmd: Option<Vec<String>>,
    /// Development mode flag
    /// When true, security validation is relaxed
    pub dev_mode: bool,
}

/// Result details returned after successful launch.
#[derive(Debug, Clone)]
pub struct LaunchResult {
    /// Process ID (None for Wasm components)
    pub pid: Option<u32>,
    /// Bundle path (optional for non-container runtimes)
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

    #[error("failed to serialize OCI spec: {0}")]
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

    #[error("sandbox setup failed: {0}")]
    SandboxSetupFailed(String),

    #[error("source target not provided in launch request")]
    SourceTargetMissing,

    #[error("security violation: {0}")]
    SecurityViolation(String),
}

#[allow(dead_code)]
struct CommandOutput {
    stderr: String,
    exit_code: Option<i32>,
}

impl CommandOutput {
    #[allow(dead_code)]
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
    use oci_spec::runtime::{ProcessBuilder, RootBuilder, SpecBuilder};

    use std::path::PathBuf;

    /// Helper to create a minimal valid OCI spec for testing
    #[allow(dead_code)]
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
    fn test_runtime_kind_from_str() {
        assert_eq!(RuntimeKind::from_str("youki"), Some(RuntimeKind::Youki));
        assert_eq!(RuntimeKind::from_str("YOUKI"), Some(RuntimeKind::Youki));
        assert_eq!(RuntimeKind::from_str("runc"), Some(RuntimeKind::Runc));
        assert_eq!(RuntimeKind::from_str("RUNC"), Some(RuntimeKind::Runc));
        assert_eq!(RuntimeKind::from_str("invalid"), None);
    }

    #[test]
    fn test_runtime_kind_binary_candidates() {
        assert_eq!(RuntimeKind::Youki.binary_candidates(), &["youki"]);
        assert_eq!(RuntimeKind::Runc.binary_candidates(), &["runc"]);
    }

    #[test]
    fn test_infer_kind_from_path() {
        let youki_path = PathBuf::from("/usr/bin/youki");
        assert_eq!(infer_kind_from_path(&youki_path), Some(RuntimeKind::Youki));

        let runc_path = PathBuf::from("/usr/bin/runc");
        assert_eq!(infer_kind_from_path(&runc_path), Some(RuntimeKind::Runc));

        let invalid_path = PathBuf::from("/usr/bin/unknown");
        assert_eq!(infer_kind_from_path(&invalid_path), None);
    }

    #[test]
    fn test_runtime_config_defaults() {
        // Test with no configuration
        let config = RuntimeConfig::from_section(None);

        // Should either find a runtime or return an error
        match config {
            Ok(cfg) => {
                // If successful, validate the config
                assert!(cfg.binary_path.exists() || !cfg.binary_path.as_os_str().is_empty());
                assert!(cfg.bundle_root.to_string_lossy().contains("bundles"));
                assert!(cfg.state_root.to_string_lossy().contains("state"));
                assert!(cfg.log_dir.to_string_lossy().contains("logs"));
                assert_eq!(cfg.hook_retry_attempts, DEFAULT_HOOK_RETRY_ATTEMPTS);
            }
            Err(e) => {
                // If no runtime found, error should mention "not found"
                assert!(e.to_string().contains("not found"));
            }
        }
    }

    #[test]
    fn test_runtime_error_display() {
        let err = RuntimeError::BinaryNotFound {
            tried: vec!["youki".to_string(), "runc".to_string()],
        };
        assert!(err.to_string().contains("not found"));
        assert!(err.to_string().contains("youki"));

        let err = RuntimeError::InvalidConfig("test error".to_string());
        assert!(err.to_string().contains("invalid runtime configuration"));

        let err = RuntimeError::CommandFailure {
            operation: "create".to_string(),
            exit_code: Some(1),
            stderr: "error message".to_string(),
        };
        assert!(err.to_string().contains("create"));
        assert!(err.to_string().contains("error message"));
    }

    #[test]
    fn test_launch_result_fields() {
        let result = LaunchResult {
            pid: Some(12345),
            bundle_path: Some(PathBuf::from("/path/to/bundle")),
            log_path: Some(PathBuf::from("/path/to/log")),
            port: None,
        };

        assert_eq!(result.pid, Some(12345));
        assert_eq!(result.bundle_path, Some(PathBuf::from("/path/to/bundle")));
        assert_eq!(result.log_path, Some(PathBuf::from("/path/to/log")));
    }
}
