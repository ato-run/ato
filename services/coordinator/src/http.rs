//! HTTP surface for the selfhost Coordinator (G3 slice 2).
//!
//! The routes mirror the hosted data-resource subset so a client can talk to
//! either authority with the same JSON:
//!
//! - `GET  /health` (unauthenticated liveness)
//! - `POST /v1/data-resources`
//! - `GET  /v1/data-resources`
//! - `GET  /v1/data-resources/{id}`
//! - `POST /v1/data-resources/{id}/operations`
//! - `POST /v1/protocols/{protocol}/operations` (publisher-owned registry)
//! - `POST /v1/data-resources/{id}/bindings`
//! - `GET  /v1/bindings/{id}/document`
//! - `POST /v1/bindings/{id}/operations`
//! - `DELETE /v1/bindings/{id}`
//! - `POST /control/shutdown`
//!
//! Authentication is a single bearer token for the local coordinator. The
//! hosted conformance suite and quota/retention/GC are follow-up slices.

use std::sync::Mutex;

use bytes::Bytes;
use http_body_util::Full;
use hyper::{Method, Response, StatusCode};
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::apps::{self, DATA_PORTS_BRIDGE_JS, DATA_PORTS_BRIDGE_PATH};
use crate::{CommitInput, Coordinator, CoordinatorError};

pub type Body = Full<Bytes>;

/// Envelope served by GET /v1/data-resources/{id}/export.
const EXPORT_SCHEMA: &str = "ato.data.export/v1";
/// Envelope served by GET /v1/data-resources/export (the whole saved state).
const EXPORT_SET_SCHEMA: &str = "ato.data.export-set/v1";

/// When set, the Coordinator serves a static app and its Data Ports bridge.
#[derive(Debug, Clone)]
pub struct AppsConfig {
    pub dir: std::path::PathBuf,
    pub instance_id: String,
    /// The Port the served app declares (the bridge's default target).
    pub port: String,
}

pub struct ApiState {
    pub coordinator: Mutex<Coordinator>,
    pub token: String,
    pub shutdown: watch::Sender<bool>,
    pub apps: Option<AppsConfig>,
}

fn json_response(status: StatusCode, value: Value) -> Response<Body> {
    let body = serde_json::to_vec(&value).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("cache-control", "no-store")
        .body(Full::new(Bytes::from(body)))
        .expect("static response")
}

fn error_response(error: CoordinatorError) -> Response<Body> {
    let (status, code) = match &error {
        CoordinatorError::NotFound => (StatusCode::NOT_FOUND, "resource_not_found"),
        CoordinatorError::Conflict(code) => (StatusCode::CONFLICT, *code),
        CoordinatorError::Invalid(code) => (StatusCode::UNPROCESSABLE_ENTITY, *code),
        CoordinatorError::Forbidden(code) => (StatusCode::FORBIDDEN, *code),
        CoordinatorError::QuotaExceeded => (StatusCode::PAYLOAD_TOO_LARGE, "quota_exceeded"),
        CoordinatorError::HandlerFailed => (StatusCode::INTERNAL_SERVER_ERROR, "handler_failed"),
        CoordinatorError::Storage(_) | CoordinatorError::Json(_) => {
            (StatusCode::SERVICE_UNAVAILABLE, "storage_error")
        }
    };
    json_response(status, json!({ "error": code }))
}

fn authorized(authorization: Option<&str>, token: &str) -> bool {
    authorization
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(|value| value == token)
        .unwrap_or(false)
}

