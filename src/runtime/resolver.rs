//! Runtime Resolution Logic for UARC V1.1.0
//!
//! Implements the multi-target resolution algorithm:
//! 1. Filter: Intersection of provided targets ∩ supported runtimes
//! 2. Constraint: Check hardware requirements (GPU, arch, etc.)
//! 3. Preference: Select based on preference order
//!
//! Falls back to legacy `execution.runtime` field if no targets are defined.

use crate::capsule_types::capsule_v1::{
    CapsuleManifestV1, OciTarget, RuntimeType, SourceTarget, TargetsConfig, WasmTarget,
};
use crate::runtime::RuntimeKind;
use std::collections::HashSet;
use std::path::PathBuf;
use thiserror::Error;
use tracing::{debug, info, warn};

/// Errors that can occur during runtime resolution
#[derive(Error, Debug)]
pub enum ResolveError {
    #[error("No compatible target found. Provided: {provided:?}, Supported: {supported:?}")]
    NoCompatibleTarget {
        provided: Vec<String>,
        supported: Vec<String>,
    },

    #[error("Target '{target}' is not supported by this engine")]
    UnsupportedTarget { target: String },

    #[error("Required toolchain not available: {toolchain}")]
    ToolchainNotAvailable { toolchain: String },

    #[error("Hardware constraint not satisfied: {constraint}")]
    ConstraintNotSatisfied { constraint: String },

    #[error("Invalid target configuration: {message}")]
    InvalidConfiguration { message: String },
}

/// Resolved runtime target with all necessary information for execution
#[derive(Debug, Clone)]
pub enum ResolvedTarget {
    /// WebAssembly Component Model target
    Wasm {
        /// CAS digest of the Wasm component
        digest: String,
        /// WIT world interface
        world: String,
        /// Component path (resolved from CAS or local)
        component_path: Option<PathBuf>,
    },

    /// Source code target (interpreted)
    Source {
        /// Language runtime (python, node, etc.)
        language: String,
        /// Version constraint
        version: Option<String>,
        /// Entry point file
        entrypoint: String,
        /// Dependencies file
        dependencies: Option<String>,
        /// Runtime arguments
        args: Vec<String>,
    },

    /// OCI container target
    Oci {
        /// OCI image reference
        image: String,
        /// Image digest for immutability
        digest: Option<String>,
        /// Command to execute
        cmd: Vec<String>,
    },

    /// Legacy execution.runtime fallback
    Legacy {
        /// The legacy RuntimeType from execution field
        runtime_type: RuntimeType,
        /// Entrypoint from execution field
        entrypoint: String,
    },
}

impl ResolvedTarget {
    /// Get the RuntimeKind for this resolved target
    pub fn runtime_kind(&self) -> RuntimeKind {
        match self {
            ResolvedTarget::Wasm { .. } => RuntimeKind::Wasm,
            ResolvedTarget::Source { language, .. } => {
                // Map source language to RuntimeKind
                // UARC V1: Source runtime for interpreted languages
                match language.to_lowercase().as_str() {
                    "python" | "python3" => RuntimeKind::Source,
                    "node" | "nodejs" | "deno" => RuntimeKind::Source,
                    _ => RuntimeKind::Source,
                }
            }
            ResolvedTarget::Oci { .. } => RuntimeKind::Youki, // Prefer Youki for OCI
            ResolvedTarget::Legacy { runtime_type, .. } =>
            {
                #[allow(deprecated)]
                match runtime_type {
                    RuntimeType::Wasm => RuntimeKind::Wasm,
                    RuntimeType::Oci | RuntimeType::Youki | RuntimeType::Docker => {
                        RuntimeKind::Youki
                    }
                    RuntimeType::Native | RuntimeType::Source => RuntimeKind::Source,
                }
            }
        }
    }

