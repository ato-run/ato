use crate::error::AtoError;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AtoErrorCode {
    AtoErrPolicyViolation,
    AtoErrManualInterventionRequired,
    AtoErrMissingRequiredEnv,
    AtoErrAmbiguousEntrypoint,
    AtoErrSecurityPolicyViolation,
    AtoErrExecutionContractInvalid,
    AtoErrRuntimeNotResolved,
    AtoErrEngineMissing,
    AtoErrSkillNotFound,
    AtoErrProvisioningLockIncomplete,
    AtoErrProvisioningTlsTrust,
    AtoErrProvisioningTlsBootstrapRequired,
    AtoErrStorageNoSpace,
    AtoErrCompatHardware,
    AtoErrArtifactIntegrityFailure,
    AtoErrRuntimeLaunchFailed,
    AtoErrLockfileTampered,
    AtoErrInternal,
}

impl AtoErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AtoErrPolicyViolation => "ATO_ERR_POLICY_VIOLATION",
            Self::AtoErrManualInterventionRequired => "ATO_ERR_MANUAL_INTERVENTION_REQUIRED",
            Self::AtoErrMissingRequiredEnv => "ATO_ERR_MISSING_REQUIRED_ENV",
            Self::AtoErrAmbiguousEntrypoint => "ATO_ERR_AMBIGUOUS_ENTRYPOINT",
            Self::AtoErrSecurityPolicyViolation => "ATO_ERR_SECURITY_POLICY_VIOLATION",
            Self::AtoErrExecutionContractInvalid => "ATO_ERR_EXECUTION_CONTRACT_INVALID",
            Self::AtoErrRuntimeNotResolved => "ATO_ERR_RUNTIME_NOT_RESOLVED",
            Self::AtoErrEngineMissing => "ATO_ERR_ENGINE_MISSING",
            Self::AtoErrSkillNotFound => "ATO_ERR_SKILL_NOT_FOUND",
            Self::AtoErrProvisioningLockIncomplete => "ATO_ERR_PROVISIONING_LOCK_INCOMPLETE",
            Self::AtoErrProvisioningTlsTrust => "ATO_ERR_PROVISIONING_TLS_TRUST",
            Self::AtoErrProvisioningTlsBootstrapRequired => {
                "ATO_ERR_PROVISIONING_TLS_BOOTSTRAP_REQUIRED"
            }
            Self::AtoErrStorageNoSpace => "ATO_ERR_STORAGE_NO_SPACE",
            Self::AtoErrCompatHardware => "ATO_ERR_COMPAT_HARDWARE",
            Self::AtoErrArtifactIntegrityFailure => "ATO_ERR_ARTIFACT_INTEGRITY_FAILURE",
            Self::AtoErrRuntimeLaunchFailed => "ATO_ERR_RUNTIME_LAUNCH_FAILED",
            Self::AtoErrLockfileTampered => "ATO_ERR_LOCKFILE_TAMPERED",
            Self::AtoErrInternal => "ATO_ERR_INTERNAL",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::AtoErrPolicyViolation => "policy_violation",
            Self::AtoErrManualInterventionRequired => "manual_intervention_required",
            Self::AtoErrMissingRequiredEnv => "missing_required_env",
            Self::AtoErrAmbiguousEntrypoint => "ambiguous_entrypoint",
            Self::AtoErrSecurityPolicyViolation => "security_policy_violation",
            Self::AtoErrExecutionContractInvalid => "execution_contract_invalid",
            Self::AtoErrRuntimeNotResolved => "runtime_not_resolved",
            Self::AtoErrEngineMissing => "engine_missing",
            Self::AtoErrSkillNotFound => "skill_not_found",
            Self::AtoErrProvisioningLockIncomplete => "dependency_lock_missing",
            Self::AtoErrProvisioningTlsTrust => "tls_bootstrap_failed",
            Self::AtoErrProvisioningTlsBootstrapRequired => "tls_bootstrap_required",
            Self::AtoErrStorageNoSpace => "storage_no_space",
            Self::AtoErrCompatHardware => "sandbox_unavailable",
            Self::AtoErrArtifactIntegrityFailure => "artifact_integrity_failure",
            Self::AtoErrRuntimeLaunchFailed => "runtime_launch_failed",
            Self::AtoErrLockfileTampered => "lockfile_tampered",
            Self::AtoErrInternal => "internal_error",
        }
    }

    pub fn phase(self) -> &'static str {
        match self {
            Self::AtoErrManualInterventionRequired
            | Self::AtoErrMissingRequiredEnv
            | Self::AtoErrAmbiguousEntrypoint => "inference",
            Self::AtoErrProvisioningLockIncomplete
            | Self::AtoErrProvisioningTlsTrust
            | Self::AtoErrProvisioningTlsBootstrapRequired
            | Self::AtoErrStorageNoSpace
            | Self::AtoErrEngineMissing
            | Self::AtoErrSkillNotFound
            | Self::AtoErrArtifactIntegrityFailure
            | Self::AtoErrLockfileTampered => "provisioning",
            Self::AtoErrCompatHardware
            | Self::AtoErrRuntimeLaunchFailed
            | Self::AtoErrPolicyViolation
            | Self::AtoErrSecurityPolicyViolation
            | Self::AtoErrExecutionContractInvalid
            | Self::AtoErrRuntimeNotResolved => "execution",
            Self::AtoErrInternal => "internal",
        }
    }

    pub fn retryable(self) -> bool {
        matches!(
            self,
            Self::AtoErrProvisioningTlsTrust
                | Self::AtoErrArtifactIntegrityFailure
                | Self::AtoErrRuntimeLaunchFailed
                | Self::AtoErrInternal
        )
    }

    pub fn interactive_resolution(self) -> bool {
        matches!(
            self,
            Self::AtoErrManualInterventionRequired
                | Self::AtoErrMissingRequiredEnv
                | Self::AtoErrAmbiguousEntrypoint
                | Self::AtoErrProvisioningLockIncomplete
                | Self::AtoErrProvisioningTlsTrust
                | Self::AtoErrProvisioningTlsBootstrapRequired
                | Self::AtoErrEngineMissing
        )
    }
}

