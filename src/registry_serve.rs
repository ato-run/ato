use std::collections::HashMap;
use std::io::ErrorKind;
use std::io::{BufRead, BufReader, Cursor, Read};
use std::net::{SocketAddr, TcpListener};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path as AxumPath, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use capsule_core::capsule_v3::manifest::validate_blake3_digest;
use capsule_core::capsule_v3::{verify_artifact_hash, CasStore};
use chrono::Utc;
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::json;
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;
use tower_http::cors::{Any, CorsLayer};

use crate::binding;
use crate::process_manager::{ProcessInfo, ProcessManager, ProcessStatus};
use crate::publish_artifact::{
    ChunkUploadResponse, SyncCommitRequest, SyncCommitResponse, SyncNegotiateRequest,
    SyncNegotiateResponse,
};
use crate::registry_store::{
    EpochResolveRequest, KeyRevokeRequest, KeyRotateRequest, LeaseRefreshRequest,
    LeaseReleaseRequest, NegotiateRequest, RegistryStore, RollbackRequest, YankRequest,
};
use crate::state::{ensure_registered_state_binding, load_manifest, open_state_store};

const README_CANDIDATES: [&str; 4] = ["README.md", "README.mdx", "README.txt", "README"];
const README_MAX_BYTES: usize = 512 * 1024;
const LOCAL_REGISTRY_DISABLE_UI_ENV: &str = "ATO_LOCAL_REGISTRY_DISABLE_UI";

#[derive(RustEmbed)]
#[folder = "apps/ato-store-local/dist"]
struct LocalRegistryUiAssets;

#[derive(Debug, Clone)]
pub struct RegistryServerConfig {
    pub host: String,
    pub port: u16,
    pub data_dir: String,
    pub auth_token: Option<String>,
}

