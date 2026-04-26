use super::*;

pub(super) async fn handle_list_local_processes() -> impl IntoResponse {
    let pm = match ProcessManager::new() {
        Ok(pm) => pm,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "process_manager_error",
                &err.to_string(),
            )
        }
    };
    let cleaned = match pm.cleanup_dead_processes_with_details() {
        Ok(cleaned) => cleaned,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "process_cleanup_failed",
                &err.to_string(),
            )
        }
    };
    for process in &cleaned {
        if let Err(err) = binding::cleanup_service_bindings_for_process_info(process) {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "service_binding_cleanup_failed",
                &err.to_string(),
            );
        }
    }
    let mut processes = match pm.list_processes() {
        Ok(processes) => processes,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "process_list_failed",
                &err.to_string(),
            )
        }
    };
    processes.sort_by_key(|p| std::cmp::Reverse(p.start_time));

    let rows = processes
        .into_iter()
        .map(|process| ProcessRowResponse {
            id: process.id,
            name: process.name,
            pid: process.pid,
            status: process_status_label(process.status).to_string(),
            active: process.status.is_active(),
            runtime: process.runtime,
            started_at: chrono::DateTime::<Utc>::from(process.start_time).to_rfc3339(),
            scoped_id: process.scoped_id,
            target_label: process.target_label,
            requested_port: process.requested_port,
        })
        .collect::<Vec<_>>();

    (StatusCode::OK, Json(rows)).into_response()
}

pub(super) async fn handle_list_persistent_states(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PersistentStateListQuery>,
) -> impl IntoResponse {
    if let Err(err) = validate_read_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }

    let owner_scope = query
        .owner_scope
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let state_name = query
        .state_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let _guard = state.lock.lock().await;
    let service = LocalRegistryService::new(&state);
    match service.list_persistent_states(owner_scope, state_name) {
        Ok(records) => (StatusCode::OK, Json(records)).into_response(),
        Err(err) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "persistent_state_list_failed",
            &err.to_string(),
        ),
    }
}

pub(super) async fn handle_get_persistent_state(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(state_id): AxumPath<String>,
) -> impl IntoResponse {
    if let Err(err) = validate_read_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }

    let state_id = state_id.trim();
    if state_id.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_state_id",
            "state_id is required",
        );
    }

    let _guard = state.lock.lock().await;
    let service = LocalRegistryService::new(&state);
    match service.get_persistent_state(state_id) {
        Ok(Some(record)) => (StatusCode::OK, Json(record)).into_response(),
        Ok(None) => json_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "Persistent state not found",
        ),
        Err(err) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "persistent_state_lookup_failed",
            &err.to_string(),
        ),
    }
}

pub(super) async fn handle_list_service_bindings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ServiceBindingListQuery>,
) -> impl IntoResponse {
    if let Err(err) = validate_read_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }

    let owner_scope = query
        .owner_scope
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let service_name = query
        .service_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let _guard = state.lock.lock().await;
    let service = LocalRegistryService::new(&state);
    match service.list_service_bindings(owner_scope, service_name) {
        Ok(records) => (StatusCode::OK, Json(records)).into_response(),
        Err(err) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "service_binding_list_failed",
            &err.to_string(),
        ),
    }
}

pub(super) async fn handle_get_service_binding(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(binding_id): AxumPath<String>,
) -> impl IntoResponse {
    if let Err(err) = validate_read_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }

    let binding_id = binding_id.trim();
    if binding_id.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_binding_id",
            "binding_id is required",
        );
    }

    let _guard = state.lock.lock().await;
    let service = LocalRegistryService::new(&state);
    match service.get_service_binding(binding_id) {
        Ok(Some(record)) => (StatusCode::OK, Json(record)).into_response(),
        Ok(None) => json_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "Host-side service binding not found",
        ),
        Err(err) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "service_binding_lookup_failed",
            &err.to_string(),
        ),
    }
}

