use serde::Serialize;
use serde_json::Value;

use capsule_core::execution_plan::error::{
    AtoErrorClassification, CleanupActionRecord, CleanupStatus, ManifestSuggestion,
};

use super::types::CliDiagnosticCode;

#[derive(Debug, Clone, Serialize)]
pub struct JsonErrorEnvelopeV1 {
    pub schema_version: &'static str,
    pub status: &'static str,
    pub error: JsonErrorPayloadV1,
}

#[derive(Debug, Clone, Serialize)]
pub struct JsonErrorPayloadV1 {
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
