use std::fmt;
use std::path::Path;

use miette::Diagnostic;
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use capsule_core::execution_plan::error::{
    AtoErrorClassification, CleanupActionRecord, CleanupStatus, ManifestSuggestion,
};

use super::json::{JsonErrorEnvelopeV1, JsonErrorPayloadV1};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandContext {
    Build,
    Run,
    Publish,
    Source,
    Other,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliDiagnosticCode {
    E001,
    E002,
    E003,
    E101,
    E102,
    E103,
    E104,
    E105,
    E106,
    E107,
    E201,
    E202,
    E203,
    E204,
    E205,
    E206,
    E207,
    E208,
    E209,
    E210,
    E211,
    E212,
    E301,
    E302,
    E303,
    E304,
    E305,
    E999,
}

impl CliDiagnosticCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::E001 => "E001",
            Self::E002 => "E002",
            Self::E003 => "E003",
            Self::E101 => "E101",
            Self::E102 => "E102",
            Self::E103 => "E103",
            Self::E104 => "E104",
            Self::E105 => "E105",
            Self::E106 => "E106",
            Self::E107 => "E107",
            Self::E201 => "E201",
            Self::E202 => "E202",
            Self::E203 => "E203",
            Self::E204 => "E204",
            Self::E205 => "E205",
            Self::E206 => "E206",
            Self::E207 => "E207",
            Self::E208 => "E208",
            Self::E209 => "E209",
            Self::E210 => "E210",
            Self::E211 => "E211",
            Self::E212 => "E212",
            Self::E301 => "E301",
            Self::E302 => "E302",
            Self::E303 => "E303",
            Self::E304 => "E304",
            Self::E305 => "E305",
            Self::E999 => "E999",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::E001 => "manifest_toml_parse",
            Self::E002 => "manifest_schema_invalid",
            Self::E003 => "manifest_required_field_missing",
            Self::E101 => "entrypoint_invalid",
            Self::E102 => "manual_intervention_required",
            Self::E103 => "missing_required_env",
            Self::E104 => "dependency_lock_missing",
            Self::E105 => "ambiguous_entrypoint",
            Self::E106 => "strict_manifest_fallback_blocked",
            Self::E107 => "unsupported_project_architecture",
            Self::E201 => "auth_required",
            Self::E202 => "publish_version_conflict",
            Self::E203 => "dependency_install_failed",
            Self::E204 => "runtime_compatibility_mismatch",
            Self::E205 => "engine_missing",
            Self::E206 => "skill_not_found",
            Self::E207 => "lockfile_tampered",
            Self::E208 => "artifact_integrity_failure",
            Self::E209 => "tls_bootstrap_required",
            Self::E210 => "tls_bootstrap_failed",
            Self::E211 => "storage_no_space",
            Self::E212 => "publish_payload_too_large",
            Self::E301 => "security_policy_violation",
            Self::E302 => "execution_contract_invalid",
            Self::E303 => "runtime_not_resolved",
            Self::E304 => "sandbox_unavailable",
            Self::E305 => "runtime_launch_failed",
            Self::E999 => "internal_error",
        }
    }

    pub fn phase(self) -> &'static str {
        match self {
            Self::E001 | Self::E002 | Self::E003 => "manifest",
            Self::E101
            | Self::E102
            | Self::E103
            | Self::E104
            | Self::E105
            | Self::E106
            | Self::E107 => "inference",
            Self::E201
            | Self::E202
            | Self::E203
            | Self::E204
            | Self::E205
            | Self::E206
            | Self::E207
            | Self::E208
            | Self::E209
            | Self::E210
            | Self::E211
            | Self::E212 => "provisioning",
            Self::E301 | Self::E302 | Self::E303 | Self::E304 | Self::E305 => "execution",
            Self::E999 => "internal",
        }
    }
}

impl fmt::Display for CliDiagnosticCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for CliDiagnosticCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Debug, Clone, Error, Serialize)]
#[error("{message}")]
pub struct CliDiagnostic {
    pub code: CliDiagnosticCode,
    pub name: &'static str,
    pub phase: &'static str,
    pub classification: AtoErrorClassification,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    pub retryable: bool,
    pub interactive_resolution: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cleanup_status: Option<CleanupStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleanup_actions: Vec<CleanupActionRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_suggestion: Option<ManifestSuggestion>,
    #[serde(default)]
    pub causes: Vec<String>,
}

impl CliDiagnostic {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        code: CliDiagnosticCode,
        message: impl Into<String>,
        hint: Option<&str>,
        path: Option<&Path>,
        field: Option<&str>,
        details: Option<Value>,
        retryable: bool,
        interactive_resolution: bool,
        causes: Vec<String>,
    ) -> Self {
        Self {
            code,
            name: code.name(),
            phase: code.phase(),
            classification: default_classification(code.phase()),
            message: message.into(),
            hint: hint.map(|v| v.to_string()),
            retryable,
            interactive_resolution,
            path: path.map(|v| v.display().to_string()),
            field: field.map(|v| v.to_string()),
            details,
            cleanup_status: None,
            cleanup_actions: Vec::new(),
            manifest_suggestion: None,
            causes,
        }
    }

    pub(super) fn with_classification(mut self, classification: AtoErrorClassification) -> Self {
        self.classification = classification;
        self
    }

    pub(super) fn with_cleanup(
        mut self,
        cleanup_status: Option<CleanupStatus>,
        cleanup_actions: Vec<CleanupActionRecord>,
    ) -> Self {
        self.cleanup_status = cleanup_status;
        self.cleanup_actions = cleanup_actions;
        self
    }

    pub(super) fn with_manifest_suggestion(
        mut self,
        manifest_suggestion: Option<ManifestSuggestion>,
    ) -> Self {
        self.manifest_suggestion = manifest_suggestion;
        self
    }

    pub fn to_json_envelope(&self) -> JsonErrorEnvelopeV1 {
        JsonErrorEnvelopeV1 {
            schema_version: "1",
            status: "error",
            error: JsonErrorPayloadV1 {
                code: self.code,
                name: self.name,
                phase: self.phase,
                classification: self.classification,
                message: self.message.clone(),
                hint: self.hint.clone(),
                retryable: self.retryable,
                interactive_resolution: self.interactive_resolution,
                path: self.path.clone(),
                field: self.field.clone(),
                details: self.details.clone(),
                cleanup_status: self.cleanup_status,
                cleanup_actions: self.cleanup_actions.clone(),
                manifest_suggestion: self.manifest_suggestion.clone(),
                causes: self.causes.clone(),
            },
        }
    }
}

impl Diagnostic for CliDiagnostic {
    fn code<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        Some(Box::new(self.code))
    }

    fn help<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        self.hint
            .as_ref()
            .map(|v| Box::new(v.clone()) as Box<dyn fmt::Display>)
    }

    fn url<'a>(&'a self) -> Option<Box<dyn fmt::Display + 'a>> {
        let url = format!(
            "https://docs.ato.run/errors#{}",
            self.code.as_str().to_ascii_lowercase()
        );
        Some(Box::new(url))
    }
}

fn default_classification(phase: &str) -> AtoErrorClassification {
    match phase {
        "manifest" | "inference" => AtoErrorClassification::Manifest,
        "source" | "build" => AtoErrorClassification::Source,
        "provisioning" => AtoErrorClassification::Provisioning,
        "execution" => AtoErrorClassification::Execution,
        _ => AtoErrorClassification::Internal,
    }
}
