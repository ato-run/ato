//! IPC common type definitions.
//!
//! These types are shared across all IPC sub-modules and define the data
//! contracts for service registration, endpoint addressing, and capability
//! negotiation.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use serde::{Deserialize, Serialize};

// ═══════════════════════════════════════════════════════════════════════════
// IPC Transport
// ═══════════════════════════════════════════════════════════════════════════

/// Transport mechanism for IPC communication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpcTransport {
    /// JSON-RPC 2.0 over stdin/stdout (default for Source runtime)
    Stdio,
    /// Unix Domain Socket (UDS)
    UnixSocket(PathBuf),
    /// TCP socket (for OCI containers)
    Tcp(String),
    /// Tailscale network (tsnet) — future
    Tsnet(String),
}

impl IpcTransport {
    /// Return a human-readable endpoint string for display.
    pub fn endpoint_display(&self) -> String {
        match self {
            IpcTransport::Stdio => "stdio://".to_string(),
            IpcTransport::UnixSocket(p) => format!("unix://{}", p.display()),
            IpcTransport::Tcp(addr) => format!("tcp://{}", addr),
            IpcTransport::Tsnet(addr) => format!("tsnet://{}", addr),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Runtime Kind (for IPC)
// ═══════════════════════════════════════════════════════════════════════════

/// Runtime kind that hosts the IPC service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpcRuntimeKind {
    /// Source (nacelle)
    Source,
    /// OCI container (Docker)
    Oci,
    /// WebAssembly
    Wasm,
}

impl std::fmt::Display for IpcRuntimeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IpcRuntimeKind::Source => write!(f, "source"),
            IpcRuntimeKind::Oci => write!(f, "oci"),
            IpcRuntimeKind::Wasm => write!(f, "wasm"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Sharing Mode
// ═══════════════════════════════════════════════════════════════════════════

/// How an IPC service instance is shared among clients.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharingMode {
    /// One instance shared by all clients (default).
    /// Stopped after `idle_timeout` with zero clients.
    #[default]
    Singleton,
    /// One instance per client. Stopped when the client disconnects.
    Exclusive,
    /// Long-lived process that ignores `idle_timeout`.
    Daemon,
}

// ═══════════════════════════════════════════════════════════════════════════
// Activation Mode
// ═══════════════════════════════════════════════════════════════════════════

/// When to start the IPC service.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationMode {
    /// Start before the client capsule (default for `startup = true`).
    #[default]
    Eager,
    /// Start on first `capsule/invoke` call.
    Lazy,
}

// ═══════════════════════════════════════════════════════════════════════════
// Service Info
// ═══════════════════════════════════════════════════════════════════════════

/// Information about a registered IPC service.
#[derive(Debug, Clone)]
pub struct IpcServiceInfo {
    /// Service name (from `capsule.toml` `[ipc.exports]`)
    pub name: String,
    /// Process ID of the running service (if started)
    pub pid: Option<u32>,
    /// IPC endpoint for clients to connect
    pub endpoint: IpcTransport,
    /// Capabilities exported by this service
    pub capabilities: Vec<String>,
    /// Current reference count (number of active clients)
    pub ref_count: u32,
    /// When the service was started
    pub started_at: Option<Instant>,
    /// Runtime kind
    pub runtime_kind: IpcRuntimeKind,
    /// Sharing mode
    pub sharing_mode: SharingMode,
}

// ═══════════════════════════════════════════════════════════════════════════
// IPC Method Descriptor
// ═══════════════════════════════════════════════════════════════════════════

/// Descriptor for an exported IPC method.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcMethodDescriptor {
    /// Method name (e.g., "greet")
    pub name: String,
    /// Description for documentation
    #[serde(default)]
    pub description: String,
    /// Path to JSON Schema for input validation (optional)
    #[serde(default)]
    pub input_schema: Option<String>,
    /// Path to JSON Schema for output (optional, informational)
    #[serde(default)]
    pub output_schema: Option<String>,
}

// ═══════════════════════════════════════════════════════════════════════════
// Manifest IPC Config
// ═══════════════════════════════════════════════════════════════════════════

/// `[ipc.exports]` section in capsule.toml — what this capsule provides.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IpcExportsConfig {
    /// Service name exposed to other capsules
    #[serde(default)]
    pub name: Option<String>,
    /// Methods exported by this service
    #[serde(default)]
    pub methods: Vec<IpcMethodDescriptor>,
    /// Sharing configuration
    #[serde(default)]
    pub sharing: IpcSharingConfig,
}

/// `[ipc.exports.sharing]` — how the service is shared.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcSharingConfig {
    /// Sharing mode
    #[serde(default)]
    pub mode: SharingMode,
    /// Idle timeout in seconds (default: 300s = 5 minutes)
    #[serde(default = "default_idle_timeout")]
    pub idle_timeout: u64,
    /// Maximum concurrent clients (0 = unlimited)
    #[serde(default)]
    pub max_clients: u32,
}

