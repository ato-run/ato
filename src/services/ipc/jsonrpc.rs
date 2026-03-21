//! JSON-RPC 2.0 wire protocol for Capsule IPC.
//!
//! Implements the request/response/notification types as defined in
//! CAPSULE_IPC_SPEC §8. All IPC communication between capsule workloads
//! uses this protocol over the configured transport (UDS, TCP, stdio).
//!
//! ## Error Codes (§8.2)
//!
//! | Code   | Meaning                |
//! |--------|------------------------|
//! | -32700 | Parse error            |
//! | -32600 | Invalid request        |
//! | -32601 | Method not found       |
//! | -32602 | Invalid params         |
//! | -32603 | Internal error         |
//! | -32001 | Permission denied      |
//! | -32002 | Service unavailable    |
//! | -32003 | Schema validation error|
//! | -32004 | Message too large      |

#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ipc::schema::SchemaError;

/// JSON-RPC version string.
pub const JSONRPC_VERSION: &str = "2.0";

// ═══════════════════════════════════════════════════════════════════════════
// Error Codes
// ═══════════════════════════════════════════════════════════════════════════

/// Standard JSON-RPC 2.0 error codes.
#[allow(dead_code)]
pub mod error_codes {
    /// Parse error — invalid JSON was received.
    pub const PARSE_ERROR: i64 = -32700;
    /// Invalid request — the JSON is not a valid Request object.
    pub const INVALID_REQUEST: i64 = -32600;
    /// Method not found — the method does not exist.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid params — invalid method parameters.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Internal error — internal JSON-RPC error.
    pub const INTERNAL_ERROR: i64 = -32603;

    // Capsule-specific error codes
    /// Permission denied — caller lacks required capability.
    pub const PERMISSION_DENIED: i64 = -32001;
    /// Service unavailable — target service is not running.
    pub const SERVICE_UNAVAILABLE: i64 = -32002;
    /// Schema validation error — input does not match schema.
    pub const SCHEMA_VALIDATION: i64 = -32003;
    /// Message too large — payload exceeds max_message_size.
    pub const MESSAGE_TOO_LARGE: i64 = -32004;
}

// ═══════════════════════════════════════════════════════════════════════════
// Request
// ═══════════════════════════════════════════════════════════════════════════

/// JSON-RPC 2.0 Request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// Must be "2.0".
    pub jsonrpc: String,
    /// Method name (e.g., "capsule/invoke", "capsule/ping").
    pub method: String,
    /// Method parameters (positional or named).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    /// Request ID (number or string). Absent for notifications.
    pub id: Value,
}

impl JsonRpcRequest {
    /// Create a new request with auto-generated fields.
    pub fn new(method: impl Into<String>, params: Option<Value>, id: impl Into<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params,
            id: id.into(),
        }
    }

    /// Validate that this is a well-formed JSON-RPC 2.0 request.
    pub fn validate(&self) -> Result<(), JsonRpcError> {
        if self.jsonrpc != JSONRPC_VERSION {
            return Err(JsonRpcError::new(
                error_codes::INVALID_REQUEST,
                "Invalid JSON-RPC version (must be \"2.0\")".to_string(),
                Some("Set jsonrpc field to \"2.0\"".to_string()),
            ));
        }
        if self.method.is_empty() {
            return Err(JsonRpcError::new(
                error_codes::INVALID_REQUEST,
                "Method name is empty".to_string(),
                Some("Provide a non-empty method name".to_string()),
            ));
        }
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Response
// ═══════════════════════════════════════════════════════════════════════════

/// JSON-RPC 2.0 Response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    /// Must be "2.0".
    pub jsonrpc: String,
    /// Result value (mutually exclusive with `error`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Error object (mutually exclusive with `result`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    /// Request ID (echoed from request).
    pub id: Value,
}

impl JsonRpcResponse {
    /// Create a successful response.
    #[allow(dead_code)]
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            result: Some(result),
            error: None,
            id,
        }
    }

    /// Create an error response.
    pub fn error(id: Value, error: JsonRpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            result: None,
            error: Some(error),
            id,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Error
// ═══════════════════════════════════════════════════════════════════════════

/// JSON-RPC 2.0 Error Object.
///
/// Includes an optional `data.hint` field as required by CAPSULE_IPC_SPEC §8.2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// Error code (integer).
    pub code: i64,
    /// Human-readable error message.
    pub message: String,
    /// Additional error data with developer hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<JsonRpcErrorData>,
}

/// Additional data attached to a JSON-RPC error.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcErrorData {
    /// Developer-facing hint about how to fix the error.
    pub hint: String,
    /// Optional additional context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Value>,
}

impl JsonRpcError {
    /// Create a new error with a hint.
    pub fn new(code: i64, message: String, hint: Option<String>) -> Self {
        Self {
            code,
            message,
            data: hint.map(|h| JsonRpcErrorData {
                hint: h,
                context: None,
            }),
        }
    }

