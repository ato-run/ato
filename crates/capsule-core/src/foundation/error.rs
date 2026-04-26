//! Error types for the `capsule-core` crate.
//!
//! Two distinct error types serve different roles and must not be merged:
//!
//! - [`CapsuleError`] — internal propagation error (thiserror, `?`-friendly).
//!   Used within the library to propagate failures across module boundaries.
//!   Not serialised; carries structured variants for pattern matching.
//!
//! - [`AtoError`] — diagnostic output error (serde, JSON schema).
//!   Carries `code`, `phase`, `hint`, and `details` fields that form an external
//!   contract (`ato_error.*` JSON keys).  Constructed at the library boundary
//!   from a `CapsuleError` or other context and sent to the CLI reporter.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

use super::types::ConfigField;

/// Internal propagation error.
///
/// Returned by library functions that can fail.  Use `?` to propagate.
/// At CLI boundaries convert to [`AtoError`] for user-facing output.
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

    #[error(transparent)]
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

impl fmt::Display for AtoErrorPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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
        /// Rich schema for the unresolved fields, surfaced to the desktop
        /// dynamic config UI. Must satisfy `missing_schema[i].name ==
        /// missing_keys[i]` (same order, same length) when constructed by the
        /// CLI preflight. The desktop deserializer is required to iterate
        /// `missing_schema` only and ignore `missing_keys` — see the E103
        /// wire contract in the plan.
        missing_schema: Vec<ConfigField>,
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

/// Aggregates the static metadata for a single `AtoError` variant.
///
/// Having a single `kind()` match reduces per-variant maintenance from 3 match arms to 1:
/// adding a new variant only requires updating `kind()` and the methods it aggregates.
struct ErrorKind {
    code: &'static str,
    name: &'static str,
    phase: AtoErrorPhase,
}

impl AtoError {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::ManifestTomlParse { .. } => ErrorKind {
                code: "E001",
                name: "manifest_toml_parse",
                phase: AtoErrorPhase::Manifest,
            },
            Self::ManifestSchemaInvalid { .. } => ErrorKind {
                code: "E002",
                name: "manifest_schema_invalid",
                phase: AtoErrorPhase::Manifest,
            },
            Self::ManifestRequiredFieldMissing { .. } => ErrorKind {
                code: "E003",
                name: "manifest_required_field_missing",
                phase: AtoErrorPhase::Manifest,
            },
            Self::EntrypointInvalid { .. } => ErrorKind {
                code: "E101",
                name: "entrypoint_invalid",
                phase: AtoErrorPhase::Inference,
            },
            Self::ManualInterventionRequired { .. } => ErrorKind {
                code: "E102",
                name: "manual_intervention_required",
                phase: AtoErrorPhase::Inference,
            },
            Self::MissingRequiredEnv { .. } => ErrorKind {
                code: "E103",
                name: "missing_required_env",
                phase: AtoErrorPhase::Inference,
            },
            Self::DependencyLockMissing { .. } => ErrorKind {
                code: "E104",
                name: "dependency_lock_missing",
                phase: AtoErrorPhase::Inference,
            },
            Self::AmbiguousEntrypoint { .. } => ErrorKind {
                code: "E105",
                name: "ambiguous_entrypoint",
                phase: AtoErrorPhase::Inference,
            },
            Self::StrictManifestFallbackBlocked { .. } => ErrorKind {
                code: "E106",
                name: "strict_manifest_fallback_blocked",
                phase: AtoErrorPhase::Inference,
            },
            Self::UnsupportedProjectArchitecture { .. } => ErrorKind {
                code: "E107",
                name: "unsupported_project_architecture",
                phase: AtoErrorPhase::Inference,
            },
            Self::AuthRequired { .. } => ErrorKind {
                code: "E201",
                name: "auth_required",
                phase: AtoErrorPhase::Provisioning,
            },
            Self::PublishVersionConflict { .. } => ErrorKind {
                code: "E202",
                name: "publish_version_conflict",
                phase: AtoErrorPhase::Provisioning,
            },
            Self::DependencyInstallFailed { .. } => ErrorKind {
                code: "E203",
                name: "dependency_install_failed",
                phase: AtoErrorPhase::Provisioning,
            },
            Self::RuntimeCompatibilityMismatch { .. } => ErrorKind {
                code: "E204",
                name: "runtime_compatibility_mismatch",
                phase: AtoErrorPhase::Provisioning,
            },
            Self::EngineMissing { .. } => ErrorKind {
                code: "E205",
                name: "engine_missing",
                phase: AtoErrorPhase::Provisioning,
            },
            Self::SkillNotFound { .. } => ErrorKind {
                code: "E206",
                name: "skill_not_found",
                phase: AtoErrorPhase::Provisioning,
            },
            Self::LockfileTampered { .. } => ErrorKind {
                code: "E207",
                name: "lockfile_tampered",
                phase: AtoErrorPhase::Provisioning,
            },
            Self::ArtifactIntegrityFailure { .. } => ErrorKind {
                code: "E208",
                name: "artifact_integrity_failure",
                phase: AtoErrorPhase::Provisioning,
            },
            Self::TlsBootstrapRequired { .. } => ErrorKind {
                code: "E209",
                name: "tls_bootstrap_required",
                phase: AtoErrorPhase::Provisioning,
            },
            Self::TlsBootstrapFailed { .. } => ErrorKind {
                code: "E210",
                name: "tls_bootstrap_failed",
                phase: AtoErrorPhase::Provisioning,
            },
            Self::StorageNoSpace { .. } => ErrorKind {
                code: "E211",
                name: "storage_no_space",
                phase: AtoErrorPhase::Provisioning,
            },
            Self::SecurityPolicyViolation { .. } => ErrorKind {
                code: "E301",
                name: "security_policy_violation",
                phase: AtoErrorPhase::Execution,
            },
            Self::ExecutionContractInvalid { .. } => ErrorKind {
                code: "E302",
                name: "execution_contract_invalid",
                phase: AtoErrorPhase::Execution,
            },
            Self::RuntimeNotResolved { .. } => ErrorKind {
                code: "E303",
                name: "runtime_not_resolved",
                phase: AtoErrorPhase::Execution,
            },
            Self::SandboxUnavailable { .. } => ErrorKind {
                code: "E304",
                name: "sandbox_unavailable",
                phase: AtoErrorPhase::Execution,
            },
            Self::RuntimeLaunchFailed { .. } => ErrorKind {
                code: "E305",
                name: "runtime_launch_failed",
                phase: AtoErrorPhase::Execution,
            },
            Self::InternalError { .. } => ErrorKind {
                code: "E999",
                name: "internal_error",
                phase: AtoErrorPhase::Internal,
            },
        }
    }

    pub fn code(&self) -> &'static str {
        self.kind().code
    }

    pub fn name(&self) -> &'static str {
        self.kind().name
    }

    pub fn phase(&self) -> AtoErrorPhase {
        self.kind().phase
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
                missing_schema,
                target,
                ..
            } => Some(json!({
                "missing_keys": missing_keys,
                "missing_schema": missing_schema,
                "target": target,
            })),
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

/// Bridge `capsule_wire::WireError` into the richer `CapsuleError` so
/// `?` at internal call sites that consume the handle parser keeps
/// working unchanged. `WireError::Config(s)` is the only variant today;
/// it maps onto `CapsuleError::Config(s)` since the parser semantics are
/// "configuration / input was malformed".
impl From<capsule_wire::WireError> for CapsuleError {
    fn from(err: capsule_wire::WireError) -> Self {
        match err {
            capsule_wire::WireError::Config(msg) => CapsuleError::Config(msg),
        }
    }
}
