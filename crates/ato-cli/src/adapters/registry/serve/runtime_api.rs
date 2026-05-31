use super::*;

use capsule_core::common::paths::ato_path_or_workspace_tmp;
use capsule_core::foundation::install_lifecycle::{
    derive_install_profile_key, InstallInstanceStore,
};
use capsule_wire::placement::{
    PlacedSessionSummary, PlacementCapabilities, PlacementFacets, PlacementIdentity,
    PlacementProviderId, PlacementProviderKind,
};

const LOCAL_DESKTOP_PROVIDER_ID: &str = "desktop:local";
const LOCAL_DESKTOP_PLACEMENT_ID: &str = "plc_local_desktop";

#[derive(Debug, Serialize)]
struct RuntimeProviderResponse {
    id: PlacementProviderId,
    kind: PlacementProviderKind,
    display_name: String,
    capabilities: PlacementCapabilities,
}

#[derive(Debug, Serialize)]
struct RuntimeInstallProfileResponse {
    installed_app_id: String,
    publisher: String,
    slug: String,
    capsule_handle: String,
    profile_id: String,
    install_profile_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_revision_id: Option<String>,
    port_policy: String,
    concurrency_policy: String,
    isolation: String,
}

pub(super) async fn handle_runtime_providers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = validate_read_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }

    let providers = vec![RuntimeProviderResponse {
        id: PlacementProviderId::new(LOCAL_DESKTOP_PROVIDER_ID),
        kind: PlacementProviderKind::Desktop,
        display_name: "Local Desktop Runtime".to_string(),
        capabilities: PlacementCapabilities {
            supports_launch: false,
            supports_stop: false,
            supports_logs: true,
            supports_open_url: true,
            supports_start_serve: false,
            isolation_classes: vec!["local".to_string()],
            storage_classes: vec!["local".to_string()],
            network_classes: vec!["loopback".to_string()],
            runner_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        },
    }];

    (StatusCode::OK, Json(providers)).into_response()
}

pub(super) async fn handle_runtime_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = validate_read_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
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
    processes.sort_by_key(|process| std::cmp::Reverse(process.start_time));

    let rows = processes
        .into_iter()
        .map(runtime_session_summary)
        .collect::<Vec<_>>();
    (StatusCode::OK, Json(rows)).into_response()
}

pub(super) async fn handle_runtime_install_profiles(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = validate_read_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }

    let instances_root = ato_path_or_workspace_tmp("instances");
    let store = match InstallInstanceStore::new(&instances_root) {
        Ok(store) => store,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "install_profile_store_error",
                &err.to_string(),
            )
        }
    };

    let apps = match store.list_installed_apps() {
        Ok(apps) => apps,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "install_profile_list_failed",
                &err.to_string(),
            )
        }
    };

    let mut rows = Vec::new();
    for app_id in apps {
        let app_record = match store.read_app_record(&app_id) {
            Ok(record) => record,
            Err(_) => continue,
        };
        let profiles = match store.list_profiles(&app_id) {
            Ok(profiles) => profiles,
            Err(_) => continue,
        };
        for profile_id in profiles {
            let profile = match store.read_profile(&app_id, &profile_id) {
                Ok(profile) => profile,
                Err(_) => continue,
            };
            rows.push(RuntimeInstallProfileResponse {
                installed_app_id: app_id.as_str().to_string(),
                publisher: app_record.publisher.clone(),
                slug: app_record.slug.clone(),
                capsule_handle: if app_record.capsule_handle.is_empty() {
                    format!("{}/{}", app_record.publisher, app_record.slug)
                } else {
                    app_record.capsule_handle.clone()
                },
                profile_id: profile_id.as_str().to_string(),
                install_profile_key: derive_install_profile_key(&app_id, &profile_id)
                    .as_str()
                    .to_string(),
                current_revision_id: store
                    .current_revision(&app_id, &profile_id)
                    .ok()
                    .map(|rev| rev.as_str().to_string()),
                port_policy: profile.port_policy,
                concurrency_policy: profile.concurrency_policy,
                isolation: profile.isolation,
            });
        }
    }
    rows.sort_by(|a, b| {
        a.publisher
            .cmp(&b.publisher)
            .then_with(|| a.slug.cmp(&b.slug))
            .then_with(|| a.profile_id.cmp(&b.profile_id))
    });

    (StatusCode::OK, Json(rows)).into_response()
}

pub(super) async fn handle_runtime_session_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<ProcessLogsQuery>,
) -> impl IntoResponse {
    if let Err(err) = validate_read_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }

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

pub(super) fn runtime_session_summary(process: ProcessInfo) -> PlacedSessionSummary {
    PlacedSessionSummary {
        session_id: process.id,
        status: process_status_label(process.status).to_string(),
        placement: local_desktop_placement_identity(),
        execution_id: None,
        user_visible_url: process
            .requested_port
            .map(|port| format!("http://127.0.0.1:{port}")),
        requested_by_client: Some("web_console".to_string()),
        runtime_owner: Some("desktop_be".to_string()),
        install_profile_key: None,
        launch_profile_id: process.target_label,
    }
}

fn local_desktop_placement_identity() -> PlacementIdentity {
    PlacementIdentity {
        placement_provider: PlacementProviderKind::Desktop,
        placement_provider_id: PlacementProviderId::new(LOCAL_DESKTOP_PROVIDER_ID),
        placement_id: LOCAL_DESKTOP_PLACEMENT_ID.to_string(),
        placement_fingerprint: None,
        placement_facets: Some(PlacementFacets {
            provider_kind: PlacementProviderKind::Desktop,
            isolation_class: "local".to_string(),
            storage_class: "local".to_string(),
            network_class: "loopback".to_string(),
            runner_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
    }
}
