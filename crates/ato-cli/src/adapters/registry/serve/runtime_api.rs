use std::collections::BTreeMap;

use super::*;

use ato_session_core::{StoredSessionInfo, read_session_records, session_root};
use capsule_core::common::paths::ato_path_or_workspace_tmp;
use capsule_core::foundation::install_lifecycle::{
    InstallInstanceStore, derive_install_profile_key,
};
use capsule_wire::placement::{
    PlacedSessionSummary, PlacementCapabilities, PlacementFacets, PlacementIdentity,
    PlacementProviderId, PlacementProviderKind,
};

#[derive(Debug, Deserialize)]
pub(super) struct LaunchSessionRequest {
    pub install_profile_key: String,
    /// Optional target label to pass as `--target` to `ato app session start`.
    /// Corresponds to the target label in the capsule manifest (e.g. `"web"`,
    /// `"worker"`). Named `target_label` to avoid confusion with install
    /// profile IDs or launch profile IDs.
    #[serde(default)]
    pub target_label: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct LaunchSessionResponse {
    pub(super) session_id: String,
    pub(super) status: String,
    pub(super) install_profile_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) launch_profile_id: Option<String>,
    pub(super) placement: PlacementIdentity,
    pub(super) requested_by_client: String,
    pub(super) runtime_owner: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) user_visible_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) local_runtime_url: Option<String>,
}

#[derive(Debug, Serialize)]
struct StopSessionResponse {
    session_id: String,
    status: String,
}

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
pub(super) struct RuntimeInstallProfileResponse {
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

#[derive(Debug, Serialize)]
pub(super) struct RuntimeSessionResponse {
    #[serde(flatten)]
    pub(super) session: PlacedSessionSummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) local_runtime_url: Option<String>,
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
            supports_launch: true,
            supports_stop: true,
            supports_logs: true,
            supports_open_url: true,
            supports_start_serve: false,
            supports_add_capsule: true,
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
            );
        }
    };
    let mut processes = match pm.list_processes() {
        Ok(processes) => processes,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "process_list_failed",
                &err.to_string(),
            );
        }
    };
    processes.sort_by_key(|process| std::cmp::Reverse(process.start_time));

    let stored_by_id = stored_sessions_by_id();
    let rows = processes
        .into_iter()
        .map(|process| {
            let stored = stored_by_id.get(&process.id);
            runtime_session_summary(process, stored)
        })
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

    let instances_root = install_profile_store_root();
    let store = match InstallInstanceStore::new(&instances_root) {
        Ok(store) => store,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "install_profile_store_error",
                &err.to_string(),
            );
        }
    };

    let apps = match store.list_installed_apps() {
        Ok(apps) => apps,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "install_profile_list_failed",
                &err.to_string(),
            );
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

    // Check whether the caller wants SSE streaming.
    let wants_sse = headers
        .get(axum::http::header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("text/event-stream"))
        .unwrap_or(false);

    let session_id = id.trim().to_string();

    if wants_sse {
        return stream_session_logs_sse(session_id, state)
            .await
            .into_response();
    }

    // Default: batch JSON response (original behaviour).
    let log_path = process_log_path(session_id.as_str());
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

