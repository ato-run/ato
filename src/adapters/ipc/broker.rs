//! IPC Broker — main orchestrator for Capsule IPC.
//!
//! The broker coordinates service resolution, startup, and connection
//! management. It ties together the registry, token manager, schema
//! validator, and refcount manager.
//!
//! ## Lifecycle
//!
//! 1. **Resolve**: Find the capsule that provides the requested service.
//! 2. **Start**: Launch the service capsule (via nacelle/docker/wasm).
//! 3. **Connect**: Issue a token and establish IPC transport.
//! 4. **Monitor**: Track refcounts and idle timeouts.
//! 5. **Shutdown**: Stop services gracefully on teardown.

use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use super::registry::IpcRegistry;
use super::token::TokenManager;
use super::types::{IpcConfig, IpcRuntimeKind, IpcServiceInfo, IpcTransport, SharingMode};

/// IPC Broker — the central IPC coordinator.
///
/// Owns the registry and token manager. Created once per `ato open`
/// session and shared across the runtime.
#[derive(Debug, Clone)]
pub struct IpcBroker {
    /// Service registry.
    pub registry: IpcRegistry,
    /// Token manager.
    pub token_manager: TokenManager,
    /// Base directory for IPC sockets.
    pub socket_dir: PathBuf,
}

/// Resolution result for a service import.
#[derive(Debug, Clone)]
pub enum ResolvedService {
    /// Service is already running (found in registry).
    Running(IpcServiceInfo),
    /// Service found in local store but not yet started.
    LocalStore(LocalServiceMetadata),
    /// Service not found anywhere.
    NotFound {
        /// Original `from` specifier.
        from: String,
        /// Suggested action for the user.
        suggestion: String,
    },
}

#[derive(Debug, Clone)]
pub struct LocalServiceMetadata {
    pub manifest_path: PathBuf,
    pub runtime_kind: IpcRuntimeKind,
    pub capabilities: Vec<String>,
    pub sharing_mode: SharingMode,
}

impl IpcBroker {
    /// Create a new IPC Broker.
    pub fn new(socket_dir: PathBuf) -> Self {
        // Ensure socket directory exists
        if let Err(e) = std::fs::create_dir_all(&socket_dir) {
            warn!(
                path = %socket_dir.display(),
                error = %e,
                "Failed to create IPC socket directory"
            );
        }

        Self {
            registry: IpcRegistry::new(),
            token_manager: TokenManager::new(),
            socket_dir,
        }
    }

    /// Resolve an IPC import to a service location.
    ///
    /// Resolution order:
    /// 1. Local Registry (already running)
    /// 2. Local Store (`~/.ato/store/`)
    /// 3. Error (suggest `ato install`)
    pub fn resolve(&self, from: &str) -> ResolvedService {
        // 1. Check if already running
        if let Some(info) = self.registry.lookup(from) {
            debug!(service = from, "IPC service found in registry (running)");
            return ResolvedService::Running(info);
        }

        // 2. Check local store
        let store_path = self.local_store_path(from);
        if store_path.exists() {
            let metadata = load_local_service_metadata(&store_path);
            debug!(
                service = from,
                path = %store_path.display(),
                runtime = %metadata.runtime_kind,
                "IPC service found in local store"
            );
            return ResolvedService::LocalStore(metadata);
        }

        // 3. Not found
        info!(
            service = from,
            "IPC service not found locally. Suggest: ato install {}", from
        );
        ResolvedService::NotFound {
            from: from.to_string(),
            suggestion: format!(
                "Service '{}' is not installed. Run: ato install {}",
                from, from
            ),
        }
    }

    /// Generate a Unix socket path for a service.
    pub fn socket_path(&self, service_name: &str) -> PathBuf {
        self.socket_dir
            .join(format!("{}.sock", sanitize_name(service_name)))
    }