#[derive(Clone)]
struct AppState {
    listen_url: String,
    data_dir: PathBuf,
    auth_token: Option<String>,
    lock: Arc<Mutex<()>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryIndex {
    schema_version: String,
    capsules: Vec<StoredCapsule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredCapsule {
    id: String,
    publisher: String,
    slug: String,
    name: String,
    description: String,
    category: String,
    #[serde(rename = "type")]
    capsule_type: String,
    price: u64,
    currency: String,
    latest_version: String,
    releases: Vec<StoredRelease>,
    downloads: u64,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredRelease {
    version: String,
    file_name: String,
    sha256: String,
    blake3: String,
    size_bytes: u64,
    signature_status: String,
    created_at: String,
    #[serde(default)]
    payload_v3: Option<StoredPayloadV3>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredPayloadV3 {
    artifact_hash: String,
    chunk_count: usize,
    total_raw_size: u64,
    manifest_rel_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoreMetadataEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    icon_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StoreMetadataIndex {
    #[serde(default)]
    entries: HashMap<String, StoreMetadataEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RuntimeConfigIndex {
    #[serde(default)]
    entries: HashMap<String, CapsuleRuntimeConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CapsuleRuntimeConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_target: Option<String>,
    #[serde(default)]
    targets: HashMap<String, RuntimeTargetConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PersistentStateListQuery {
    owner_scope: Option<String>,
    state_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct ServiceBindingListQuery {
    owner_scope: Option<String>,
    service_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct ServiceBindingResolveQuery {
    owner_scope: Option<String>,
    service_name: Option<String>,
    binding_kind: Option<String>,
    caller_service: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RegisterPersistentStateRequest {
    manifest: String,
    state_name: String,
    path: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RegisterServiceBindingRequest {
    manifest: String,
    service_name: String,
    url: Option<String>,
    binding_kind: Option<String>,
    process_id: Option<String>,
    port: Option<u16>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RunPermissionMode {
    Sandbox,
    Dangerous,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RuntimeTargetConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
    #[serde(default)]
    env: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    permission_mode: Option<RunPermissionMode>,
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: Option<String>,
    category: Option<String>,
    limit: Option<usize>,
    cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DistributionQuery {
    version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UploadQuery {
    file_name: Option<String>,
    allow_existing: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct UpsertStoreMetadataRequest {
    confirmed: bool,
    icon_path: Option<String>,
    text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpsertRuntimeConfigRequest {
    selected_target: Option<String>,
    targets: Option<HashMap<String, RuntimeTargetConfigRequest>>,
}

#[derive(Debug, Clone, Deserialize)]
struct RuntimeTargetConfigRequest {
    port: Option<u16>,
    env: Option<HashMap<String, String>>,
    permission_mode: Option<RunPermissionMode>,
}

#[derive(Debug, Deserialize)]
struct DeleteCapsuleQuery {
    version: Option<String>,
    confirmed: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ProcessLogsQuery {
    tail: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct UrlReadyQuery {
    url: Option<String>,
}

#[derive(Debug, Serialize)]
struct UploadResponse {
    scoped_id: String,
    version: String,
    artifact_url: String,
    file_name: String,
    sha256: String,
    blake3: String,
    size_bytes: u64,
    already_existed: bool,
}

#[derive(Debug, Serialize)]
struct DeleteCapsuleResponse {
    deleted: bool,
    scoped_id: String,
    removed_capsule: bool,
    removed_versions: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    removed_version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    removed_service_binding_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoreMetadataPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    icon_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    icon_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RunCapsuleRequest {
    confirmed: bool,
    target: Option<String>,
    port: Option<u16>,
    env: Option<HashMap<String, String>>,
    permission_mode: Option<RunPermissionMode>,
}

#[derive(Debug, Serialize)]
struct RunCapsuleResponse {
    accepted: bool,
    scoped_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    registered_service_binding_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RollbackResponsePayload {
    scoped_id: String,
    target_manifest_hash: String,
    manifest_hash: String,
    to_epoch: u64,
    pointer: capsule_core::types::EpochPointer,
    public_key: String,
}

#[derive(Debug, Deserialize)]
struct StopProcessRequest {
    confirmed: bool,
    force: Option<bool>,
}

#[derive(Debug, Serialize)]
struct StopProcessResponse {
    stopped: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    removed_service_binding_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ProcessRowResponse {
    id: String,
    name: String,
    pid: i32,
    status: String,
    active: bool,
    runtime: String,
    started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    scoped_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_port: Option<u16>,
}

#[derive(Debug, Serialize)]
struct ProcessLogsResponse {
    lines: Vec<String>,
    updated_at: String,
}

#[derive(Debug, Serialize)]
struct ClearLogsResponse {
    cleared: bool,
}

#[derive(Debug, Serialize)]
struct UrlReadyResponse {
    ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PublisherInfo {
    handle: String,
    #[serde(rename = "authorDid")]
    author_did: String,
    verified: bool,
}

#[derive(Debug, Clone, Serialize)]
struct SearchCapsuleRow {
    id: String,
    slug: String,
    scoped_id: String,
    #[serde(rename = "scopedId")]
    scoped_id_camel: String,
    name: String,
    description: String,
    category: String,
    #[serde(rename = "type")]
    capsule_type: String,
    price: u64,
    currency: String,
    publisher: PublisherInfo,
    #[serde(rename = "latestVersion")]
    latest_version: String,
    #[serde(rename = "latestSizeBytes")]
    latest_size_bytes: u64,
    downloads: u64,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    store_metadata: Option<StoreMetadataPayload>,
}

#[derive(Debug, Serialize)]
struct SearchResponse {
    capsules: Vec<SearchCapsuleRow>,
    next_cursor: Option<String>,
}

#[derive(Debug, Serialize)]
struct CapsuleDetailResponse {
    id: String,
    scoped_id: String,
    slug: String,
    name: String,
    description: String,
    price: u64,
    currency: String,
    #[serde(rename = "latestVersion")]
    latest_version: String,
    releases: Vec<CapsuleReleaseRow>,
    publisher: PublisherInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest_toml: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capsule_lock: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    readme_markdown: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    readme_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    store_metadata: Option<StoreMetadataPayload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_config: Option<CapsuleRuntimeConfig>,
}

#[derive(Debug, Serialize)]
struct CapsuleReleaseRow {
    version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest_hash: Option<String>,
    content_hash: String,
    signature_status: String,
    is_current: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    yanked_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct DistributionResponse {
    version: String,
    artifact_url: String,
    sha256: String,
    blake3: String,
    file_name: String,
    signature_status: String,
    publisher_verified: bool,
}

#[derive(Debug)]
struct ArtifactMeta {
    name: String,
    version: String,
    description: String,
}

pub async fn serve(config: RegistryServerConfig) -> Result<()> {
    let host = config.host;
    let access_host = display_access_host(&host);
    let auth_token = config
        .auth_token
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string());
    if host != "127.0.0.1" && auth_token.is_none() {
        bail!("--auth-token is required when binding local registry to non-loopback host");
    }
    let data_dir = expand_data_dir(&config.data_dir)?;
    initialize_storage(&data_dir)?;
    let listen_url = format!("http://{}:{}", host, config.port);
    let state = AppState {
        listen_url: listen_url.clone(),
        data_dir,
        auth_token,
        lock: Arc::new(Mutex::new(())),
    };
    spawn_registry_gc_worker(state.data_dir.clone());

    let ui_enabled = std::env::var_os(LOCAL_REGISTRY_DISABLE_UI_ENV).is_none();

    let mut app = Router::new();
    app = app.route("/.well-known/capsule.json", get(handle_well_known));
    app = app.route("/v1/capsules", get(handle_search_capsules));
    app = app.route("/v1/manifest/capsules", get(handle_search_capsules));
    app = app.route(
        "/v1/manifest/capsules/by/:publisher/:slug",
        get(handle_get_capsule),
    );
    app = app.route("/v1/capsules/by/:publisher/:slug", get(handle_get_capsule));
    app = app.route(
        "/v1/manifest/capsules/by/:publisher/:slug/distributions",
        get(handle_distributions),
    );
    app = app.route(
        "/v1/capsules/by/:publisher/:slug/distributions",
        get(handle_distributions),
    );
    app = app.route(
        "/v1/manifest/capsules/by/:publisher/:slug/download",
        get(handle_download),
    );
    app = app.route(
        "/v1/capsules/by/:publisher/:slug/download",
        get(handle_download),
    );
    app = app.route("/v1/manifest/negotiate", post(handle_manifest_negotiate));
    app = app.route(
        "/v1/manifest/resolve/:publisher/:slug/:version",
        get(handle_manifest_resolve_version),
    );
    app = app.route(
        "/v1/manifest/documents/:manifest_hash",
        get(handle_manifest_get_manifest),
    );
    app = app.route(
        "/v1/manifest/chunks/:chunk_hash",
        get(handle_manifest_get_chunk),
    );
    app = app.route(
        "/v1/manifest/epoch/resolve",
        post(handle_manifest_epoch_resolve),
    );
    app = app.route(
        "/v1/manifest/leases/refresh",
        post(handle_manifest_lease_refresh),
    );
    app = app.route(
        "/v1/manifest/leases/release",
        post(handle_manifest_lease_release),
    );
    app = app.route("/v1/manifest/keys/rotate", post(handle_manifest_key_rotate));
    app = app.route("/v1/manifest/keys/revoke", post(handle_manifest_key_revoke));
    app = app.route("/v1/manifest/rollback", post(handle_manifest_rollback));
    app = app.route("/v1/manifest/yank", post(handle_manifest_yank));
    app = app.route(
        "/v1/artifacts/:publisher/:slug/:version/:file_name",
        get(handle_get_artifact),
    );
    app = app.route("/v1/sync/negotiate", post(handle_sync_negotiate));
    app = app.route("/v1/sync/commit", post(handle_sync_commit));
    app = app.route("/v1/chunks/:raw_hash", put(handle_put_chunk));
    app = app.route("/v1/chunks/:raw_hash", get(handle_get_chunk));
    app = app.route(
        "/v1/releases/:publisher/:slug/:version/manifest",
        get(handle_get_release_manifest),
    );
    app = app.route(
        "/v1/local/capsules/:publisher/:slug/:version",
        put(handle_put_local_capsule),
    );
    app = app.route(
        "/v1/local/capsules/by/:publisher/:slug/store-metadata",
        get(handle_get_store_metadata).put(handle_put_store_metadata),
    );
    app = app.route(
        "/v1/local/capsules/by/:publisher/:slug/runtime-config",
        get(handle_get_runtime_config).put(handle_put_runtime_config),
    );
    app = app.route(
        "/v1/local/capsules/by/:publisher/:slug/store-icon",
        get(handle_get_store_icon),
    );
    app = app.route(
        "/v1/local/capsules/by/:publisher/:slug",
        delete(handle_delete_local_capsule),
    );
    app = app.route(
        "/v1/local/capsules/by/:publisher/:slug/run",
        post(handle_run_local_capsule),
    );
    app = app.route(
        "/v1/local/states",
        get(handle_list_persistent_states).post(handle_register_persistent_state),
    );
    app = app.route(
        "/v1/local/states/:state_id",
        get(handle_get_persistent_state),
    );
    app = app.route(
        "/v1/local/bindings",
        get(handle_list_service_bindings).post(handle_register_service_binding),
    );
    app = app.route(
        "/v1/local/bindings/resolve",
        get(handle_resolve_service_binding),
    );
    app = app.route(
        "/v1/local/bindings/:binding_id",
        get(handle_get_service_binding),
    );
    app = app.route("/v1/local/processes", get(handle_list_local_processes));
    app = app.route("/v1/local/url-ready", get(handle_local_url_ready));
    app = app.route(
        "/v1/local/processes/:id/stop",
        post(handle_stop_local_process),
    );
    app = app.route(
        "/v1/local/processes/:id/logs",
        get(handle_get_process_logs).delete(handle_clear_process_logs),
    );

    if ui_enabled {
        app = app.fallback(handle_ui_request);
    }

    let mut app = app.with_state(state);

    if std::env::var_os("ATO_LOCAL_REGISTRY_DEV_CORS").is_some() {
        app = app.layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([
                    Method::GET,
                    Method::PUT,
                    Method::POST,
                    Method::DELETE,
                    Method::OPTIONS,
                ])
                .allow_headers(Any),
        );
    }

    app = app.layer(DefaultBodyLimit::max(512 * 1024 * 1024));

    let addr: SocketAddr = format!("{}:{}", host, config.port)
        .parse()
        .context("Invalid listen address")?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|err| anyhow::anyhow!(format_bind_error(addr, &err)))?;
    let access_base_url = format!("http://{}:{}", access_host, config.port);
    println!("🚀 Local registry serving at {}", listen_url);
    println!("🔌 API: {}/v1/...", access_base_url);
    if ui_enabled {
        println!("🌐 Web UI: {}/", access_base_url);
    }
    if ui_enabled && LocalRegistryUiAssets::get("index.html").is_none() {
        println!("⚠️  Web UI assets are missing. Rebuild with `cargo build` after installing npm deps in apps/ato-store-local.");
    }
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("Local registry server failed")?;
    Ok(())
}

fn format_bind_error(addr: SocketAddr, err: &std::io::Error) -> String {
    let mut message = format!("Failed to bind {}: {}", addr, err);
    match err.kind() {
        ErrorKind::AddrInUse => {
            message.push_str(". Another process is already listening on that port. Try a different `--port` or inspect listeners with `lsof -nP -iTCP:<port> -sTCP:LISTEN`.");
        }
        ErrorKind::AddrNotAvailable => {
            message.push_str(". The requested host is not available on this machine.");
        }
        ErrorKind::PermissionDenied => {
            message.push_str(". Permission was denied while opening the socket.");
        }
        _ => {}
    }
    message
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

fn spawn_registry_gc_worker(data_dir: PathBuf) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        let mut ticks: u64 = 0;
        loop {
            interval.tick().await;
            let store = match RegistryStore::open(&data_dir) {
                Ok(store) => store,
                Err(err) => {
                    tracing::warn!(
                        "registry gc worker failed to open store path={} error={}",
                        data_dir.display(),
                        err
                    );
                    continue;
                }
            };

            let now = Utc::now().to_rfc3339();
            match store.gc_tick(&now, 32) {
                Ok(tick) => {
                    if tick.deleted > 0 {
                        let vacuum_pages = (tick.deleted.saturating_mul(2)).max(1);
                        if let Err(err) = store.incremental_vacuum(vacuum_pages) {
                            tracing::warn!(
                                "registry gc incremental vacuum failed path={} error={}",
                                data_dir.display(),
                                err
                            );
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        "registry gc tick failed path={} error={}",
                        data_dir.display(),
                        err
                    );
                }
            }

            ticks = ticks.saturating_add(1);
            if ticks.is_multiple_of(60) {
                if let Err(err) = store.checkpoint_wal_truncate() {
                    tracing::warn!(
                        "registry gc checkpoint failed path={} error={}",
                        data_dir.display(),
                        err
                    );
                }
            }
        }
    });
}

async fn handle_well_known(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let public_base_url = resolve_public_base_url(&headers, &state.listen_url);
    let write_auth_required = state.auth_token.is_some();
    Json(json!({
        "url": public_base_url,
        "name": "Ato Local Registry",
        "version": "1",
        "write_auth_required": write_auth_required
    }))
}

async fn handle_ui_request(uri: Uri) -> impl IntoResponse {
    let path = uri.path();
    if path == "/v1" || path.starts_with("/v1/") {
        return json_error(StatusCode::NOT_FOUND, "not_found", "API route not found");
    }

    if let Some(response) = ui_asset_response(path) {
        return response;
    }

    if let Some(response) = ui_embedded_response("index.html", true) {
        return response;
    }

    let html = "<!doctype html><html><head><meta charset=\"utf-8\"><title>Web UI unavailable</title></head><body style=\"font-family:sans-serif;padding:24px\"><h2>Web UI assets are missing</h2><p>Build <code>apps/ato-store-local</code> and rebuild <code>ato</code>.</p><pre>npm install --prefix apps/ato-store-local\ncargo build</pre></body></html>";
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        html,
    )
        .into_response()
}

fn ui_asset_response(request_path: &str) -> Option<axum::response::Response> {
    let normalized = normalize_ui_path(request_path)?;
    ui_embedded_response(&normalized, false)
}

fn normalize_ui_path(request_path: &str) -> Option<String> {
    let path = request_path.trim_start_matches('/');
    if path.is_empty() {
        return Some("index.html".to_string());
    }
    if path.contains('\\') || path.contains("..") {
        return None;
    }
    Some(path.to_string())
}

fn ui_embedded_response(path: &str, force_html: bool) -> Option<axum::response::Response> {
    let file = LocalRegistryUiAssets::get(path)?;
    let mime = if force_html {
        "text/html; charset=utf-8".to_string()
    } else {
        mime_guess::from_path(path)
            .first_or_octet_stream()
            .essence_str()
            .to_string()
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&mime)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(
        header::CACHE_CONTROL,
        cache_control_for_ui_path(path, force_html),
    );
    Some((StatusCode::OK, headers, file.data.into_owned()).into_response())
}

fn cache_control_for_ui_path(path: &str, force_html: bool) -> HeaderValue {
    if force_html || path == "index.html" {
        return HeaderValue::from_static("no-cache");
    }
    if path.starts_with("assets/") {
        return HeaderValue::from_static("public, max-age=31536000, immutable");
    }
    HeaderValue::from_static("public, max-age=300")
}

fn display_access_host(bind_host: &str) -> &str {
    match bind_host {
        "0.0.0.0" => "127.0.0.1",
        "::" | "[::]" => "::1",
        _ => bind_host,
    }
}

async fn handle_search_capsules(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> impl IntoResponse {
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
    let store_metadata = load_store_metadata(&state.data_dir).unwrap_or_default();
    let public_base_url = resolve_public_base_url(&headers, &state.listen_url);

    let limit = query.limit.unwrap_or(20).clamp(1, 50);
    let cursor = query
        .cursor
        .as_deref()
        .unwrap_or("0")
        .parse::<usize>()
        .unwrap_or(0);
    let needle = query.q.as_deref().unwrap_or("").trim().to_lowercase();
    let category = query.category.as_deref().map(str::to_lowercase);

    let mut rows = index
        .capsules
        .iter()
        .filter(|capsule| {
            let metadata_text =
                get_store_metadata_entry(&store_metadata, &capsule.publisher, &capsule.slug)
                    .and_then(|entry| entry.text.as_deref())
                    .unwrap_or(capsule.description.as_str());
            if needle.is_empty() {
                true
            } else {
                capsule.slug.to_lowercase().contains(&needle)
                    || capsule.name.to_lowercase().contains(&needle)
                    || metadata_text.to_lowercase().contains(&needle)
            }
        })
        .filter(|capsule| {
            category
                .as_ref()
                .map(|cat| capsule.category.to_lowercase() == *cat)
                .unwrap_or(true)
        })
        .map(|capsule| {
            let metadata =
                get_store_metadata_entry(&store_metadata, &capsule.publisher, &capsule.slug);
            stored_to_search_row(capsule, metadata, &public_base_url)
        })
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    let total = rows.len();
    let start = cursor.min(total);
    let end = (start + limit).min(total);
    let page = rows[start..end].to_vec();
    let next_cursor = if end < total {
        Some(end.to_string())
    } else {
        None
    };
    (
        StatusCode::OK,
        Json(SearchResponse {
            capsules: page,
            next_cursor,
        }),
    )
        .into_response()
}

async fn handle_get_capsule(
    State(state): State<AppState>,
    headers: HeaderMap,
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
    let Some(capsule) = index
        .capsules
        .iter()
        .find(|c| c.publisher == publisher && c.slug == slug)
    else {
        return json_error(StatusCode::NOT_FOUND, "not_found", "Capsule not found");
    };
    let store_metadata = load_store_metadata(&state.data_dir).unwrap_or_default();
    let metadata = get_store_metadata_entry(&store_metadata, &publisher, &slug);
    let public_base_url = resolve_public_base_url(&headers, &state.listen_url);
    let store_metadata_payload = metadata_to_payload(metadata, &public_base_url, &publisher, &slug);
    let runtime_config = load_runtime_config(&state.data_dir)
        .ok()
        .and_then(|index| get_runtime_config_entry(&index, &publisher, &slug).cloned());
    let scoped_id = format!("{}/{}", capsule.publisher, capsule.slug);
    let store = RegistryStore::open(&state.data_dir).ok();
    let current_manifest_hash = store.as_ref().and_then(|store| {
        store
            .resolve_epoch_pointer(&scoped_id)
            .ok()
            .flatten()
            .map(|response| response.pointer.manifest_hash)
    });

    let (manifest, repository, manifest_toml, capsule_lock, readme_markdown, readme_source) =
        load_capsule_detail_manifest(&state.data_dir, capsule);
    let readme_markdown = append_store_metadata_section(readme_markdown, metadata);
    let detail = CapsuleDetailResponse {
        id: capsule.id.clone(),
        scoped_id: scoped_id.clone(),
        slug: capsule.slug.clone(),
        name: capsule.name.clone(),
        description: metadata
            .and_then(|entry| entry.text.as_ref())
            .map(String::as_str)
            .unwrap_or(capsule.description.as_str())
            .to_string(),
        price: capsule.price,
        currency: capsule.currency.clone(),
        latest_version: capsule.latest_version.clone(),
        releases: capsule
            .releases
            .iter()
            .map(|release| {
                let resolved = store.as_ref().and_then(|store| {
                    store
                        .resolve_release_version(&publisher, &slug, &release.version)
                        .ok()
                        .flatten()
                });
                let manifest_hash = resolved.as_ref().map(|record| record.manifest_hash.clone());
                let is_current = manifest_hash.as_deref() == current_manifest_hash.as_deref();
                let yanked_at = resolved.and_then(|record| record.yanked_at);
                CapsuleReleaseRow {
                    version: release.version.clone(),
                    manifest_hash,
                    content_hash: release.blake3.clone(),
                    signature_status: release.signature_status.clone(),
                    is_current,
                    yanked_at,
                }
            })
            .collect(),
        publisher: publisher_info(&capsule.publisher),
        manifest,
        manifest_toml,
        capsule_lock,
        repository,
        readme_markdown,
        readme_source,
        store_metadata: store_metadata_payload,
        runtime_config,
    };
    (StatusCode::OK, Json(detail)).into_response()
}

async fn handle_distributions(
    State(state): State<AppState>,
    AxumPath((publisher, slug)): AxumPath<(String, String)>,
    Query(query): Query<DistributionQuery>,
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
    let Some(capsule) = index
        .capsules
        .iter()
        .find(|c| c.publisher == publisher && c.slug == slug)
    else {
        return json_error(StatusCode::NOT_FOUND, "not_found", "Capsule not found");
    };

    let requested = query
        .version
        .unwrap_or_else(|| capsule.latest_version.clone());
    let Some(release) = capsule.releases.iter().find(|r| r.version == requested) else {
        return json_error(
            StatusCode::NOT_FOUND,
            "version_not_found",
            "Version not found",
        );
    };
    let public_base_url = resolve_public_base_url(&headers, &state.listen_url);
    let artifact_url = format!(
        "{}/v1/artifacts/{}/{}/{}/{}",
        public_base_url, capsule.publisher, capsule.slug, release.version, release.file_name
    );
    let response = DistributionResponse {
        version: release.version.clone(),
        artifact_url,
        sha256: release.sha256.clone(),
        blake3: release.blake3.clone(),
        file_name: release.file_name.clone(),
        signature_status: release.signature_status.clone(),
        publisher_verified: true,
    };
    (StatusCode::OK, Json(response)).into_response()
}

async fn handle_download(
    State(state): State<AppState>,
    AxumPath((publisher, slug)): AxumPath<(String, String)>,
    Query(query): Query<DistributionQuery>,
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
    let Some(capsule) = index
        .capsules
        .iter()
        .find(|c| c.publisher == publisher && c.slug == slug)
    else {
        return json_error(StatusCode::NOT_FOUND, "not_found", "Capsule not found");
    };

    let requested = query
        .version
        .unwrap_or_else(|| capsule.latest_version.clone());
    let Some(release) = capsule.releases.iter().find(|r| r.version == requested) else {
        return json_error(
            StatusCode::NOT_FOUND,
            "version_not_found",
            "Version not found",
        );
    };

    let public_base_url = resolve_public_base_url(&headers, &state.listen_url);
    let artifact_url = format!(
        "{}/v1/artifacts/{}/{}/{}/{}",
        public_base_url, capsule.publisher, capsule.slug, release.version, release.file_name
    );
    (
        StatusCode::FOUND,
        [(header::LOCATION, artifact_url.as_str())],
    )
        .into_response()
}

async fn handle_manifest_negotiate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<NegotiateRequest>,
) -> impl IntoResponse {
    if let Err(err) = validate_read_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }
    let _guard = state.lock.lock().await;
    let store = match RegistryStore::open(&state.data_dir) {
        Ok(store) => store,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "registry_store_error",
                &err.to_string(),
            )
        }
    };
    match store.negotiate(&request) {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(err) => {
            let message = err.to_string();
            if message.contains("manifest yanked:") {
                return (
                    StatusCode::GONE,
                    Json(json!({
                        "error": "manifest_yanked",
                        "message": message,
                        "yanked": true
                    })),
                )
                    .into_response();
            }
            json_error(StatusCode::BAD_REQUEST, "negotiate_failed", &message)
        }
    }
}

async fn handle_manifest_get_manifest(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(manifest_hash): AxumPath<String>,
) -> impl IntoResponse {
    if let Err(err) = validate_read_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }
    let store = match RegistryStore::open(&state.data_dir) {
        Ok(store) => store,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "registry_store_error",
                &err.to_string(),
            )
        }
    };
    match store.load_manifest_document(&manifest_hash) {
        Ok(Some(document)) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/toml; charset=utf-8")],
            document,
        )
            .into_response(),
        Ok(None) => json_error(StatusCode::NOT_FOUND, "not_found", "Manifest not found"),
        Err(err) => {
            let message = err.to_string();
            if message.contains("manifest yanked:") {
                return (
                    StatusCode::GONE,
                    Json(json!({
                        "error": "manifest_yanked",
                        "message": message,
                        "yanked": true
                    })),
                )
                    .into_response();
            }
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "manifest_read_failed",
                &message,
            )
        }
    }
}

async fn handle_manifest_resolve_version(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath((publisher, slug, version)): AxumPath<(String, String, String)>,
) -> impl IntoResponse {
    if let Err(err) = validate_read_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }
    if let Err(err) = validate_capsule_segments(&publisher, &slug) {
        return json_error(StatusCode::BAD_REQUEST, "invalid_scope", &err.to_string());
    }
    let store = match RegistryStore::open(&state.data_dir) {
        Ok(store) => store,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "registry_store_error",
                &err.to_string(),
            )
        }
    };
    match store.resolve_release_version(&publisher, &slug, &version) {
        Ok(Some(record)) => {
            if let Some(yanked_at) = record.yanked_at.clone() {
                return (
                    StatusCode::GONE,
                    Json(json!({
                        "error": "manifest_yanked",
                        "message": format!(
                            "manifest yanked: scoped_id={} manifest_hash={} yanked_at={}",
                            record.scoped_id,
                            record.manifest_hash,
                            yanked_at
                        ),
                        "yanked": true,
                        "yanked_at": yanked_at
                    })),
                )
                    .into_response();
            }
            (StatusCode::OK, Json(record)).into_response()
        }
        Ok(None) => json_error(StatusCode::NOT_FOUND, "not_found", "Version not found"),
        Err(err) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "resolve_failed",
            &err.to_string(),
        ),
    }
}

async fn handle_manifest_get_chunk(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath(chunk_hash): AxumPath<String>,
) -> impl IntoResponse {
    if let Err(err) = validate_read_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }
    let store = match RegistryStore::open(&state.data_dir) {
        Ok(store) => store,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "registry_store_error",
                &err.to_string(),
            )
        }
    };
    match store.load_chunk_bytes(&chunk_hash) {
        Ok(Some(bytes)) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/octet-stream")],
            bytes,
        )
            .into_response(),
        Ok(None) => json_error(StatusCode::NOT_FOUND, "not_found", "Chunk not found"),
        Err(err) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "chunk_read_failed",
            &err.to_string(),
        ),
    }
}

async fn handle_manifest_epoch_resolve(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EpochResolveRequest>,
) -> impl IntoResponse {
    if let Err(err) = validate_read_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }
    let store = match RegistryStore::open(&state.data_dir) {
        Ok(store) => store,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "registry_store_error",
                &err.to_string(),
            )
        }
    };
    match store.resolve_epoch_pointer(&request.scoped_id) {
        Ok(Some(response)) => (StatusCode::OK, Json(response)).into_response(),
        Ok(None) => json_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "Epoch pointer not found",
        ),
        Err(err) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "epoch_resolve_failed",
            &err.to_string(),
        ),
    }
}