    /// Get the target type name for logging
    pub fn target_type_name(&self) -> &'static str {
        match self {
            ResolvedTarget::Wasm { .. } => "wasm",
            ResolvedTarget::Source { .. } => "source",
            ResolvedTarget::Oci { .. } => "oci",
            ResolvedTarget::Legacy { .. } => "legacy",
        }
    }
}

/// Context for runtime resolution decisions
#[derive(Debug, Clone)]
pub struct ResolveContext {
    /// Platform identifier (e.g., "darwin-arm64", "linux-amd64")
    pub platform: String,

    /// Engine capabilities - which runtime types are supported
    pub supported_runtimes: HashSet<RuntimeKind>,

    /// Whether Wasm runtime is available and functional
    pub wasm_available: bool,

    /// Whether Docker/OCI runtime is available
    pub docker_available: bool,

    /// Whether GPU is available
    pub gpu_available: bool,

    /// Available toolchains on the host (for source targets)
    pub available_toolchains: HashSet<String>,

    /// Toolchains that can be provided via JIT provisioning (engine-managed runtimes)
    pub jit_toolchains: HashSet<String>,
}

impl ResolveContext {
    /// Create a default context with all runtimes enabled
    pub fn with_all_runtimes() -> Self {
        let mut supported = HashSet::new();
        supported.insert(RuntimeKind::Wasm);
        supported.insert(RuntimeKind::Youki);
        supported.insert(RuntimeKind::Source); // UARC V1: Native → Source

        let mut toolchains = HashSet::new();
        toolchains.insert("python".to_string());
        toolchains.insert("node".to_string());

        // JIT-provisionable runtimes (Phase 1)
        let mut jit_toolchains = HashSet::new();
        jit_toolchains.insert("python".to_string());
        jit_toolchains.insert("node".to_string());
        jit_toolchains.insert("deno".to_string());
        jit_toolchains.insert("bun".to_string());

        Self {
            platform: detect_current_platform(),
            supported_runtimes: supported,
            wasm_available: true,
            docker_available: true,
            gpu_available: false,
            available_toolchains: toolchains,
            jit_toolchains,
        }
    }

    /// Check if a specific toolchain is available
    pub fn has_toolchain(&self, language: &str) -> bool {
        let normalized = language.to_lowercase();
        self.available_toolchains.contains(&normalized)
            || self.jit_toolchains.contains(&normalized)
            || match normalized.as_str() {
                "python3" => {
                    self.available_toolchains.contains("python")
                        || self.jit_toolchains.contains("python")
                }
                "nodejs" => {
                    self.available_toolchains.contains("node") || self.jit_toolchains.contains("node")
                }
                _ => false,
            }
    }
}

/// Detect the current platform string
pub fn detect_current_platform() -> String {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "darwin-arm64".to_string()
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "darwin-x86_64".to_string()
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "linux-amd64".to_string()
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "linux-arm64".to_string()
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
    )))]
    {
        "unknown".to_string()
    }
}

/// Resolve the runtime target from a manifest
///
/// Implements UARC V1.1.0 Runtime Resolution Algorithm:
/// 1. If manifest.targets exists and has_any_target(), use multi-target resolution
/// 2. Otherwise, fall back to manifest.execution.runtime (legacy mode)
///
/// Multi-target resolution:
/// 1. Get preference order (from manifest or default: wasm → source → oci)
/// 2. For each target in preference order:
///    a. Check if engine supports this runtime type
///    b. Check hardware/toolchain constraints
///    c. If all checks pass, return the target
/// 3. If no target matches, return NoCompatibleTarget error
pub fn resolve_runtime(
    manifest: &CapsuleManifestV1,
    context: &ResolveContext,
) -> Result<ResolvedTarget, ResolveError> {
    // Check if multi-target mode is available
    if let Some(targets) = &manifest.targets {
        if targets.has_any_target() {
            return resolve_multi_target(targets, context);
        }
    }

    // Fall back to legacy mode
    debug!(
        "No targets defined, falling back to legacy execution.runtime: {:?}",
        manifest.execution.runtime
    );
    resolve_legacy_runtime(manifest, context)
}