#[derive(Debug, Clone, Error)]
#[error("{code}: {message}")]
pub struct AtoExecutionError {
    pub code: &'static str,
    pub name: &'static str,
    pub phase: &'static str,
    pub message: String,
    pub resource: Option<String>,
    pub target: Option<String>,
    pub hint: Option<String>,
    pub retryable: bool,
    pub interactive_resolution: bool,
    pub details: Option<Value>,
}

impl AtoExecutionError {
    pub fn new(
        code: AtoErrorCode,
        message: impl Into<String>,
        resource: Option<&str>,
        target: Option<&str>,
        hint: Option<&str>,
    ) -> Self {
        Self {
            code: code.as_str(),
            name: code.name(),
            phase: code.phase(),
            message: message.into(),
            resource: resource.map(|v| v.to_string()),
            target: target.map(|v| v.to_string()),
            hint: hint.map(|v| v.to_string()),
            retryable: code.retryable(),
            interactive_resolution: code.interactive_resolution(),
            details: None,
        }
    }

    pub fn from_ato_error(error: AtoError) -> Self {
        let code = map_ato_error_code(&error);
        Self {
            code: code.as_str(),
            name: error.name(),
            phase: error.phase().as_str(),
            message: error.message().to_string(),
            resource: error.resource().map(ToString::to_string),
            target: error.target().map(ToString::to_string),
            hint: error.hint().map(ToString::to_string),
            retryable: error.retryable(),
            interactive_resolution: error.interactive_resolution(),
            details: error.details(),
        }
    }

    pub fn policy_violation(message: impl Into<String>) -> Self {
        Self::new(
            AtoErrorCode::AtoErrPolicyViolation,
            message,
            Some("policy"),
            None,
            None,
        )
    }

    pub fn security_policy_violation(
        message: impl Into<String>,
        resource: Option<&str>,
        blocked_host: Option<&str>,
    ) -> Self {
        Self::from_ato_error(AtoError::SecurityPolicyViolation {
            message: message.into(),
            hint: Some(
                "runtime policy の egress / permission 設定を確認してください。".to_string(),
            ),
            resource: resource.map(ToString::to_string),
            blocked_host: blocked_host.map(ToString::to_string),
        })
    }