    /// Generate the IPC environment variables for a client capsule.
    ///
    /// Returns a list of `(key, value)` pairs to inject into the child process.
    pub fn generate_ipc_env(
        &self,
        service_name: &str,
        info: &IpcServiceInfo,
        token: &str,
    ) -> Vec<(String, String)> {
        let upper = service_name.to_uppercase().replace('-', "_");
        let mut env = vec![
            (
                format!("CAPSULE_IPC_{}_URL", upper),
                info.endpoint.endpoint_display(),
            ),
            (format!("CAPSULE_IPC_{}_TOKEN", upper), token.to_string()),
        ];

        if let IpcTransport::UnixSocket(ref path) = info.endpoint {
            env.push((
                format!("CAPSULE_IPC_{}_SOCKET", upper),
                path.display().to_string(),
            ));
        }

        env
    }

    /// Compute the local store path for a service.
    fn local_store_path(&self, from: &str) -> PathBuf {
        // Handle different `from` formats:
        // - "greeter-service" → ~/.ato/store/greeter-service/
        // - "@scope/name:1.0" → ~/.ato/store/@scope/name/
        // - "./relative/path" / "/absolute/path" / "~/path" → filesystem path
        if from.starts_with("./") || from.starts_with("../") || Path::new(from).is_absolute() {
            return PathBuf::from(from);
        }

        if let Some(path) = from.strip_prefix("~/") {
            let home = capsule_core::common::paths::home_dir_or_workspace_tmp();
            return home.join(path);
        }

        let home = capsule_core::common::paths::home_dir_or_workspace_tmp();
        let store_dir = home.join(".ato").join("store");

        if from.starts_with('@') {
            // @scope/name:version → scope/name
            let without_at = from.strip_prefix('@').unwrap_or(from);
            let name = without_at.split(':').next().unwrap_or(without_at);
            store_dir.join(format!("@{}", name))
        } else {
            store_dir.join(from)
        }
    }
}

/// Detect the runtime kind by examining the capsule directory.
fn detect_runtime_kind(capsule_root: &std::path::Path) -> IpcRuntimeKind {
    let manifest_path = resolve_manifest_path(capsule_root);
    if let Ok(content) = std::fs::read_to_string(&manifest_path) {
        if let Ok(raw) = content.parse::<toml::Value>() {
            if let Some(default_target) = raw
                .get("default_target")
                .and_then(|v| v.as_str())
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
            {
                if let Some(runtime) = raw
                    .get("targets")
                    .and_then(|v| v.as_table())
                    .and_then(|targets| targets.get(default_target))
                    .and_then(|v| v.get("runtime"))
                    .and_then(|v| v.as_str())
                {
                    if runtime.eq_ignore_ascii_case("oci") {
                        return IpcRuntimeKind::Oci;
                    }
                    if runtime.eq_ignore_ascii_case("wasm") {
                        return IpcRuntimeKind::Wasm;
                    }
                    return IpcRuntimeKind::Source;
                }
            }
        }
    }
    IpcRuntimeKind::Source
}

fn load_local_service_metadata(capsule_root: &Path) -> LocalServiceMetadata {
    let manifest_path = resolve_manifest_path(capsule_root);
    let runtime_kind = detect_runtime_kind(capsule_root);
    let mut capabilities = Vec::new();
    let mut sharing_mode = SharingMode::default();

    if let Ok(content) = std::fs::read_to_string(&manifest_path) {
        if let Ok(raw) = content.parse::<toml::Value>() {
            if let Some(ipc_table) = raw.get("ipc") {
                if let Ok(ipc_str) = toml::to_string(ipc_table) {
                    if let Ok(config) = toml::from_str::<IpcConfig>(&ipc_str) {
                        if let Some(exports) = config.exports {
                            capabilities = exports
                                .methods
                                .into_iter()
                                .map(|method| method.name)
                                .collect();
                            sharing_mode = exports.sharing.mode;
                        }
                    }
                }
            }
        }
    }

    LocalServiceMetadata {
        manifest_path,
        runtime_kind,
        capabilities,
        sharing_mode,
    }
}