    /// Create a "method not found" (-32601).
    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            error_codes::METHOD_NOT_FOUND,
            format!("Method not found: {}", method),
            Some(format!(
                "Check that the service exports the method '{}'. Use ato ipc status to list available methods.",
                method
            )),
        )
    }

    /// Create a "permission denied" (-32001).
    #[allow(dead_code)]
    pub fn permission_denied(reason: &str) -> Self {
        Self::new(
            error_codes::PERMISSION_DENIED,
            format!("Permission denied: {}", reason),
            Some("Check that your token has the required capability".to_string()),
        )
    }

    /// Create a "service unavailable" (-32002).
    pub fn service_unavailable(reason: &str) -> Self {
        Self::new(
            error_codes::SERVICE_UNAVAILABLE,
            format!("Service unavailable: {}", reason),
            Some(
                "Start the service with `ato ipc start <capsule-dir>` and ensure its socket is reachable."
                    .to_string(),
            ),
        )
    }

    /// Create an "invalid params" (-32602).
    pub fn invalid_params(message: &str, hint: &str) -> Self {
        Self::new(
            error_codes::INVALID_PARAMS,
            message.to_string(),
            Some(hint.to_string()),
        )
    }

    /// Convert a schema validation failure into a JSON-RPC error object.
    pub fn from_schema_error(error: &SchemaError) -> Self {
        Self::new(error.error_code(), error.to_string(), Some(error.hint()))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Notification
// ═══════════════════════════════════════════════════════════════════════════

/// JSON-RPC 2.0 Notification (no id, no response expected).
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    /// Must be "2.0".
    pub jsonrpc: String,
    /// Method name (e.g., "capsule/internal.tokenRevoked").
    pub method: String,
    /// Parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcNotification {
    /// Create a new notification.
    #[allow(dead_code)]
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_string(),
            method: method.into(),
            params,
        }
    }

    /// Create a token-revoked notification.
    #[allow(dead_code)]
    pub fn token_revoked(reason: &str) -> Self {
        Self::new(
            "capsule/internal.tokenRevoked",
            Some(serde_json::json!({ "reason": reason })),
        )
    }
}

/// Parameters for `capsule/initialize`.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    /// Client capsule name.
    pub client_name: String,
    /// Protocol version supported by the client.
    pub protocol_version: String,
    /// Capabilities the client supports.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// Parameters for `capsule/invoke`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvokeParams {
    /// Target service name.
    pub service: String,
    /// Method to invoke on the service.
    pub method: String,
    /// Bearer token.
    pub token: String,
    /// Method arguments.
    #[serde(default)]
    pub args: Value,
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_request_serialization() {
        let req = JsonRpcRequest::new(
            "capsule/invoke",
            Some(json!({"service": "greeter", "method": "greet"})),
            json!(1),
        );

        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"method\":\"capsule/invoke\""));
        assert!(json.contains("\"id\":1"));
    }

    #[test]
    fn test_request_validation_ok() {
        let req = JsonRpcRequest::new("capsule/ping", None, json!(1));
        assert!(req.validate().is_ok());
    }

    #[test]
    fn test_request_validation_bad_version() {
        let req = JsonRpcRequest {
            jsonrpc: "1.0".to_string(),
            method: "test".to_string(),
            params: None,
            id: json!(1),
        };
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_request_validation_empty_method() {
        let req = JsonRpcRequest::new("", None, json!(1));
        assert!(req.validate().is_err());
    }

    #[test]
    fn test_response_success() {
        let resp = JsonRpcResponse::success(json!(1), json!({"greeting": "Hello!"}));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"result\""));
        assert!(!json.contains("\"error\""));
    }

    #[test]
    fn test_response_error() {
        let err = JsonRpcError::method_not_found("greet");
        let resp = JsonRpcResponse::error(json!(1), err);
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"error\""));
        assert!(json.contains("-32601"));
        assert!(!json.contains("\"result\""));
    }

    #[test]
    fn test_error_with_hint() {
        let err = JsonRpcError::permission_denied("missing capability 'greet'");
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("\"hint\""));
        assert!(json.contains("-32001"));
    }

    #[test]
    fn test_notification_serialization() {
        let notif = JsonRpcNotification::token_revoked("session expired");
        let json = serde_json::to_string(&notif).unwrap();
        assert!(json.contains("\"capsule/internal.tokenRevoked\""));
        assert!(json.contains("\"session expired\""));
        // Notifications should not have "id"
        assert!(!json.contains("\"id\""));
    }

    #[test]
    fn test_initialize_params_deserialization() {
        let json = r#"{
            "client_name": "my-app",
            "protocol_version": "1.0",
            "capabilities": ["greet", "compute"]
        }"#;
        let params: InitializeParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.client_name, "my-app");
        assert_eq!(params.protocol_version, "1.0");
        assert_eq!(params.capabilities.len(), 2);
    }

    #[test]
    fn test_invoke_params_deserialization() {
        let json = r#"{
            "service": "greeter",
            "method": "greet",
            "token": "abc123",
            "args": {"name": "World"}
        }"#;
        let params: InvokeParams = serde_json::from_str(json).unwrap();
        assert_eq!(params.service, "greeter");
        assert_eq!(params.method, "greet");
        assert_eq!(params.token, "abc123");
    }

    #[test]
    fn test_error_codes_values() {
        assert_eq!(error_codes::PARSE_ERROR, -32700);
        assert_eq!(error_codes::INVALID_REQUEST, -32600);
        assert_eq!(error_codes::METHOD_NOT_FOUND, -32601);
        assert_eq!(error_codes::INVALID_PARAMS, -32602);
        assert_eq!(error_codes::INTERNAL_ERROR, -32603);
        assert_eq!(error_codes::PERMISSION_DENIED, -32001);
        assert_eq!(error_codes::SERVICE_UNAVAILABLE, -32002);
        assert_eq!(error_codes::SCHEMA_VALIDATION, -32003);
        assert_eq!(error_codes::MESSAGE_TOO_LARGE, -32004);
    }

    #[test]
    fn test_request_deserialization_roundtrip() {
        let req = JsonRpcRequest::new(
            "capsule/invoke",
            Some(json!({"key": "value"})),
            json!("req-001"),
        );
        let serialized = serde_json::to_string(&req).unwrap();
        let deserialized: JsonRpcRequest = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.method, "capsule/invoke");
        assert_eq!(deserialized.id, json!("req-001"));
    }
}