pub(super) async fn handle_resolve_service_binding(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ServiceBindingResolveQuery>,
) -> impl IntoResponse {
    if let Err(err) = validate_read_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }

    let owner_scope = query
        .owner_scope
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let service_name = query
        .service_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let binding_kind = query
        .binding_kind
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(binding::SERVICE_BINDING_KIND_INGRESS);
    let caller_service = query
        .caller_service
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let Some(owner_scope) = owner_scope else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_owner_scope",
            "owner_scope is required",
        );
    };
    let Some(service_name) = service_name else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_service_name",
            "service_name is required",
        );
    };

    let _guard = state.lock.lock().await;
    let service = LocalRegistryService::new(&state);
    match service.resolve_service_binding(owner_scope, service_name, binding_kind, caller_service) {
        Ok(record) => (StatusCode::OK, Json(record)).into_response(),
        Err(err) => {
            let message = err.to_string();
            if message.contains("was not found") {
                return json_error(StatusCode::NOT_FOUND, "not_found", &message);
            }
            if message.contains("not allowed") {
                return json_error(StatusCode::FORBIDDEN, "forbidden", &message);
            }
            json_error(
                StatusCode::BAD_REQUEST,
                "service_binding_resolve_failed",
                &message,
            )
        }
    }
}

pub(super) async fn handle_register_persistent_state(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RegisterPersistentStateRequest>,
) -> impl IntoResponse {
    if let Err(err) = validate_write_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }

    let manifest = request.manifest.trim();
    let state_name = request.state_name.trim();
    let path = request.path.trim();
    if manifest.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_manifest",
            "manifest is required",
        );
    }
    if state_name.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_state_name",
            "state_name is required",
        );
    }
    if path.is_empty() {
        return json_error(StatusCode::BAD_REQUEST, "invalid_path", "path is required");
    }

    let _guard = state.lock.lock().await;
    let service = LocalRegistryService::new(&state);
    let result = service.register_persistent_state(Path::new(manifest), state_name, path);
    match result {
        Ok(record) => (StatusCode::CREATED, Json(record)).into_response(),
        Err(err) => json_error(
            StatusCode::BAD_REQUEST,
            "persistent_state_register_failed",
            &err.to_string(),
        ),
    }
}

pub(super) async fn handle_register_service_binding(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RegisterServiceBindingRequest>,
) -> impl IntoResponse {
    if let Err(err) = validate_write_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }

    let manifest = request.manifest.trim();
    let service_name = request.service_name.trim();
    let url = request
        .url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let process_id = request
        .process_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let binding_kind = request
        .binding_kind
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(binding::SERVICE_BINDING_KIND_INGRESS);
    if manifest.is_empty() && process_id.is_none() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_manifest",
            "manifest is required unless process_id is provided",
        );
    }
    if service_name.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_service_name",
            "service_name is required",
        );
    }
    if request.port.is_some_and(|port| port == 0) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_port",
            "port must be between 1 and 65535",
        );
    }

    let _guard = state.lock.lock().await;
    let result = match binding_kind {
        binding::SERVICE_BINDING_KIND_INGRESS => {
            let Some(url) = url else {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_url",
                    "url is required for ingress bindings",
                );
            };
            binding::register_ingress_binding(Path::new(manifest), service_name, url)
        }
        binding::SERVICE_BINDING_KIND_SERVICE => match (url, process_id) {
            (Some(url), _) => {
                binding::register_service_binding(Path::new(manifest), service_name, url)
            }
            (None, Some(process_id)) => binding::register_service_binding_for_process(
                process_id,
                service_name,
                request.port,
            ),
            (None, None) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_service_binding_source",
                    "service bindings require either url or process_id",
                );
            }
        },
        other => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "invalid_binding_kind",
                &format!(
                    "binding_kind must be '{}' or '{}' (got '{}')",
                    binding::SERVICE_BINDING_KIND_INGRESS,
                    binding::SERVICE_BINDING_KIND_SERVICE,
                    other
                ),
            );
        }
    };
    match result {
        Ok(record) => (StatusCode::CREATED, Json(record)).into_response(),
        Err(err) => json_error(
            StatusCode::BAD_REQUEST,
            "service_binding_register_failed",
            &err.to_string(),
        ),
    }
}

