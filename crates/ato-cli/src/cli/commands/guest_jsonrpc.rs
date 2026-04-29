//! Phase 13b.9 — JSON-RPC 2.0 wire layer for `ato guest`.
//!
//! Routes JSON-RPC 2.0 requests to the shared `dispatch_guest_action` exported
//! by `super::guest`. The legacy `guest.v1` envelope continues to be served by
//! `super::guest::execute()` directly.
//!
//! Method ↔ `GuestAction` mapping (v2.1 — 5 file IO methods only):
//!
//! | Method                   | GuestAction     | params                     | result                |
//! |--------------------------|-----------------|----------------------------|-----------------------|
//! | `capsule/payload.read`   | `ReadPayload`   | `{ context }`              | `{ payload_b64 }`     |
//! | `capsule/payload.write`  | `WritePayload`  | `{ context, payload_b64 }` | `null`                |
//! | `capsule/payload.update` | `UpdatePayload` | `{ context, payload_b64 }` | `null`                |
//! | `capsule/context.read`   | `ReadContext`   | `{ context }`              | `{ value }`           |
//! | `capsule/context.write`  | `WriteContext`  | `{ context, value }`       | `null`                |
//!
//! `capsule/wasm.execute` is reserved for a future PR once the WASM/OCI
//! runtimes are integrated end-to-end. Until then, requests for that method
//! return `-32601 Method not found`. `ExecuteWasm` remains reachable through
//! the legacy `guest.v1` envelope for tests and existing integrations.

use crate::guest_protocol::{GuestAction, GuestContext, GuestError, GuestErrorCode};
use crate::ipc::jsonrpc::{error_codes, JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use anyhow::Result;
use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;

/// Entry point used by `super::guest::execute()` when the stdin envelope has
/// `jsonrpc: "2.0"`. Always emits a single JSON-RPC response on stdout and
/// returns `Ok(())` so the process exits cleanly.
pub(super) fn handle_jsonrpc_request(raw: Value, sync_path: &PathBuf) -> Result<()> {
    let req: JsonRpcRequest = match serde_json::from_value(raw) {
        Ok(req) => req,
        Err(err) => {
            return write_error(
                Value::Null,
                JsonRpcError::new(
                    error_codes::INVALID_REQUEST,
                    format!("Failed to parse JSON-RPC request: {err}"),
                    Some(
                        "Send a JSON object with `jsonrpc=\"2.0\"`, `method`, `params`, `id`."
                            .to_string(),
                    ),
                ),
            );
        }
    };

    if let Err(err) = req.validate() {
        return write_error(req.id.clone(), err);
    }

    let action = match method_to_action(&req.method) {
        Some(action) => action,
        None => {
            return write_error(req.id.clone(), JsonRpcError::method_not_found(&req.method));
        }
    };

    let (context, input) = match parse_method_params(&req.method, req.params.as_ref()) {
        Ok(parts) => parts,
        Err(err) => return write_error(req.id.clone(), err),
    };

    match super::guest::dispatch_guest_action(sync_path, &action, &context, &input) {
        Ok(Some(value)) => write_response(JsonRpcResponse::success(req.id, wrap_result(&action, value))),
        Ok(None) => write_response(JsonRpcResponse::success(req.id, Value::Null)),
        Err(err) => write_response(JsonRpcResponse::error(req.id, jsonrpc_error_from_guest(err))),
    }
}

/// Stdin contained text that did not parse as JSON. Reply with `-32700` and
/// `id: null` per JSON-RPC 2.0 §5.1.
pub(super) fn write_parse_error(detail: String) -> Result<()> {
    write_error(
        Value::Null,
        JsonRpcError::new(
            error_codes::PARSE_ERROR,
            format!("Parse error: {detail}"),
            Some("stdin must contain a single JSON value".to_string()),
        ),
    )
}

/// Stdin contained valid JSON but neither a `jsonrpc=\"2.0\"` envelope nor a
/// `version=\"guest.v1\"` envelope. Treat as JSON-RPC `-32600 Invalid Request`
/// since we have no legacy envelope to echo into.
pub(super) fn write_unknown_envelope_error() -> Result<()> {
    write_error(
        Value::Null,
        JsonRpcError::new(
            error_codes::INVALID_REQUEST,
            "Unknown envelope: expected jsonrpc=\"2.0\" or version=\"guest.v1\"".to_string(),
            Some(
                "Set `jsonrpc=\"2.0\"` (preferred) or `version=\"guest.v1\"` at the top level."
                    .to_string(),
            ),
        ),
    )
}

pub(crate) fn method_to_action(method: &str) -> Option<GuestAction> {
    match method {
        "capsule/payload.read" => Some(GuestAction::ReadPayload),
        "capsule/payload.write" => Some(GuestAction::WritePayload),
        "capsule/payload.update" => Some(GuestAction::UpdatePayload),
        "capsule/context.read" => Some(GuestAction::ReadContext),
        "capsule/context.write" => Some(GuestAction::WriteContext),
        // capsule/wasm.execute is reserved for a future PR (WASM/OCI runtime
        // integration). Returning None forces -32601 from the dispatcher.
        _ => None,
    }
}

pub(crate) fn parse_method_params(
    method: &str,
    params: Option<&Value>,
) -> Result<(GuestContext, Value), JsonRpcError> {
    let params_value = params.ok_or_else(|| {
        JsonRpcError::invalid_params(
            "Missing params object",
            "Send `params` with at least `{ context }`",
        )
    })?;

    let context_value = params_value.get("context").cloned().ok_or_else(|| {
        JsonRpcError::invalid_params(
            "Missing params.context",
            "All guest stdio methods require `params.context` (GuestContext schema)",
        )
    })?;
    let context: GuestContext = serde_json::from_value(context_value).map_err(|err| {
        JsonRpcError::invalid_params(
            &format!("params.context did not match GuestContext schema: {err}"),
            "Provide mode, role, permissions, sync_path, and host_app on the context object",
        )
    })?;

    let input = match method {
        "capsule/payload.read" | "capsule/context.read" => Value::Null,
        "capsule/payload.write" | "capsule/payload.update" => {
            let b64 = params_value
                .get("payload_b64")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    JsonRpcError::invalid_params(
                        "Missing params.payload_b64",
                        "Provide payload as a base64 string in `payload_b64`",
                    )
                })?;
            Value::String(b64.to_string())
        }
        "capsule/context.write" => params_value.get("value").cloned().unwrap_or(Value::Null),
        // method_to_action already filtered to known methods.
        _ => Value::Null,
    };

    Ok((context, input))
}

