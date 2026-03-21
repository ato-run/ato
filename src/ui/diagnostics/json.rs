use serde::Serialize;
use serde_json::Value;

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
    #[serde(default)]
    pub causes: Vec<String>,
}