/// Stream session log lines as `text/event-stream` (SSE).
///
/// - Existing lines in the log file are sent immediately as `data:` events.
/// - New lines are polled every 100 ms and sent as they appear.
/// - A heartbeat comment (`:\n\n`) is sent every 15 seconds to keep the
///   connection alive through proxies.
/// - The stream terminates when the session process is no longer running.
async fn stream_session_logs_sse(session_id: String, _state: AppState) -> axum::response::Response {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use futures::SinkExt;
    use futures::channel::mpsc;

    let log_path = process_log_path(session_id.as_str());

    // Bounded channel — capacity covers burst of existing lines plus
    // in-flight tails.  A slow consumer causes back-pressure on the
    // poll task which is acceptable for log streaming.
    let (mut tx, rx) = mpsc::channel::<Result<Event, std::convert::Infallible>>(512);

    // Send existing lines then poll for new ones in a background task.
    let log_path_clone = log_path.clone();
    tokio::spawn(async move {
        // 1. Flush existing lines.
        let existing = read_process_log_lines(&log_path_clone, 5000);
        for line in existing {
            if tx.send(Ok(Event::default().data(line))).await.is_err() {
                return;
            }
        }

        // 2. Tail the file for new content.
        let mut byte_offset: u64 = std::fs::metadata(&log_path_clone)
            .map(|m| m.len())
            .unwrap_or(0);
        let mut last_heartbeat = std::time::Instant::now();

        loop {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            // Check process liveness.
            let still_running = ProcessManager::new()
                .ok()
                .and_then(|pm| pm.list_processes().ok())
                .map(|procs| {
                    procs
                        .iter()
                        .any(|p| p.id == session_id && p.status.is_active())
                })
                .unwrap_or(false);

            // Append new log lines since last poll.
            if let Ok(mut file) = std::fs::File::open(&log_path_clone) {
                use std::io::{Read, Seek, SeekFrom};
                if file.seek(SeekFrom::Start(byte_offset)).is_ok() {
                    let mut buf = String::new();
                    if file.read_to_string(&mut buf).is_ok() {
                        byte_offset += buf.len() as u64;
                        for line in buf.lines() {
                            if tx
                                .send(Ok(Event::default().data(line.to_string())))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                }
            }

            // Send a heartbeat comment every 15 seconds.
            if last_heartbeat.elapsed() >= std::time::Duration::from_secs(15) {
                if tx
                    .send(Ok(Event::default().comment("heartbeat")))
                    .await
                    .is_err()
                {
                    return;
                }
                last_heartbeat = std::time::Instant::now();
            }

            if !still_running {
                break;
            }
        }
    });

    Sse::new(rx)
        .keep_alive(KeepAlive::default())
        .into_response()
}

pub(super) fn runtime_session_summary(
    process: ProcessInfo,
    stored: Option<&StoredSessionInfo>,
) -> RuntimeSessionResponse {
    let local_runtime_url = process
        .requested_port
        .map(|port| format!("http://127.0.0.1:{port}"));
    RuntimeSessionResponse {
        session: PlacedSessionSummary {
            session_id: process.id,
            status: process_status_label(process.status).to_string(),
            placement: placement_identity_for(stored),
            execution_id: stored.and_then(|record| record.execution_id.clone()),
            user_visible_url: stored.and_then(|record| record.user_visible_url.clone()),
            requested_by_client: stored
                .and_then(|record| record.requested_by_client.clone())
                .or_else(|| Some("unknown".to_string())),
            runtime_owner: stored
                .and_then(|record| record.runtime_owner.clone())
                .or_else(|| Some("local_runtime".to_string())),
            install_profile_key: stored.and_then(|record| record.install_profile_key.clone()),
            launch_profile_id: stored
                .and_then(|record| record.install_profile_id.clone())
                .or(process.target_label),
        },
        local_runtime_url,
    }
}

pub(super) fn install_profile_store_root() -> PathBuf {
    // `ato launch` and the install lifecycle store use the canonical ATO_HOME
    // root. The Runtime Control read API intentionally mirrors that source of
    // truth instead of the local registry's package data_dir.
    ato_path_or_workspace_tmp("instances")
}

fn stored_sessions_by_id() -> BTreeMap<String, StoredSessionInfo> {
    let Ok(root) = session_root() else {
        return BTreeMap::new();
    };
    let Ok(records) = read_session_records(&root) else {
        return BTreeMap::new();
    };
    records
        .into_iter()
        .map(|record| (record.session_id.clone(), record))
        .collect()
}

fn placement_identity_for(stored: Option<&StoredSessionInfo>) -> PlacementIdentity {
    let Some(record) = stored else {
        return local_desktop_placement_identity();
    };
    let placement_provider = record
        .placement_provider
        .as_deref()
        .and_then(placement_provider_kind_from_str)
        .unwrap_or(PlacementProviderKind::Desktop);
    PlacementIdentity {
        placement_provider,
        placement_provider_id: PlacementProviderId::new(
            record
                .placement_provider_id
                .clone()
                .unwrap_or_else(|| LOCAL_DESKTOP_PROVIDER_ID.to_string()),
        ),
        placement_id: record
            .placement_id
            .clone()
            .unwrap_or_else(|| LOCAL_DESKTOP_PLACEMENT_ID.to_string()),
        placement_fingerprint: record.placement_fingerprint.clone(),
        placement_facets: record.placement_facets.clone(),
    }
}

fn placement_provider_kind_from_str(value: &str) -> Option<PlacementProviderKind> {
    match value {
        "desktop" => Some(PlacementProviderKind::Desktop),
        "managed" => Some(PlacementProviderKind::Managed),
        "external" => Some(PlacementProviderKind::External),
        _ => None,
    }
}

/// `POST /v1/runtime/sessions` — launch a new app session.
///
/// Spawns `ato app session start --app <install_profile_key>`, captures
/// the JSON output to extract the `session_id`, then attempts to register
/// an ephemeral ingress route via ato-netd if the session bound a port.
pub(super) async fn handle_runtime_launch_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<LaunchSessionRequest>,
) -> impl IntoResponse {
    if let Err(err) = validate_write_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }

    let key = request.install_profile_key.trim().to_string();
    if key.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "install_profile_key is required",
        );
    }

    // Verify the install profile exists before spawning.
    let instances_root = install_profile_store_root();
    let store = match InstallInstanceStore::new(&instances_root) {
        Ok(store) => store,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "install_profile_store_error",
                &err.to_string(),
            );
        }
    };

    // Derive the app_id and profile_id from the install_profile_key.
    // The key format is "app_id::profile_id" as produced by
    // `derive_install_profile_key`. We verify the profile is installed
    // before spawning the process.
    let apps = match store.list_installed_apps() {
        Ok(apps) => apps,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "install_profile_list_failed",
                &err.to_string(),
            );
        }
    };
    // Resolve the capsule handle and profile_id from the matching app record.
    let mut resolved: Option<(String, String)> = None; // (capsule_handle, profile_id)
    'outer: for app_id in &apps {
        let app_record = match store.read_app_record(app_id) {
            Ok(record) => record,
            Err(_) => continue,
        };
        let profiles = match store.list_profiles(app_id) {
            Ok(profiles) => profiles,
            Err(_) => continue,
        };
        for profile_id in &profiles {
            if derive_install_profile_key(app_id, profile_id)
                .as_str()
                .to_string()
                == key
            {
                let handle = if app_record.capsule_handle.is_empty() {
                    format!("{}/{}", app_record.publisher, app_record.slug)
                } else {
                    app_record.capsule_handle.clone()
                };
                resolved = Some((handle, profile_id.as_str().to_string()));
                break 'outer;
            }
        }
    }
    let Some((capsule_handle, profile_id)) = resolved else {
        return json_error(
            StatusCode::NOT_FOUND,
            "install_profile_not_found",
            &format!("install profile '{}' not found", key),
        );
    };

    // `ato app session start` does not accept a --profile flag, so it always
    // launches with the default profile. Reject non-default profiles explicitly
    // to prevent silently running the wrong configuration.
    if profile_id != "default" {
        return json_error(
            StatusCode::NOT_IMPLEMENTED,
            "non_default_profile_not_supported",
            &format!(
                "install profile '{}' uses profile_id '{}'; only the 'default' profile can be \
                 launched via the Runtime Control API at this time",
                key, profile_id
            ),
        );
    }

    // Resolve the `ato` executable path (same binary that is running).
    let ato_exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "exe_resolve_failed",
                &err.to_string(),
            );
        }
    };

    // Build the command: `ato app session start <handle> --json [--target <id>]`
    let mut cmd = tokio::process::Command::new(&ato_exe);
    cmd.args(["app", "session", "start", &capsule_handle, "--json"]);
    if let Some(ref target) = request.target_label {
        if !target.trim().is_empty() {
            cmd.args(["--target", target.trim()]);
        }
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let output = match cmd.output().await {
        Ok(output) => output,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "session_spawn_failed",
                &err.to_string(),
            );
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "session_start_failed",
            &format!(
                "session start exited with {}: {}",
                output.status,
                stderr.trim()
            ),
        );
    }

    // Parse session_id and web_local_url from JSON output.
    // `ato app session start --json` emits a SessionStartEnvelope as either:
    //   - a single pretty-printed JSON object (most common), or
    //   - one JSON object per line (JSONL, for streaming future compatibility).
    // Try the full output as one blob first; fall back to line-by-line JSONL.
    let stdout = String::from_utf8_lossy(&output.stdout);
    let (mut session_id, mut web_local_url) = (None::<String>, None::<String>);
    let candidates: Vec<serde_json::Value> =
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&stdout) {
            vec![v]
        } else {
            stdout
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect()
        };
    for v in &candidates {
        if session_id.is_none() {
            session_id = v
                .pointer("/session/session_id")
                .or_else(|| v.get("session_id"))
                .and_then(|s| s.as_str())
                .map(str::to_string);
        }
        if web_local_url.is_none() {
            web_local_url = v
                .pointer("/session/web/local_url")
                .and_then(|s| s.as_str())
                .map(str::to_string);
        }
        if session_id.is_some() && web_local_url.is_some() {
            break;
        }
    }

    let Some(session_id) = session_id else {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "session_id_missing",
            "session start did not emit a session_id in its JSON output",
        );
    };

    // Best-effort: register an ephemeral ingress route so the local port is
    // reachable through ato-netd. The resulting URL is a loopback address and
    // must NOT be used as user_visible_url (which is reserved for mobile-safe /
    // externally-reachable URLs). It is not exposed in the response for now;
    // StartServe integration is a future concern.
    if let Some(ref upstream) = web_local_url {
        try_register_ephemeral_ingress_with_url(&session_id, upstream).await;
    }

    (
        StatusCode::CREATED,
        Json(LaunchSessionResponse {
            status: "starting".to_string(),
            install_profile_key: key,
            launch_profile_id: None,
            placement: local_desktop_placement_identity(),
            requested_by_client: "web_console".to_string(),
            runtime_owner: "local_runtime".to_string(),
            session_id,
            // user_visible_url is reserved for mobile-safe / externally-reachable
            // URLs (StartServe). Loopback addresses from ato-netd must not appear
            // here, so this is None until StartServe integration lands.
            user_visible_url: None,
            local_runtime_url: web_local_url,
        }),
    )
        .into_response()
}

