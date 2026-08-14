//! Runtime Resolution Logic for Source-only nacelle
//!
//! nacelle only supports Source runtime targets. OCI/Wasm targets are rejected
//! and should be routed by ato-cli.

use crate::capsule_types::capsule_v1::{
    CapsuleManifestV1, SourceTarget, TargetsConfig,
};
use crate::launcher::RuntimeKind;
use std::collections::HashSet;
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

    #[error("Invalid target configuration: {message}")]
    InvalidConfiguration { message: String },
}

/// Resolved runtime target with all necessary information for execution
#[derive(Debug, Clone)]
pub enum ResolvedTarget {
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
        /// Dev mode flag
        dev_mode: bool,
    },
}

impl ResolvedTarget {
    /// Get the RuntimeKind for this resolved target
    pub fn runtime_kind(&self) -> RuntimeKind {
        RuntimeKind::Source
    }

    /// Get the target type name for logging
    pub fn target_type_name(&self) -> &'static str {
        "source"
    }
}

/// Context for runtime resolution decisions
#[derive(Debug, Clone)]
pub struct ResolveContext {
    /// Engine capabilities - which runtime types are supported
    pub supported_runtimes: HashSet<RuntimeKind>,
    /// Available toolchains on the host (for source targets)
    pub available_toolchains: HashSet<String>,
}

impl ResolveContext {
    /// Create a default context with Source runtime enabled
    pub fn source_only() -> Self {
        let mut supported = HashSet::new();
        supported.insert(RuntimeKind::Source);

        let mut toolchains = HashSet::new();
        toolchains.insert("python".to_string());
        toolchains.insert("node".to_string());

        Self {
            supported_runtimes: supported,
            available_toolchains: toolchains,
        }
    }

    /// Check if a specific toolchain is available
    pub fn has_toolchain(&self, language: &str) -> bool {
        let normalized = language.to_lowercase();
        self.available_toolchains.contains(&normalized)
            || match normalized.as_str() {
                "python3" => self.available_toolchains.contains("python"),
                "nodejs" => self.available_toolchains.contains("node"),
                _ => false,
            }
    }
}

/// Resolve the runtime target from a manifest.
///
/// Source-only behavior:
/// - Resolve `default_target` under `targets.<label>`.
/// - Only `runtime="source"` is supported by nacelle.
/// - Legacy `[execution]` fallback is not supported in schema v0.2.
pub fn resolve_runtime(
    manifest: &CapsuleManifestV1,
    context: &ResolveContext,
) -> Result<ResolvedTarget, ResolveError> {
    if let Some(targets) = &manifest.targets {
        if !targets.named.is_empty() {
            let label = manifest.default_target.trim();
            if label.is_empty() {
                return Err(ResolveError::InvalidConfiguration {
                    message: "default_target is required".to_string(),
                });
            }
            let target = targets
                .named_target(label)
                .ok_or_else(|| ResolveError::InvalidConfiguration {
                    message: format!("default_target '{}' not found in [targets]", label),
                })?;
            let runtime = target.runtime.trim().to_ascii_lowercase();
            if runtime != "source" {
                return Err(ResolveError::UnsupportedTarget { target: runtime });
            }
            if target.entrypoint.trim().is_empty() {
                return Err(ResolveError::InvalidConfiguration {
                    message: format!(
                        "targets.{}.entrypoint is required for runtime=source",
                        label
                    ),
                });
            }
            let language = infer_language_from_entrypoint(&target.entrypoint);
            return Ok(ResolvedTarget::Source {
                language,
                version: None,
                entrypoint: target.entrypoint.clone(),
                dependencies: None,
                args: target.cmd.clone(),
                dev_mode: false,
            });
        }

        if targets.has_any_target() {
            return resolve_source_target(targets, context);
        }
    }
    Err(ResolveError::InvalidConfiguration {
        message: "No targets defined".to_string(),
    })
}

fn resolve_source_target(
    targets: &TargetsConfig,
    context: &ResolveContext,
) -> Result<ResolvedTarget, ResolveError> {
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

    if let Some(source) = &targets.source {
        let resolved = try_resolve_source(source, context)?;
        info!("Resolved to Source target (language: {})", source.language);
        return Ok(resolved);
    }

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

fn try_resolve_source(
    source: &SourceTarget,
    context: &ResolveContext,
) -> Result<ResolvedTarget, ResolveError> {
    if !context.supported_runtimes.contains(&RuntimeKind::Source) {
        return Err(ResolveError::UnsupportedTarget {
            target: "source".to_string(),
        });
    }

    if !context.has_toolchain(&source.language) {
        warn!(
            "Toolchain not registered in context: {} (continuing)",
            source.language
        );
    }

    Ok(ResolvedTarget::Source {
        language: source.language.clone(),
        version: source.version.clone(),
        entrypoint: source.entrypoint.clone(),
        dependencies: source.dependencies.clone(),
        args: source.args.clone(),
        dev_mode: source.dev_mode,
    })
}

fn infer_language_from_entrypoint(entrypoint: &str) -> String {
    let lower = entrypoint.to_ascii_lowercase();
    if lower.ends_with(".py") {
        return "python".to_string();
    }
    if lower.ends_with(".js") || lower.ends_with(".mjs") || lower.ends_with(".cjs") {
        return "node".to_string();
    }
    if lower.ends_with(".ts") {
        return "node".to_string();
    }
    "source".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capsule_types::capsule_v1::{
        CapsuleExecution, CapsuleManifestV1, CapsuleRequirements, CapsuleRouting, CapsuleStorage,
        CapsuleType,
    };

    fn make_base_manifest() -> CapsuleManifestV1 {
        CapsuleManifestV1 {
            schema_version: "1.0".to_string(),
            name: "test".to_string(),
            version: "0.1.0".to_string(),
            capsule_type: CapsuleType::App,
            metadata: Default::default(),
            capabilities: None,
            requirements: CapsuleRequirements::default(),
            execution: CapsuleExecution {
                runtime: RuntimeType::Source,
                entrypoint: "main.py".to_string(),
                port: None,
                health_check: None,
                startup_timeout: 60,
                env: Default::default(),
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
            targets: None,
            services: None,
        }
    }

    #[test]
    fn resolves_source_target() {
        let mut manifest = make_base_manifest();
        manifest.targets = Some(TargetsConfig {
            preference: vec!["source".to_string()],
            source: Some(SourceTarget {
                language: "python".to_string(),
                version: Some("3.11".to_string()),
                entrypoint: "main.py".to_string(),
                dependencies: None,
                args: vec!["--help".to_string()],
                dev_mode: false,
            }),
            ..Default::default()
        });

        let context = ResolveContext::source_only();
        let result = resolve_runtime(&manifest, &context).unwrap();
        match result {
            ResolvedTarget::Source {
                language,
                entrypoint,
                ..
            } => {
                assert_eq!(language, "python");
                assert_eq!(entrypoint, "main.py");
            }
        }
    }

    #[test]
    fn rejects_legacy_runtime_without_targets() {
        let manifest = make_base_manifest();
        let context = ResolveContext::source_only();
        let result = resolve_runtime(&manifest, &context);
        assert!(matches!(
            result,
            Err(ResolveError::InvalidConfiguration { .. })
        ));
    }
}
