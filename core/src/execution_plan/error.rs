use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AtoErrorCode {
    AtoErrPolicyViolation,
    AtoErrEngineMissing,
    AtoErrSkillNotFound,
    AtoErrProvisioningLockIncomplete,
    AtoErrProvisioningTlsTrust,
    AtoErrStorageNoSpace,
    AtoErrCompatHardware,
    AtoErrLockfileTampered,
    AtoErrInternal,
}

impl AtoErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AtoErrPolicyViolation => "ATO_ERR_POLICY_VIOLATION",
            Self::AtoErrEngineMissing => "ATO_ERR_ENGINE_MISSING",
            Self::AtoErrSkillNotFound => "ATO_ERR_SKILL_NOT_FOUND",
            Self::AtoErrProvisioningLockIncomplete => "ATO_ERR_PROVISIONING_LOCK_INCOMPLETE",
            Self::AtoErrProvisioningTlsTrust => "ATO_ERR_PROVISIONING_TLS_TRUST",
            Self::AtoErrStorageNoSpace => "ATO_ERR_STORAGE_NO_SPACE",
            Self::AtoErrCompatHardware => "ATO_ERR_COMPAT_HARDWARE",
            Self::AtoErrLockfileTampered => "ATO_ERR_LOCKFILE_TAMPERED",
            Self::AtoErrInternal => "ATO_ERR_INTERNAL",
        }
    }
}

#[derive(Debug, Clone, Error)]
#[error("{code}: {message}")]
pub struct AtoExecutionError {
    pub code: &'static str,
    pub message: String,
    pub resource: Option<String>,
    pub target: Option<String>,
    pub hint: Option<String>,
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
            message: message.into(),
            resource: resource.map(|v| v.to_string()),
            target: target.map(|v| v.to_string()),
            hint: hint.map(|v| v.to_string()),
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

    pub fn lock_incomplete(message: impl Into<String>, target: Option<&str>) -> Self {
        Self::new(
            AtoErrorCode::AtoErrProvisioningLockIncomplete,
            message,
            Some("lockfile"),
            target,
            None,
        )
    }

    pub fn engine_missing(message: impl Into<String>, target: Option<&str>) -> Self {
        Self::new(
            AtoErrorCode::AtoErrEngineMissing,
            message,
            Some("engine"),
            target,
            None,
        )
    }

    pub fn skill_not_found(message: impl Into<String>, target: Option<&str>) -> Self {
        Self::new(
            AtoErrorCode::AtoErrSkillNotFound,
            message,
            Some("skill"),
            target,
            None,
        )
    }

    pub fn lockfile_tampered(message: impl Into<String>, target: Option<&str>) -> Self {
        Self::new(
            AtoErrorCode::AtoErrLockfileTampered,
            message,
            Some("lockfile"),
            target,
            None,
        )
    }

    pub fn compat_hardware(message: impl Into<String>, target: Option<&str>) -> Self {
        Self::new(
            AtoErrorCode::AtoErrCompatHardware,
            message,
            Some("sandbox"),
            target,
            None,
        )
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(
            AtoErrorCode::AtoErrInternal,
            message,
            Some("internal"),
            None,
            None,
        )
    }
}