    pub fn execution_contract_invalid(
        message: impl Into<String>,
        field: Option<&str>,
        service: Option<&str>,
    ) -> Self {
        Self::from_ato_error(AtoError::ExecutionContractInvalid {
            message: message.into(),
            hint: Some(
                "manifest と execution plan の整合性、特に service 設定と readiness_probe を確認してください。"
                    .to_string(),
            ),
            field: field.map(ToString::to_string),
            service: service.map(ToString::to_string),
        })
    }

    pub fn manual_intervention_required(
        message: impl Into<String>,
        manifest_path: Option<&str>,
        next_steps: Vec<String>,
    ) -> Self {
        let hint = if next_steps.is_empty() {
            None
        } else {
            Some(format!("Next steps:\n- {}", next_steps.join("\n- ")))
        };

        Self::from_ato_error(AtoError::ManualInterventionRequired {
            message: message.into(),
            hint,
            manifest_path: manifest_path.map(ToString::to_string),
            next_steps,
        })
    }

    pub fn missing_required_env(
        message: impl Into<String>,
        missing_keys: Vec<String>,
        target: Option<&str>,
    ) -> Self {
        Self::from_ato_error(AtoError::MissingRequiredEnv {
            message: message.into(),
            hint: Some("必要な環境変数を設定してから再実行してください。".to_string()),
            missing_keys,
            target: target.map(ToString::to_string),
        })
    }

    pub fn ambiguous_entrypoint(message: impl Into<String>, candidates: Vec<String>) -> Self {
        Self::from_ato_error(AtoError::AmbiguousEntrypoint {
            message: message.into(),
            hint: Some(
                "entrypoint を明示するか、候補を 1 つに絞ってから再実行してください。".to_string(),
            ),
            candidates,
        })
    }

    pub fn runtime_not_resolved(message: impl Into<String>, runtime: Option<&str>) -> Self {
        Self::from_ato_error(AtoError::RuntimeNotResolved {
            message: message.into(),
            hint: Some(
                "runtime 解決を再試行するか、version 指定と toolchain 設定を確認してください。"
                    .to_string(),
            ),
            runtime: runtime.map(ToString::to_string),
        })
    }

    pub fn lock_incomplete(message: impl Into<String>, target: Option<&str>) -> Self {
        let lockfile = target.unwrap_or("lockfile");
        Self::from_ato_error(AtoError::DependencyLockMissing {
            message: message.into(),
            hint: Some("lockfile を生成または同期してから再実行してください。".to_string()),
            lockfile: lockfile.to_string(),
            package_manager: None,
            target: target.map(ToString::to_string),
        })
    }

    pub fn engine_missing(message: impl Into<String>, target: Option<&str>) -> Self {
        Self::from_ato_error(AtoError::EngineMissing {
            message: message.into(),
            hint: Some(
                "必要な engine をインストールまたは bootstrap してから再試行してください。"
                    .to_string(),
            ),
            engine: target.map(ToString::to_string),
        })
    }

    pub fn skill_not_found(message: impl Into<String>, target: Option<&str>) -> Self {
        Self::from_ato_error(AtoError::SkillNotFound {
            message: message.into(),
            hint: Some("必要な skill 名と登録状態を確認してください。".to_string()),
            skill: target.map(ToString::to_string),
        })
    }

    pub fn lockfile_tampered(message: impl Into<String>, target: Option<&str>) -> Self {
        Self::from_ato_error(AtoError::LockfileTampered {
            message: message.into(),
            hint: Some(
                "lockfile を再生成し、manifest と一致していることを確認してください。".to_string(),
            ),
            lockfile: target.map(ToString::to_string),
        })
    }

    pub fn compat_hardware(message: impl Into<String>, target: Option<&str>) -> Self {
        Self::from_ato_error(AtoError::SandboxUnavailable {
            message: message.into(),
            hint: Some(
                "利用可能な sandbox backend または対応ホスト環境を確認してください。".to_string(),
            ),
            backend: target.map(ToString::to_string),
        })
    }

    pub fn artifact_integrity_failure(message: impl Into<String>, resource: Option<&str>) -> Self {
        Self::from_ato_error(AtoError::ArtifactIntegrityFailure {
            message: message.into(),
            hint: Some("artifact の整合性を確認し、必要なら再取得してください。".to_string()),
            resource: resource.map(ToString::to_string),
        })
    }