async fn handle_manifest_lease_refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<LeaseRefreshRequest>,
) -> impl IntoResponse {
    if let Err(err) = validate_read_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }
    let store = match RegistryStore::open(&state.data_dir) {
        Ok(store) => store,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "registry_store_error",
                &err.to_string(),
            )
        }
    };
    let ttl_secs = request.ttl_secs.unwrap_or(300).max(1);
    match store.refresh_lease(&request.lease_id, ttl_secs) {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(err) => json_error(
            StatusCode::BAD_REQUEST,
            "lease_refresh_failed",
            &err.to_string(),
        ),
    }
}

async fn handle_manifest_lease_release(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<LeaseReleaseRequest>,
) -> impl IntoResponse {
    if let Err(err) = validate_read_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }
    let store = match RegistryStore::open(&state.data_dir) {
        Ok(store) => store,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "registry_store_error",
                &err.to_string(),
            )
        }
    };
    match store.release_lease(&request.lease_id) {
        Ok(removed) => (StatusCode::OK, Json(json!({ "released": removed }))).into_response(),
        Err(err) => json_error(
            StatusCode::BAD_REQUEST,
            "lease_release_failed",
            &err.to_string(),
        ),
    }
}

async fn handle_manifest_key_rotate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<KeyRotateRequest>,
) -> impl IntoResponse {
    if let Err(err) = validate_write_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }
    let _guard = state.lock.lock().await;
    let store = match RegistryStore::open(&state.data_dir) {
        Ok(store) => store,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "registry_store_error",
                &err.to_string(),
            )
        }
    };
    match store.rotate_signing_key(request.overlap_hours.unwrap_or(24)) {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(err) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "key_rotate_failed",
            &err.to_string(),
        ),
    }
}

async fn handle_manifest_key_revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<KeyRevokeRequest>,
) -> impl IntoResponse {
    if let Err(err) = validate_write_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }
    let _guard = state.lock.lock().await;
    let store = match RegistryStore::open(&state.data_dir) {
        Ok(store) => store,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "registry_store_error",
                &err.to_string(),
            )
        }
    };
    match store.revoke_key(&request.key_id, request.did.as_deref()) {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(err) => json_error(
            StatusCode::BAD_REQUEST,
            "key_revoke_failed",
            &err.to_string(),
        ),
    }
}

async fn handle_manifest_rollback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RollbackRequest>,
) -> impl IntoResponse {
    if let Err(err) = validate_write_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }
    let _guard = state.lock.lock().await;
    let store = match RegistryStore::open(&state.data_dir) {
        Ok(store) => store,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "registry_store_error",
                &err.to_string(),
            )
        }
    };
    match store.rollback_to_manifest(&request.scoped_id, &request.target_manifest_hash) {
        Ok(Some(response)) => (
            StatusCode::OK,
            Json(RollbackResponsePayload {
                scoped_id: request.scoped_id.clone(),
                target_manifest_hash: request.target_manifest_hash.clone(),
                manifest_hash: response.pointer.manifest_hash.clone(),
                to_epoch: response.pointer.epoch,
                pointer: response.pointer,
                public_key: response.public_key,
            }),
        )
            .into_response(),
        Ok(None) => json_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "Rollback target not found",
        ),
        Err(err) => {
            let message = err.to_string();
            if message.contains(crate::error_codes::ATO_ERR_INTEGRITY_FAILURE)
                || message.contains("rollback target is yanked")
            {
                return json_error(StatusCode::CONFLICT, "rollback_failed", &message);
            }
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "rollback_failed",
                &message,
            )
        }
    }
}

async fn handle_manifest_yank(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<YankRequest>,
) -> impl IntoResponse {
    if let Err(err) = validate_write_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }
    let _guard = state.lock.lock().await;
    let store = match RegistryStore::open(&state.data_dir) {
        Ok(store) => store,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "registry_store_error",
                &err.to_string(),
            )
        }
    };
    match store.yank_manifest(&request.scoped_id, &request.target_manifest_hash) {
        Ok(true) => (
            StatusCode::OK,
            Json(json!({
                "scoped_id": request.scoped_id,
                "target_manifest_hash": request.target_manifest_hash,
                "yanked": true,
            })),
        )
            .into_response(),
        Ok(false) => json_error(
            StatusCode::NOT_FOUND,
            "not_found",
            "Yank target not found in capsule history",
        ),
        Err(err) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "yank_failed",
            &err.to_string(),
        ),
    }
}

async fn handle_get_artifact(
    State(state): State<AppState>,
    AxumPath((publisher, slug, version, file_name)): AxumPath<(String, String, String, String)>,
) -> impl IntoResponse {
    if let Err(err) = validate_capsule_segments(&publisher, &slug) {
        return json_error(StatusCode::BAD_REQUEST, "invalid_scope", &err.to_string());
    }
    if let Err(err) = validate_version(&version) {
        return json_error(StatusCode::BAD_REQUEST, "invalid_version", &err.to_string());
    }
    if let Err(err) = validate_file_name(&file_name) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_file_name",
            &err.to_string(),
        );
    }

    let path = artifact_path(&state.data_dir, &publisher, &slug, &version, &file_name);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return json_error(StatusCode::NOT_FOUND, "not_found", "Artifact not found"),
    };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/octet-stream")],
        bytes,
    )
        .into_response()
}

async fn handle_sync_negotiate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SyncNegotiateRequest>,
) -> impl IntoResponse {
    if let Err(err) = validate_write_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }

    if request.schema_version != 3 {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_schema_version",
            "schema_version must be 3",
        );
    }
    if let Err(err) = validate_blake3_digest("artifact_hash", &request.artifact_hash) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_artifact_hash",
            &err.to_string(),
        );
    }

    let cas = match registry_cas_store(&state.data_dir) {
        Ok(cas) => cas,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cas_init_failed",
                &err.to_string(),
            );
        }
    };

    let mut missing_chunks = Vec::new();
    for chunk in &request.chunks {
        if let Err(err) = validate_blake3_digest("chunk.raw_hash", &chunk.raw_hash) {
            return json_error(
                StatusCode::BAD_REQUEST,
                "invalid_chunk_hash",
                &err.to_string(),
            );
        }
        if chunk.raw_size == 0 {
            return json_error(
                StatusCode::BAD_REQUEST,
                "invalid_chunk_size",
                "chunk.raw_size must be greater than zero",
            );
        }

        match cas.has_chunk(&chunk.raw_hash) {
            Ok(true) => {}
            Ok(false) => missing_chunks.push(chunk.raw_hash.clone()),
            Err(err) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "cas_lookup_failed",
                    &err.to_string(),
                );
            }
        }
    }

    (
        StatusCode::OK,
        Json(SyncNegotiateResponse {
            missing_chunks,
            total_chunks: request.chunks.len(),
        }),
    )
        .into_response()
}

async fn handle_put_chunk(
    State(state): State<AppState>,
    AxumPath(raw_hash): AxumPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = validate_write_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }
    if let Err(err) = validate_blake3_digest("raw_hash", &raw_hash) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_chunk_hash",
            &err.to_string(),
        );
    }

    let raw_size = match parse_required_u32_header(&headers, "x-raw-size") {
        Ok(v) if v > 0 => v,
        Ok(_) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "invalid_chunk_size",
                "x-raw-size must be greater than zero",
            );
        }
        Err(err) => {
            return json_error(StatusCode::BAD_REQUEST, "missing_header", &err.to_string());
        }
    };

    if let Err(err) = verify_uploaded_chunk(&raw_hash, raw_size, &body) {
        return json_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "chunk_validation_failed",
            &err,
        );
    }

    let cas = match registry_cas_store(&state.data_dir) {
        Ok(cas) => cas,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cas_init_failed",
                &err.to_string(),
            );
        }
    };
    let put = match cas.put_chunk_zstd(&raw_hash, &body) {
        Ok(result) => result,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "chunk_store_failed",
                &err.to_string(),
            );
        }
    };

    let status = if put.inserted {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    (
        status,
        Json(ChunkUploadResponse {
            raw_hash,
            inserted: put.inserted,
            zstd_size: put.zstd_size,
        }),
    )
        .into_response()
}

async fn handle_get_chunk(
    State(state): State<AppState>,
    AxumPath(raw_hash): AxumPath<String>,
) -> impl IntoResponse {
    if let Err(err) = validate_blake3_digest("raw_hash", &raw_hash) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_chunk_hash",
            &err.to_string(),
        );
    }

    let cas = match registry_cas_store(&state.data_dir) {
        Ok(cas) => cas,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cas_init_failed",
                &err.to_string(),
            );
        }
    };
    let path = match cas.chunk_path(&raw_hash) {
        Ok(path) => path,
        Err(err) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "invalid_chunk_hash",
                &err.to_string(),
            );
        }
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return json_error(StatusCode::NOT_FOUND, "not_found", "Chunk not found"),
    };

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/zstd"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        bytes,
    )
        .into_response()
}

async fn handle_sync_commit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SyncCommitRequest>,
) -> impl IntoResponse {
    if let Err(err) = validate_write_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }
    if let Err(err) = validate_capsule_segments(&request.publisher, &request.slug) {
        return json_error(StatusCode::BAD_REQUEST, "invalid_scope", &err.to_string());
    }
    if let Err(err) = validate_version(&request.version) {
        return json_error(StatusCode::BAD_REQUEST, "invalid_version", &err.to_string());
    }
    if let Err(err) = request.manifest.validate() {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_manifest",
            &err.to_string(),
        );
    }
    if let Err(err) = verify_artifact_hash(&request.manifest) {
        return json_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "hash_mismatch",
            &err.to_string(),
        );
    }

    let cas = match registry_cas_store(&state.data_dir) {
        Ok(cas) => cas,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cas_init_failed",
                &err.to_string(),
            );
        }
    };
    let fsck = match cas.fsck_manifest(&request.manifest) {
        Ok(report) => report,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "fsck_failed",
                &err.to_string(),
            );
        }
    };
    if !fsck.is_ok() {
        let message = if fsck.hard_errors.is_empty() {
            "manifest references invalid chunks".to_string()
        } else {
            fsck.hard_errors.join("; ")
        };
        return json_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "manifest_chunks_invalid",
            &message,
        );
    }

    let canonical_manifest = match serde_jcs::to_vec(&request.manifest) {
        Ok(bytes) => bytes,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "manifest_serialize_failed",
                &err.to_string(),
            );
        }
    };
    let rel_path = release_manifest_rel_path(&request.publisher, &request.slug, &request.version);
    let abs_path = state.data_dir.join(&rel_path);
    if let Err(err) = atomic_write_bytes(&abs_path, &canonical_manifest) {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "manifest_write_failed",
            &err.to_string(),
        );
    }

    (
        StatusCode::CREATED,
        Json(SyncCommitResponse {
            scoped_id: format!("{}/{}", request.publisher, request.slug),
            version: request.version,
            artifact_hash: request.manifest.artifact_hash,
            chunk_count: request.manifest.chunks.len(),
            total_raw_size: request.manifest.total_raw_size,
        }),
    )
        .into_response()
}

async fn handle_get_release_manifest(
    State(state): State<AppState>,
    AxumPath((publisher, slug, version)): AxumPath<(String, String, String)>,
) -> impl IntoResponse {
    if let Err(err) = validate_capsule_segments(&publisher, &slug) {
        return json_error(StatusCode::BAD_REQUEST, "invalid_scope", &err.to_string());
    }
    if let Err(err) = validate_version(&version) {
        return json_error(StatusCode::BAD_REQUEST, "invalid_version", &err.to_string());
    }

    let _guard = state.lock.lock().await;
    let mut manifest_path = state
        .data_dir
        .join(release_manifest_rel_path(&publisher, &slug, &version));
    if let Ok(index) = load_index(&state.data_dir) {
        if let Some(rel_path) = index
            .capsules
            .iter()
            .find(|c| c.publisher == publisher && c.slug == slug)
            .and_then(|c| c.releases.iter().find(|r| r.version == version))
            .and_then(|r| r.payload_v3.as_ref())
            .map(|v| v.manifest_rel_path.clone())
        {
            manifest_path = state.data_dir.join(rel_path);
        }
    }
    let bytes = match std::fs::read(&manifest_path) {
        Ok(bytes) => bytes,
        Err(_) => {
            return json_error(
                StatusCode::NOT_FOUND,
                "not_found",
                "payload v3 manifest not found",
            )
        }
    };

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        bytes,
    )
        .into_response()
}

async fn handle_put_local_capsule(
    State(state): State<AppState>,
    AxumPath((publisher, slug, version)): AxumPath<(String, String, String)>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(err) = validate_write_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }

    if let Err(err) = validate_capsule_segments(&publisher, &slug) {
        return json_error(StatusCode::BAD_REQUEST, "invalid_scope", &err.to_string());
    }
    if let Err(err) = validate_version(&version) {
        return json_error(StatusCode::BAD_REQUEST, "invalid_version", &err.to_string());
    }
    let file_name = query
        .file_name
        .unwrap_or_else(|| format!("{}-{}.capsule", slug, version));
    let allow_existing = query.allow_existing.unwrap_or(false);
    if let Err(err) = validate_file_name(&file_name) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_file_name",
            &err.to_string(),
        );
    }

    let expected_sha = match get_required_header(&headers, "x-ato-sha256") {
        Ok(v) => v,
        Err(err) => return json_error(StatusCode::BAD_REQUEST, "missing_header", &err.to_string()),
    };
    let expected_blake3 = match get_required_header(&headers, "x-ato-blake3") {
        Ok(v) => v,
        Err(err) => return json_error(StatusCode::BAD_REQUEST, "missing_header", &err.to_string()),
    };

    let actual_sha = compute_sha256(&body);
    if !equals_hash(&expected_sha, &actual_sha) {
        return json_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "hash_mismatch",
            "sha256 mismatch",
        );
    }
    let actual_blake3 = compute_blake3(&body);
    if !equals_hash(&expected_blake3, &actual_blake3) {
        return json_error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "hash_mismatch",
            "blake3 mismatch",
        );
    }

    let artifact_meta = match parse_artifact_manifest(&body) {
        Ok(meta) => meta,
        Err(err) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "invalid_artifact",
                &format!("manifest parse failed: {}", err),
            )
        }
    };
    if artifact_meta.name != slug {
        return json_error(
            StatusCode::BAD_REQUEST,
            "scoped_id_mismatch",
            "path slug does not match artifact manifest.name",
        );
    }
    if artifact_meta.version != version {
        return json_error(
            StatusCode::BAD_REQUEST,
            "version_mismatch",
            "path version does not match artifact manifest.version",
        );
    }

    let _guard = state.lock.lock().await;
    let store = match RegistryStore::open(&state.data_dir) {
        Ok(store) => store,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "registry_store_error",
                &err.to_string(),
            )
        }
    };

    if let Some(existing_release) = match store.find_registry_release(&publisher, &slug, &version) {
        Ok(release) => release,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "registry_query_failed",
                &err.to_string(),
            )
        }
    } {
        match existing_release_outcome(&existing_release.sha256, allow_existing, &actual_sha) {
            ExistingReleaseOutcome::Reuse => {
                let public_base_url = resolve_public_base_url(&headers, &state.listen_url);
                let artifact_url = format!(
                    "{}/v1/artifacts/{}/{}/{}/{}",
                    public_base_url, publisher, slug, version, existing_release.file_name
                );
                return (
                    StatusCode::OK,
                    Json(UploadResponse {
                        scoped_id: format!("{}/{}", publisher, slug),
                        version,
                        artifact_url,
                        file_name: existing_release.file_name.clone(),
                        sha256: format!("sha256:{}", existing_release.sha256),
                        blake3: format!("blake3:{}", existing_release.blake3),
                        size_bytes: existing_release.size_bytes,
                        already_existed: true,
                    }),
                )
                    .into_response();
            }
            ExistingReleaseOutcome::Conflict(message) => {
                return json_error(StatusCode::CONFLICT, "version_exists", message);
            }
        }
    }

    let artifact_path = artifact_path(&state.data_dir, &publisher, &slug, &version, &file_name);
    if let Some(parent) = artifact_path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage_error",
                &format!("failed to create artifact dir: {}", err),
            );
        }
    }
    if let Err(err) = std::fs::write(&artifact_path, &body) {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "storage_error",
            &format!("failed to write artifact: {}", err),
        );
    }

    let now = Utc::now().to_rfc3339();
    if let Err(err) = store.publish_registry_release(
        &publisher,
        &slug,
        &artifact_meta.name,
        &artifact_meta.description,
        &version,
        &file_name,
        &actual_sha,
        &actual_blake3,
        body.len() as u64,
        &body,
        &now,
    ) {
        let message = err.to_string();
        let _ = std::fs::remove_file(&artifact_path);
        if message.contains("capsule.toml not found")
            || message.contains("payload.tar.zst not found")
        {
            return json_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "manifest_required",
                "capsule.toml and payload.tar.zst are required for upload",
            );
        }
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "registry_record_failed",
            &message,
        );
    }

    let public_base_url = resolve_public_base_url(&headers, &state.listen_url);
    let artifact_url = format!(
        "{}/v1/artifacts/{}/{}/{}/{}",
        public_base_url, publisher, slug, version, file_name
    );
    (
        StatusCode::CREATED,
        Json(UploadResponse {
            scoped_id: format!("{}/{}", publisher, slug),
            version,
            artifact_url,
            file_name,
            sha256: actual_sha,
            blake3: actual_blake3,
            size_bytes: body.len() as u64,
            already_existed: false,
        }),
    )
        .into_response()
}