/// Resolve using multi-target algorithm
fn resolve_multi_target(
    targets: &TargetsConfig,
    context: &ResolveContext,
) -> Result<ResolvedTarget, ResolveError> {
    let preference_order = targets.preference_order();

    info!(
        "Resolving runtime with preference order: {:?}",
        preference_order
    );

    // Collect what targets are actually provided
    let mut provided_targets = Vec::new();
    if targets.wasm.is_some() {
        provided_targets.push("wasm".to_string());
    }
    if targets.source.is_some() {
        provided_targets.push("source".to_string());
    }
    if targets.oci.is_some() {
        provided_targets.push("oci".to_string());
    }

    // Try each target in preference order
    for target_type in preference_order {
        match target_type {
            "wasm" => {
                if let Some(wasm) = &targets.wasm {
                    match try_resolve_wasm(wasm, context) {
                        Ok(resolved) => {
                            info!("Resolved to Wasm target (world: {})", wasm.world);
                            return Ok(resolved);
                        }
                        Err(e) => {
                            debug!("Wasm target not suitable: {}", e);
                            continue;
                        }
                    }
                }
            }
            "source" => {
                if let Some(source) = &targets.source {
                    match try_resolve_source(source, context) {
                        Ok(resolved) => {
                            info!("Resolved to Source target (language: {})", source.language);
                            return Ok(resolved);
                        }
                        Err(e) => {
                            debug!("Source target not suitable: {}", e);
                            continue;
                        }
                    }
                }
            }
            "oci" => {
                if let Some(oci) = &targets.oci {
                    match try_resolve_oci(oci, context) {
                        Ok(resolved) => {
                            info!("Resolved to OCI target (image: {})", oci.image);
                            return Ok(resolved);
                        }
                        Err(e) => {
                            debug!("OCI target not suitable: {}", e);
                            continue;
                        }
                    }
                }
            }
            other => {
                warn!("Unknown target type in preference list: {}", other);
            }
        }
    }

    // No compatible target found
    let supported: Vec<String> = context
        .supported_runtimes
        .iter()
        .map(|r| format!("{:?}", r))
        .collect();

    Err(ResolveError::NoCompatibleTarget {
        provided: provided_targets,
        supported,
    })
}

/// Try to resolve a Wasm target
fn try_resolve_wasm(
    wasm: &WasmTarget,
    context: &ResolveContext,
) -> Result<ResolvedTarget, ResolveError> {
    // Check if Wasm runtime is supported
    if !context.supported_runtimes.contains(&RuntimeKind::Wasm) {
        return Err(ResolveError::UnsupportedTarget {
            target: "wasm".to_string(),
        });
    }

    // Check if Wasm is actually available (engine capability)
    if !context.wasm_available {
        return Err(ResolveError::ToolchainNotAvailable {
            toolchain: "wasmtime".to_string(),
        });
    }

    // Validate digest format
    if wasm.digest.is_empty() {
        return Err(ResolveError::InvalidConfiguration {
            message: "Wasm target digest is empty".to_string(),
        });
    }

    Ok(ResolvedTarget::Wasm {
        digest: wasm.digest.clone(),
        world: wasm.world.clone(),
        component_path: None, // Will be resolved by CAS later
    })
}

/// Try to resolve a Source target
fn try_resolve_source(
    source: &SourceTarget,
    context: &ResolveContext,
) -> Result<ResolvedTarget, ResolveError> {
    let lang = source.language.to_ascii_lowercase();

    // Some runtimes require an explicit version so the engine can fetch deterministically.
    if (lang == "bun" || lang == "deno")
        && source
            .version
            .as_deref()
            .map(|v| v.trim().is_empty())
            .unwrap_or(true)
    {
        return Err(ResolveError::InvalidConfiguration {
            message: format!(
                "Source target version is required when language = '{}'",
                source.language
            ),
        });
    }

    // Check if the required toolchain is available on the host
    if !context.has_toolchain(&source.language) {
        return Err(ResolveError::ToolchainNotAvailable {
            toolchain: source.language.clone(),
        });
    }

    // Validate entrypoint is specified
    if source.entrypoint.is_empty() {
        return Err(ResolveError::InvalidConfiguration {
            message: "Source target entrypoint is empty".to_string(),
        });
    }

    Ok(ResolvedTarget::Source {
        language: source.language.clone(),
        version: source.version.clone(),
        entrypoint: source.entrypoint.clone(),
        dependencies: source.dependencies.clone(),
        args: source.args.clone(),
    })
}