pub(super) async fn handle_local_url_ready(
    Query(query): Query<UrlReadyQuery>,
) -> impl IntoResponse {
    let Some(raw_url) = query
        .url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_url",
            "url query is required",
        );
    };

    let url = match reqwest::Url::parse(raw_url) {
        Ok(url) => url,
        Err(err) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "invalid_url",
                &format!("failed to parse url: {}", err),
            )
        }
    };

    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_url",
            "url must be an absolute http(s) URL",
        );
    }

    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(1200))
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "url_probe_failed",
                &format!("failed to create probe client: {}", err),
            )
        }
    };

    match client.get(url).send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            (
                StatusCode::OK,
                Json(UrlReadyResponse {
                    ready: status == 200,
                    status: Some(status),
                    error: None,
                }),
            )
                .into_response()
        }
        Err(err) => (
            StatusCode::OK,
            Json(UrlReadyResponse {
                ready: false,
                status: None,
                error: Some(err.to_string()),
            }),
        )
            .into_response(),
    }
}

pub(super) async fn handle_stop_local_process(
    AxumPath(id): AxumPath<String>,
    Json(request): Json<StopProcessRequest>,
) -> impl IntoResponse {
    if !request.confirmed {
        return json_error(
            StatusCode::BAD_REQUEST,
            "confirmation_required",
            "confirmed=true is required",
        );
    }
    let pm = match ProcessManager::new() {
        Ok(pm) => pm,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "process_manager_error",
                &err.to_string(),
            )
        }
    };
    let process = pm.read_pid(id.trim()).ok();
    let stopped = match pm.stop_process(id.trim(), request.force.unwrap_or(false)) {
        Ok(stopped) => stopped,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "process_stop_failed",
                &err.to_string(),
            )
        }
    };

    let removed_service_binding_ids = match process {
        Some(process) => match binding::cleanup_service_bindings_for_process_info(&process) {
            Ok(records) => records
                .into_iter()
                .map(|record| record.binding_id)
                .collect(),
            Err(err) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "service_binding_cleanup_failed",
                    &err.to_string(),
                )
            }
        },
        None => Vec::new(),
    };

    (
        StatusCode::OK,
        Json(StopProcessResponse {
            stopped,
            removed_service_binding_ids,
        }),
    )
        .into_response()
}

pub(super) async fn handle_get_process_logs(
    AxumPath(id): AxumPath<String>,
    Query(query): Query<ProcessLogsQuery>,
) -> impl IntoResponse {
    let log_path = process_log_path(id.trim());
    let tail = query.tail.unwrap_or(500).clamp(1, 5000);
    let lines = read_process_log_lines(&log_path, tail);
    let updated_at = std::fs::metadata(&log_path)
        .and_then(|meta| meta.modified())
        .map(|time| chrono::DateTime::<Utc>::from(time).to_rfc3339())
        .unwrap_or_else(|_| Utc::now().to_rfc3339());
    (
        StatusCode::OK,
        Json(ProcessLogsResponse { lines, updated_at }),
    )
        .into_response()
}

pub(super) async fn handle_clear_process_logs(AxumPath(id): AxumPath<String>) -> impl IntoResponse {
    let path = process_log_path(id.trim());
    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "log_clear_failed",
                &format!("failed to prepare log directory: {}", err),
            );
        }
    }
    if let Err(err) = std::fs::write(&path, "") {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "log_clear_failed",
            &format!("failed to clear log file: {}", err),
        );
    }
    (StatusCode::OK, Json(ClearLogsResponse { cleared: true })).into_response()
}

fn process_status_label(status: ProcessStatus) -> &'static str {
    match status {
        ProcessStatus::Starting => "starting",
        ProcessStatus::Ready => "ready",
        ProcessStatus::Running => "running",
        ProcessStatus::Exited => "exited",
        ProcessStatus::Failed => "failed",
        ProcessStatus::Stopped => "stopped",
        ProcessStatus::Unknown => "unknown",
    }
}