/// `POST /v1/runtime/sessions/:id/stop` — stop a running session, returning JSON.
///
/// Same semantics as `DELETE /v1/runtime/sessions/:id` but returns a JSON body
/// and a 200 OK instead of 204 No Content, for PWA compatibility.
pub(super) async fn handle_runtime_stop_session_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> impl IntoResponse {
    if let Err(err) = validate_write_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }

    let session_id = id.trim().to_string();
    if session_id.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "session id is required",
        );
    }

    let pm = match ProcessManager::new() {
        Ok(pm) => pm,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "process_manager_error",
                &err.to_string(),
            );
        }
    };

    // Verify the session exists before attempting to stop it.
    let processes = match pm.list_processes() {
        Ok(p) => p,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "process_list_failed",
                &err.to_string(),
            );
        }
    };
    if !processes.iter().any(|p| p.id == session_id) {
        return json_error(
            StatusCode::NOT_FOUND,
            "session_not_found",
            &format!("session '{}' not found", session_id),
        );
    }

    // Best-effort: deregister the ephemeral ingress route if ato-netd is up.
    if let Ok(mut client) = ato_net::control::Client::connect_default().await {
        let session_key = format!("ephemeral:{session_id}");
        let _ = client.deregister_ephemeral_ingress(&session_key).await;
    }

    match pm.stop_process(&session_id, false) {
        Ok(_) => (
            StatusCode::OK,
            Json(StopSessionResponse {
                session_id,
                status: "stopped".to_string(),
            }),
        )
            .into_response(),
        Err(err) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "session_stop_failed",
            &err.to_string(),
        ),
    }
}