async fn handle_get_store_metadata(
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

async fn handle_put_store_metadata(
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

async fn handle_get_store_icon(
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

async fn handle_get_runtime_config(
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

async fn handle_put_runtime_config(
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

async fn handle_run_local_capsule(
    State(state): State<AppState>,
    AxumPath((publisher, slug)): AxumPath<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<RunCapsuleRequest>,
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
    if request.port.is_some_and(|port| port == 0) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "invalid_port",
            "port must be between 1 and 65535",
        );
    }

    let requested_target = request
        .target
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let requested_env_overrides = request
        .env
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(key, value)| {
            let trimmed = key.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some((trimmed.to_string(), value))
            }
        })
        .collect::<HashMap<_, _>>();

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
    let Some(capsule) = index
        .capsules
        .iter()
        .find(|capsule| capsule.publisher == publisher && capsule.slug == slug)
    else {
        return json_error(StatusCode::NOT_FOUND, "not_found", "Capsule not found");
    };

    let local_artifact = resolve_run_artifact_path(&state.data_dir, capsule);
    let saved_runtime_config = load_runtime_config(&state.data_dir)
        .ok()
        .and_then(|cfg| get_runtime_config_entry(&cfg, &publisher, &slug).cloned());

    let mut effective_target = requested_target.clone().or_else(|| {
        saved_runtime_config
            .as_ref()
            .and_then(|cfg| normalize_optional_string(cfg.selected_target.clone()))
    });
    let mut effective_port = request.port;
    let mut effective_permission_mode = request.permission_mode;
    let mut env_overrides = HashMap::new();

    if let Some(saved) = saved_runtime_config.as_ref() {
        let saved_target_config = effective_target
            .as_deref()
            .and_then(|label| saved.targets.get(label))
            .or_else(|| {
                saved
                    .selected_target
                    .as_deref()
                    .and_then(|label| saved.targets.get(label))
            });
        if let Some(target_config) = saved_target_config {
            if effective_port.is_none() {
                effective_port = target_config.port;
            }
            if effective_permission_mode.is_none() {
                effective_permission_mode = target_config.permission_mode;
            }
            for (key, value) in &target_config.env {
                let normalized = key.trim();
                if !normalized.is_empty() {
                    env_overrides.insert(normalized.to_string(), value.clone());
                }
            }
            if effective_target.is_none() {
                effective_target = saved
                    .selected_target
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string);
            }
        }
    }
    env_overrides.extend(requested_env_overrides);

    if !env_overrides.contains_key("ATO_CONTROL_PLANE_PORT") {
        if let Some(port) = allocate_loopback_port() {
            env_overrides.insert("ATO_CONTROL_PLANE_PORT".to_string(), port.to_string());
        }
    }
    drop(_guard);
    let Some(local_artifact) = local_artifact else {
        return json_error(
            StatusCode::NOT_FOUND,
            "artifact_not_found",
            "artifact is missing in local registry storage",
        );
    };
    if !local_artifact.exists() {
        return json_error(
            StatusCode::NOT_FOUND,
            "artifact_not_found",
            "resolved artifact is missing in local registry storage",
        );
    }

    let scoped_id = format!("{}/{}", publisher, slug);
    let request_base_url = resolve_public_base_url(&headers, &state.listen_url);
    let registry_url =
        normalize_registry_base_url_for_local_run(&request_base_url, &state.listen_url);
    let ato_path = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "runtime_error",
                &format!("failed to resolve ato binary path: {}", err),
            );
        }
    };
    let run_target = local_artifact
        .canonicalize()
        .unwrap_or_else(|_| local_artifact.clone());
    let mut consent_manifest_tmpdir = None;
    let consent_manifest_path = if run_target
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("capsule"))
    {
        let bytes = match std::fs::read(&run_target) {
            Ok(bytes) => bytes,
            Err(err) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "run_plan_invalid",
                    &format!(
                        "failed to read local artifact for consent planning: {}",
                        err
                    ),
                );
            }
        };
        let manifest_raw = match extract_manifest_from_capsule(&bytes) {
            Ok(raw) => raw,
            Err(err) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "run_plan_invalid",
                    &format!("failed to extract capsule.toml from artifact: {}", err),
                );
            }
        };
        let temp_dir = match tempfile::tempdir() {
            Ok(dir) => dir,
            Err(err) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "run_plan_invalid",
                    &format!("failed to create consent planning workspace: {}", err),
                );
            }
        };
        let manifest_path = temp_dir.path().join("capsule.toml");
        if let Err(err) = std::fs::write(&manifest_path, manifest_raw) {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "run_plan_invalid",
                &format!("failed to prepare consent planning manifest: {}", err),
            );
        }
        consent_manifest_tmpdir = Some(temp_dir);
        manifest_path
    } else {
        run_target.clone()
    };
    let compiled = match capsule_core::execution_plan::derive::compile_execution_plan(
        &consent_manifest_path,
        capsule_core::router::ExecutionProfile::Dev,
        effective_target.as_deref(),
    ) {
        Ok(compiled) => compiled,
        Err(err) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                "run_plan_invalid",
                &format!("failed to prepare execution plan: {}", err),
            );
        }
    };
    let _ = consent_manifest_tmpdir.as_ref();
    if let Err(err) = crate::consent_store::seed_consent(&compiled.execution_plan) {
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "run_consent_seed_failed",
            &format!("failed to seed execution consent: {}", err),
        );
    }
    let mut cmd = std::process::Command::new(ato_path);
    cmd.arg("run")
        .arg(&run_target)
        .arg("--registry")
        .arg(&registry_url)
        .arg("--yes")
        .env("ATO_UI_SCOPED_ID", &scoped_id)
        .stdin(Stdio::null());
    if let Some(target) = effective_target.as_deref() {
        cmd.arg("--target").arg(target);
    }
    if let Some(port) = effective_port {
        cmd.env("ATO_UI_OVERRIDE_PORT", port.to_string());
    }
    match effective_permission_mode {
        Some(RunPermissionMode::Sandbox) => {
            cmd.arg("--sandbox");
        }
        Some(RunPermissionMode::Dangerous) => {
            cmd.arg("--dangerously-skip-permissions")
                .env("CAPSULE_ALLOW_UNSAFE", "1");
        }
        None => {}
    }
    if !env_overrides.is_empty() {
        let env_json = match serde_json::to_string(&env_overrides) {
            Ok(value) => value,
            Err(err) => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_env",
                    &format!("failed to serialize env overrides: {}", err),
                );
            }
        };
        cmd.env("ATO_UI_OVERRIDE_ENV_JSON", env_json);
    }

    let now = Utc::now();
    let nonce = now
        .timestamp_nanos_opt()
        .unwrap_or_else(|| now.timestamp_millis() * 1_000_000);
    let process_id = format!("capsule-{}-{}", nonce, std::process::id());
    let log_path = process_log_path(&process_id);
    if let Some(parent) = log_path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "run_spawn_failed",
                &format!("failed to prepare log directory: {}", err),
            );
        }
    }
    let log_file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(file) => file,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "run_spawn_failed",
                &format!("failed to open process log file: {}", err),
            );
        }
    };
    let log_file_err = match log_file.try_clone() {
        Ok(file) => file,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "run_spawn_failed",
                &format!("failed to clone process log handle: {}", err),
            );
        }
    };
    cmd.stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_err));

    #[cfg(unix)]
    unsafe {
        // Isolate spawned runtime into its own process group so stop can terminate the full tree.
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "run_spawn_failed",
                &format!("failed to spawn `ato run`: {}", err),
            );
        }
    };

    let pid = child.id() as i32;
    let process_info = ProcessInfo {
        id: process_id.clone(),
        name: slug.clone(),
        pid,
        workload_pid: None,
        status: ProcessStatus::Running,
        runtime: "ato-run".to_string(),
        start_time: std::time::SystemTime::now(),
        manifest_path: Some(run_target.clone()),
        scoped_id: Some(scoped_id.clone()),
        target_label: effective_target.clone(),
        requested_port: effective_port,
        log_path: Some(process_log_path(&process_id)),
        ready_at: Some(std::time::SystemTime::now()),
        last_event: Some("spawned".to_string()),
        last_error: None,
        exit_code: None,
    };
    let process_manager = match ProcessManager::new() {
        Ok(manager) => manager,
        Err(err) => {
            let _ = child.kill();
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "run_spawn_failed",
                &format!("failed to initialize process manager: {}", err),
            );
        }
    };
    if let Err(err) = process_manager.write_pid(&process_info) {
        let _ = child.kill();
        return json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "run_spawn_failed",
            &format!("failed to persist process record: {}", err),
        );
    }
    let registered_service_binding_ids =
        match binding::sync_service_bindings_for_process(&process_id) {
            Ok(records) => records
                .into_iter()
                .map(|record| record.binding_id)
                .collect(),
            Err(err) => {
                let _ = child.kill();
                let _ = process_manager.delete_pid(&process_id);
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "service_binding_register_failed",
                    &err.to_string(),
                );
            }
        };
    std::thread::spawn(move || {
        let _ = child.wait();
    });

    (
        StatusCode::ACCEPTED,
        Json(RunCapsuleResponse {
            accepted: true,
            scoped_id,
            requested_target: effective_target,
            requested_port: effective_port,
            registered_service_binding_ids,
        }),
    )
        .into_response()
}

async fn handle_delete_local_capsule(
    State(state): State<AppState>,
    AxumPath((publisher, slug)): AxumPath<(String, String)>,
    Query(query): Query<DeleteCapsuleQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(err) = validate_write_auth(&headers, state.auth_token.as_deref()) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized", &err);
    }
    if !query.confirmed.unwrap_or(false) {
        return json_error(
            StatusCode::BAD_REQUEST,
            "confirmation_required",
            "confirmed=true is required",
        );
    }
    if let Err(err) = validate_capsule_segments(&publisher, &slug) {
        return json_error(StatusCode::BAD_REQUEST, "invalid_scope", &err.to_string());
    }
    let delete_version = query
        .version
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    if let Some(version) = delete_version.as_deref() {
        if let Err(err) = validate_version(version) {
            return json_error(StatusCode::BAD_REQUEST, "invalid_version", &err.to_string());
        }
    }

    let scoped_id = format!("{}/{}", publisher, slug);
    let process_manager = match ProcessManager::new() {
        Ok(manager) => manager,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "process_manager_error",
                &err.to_string(),
            )
        }
    };
    let processes = match process_manager.list_processes() {
        Ok(processes) => processes,
        Err(err) => {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "process_list_failed",
                &err.to_string(),
            )
        }
    };
    if processes.iter().any(|process| {
        process.status.is_active() && process.scoped_id.as_deref() == Some(scoped_id.as_str())
    }) {
        return json_error(
            StatusCode::CONFLICT,
            "capsule_running",
            "capsule is running; stop active process before delete",
        );
    }
    let inactive_processes = processes
        .into_iter()
        .filter(|process| process.scoped_id.as_deref() == Some(scoped_id.as_str()))
        .collect::<Vec<_>>();

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
    let now = Utc::now().to_rfc3339();
    let outcome =
        match store.delete_registry_capsule(&publisher, &slug, delete_version.as_deref(), &now) {
            Ok(crate::registry_store::RegistryDeleteOutcome::CapsuleNotFound) => {
                return json_error(StatusCode::NOT_FOUND, "not_found", "Capsule not found");
            }
            Ok(crate::registry_store::RegistryDeleteOutcome::VersionNotFound(version)) => {
                return json_error(
                    StatusCode::NOT_FOUND,
                    "version_not_found",
                    &format!("Version '{}' not found", version),
                );
            }
            Ok(crate::registry_store::RegistryDeleteOutcome::Deleted(outcome)) => outcome,
            Err(err) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "delete_failed",
                    &err.to_string(),
                );
            }
        };

    let mut removed_service_binding_ids = Vec::new();
    if outcome.removed_capsule {
        if let Err(err) = store.delete_store_metadata(&scoped_id) {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_metadata_delete_failed",
                &err.to_string(),
            );
        }
        let mut runtime_config = load_runtime_config(&state.data_dir).unwrap_or_default();
        runtime_config
            .entries
            .remove(&runtime_config_key(&publisher, &slug));
        if let Err(err) = write_runtime_config(&state.data_dir, &runtime_config) {
            return json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "runtime_config_write_failed",
                &err.to_string(),
            );
        }
        for process in &inactive_processes {
            match binding::cleanup_service_bindings_for_process_info(process) {
                Ok(records) => removed_service_binding_ids
                    .extend(records.into_iter().map(|record| record.binding_id)),
                Err(err) => {
                    return json_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "service_binding_cleanup_failed",
                        &err.to_string(),
                    );
                }
            }
            if let Err(err) = process_manager.delete_pid(&process.id) {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "process_cleanup_failed",
                    &err.to_string(),
                );
            }
        }
    }

    cleanup_removed_artifacts(
        &state.data_dir,
        &publisher,
        &slug,
        &outcome.removed_releases,
    );
    (
        StatusCode::OK,
        Json(DeleteCapsuleResponse {
            deleted: true,
            scoped_id,
            removed_capsule: outcome.removed_capsule,
            removed_versions: outcome.removed_releases.len(),
            removed_version: outcome.removed_version,
            removed_service_binding_ids,
        }),
    )
        .into_response()
}