pub(crate) fn jsonrpc_error_from_guest(err: GuestError) -> JsonRpcError {
    let code = err.code.to_jsonrpc_code();
    let message = guest_error_label(&err);
    let hint = if err.message.is_empty() {
        None
    } else {
        Some(err.message.clone())
    };
    JsonRpcError::new(code, message, hint)
}

fn guest_error_label(err: &GuestError) -> String {
    match err.code {
        GuestErrorCode::PermissionDenied => format!("Permission denied: {}", err.message),
        GuestErrorCode::InvalidRequest => format!("Invalid request: {}", err.message),
        GuestErrorCode::ExecutionFailed => format!("Execution failed: {}", err.message),
        GuestErrorCode::HostUnavailable => format!("Host unavailable: {}", err.message),
        GuestErrorCode::ProtocolError => format!("Protocol error: {}", err.message),
        GuestErrorCode::IoError => format!("I/O error: {}", err.message),
    }
}

/// Wrap `dispatch_guest_action` results in a method-appropriate object envelope
/// so JSON-RPC consumers always see `{ "payload_b64": ... }` / `{ "value": ... }`
/// rather than a raw scalar (which would force casing logic in callers).
fn wrap_result(action: &GuestAction, raw: Value) -> Value {
    match action {
        GuestAction::ReadPayload => serde_json::json!({ "payload_b64": raw }),
        GuestAction::ReadContext => serde_json::json!({ "value": raw }),
        // ExecuteWasm is unreachable here (method_to_action returns None),
        // and the remaining write actions return Ok(None), so they don't
        // call wrap_result. Default keeps the raw value just in case.
        _ => raw,
    }
}