/// Try to resolve an OCI target
fn try_resolve_oci(
    oci: &OciTarget,
    context: &ResolveContext,
) -> Result<ResolvedTarget, ResolveError> {
    // Check if Docker/OCI runtime is supported
    if !context.supported_runtimes.contains(&RuntimeKind::Youki) && !context.docker_available {
        return Err(ResolveError::UnsupportedTarget {
            target: "oci".to_string(),
        });
    }

    // Validate image is specified
    if oci.image.is_empty() {
        return Err(ResolveError::InvalidConfiguration {
            message: "OCI target image is empty".to_string(),
        });
    }

    Ok(ResolvedTarget::Oci {
        image: oci.image.clone(),
        digest: oci.digest.clone(),
        cmd: oci.cmd.clone(),
    })
}

/// Resolve using legacy execution.runtime field
fn resolve_legacy_runtime(
    manifest: &CapsuleManifestV1,
    _context: &ResolveContext,
) -> Result<ResolvedTarget, ResolveError> {
    Ok(ResolvedTarget::Legacy {
        runtime_type: manifest.execution.runtime.clone(),
        entrypoint: manifest.execution.entrypoint.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn test_context() -> ResolveContext {
        ResolveContext::with_all_runtimes()
    }

    fn test_manifest_with_targets() -> CapsuleManifestV1 {
        use crate::capsule_types::capsule_v1::*;

        CapsuleManifestV1 {
            schema_version: "1.0".to_string(),
            name: "test-capsule".to_string(),
            version: "1.0.0".to_string(),
            capsule_type: CapsuleType::App,
            metadata: CapsuleMetadataV1::default(),
            capabilities: None,
            requirements: CapsuleRequirements::default(),
            execution: CapsuleExecution {
                runtime: RuntimeType::Oci, // UARC V1.1.0: Use Oci instead of deprecated Docker
                entrypoint: "/app/main".to_string(),
                port: Some(8080),
                health_check: None,
                startup_timeout: 60,
                env: HashMap::new(),
                signals: Default::default(),
            },
            storage: CapsuleStorage::default(),
            routing: CapsuleRouting::default(),
            network: None,
            model: None,
            transparency: None,
            pool: None,
            build: None,
            isolation: None,
            targets: Some(TargetsConfig {
                preference: vec!["wasm".to_string(), "oci".to_string()],
                source_digest: None,
                port: None,
                startup_timeout: 60,
                env: HashMap::new(),
                health_check: None,
                wasm: Some(WasmTarget {
                    digest: "sha256:abc123".to_string(),
                    world: "wasi:cli/command".to_string(),
                    config: HashMap::new(),
                }),
                source: None,
                oci: Some(OciTarget {
                    image: "python:3.11-slim".to_string(),
                    digest: Some("sha256:def456".to_string()),
                    cmd: vec!["python".to_string(), "main.py".to_string()],
                    env: HashMap::new(),
                }),
            }),
        }
    }

    fn test_manifest_legacy() -> CapsuleManifestV1 {
        use crate::capsule_types::capsule_v1::*;

        CapsuleManifestV1 {
            schema_version: "1.0".to_string(),
            name: "test-legacy".to_string(),
            version: "1.0.0".to_string(),
            capsule_type: CapsuleType::App,
            metadata: CapsuleMetadataV1::default(),
            capabilities: None,
            requirements: CapsuleRequirements::default(),
            execution: CapsuleExecution {
                runtime: RuntimeType::Oci, // UARC V1.1.0: Use Oci for legacy OCI tests
                entrypoint: "python:3.11-slim".to_string(),
                port: Some(8080),
                health_check: None,
                startup_timeout: 60,
                env: HashMap::new(),
                signals: Default::default(),
            },
            storage: CapsuleStorage::default(),
            routing: CapsuleRouting::default(),
            network: None,
            model: None,
            transparency: None,
            pool: None,
            build: None,
            isolation: None,
            targets: None, // No targets = legacy mode
        }
    }

    #[test]
    fn test_resolve_prefers_wasm_when_available() {
        let manifest = test_manifest_with_targets();
        let context = test_context();

        let result = resolve_runtime(&manifest, &context).unwrap();

        match result {
            ResolvedTarget::Wasm { digest, world, .. } => {
                assert_eq!(digest, "sha256:abc123");
                assert_eq!(world, "wasi:cli/command");
            }
            _ => panic!("Expected Wasm target, got {:?}", result),
        }
    }

    #[test]
    fn test_resolve_falls_back_to_oci_when_wasm_unavailable() {
        let manifest = test_manifest_with_targets();
        let mut context = test_context();
        context.wasm_available = false;
        context.supported_runtimes.remove(&RuntimeKind::Wasm);

        let result = resolve_runtime(&manifest, &context).unwrap();

        match result {
            ResolvedTarget::Oci { image, .. } => {
                assert_eq!(image, "python:3.11-slim");
            }
            _ => panic!("Expected OCI target, got {:?}", result),
        }
    }

    #[test]
    fn test_resolve_legacy_when_no_targets() {
        let manifest = test_manifest_legacy();
        let context = test_context();

        let result = resolve_runtime(&manifest, &context).unwrap();

        match result {
            ResolvedTarget::Legacy {
                runtime_type,
                entrypoint,
            } => {
                assert_eq!(runtime_type, RuntimeType::Oci); // UARC V1.1.0
                assert_eq!(entrypoint, "python:3.11-slim");
            }
            _ => panic!("Expected Legacy target, got {:?}", result),
        }
    }

    #[test]
    fn test_resolve_error_when_no_compatible_target() {
        let manifest = test_manifest_with_targets();
        let mut context = test_context();
        // Disable all runtimes
        context.wasm_available = false;
        context.docker_available = false;
        context.supported_runtimes.clear();

        let result = resolve_runtime(&manifest, &context);

        assert!(result.is_err());
        match result.unwrap_err() {
            ResolveError::NoCompatibleTarget { provided, .. } => {
                assert!(provided.contains(&"wasm".to_string()));
                assert!(provided.contains(&"oci".to_string()));
            }
            other => panic!("Expected NoCompatibleTarget, got {:?}", other),
        }
    }

    #[test]
    fn test_detect_platform() {
        let platform = detect_current_platform();
        assert!(!platform.is_empty());
        // On macOS ARM64 CI:
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        assert_eq!(platform, "darwin-arm64");
    }

    #[test]
    fn test_resolved_target_runtime_kind() {
        let wasm = ResolvedTarget::Wasm {
            digest: "test".to_string(),
            world: "test".to_string(),
            component_path: None,
        };
        assert_eq!(wasm.runtime_kind(), RuntimeKind::Wasm);

        let oci = ResolvedTarget::Oci {
            image: "test".to_string(),
            digest: None,
            cmd: vec![],
        };
        assert_eq!(oci.runtime_kind(), RuntimeKind::Youki);
    }

    #[test]
    fn test_context_has_toolchain() {
        let context = test_context();
        assert!(context.has_toolchain("python"));
        assert!(context.has_toolchain("Python"));
        assert!(context.has_toolchain("python3")); // Alias
        assert!(context.has_toolchain("node"));
        assert!(context.has_toolchain("nodejs")); // Alias
        assert!(!context.has_toolchain("ruby"));
    }
}