async fn handle_list_local_processes() -> impl IntoResponse {
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
    processes.sort_by(|left, right| right.start_time.cmp(&left.start_time));

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

async fn handle_list_persistent_states(
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
    match open_state_store().and_then(|store| store.list_persistent_states(owner_scope, state_name))
    {
        Ok(records) => (StatusCode::OK, Json(records)).into_response(),
        Err(err) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "persistent_state_list_failed",
            &err.to_string(),
        ),
    }
}

async fn handle_get_persistent_state(
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
    match open_state_store().and_then(|store| store.find_persistent_state_by_id(state_id)) {
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

async fn handle_list_service_bindings(
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
    match binding::open_binding_store()
        .and_then(|store| store.list_service_bindings(owner_scope, service_name))
    {
        Ok(records) => (StatusCode::OK, Json(records)).into_response(),
        Err(err) => json_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "service_binding_list_failed",
            &err.to_string(),
        ),
    }
}

async fn handle_get_service_binding(
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
    match binding::open_binding_store()
        .and_then(|store| store.find_service_binding_by_id(binding_id))
    {
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

async fn handle_resolve_service_binding(
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
    match binding::resolve_binding_record(owner_scope, service_name, binding_kind, caller_service) {
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

async fn handle_register_persistent_state(
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
    let result = load_manifest(Path::new(manifest))
        .and_then(|manifest| ensure_registered_state_binding(&manifest, state_name, path));
    match result {
        Ok(record) => (StatusCode::CREATED, Json(record)).into_response(),
        Err(err) => json_error(
            StatusCode::BAD_REQUEST,
            "persistent_state_register_failed",
            &err.to_string(),
        ),
    }
}

async fn handle_register_service_binding(
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

async fn handle_local_url_ready(Query(query): Query<UrlReadyQuery>) -> impl IntoResponse {
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

async fn handle_stop_local_process(
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

async fn handle_get_process_logs(
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

async fn handle_clear_process_logs(AxumPath(id): AxumPath<String>) -> impl IntoResponse {
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

fn process_log_path(id: &str) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".ato").join("logs").join(format!("{id}.log"))
}

fn read_process_log_lines(path: &Path, tail: usize) -> Vec<String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
    let all_lines = reader.lines().map_while(Result::ok).collect::<Vec<_>>();
    if all_lines.len() <= tail {
        return all_lines;
    }
    all_lines[all_lines.len() - tail..].to_vec()
}

#[cfg(test)]
#[derive(Debug)]
struct DeleteCapsuleResult {
    removed_capsule: bool,
    removed_version: Option<String>,
}

#[cfg(test)]
#[derive(Debug)]
enum DeleteCapsuleOutcome {
    CapsuleNotFound,
    VersionNotFound(String),
    Deleted(DeleteCapsuleResult),
}

#[cfg(test)]
fn delete_capsule_from_index(
    index: &mut RegistryIndex,
    publisher: &str,
    slug: &str,
    version: Option<&str>,
    now: &str,
) -> DeleteCapsuleOutcome {
    let Some(capsule_pos) = index
        .capsules
        .iter()
        .position(|capsule| capsule.publisher == publisher && capsule.slug == slug)
    else {
        return DeleteCapsuleOutcome::CapsuleNotFound;
    };

    if let Some(version) = version {
        let capsule = &mut index.capsules[capsule_pos];
        let Some(release_pos) = capsule
            .releases
            .iter()
            .position(|release| release.version == version)
        else {
            return DeleteCapsuleOutcome::VersionNotFound(version.to_string());
        };

        let removed = capsule.releases.remove(release_pos);
        if capsule.releases.is_empty() {
            index.capsules.remove(capsule_pos);
            return DeleteCapsuleOutcome::Deleted(DeleteCapsuleResult {
                removed_capsule: true,
                removed_version: Some(removed.version.clone()),
            });
        }

        if capsule.latest_version == removed.version {
            if let Some(last) = capsule.releases.last() {
                capsule.latest_version = last.version.clone();
            }
        }
        capsule.updated_at = now.to_string();
        return DeleteCapsuleOutcome::Deleted(DeleteCapsuleResult {
            removed_capsule: false,
            removed_version: Some(removed.version.clone()),
        });
    }

    index.capsules.remove(capsule_pos);
    DeleteCapsuleOutcome::Deleted(DeleteCapsuleResult {
        removed_capsule: true,
        removed_version: None,
    })
}

fn cleanup_removed_artifacts(
    data_dir: &Path,
    publisher: &str,
    slug: &str,
    releases: &[crate::registry_store::RegistryReleaseRecord],
) {
    for release in releases {
        let path = artifact_path(
            data_dir,
            publisher,
            slug,
            &release.version,
            &release.file_name,
        );
        if !path.exists() {
            continue;
        }
        if let Err(err) = std::fs::remove_file(&path) {
            tracing::warn!(
                "local registry failed to remove artifact file path={} error={}",
                path.display(),
                err
            );
        }
    }
}

fn validate_write_auth(headers: &HeaderMap, expected_token: Option<&str>) -> Result<(), String> {
    let Some(expected) = expected_token.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(());
    };

    let actual = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty());

    if constant_time_token_eq(expected.as_bytes(), actual.unwrap_or("").as_bytes()) {
        return Ok(());
    }

    Err("Bearer token is required for upload".to_string())
}

fn validate_read_auth(headers: &HeaderMap, expected_token: Option<&str>) -> Result<(), String> {
    let Some(expected) = expected_token.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(());
    };

    let actual = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty());

    if constant_time_token_eq(expected.as_bytes(), actual.unwrap_or("").as_bytes()) {
        return Ok(());
    }

    Err("Bearer token is required for manifest read API".to_string())
}

fn constant_time_token_eq(expected: &[u8], actual: &[u8]) -> bool {
    use sha2::{Digest, Sha256};

    let expected_digest = Sha256::digest(expected);
    let actual_digest = Sha256::digest(actual);
    expected_digest[..].ct_eq(&actual_digest[..]).into()
}

fn resolve_public_base_url(headers: &HeaderMap, fallback: &str) -> String {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| *v == "http" || *v == "https")
        .unwrap_or("http");

    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(header::HOST))
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.split(',').next().unwrap_or(v).trim().to_string());

    if let Some(host) = host {
        return format!("{}://{}", scheme, host);
    }

    fallback.to_string()
}

fn normalize_registry_base_url_for_local_run(request_base_url: &str, listen_url: &str) -> String {
    rewrite_wildcard_registry_host(request_base_url).unwrap_or_else(|| {
        rewrite_wildcard_registry_host(listen_url).unwrap_or_else(|| request_base_url.to_string())
    })
}

fn rewrite_wildcard_registry_host(raw: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(raw).ok()?;
    let host = url.host_str()?.to_string();
    let replacement = match host.as_str() {
        "0.0.0.0" => "127.0.0.1",
        "::" | "[::]" => "::1",
        _ => return Some(raw.to_string()),
    };
    url.set_host(Some(replacement)).ok()?;
    Some(url.to_string().trim_end_matches('/').to_string())
}

#[cfg(test)]
fn truncate_for_error(message: &str, max_chars: usize) -> String {
    if message.chars().count() <= max_chars {
        return message.to_string();
    }
    let head = message.chars().take(max_chars).collect::<String>();
    format!("{}...", head)
}

fn store_metadata_key(publisher: &str, slug: &str) -> String {
    format!("{}/{}", publisher, slug)
}

fn runtime_config_key(publisher: &str, slug: &str) -> String {
    format!("{}/{}", publisher, slug)
}

fn get_store_metadata_entry<'a>(
    index: &'a StoreMetadataIndex,
    publisher: &str,
    slug: &str,
) -> Option<&'a StoreMetadataEntry> {
    index.entries.get(&store_metadata_key(publisher, slug))
}

fn get_runtime_config_entry<'a>(
    index: &'a RuntimeConfigIndex,
    publisher: &str,
    slug: &str,
) -> Option<&'a CapsuleRuntimeConfig> {
    index.entries.get(&runtime_config_key(publisher, slug))
}

fn metadata_icon_url(
    base_url: &str,
    publisher: &str,
    slug: &str,
    icon_path: Option<&str>,
) -> Option<String> {
    icon_path.map(|_| {
        format!(
            "{}/v1/local/capsules/by/{}/{}/store-icon",
            base_url.trim_end_matches('/'),
            urlencoding::encode(publisher),
            urlencoding::encode(slug),
        )
    })
}

fn metadata_to_payload(
    metadata: Option<&StoreMetadataEntry>,
    base_url: &str,
    publisher: &str,
    slug: &str,
) -> Option<StoreMetadataPayload> {
    metadata.map(|entry| {
        let icon_path = entry.icon_path.clone();
        StoreMetadataPayload {
            icon_url: metadata_icon_url(base_url, publisher, slug, icon_path.as_deref()),
            icon_path,
            text: entry.text.clone(),
        }
    })
}

fn append_store_metadata_section(
    readme_markdown: Option<String>,
    metadata: Option<&StoreMetadataEntry>,
) -> Option<String> {
    let Some(entry) = metadata else {
        return readme_markdown;
    };
    if entry.icon_path.is_none() && entry.text.is_none() {
        return readme_markdown;
    }

    let mut section_lines = vec!["## store.metadata".to_string(), "".to_string()];
    if let Some(icon_path) = entry.icon_path.as_ref() {
        section_lines.push(format!("- file_path: `{}`", icon_path));
    }
    if let Some(text) = entry.text.as_ref() {
        section_lines.push(format!("- text: {}", text));
    }
    let section = section_lines.join("\n");
    match readme_markdown {
        Some(existing) if !existing.trim().is_empty() => {
            Some(format!("{}\n\n{}", existing.trim_end(), section))
        }
        _ => Some(section),
    }
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value
        .map(|raw| raw.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn publisher_info(handle: &str) -> PublisherInfo {
    PublisherInfo {
        handle: handle.to_string(),
        author_did: format!("did:key:local:{}", handle),
        verified: true,
    }
}

fn stored_to_search_row(
    capsule: &StoredCapsule,
    metadata: Option<&StoreMetadataEntry>,
    base_url: &str,
) -> SearchCapsuleRow {
    let scoped_id = format!("{}/{}", capsule.publisher, capsule.slug);
    let description = metadata
        .and_then(|entry| entry.text.as_ref())
        .map(String::as_str)
        .unwrap_or(capsule.description.as_str())
        .to_string();
    let latest_size_bytes = capsule
        .releases
        .iter()
        .find(|release| release.version == capsule.latest_version)
        .or_else(|| capsule.releases.last())
        .map(|release| release.size_bytes)
        .unwrap_or(0);
    let store_metadata = metadata_to_payload(metadata, base_url, &capsule.publisher, &capsule.slug);
    SearchCapsuleRow {
        id: capsule.id.clone(),
        slug: capsule.slug.clone(),
        scoped_id: scoped_id.clone(),
        scoped_id_camel: scoped_id,
        name: capsule.name.clone(),
        description,
        category: capsule.category.clone(),
        capsule_type: capsule.capsule_type.clone(),
        price: capsule.price,
        currency: capsule.currency.clone(),
        publisher: publisher_info(&capsule.publisher),
        latest_version: capsule.latest_version.clone(),
        latest_size_bytes,
        downloads: capsule.downloads,
        created_at: capsule.created_at.clone(),
        updated_at: capsule.updated_at.clone(),
        store_metadata,
    }
}

#[cfg(test)]
fn upsert_capsule(
    index: &mut RegistryIndex,
    publisher: &str,
    slug: &str,
    name: &str,
    description: &str,
    release: StoredRelease,
    now: &str,
) {
    if let Some(capsule) = index
        .capsules
        .iter_mut()
        .find(|c| c.publisher == publisher && c.slug == slug)
    {
        capsule.latest_version = release.version.clone();
        capsule.updated_at = now.to_string();
        capsule.releases.push(release);
        return;
    }

    index.capsules.push(StoredCapsule {
        id: format!("local-{}-{}", publisher, slug),
        publisher: publisher.to_string(),
        slug: slug.to_string(),
        name: name.to_string(),
        description: description.to_string(),
        category: "tools".to_string(),
        capsule_type: "app".to_string(),
        price: 0,
        currency: "usd".to_string(),
        latest_version: release.version.clone(),
        releases: vec![release],
        downloads: 0,
        created_at: now.to_string(),
        updated_at: now.to_string(),
    });
}

#[cfg(test)]
fn has_release_version(index: &RegistryIndex, publisher: &str, slug: &str, version: &str) -> bool {
    find_release_by_version(index, publisher, slug, version).is_some()
}

#[cfg(test)]
fn find_release_by_version<'a>(
    index: &'a RegistryIndex,
    publisher: &str,
    slug: &str,
    version: &str,
) -> Option<&'a StoredRelease> {
    index
        .capsules
        .iter()
        .find(|capsule| capsule.publisher == publisher && capsule.slug == slug)
        .and_then(|capsule| {
            capsule
                .releases
                .iter()
                .find(|release| release.version == version)
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExistingReleaseOutcome {
    Reuse,
    Conflict(&'static str),
}

fn existing_release_outcome(
    existing_sha256: &str,
    allow_existing: bool,
    actual_sha: &str,
) -> ExistingReleaseOutcome {
    if !allow_existing {
        return ExistingReleaseOutcome::Conflict("same version is already published");
    }

    if equals_hash(existing_sha256, actual_sha) {
        return ExistingReleaseOutcome::Reuse;
    }

    ExistingReleaseOutcome::Conflict("same version is already published (sha256 mismatch)")
}

fn get_required_header(headers: &HeaderMap, key: &str) -> Result<String> {
    headers
        .get(key)
        .and_then(|value| value.to_str().ok())
        .map(|v| v.to_string())
        .ok_or_else(|| anyhow::anyhow!("required header '{}' is missing", key))
}

fn parse_required_u32_header(headers: &HeaderMap, key: &str) -> Result<u32> {
    let value = get_required_header(headers, key)?;
    value
        .trim()
        .parse::<u32>()
        .with_context(|| format!("invalid '{}' header value: {}", key, value))
}

fn verify_uploaded_chunk(
    raw_hash: &str,
    raw_size: u32,
    zstd_bytes: &[u8],
) -> std::result::Result<(), String> {
    let mut decoder = zstd::stream::Decoder::new(Cursor::new(zstd_bytes))
        .map_err(|e| format!("failed to initialize zstd decoder: {}", e))?;

    let mut hasher = blake3::Hasher::new();
    let mut total = 0u64;
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = decoder
            .read(&mut buf)
            .map_err(|e| format!("failed to decode zstd chunk: {}", e))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        hasher.update(&buf[..n]);
    }

    if total != raw_size as u64 {
        return Err(format!(
            "raw size mismatch: expected {} got {}",
            raw_size, total
        ));
    }

    let computed = format!("blake3:{}", hasher.finalize().to_hex());
    if computed != raw_hash {
        return Err(format!(
            "raw hash mismatch: expected {} got {}",
            raw_hash, computed
        ));
    }
    Ok(())
}

fn registry_cas_store(data_dir: &Path) -> Result<CasStore> {
    CasStore::new(data_dir.join("cas")).map_err(|e| anyhow::anyhow!("{}", e))
}

fn parse_artifact_manifest(bytes: &[u8]) -> Result<ArtifactMeta> {
    let manifest = extract_manifest_from_capsule(bytes)?;
    let parsed = capsule_core::types::CapsuleManifest::from_toml(&manifest)
        .map_err(|err| anyhow::anyhow!("{}", err))?;
    Ok(ArtifactMeta {
        name: parsed.name,
        version: parsed.version,
        description: parsed.metadata.description.unwrap_or_default(),
    })
}

fn extract_manifest_from_capsule(bytes: &[u8]) -> Result<String> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive
        .entries()
        .context("Failed to iterate artifact entries")?;
    for entry in entries {
        let mut entry = entry.context("Invalid artifact entry")?;
        let entry_path = entry.path()?.to_string_lossy().to_string();
        if entry_path == "capsule.toml" {
            let mut manifest = String::new();
            entry
                .read_to_string(&mut manifest)
                .context("Failed to read capsule.toml")?;
            return Ok(manifest);
        }
    }

    bail!("capsule.toml not found in artifact")
}

fn extract_capsule_lock_from_capsule(bytes: &[u8]) -> Option<String> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive.entries().ok()?;
    for entry in entries {
        let mut entry = entry.ok()?;
        let entry_path = entry.path().ok()?.to_string_lossy().to_string();
        if entry_path == "capsule.lock.json" || entry_path == "capsule.lock" {
            let mut lock = String::new();
            entry.read_to_string(&mut lock).ok()?;
            return Some(lock);
        }
    }
    None
}

fn collect_readme_candidates<R: Read>(archive: &mut tar::Archive<R>) -> HashMap<String, Vec<u8>> {
    let mut candidates = HashMap::new();
    let Ok(entries) = archive.entries() else {
        return candidates;
    };

    for entry in entries {
        let mut entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let entry_path = match entry.path() {
            Ok(path) => path.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        let file_name = match entry_path.rsplit('/').next() {
            Some(name) => name.to_string(),
            None => continue,
        };
        if !README_CANDIDATES
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(file_name.as_str()))
        {
            continue;
        }

        let mut buf = Vec::new();
        if entry.read_to_end(&mut buf).is_err() {
            continue;
        }
        if buf.len() > README_MAX_BYTES {
            buf.truncate(README_MAX_BYTES);
        }
        candidates.entry(file_name).or_insert(buf);
    }

    candidates
}

fn extract_readme_from_capsule(bytes: &[u8]) -> Option<String> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let mut candidates = collect_readme_candidates(&mut archive);

    if candidates.is_empty() {
        let mut archive = tar::Archive::new(Cursor::new(bytes));
        let entries = archive.entries().ok()?;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let entry_path = match entry.path() {
                Ok(path) => path.to_string_lossy().to_string(),
                Err(_) => continue,
            };
            if entry_path != "payload.tar.zst" {
                continue;
            }

            let decoder = match zstd::stream::Decoder::new(entry) {
                Ok(decoder) => decoder,
                Err(_) => continue,
            };
            let mut payload_archive = tar::Archive::new(decoder);
            candidates = collect_readme_candidates(&mut payload_archive);
            if !candidates.is_empty() {
                break;
            }
        }
    }

    for candidate in README_CANDIDATES {
        if let Some((_, content)) = candidates
            .iter()
            .find(|(name, _)| candidate.eq_ignore_ascii_case(name.as_str()))
        {
            return Some(String::from_utf8_lossy(content).to_string());
        }
    }
    None
}