/// `DELETE /v1/runtime/sessions/:id` — stop a running session.
///
/// Deregisters the ephemeral ingress route (best-effort, ignores
/// ato-netd-not-running errors) then kills the session process.
pub(super) async fn handle_runtime_stop_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(id): AxumPath<String>,
) -> impl IntoResponse {
    if let Err(err) = validate_write_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }

    let id = id.trim().to_string();
    if id.is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "session id is required",
        );
    }

    // Best-effort: deregister the ephemeral ingress route if ato-netd is up.
    if let Ok(mut client) = ato_net::control::Client::connect_default().await {
        let session_key = format!("ephemeral:{id}");
        // Ignore errors — not running or already deregistered are both fine.
        let _ = client.deregister_ephemeral_ingress(&session_key).await;
    }

    // Stop the process via ProcessManager.
    let pm = match ProcessManager::new() {
        Ok(pm) => pm,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "process_manager_error",
                &err.to_string(),
            );
        }
    };

    match pm.stop_process(&id, false) {
        Ok(_stopped) => (StatusCode::NO_CONTENT, axum::body::Body::empty()).into_response(),
        Err(err) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "session_stop_failed",
            &err.to_string(),
        ),
    }
}

/// Try to register an ephemeral ingress route for a freshly-started session
/// using the upstream URL extracted from the session start output.
///
/// Returns the `user_visible_url` if registration succeeded, or `None`
/// if ato-netd is not running.
async fn try_register_ephemeral_ingress_with_url(
    session_id: &str,
    upstream_url: &str,
) -> Option<String> {
    let session_key = format!("ephemeral:{session_id}");
    let mut client = ato_net::control::Client::connect_default().await.ok()?;
    let info = client
        .register_ephemeral_ingress(&session_key, upstream_url)
        .await
        .ok()?;
    Some(format!("http://127.0.0.1:{}", info.port))
}

