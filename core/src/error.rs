use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CapsuleError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Manifest error in {0}: {1}")]
    Manifest(PathBuf, String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Process execution error: {0}")]
    Execution(String),

    #[error("Hash mismatch: expected {0}, got {1}")]
    HashMismatch(String, String),

    #[error("Runtime error: {0}")]
    Runtime(String),

    #[error("Sidecar IPC error: {0}")]
    SidecarIpc(String),

    #[error("Sidecar request failed ({0}): {1}")]
    SidecarRequest(String, String),

    #[error("Sidecar response error: {0}")]
    SidecarResponse(String),

    #[error("Container engine error: {0}")]
    ContainerEngine(String),

    #[error("Process spawn error: {0}")]
    ProcessStart(String),

    #[error("Execution timed out")]
    Timeout,

    #[error("Cryptographic error: {0}")]
    Crypto(String),

    #[error("Resource not found: {0}")]
    NotFound(String),

    #[error("Authentication required: {0}")]
    AuthRequired(String),

    #[error("Build/Pack error: {0}")]
    Pack(String),

    #[error("Strict manifest fallback is not allowed: {0}")]
    StrictManifestFallbackNotAllowed(String),

    #[error("Unknown error: {0}")]
    Other(#[from] anyhow::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AtoErrorPhase {
    Manifest,
    Inference,
    Provisioning,
    Execution,
    Internal,
}

impl AtoErrorPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::Inference => "inference",
            Self::Provisioning => "provisioning",
            Self::Execution => "execution",
            Self::Internal => "internal",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AtoError {
    ManifestTomlParse {
        message: String,
        hint: Option<String>,
        path: Option<String>,
    },
    ManifestSchemaInvalid {
        message: String,
        hint: Option<String>,
        path: Option<String>,
        field: Option<String>,
    },
    ManifestRequiredFieldMissing {
        message: String,
        hint: Option<String>,
        path: Option<String>,
        field: Option<String>,
    },
    EntrypointInvalid {
        message: String,
        hint: Option<String>,
        field: Option<String>,
    },
    ManualInterventionRequired {
        message: String,
        hint: Option<String>,
        manifest_path: Option<String>,
        next_steps: Vec<String>,
    },
    MissingRequiredEnv {
        message: String,
        hint: Option<String>,
        missing_keys: Vec<String>,
        target: Option<String>,
    },
    DependencyLockMissing {
        message: String,
        hint: Option<String>,
        lockfile: String,
        package_manager: Option<String>,
        target: Option<String>,
    },
    AmbiguousEntrypoint {
        message: String,
        hint: Option<String>,
        candidates: Vec<String>,
    },
    StrictManifestFallbackBlocked {
        message: String,
        hint: Option<String>,
        field: Option<String>,
    },
    UnsupportedProjectArchitecture {
        message: String,
        hint: Option<String>,
        requirement: Option<String>,
    },
    AuthRequired {
        message: String,
        hint: Option<String>,
        resource: Option<String>,
        target: Option<String>,
    },
    PublishVersionConflict {
        message: String,
        hint: Option<String>,
        version: Option<String>,
    },
    DependencyInstallFailed {
        message: String,
        hint: Option<String>,
        package_manager: Option<String>,
        target: Option<String>,
    },
    RuntimeCompatibilityMismatch {
        message: String,
        hint: Option<String>,
        runtime: Option<String>,
        target: Option<String>,
    },
    EngineMissing {
        message: String,
        hint: Option<String>,
        engine: Option<String>,
    },
    SkillNotFound {
        message: String,
        hint: Option<String>,
        skill: Option<String>,
    },
    LockfileTampered {
        message: String,
        hint: Option<String>,
        lockfile: Option<String>,
    },
    ArtifactIntegrityFailure {
        message: String,
        hint: Option<String>,
        resource: Option<String>,
    },
    TlsBootstrapRequired {
        message: String,
        hint: Option<String>,
        binding: Option<String>,
    },
    TlsBootstrapFailed {
        message: String,
        hint: Option<String>,
        binding: Option<String>,
    },
    StorageNoSpace {
        message: String,
        hint: Option<String>,
        path: Option<String>,
    },
    SecurityPolicyViolation {
        message: String,
        hint: Option<String>,
        resource: Option<String>,
        blocked_host: Option<String>,
    },
    ExecutionContractInvalid {
        message: String,
        hint: Option<String>,
        field: Option<String>,
        service: Option<String>,
    },
    RuntimeNotResolved {
        message: String,
        hint: Option<String>,
        runtime: Option<String>,
    },
    SandboxUnavailable {
        message: String,
        hint: Option<String>,
        backend: Option<String>,
    },
    RuntimeLaunchFailed {
        message: String,
        hint: Option<String>,
        backend: Option<String>,
        target: Option<String>,
    },
    InternalError {
        message: String,
        hint: Option<String>,
        component: Option<String>,
    },
}

impl AtoError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ManifestTomlParse { .. } => "E001",
            Self::ManifestSchemaInvalid { .. } => "E002",
            Self::ManifestRequiredFieldMissing { .. } => "E003",
            Self::EntrypointInvalid { .. } => "E101",
            Self::ManualInterventionRequired { .. } => "E102",
            Self::MissingRequiredEnv { .. } => "E103",
            Self::DependencyLockMissing { .. } => "E104",
            Self::AmbiguousEntrypoint { .. } => "E105",
            Self::StrictManifestFallbackBlocked { .. } => "E106",
            Self::UnsupportedProjectArchitecture { .. } => "E107",
            Self::AuthRequired { .. } => "E201",
            Self::PublishVersionConflict { .. } => "E202",
            Self::DependencyInstallFailed { .. } => "E203",
            Self::RuntimeCompatibilityMismatch { .. } => "E204",
            Self::EngineMissing { .. } => "E205",
            Self::SkillNotFound { .. } => "E206",
            Self::LockfileTampered { .. } => "E207",
            Self::ArtifactIntegrityFailure { .. } => "E208",
            Self::TlsBootstrapRequired { .. } => "E209",
            Self::TlsBootstrapFailed { .. } => "E210",
            Self::StorageNoSpace { .. } => "E211",
            Self::SecurityPolicyViolation { .. } => "E301",
            Self::ExecutionContractInvalid { .. } => "E302",
            Self::RuntimeNotResolved { .. } => "E303",
            Self::SandboxUnavailable { .. } => "E304",
            Self::RuntimeLaunchFailed { .. } => "E305",
            Self::InternalError { .. } => "E999",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::ManifestTomlParse { .. } => "manifest_toml_parse",
            Self::ManifestSchemaInvalid { .. } => "manifest_schema_invalid",
            Self::ManifestRequiredFieldMissing { .. } => "manifest_required_field_missing",
            Self::EntrypointInvalid { .. } => "entrypoint_invalid",
            Self::ManualInterventionRequired { .. } => "manual_intervention_required",
            Self::MissingRequiredEnv { .. } => "missing_required_env",
            Self::DependencyLockMissing { .. } => "dependency_lock_missing",
            Self::AmbiguousEntrypoint { .. } => "ambiguous_entrypoint",
            Self::StrictManifestFallbackBlocked { .. } => "strict_manifest_fallback_blocked",
            Self::UnsupportedProjectArchitecture { .. } => "unsupported_project_architecture",
            Self::AuthRequired { .. } => "auth_required",
            Self::PublishVersionConflict { .. } => "publish_version_conflict",
            Self::DependencyInstallFailed { .. } => "dependency_install_failed",
            Self::RuntimeCompatibilityMismatch { .. } => "runtime_compatibility_mismatch",
            Self::EngineMissing { .. } => "engine_missing",
            Self::SkillNotFound { .. } => "skill_not_found",
            Self::LockfileTampered { .. } => "lockfile_tampered",
            Self::ArtifactIntegrityFailure { .. } => "artifact_integrity_failure",
            Self::TlsBootstrapRequired { .. } => "tls_bootstrap_required",
            Self::TlsBootstrapFailed { .. } => "tls_bootstrap_failed",
            Self::StorageNoSpace { .. } => "storage_no_space",
            Self::SecurityPolicyViolation { .. } => "security_policy_violation",
            Self::ExecutionContractInvalid { .. } => "execution_contract_invalid",
            Self::RuntimeNotResolved { .. } => "runtime_not_resolved",
            Self::SandboxUnavailable { .. } => "sandbox_unavailable",
            Self::RuntimeLaunchFailed { .. } => "runtime_launch_failed",
            Self::InternalError { .. } => "internal_error",
        }
    }

    pub fn phase(&self) -> AtoErrorPhase {
        match self {
            Self::ManifestTomlParse { .. }
            | Self::ManifestSchemaInvalid { .. }
            | Self::ManifestRequiredFieldMissing { .. } => AtoErrorPhase::Manifest,
            Self::EntrypointInvalid { .. }
            | Self::ManualInterventionRequired { .. }
            | Self::MissingRequiredEnv { .. }
            | Self::DependencyLockMissing { .. }
            | Self::AmbiguousEntrypoint { .. }
            | Self::StrictManifestFallbackBlocked { .. }
            | Self::UnsupportedProjectArchitecture { .. } => AtoErrorPhase::Inference,
            Self::AuthRequired { .. }
            | Self::PublishVersionConflict { .. }
            | Self::DependencyInstallFailed { .. }
            | Self::RuntimeCompatibilityMismatch { .. }
            | Self::EngineMissing { .. }
            | Self::SkillNotFound { .. }
            | Self::LockfileTampered { .. }
            | Self::ArtifactIntegrityFailure { .. }
            | Self::TlsBootstrapRequired { .. }
            | Self::TlsBootstrapFailed { .. }
            | Self::StorageNoSpace { .. } => AtoErrorPhase::Provisioning,
            Self::SecurityPolicyViolation { .. }
            | Self::ExecutionContractInvalid { .. }
            | Self::RuntimeNotResolved { .. }
            | Self::SandboxUnavailable { .. }
            | Self::RuntimeLaunchFailed { .. } => AtoErrorPhase::Execution,
            Self::InternalError { .. } => AtoErrorPhase::Internal,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::ManifestTomlParse { message, .. }
            | Self::ManifestSchemaInvalid { message, .. }
            | Self::ManifestRequiredFieldMissing { message, .. }
            | Self::EntrypointInvalid { message, .. }
            | Self::ManualInterventionRequired { message, .. }
            | Self::MissingRequiredEnv { message, .. }
            | Self::DependencyLockMissing { message, .. }
            | Self::AmbiguousEntrypoint { message, .. }
            | Self::StrictManifestFallbackBlocked { message, .. }
            | Self::UnsupportedProjectArchitecture { message, .. }
            | Self::AuthRequired { message, .. }
            | Self::PublishVersionConflict { message, .. }
            | Self::DependencyInstallFailed { message, .. }
            | Self::RuntimeCompatibilityMismatch { message, .. }
            | Self::EngineMissing { message, .. }
            | Self::SkillNotFound { message, .. }
            | Self::LockfileTampered { message, .. }
            | Self::ArtifactIntegrityFailure { message, .. }
            | Self::TlsBootstrapRequired { message, .. }
            | Self::TlsBootstrapFailed { message, .. }
            | Self::StorageNoSpace { message, .. }
            | Self::SecurityPolicyViolation { message, .. }
            | Self::ExecutionContractInvalid { message, .. }
            | Self::RuntimeNotResolved { message, .. }
            | Self::SandboxUnavailable { message, .. }
            | Self::RuntimeLaunchFailed { message, .. }
            | Self::InternalError { message, .. } => message,
        }
    }

    pub fn hint(&self) -> Option<&str> {
        match self {
            Self::ManifestTomlParse { hint, .. }
            | Self::ManifestSchemaInvalid { hint, .. }
            | Self::ManifestRequiredFieldMissing { hint, .. }
            | Self::EntrypointInvalid { hint, .. }
            | Self::ManualInterventionRequired { hint, .. }
            | Self::MissingRequiredEnv { hint, .. }
            | Self::DependencyLockMissing { hint, .. }
            | Self::AmbiguousEntrypoint { hint, .. }
            | Self::StrictManifestFallbackBlocked { hint, .. }
            | Self::UnsupportedProjectArchitecture { hint, .. }
            | Self::AuthRequired { hint, .. }
            | Self::PublishVersionConflict { hint, .. }
            | Self::DependencyInstallFailed { hint, .. }
            | Self::RuntimeCompatibilityMismatch { hint, .. }
            | Self::EngineMissing { hint, .. }
            | Self::SkillNotFound { hint, .. }
            | Self::LockfileTampered { hint, .. }
            | Self::ArtifactIntegrityFailure { hint, .. }
            | Self::TlsBootstrapRequired { hint, .. }
            | Self::TlsBootstrapFailed { hint, .. }
            | Self::StorageNoSpace { hint, .. }
            | Self::SecurityPolicyViolation { hint, .. }
            | Self::ExecutionContractInvalid { hint, .. }
            | Self::RuntimeNotResolved { hint, .. }
            | Self::SandboxUnavailable { hint, .. }
            | Self::RuntimeLaunchFailed { hint, .. }
            | Self::InternalError { hint, .. } => hint.as_deref(),
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(
            self,
            Self::AuthRequired { .. }
                | Self::DependencyInstallFailed { .. }
                | Self::RuntimeCompatibilityMismatch { .. }
                | Self::PublishVersionConflict { .. }
                | Self::TlsBootstrapFailed { .. }
                | Self::RuntimeLaunchFailed { .. }
                | Self::InternalError { .. }
        )
    }

    pub fn interactive_resolution(&self) -> bool {
        matches!(
            self,
            Self::ManualInterventionRequired { .. }
                | Self::MissingRequiredEnv { .. }
                | Self::DependencyLockMissing { .. }
                | Self::AmbiguousEntrypoint { .. }
                | Self::AuthRequired { .. }
                | Self::TlsBootstrapRequired { .. }
                | Self::TlsBootstrapFailed { .. }
        )
    }

    pub fn resource(&self) -> Option<&str> {
        match self {
            Self::ManualInterventionRequired { .. } => Some("manifest"),
            Self::MissingRequiredEnv { .. } => Some("environment"),
            Self::DependencyLockMissing { .. } | Self::LockfileTampered { .. } => Some("lockfile"),
            Self::AuthRequired { resource, .. } => resource.as_deref(),
            Self::DependencyInstallFailed {
                package_manager, ..
            } => package_manager.as_deref(),
            Self::RuntimeCompatibilityMismatch { runtime, .. } => runtime.as_deref(),
            Self::EngineMissing { engine, .. } => engine.as_deref().or(Some("engine")),
            Self::SkillNotFound { .. } => Some("skill"),
            Self::ArtifactIntegrityFailure { resource, .. } => resource.as_deref(),
            Self::TlsBootstrapRequired { .. } | Self::TlsBootstrapFailed { .. } => Some("tls"),
            Self::StorageNoSpace { .. } => Some("storage"),
            Self::SecurityPolicyViolation { resource, .. } => {
                resource.as_deref().or(Some("policy"))
            }
            Self::ExecutionContractInvalid { .. } => Some("contract"),
            Self::RuntimeNotResolved { .. } => Some("runtime"),
            Self::SandboxUnavailable { .. } => Some("sandbox"),
            Self::RuntimeLaunchFailed { backend, .. } => backend.as_deref().or(Some("runtime")),
            Self::InternalError { component, .. } => component.as_deref().or(Some("internal")),
            _ => None,
        }
    }

    pub fn target(&self) -> Option<&str> {
        match self {
            Self::ManualInterventionRequired { manifest_path, .. } => manifest_path.as_deref(),
            Self::MissingRequiredEnv { target, .. }
            | Self::DependencyLockMissing { target, .. }
            | Self::AuthRequired { target, .. }
            | Self::DependencyInstallFailed { target, .. }
            | Self::RuntimeCompatibilityMismatch { target, .. }
            | Self::RuntimeLaunchFailed { target, .. } => target.as_deref(),
            Self::SkillNotFound { skill, .. } => skill.as_deref(),
            Self::LockfileTampered { lockfile, .. } => lockfile.as_deref(),
            Self::TlsBootstrapRequired { binding, .. }
            | Self::TlsBootstrapFailed { binding, .. } => binding.as_deref(),
            Self::SecurityPolicyViolation { blocked_host, .. } => blocked_host.as_deref(),
            Self::RuntimeNotResolved { runtime, .. } => runtime.as_deref(),
            Self::SandboxUnavailable { backend, .. } => backend.as_deref(),
            _ => None,
        }
    }

    pub fn details(&self) -> Option<Value> {
        match self {
            Self::ManifestTomlParse { path, .. } => Some(json!({ "path": path })),
            Self::ManifestSchemaInvalid { path, field, .. }
            | Self::ManifestRequiredFieldMissing { path, field, .. } => {
                Some(json!({ "path": path, "field": field }))
            }
            Self::ManualInterventionRequired {
                manifest_path,
                next_steps,
                ..
            } => Some(json!({
                "manifest_path": manifest_path,
                "next_steps": next_steps,
            })),
            Self::EntrypointInvalid { field, .. } => Some(json!({ "field": field })),
            Self::MissingRequiredEnv {
                missing_keys,
                target,
                ..
            } => Some(json!({ "missing_keys": missing_keys, "target": target })),
            Self::DependencyLockMissing {
                lockfile,
                package_manager,
                target,
                ..
            } => Some(json!({
                "lockfile": lockfile,
                "package_manager": package_manager,
                "target": target,
            })),
            Self::AmbiguousEntrypoint { candidates, .. } => {
                Some(json!({ "candidates": candidates }))
            }
            Self::StrictManifestFallbackBlocked { field, .. } => Some(json!({ "field": field })),
            Self::UnsupportedProjectArchitecture { requirement, .. } => {
                Some(json!({ "requirement": requirement }))
            }
            Self::PublishVersionConflict { version, .. } => Some(json!({ "version": version })),
            Self::DependencyInstallFailed {
                package_manager,
                target,
                ..
            } => Some(json!({ "package_manager": package_manager, "target": target })),
            Self::RuntimeCompatibilityMismatch {
                runtime, target, ..
            } => Some(json!({ "runtime": runtime, "target": target })),
            Self::EngineMissing { engine, .. } => Some(json!({ "engine": engine })),
            Self::SkillNotFound { skill, .. } => Some(json!({ "skill": skill })),
            Self::LockfileTampered { lockfile, .. } => Some(json!({ "lockfile": lockfile })),
            Self::ArtifactIntegrityFailure { resource, .. } => {
                Some(json!({ "resource": resource }))
            }
            Self::TlsBootstrapRequired { binding, .. }
            | Self::TlsBootstrapFailed { binding, .. } => Some(json!({ "binding": binding })),
            Self::StorageNoSpace { path, .. } => Some(json!({ "path": path })),
            Self::SecurityPolicyViolation {
                resource,
                blocked_host,
                ..
            } => Some(json!({ "resource": resource, "blocked_host": blocked_host })),
            Self::ExecutionContractInvalid { field, service, .. } => {
                Some(json!({ "field": field, "service": service }))
            }
            Self::RuntimeNotResolved { runtime, .. } => Some(json!({ "runtime": runtime })),
            Self::SandboxUnavailable { backend, .. } => Some(json!({ "backend": backend })),
            Self::RuntimeLaunchFailed {
                backend, target, ..
            } => Some(json!({ "backend": backend, "target": target })),
            Self::InternalError { component, .. } => Some(json!({ "component": component })),
            Self::AuthRequired { .. } => None,
        }
    }
}

impl std::fmt::Display for AtoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.message())
    }
}

impl std::error::Error for AtoError {}

pub type Result<T> = std::result::Result<T, CapsuleError>;