fn resolve_manifest_path(capsule_root: &Path) -> PathBuf {
    if capsule_root.is_file() {
        capsule_root.to_path_buf()
    } else {
        capsule_root.join("capsule.toml")
    }
}

/// Sanitize a service name for use in file paths.
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    use crate::ipc::types::SharingMode;

    fn test_broker() -> IpcBroker {
        let dir = std::env::temp_dir().join("capsule-ipc-test");
        IpcBroker::new(dir)
    }

    #[test]
    fn test_resolve_not_found() {
        let broker = test_broker();
        match broker.resolve("nonexistent-service") {
            ResolvedService::NotFound { from, suggestion } => {
                assert_eq!(from, "nonexistent-service");
                assert!(suggestion.contains("ato install"));
            }
            other => panic!("Expected NotFound, got: {:?}", other),
        }
    }

    #[test]
    fn test_resolve_running() {
        let broker = test_broker();
        broker.registry.register(IpcServiceInfo {
            name: "greeter".to_string(),
            pid: Some(12345),
            endpoint: IpcTransport::UnixSocket(PathBuf::from("/tmp/greeter.sock")),
            capabilities: vec!["greet".to_string()],
            ref_count: 1,
            started_at: Some(Instant::now()),
            runtime_kind: IpcRuntimeKind::Source,
            sharing_mode: SharingMode::Singleton,
        });

        match broker.resolve("greeter") {
            ResolvedService::Running(info) => {
                assert_eq!(info.name, "greeter");
                assert_eq!(info.pid, Some(12345));
            }
            other => panic!("Expected Running, got: {:?}", other),
        }
    }

    #[test]
    fn test_socket_path() {
        let broker = test_broker();
        let path = broker.socket_path("greeter-service");
        assert!(path.to_str().unwrap().contains("greeter-service.sock"));
    }

    #[test]
    fn test_generate_ipc_env() {
        let broker = test_broker();
        let info = IpcServiceInfo {
            name: "llm-service".to_string(),
            pid: Some(1),
            endpoint: IpcTransport::UnixSocket(PathBuf::from("/tmp/llm.sock")),
            capabilities: vec![],
            ref_count: 0,
            started_at: None,
            runtime_kind: IpcRuntimeKind::Source,
            sharing_mode: SharingMode::Singleton,
        };

        let env = broker.generate_ipc_env("llm-service", &info, "token123");
        assert_eq!(env.len(), 3); // URL, TOKEN, SOCKET
        assert!(env.iter().any(|(k, _)| k == "CAPSULE_IPC_LLM_SERVICE_URL"));
        assert!(env
            .iter()
            .any(|(k, v)| k == "CAPSULE_IPC_LLM_SERVICE_TOKEN" && v == "token123"));
        assert!(env
            .iter()
            .any(|(k, _)| k == "CAPSULE_IPC_LLM_SERVICE_SOCKET"));
    }

    #[test]
    fn test_sanitize_name() {
        assert_eq!(sanitize_name("greeter-service"), "greeter-service");
        assert_eq!(sanitize_name("my.service/v2"), "my_service_v2");
        assert_eq!(sanitize_name("hello_world"), "hello_world");
    }

    #[test]
    fn test_local_store_path_simple() {
        let broker = test_broker();
        let path = broker.local_store_path("greeter-service");
        assert!(path
            .to_str()
            .unwrap()
            .contains(".ato/store/greeter-service"));
    }

    #[test]
    fn test_local_store_path_scoped() {
        let broker = test_broker();
        let path = broker.local_store_path("@ato/llm-service:1.0");
        assert!(path
            .to_str()
            .unwrap()
            .contains(".ato/store/@ato/llm-service"));
    }

    #[test]
    fn test_local_store_path_relative() {
        let broker = test_broker();
        let path = broker.local_store_path("./my-local-service");
        assert_eq!(path, PathBuf::from("./my-local-service"));
    }
}
