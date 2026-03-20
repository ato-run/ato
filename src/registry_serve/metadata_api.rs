use super::*;

pub(super) async fn handle_get_store_metadata(
    State(state): State<AppState>,
    AxumPath((publisher, slug)): AxumPath<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = validate_capsule_segments(&publisher, &slug) {
        return json_error(StatusCode::BAD_REQUEST, "invalid_scope", &err.to_string());
    }

    let _guard = state.lock.lock().await;
    let index = match load_index(&state.data_dir) {
        Ok(index) => index,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "index_read_failed",
                &err.to_string(),
            );
        }
    };
    let exists = index
        .capsules
        .iter()
        .any(|capsule| capsule.publisher == publisher && capsule.slug == slug);
    if !exists {
        return json_error(StatusCode::NOT_FOUND, "not_found", "Capsule not found");
    }

    let store_metadata = load_store_metadata(&state.data_dir).unwrap_or_default();
    let metadata = get_store_metadata_entry(&store_metadata, &publisher, &slug);
    let public_base_url = resolve_public_base_url(&headers, &state.listen_url);
    let payload = metadata_to_payload(metadata, &public_base_url, &publisher, &slug).unwrap_or(
        StoreMetadataPayload {
            icon_path: None,
            text: None,
            icon_url: None,
        },
    );
    (StatusCode::OK, Json(payload)).into_response()
}

pub(super) async fn handle_put_store_metadata(
    State(state): State<AppState>,
    AxumPath((publisher, slug)): AxumPath<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<UpsertStoreMetadataRequest>,
) -> impl IntoResponse {
    if !request.confirmed {
        return json_error(
            StatusCode::BAD_REQUEST,
            "confirmation_required",
            "confirmed=true is required",
        );
    }
    if let Err(err) = validate_capsule_segments(&publisher, &slug) {
        return json_error(StatusCode::BAD_REQUEST, "invalid_scope", &err.to_string());
    }

    let icon_path = normalize_optional_string(request.icon_path);
    let text = normalize_optional_string(request.text);
    let scoped_id = format!("{}/{}", publisher, slug);
    let now = Utc::now().to_rfc3339();

    let _guard = state.lock.lock().await;
    let index = match load_index(&state.data_dir) {
        Ok(index) => index,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "index_read_failed",
                &err.to_string(),
            );
        }
    };
    let exists = index
        .capsules
        .iter()
        .any(|capsule| capsule.publisher == publisher && capsule.slug == slug);
    if !exists {
        return json_error(StatusCode::NOT_FOUND, "not_found", "Capsule not found");
    }

    let store = match RegistryStore::open(&state.data_dir) {
        Ok(store) => store,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "registry_store_error",
                &err.to_string(),
            );
        }
    };
    if let Err(err) =
        store.upsert_store_metadata(&scoped_id, icon_path.as_deref(), text.as_deref(), &now)
    {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store_metadata_upsert_failed",
            &err.to_string(),
        );
    }
    let store_metadata = load_store_metadata(&state.data_dir).unwrap_or_default();
    let public_base_url = resolve_public_base_url(&headers, &state.listen_url);
    let payload = metadata_to_payload(
        get_store_metadata_entry(&store_metadata, &publisher, &slug),
        &public_base_url,
        &publisher,
        &slug,
    )
    .unwrap_or(StoreMetadataPayload {
        icon_path: None,
        text: None,
        icon_url: None,
    });

    (
        StatusCode::OK,
        Json(json!({
            "updated": true,
            "scoped_id": scoped_id,
            "store_metadata": payload,
        })),
    )
        .into_response()
}