/// Handle one request. Kept independent of hyper's body type so tests can call
/// it directly with bytes.
pub async fn handle_request(
    state: &ApiState,
    method: &Method,
    path: &str,
    authorization: Option<&str>,
    body: &[u8],
) -> Response<Body> {
    if method == Method::GET && path == "/health" {
        let resources = state
            .coordinator
            .lock()
            .ok()
            .and_then(|coordinator| coordinator.list_resources("local").ok())
            .map(|resources| resources.len())
            .unwrap_or(0);
        return json_response(
            StatusCode::OK,
            json!({ "status": "ok", "schema_version": crate::SCHEMA_VERSION, "resources": resources }),
        );
    }
    if method == Method::GET
        && path == apps::RUNTIME_CONTEXT_PATH
        && let Some(apps) = &state.apps
    {
        {
            let script = apps::runtime_context_js(&apps.port);
            return Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/javascript; charset=utf-8")
                .header("cache-control", "no-store")
                .body(Full::new(Bytes::from(script)))
                .expect("static response");
        }
    }
    if method == Method::GET && path == DATA_PORTS_BRIDGE_PATH {
        return Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/javascript; charset=utf-8")
            .header("cache-control", "no-store")
            .body(Full::new(Bytes::from(DATA_PORTS_BRIDGE_JS)))
            .expect("static response");
    }
    // The bridge is same-origin and holds no secret: it is the served page
    // calling the local Coordinator. It exists only when an app is served.
    if let Some(apps) = &state.apps {
        if method == Method::GET && path == "/__ato/data-ports" {
            let coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            return match coordinator.bindings_for_instance(&apps.instance_id) {
                Ok(bindings) => json_response(
                    StatusCode::OK,
                    json!({
                        "instance_id": apps.instance_id,
                        "bindings": bindings
                    }),
                ),
                Err(error) => error_response(error),
            };
        }
        if method == Method::POST && path.starts_with("/__ato/data-ports/") {
            let rest = path.trim_start_matches("/__ato/data-ports/");
            let Some((port, "invoke")) = rest.split_once('/') else {
                return json_response(StatusCode::NOT_FOUND, json!({ "error": "not_found" }));
            };
            let input: Value = match serde_json::from_slice(body) {
                Ok(value) => value,
                Err(_) => {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        json!({ "error": "invalid_json" }),
                    );
                }
            };
            let operation = input
                .get("operation")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let idempotency_key = input
                .get("idempotency_key")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = input.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let mut coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            let binding = match coordinator.bindings_for_instance(&apps.instance_id) {
                Ok(bindings) => bindings
                    .into_iter()
                    .find(|binding| binding.port_name == port),
                Err(error) => return error_response(error),
            };
            let Some(binding) = binding else {
                return json_response(
                    StatusCode::NOT_FOUND,
                    json!({ "error": "data_port_not_connected" }),
                );
            };
            let resource = match coordinator.read_resource("local", &binding.resource_id) {
                Ok(resource) => resource,
                Err(error) => return error_response(error),
            };
            return match coordinator.invoke_binding_handler(
                &binding.binding_id,
                operation,
                &arguments,
                idempotency_key,
                resource.revision,
                resource.generation,
            ) {
                Ok(outcome) => json_response(StatusCode::OK, json!(outcome)),
                Err(error) => error_response(error),
            };
        }
    }
    // The served app's own files are public on this local origin.
    if method == Method::GET
        && let Some(apps) = &state.apps
        && let Some(file) = apps::resolve(&apps.dir, path)
    {
        let content_type = apps::content_type(&file);
        return match std::fs::read_to_string(&file) {
            Ok(text) if content_type.starts_with("text/html") => Response::builder()
                .status(StatusCode::OK)
                .header("content-type", content_type)
                .header("cache-control", "no-store")
                .body(Full::new(Bytes::from(apps::inject_runtime(&text))))
                .expect("static response"),
            Ok(text) => Response::builder()
                .status(StatusCode::OK)
                .header("content-type", content_type)
                .header("cache-control", "no-store")
                .body(Full::new(Bytes::from(text)))
                .expect("static response"),
            Err(_) => json_response(StatusCode::NOT_FOUND, json!({ "error": "not_found" })),
        };
    }
    if !authorized(authorization, &state.token) {
        return json_response(StatusCode::UNAUTHORIZED, json!({ "error": "unauthorized" }));
    }

    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    match (method, segments.as_slice()) {
        (&Method::POST, ["v1", "data-resources"]) => {
            let input: Value = match serde_json::from_slice(body) {
                Ok(value) => value,
                Err(_) => {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        json!({ "error": "invalid_json" }),
                    );
                }
            };
            let display_name = input.get("display_name").and_then(Value::as_str);
            let protocol = input
                .get("protocol")
                .and_then(Value::as_str)
                .unwrap_or("ato.data.json@1");
            let document = input.get("document").cloned().unwrap_or_else(|| json!({}));
            let mut coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            match coordinator.create_resource("local", protocol, display_name, document) {
                Ok(snapshot) => json_response(StatusCode::CREATED, json!(snapshot)),
                Err(error) => error_response(error),
            }
        }
        (&Method::GET, ["v1", "data-resources"]) => {
            let coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            match coordinator.list_resources("local") {
                Ok(resources) => json_response(StatusCode::OK, json!({ "resources": resources })),
                Err(error) => error_response(error),
            }
        }
        (&Method::GET, ["v1", "operations"]) => {
            let coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            match coordinator.list_operations(None) {
                Ok(operations) => {
                    json_response(StatusCode::OK, json!({ "operations": operations }))
                }
                Err(error) => error_response(error),
            }
        }
        (&Method::GET, ["v1", "protocols", protocol, "operations"]) => {
            let coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            match coordinator.list_operations(Some(protocol)) {
                Ok(operations) => {
                    json_response(StatusCode::OK, json!({ "operations": operations }))
                }
                Err(error) => error_response(error),
            }
        }
        (&Method::GET, ["v1", "bindings"]) => {
            let coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            match coordinator.list_bindings() {
                Ok(bindings) => json_response(StatusCode::OK, json!({ "bindings": bindings })),
                Err(error) => error_response(error),
            }
        }
        (&Method::GET, ["v1", "data-resources", "quota"]) => {
            let coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            match coordinator.quota("local") {
                Ok(quota) => json_response(StatusCode::OK, json!(quota)),
                Err(error) => error_response(error),
            }
        }
        // The whole owner's saved state as one file a snapshot can carry.
        (&Method::GET, ["v1", "data-resources", "export"]) => {
            let coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            match coordinator.list_resources("local") {
                Ok(resources) => {
                    let mut exported = Vec::new();
                    for resource in resources {
                        match coordinator.read_resource("local", &resource.resource_id) {
                            Ok(snapshot) => exported.push(json!({
                                "schema": EXPORT_SCHEMA,
                                "resource_id": snapshot.resource_id,
                                "protocol": snapshot.protocol,
                                "revision": snapshot.revision,
                                "generation": snapshot.generation,
                                "content_digest": snapshot.content_digest,
                                "byte_size": snapshot.byte_size,
                                "document": snapshot.document,
                            })),
                            Err(error) => return error_response(error),
                        }
                    }
                    json_response(
                        StatusCode::OK,
                        json!({ "schema": EXPORT_SET_SCHEMA, "resources": exported }),
                    )
                }
                Err(error) => error_response(error),
            }
        }
        (&Method::GET, ["v1", "data-resources", id]) => {
            let coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            match coordinator.read_resource("local", id) {
                Ok(snapshot) => json_response(StatusCode::OK, json!(snapshot)),
                Err(error) => error_response(error),
            }
        }
        (&Method::GET, ["v1", "data-resources", id, "export"]) => {
            let coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            match coordinator.read_resource("local", id) {
                Ok(snapshot) => json_response(
                    StatusCode::OK,
                    json!({
                        "schema": EXPORT_SCHEMA,
                        "resource_id": snapshot.resource_id,
                        "protocol": snapshot.protocol,
                        "revision": snapshot.revision,
                        "generation": snapshot.generation,
                        "content_digest": snapshot.content_digest,
                        "byte_size": snapshot.byte_size,
                        "document": snapshot.document,
                    }),
                ),
                Err(error) => error_response(error),
            }
        }
        (&Method::GET, ["v1", "data-resources", id, "document"]) => {
            let coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            match coordinator.read_resource("local", id) {
                Ok(snapshot) => json_response(StatusCode::OK, json!(snapshot)),
                Err(error) => error_response(error),
            }
        }
        (&Method::POST, ["v1", "data-resources", id, "operations"]) => {
            let input: Value = match serde_json::from_slice(body) {
                Ok(value) => value,
                Err(_) => {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        json!({ "error": "invalid_json" }),
                    );
                }
            };
            let operation_id = input
                .get("operation_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let expected_revision = input
                .get("expected_revision")
                .and_then(Value::as_i64)
                .unwrap_or(-1);
            let expected_generation = input
                .get("expected_generation")
                .and_then(Value::as_i64)
                .unwrap_or(-1);
            let document = input.get("document").cloned().unwrap_or_else(|| json!({}));
            let mut coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            match coordinator.commit(
                "local",
                id,
                CommitInput {
                    operation_id,
                    expected_revision,
                    expected_generation,
                    document,
                },
            ) {
                Ok(outcome) => json_response(StatusCode::OK, json!(outcome)),
                Err(error) => error_response(error),
            }
        }
        (&Method::POST, ["v1", "protocols", protocol, "operations"]) => {
            let input: Value = match serde_json::from_slice(body) {
                Ok(value) => value,
                Err(_) => {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        json!({ "error": "invalid_json" }),
                    );
                }
            };
            let operation = input
                .get("operation")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let semantics = input
                .get("semantics")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let access = input
                .get("access")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let implementation_digest = input
                .get("implementation_digest")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let publisher = input
                .get("publisher")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let module_source = input.get("module_source").and_then(Value::as_str);
            let module_export = input.get("module_export").and_then(Value::as_str);
            let mut coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            let registered = match (module_source, module_export) {
                (Some(source), Some(export)) => coordinator.register_handler_operation(
                    protocol, operation, semantics, access, source, export, publisher,
                ),
                _ => coordinator.register_operation(
                    protocol,
                    operation,
                    semantics,
                    access,
                    implementation_digest,
                    publisher,
                ),
            };
            match registered {
                Ok(()) => json_response(StatusCode::CREATED, json!({ "status": "registered" })),
                Err(error) => error_response(error),
            }
        }
        (&Method::POST, ["v1", "data-resources", id, "bindings"]) => {
            let input: Value = match serde_json::from_slice(body) {
                Ok(value) => value,
                Err(_) => {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        json!({ "error": "invalid_json" }),
                    );
                }
            };
            let port = input
                .get("port")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let protocol = input
                .get("protocol")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let access = input
                .get("access")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let operations = input
                .get("operations")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let mut coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            let instance_id = input.get("instance_id").and_then(Value::as_str);
            let bound = match instance_id {
                Some(instance_id) => coordinator.bind_resource_for_instance(
                    id,
                    port,
                    protocol,
                    access,
                    operations,
                    instance_id,
                ),
                None => coordinator.bind_resource(id, port, protocol, access, operations),
            };
            match bound {
                Ok(binding) => json_response(StatusCode::CREATED, json!(binding)),
                Err(error) => error_response(error),
            }
        }
        (&Method::GET, ["v1", "bindings", id, "document"]) => {
            let coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            match coordinator.read_binding_document(id) {
                Ok(snapshot) => json_response(StatusCode::OK, json!(snapshot)),
                Err(error) => error_response(error),
            }
        }
        (&Method::POST, ["v1", "bindings", id, "operations"]) => {
            let input: Value = match serde_json::from_slice(body) {
                Ok(value) => value,
                Err(_) => {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        json!({ "error": "invalid_json" }),
                    );
                }
            };
            let operation = input
                .get("operation")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let operation_id = input
                .get("operation_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let expected_revision = input
                .get("expected_revision")
                .and_then(Value::as_i64)
                .unwrap_or(-1);
            let expected_generation = input
                .get("expected_generation")
                .and_then(Value::as_i64)
                .unwrap_or(-1);
            let document = input.get("document").cloned().unwrap_or_else(|| json!({}));
            let mut coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            let arguments = input.get("arguments").cloned();
            let outcome = match arguments {
                Some(arguments) => coordinator
                    .invoke_binding_handler(
                        id,
                        &operation,
                        &arguments,
                        &operation_id,
                        expected_revision,
                        expected_generation,
                    )
                    .map(|outcome| json!(outcome)),
                None => coordinator
                    .invoke_binding_operation(
                        id,
                        &operation,
                        CommitInput {
                            operation_id,
                            expected_revision,
                            expected_generation,
                            document,
                        },
                    )
                    .map(|outcome| json!(outcome)),
            };
            match outcome {
                Ok(value) => json_response(StatusCode::OK, value),
                Err(error) => error_response(error),
            }
        }
        (&Method::DELETE, ["v1", "bindings", id]) => {
            let mut coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            match coordinator.revoke_binding(id) {
                Ok(binding) => json_response(StatusCode::OK, json!(binding)),
                Err(error) => error_response(error),
            }
        }
        (
            &Method::POST,
            [
                "v1",
                "protocols",
                protocol,
                "operations",
                operation,
                "expose",
            ],
        ) => {
            let input: Value = match serde_json::from_slice(body) {
                Ok(value) => value,
                Err(_) => {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        json!({ "error": "invalid_json" }),
                    );
                }
            };
            let expose_mcp = input.get("mcp").and_then(Value::as_bool).unwrap_or(false);
            let mut coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            match coordinator.set_operation_exposure(protocol, operation, expose_mcp) {
                Ok(()) => json_response(
                    StatusCode::OK,
                    json!({ "status": "updated", "mcp": expose_mcp }),
                ),
                Err(error) => error_response(error),
            }
        }
        (&Method::POST, ["mcp"]) => {
            let request: Value = match serde_json::from_slice(body) {
                Ok(value) => value,
                Err(_) => {
                    return json_response(
                        StatusCode::BAD_REQUEST,
                        json!({ "error": "invalid_json" }),
                    );
                }
            };
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            let method = request
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let reply = |result: Value| json!({ "jsonrpc": "2.0", "id": id, "result": result });
            let reply_error = |code: i64, message: &str| json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } });
            let mut coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            match method {
                // MCP clients send this notification after initialize; it is not
                // a request and must not be answered with an error.
                "notifications/initialized" => json_response(StatusCode::ACCEPTED, json!({})),
                "ping" => json_response(StatusCode::OK, reply(json!({}))),
                "initialize" => json_response(
                    StatusCode::OK,
                    reply(json!({
                        "protocolVersion": "2025-06-18",
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "ato-coordinator", "version": env!("CARGO_PKG_VERSION") }
                    })),
                ),
                "tools/list" => match coordinator.mcp_tools() {
                    Ok(tools) => json_response(
                        StatusCode::OK,
                        reply(json!({
                            "tools": tools.into_iter().map(|tool| json!({
                                "name": tool.operation,
                                "description": format!("{} ({})", tool.protocol, tool.semantics),
                                "inputSchema": { "type": "object" }
                            })).collect::<Vec<_>>()
                        })),
                    ),
                    Err(error) => {
                        json_response(StatusCode::OK, reply_error(-32000, &error.to_string()))
                    }
                },
                "tools/call" => {
                    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
                    let name = params
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    let arguments = params
                        .get("arguments")
                        .cloned()
                        .unwrap_or_else(|| json!({}));
                    let binding =
                        coordinator
                            .bindings_for_instance("mcp")
                            .ok()
                            .and_then(|bindings| {
                                bindings.into_iter().find(|binding| {
                                    binding
                                        .operations
                                        .iter()
                                        .any(|operation| operation == &name)
                                })
                            });
                    let Some(binding) = binding else {
                        return json_response(
                            StatusCode::OK,
                            reply(json!({
                                "content": [{ "type": "text", "text": "operation_not_granted" }],
                                "isError": true
                            })),
                        );
                    };
                    match coordinator.read_resource("local", &binding.resource_id) {
                        Err(error) => json_response(
                            StatusCode::OK,
                            reply(json!({
                                "content": [{ "type": "text", "text": error.to_string() }],
                                "isError": true
                            })),
                        ),
                        Ok(resource) => {
                            // JSON-RPC ids may be numbers or strings; the
                            // operation id must distinguish them or two calls
                            // with different ids would collide.
                            let id_text = match &id {
                                Value::String(text) => text.clone(),
                                other => other.to_string(),
                            };
                            let operation_id = format!("mcp:{id_text}");
                            match coordinator.invoke_binding_handler(
                                &binding.binding_id,
                                &name,
                                &arguments,
                                &operation_id,
                                resource.revision,
                                resource.generation,
                            ) {
                                Ok(outcome) => json_response(
                                    StatusCode::OK,
                                    reply(json!({
                                        "content": [{ "type": "text", "text": json!(outcome).to_string() }],
                                        "isError": false
                                    })),
                                ),
                                Err(error) => json_response(
                                    StatusCode::OK,
                                    reply(json!({
                                        "content": [{ "type": "text", "text": error.to_string() }],
                                        "isError": true
                                    })),
                                ),
                            }
                        }
                    }
                }
                _ => json_response(StatusCode::OK, reply_error(-32601, "method_not_found")),
            }
        }
        (&Method::POST, ["control", "gc"]) => {
            let mut coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            match coordinator.gc_expired_results() {
                Ok(deleted) => json_response(StatusCode::OK, json!({ "deleted_results": deleted })),
                Err(error) => error_response(error),
            }
        }
        (&Method::POST, ["control", "shutdown"]) => {
            let _ = state.shutdown.send(true);
            json_response(StatusCode::OK, json!({ "status": "stopping" }))
        }
        (&Method::GET, _) => json_response(StatusCode::NOT_FOUND, json!({ "error": "not_found" })),
        _ => json_response(StatusCode::NOT_FOUND, json!({ "error": "not_found" })),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Coordinator;
    use http_body_util::BodyExt;
    use tokio::sync::watch;

    fn state() -> ApiState {
        let (shutdown, _receiver) = watch::channel(false);
        ApiState {
            coordinator: Mutex::new(Coordinator::open_in_memory().unwrap()),
            token: "test-token".to_owned(),
            shutdown,
            apps: None,
        }
    }

    async fn call(
        state: &ApiState,
        method: Method,
        path: &str,
        authorization: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        let bytes = serde_json::to_vec(&body).unwrap();
        let response = handle_request(state, &method, path, authorization, &bytes).await;
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&body).unwrap())
    }

    #[tokio::test]
    async fn health_is_unauthenticated() {
        let state = state();
        let (status, body) = call(&state, Method::GET, "/health", None, json!({})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "ok");
        assert_eq!(body["resources"], 0);
    }

    #[tokio::test]
    async fn rejects_a_missing_or_wrong_token() {
        let state = state();
        let (status, body) = call(&state, Method::GET, "/v1/data-resources", None, json!({})).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"], "unauthorized");
        let (status, _) = call(
            &state,
            Method::GET,
            "/v1/data-resources",
            Some("Bearer wrong"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn creates_lists_reads_and_commits() {
        let state = state();
        let (status, created) = call(
            &state,
            Method::POST,
            "/v1/data-resources",
            Some("Bearer test-token"),
            json!({"display_name": "TODO", "protocol": "todo.board@1", "document": {"tasks": {}}}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(created["revision"], 1);
        let resource_id = created["resource_id"].as_str().unwrap().to_owned();

        let (status, listed) = call(
            &state,
            Method::GET,
            "/v1/data-resources",
            Some("Bearer test-token"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed["resources"].as_array().unwrap().len(), 1);

        let (status, read) = call(
            &state,
            Method::GET,
            &format!("/v1/data-resources/{resource_id}"),
            Some("Bearer test-token"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(read["document"], json!({"tasks": {}}));

        let (status, committed) = call(
            &state,
            Method::POST,
            &format!("/v1/data-resources/{resource_id}/operations"),
            Some("Bearer test-token"),
            json!({
                "operation_id": "op-1",
                "expected_revision": 1,
                "expected_generation": 0,
                "document": {"tasks": {"t1": {"id": "t1"}}}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(committed["revision"], 2);
        assert_eq!(committed["replayed"], false);
        assert_eq!(committed["receipt"]["revision"], 2);
    }

    #[tokio::test]
    async fn replays_and_reports_conflicts() {
        let state = state();
        let (_, created) = call(
            &state,
            Method::POST,
            "/v1/data-resources",
            Some("Bearer test-token"),
            json!({"protocol": "todo.board@1", "document": {}}),
        )
        .await;
        let resource_id = created["resource_id"].as_str().unwrap().to_owned();
        let path = format!("/v1/data-resources/{resource_id}/operations");
        let body = json!({
            "operation_id": "op-1",
            "expected_revision": 1,
            "expected_generation": 0,
            "document": {"tasks": {"t1": {"id": "t1"}}}
        });
        let (status, first) = call(
            &state,
            Method::POST,
            &path,
            Some("Bearer test-token"),
            body.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, replay) =
            call(&state, Method::POST, &path, Some("Bearer test-token"), body).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(replay["replayed"], true);
        assert_eq!(replay["revision"], first["revision"]);

        let (status, conflict) = call(
            &state,
            Method::POST,
            &path,
            Some("Bearer test-token"),
            json!({
                "operation_id": "op-1",
                "expected_revision": 2,
                "expected_generation": 0,
                "document": {"tasks": {}}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(conflict["error"], "operation_id_conflict");

        let (status, stale) = call(
            &state,
            Method::POST,
            &path,
            Some("Bearer test-token"),
            json!({
                "operation_id": "op-2",
                "expected_revision": 1,
                "expected_generation": 0,
                "document": {"tasks": {}}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(stale["error"], "revision_conflict");
    }

    #[tokio::test]
    async fn unknown_resource_is_404() {
        let state = state();
        let (status, body) = call(
            &state,
            Method::GET,
            "/v1/data-resources/dres_missing",
            Some("Bearer test-token"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "resource_not_found");
    }

    async fn register_due_dates(state: &ApiState) {
        let (status, _) = call(
            state,
            Method::POST,
            "/v1/protocols/todo.board@1/operations",
            Some("Bearer test-token"),
            json!({
                "operation": "todo_list_due_dates",
                "semantics": "observation",
                "access": "read",
                "implementation_digest": "sha256:due",
                "publisher": "alice/app"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, _) = call(
            state,
            Method::POST,
            "/v1/protocols/todo.board@1/operations",
            Some("Bearer test-token"),
            json!({
                "operation": "todo_create",
                "semantics": "evolution",
                "access": "read_write",
                "implementation_digest": "sha256:create",
                "publisher": "alice/app"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }

    #[tokio::test]
    async fn refuses_a_conflicting_protocol_publisher() {
        let state = state();
        register_due_dates(&state).await;
        let (status, body) = call(
            &state,
            Method::POST,
            "/v1/protocols/todo.board@1/operations",
            Some("Bearer test-token"),
            json!({
                "operation": "todo_list_due_dates",
                "semantics": "observation",
                "access": "read",
                "implementation_digest": "sha256:other",
                "publisher": "mallory/app"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "protocol_operation_publisher_conflict");
    }

    #[tokio::test]
    async fn operation_only_binding_cannot_read_the_document() {
        let state = state();
        register_due_dates(&state).await;
        let (_, created) = call(
            &state,
            Method::POST,
            "/v1/data-resources",
            Some("Bearer test-token"),
            json!({"protocol": "todo.board@1", "document": {"tasks": {"t1": {"id": "t1", "dueAt": "2026-11-20"}}}}),
        )
        .await;
        let resource_id = created["resource_id"].as_str().unwrap().to_owned();
        let (status, binding) = call(
            &state,
            Method::POST,
            &format!("/v1/data-resources/{resource_id}/bindings"),
            Some("Bearer test-token"),
            json!({
                "port": "tasks",
                "protocol": "todo.board@1",
                "access": "operations",
                "operations": ["todo_list_due_dates"]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let binding_id = binding["binding_id"].as_str().unwrap().to_owned();

        let (status, body) = call(
            &state,
            Method::GET,
            &format!("/v1/bindings/{binding_id}/document"),
            Some("Bearer test-token"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "operation_only_binding");

        let (status, body) = call(
            &state,
            Method::POST,
            &format!("/v1/bindings/{binding_id}/operations"),
            Some("Bearer test-token"),
            json!({
                "operation": "todo_create",
                "operation_id": "op-create",
                "expected_revision": 1,
                "expected_generation": 0,
                "document": {"tasks": {}}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "operation_not_granted");

        let (status, outcome) = call(
            &state,
            Method::POST,
            &format!("/v1/bindings/{binding_id}/operations"),
            Some("Bearer test-token"),
            json!({
                "operation": "todo_list_due_dates",
                "operation_id": "op-due",
                "expected_revision": 1,
                "expected_generation": 0,
                "document": {"tasks": {"t1": {"id": "t1", "dueAt": "2026-11-20"}}}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(outcome["revision"], 2);

        let (status, _) = call(
            &state,
            Method::DELETE,
            &format!("/v1/bindings/{binding_id}"),
            Some("Bearer test-token"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = call(
            &state,
            Method::POST,
            &format!("/v1/bindings/{binding_id}/operations"),
            Some("Bearer test-token"),
            json!({
                "operation": "todo_list_due_dates",
                "operation_id": "op-due-2",
                "expected_revision": 2,
                "expected_generation": 0,
                "document": {}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "binding_revoked");
    }

    #[tokio::test]
    async fn document_binding_can_read_but_not_write_through_a_read_binding() {
        let state = state();
        register_due_dates(&state).await;
        let (_, created) = call(
            &state,
            Method::POST,
            "/v1/data-resources",
            Some("Bearer test-token"),
            json!({"protocol": "todo.board@1", "document": {"tasks": {}}}),
        )
        .await;
        let resource_id = created["resource_id"].as_str().unwrap().to_owned();
        let (status, binding) = call(
            &state,
            Method::POST,
            &format!("/v1/data-resources/{resource_id}/bindings"),
            Some("Bearer test-token"),
            json!({"port": "tasks", "protocol": "todo.board@1", "access": "read"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let binding_id = binding["binding_id"].as_str().unwrap().to_owned();
        let (status, document) = call(
            &state,
            Method::GET,
            &format!("/v1/bindings/{binding_id}/document"),
            Some("Bearer test-token"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(document["document"], json!({"tasks": {}}));

        let (status, body) = call(
            &state,
            Method::POST,
            &format!("/v1/bindings/{binding_id}/operations"),
            Some("Bearer test-token"),
            json!({
                "operation": "todo_create",
                "operation_id": "op-create",
                "expected_revision": 1,
                "expected_generation": 0,
                "document": {"tasks": {}}
            }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"], "binding_read_only");
    }

    #[tokio::test]
    async fn invokes_a_registered_handler_through_a_binding() {
        let state = state();
        let module = r#"
function boardOf(state) {
  return state && state.document && typeof state.document === "object" ? state.document : {};
}
export function createTask({ arguments: args, state }) {
  const board = boardOf(state);
  const tasks = Object.assign({}, board.tasks || {});
  tasks["task-1"] = { id: "task-1", title: String(args.title), dueAt: null, completed: false };
  return { operations: [{ kind: "replace", value: { tasks } }], result: { task_id: "task-1" } };
}
"#;
        let (status, _) = call(
            &state,
            Method::POST,
            "/v1/protocols/todo.board@1/operations",
            Some("Bearer test-token"),
            json!({
                "operation": "todo_create",
                "semantics": "evolution",
                "access": "read_write",
                "module_source": module,
                "module_export": "createTask",
                "publisher": "alice/app"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (_, created) = call(
            &state,
            Method::POST,
            "/v1/data-resources",
            Some("Bearer test-token"),
            json!({"protocol": "todo.board@1", "document": {"tasks": {}}}),
        )
        .await;
        let resource_id = created["resource_id"].as_str().unwrap().to_owned();
        let (status, binding) = call(
            &state,
            Method::POST,
            &format!("/v1/data-resources/{resource_id}/bindings"),
            Some("Bearer test-token"),
            json!({
                "port": "tasks",
                "protocol": "todo.board@1",
                "access": "operations",
                "operations": ["todo_create"]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let binding_id = binding["binding_id"].as_str().unwrap().to_owned();
        let (status, outcome) = call(
            &state,
            Method::POST,
            &format!("/v1/bindings/{binding_id}/operations"),
            Some("Bearer test-token"),
            json!({
                "operation": "todo_create",
                "arguments": {"title": "From a handler"},
                "operation_id": "op-create",
                "expected_revision": 1,
                "expected_generation": 0
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{outcome}");
        assert_eq!(outcome["revision"], 2);
        assert_eq!(outcome["result"]["task_id"], "task-1");
        assert!(outcome["receipt"].is_object());
    }

    #[tokio::test]
    async fn reports_quota_and_runs_gc() {
        let state = state();
        let (status, created) = call(
            &state,
            Method::POST,
            "/v1/data-resources",
            Some("Bearer test-token"),
            json!({"protocol": "todo.board@1", "document": {"tasks": {}}}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, quota) = call(
            &state,
            Method::GET,
            "/v1/data-resources/quota",
            Some("Bearer test-token"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(quota["resources"], 1);
        assert_eq!(quota["used_bytes"], created["byte_size"]);
        assert!(quota["limit_bytes"].as_i64().unwrap() > 0);

        let (status, gc) = call(
            &state,
            Method::POST,
            "/control/gc",
            Some("Bearer test-token"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(gc["deleted_results"], 0);
    }

    async fn call_raw(
        state: &ApiState,
        method: Method,
        path: &str,
        authorization: Option<&str>,
        body: Value,
    ) -> (StatusCode, String, String) {
        let bytes = serde_json::to_vec(&body).unwrap();
        let response = handle_request(state, &method, path, authorization, &bytes).await;
        let status = response.status();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            content_type,
            String::from_utf8_lossy(&body).into_owned(),
        )
    }

    #[tokio::test]
    async fn serves_an_app_and_its_data_ports_bridge() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("index.html"), "<h1>served app</h1>").unwrap();
        let (shutdown, _receiver) = watch::channel(false);
        let state = ApiState {
            coordinator: Mutex::new(Coordinator::open_in_memory().unwrap()),
            token: "test-token".to_owned(),
            shutdown,
            apps: Some(crate::http::AppsConfig {
                dir: directory.path().to_path_buf(),
                instance_id: "app-1".to_owned(),
                port: "tasks".to_owned(),
            }),
        };
        let module = r#"
export function createTask({ arguments: args, state }) {
  const board = state && state.document ? state.document : {};
  const tasks = Object.assign({}, board.tasks || {});
  tasks["task-1"] = { id: "task-1", title: String(args.title), dueAt: null, completed: false };
  return { operations: [{ kind: "replace", value: { tasks } }], result: { task_id: "task-1" } };
}
"#;
        {
            let mut coordinator = state.coordinator.lock().unwrap();
            coordinator
                .register_handler_operation(
                    "todo.board@1",
                    "todo_create",
                    "evolution",
                    "read_write",
                    module,
                    "createTask",
                    "alice/app",
                )
                .unwrap();
            let created = coordinator
                .create_resource("local", "todo.board@1", None, json!({"tasks": {}}))
                .unwrap();
            coordinator
                .bind_resource_for_instance(
                    &created.resource_id,
                    "tasks",
                    "todo.board@1",
                    "operations",
                    vec!["todo_create".into()],
                    "app-1",
                )
                .unwrap();
        }

        let (status, content_type, body) =
            call_raw(&state, Method::GET, "/", None, json!({})).await;
        assert_eq!(status, StatusCode::OK);
        assert!(content_type.starts_with("text/html"));
        assert!(body.contains("served app"));

        let (status, _, bridge) =
            call_raw(&state, Method::GET, "/__ato/data-ports", None, json!({})).await;
        assert_eq!(status, StatusCode::OK);
        let bridge: Value = serde_json::from_str(&bridge).unwrap();
        assert_eq!(bridge["instance_id"], "app-1");
        assert_eq!(bridge["bindings"][0]["port_name"], "tasks");
        assert_eq!(bridge["bindings"][0]["access"], "operations");

        let (status, _, outcome) = call_raw(
            &state,
            Method::POST,
            "/__ato/data-ports/tasks/invoke",
            None,
            json!({
                "operation": "todo_create",
                "arguments": {"title": "From the served app"},
                "idempotency_key": "ui-1"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{outcome}");
        let outcome: Value = serde_json::from_str(&outcome).unwrap();
        assert_eq!(outcome["revision"], 2);
        assert_eq!(outcome["result"]["task_id"], "task-1");

        // A path outside the app directory is not served.
        let (status, _, _) = call_raw(&state, Method::GET, "/../secret", None, json!({})).await;
        // Not served; the request falls through to the authenticated API and
        // is refused there.
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn serves_mcp_tools_with_grant_and_revocation() {
        let state = state();
        let module = r#"
export function createTask({ arguments: args, state }) {
  const board = state && state.document ? state.document : {};
  const tasks = Object.assign({}, board.tasks || {});
  tasks["mcp-1"] = { id: "mcp-1", title: String(args.title), dueAt: null, completed: false };
  return { operations: [{ kind: "replace", value: { tasks } }], result: { task_id: "mcp-1" } };
}
"#;
        let (status, _) = call(
            &state,
            Method::POST,
            "/v1/protocols/todo.board@1/operations",
            Some("Bearer test-token"),
            json!({
                "operation": "todo_create",
                "semantics": "evolution",
                "access": "read_write",
                "module_source": module,
                "module_export": "createTask",
                "publisher": "alice/app"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, _) = call(
            &state,
            Method::POST,
            "/v1/protocols/todo.board@1/operations/todo_create/expose",
            Some("Bearer test-token"),
            json!({"mcp": true}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (_, created) = call(
            &state,
            Method::POST,
            "/v1/data-resources",
            Some("Bearer test-token"),
            json!({"protocol": "todo.board@1", "document": {"tasks": {}}}),
        )
        .await;
        let resource_id = created["resource_id"].as_str().unwrap().to_owned();
        let (status, binding) = call(
            &state,
            Method::POST,
            &format!("/v1/data-resources/{resource_id}/bindings"),
            Some("Bearer test-token"),
            json!({
                "port": "tasks",
                "protocol": "todo.board@1",
                "access": "operations",
                "operations": ["todo_create"],
                "instance_id": "mcp"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let binding_id = binding["binding_id"].as_str().unwrap().to_owned();

        // MCP requires the Coordinator token.
        let (status, _, _) = call_raw(
            &state,
            Method::POST,
            "/mcp",
            None,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        let (status, _, initialize) = call_raw(
            &state,
            Method::POST,
            "/mcp",
            Some("Bearer test-token"),
            json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let initialize: Value = serde_json::from_str(&initialize).unwrap();
        assert_eq!(
            initialize["result"]["serverInfo"]["name"],
            "ato-coordinator"
        );

        let (_, _, listed) = call_raw(
            &state,
            Method::POST,
            "/mcp",
            Some("Bearer test-token"),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .await;
        let listed: Value = serde_json::from_str(&listed).unwrap();
        assert_eq!(listed["result"]["tools"][0]["name"], "todo_create");

        let (_, _, called) = call_raw(
            &state,
            Method::POST,
            "/mcp",
            Some("Bearer test-token"),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"todo_create","arguments":{"title":"via MCP"}}}),
        )
        .await;
        let called: Value = serde_json::from_str(&called).unwrap();
        assert_eq!(called["result"]["isError"], false, "{called}");
        let payload: Value =
            serde_json::from_str(called["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(payload["revision"], 2);
        assert_eq!(payload["result"]["task_id"], "mcp-1");

        // Revocation closes the MCP path too.
        let (status, _) = call(
            &state,
            Method::DELETE,
            &format!("/v1/bindings/{binding_id}"),
            Some("Bearer test-token"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (_, _, after) = call_raw(
            &state,
            Method::POST,
            "/mcp",
            Some("Bearer test-token"),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"todo_create","arguments":{"title":"nope"}}}),
        )
        .await;
        let after: Value = serde_json::from_str(&after).unwrap();
        assert_eq!(after["result"]["isError"], true, "{after}");
    }

    #[tokio::test]
    async fn lists_bindings_for_admin() {
        let state = state();
        let (_, created) = call(
            &state,
            Method::POST,
            "/v1/data-resources",
            Some("Bearer test-token"),
            json!({"protocol": "todo.board@1", "document": {}}),
        )
        .await;
        let resource_id = created["resource_id"].as_str().unwrap().to_owned();
        let (status, _) = call(
            &state,
            Method::POST,
            &format!("/v1/data-resources/{resource_id}/bindings"),
            Some("Bearer test-token"),
            json!({"port": "tasks", "protocol": "todo.board@1", "access": "read"}),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, listed) = call(
            &state,
            Method::GET,
            "/v1/bindings",
            Some("Bearer test-token"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed["bindings"].as_array().unwrap().len(), 1);
        assert_eq!(listed["bindings"][0]["port_name"], "tasks");
        let (status, _) = call(&state, Method::GET, "/v1/bindings", None, json!({})).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn lists_operations_over_http() {
        let state = state();
        let (status, _) = call(
            &state,
            Method::POST,
            "/v1/protocols/todo.board@1/operations",
            Some("Bearer test-token"),
            json!({
                "operation": "todo_create",
                "semantics": "evolution",
                "access": "read_write",
                "module_source": "export function createTask(){}",
                "module_export": "createTask",
                "publisher": "alice/app"
            }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, listed) = call(
            &state,
            Method::GET,
            "/v1/protocols/todo.board@1/operations",
            Some("Bearer test-token"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed["operations"][0]["operation"], "todo_create");
        let (status, all) = call(
            &state,
            Method::GET,
            "/v1/operations",
            Some("Bearer test-token"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(all["operations"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn accepts_mcp_notifications_without_an_error() {
        let state = state();
        let (status, _, body) = call_raw(
            &state,
            Method::POST,
            "/mcp",
            Some("Bearer test-token"),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        let (status, _, pong) = call_raw(
            &state,
            Method::POST,
            "/mcp",
            Some("Bearer test-token"),
            json!({"jsonrpc": "2.0", "id": 9, "method": "ping"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let pong: Value = serde_json::from_str(&pong).unwrap();
        assert_eq!(pong["id"], 9);
        assert_eq!(pong["result"], json!({}));
    }

    #[tokio::test]
    async fn exports_a_document_with_its_digest() {
        let state = state();
        let (_, created) = call(
            &state,
            Method::POST,
            "/v1/data-resources",
            Some("Bearer test-token"),
            json!({"protocol": "todo.board@1", "document": {"tasks": {"t1": {"id": "t1"}}}}),
        )
        .await;
        let resource_id = created["resource_id"].as_str().unwrap().to_owned();
        let (status, exported) = call(
            &state,
            Method::GET,
            &format!("/v1/data-resources/{resource_id}/document"),
            Some("Bearer test-token"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(exported["revision"], 1);
        assert_eq!(exported["document"]["tasks"]["t1"]["id"], "t1");
        assert_eq!(exported["content_digest"], created["content_digest"]);
    }

    #[tokio::test]
    async fn exports_the_cream_envelope() {
        let state = state();
        let (_, created) = call(
            &state,
            Method::POST,
            "/v1/data-resources",
            Some("Bearer test-token"),
            json!({"protocol": "todo.board@1", "document": {"tasks": {"t1": {"id": "t1"}}}}),
        )
        .await;
        let resource_id = created["resource_id"].as_str().unwrap().to_owned();
        let (status, export) = call(
            &state,
            Method::GET,
            &format!("/v1/data-resources/{resource_id}/export"),
            Some("Bearer test-token"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(export["schema"], "ato.data.export/v1");
        assert_eq!(export["resource_id"], resource_id);
        assert_eq!(export["protocol"], "todo.board@1");
        assert_eq!(export["content_digest"], created["content_digest"]);
        assert_eq!(export["document"]["tasks"]["t1"]["id"], "t1");
    }

    #[tokio::test]
    async fn exports_the_whole_saved_state() {
        let state = state();
        for document in [json!({"tasks": {}}), json!({"tasks": {"t1": {"id": "t1"}}})] {
            let (status, _) = call(
                &state,
                Method::POST,
                "/v1/data-resources",
                Some("Bearer test-token"),
                json!({"protocol": "todo.board@1", "document": document}),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }
        let (status, export) = call(
            &state,
            Method::GET,
            "/v1/data-resources/export",
            Some("Bearer test-token"),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(export["schema"], "ato.data.export-set/v1");
        assert_eq!(export["resources"].as_array().unwrap().len(), 2);
        for resource in export["resources"].as_array().unwrap() {
            assert_eq!(resource["schema"], "ato.data.export/v1");
            assert!(
                resource["content_digest"]
                    .as_str()
                    .unwrap()
                    .starts_with("sha256:")
            );
        }
    }
}