// ── Source validation ──────────────────────────────────────────────────────

/// Schemes the Store→PWA bridge explicitly disallows regardless of format.
const UNSAFE_SCHEMES: &[&str] = &[
    "javascript:",
    "data:",
    "file:",
    "vbscript:",
    "blob:",
    "about:",
];

/// Validates a capsule source string from the PWA add-capsule request.
///
/// Accepted for MVP:
///   - `publisher/slug` (canonical for `ato install`)
///   - `publisher/slug@version`
///   - `https://ato.run/s/<id>` (share URL — validated format, resolved below)
///
/// Returns `Ok(normalized)` or an error string suitable for the 400 response.
pub(super) fn validate_add_capsule_source(raw: &str) -> Result<String, String> {
    let source = raw.trim();
    if source.is_empty() {
        return Err("source is required".to_string());
    }
    if source.len() > 2048 {
        return Err("source exceeds maximum length".to_string());
    }
    let lower = source.to_lowercase();
    for scheme in UNSAFE_SCHEMES {
        if lower.starts_with(scheme) {
            return Err(format!(
                "unsafe source scheme: '{}'",
                scheme.trim_end_matches(':')
            ));
        }
    }
    // Accepted: https://ato.run/s/<id>
    if source.starts_with("https://ato.run/s/") {
        let tail = source.trim_start_matches("https://ato.run/s/");
        if tail.is_empty() || tail.contains('/') {
            return Err("invalid ato.run share URL: expected https://ato.run/s/<id>".to_string());
        }
        return Ok(source.to_string());
    }
    // Reject @version suffix for MVP: the idempotency check in
    // read_default_profile_for_ref() strips @version when deriving the app_id,
    // so `publisher/slug@v2` would incorrectly return already_installed if
    // `publisher/slug` default profile already exists. Reject rather than silently
    // truncate until version-aware idempotency is implemented.
    if source.contains('@') {
        return Err(
            "source with @version is not supported; use publisher/slug or https://ato.run/s/<id>"
                .to_string(),
        );
    }
    // Accepted: publisher/slug
    let parts: Vec<&str> = source.splitn(2, '/').collect();
    if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        let valid = |s: &str| {
            s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        };
        if valid(parts[0]) && valid(parts[1]) {
            return Ok(source.to_string());
        }
    }
    Err(
        "source must be publisher/slug or https://ato.run/s/<id> (received: invalid format)"
            .to_string(),
    )
}

