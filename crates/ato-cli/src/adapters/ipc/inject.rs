//! IPC environment injection — bridges the IPC Broker with runtime executors.
//!
//! `IpcContext` is created once per `ato run` session. It keeps both the
//! flat environment variables that runtimes need today and the structured
//! session metadata that the broker/runtime boundary will use going forward.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use super::broker::{IpcBroker, ResolvedService};
use super::types::{
    ActivationMode, IpcConfig, IpcRuntimeKind, IpcServiceInfo, IpcTransport, SharingMode,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SessionActivationMode {
    #[default]
    None,
    Eager,
    Lazy,
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedServiceState {
    Running,
    PendingStart,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ResolvedIpcService {
    pub import_name: String,
    pub source: String,
    pub runtime_kind: IpcRuntimeKind,
    pub endpoint: IpcTransport,
    pub socket_path: Option<PathBuf>,
    pub capabilities: Vec<String>,
    pub sharing_mode: SharingMode,
    pub activation: ActivationMode,
    pub state: ResolvedServiceState,
    pub manifest_path: Option<PathBuf>,
}

/// IPC context for a single `ato run` session.
#[derive(Debug, Clone, Default)]
pub struct IpcContext {
    /// Environment variables to inject into child processes.
    pub env_vars: HashMap<String, String>,
    /// Number of services successfully resolved.
    pub resolved_count: usize,
    /// Socket paths keyed by import alias.
    #[allow(dead_code)]
    pub socket_paths: HashMap<String, PathBuf>,
    /// Structured resolution results keyed by import alias.
    pub resolved_services: HashMap<String, ResolvedIpcService>,
    /// Session-wide activation summary.
    pub activation_mode: SessionActivationMode,
    /// Non-fatal warnings gathered during resolution.
    pub warnings: Vec<String>,
}

impl IpcContext {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn has_ipc(&self) -> bool {
        !self.resolved_services.is_empty()
    }

    pub fn from_manifest(raw_manifest: &toml::Value) -> Result<Self> {
        let Some(ipc_section) = raw_manifest.get("ipc") else {
            return Ok(Self::empty());
        };

        let ipc_str = toml::to_string(ipc_section).context("Failed to serialize [ipc] section")?;
        let config: IpcConfig =
            toml::from_str(&ipc_str).context("Failed to parse [ipc] section")?;

        if config.imports.is_empty() {
            debug!("No IPC imports defined");
            return Ok(Self::empty());
        }

        let broker = IpcBroker::new(default_socket_dir());
        Self::resolve_imports(&broker, &config)
    }

    fn resolve_imports(broker: &IpcBroker, config: &IpcConfig) -> Result<Self> {
        let mut env_vars = HashMap::new();
        let mut warnings = Vec::new();
        let mut resolved_count = 0usize;
        let mut socket_paths = HashMap::new();
        let mut resolved_services = HashMap::new();
        let mut saw_eager = false;
        let mut saw_lazy = false;

        info!(imports = config.imports.len(), "Resolving IPC imports");

        for (import_name, import_config) in &config.imports {
            match import_config.activation {
                ActivationMode::Eager => saw_eager = true,
                ActivationMode::Lazy => saw_lazy = true,
            }

            match broker.resolve(&import_config.from) {
                ResolvedService::Running(info) => {
                    register_resolved_service(
                        broker,
                        import_name,
                        &import_config.from,
                        import_config.activation,
                        info,
                        ResolvedServiceState::Running,
                        None,
                        &mut env_vars,
                        &mut socket_paths,
                        &mut resolved_services,
                    );
                    resolved_count += 1;
                    debug!(
                        service = import_name,
                        from = %import_config.from,
                        "IPC import resolved (running)"
                    );
                }
                ResolvedService::LocalStore(metadata) => {
                    let socket_path = broker.socket_path(import_name);
                    let info = IpcServiceInfo {
                        name: import_name.clone(),
                        pid: None,
                        endpoint: IpcTransport::UnixSocket(socket_path),
                        capabilities: metadata.capabilities.clone(),
                        ref_count: 0,
                        started_at: None,
                        runtime_kind: metadata.runtime_kind,
                        sharing_mode: metadata.sharing_mode,
                    };

                    if import_config.activation == ActivationMode::Eager {
                        broker.registry.register(info.clone());
                    }

                    register_resolved_service(
                        broker,
                        import_name,
                        &import_config.from,
                        import_config.activation,
                        info,
                        ResolvedServiceState::PendingStart,
                        Some(metadata.manifest_path.clone()),
                        &mut env_vars,
                        &mut socket_paths,
                        &mut resolved_services,
                    );
                    resolved_count += 1;

                    debug!(
                        service = import_name,
                        from = %import_config.from,
                        runtime = %metadata.runtime_kind,
                        activation = ?import_config.activation,
                        "IPC import resolved (local store — pending start)"
                    );
                }
                ResolvedService::NotFound { from, suggestion } => {
                    if import_config.optional {
                        warnings.push(format!(
                            "Optional IPC import '{}' not found: {}",
                            import_name, suggestion
                        ));
                        warn!(
                            service = import_name,
                            from = %from,
                            "Optional IPC import not found; skipping"
                        );
                    } else {
                        return Err(
                            capsule_core::execution_plan::error::AtoExecutionError::policy_violation(
                                format!(
                                    "Required IPC import '{}' (from '{}') not found. {}",
                                    import_name, from, suggestion
                                ),
                            )
                            .into(),
                        );
                    }
                }
            }
        }

        if resolved_count > 0 {
            env_vars.insert(
                "CAPSULE_IPC_PROTOCOL".to_string(),
                "jsonrpc-2.0".to_string(),
            );
            env_vars.insert(
                "CAPSULE_IPC_TRANSPORT".to_string(),
                "unix-socket".to_string(),
            );
        }

        let activation_mode = match (resolved_count, saw_eager, saw_lazy) {
            (0, _, _) => SessionActivationMode::None,
            (_, true, true) => SessionActivationMode::Mixed,
            (_, true, false) => SessionActivationMode::Eager,
            (_, false, true) => SessionActivationMode::Lazy,
            _ => SessionActivationMode::None,
        };

        info!(
            resolved = resolved_count,
            env_count = env_vars.len(),
            activation = ?activation_mode,
            "IPC imports resolved"
        );

        Ok(Self {
            env_vars,
            resolved_count,
            socket_paths,
            resolved_services,
            activation_mode,
            warnings,
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn register_resolved_service(
    broker: &IpcBroker,
    import_name: &str,
    source: &str,
    activation: ActivationMode,
    info: IpcServiceInfo,
    state: ResolvedServiceState,
    manifest_path: Option<PathBuf>,
    env_vars: &mut HashMap<String, String>,
    socket_paths: &mut HashMap<String, PathBuf>,
    resolved_services: &mut HashMap<String, ResolvedIpcService>,
) {
    let token = broker.token_manager.generate(info.capabilities.clone());
    for (key, value) in broker.generate_ipc_env(import_name, &info, &token.value) {
        env_vars.insert(key, value);
    }

    let socket_path = match &info.endpoint {
        IpcTransport::UnixSocket(path) => {
            socket_paths.insert(import_name.to_string(), path.clone());
            Some(path.clone())
        }
        _ => None,
    };

    resolved_services.insert(
        import_name.to_string(),
        ResolvedIpcService {
            import_name: import_name.to_string(),
            source: source.to_string(),
            runtime_kind: info.runtime_kind,
            endpoint: info.endpoint.clone(),
            socket_path,
            capabilities: info.capabilities,
            sharing_mode: info.sharing_mode,
            activation,
            state,
            manifest_path,
        },
    );
}

fn default_socket_dir() -> PathBuf {
    // Prefer ATO_SOCKET_DIR env var for testing / override scenarios.
    if let Ok(dir) = std::env::var("ATO_SOCKET_DIR") {
        return PathBuf::from(dir);
    }
    dirs::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".ato")
        .join("run")
        .join("capsule-ipc")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_context() {
        let ctx = IpcContext::empty();
        assert!(!ctx.has_ipc());
        assert!(ctx.env_vars.is_empty());
        assert_eq!(ctx.resolved_count, 0);
        assert!(ctx.socket_paths.is_empty());
        assert!(ctx.resolved_services.is_empty());
        assert_eq!(ctx.activation_mode, SessionActivationMode::None);
    }

    #[test]
    fn test_from_manifest_no_ipc_section() {
        let manifest: toml::Value = toml::from_str(
            r#"
            [execution]
            entrypoint = "python main.py"
            "#,
        )
        .unwrap();

        let ctx = IpcContext::from_manifest(&manifest).unwrap();
        assert!(!ctx.has_ipc());
    }

    #[test]
    fn test_from_manifest_empty_imports() {
        let manifest: toml::Value = toml::from_str(
            r#"
            [ipc.exports]
            name = "my-service"

            [ipc.imports]
            "#,
        )
        .unwrap();

        let ctx = IpcContext::from_manifest(&manifest).unwrap();
        assert!(!ctx.has_ipc());
    }

    #[test]
    fn test_from_manifest_optional_import_not_found() {
        let manifest: toml::Value = toml::from_str(
            r#"
            [ipc.imports.analytics]
            from = "nonexistent-analytics-service"
            optional = true
            "#,
        )
        .unwrap();

        let ctx = IpcContext::from_manifest(&manifest).unwrap();
        assert!(!ctx.has_ipc());
        assert_eq!(ctx.warnings.len(), 1);
        assert!(ctx.warnings[0].contains("Optional IPC import"));
    }

    #[test]
    fn test_from_manifest_required_import_not_found_errors() {
        let manifest: toml::Value = toml::from_str(
            r#"
            [ipc.imports.llm]
            from = "nonexistent-llm-service"
            optional = false
            "#,
        )
        .unwrap();

        let result = IpcContext::from_manifest(&manifest);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"), "Error: {}", err);
    }

    #[test]
    fn test_context_with_protocol_markers_only_does_not_count_as_ipc() {
        let ctx = IpcContext {
            env_vars: [
                (
                    "CAPSULE_IPC_PROTOCOL".to_string(),
                    "jsonrpc-2.0".to_string(),
                ),
                (
                    "CAPSULE_IPC_TRANSPORT".to_string(),
                    "unix-socket".to_string(),
                ),
            ]
            .into_iter()
            .collect(),
            resolved_count: 1,
            socket_paths: HashMap::new(),
            resolved_services: HashMap::new(),
            activation_mode: SessionActivationMode::Eager,
            warnings: Vec::new(),
        };

        assert!(!ctx.has_ipc());
        assert_eq!(
            ctx.env_vars.get("CAPSULE_IPC_PROTOCOL").unwrap(),
            "jsonrpc-2.0"
        );
    }

    #[test]
    fn test_from_manifest_tracks_mixed_activation_and_structured_service_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let eager_service = temp.path().join("svc-eager");
        let lazy_service = temp.path().join("svc-lazy");
        std::fs::create_dir_all(&eager_service).unwrap();
        std::fs::create_dir_all(&lazy_service).unwrap();

        let service_manifest = r#"
schema_version = "0.3"
name = "service"
version = "0.1.0"
type = "app"

runtime = "source"
run = "echo ok"
[ipc.exports]
name = "service"

[ipc.exports.sharing]
mode = "daemon"

[[ipc.exports.methods]]
name = "ping"
"#;

        std::fs::write(eager_service.join("capsule.toml"), service_manifest).unwrap();
        std::fs::write(lazy_service.join("capsule.toml"), service_manifest).unwrap();

        let manifest: toml::Value = toml::from_str(&format!(
            r#"
            [ipc.imports.greeter]
            from = "{}"
            activation = "eager"

            [ipc.imports.analytics]
            from = "{}"
            activation = "lazy"
            "#,
            eager_service.display(),
            lazy_service.display()
        ))
        .unwrap();

        let ctx = IpcContext::from_manifest(&manifest).unwrap();
        assert!(ctx.has_ipc());
        assert_eq!(ctx.resolved_count, 2);
        assert_eq!(ctx.activation_mode, SessionActivationMode::Mixed);
        assert_eq!(
            ctx.resolved_services["greeter"].activation,
            ActivationMode::Eager
        );
        assert_eq!(
            ctx.resolved_services["analytics"].activation,
            ActivationMode::Lazy
        );
        assert_eq!(
            ctx.resolved_services["greeter"].state,
            ResolvedServiceState::PendingStart
        );
        assert_eq!(
            ctx.resolved_services["greeter"].sharing_mode,
            SharingMode::Daemon
        );
        assert_eq!(
            ctx.resolved_services["greeter"].capabilities,
            vec!["ping".to_string()]
        );
        assert!(ctx.socket_paths.contains_key("greeter"));
        assert!(ctx.socket_paths.contains_key("analytics"));
        assert!(ctx.env_vars.contains_key("CAPSULE_IPC_GREETER_SOCKET"));
        assert!(ctx.env_vars.contains_key("CAPSULE_IPC_ANALYTICS_SOCKET"));
    }
}