type CapsuleDetailManifestParts = (
    Option<serde_json::Value>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

fn load_capsule_detail_manifest(
    data_dir: &Path,
    capsule: &StoredCapsule,
) -> CapsuleDetailManifestParts {
    let Some(release) = capsule
        .releases
        .iter()
        .find(|release| release.version == capsule.latest_version)
        .or_else(|| capsule.releases.last())
    else {
        return (None, None, None, None, None, None);
    };
    let path = artifact_path(
        data_dir,
        &capsule.publisher,
        &capsule.slug,
        &release.version,
        &release.file_name,
    );

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::warn!(
                "local registry failed to read artifact for detail manifest path={} error={}",
                path.display(),
                err
            );
            return (None, None, None, None, None, None);
        }
    };
    let readme_markdown = extract_readme_from_capsule(&bytes);
    let capsule_lock = extract_capsule_lock_from_capsule(&bytes);
    let readme_source = readme_markdown
        .as_ref()
        .map(|_| "artifact".to_string())
        .or_else(|| Some("none".to_string()));
    let manifest_raw = match extract_manifest_from_capsule(&bytes) {
        Ok(raw) => raw,
        Err(err) => {
            tracing::warn!(
                "local registry failed to extract capsule.toml for {}/{}@{}: {}",
                capsule.publisher,
                capsule.slug,
                release.version,
                err
            );
            return (
                None,
                None,
                None,
                capsule_lock,
                readme_markdown,
                readme_source,
            );
        }
    };
    let parsed = toml::from_str::<toml::Value>(&manifest_raw);
    let (manifest, repository) = match parsed {
        Ok(parsed) => {
            let repository = extract_repository_from_manifest(&parsed);
            let manifest = match serde_json::to_value(parsed) {
                Ok(value) => Some(value),
                Err(err) => {
                    tracing::warn!(
                        "local registry failed to serialize manifest JSON for {}/{}@{}: {}",
                        capsule.publisher,
                        capsule.slug,
                        release.version,
                        err
                    );
                    None
                }
            };
            (manifest, repository)
        }
        Err(err) => {
            tracing::warn!(
                "local registry failed to parse capsule.toml for {}/{}@{}: {}",
                capsule.publisher,
                capsule.slug,
                release.version,
                err
            );
            (None, None)
        }
    };
    (
        manifest,
        repository,
        Some(manifest_raw),
        capsule_lock,
        readme_markdown,
        readme_source,
    )
}

fn extract_repository_from_manifest(parsed: &toml::Value) -> Option<String> {
    parsed
        .get("metadata")
        .and_then(|v| v.get("repository"))
        .and_then(toml::Value::as_str)
        .or_else(|| parsed.get("repository").and_then(toml::Value::as_str))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

fn expand_data_dir(raw: &str) -> Result<PathBuf> {
    if raw == "~" {
        return dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Failed to resolve home directory"));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        let home =
            dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Failed to resolve home directory"))?;
        return Ok(home.join(rest));
    }
    Ok(PathBuf::from(raw))
}

fn initialize_storage(data_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("Failed to create data dir {}", data_dir.display()))?;
    std::fs::create_dir_all(data_dir.join("artifacts"))
        .with_context(|| format!("Failed to create artifact dir {}", data_dir.display()))?;
    let _ = RegistryStore::open(data_dir)?;
    let runtime_config_path = runtime_config_path(data_dir);
    if !runtime_config_path.exists() {
        write_runtime_config(data_dir, &RuntimeConfigIndex::default())?;
    }
    Ok(())
}

fn runtime_config_path(data_dir: &Path) -> PathBuf {
    data_dir.join("runtime-config.json")
}

fn load_index(data_dir: &Path) -> Result<RegistryIndex> {
    let store = RegistryStore::open(data_dir)?;
    let packages = store.list_registry_packages()?;
    Ok(RegistryIndex {
        schema_version: "local-registry-v1".to_string(),
        capsules: packages
            .into_iter()
            .map(|package| StoredCapsule {
                id: format!("local-{}-{}", package.publisher, package.slug),
                publisher: package.publisher,
                slug: package.slug,
                name: package.name,
                description: package.description,
                category: "tools".to_string(),
                capsule_type: "app".to_string(),
                price: 0,
                currency: "usd".to_string(),
                latest_version: package.latest_version,
                releases: package
                    .releases
                    .into_iter()
                    .map(|release| StoredRelease {
                        version: release.version,
                        file_name: release.file_name,
                        sha256: format!("sha256:{}", release.sha256),
                        blake3: format!("blake3:{}", release.blake3),
                        size_bytes: release.size_bytes,
                        signature_status: release.signature_status,
                        created_at: release.created_at,
                        payload_v3: None,
                    })
                    .collect(),
                downloads: 0,
                created_at: package.created_at,
                updated_at: package.updated_at,
            })
            .collect(),
    })
}

fn release_manifest_rel_path(publisher: &str, slug: &str, version: &str) -> PathBuf {
    PathBuf::from("payload-v3")
        .join(publisher)
        .join(slug)
        .join(format!("{}.json", version))
}

fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context("payload v3 manifest path must have a parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create directory {}", parent.display()))?;

    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, bytes)
        .with_context(|| format!("Failed to write temporary file {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "Failed to atomically rename {} to {}",
            tmp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

fn load_store_metadata(data_dir: &Path) -> Result<StoreMetadataIndex> {
    let store = RegistryStore::open(data_dir)?;
    let entries = store.list_store_metadata_entries()?;
    let mut index = StoreMetadataIndex::default();
    for entry in entries {
        index.entries.insert(
            entry.scoped_id,
            StoreMetadataEntry {
                icon_path: entry.icon_path,
                text: entry.text,
                updated_at: entry.updated_at,
            },
        );
    }
    Ok(index)
}

fn load_runtime_config(data_dir: &Path) -> Result<RuntimeConfigIndex> {
    let path = runtime_config_path(data_dir);
    if !path.exists() {
        return Ok(RuntimeConfigIndex::default());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let parsed = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(parsed)
}

fn write_runtime_config(data_dir: &Path, config: &RuntimeConfigIndex) -> Result<()> {
    let path = runtime_config_path(data_dir);
    let json =
        serde_json::to_string_pretty(config).context("Failed to serialize runtime config")?;
    std::fs::write(&path, json).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

fn expand_user_path(raw: &str) -> PathBuf {
    if raw == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(raw));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(raw)
}

fn artifact_path(
    data_dir: &Path,
    publisher: &str,
    slug: &str,
    version: &str,
    file_name: &str,
) -> PathBuf {
    data_dir
        .join("artifacts")
        .join(publisher)
        .join(slug)
        .join(version)
        .join(file_name)
}

fn resolve_run_artifact_path(data_dir: &Path, capsule: &StoredCapsule) -> Option<PathBuf> {
    // Prefer the freshest on-disk artifact to avoid stale legacy index snapshots.
    find_latest_capsule_artifact_on_disk(data_dir, &capsule.publisher, &capsule.slug).or_else(
        || {
            capsule
                .releases
                .iter()
                .find(|release| release.version == capsule.latest_version)
                .map(|release| {
                    artifact_path(
                        data_dir,
                        &capsule.publisher,
                        &capsule.slug,
                        &release.version,
                        &release.file_name,
                    )
                })
        },
    )
}

fn find_latest_capsule_artifact_on_disk(
    data_dir: &Path,
    publisher: &str,
    slug: &str,
) -> Option<PathBuf> {
    let root = data_dir.join("artifacts").join(publisher).join(slug);
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;

    let versions = std::fs::read_dir(root).ok()?;
    for version_entry in versions.flatten() {
        let version_path = version_entry.path();
        if !version_path.is_dir() {
            continue;
        }
        let files = match std::fs::read_dir(&version_path) {
            Ok(files) => files,
            Err(_) => continue,
        };
        for file_entry in files.flatten() {
            let file_path = file_entry.path();
            if !file_path.is_file() {
                continue;
            }
            if file_path.extension().and_then(|ext| ext.to_str()) != Some("capsule") {
                continue;
            }
            let modified = file_entry
                .metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let is_newer = newest
                .as_ref()
                .map(|(current, _)| modified > *current)
                .unwrap_or(true);
            if is_newer {
                newest = Some((modified, file_path));
            }
        }
    }

    newest.map(|(_, path)| path)
}

fn allocate_loopback_port() -> Option<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
    let port = listener.local_addr().ok()?.port();
    if port == 0 {
        None
    } else {
        Some(port)
    }
}

fn validate_capsule_segments(publisher: &str, slug: &str) -> Result<()> {
    let scoped = format!("{}/{}", publisher, slug);
    let _ = crate::install::parse_capsule_ref(&scoped)?;
    Ok(())
}

fn validate_version(value: &str) -> Result<()> {
    if value.is_empty() || value.contains('/') || value.contains('\\') || value.contains("..") {
        bail!("invalid version segment");
    }
    Ok(())
}

fn validate_file_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.contains('/')
        || value.contains('\\')
        || value.contains("..")
        || !value.to_ascii_lowercase().ends_with(".capsule")
    {
        bail!("file_name must be a .capsule file name");
    }
    Ok(())
}

fn compute_sha256(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

fn compute_blake3(data: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(data);
    format!("blake3:{}", hex::encode(hasher.finalize().as_bytes()))
}

fn equals_hash(expected: &str, got: &str) -> bool {
    let normalize = |value: &str| {
        value
            .trim()
            .trim_start_matches("sha256:")
            .trim_start_matches("blake3:")
            .to_ascii_lowercase()
    };
    normalize(expected) == normalize(got)
}

fn json_error(status: StatusCode, error: &str, message: &str) -> axum::response::Response {
    (
        status,
        Json(json!({
            "error": error,
            "message": message
        })),
    )
        .into_response()
}

impl Default for RegistryIndex {
    fn default() -> Self {
        Self {
            schema_version: "local-registry-v1".to_string(),
            capsules: vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use std::io::{Cursor, Write};
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::{Mutex as StdMutex, OnceLock};

    fn env_lock() -> &'static StdMutex<()> {
        static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| StdMutex::new(()))
    }

    #[test]
    fn format_bind_error_mentions_port_conflict_guidance() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9090);
        let err = std::io::Error::new(ErrorKind::AddrInUse, "Address already in use");
        let message = format_bind_error(addr, &err);
        assert!(message.contains("Failed to bind 127.0.0.1:9090"));
        assert!(message.contains("Address already in use"));
        assert!(message.contains("Another process is already listening"));
        assert!(message.contains("lsof -nP -iTCP:<port> -sTCP:LISTEN"));
    }

    #[test]
    fn format_bind_error_preserves_generic_io_message() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9090);
        let err = std::io::Error::other("boom");
        let message = format_bind_error(addr, &err);
        assert!(message.contains("Failed to bind 127.0.0.1:9090: boom"));
        assert!(!message.contains("Another process is already listening"));
    }

    struct HomeGuard {
        previous: Option<std::ffi::OsString>,
    }

    impl HomeGuard {
        fn set(path: &std::path::Path) -> Self {
            let previous = std::env::var_os("HOME");
            std::env::set_var("HOME", path);
            Self { previous }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                std::env::set_var("HOME", previous);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    fn build_capsule_bytes(manifest: &str) -> Vec<u8> {
        build_capsule_bytes_with_files(manifest, &[("README.md", b"dummy".as_slice())])
    }

    fn build_capsule_bytes_with_files(manifest: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
        let payload_tar = build_payload_tar().expect("build payload tar");
        let parsed_manifest =
            capsule_core::types::CapsuleManifest::from_toml(manifest).expect("parse manifest");
        let (distribution_manifest, _) =
            capsule_core::packers::payload::build_distribution_manifest(
                &parsed_manifest,
                &payload_tar,
            )
            .expect("build distribution manifest");
        let mut raw_manifest: toml::Value = toml::from_str(manifest).expect("parse raw manifest");
        let raw_manifest_table = raw_manifest
            .as_table_mut()
            .expect("raw manifest must be a table");
        raw_manifest_table.insert(
            "schema_version".to_string(),
            toml::Value::String(distribution_manifest.schema_version.clone()),
        );
        raw_manifest_table.insert(
            "distribution".to_string(),
            toml::Value::try_from(
                distribution_manifest
                    .distribution
                    .expect("distribution metadata"),
            )
            .expect("distribution value"),
        );
        let manifest_bytes = toml::to_string_pretty(&raw_manifest).expect("serialize manifest");
        let payload_zst =
            zstd::stream::encode_all(Cursor::new(payload_tar), 1).expect("encode payload");

        let mut out = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut out);
            let mut header = tar::Header::new_gnu();
            header.set_path("capsule.toml").expect("set path");
            header.set_mode(0o644);
            header.set_size(manifest_bytes.len() as u64);
            header.set_cksum();
            builder
                .append_data(&mut header, "capsule.toml", Cursor::new(manifest_bytes))
                .expect("append manifest");

            let mut payload_header = tar::Header::new_gnu();
            payload_header
                .set_path("payload.tar.zst")
                .expect("set payload path");
            payload_header.set_mode(0o644);
            payload_header.set_size(payload_zst.len() as u64);
            payload_header.set_cksum();
            builder
                .append_data(
                    &mut payload_header,
                    "payload.tar.zst",
                    Cursor::new(payload_zst),
                )
                .expect("append payload");

            for (path, bytes) in files {
                let mut extra_header = tar::Header::new_gnu();
                extra_header.set_path(path).expect("set path");
                extra_header.set_mode(0o644);
                extra_header.set_size(bytes.len() as u64);
                extra_header.set_cksum();
                builder
                    .append_data(&mut extra_header, *path, *bytes)
                    .expect("append extra");
            }
            builder.finish().expect("finish archive");
        }
        out.flush().expect("flush vec");
        out
    }

    fn build_capsule_bytes_with_payload_files(
        manifest: &str,
        payload_files: &[(&str, &[u8])],
    ) -> Vec<u8> {
        let payload_tar = build_payload_tar_with_files(payload_files).expect("build payload tar");
        let parsed_manifest =
            capsule_core::types::CapsuleManifest::from_toml(manifest).expect("parse manifest");
        let (distribution_manifest, _) =
            capsule_core::packers::payload::build_distribution_manifest(
                &parsed_manifest,
                &payload_tar,
            )
            .expect("build distribution manifest");
        let mut raw_manifest: toml::Value = toml::from_str(manifest).expect("parse raw manifest");
        let raw_manifest_table = raw_manifest
            .as_table_mut()
            .expect("raw manifest must be a table");
        raw_manifest_table.insert(
            "schema_version".to_string(),
            toml::Value::String(distribution_manifest.schema_version.clone()),
        );
        raw_manifest_table.insert(
            "distribution".to_string(),
            toml::Value::try_from(
                distribution_manifest
                    .distribution
                    .expect("distribution metadata"),
            )
            .expect("distribution value"),
        );
        let manifest_bytes = toml::to_string_pretty(&raw_manifest).expect("serialize manifest");
        let payload_zst =
            zstd::stream::encode_all(Cursor::new(payload_tar), 1).expect("encode payload");

        let mut out = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut out);
            let mut header = tar::Header::new_gnu();
            header.set_path("capsule.toml").expect("set path");
            header.set_mode(0o644);
            header.set_size(manifest_bytes.len() as u64);
            header.set_cksum();
            builder
                .append_data(&mut header, "capsule.toml", Cursor::new(manifest_bytes))
                .expect("append manifest");

            let mut payload_header = tar::Header::new_gnu();
            payload_header
                .set_path("payload.tar.zst")
                .expect("set payload path");
            payload_header.set_mode(0o644);
            payload_header.set_size(payload_zst.len() as u64);
            payload_header.set_cksum();
            builder
                .append_data(
                    &mut payload_header,
                    "payload.tar.zst",
                    Cursor::new(payload_zst),
                )
                .expect("append payload");
            builder.finish().expect("finish archive");
        }
        out.flush().expect("flush vec");
        out
    }

    fn build_payload_tar() -> Result<Vec<u8>> {
        build_payload_tar_with_files(&[])
    }

    fn build_payload_tar_with_files(files: &[(&str, &[u8])]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut out);
            let source = b"print('hello from registry test')\n";
            let mut header = tar::Header::new_gnu();
            header.set_path("main.py")?;
            header.set_mode(0o644);
            header.set_size(source.len() as u64);
            header.set_mtime(0);
            header.set_cksum();
            builder.append_data(&mut header, "main.py", Cursor::new(source))?;
            for (path, bytes) in files {
                let mut extra_header = tar::Header::new_gnu();
                extra_header.set_path(path)?;
                extra_header.set_mode(0o644);
                extra_header.set_size(bytes.len() as u64);
                extra_header.set_mtime(0);
                extra_header.set_cksum();
                builder.append_data(&mut extra_header, *path, Cursor::new(*bytes))?;
            }
            builder.finish()?;
        }
        out.flush().expect("flush payload vec");
        Ok(out)
    }

    #[allow(dead_code)]
    fn compress(data: &[u8]) -> Vec<u8> {
        let mut encoder = zstd::Encoder::new(Vec::new(), 3).expect("encoder");
        encoder.write_all(data).expect("write");
        encoder.finish().expect("finish")
    }

    #[test]
    fn initialize_storage_creates_index() {
        let tmp = tempfile::tempdir().expect("tempdir");
        initialize_storage(tmp.path()).expect("initialize");
        let index = load_index(tmp.path()).expect("load index");
        assert_eq!(index.schema_version, "local-registry-v1");
        assert!(index.capsules.is_empty());
    }

    #[test]
    fn duplicate_version_is_detected() {
        let mut index = RegistryIndex::default();
        let now = Utc::now().to_rfc3339();
        upsert_capsule(
            &mut index,
            "koh0920",
            "sample-capsule",
            "sample-capsule",
            "",
            StoredRelease {
                version: "1.0.0".to_string(),
                file_name: "sample.capsule".to_string(),
                sha256: "sha256:abc".to_string(),
                blake3: "blake3:def".to_string(),
                size_bytes: 1,
                signature_status: "verified".to_string(),
                created_at: now.clone(),
                payload_v3: None,
            },
            &now,
        );
        assert!(has_release_version(
            &index,
            "koh0920",
            "sample-capsule",
            "1.0.0"
        ));
    }

    #[test]
    fn delete_capsule_from_index_removes_requested_version_only() {
        let mut index = RegistryIndex::default();
        let now = Utc::now().to_rfc3339();
        upsert_capsule(
            &mut index,
            "koh0920",
            "sample-capsule",
            "sample-capsule",
            "",
            StoredRelease {
                version: "1.0.0".to_string(),
                file_name: "sample-1.0.0.capsule".to_string(),
                sha256: "sha256:abc".to_string(),
                blake3: "blake3:def".to_string(),
                size_bytes: 1,
                signature_status: "verified".to_string(),
                created_at: now.clone(),
                payload_v3: None,
            },
            &now,
        );
        upsert_capsule(
            &mut index,
            "koh0920",
            "sample-capsule",
            "sample-capsule",
            "",
            StoredRelease {
                version: "1.1.0".to_string(),
                file_name: "sample-1.1.0.capsule".to_string(),
                sha256: "sha256:ghi".to_string(),
                blake3: "blake3:jkl".to_string(),
                size_bytes: 1,
                signature_status: "verified".to_string(),
                created_at: now.clone(),
                payload_v3: None,
            },
            &now,
        );

        let outcome =
            delete_capsule_from_index(&mut index, "koh0920", "sample-capsule", Some("1.1.0"), &now);
        let DeleteCapsuleOutcome::Deleted(result) = outcome else {
            panic!("expected deleted outcome");
        };
        assert!(!result.removed_capsule);
        assert_eq!(result.removed_version.as_deref(), Some("1.1.0"));
        assert!(has_release_version(
            &index,
            "koh0920",
            "sample-capsule",
            "1.0.0"
        ));
        assert!(!has_release_version(
            &index,
            "koh0920",
            "sample-capsule",
            "1.1.0"
        ));
    }

    #[test]
    fn delete_capsule_from_index_removes_capsule_when_last_release_deleted() {
        let mut index = RegistryIndex::default();
        let now = Utc::now().to_rfc3339();
        upsert_capsule(
            &mut index,
            "koh0920",
            "sample-capsule",
            "sample-capsule",
            "",
            StoredRelease {
                version: "1.0.0".to_string(),
                file_name: "sample-1.0.0.capsule".to_string(),
                sha256: "sha256:abc".to_string(),
                blake3: "blake3:def".to_string(),
                size_bytes: 1,
                signature_status: "verified".to_string(),
                created_at: now.clone(),
                payload_v3: None,
            },
            &now,
        );
        let outcome =
            delete_capsule_from_index(&mut index, "koh0920", "sample-capsule", Some("1.0.0"), &now);
        let DeleteCapsuleOutcome::Deleted(result) = outcome else {
            panic!("expected deleted outcome");
        };
        assert!(result.removed_capsule);
        assert!(index.capsules.is_empty());
    }

    #[test]
    fn delete_capsule_from_index_reports_version_not_found() {
        let mut index = RegistryIndex::default();
        let now = Utc::now().to_rfc3339();
        upsert_capsule(
            &mut index,
            "koh0920",
            "sample-capsule",
            "sample-capsule",
            "",
            StoredRelease {
                version: "1.0.0".to_string(),
                file_name: "sample-1.0.0.capsule".to_string(),
                sha256: "sha256:abc".to_string(),
                blake3: "blake3:def".to_string(),
                size_bytes: 1,
                signature_status: "verified".to_string(),
                created_at: now.clone(),
                payload_v3: None,
            },
            &now,
        );
        let outcome =
            delete_capsule_from_index(&mut index, "koh0920", "sample-capsule", Some("9.9.9"), &now);
        let DeleteCapsuleOutcome::VersionNotFound(version) = outcome else {
            panic!("expected version not found");
        };
        assert_eq!(version, "9.9.9");
    }

    #[test]
    fn existing_release_outcome_requires_opt_in() {
        let release = StoredRelease {
            version: "1.0.0".to_string(),
            file_name: "sample.capsule".to_string(),
            sha256: "sha256:abc".to_string(),
            blake3: "blake3:def".to_string(),
            size_bytes: 1,
            signature_status: "verified".to_string(),
            created_at: Utc::now().to_rfc3339(),
            payload_v3: None,
        };

        let outcome = existing_release_outcome(&release.sha256, false, "sha256:abc");
        assert_eq!(
            outcome,
            ExistingReleaseOutcome::Conflict("same version is already published")
        );
    }

    #[test]
    fn existing_release_outcome_reuses_when_sha256_matches() {
        let release = StoredRelease {
            version: "1.0.0".to_string(),
            file_name: "sample.capsule".to_string(),
            sha256: "sha256:abc".to_string(),
            blake3: "blake3:def".to_string(),
            size_bytes: 1,
            signature_status: "verified".to_string(),
            created_at: Utc::now().to_rfc3339(),
            payload_v3: None,
        };

        let outcome = existing_release_outcome(&release.sha256, true, "sha256:abc");
        assert_eq!(outcome, ExistingReleaseOutcome::Reuse);
    }

    #[test]
    fn existing_release_outcome_conflicts_when_sha256_differs() {
        let release = StoredRelease {
            version: "1.0.0".to_string(),
            file_name: "sample.capsule".to_string(),
            sha256: "sha256:abc".to_string(),
            blake3: "blake3:def".to_string(),
            size_bytes: 1,
            signature_status: "verified".to_string(),
            created_at: Utc::now().to_rfc3339(),
            payload_v3: None,
        };

        let outcome = existing_release_outcome(&release.sha256, true, "sha256:xyz");
        assert_eq!(
            outcome,
            ExistingReleaseOutcome::Conflict("same version is already published (sha256 mismatch)")
        );
    }

    #[test]
    fn search_cursor_paginates() {
        let mut index = RegistryIndex::default();
        let now = Utc::now().to_rfc3339();
        for slug in ["a", "b", "c"] {
            upsert_capsule(
                &mut index,
                "koh0920",
                slug,
                slug,
                "",
                StoredRelease {
                    version: "1.0.0".to_string(),
                    file_name: format!("{slug}.capsule"),
                    sha256: "sha256:abc".to_string(),
                    blake3: "blake3:def".to_string(),
                    size_bytes: 1,
                    signature_status: "verified".to_string(),
                    created_at: now.clone(),
                    payload_v3: None,
                },
                &now,
            );
        }
        let rows = index
            .capsules
            .iter()
            .map(|capsule| stored_to_search_row(capsule, None, "http://127.0.0.1:8787"))
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].publisher.handle, "koh0920");
    }

    #[test]
    fn validate_write_auth_allows_when_disabled() {
        let headers = HeaderMap::new();
        assert!(validate_write_auth(&headers, None).is_ok());
    }

    #[test]
    fn validate_write_auth_requires_matching_bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            "Bearer secret-token".parse().unwrap(),
        );
        assert!(validate_write_auth(&headers, Some("secret-token")).is_ok());
        assert!(validate_write_auth(&headers, Some("wrong-token")).is_err());
        let empty = HeaderMap::new();
        assert!(validate_write_auth(&empty, Some("secret-token")).is_err());
    }

    #[test]
    fn validate_read_auth_requires_matching_bearer_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            "Bearer secret-token".parse().unwrap(),
        );
        assert!(validate_read_auth(&headers, Some("secret-token")).is_ok());
        assert!(validate_read_auth(&headers, Some("wrong-token")).is_err());
        let empty = HeaderMap::new();
        assert!(validate_read_auth(&empty, Some("secret-token")).is_err());
    }

    #[test]
    fn constant_time_token_eq_handles_length_mismatch() {
        assert!(constant_time_token_eq(b"secret-token", b"secret-token"));
        assert!(!constant_time_token_eq(b"secret-token", b"secret-token-x"));
        assert!(!constant_time_token_eq(b"secret-token", b"secret"));
    }

    #[test]
    fn resolve_public_base_url_uses_host_header() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "100.64.0.10:8787".parse().unwrap());
        let url = resolve_public_base_url(&headers, "http://0.0.0.0:8787");
        assert_eq!(url, "http://100.64.0.10:8787");
    }

    #[test]
    fn resolve_public_base_url_uses_forwarded_host_and_proto() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", "https".parse().unwrap());
        headers.insert("x-forwarded-host", "store.example.com".parse().unwrap());
        let url = resolve_public_base_url(&headers, "http://127.0.0.1:8787");
        assert_eq!(url, "https://store.example.com");
    }

    #[test]
    fn resolve_public_base_url_falls_back_when_headers_missing() {
        let headers = HeaderMap::new();
        let url = resolve_public_base_url(&headers, "http://127.0.0.1:8787");
        assert_eq!(url, "http://127.0.0.1:8787");
    }

    #[test]
    fn normalize_registry_base_url_for_local_run_rewrites_wildcard_host() {
        let rewritten =
            normalize_registry_base_url_for_local_run("http://0.0.0.0:9000", "http://0.0.0.0:9000");
        assert_eq!(rewritten, "http://127.0.0.1:9000");
    }

    #[test]
    fn truncate_for_error_limits_message_length() {
        let input = "a".repeat(1000);
        let truncated = truncate_for_error(&input, 32);
        assert!(truncated.starts_with(&"a".repeat(32)));
        assert!(truncated.ends_with("..."));
    }

    #[test]
    fn extract_manifest_from_capsule_returns_text() {
        let manifest = r#"schema_version = "0.2"
name = "sample"
version = "1.0.0"
type = "app"
default_target = "cli"
"#;
        let bytes = build_capsule_bytes(manifest);
        let extracted = extract_manifest_from_capsule(&bytes).expect("extract");
        assert!(extracted.contains("name = \"sample\""));
    }

    #[test]
    fn extract_readme_from_capsule_prefers_priority_order() {
        let manifest = r#"schema_version = "0.2"
name = "sample"
version = "1.0.0"
type = "app"
default_target = "cli"
"#;
        let bytes = build_capsule_bytes_with_files(
            manifest,
            &[
                ("README.txt", b"txt readme"),
                ("docs/README.mdx", b"mdx readme"),
                ("README.md", b"markdown readme"),
            ],
        );
        let extracted = extract_readme_from_capsule(&bytes);
        assert_eq!(extracted.as_deref(), Some("markdown readme"));
    }

    #[test]
    fn extract_readme_from_capsule_truncates_large_files() {
        let manifest = r#"schema_version = "0.2"
name = "sample"
version = "1.0.0"
type = "app"
default_target = "cli"
"#;
        let large = vec![b'a'; README_MAX_BYTES + 4096];
        let bytes = build_capsule_bytes_with_files(manifest, &[("README.md", &large)]);
        let extracted = extract_readme_from_capsule(&bytes).expect("extract readme");
        assert_eq!(extracted.len(), README_MAX_BYTES);
    }

    #[test]
    fn extract_readme_from_capsule_reads_payload_tar_zst_contents() {
        let manifest = r#"schema_version = "0.2"
name = "sample"
version = "1.0.0"
type = "app"
default_target = "cli"
"#;
        let bytes = build_capsule_bytes_with_payload_files(
            manifest,
            &[("README.md", b"payload readme markdown")],
        );
        let extracted = extract_readme_from_capsule(&bytes);
        assert_eq!(extracted.as_deref(), Some("payload readme markdown"));
    }

    #[test]
    fn extract_repository_from_manifest_prefers_metadata_then_root() {
        let parsed: toml::Value = toml::from_str(
            r#"
repository = "root/repo"
[metadata]
repository = "meta/repo"
"#,
        )
        .expect("parse");
        assert_eq!(
            extract_repository_from_manifest(&parsed).as_deref(),
            Some("meta/repo")
        );

        let parsed_root: toml::Value =
            toml::from_str(r#"repository = "root-only/repo""#).expect("parse");
        assert_eq!(
            extract_repository_from_manifest(&parsed_root).as_deref(),
            Some("root-only/repo")
        );
    }

    #[test]
    fn load_capsule_detail_manifest_reads_latest_release_artifact() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let manifest = r#"schema_version = "0.2"
name = "sample"
version = "1.0.0"
type = "app"
default_target = "cli"

[metadata]
repository = "koh0920/sample"
"#;
        let file_name = "sample-1.0.0.capsule";
        let artifact = artifact_path(tmp.path(), "local", "sample", "1.0.0", file_name);
        std::fs::create_dir_all(artifact.parent().expect("parent")).expect("mkdir");
        std::fs::write(&artifact, build_capsule_bytes(manifest)).expect("write artifact");

        let capsule = StoredCapsule {
            id: "id-1".to_string(),
            publisher: "local".to_string(),
            slug: "sample".to_string(),
            name: "sample".to_string(),
            description: "".to_string(),
            category: "tools".to_string(),
            capsule_type: "app".to_string(),
            price: 0,
            currency: "usd".to_string(),
            latest_version: "1.0.0".to_string(),
            releases: vec![StoredRelease {
                version: "1.0.0".to_string(),
                file_name: file_name.to_string(),
                sha256: "sha256:x".to_string(),
                blake3: "blake3:y".to_string(),
                size_bytes: 1,
                signature_status: "verified".to_string(),
                created_at: Utc::now().to_rfc3339(),
                payload_v3: None,
            }],
            downloads: 0,
            created_at: Utc::now().to_rfc3339(),
            updated_at: Utc::now().to_rfc3339(),
        };

        let (
            manifest_json,
            repository,
            manifest_toml,
            capsule_lock,
            readme_markdown,
            readme_source,
        ) = load_capsule_detail_manifest(tmp.path(), &capsule);
        let manifest_json = manifest_json.expect("manifest json");
        assert_eq!(
            manifest_json
                .get("name")
                .and_then(serde_json::Value::as_str),
            Some("sample")
        );
        assert_eq!(repository.as_deref(), Some("koh0920/sample"));
        assert!(manifest_toml
            .as_deref()
            .is_some_and(|raw| raw.contains("default_target = \"cli\"")));
        assert!(capsule_lock.is_none());
        assert_eq!(readme_markdown.as_deref(), Some("dummy"));
        assert_eq!(readme_source.as_deref(), Some("artifact"));
    }

    #[test]
    fn normalize_ui_path_maps_root_to_index() {
        assert_eq!(normalize_ui_path("/").as_deref(), Some("index.html"),);
        assert_eq!(
            normalize_ui_path("/assets/index.js").as_deref(),
            Some("assets/index.js"),
        );
        assert!(normalize_ui_path("/../../etc/passwd").is_none());
    }

    #[test]
    fn cache_control_for_ui_path_respects_spa_policy() {
        assert_eq!(
            cache_control_for_ui_path("index.html", false),
            HeaderValue::from_static("no-cache")
        );
        assert_eq!(
            cache_control_for_ui_path("assets/index-abc.js", false),
            HeaderValue::from_static("public, max-age=31536000, immutable")
        );
    }

    #[test]
    fn read_process_log_lines_applies_tail_limit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("capsule-123.log");
        std::fs::write(&path, "line1\nline2\nline3\n").expect("write log");
        let lines = read_process_log_lines(&path, 2);
        assert_eq!(lines, vec!["line2".to_string(), "line3".to_string()]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manifest_yank_requires_auth() {
        let tmp = tempfile::tempdir().expect("tempdir");
        initialize_storage(tmp.path()).expect("init");
        let state = AppState {
            listen_url: "http://127.0.0.1:8787".to_string(),
            data_dir: tmp.path().to_path_buf(),
            auth_token: Some("secret".to_string()),
            lock: Arc::new(Mutex::new(())),
        };
        let response = handle_manifest_yank(
            State(state),
            HeaderMap::new(),
            Json(YankRequest {
                scoped_id: "koh0920/sample".to_string(),
                target_manifest_hash: "blake3:deadbeef".to_string(),
            }),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manifest_yank_rejects_unknown_history_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        initialize_storage(tmp.path()).expect("init");
        let state = AppState {
            listen_url: "http://127.0.0.1:8787".to_string(),
            data_dir: tmp.path().to_path_buf(),
            auth_token: Some("secret".to_string()),
            lock: Arc::new(Mutex::new(())),
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_static("Bearer secret"),
        );
        let response = handle_manifest_yank(
            State(state),
            headers,
            Json(YankRequest {
                scoped_id: "koh0920/sample".to_string(),
                target_manifest_hash: "blake3:deadbeef".to_string(),
            }),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn yanked_manifest_blocks_negotiate_and_manifest_fetch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        initialize_storage(tmp.path()).expect("init");
        let store = RegistryStore::open(tmp.path()).expect("open store");
        let recorded = store
            .record_manifest_and_epoch(
                "koh0920/sample",
                "schema_version = \"0.2\"\nname = \"sample\"\nversion = \"1.0.0\"\ntype = \"app\"\ndefault_target = \"cli\"\n",
                b"payload-v1",
                "2026-03-05T00:00:00Z",
            )
            .expect("record");
        let yanked = store
            .yank_manifest("koh0920/sample", &recorded.pointer.manifest_hash)
            .expect("yank");
        assert!(yanked);

        let state = AppState {
            listen_url: "http://127.0.0.1:8787".to_string(),
            data_dir: tmp.path().to_path_buf(),
            auth_token: None,
            lock: Arc::new(Mutex::new(())),
        };
        let negotiate_resp = handle_manifest_negotiate(
            State(state.clone()),
            HeaderMap::new(),
            Json(NegotiateRequest {
                scoped_id: "koh0920/sample".to_string(),
                target_manifest_hash: recorded.pointer.manifest_hash.clone(),
                have_chunks: vec![],
                have_chunks_bloom: None,
                reuse_lease_id: None,
                max_bytes: None,
            }),
        )
        .await
        .into_response();
        assert_eq!(negotiate_resp.status(), StatusCode::GONE);
        let negotiate_body = to_bytes(negotiate_resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let negotiate_json: serde_json::Value =
            serde_json::from_slice(&negotiate_body).expect("parse json");
        assert_eq!(
            negotiate_json.get("yanked"),
            Some(&serde_json::Value::Bool(true))
        );

        let manifest_resp = handle_manifest_get_manifest(
            State(state),
            HeaderMap::new(),
            AxumPath(recorded.pointer.manifest_hash),
        )
        .await
        .into_response();
        assert_eq!(manifest_resp.status(), StatusCode::GONE);
        let manifest_body = to_bytes(manifest_resp.into_body(), usize::MAX)
            .await
            .expect("read body");
        let manifest_json: serde_json::Value =
            serde_json::from_slice(&manifest_body).expect("parse json");
        assert_eq!(
            manifest_json.get("yanked"),
            Some(&serde_json::Value::Bool(true))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn version_resolve_returns_manifest_hash_for_release() {
        let tmp = tempfile::tempdir().expect("tempdir");
        initialize_storage(tmp.path()).expect("init");
        let store = RegistryStore::open(tmp.path()).expect("open store");
        let manifest = "schema_version = \"0.2\"\nname = \"sample\"\nversion = \"1.0.0\"\ntype = \"app\"\ndefault_target = \"cli\"\n";
        let capsule = build_capsule_bytes(manifest);
        let published = store
            .publish_registry_release(
                "koh0920",
                "sample",
                "sample",
                "demo",
                "1.0.0",
                "sample-1.0.0.capsule",
                "sha256:abc",
                "blake3:def",
                capsule.len() as u64,
                &capsule,
                "2026-03-05T00:00:00Z",
            )
            .expect("publish");

        let state = AppState {
            listen_url: "http://127.0.0.1:8787".to_string(),
            data_dir: tmp.path().to_path_buf(),
            auth_token: None,
            lock: Arc::new(Mutex::new(())),
        };
        let response = handle_manifest_resolve_version(
            State(state),
            HeaderMap::new(),
            AxumPath((
                "koh0920".to_string(),
                "sample".to_string(),
                "1.0.0".to_string(),
            )),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json: serde_json::Value = serde_json::from_slice(&body).expect("parse json");
        assert_eq!(
            json.get("manifest_hash")
                .and_then(serde_json::Value::as_str),
            Some(published.pointer.manifest_hash.as_str())
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn version_resolve_returns_gone_for_yanked_release() {
        let tmp = tempfile::tempdir().expect("tempdir");
        initialize_storage(tmp.path()).expect("init");
        let store = RegistryStore::open(tmp.path()).expect("open store");
        let manifest = "schema_version = \"0.2\"\nname = \"sample\"\nversion = \"1.0.0\"\ntype = \"app\"\ndefault_target = \"cli\"\n";
        let capsule = build_capsule_bytes(manifest);
        let published = store
            .publish_registry_release(
                "koh0920",
                "sample",
                "sample",
                "demo",
                "1.0.0",
                "sample-1.0.0.capsule",
                "sha256:abc",
                "blake3:def",
                capsule.len() as u64,
                &capsule,
                "2026-03-05T00:00:00Z",
            )
            .expect("publish");
        store
            .yank_manifest("koh0920/sample", &published.pointer.manifest_hash)
            .expect("yank");

        let state = AppState {
            listen_url: "http://127.0.0.1:8787".to_string(),
            data_dir: tmp.path().to_path_buf(),
            auth_token: None,
            lock: Arc::new(Mutex::new(())),
        };
        let response = handle_manifest_resolve_version(
            State(state),
            HeaderMap::new(),
            AxumPath((
                "koh0920".to_string(),
                "sample".to_string(),
                "1.0.0".to_string(),
            )),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::GONE);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn persistent_state_local_api_registers_and_lists_records() {
        let (_home, _home_guard, manifest_path, bind_dir, state) = {
            let _guard = env_lock().lock().expect("env lock");
            let home = tempfile::tempdir().expect("home");
            let home_guard = HomeGuard::set(home.path());

            let manifest_dir = home.path().join("workspace");
            std::fs::create_dir_all(&manifest_dir).expect("create manifest dir");
            let manifest_path = manifest_dir.join("capsule.toml");
            std::fs::write(
                &manifest_path,
                r#"
schema_version = "0.2"
name = "demo-app"
version = "0.1.0"
type = "app"
default_target = "app"

[targets.app]
runtime = "oci"
image = "ghcr.io/example/app:latest"

[state.data]
kind = "filesystem"
durability = "persistent"
purpose = "primary-data"
attach = "explicit"
schema_id = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[services.main]
target = "app"

[[services.main.state_bindings]]
state = "data"
target = "/var/lib/app"
"#,
            )
            .expect("write manifest");

            let bind_dir = home.path().join("bind").join("data");
            let state = AppState {
                listen_url: "http://127.0.0.1:8787".to_string(),
                data_dir: home.path().to_path_buf(),
                auth_token: None,
                lock: Arc::new(Mutex::new(())),
            };

            (home, home_guard, manifest_path, bind_dir, state)
        };

        let register_response = handle_register_persistent_state(
            State(state.clone()),
            HeaderMap::new(),
            Json(RegisterPersistentStateRequest {
                manifest: manifest_path.to_string_lossy().to_string(),
                state_name: "data".to_string(),
                path: bind_dir.to_string_lossy().to_string(),
            }),
        )
        .await
        .into_response();
        let register_status = register_response.status();
        let register_body = to_bytes(register_response.into_body(), usize::MAX)
            .await
            .expect("read register body");
        assert_eq!(register_status, StatusCode::CREATED);
        let registered: crate::registry_store::PersistentStateRecord =
            serde_json::from_slice(&register_body).expect("parse register json");
        assert_eq!(registered.owner_scope, "demo-app");
        assert_eq!(registered.state_name, "data");
        assert_eq!(registered.kind, "filesystem");
        assert_eq!(registered.backend_kind, "host_path");

        let list_response = handle_list_persistent_states(
            State(state.clone()),
            HeaderMap::new(),
            Query(PersistentStateListQuery {
                owner_scope: Some("demo-app".to_string()),
                state_name: Some("data".to_string()),
            }),
        )
        .await
        .into_response();
        assert_eq!(list_response.status(), StatusCode::OK);
        let list_body = to_bytes(list_response.into_body(), usize::MAX)
            .await
            .expect("read list body");
        let listed: Vec<crate::registry_store::PersistentStateRecord> =
            serde_json::from_slice(&list_body).expect("parse list json");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0], registered);

        let get_response = handle_get_persistent_state(
            State(state),
            HeaderMap::new(),
            AxumPath(registered.state_id.clone()),
        )
        .await
        .into_response();
        assert_eq!(get_response.status(), StatusCode::OK);
        let get_body = to_bytes(get_response.into_body(), usize::MAX)
            .await
            .expect("read get body");
        let fetched: crate::registry_store::PersistentStateRecord =
            serde_json::from_slice(&get_body).expect("parse get json");
        assert_eq!(fetched, registered);
    }
}