/// Resolve an `https://ato.run/s/<id>` share URL to a `publisher/slug` capsule ref.
///
/// SSRF guard: the request is only issued to `https://ato.run/`. The redirect
/// Location header is also validated — it must point back to `ato.run` so that
/// a compromised or spoofed short-link cannot cause the server to make requests
/// to private networks (127.x, 10.x, 192.168.x, 169.254.x, ::1, …).
async fn resolve_share_url_to_slug(share_url: &str) -> Result<String, String> {
    // Belt-and-suspenders: verify the URL is exactly ato.run before issuing the request.
    // validate_add_capsule_source() already checked this, but defense-in-depth is cheap.
    let parsed = reqwest::Url::parse(share_url).map_err(|e| format!("invalid share URL: {e}"))?;
    if parsed.scheme() != "https" || parsed.host_str() != Some("ato.run") {
        return Err("share URL must use https://ato.run/s/<id>".to_string());
    }

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;

    let resp = client
        .head(share_url)
        .send()
        .await
        .map_err(|e| format!("failed to resolve share URL: {e}"))?;

    if resp.status().is_redirection() {
        let location = resp
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| "share URL redirect missing Location header".to_string())?;

        // Validate redirect target: must be https://ato.run/<publisher>/<slug>.
        // Rejects redirects to localhost, private IPs, or arbitrary external hosts.
        let location_url = reqwest::Url::parse(location)
            .map_err(|_| format!("share URL redirected to unparseable location: {location}"))?;
        if location_url.scheme() != "https" || location_url.host_str() != Some("ato.run") {
            return Err(format!(
                "share URL redirected to disallowed host: {}",
                location_url.host_str().unwrap_or("unknown")
            ));
        }
        let path = location_url.path().trim_start_matches('/');
        let parts: Vec<&str> = path.splitn(2, '/').collect();
        if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
            return Ok(format!("{}/{}", parts[0], parts[1]));
        }
        return Err(format!(
            "share URL resolved to unexpected path: {}",
            location
        ));
    }

    Err(format!(
        "share URL did not redirect to a capsule (status {})",
        resp.status()
    ))
}