fn default_idle_timeout() -> u64 {
    300
}

impl Default for IpcSharingConfig {
    fn default() -> Self {
        Self {
            mode: SharingMode::default(),
            idle_timeout: default_idle_timeout(),
            max_clients: 0,
        }
    }
}

/// `[ipc.imports.<name>]` section — what this capsule depends on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcImportConfig {
    /// Source capsule identifier.
    /// Formats: `"<name>"`, `"@<scope>/<name>:<semver>"`, `"./<path>"`
    pub from: String,
    /// Activation mode (default: eager)
    #[serde(default)]
    pub activation: ActivationMode,
    /// Whether this import is optional (client starts even if service unavailable)
    #[serde(default)]
    pub optional: bool,
}

/// Top-level `[ipc]` section in capsule.toml.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IpcConfig {
    /// Exported service configuration
    #[serde(default)]
    pub exports: Option<IpcExportsConfig>,
    /// Imported services
    #[serde(default)]
    pub imports: HashMap<String, IpcImportConfig>,
}

// ═══════════════════════════════════════════════════════════════════════════
// Status Display
// ═══════════════════════════════════════════════════════════════════════════

/// Service status for `ato ipc status` display.
#[derive(Debug, Clone, Serialize)]
pub struct IpcServiceStatus {
    pub name: String,
    pub mode: SharingMode,
    pub ref_count: u32,
    pub transport: String,
    pub endpoint: String,
    pub runtime: IpcRuntimeKind,
    pub uptime_secs: u64,
    pub pid: Option<u32>,
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ipc_transport_display() {
        assert_eq!(IpcTransport::Stdio.endpoint_display(), "stdio://");
        assert_eq!(
            IpcTransport::UnixSocket(PathBuf::from("/tmp/test.sock")).endpoint_display(),
            "unix:///tmp/test.sock"
        );
        assert_eq!(
            IpcTransport::Tcp("127.0.0.1:54321".to_string()).endpoint_display(),
            "tcp://127.0.0.1:54321"
        );
    }

    #[test]
    fn test_sharing_mode_default() {
        assert_eq!(SharingMode::default(), SharingMode::Singleton);
    }

    #[test]
    fn test_activation_mode_default() {
        assert_eq!(ActivationMode::default(), ActivationMode::Eager);
    }

    #[test]
    fn test_ipc_runtime_kind_display() {
        assert_eq!(IpcRuntimeKind::Source.to_string(), "source");
        assert_eq!(IpcRuntimeKind::Oci.to_string(), "oci");
        assert_eq!(IpcRuntimeKind::Wasm.to_string(), "wasm");
    }

    #[test]
    fn test_ipc_config_deserialization() {
        let toml_str = r#"
            [exports]
            name = "greeter-service"
            [[exports.methods]]
            name = "greet"
            description = "Say hello"
            input_schema = "schemas/greet-input.json"

            [exports.sharing]
            mode = "singleton"
            idle_timeout = 600
            max_clients = 10

            [imports.llm]
            from = "llm-service"
            activation = "lazy"
            optional = false
        "#;

        let config: IpcConfig = toml::from_str(toml_str).unwrap();

        let exports = config.exports.unwrap();
        assert_eq!(exports.name.unwrap(), "greeter-service");
        assert_eq!(exports.methods.len(), 1);
        assert_eq!(exports.methods[0].name, "greet");
        assert_eq!(exports.sharing.mode, SharingMode::Singleton);
        assert_eq!(exports.sharing.idle_timeout, 600);
        assert_eq!(exports.sharing.max_clients, 10);

        let llm = config.imports.get("llm").unwrap();
        assert_eq!(llm.from, "llm-service");
        assert_eq!(llm.activation, ActivationMode::Lazy);
        assert!(!llm.optional);
    }

    #[test]
    fn test_ipc_config_empty_deserialization() {
        let toml_str = "";
        let config: IpcConfig = toml::from_str(toml_str).unwrap();
        assert!(config.exports.is_none());
        assert!(config.imports.is_empty());
    }

    #[test]
    fn test_ipc_sharing_config_defaults() {
        let config = IpcSharingConfig::default();
        assert_eq!(config.mode, SharingMode::Singleton);
        assert_eq!(config.idle_timeout, 300);
        assert_eq!(config.max_clients, 0);
    }

    #[test]
    fn test_ipc_import_config_minimal() {
        let toml_str = r#"from = "my-service""#;
        let import: IpcImportConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(import.from, "my-service");
        assert_eq!(import.activation, ActivationMode::Eager);
        assert!(!import.optional);
    }
}
