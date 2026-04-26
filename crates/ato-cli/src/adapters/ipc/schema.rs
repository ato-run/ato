//! JSON Schema validation for IPC method inputs.
//!
//! Each exported IPC method can optionally define an `input_schema` in
//! `capsule.toml`. When present, the broker validates incoming `params`
//! against the schema before routing to the service.
//!
//! ## Limits
//!
//! - `max_message_size`: 1 MB (configurable). Messages exceeding this
//!   limit are rejected with error code `-32004` (Message too large).

use std::path::Path;

use serde_json::Value;
use thiserror::Error;
use tracing::debug;

/// Maximum message size in bytes (default: 1 MB).
#[allow(dead_code)]
pub const DEFAULT_MAX_MESSAGE_SIZE: usize = 1_024 * 1_024;

/// Schema validation error with developer-facing hint.
#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum SchemaError {
    /// Input does not conform to the JSON Schema.
    #[error("Schema validation failed: {message}")]
    ValidationFailed {
        message: String,
        /// Developer-facing hint about how to fix the input.
        hint: String,
    },
    /// Message exceeds the maximum allowed size.
    #[error("Message too large: {size} bytes (max: {max} bytes)")]
    MessageTooLarge { size: usize, max: usize },
    /// Schema file could not be loaded.
    #[error("Failed to load schema: {0}")]
    SchemaLoadError(String),
}

impl SchemaError {
    /// JSON-RPC error code for this error.
    #[allow(dead_code)]
    pub fn error_code(&self) -> i64 {
        match self {
            SchemaError::ValidationFailed { .. } => -32003,
            SchemaError::MessageTooLarge { .. } => -32004,
            SchemaError::SchemaLoadError(_) => -32603,
        }
    }

    /// Developer-facing hint.
    #[allow(dead_code)]
    pub fn hint(&self) -> String {
        match self {
            SchemaError::ValidationFailed { hint, .. } => hint.clone(),
            SchemaError::MessageTooLarge { max, .. } => {
                format!("Reduce message size. Maximum allowed is {} bytes.", max)
            }
            SchemaError::SchemaLoadError(msg) => {
                format!(
                    "Check that the schema file exists and is valid JSON Schema. {}",
                    msg
                )
            }
        }
    }
}

/// Validate input JSON against a schema file.
///
/// # Arguments
///
/// * `schema_path` — Path to the JSON Schema file (relative to capsule root).
/// * `capsule_root` — Root directory of the capsule (for resolving relative paths).
/// * `input` — The JSON value to validate.
///
/// # Errors
///
/// Returns `SchemaError::ValidationFailed` if the input does not match the schema.
/// Returns `SchemaError::SchemaLoadError` if the schema file cannot be read.
#[allow(dead_code)]
pub fn validate_input(
    schema_path: &str,
    capsule_root: &Path,
    input: &Value,
) -> Result<(), SchemaError> {
    let schema_value = load_schema_value(schema_path, capsule_root)?;
    validate_against_schema(&schema_value, input, schema_path)
}

/// Load and parse a schema file relative to the capsule root.
pub fn load_schema_value(schema_path: &str, capsule_root: &Path) -> Result<Value, SchemaError> {
    let resolved = capsule_root.join(schema_path);
    let schema_content = std::fs::read_to_string(&resolved).map_err(|e| {
        SchemaError::SchemaLoadError(format!(
            "Cannot read schema at {}: {}",
            resolved.display(),
            e
        ))
    })?;

    serde_json::from_str(&schema_content).map_err(|e| {
        SchemaError::SchemaLoadError(format!("Invalid JSON in schema {}: {}", schema_path, e))
    })
}

/// Validate input JSON against an already-parsed schema value.
#[allow(dead_code)]
pub fn validate_against_schema(
    schema: &Value,
    input: &Value,
    schema_name: &str,
) -> Result<(), SchemaError> {
    validate_schema_definition(schema, schema_name)?;

    let compiled = jsonschema::JSONSchema::compile(schema).map_err(|e| {
        SchemaError::SchemaLoadError(format!("Failed to compile schema '{}': {}", schema_name, e))
    })?;

    let result = compiled.validate(input);
    if let Err(errors) = result {
        let error_messages: Vec<String> = errors.map(|e| format!("{}", e)).collect();
        let message = error_messages.join("; ");
        let hint = format!(
            "Input validation failed for schema '{}'. Errors: {}",
            schema_name, message
        );
        debug!(schema = schema_name, errors = %message, "Schema validation failed");
        return Err(SchemaError::ValidationFailed { message, hint });
    }

    Ok(())
}

/// Validate that a schema definition is itself valid JSON Schema.
pub fn validate_schema_definition(schema: &Value, schema_name: &str) -> Result<(), SchemaError> {
    let compiled = jsonschema::JSONSchema::compile(schema).map_err(|e| {
        SchemaError::SchemaLoadError(format!("Failed to compile schema '{}': {}", schema_name, e))
    })?;
    let _ = compiled;
    Ok(())
}

/// Check that a serialized message does not exceed the size limit.
///
/// # Arguments
///
/// * `data` — Serialized message bytes.
/// * `max_size` — Maximum allowed size (use `DEFAULT_MAX_MESSAGE_SIZE` for default).
#[allow(dead_code)]
pub fn check_message_size(data: &[u8], max_size: usize) -> Result<(), SchemaError> {
    if data.len() > max_size {
        return Err(SchemaError::MessageTooLarge {
            size: data.len(),
            max: max_size,
        });
    }
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_schema() -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "integer", "minimum": 0 }
            },
            "required": ["name"]
        })
    }

    #[test]
    fn test_validate_valid_input() {
        let schema = sample_schema();
        let input = serde_json::json!({ "name": "Alice", "age": 30 });
        let result = validate_against_schema(&schema, &input, "test");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_missing_required_field() {
        let schema = sample_schema();
        let input = serde_json::json!({ "age": 30 });
        let result = validate_against_schema(&schema, &input, "test");
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert_eq!(err.error_code(), -32003);
        assert!(err.hint().contains("test"));
    }

    #[test]
    fn test_validate_wrong_type() {
        let schema = sample_schema();
        let input = serde_json::json!({ "name": 123 });
        let result = validate_against_schema(&schema, &input, "test");
        assert!(result.is_err());
    }

    #[test]
    fn test_check_message_size_ok() {
        let data = vec![0u8; 100];
        assert!(check_message_size(&data, DEFAULT_MAX_MESSAGE_SIZE).is_ok());
    }

    #[test]
    fn test_check_message_size_exceeded() {
        let data = vec![0u8; DEFAULT_MAX_MESSAGE_SIZE + 1];
        let result = check_message_size(&data, DEFAULT_MAX_MESSAGE_SIZE);
        assert!(result.is_err());

        let err = result.unwrap_err();
        assert_eq!(err.error_code(), -32004);
    }

    #[test]
    fn test_validate_input_file_not_found() {
        let result = validate_input(
            "nonexistent.json",
            Path::new("/tmp"),
            &serde_json::json!({}),
        );
        assert!(result.is_err());
        match result.unwrap_err() {
            SchemaError::SchemaLoadError(msg) => {
                assert!(msg.contains("Cannot read schema"));
            }
            other => panic!("Expected SchemaLoadError, got: {:?}", other),
        }
    }
}