// ── Add-capsule handler ────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
pub(super) struct AddCapsuleRequest {
    pub(super) source: String,
    #[serde(default)]
    pub(super) profile_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct AddCapsuleResponse {
    pub(super) status: String, // "installed" | "already_installed"
    pub(super) profile: RuntimeInstallProfileResponse,
}

/// `POST /v1/runtime/install-profiles` — add/install a capsule from a source.
///
/// Spawns `ato install <publisher/slug> --json --yes --no-project`, waits for
/// completion, then reads the resulting install profile from the instance store.
/// Returns 201 Created (newly installed) or 200 OK (already installed).
pub(super) async fn handle_runtime_add_capsule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AddCapsuleRequest>,
) -> impl IntoResponse {
    if let Err(err) = validate_write_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }

    // Validate and normalize source.
    let source = match validate_add_capsule_source(&request.source) {
        Ok(s) => s,
        Err(msg) => return json_error(StatusCode::BAD_REQUEST, "invalid_source", &msg),
    };

    // Only default profile is supported for MVP.
    if let Some(ref pid) = request.profile_id {
        if pid != "default" {
            return json_error(
                StatusCode::NOT_IMPLEMENTED,
                "non_default_profile_not_supported",
                "only 'default' profile is supported for add-capsule at this time",
            );
        }
    }

    // Resolve https://ato.run/s/<id> to publisher/slug.
    let capsule_ref = if source.starts_with("https://ato.run/s/") {
        match resolve_share_url_to_slug(&source).await {
            Ok(slug) => slug,
            Err(msg) => return json_error(StatusCode::NOT_FOUND, "source_not_found", &msg),
        }
    } else {
        source.clone()
    };

    // Snapshot existing profiles to detect whether install is new or idempotent.
    let instances_root = install_profile_store_root();
    let profile_before = read_default_profile_for_ref(&instances_root, &capsule_ref);

    // If already installed, return immediately (idempotent).
    if let Some(profile) = profile_before {
        return (
            StatusCode::OK,
            Json(AddCapsuleResponse {
                status: "already_installed".to_string(),
                profile,
            }),
        )
            .into_response();
    }

    // Spawn `ato install <ref> --json --yes --no-project`.
    let ato_exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "exe_resolve_failed",
                &err.to_string(),
            );
        }
    };

    let mut cmd = tokio::process::Command::new(&ato_exe);
    cmd.args(["install", &capsule_ref, "--json", "--yes", "--no-project"]);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let output = match cmd.output().await {
        Ok(output) => output,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "install_spawn_failed",
                &err.to_string(),
            );
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        // Surface any structured error from the JSON output.
        let detail = if stdout.contains("price") {
            "paid capsules are not supported in MVP"
        } else if stderr.contains("not found") || stdout.contains("not_found") {
            return json_error(
                StatusCode::NOT_FOUND,
                "capsule_not_found",
                &format!("capsule '{}' not found in registry", capsule_ref),
            );
        } else {
            stderr.trim()
        };
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "install_failed",
            &format!("install exited with {}: {}", output.status, detail),
        );
    }

    // Read the installed profile from the instance store.
    let profile_after = read_default_profile_for_ref(&instances_root, &capsule_ref);

    match profile_after {
        Some(profile) => (
            StatusCode::CREATED,
            Json(AddCapsuleResponse {
                status: "installed".to_string(),
                profile,
            }),
        )
            .into_response(),
        None => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "install_profile_missing",
            "install completed but profile not found in instance store",
        ),
    }
}

/// Look up the `default` install profile for a `publisher/slug` capsule ref.
/// Returns `None` if not installed.
fn read_default_profile_for_ref(
    instances_root: &std::path::Path,
    capsule_ref: &str,
) -> Option<RuntimeInstallProfileResponse> {
    use capsule_core::foundation::install_lifecycle::ids::{
        InstalledAppId, ProfileId, path_safe_app_id,
    };

    let store = InstallInstanceStore::new(instances_root).ok()?;

    // Derive the app_id from the scoped id (publisher/slug part only).
    let scoped_id = capsule_ref.split('@').next().unwrap_or(capsule_ref);
    let app_id: InstalledAppId = path_safe_app_id(scoped_id);
    let profile_id = ProfileId::new("default");

    let app_record = store.read_app_record(&app_id).ok()?;
    let profile = store.read_profile(&app_id, &profile_id).ok()?;
    let ipk = derive_install_profile_key(&app_id, &profile_id);
    let current_revision_id = store
        .current_revision(&app_id, &profile_id)
        .ok()
        .map(|r| r.as_str().to_string());

    Some(RuntimeInstallProfileResponse {
        installed_app_id: app_id.as_str().to_string(),
        publisher: app_record.publisher.clone(),
        slug: app_record.slug.clone(),
        capsule_handle: if app_record.capsule_handle.is_empty() {
            format!("{}/{}", app_record.publisher, app_record.slug)
        } else {
            app_record.capsule_handle.clone()
        },
        profile_id: profile_id.as_str().to_string(),
        install_profile_key: ipk.as_str().to_string(),
        current_revision_id,
        port_policy: profile.port_policy,
        concurrency_policy: profile.concurrency_policy,
        isolation: profile.isolation,
    })
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