    pub fn tls_bootstrap_required(message: impl Into<String>, binding: Option<&str>) -> Self {
        Self::from_ato_error(AtoError::TlsBootstrapRequired {
            message: message.into(),
            hint: Some(
                "ato binding bootstrap-tls を実行して TLS trust をセットアップしてください。"
                    .to_string(),
            ),
            binding: binding.map(ToString::to_string),
        })
    }

    pub fn tls_bootstrap_failed(message: impl Into<String>, binding: Option<&str>) -> Self {
        Self::from_ato_error(AtoError::TlsBootstrapFailed {
            message: message.into(),
            hint: Some(
                "明示的同意と trust 設定を確認して bootstrap を再実行してください。".to_string(),
            ),
            binding: binding.map(ToString::to_string),
        })
    }

    pub fn storage_no_space(message: impl Into<String>, path: Option<&str>) -> Self {
        Self::from_ato_error(AtoError::StorageNoSpace {
            message: message.into(),
            hint: Some(
                "作業ディレクトリまたはキャッシュ領域の空き容量を確保してください。".to_string(),
            ),
            path: path.map(ToString::to_string),
        })
    }

    pub fn runtime_launch_failed(
        message: impl Into<String>,
        backend: Option<&str>,
        target: Option<&str>,
    ) -> Self {
        Self::from_ato_error(AtoError::RuntimeLaunchFailed {
            message: message.into(),
            hint: Some(
                "runtime backend の起動ログを確認し、依存プロセスと権限を確認してください。"
                    .to_string(),
            ),
            backend: backend.map(ToString::to_string),
            target: target.map(ToString::to_string),
        })
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::from_ato_error(AtoError::InternalError {
            message: message.into(),
            hint: Some(
                "再実行時に詳細ログを採取し、継続する場合は issue を報告してください。".to_string(),
            ),
            component: Some("internal".to_string()),
        })
    }
}

fn map_ato_error_code(error: &AtoError) -> AtoErrorCode {
    match error {
        AtoError::ManualInterventionRequired { .. } => {
            AtoErrorCode::AtoErrManualInterventionRequired
        }
        AtoError::MissingRequiredEnv { .. } => AtoErrorCode::AtoErrMissingRequiredEnv,
        AtoError::DependencyLockMissing { .. } => AtoErrorCode::AtoErrProvisioningLockIncomplete,
        AtoError::AmbiguousEntrypoint { .. } => AtoErrorCode::AtoErrAmbiguousEntrypoint,
        AtoError::EngineMissing { .. } => AtoErrorCode::AtoErrEngineMissing,
        AtoError::SkillNotFound { .. } => AtoErrorCode::AtoErrSkillNotFound,
        AtoError::LockfileTampered { .. } => AtoErrorCode::AtoErrLockfileTampered,
        AtoError::TlsBootstrapRequired { .. } => {
            AtoErrorCode::AtoErrProvisioningTlsBootstrapRequired
        }
        AtoError::TlsBootstrapFailed { .. } => AtoErrorCode::AtoErrProvisioningTlsTrust,
        AtoError::StorageNoSpace { .. } => AtoErrorCode::AtoErrStorageNoSpace,
        AtoError::ArtifactIntegrityFailure { .. } => AtoErrorCode::AtoErrArtifactIntegrityFailure,
        AtoError::SecurityPolicyViolation { .. } => AtoErrorCode::AtoErrSecurityPolicyViolation,
        AtoError::ExecutionContractInvalid { .. } => AtoErrorCode::AtoErrExecutionContractInvalid,
        AtoError::RuntimeNotResolved { .. } => AtoErrorCode::AtoErrRuntimeNotResolved,
        AtoError::SandboxUnavailable { .. } => AtoErrorCode::AtoErrCompatHardware,
        AtoError::RuntimeLaunchFailed { .. } => AtoErrorCode::AtoErrRuntimeLaunchFailed,
        AtoError::InternalError { .. } => AtoErrorCode::AtoErrInternal,
        _ => AtoErrorCode::AtoErrInternal,
    }
}
