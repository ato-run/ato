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

use crate::{CommitInput, Coordinator, CoordinatorError};

pub type Body = Full<Bytes>;

pub struct ApiState {
    pub coordinator: Mutex<Coordinator>,
    pub token: String,
    pub shutdown: watch::Sender<bool>,
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
            let mut coordinator = match state.coordinator.lock() {
                Ok(coordinator) => coordinator,
                Err(_) => {
                    return json_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        json!({ "error": "storage_error" }),
                    );
                }
            };
            match coordinator.register_operation(
                protocol,
                operation,
                semantics,
                access,
                implementation_digest,
                publisher,
            ) {
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
            match coordinator.bind_resource(id, port, protocol, access, operations) {
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
            match coordinator.invoke_binding_operation(
                id,
                &operation,
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
}