fn write_response(response: JsonRpcResponse) -> Result<()> {
    let json = serde_json::to_string(&response)?;
    let mut stdout = std::io::stdout();
    stdout.write_all(json.as_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

fn write_error(id: Value, error: JsonRpcError) -> Result<()> {
    write_response(JsonRpcResponse::error(id, error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guest_protocol::{GuestErrorCode, GuestMode, GuestPermission};

    fn sample_context_value() -> Value {
        serde_json::json!({
            "mode": "Headless",
            "role": "Owner",
            "permissions": {
                "can_read_payload": true,
                "can_read_context": true,
                "can_write_payload": false,
                "can_write_context": false,
                "can_execute_wasm": false,
                "allowed_hosts": [],
                "allowed_env": []
            },
            "sync_path": "/tmp/foo.zip",
            "host_app": null
        })
    }

    #[test]
    fn method_to_action_maps_known_methods() {
        assert!(matches!(
            method_to_action("capsule/payload.read"),
            Some(GuestAction::ReadPayload)
        ));
        assert!(matches!(
            method_to_action("capsule/payload.write"),
            Some(GuestAction::WritePayload)
        ));
        assert!(matches!(
            method_to_action("capsule/payload.update"),
            Some(GuestAction::UpdatePayload)
        ));
        assert!(matches!(
            method_to_action("capsule/context.read"),
            Some(GuestAction::ReadContext)
        ));
        assert!(matches!(
            method_to_action("capsule/context.write"),
            Some(GuestAction::WriteContext)
        ));
    }

    #[test]
    fn method_to_action_rejects_unknown_and_deferred() {
        assert!(method_to_action("capsule/wasm.execute").is_none());
        assert!(method_to_action("capsule/invoke").is_none());
        assert!(method_to_action("foo").is_none());
    }

    #[test]
    fn parse_method_params_requires_context() {
        let params = serde_json::json!({});
        let err = parse_method_params("capsule/payload.read", Some(&params)).unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("Missing params.context"));
    }

    #[test]
    fn parse_method_params_requires_payload_b64_for_writes() {
        let params = serde_json::json!({ "context": sample_context_value() });
        let err = parse_method_params("capsule/payload.write", Some(&params)).unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("payload_b64"));
    }

    #[test]
    fn parse_method_params_accepts_read_with_context_only() {
        let params = serde_json::json!({ "context": sample_context_value() });
        let (ctx, input) = parse_method_params("capsule/payload.read", Some(&params)).unwrap();
        assert!(matches!(ctx.mode, GuestMode::Headless));
        assert_eq!(input, Value::Null);
    }

    #[test]
    fn parse_method_params_accepts_write_with_payload_b64() {
        let params = serde_json::json!({
            "context": sample_context_value(),
            "payload_b64": "SGVsbG8="
        });
        let (_, input) = parse_method_params("capsule/payload.write", Some(&params)).unwrap();
        assert_eq!(input, Value::String("SGVsbG8=".to_string()));
    }

    #[test]
    fn parse_method_params_accepts_context_write_with_value() {
        let params = serde_json::json!({
            "context": sample_context_value(),
            "value": { "k": "v" }
        });
        let (_, input) = parse_method_params("capsule/context.write", Some(&params)).unwrap();
        assert_eq!(input, serde_json::json!({ "k": "v" }));
    }

    #[test]
    fn parse_method_params_missing_params_is_invalid_params() {
        let err = parse_method_params("capsule/payload.read", None).unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_PARAMS);
        assert!(err.message.contains("Missing params"));
    }

    #[test]
    fn jsonrpc_error_from_guest_maps_each_code() {
        let cases = [
            (GuestErrorCode::PermissionDenied, error_codes::PERMISSION_DENIED),
            (GuestErrorCode::InvalidRequest, error_codes::INVALID_PARAMS),
            (GuestErrorCode::ExecutionFailed, error_codes::INTERNAL_ERROR),
            (GuestErrorCode::HostUnavailable, error_codes::SERVICE_UNAVAILABLE),
            (GuestErrorCode::ProtocolError, error_codes::INVALID_REQUEST),
            (GuestErrorCode::IoError, error_codes::INTERNAL_ERROR),
        ];
        for (guest_code, expected) in cases {
            let err = jsonrpc_error_from_guest(GuestError::new(guest_code, "x"));
            assert_eq!(err.code, expected, "mapping mismatch for {guest_code:?}");
        }
    }

    #[test]
    fn permission_struct_default_used_in_sample_context() {
        // Sanity: the sample context decodes into the GuestContext shape we
        // claim in module docs.
        let value = sample_context_value();
        let ctx: GuestContext = serde_json::from_value(value).unwrap();
        assert_eq!(ctx.permissions.can_read_payload, true);
        assert!(matches!(ctx.role, crate::guest_protocol::GuestContextRole::Owner));
        assert_eq!(ctx.sync_path, "/tmp/foo.zip");
        // Avoid an unused-import warning in this scoped test.
        let _ = GuestPermission::default();
    }
}