pub(super) async fn handle_get_store_icon(
    State(state): State<AppState>,
    AxumPath((publisher, slug)): AxumPath<(String, String)>,
) -> impl IntoResponse {
    if let Err(err) = validate_capsule_segments(&publisher, &slug) {
        return json_error(StatusCode::BAD_REQUEST, "invalid_scope", &err.to_string());
    }

    let _guard = state.lock.lock().await;
    let store = match RegistryStore::open(&state.data_dir) {
        Ok(store) => store,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "registry_store_error",
                &err.to_string(),
            );
        }
    };
    let scoped_id = format!("{}/{}", publisher, slug);
    let metadata = match store.load_store_metadata_entry(&scoped_id) {
        Ok(metadata) => metadata,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_metadata_read_failed",
                &err.to_string(),
            );
        }
    };
    let Some(metadata) = metadata else {
        return json_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "Store metadata not found",
        );
    };
    let Some(raw_icon_path) = metadata.icon_path.as_deref() else {
        return json_error(StatusCode::NOT_FOUND, "not_found", "Icon path is not set");
    };
    let icon_path = expand_user_path(raw_icon_path);
    let bytes = match std::fs::read(&icon_path) {
        Ok(bytes) => bytes,
        Err(err) => {
            return json_error(
                StatusCode::NOT_FOUND,
                "not_found",
                &format!("Icon file is not readable: {}", err),
            );
        }
    };
    let content_type = mime_guess::from_path(&icon_path)
        .first_or_octet_stream()
        .essence_str()
        .to_string();
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_str(&content_type)
                    .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        bytes,
    )
        .into_response()
}

pub(super) async fn handle_get_runtime_config(
    State(state): State<AppState>,
    AxumPath((publisher, slug)): AxumPath<(String, String)>,
) -> impl IntoResponse {
    if let Err(err) = validate_capsule_segments(&publisher, &slug) {
        return json_error(StatusCode::BAD_REQUEST, "invalid_scope", &err.to_string());
    }

    let _guard = state.lock.lock().await;
    let index = match load_index(&state.data_dir) {
        Ok(index) => index,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "index_read_failed",
                &err.to_string(),
            );
        }
    };
    let exists = index
        .capsules
        .iter()
        .any(|capsule| capsule.publisher == publisher && capsule.slug == slug);
    if !exists {
        return json_error(StatusCode::NOT_FOUND, "not_found", "Capsule not found");
    }

    let runtime_config = load_runtime_config(&state.data_dir)
        .ok()
        .and_then(|cfg| get_runtime_config_entry(&cfg, &publisher, &slug).cloned())
        .unwrap_or_default();
    (StatusCode::OK, Json(runtime_config)).into_response()
}

pub(super) async fn handle_put_runtime_config(
    State(state): State<AppState>,
    AxumPath((publisher, slug)): AxumPath<(String, String)>,
    Json(request): Json<UpsertRuntimeConfigRequest>,
) -> impl IntoResponse {
    if let Err(err) = validate_capsule_segments(&publisher, &slug) {
        return json_error(StatusCode::BAD_REQUEST, "invalid_scope", &err.to_string());
    }

    let _guard = state.lock.lock().await;
    let index = match load_index(&state.data_dir) {
        Ok(index) => index,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "index_read_failed",
                &err.to_string(),
            );
        }
    };
    let exists = index
        .capsules
        .iter()
        .any(|capsule| capsule.publisher == publisher && capsule.slug == slug);
    if !exists {
        return json_error(StatusCode::NOT_FOUND, "not_found", "Capsule not found");
    }

    let mut runtime_index = load_runtime_config(&state.data_dir).unwrap_or_default();
    let entry_key = runtime_config_key(&publisher, &slug);
    let selected_target = normalize_optional_string(request.selected_target);
    let mut targets = HashMap::new();
    if let Some(request_targets) = request.targets {
        for (raw_label, raw_config) in request_targets {
            let label = raw_label.trim();
            if label.is_empty() {
                continue;
            }
            let env = raw_config
                .env
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(key, value)| {
                    let normalized = key.trim();
                    if normalized.is_empty() {
                        None
                    } else {
                        Some((normalized.to_string(), value))
                    }
                })
                .collect::<HashMap<_, _>>();
            if env.is_empty() && raw_config.port.is_none() && raw_config.permission_mode.is_none() {
                continue;
            }
            targets.insert(
                label.to_string(),
                RuntimeTargetConfig {
                    port: raw_config.port,
                    env,
                    permission_mode: raw_config.permission_mode,
                },
            );
        }
    }
    let next_config = CapsuleRuntimeConfig {
        selected_target,
        targets,
    };
    if next_config.selected_target.is_none() && next_config.targets.is_empty() {
        runtime_index.entries.remove(&entry_key);
    } else {
        runtime_index.entries.insert(entry_key, next_config.clone());
    }
    if let Err(err) = write_runtime_config(&state.data_dir, &runtime_index) {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "runtime_config_write_failed",
            &err.to_string(),
        );
    }
    (StatusCode::OK, Json(next_config)).into_response()
}
