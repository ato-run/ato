use std::collections::HashMap;
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
#[cfg(feature = "webui")]
use axum::http::Uri;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use capsule_core::capsule::manifest::validate_blake3_digest;
use capsule_core::capsule::{verify_artifact_hash, CasStore};
use chrono::Utc;
#[cfg(feature = "webui")]
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;
use tower_http::cors::{Any, CorsLayer};

use crate::application::ports::publish::{PublishArtifactIdentityClass, PublishArtifactMetadata};
use crate::artifact_hash::{
    compute_blake3_label as compute_blake3, compute_sha256_label as compute_sha256, equals_hash,
};
use crate::binding;
use crate::publish_artifact::{
    ChunkUploadResponse, SyncCommitRequest, SyncCommitResponse, SyncNegotiateRequest,
    SyncNegotiateResponse,
};
use crate::registry::store::{
    EpochResolveRequest, KeyRevokeRequest, KeyRotateRequest, LeaseRefreshRequest,
    LeaseReleaseRequest, NegotiateRequest, RegistryStore, RollbackRequest, YankRequest,
};
use crate::runtime::process::{ProcessInfo, ProcessManager, ProcessStatus};

mod auth;
mod http;
mod local_api;
mod local_capsule;
mod local_service;
mod metadata_api;
mod registry_storage;
mod routes;
mod runtime_support;
#[cfg(feature = "webui")]
mod ui;

use auth::*;
use http::*;
use local_api::*;
use local_capsule::*;
use local_service::*;
use metadata_api::*;
use registry_storage::*;
use routes::*;
use runtime_support::*;
#[cfg(feature = "webui")]
use ui::*;

const README_CANDIDATES: [&str; 4] = ["README.md", "README.mdx", "README.txt", "README"];
const README_MAX_BYTES: usize = 512 * 1024;
#[cfg(feature = "webui")]
const LOCAL_REGISTRY_DISABLE_UI_ENV: &str = "ATO_LOCAL_REGISTRY_DISABLE_UI";

#[cfg(feature = "webui")]
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
    lock_id: Option<String>,
    #[serde(default)]
    closure_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    publish_metadata: Option<PublishArtifactMetadata>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    lock_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    closure_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    publish_metadata: Option<PublishArtifactMetadata>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    lock_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    closure_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    publish_metadata: Option<PublishArtifactMetadata>,
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

fn parse_publish_metadata_headers(
    headers: &HeaderMap,
) -> std::result::Result<Option<PublishArtifactMetadata>, String> {
    let Some(identity_class) = get_optional_header(headers, "x-ato-publish-identity-class") else {
        return Ok(None);
    };
    let identity_class = PublishArtifactIdentityClass::parse(&identity_class).ok_or_else(|| {
        format!(
            "unsupported x-ato-publish-identity-class '{}'",
            identity_class
        )
    })?;
    let delivery_mode = get_optional_header(headers, "x-ato-publish-delivery-mode");
    let provenance_limited =
        match get_optional_header(headers, "x-ato-publish-provenance-limited").as_deref() {
            Some(value) if value.eq_ignore_ascii_case("true") => true,
            Some(value) if value.eq_ignore_ascii_case("false") => false,
            Some(value) => {
                return Err(format!(
                    "x-ato-publish-provenance-limited must be 'true' or 'false' (got '{}')",
                    value
                ))
            }
            None => false,
        };
    Ok(Some(PublishArtifactMetadata {
        identity_class,
        delivery_mode,
        provenance_limited,
    }))
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

    #[cfg(feature = "webui")]
    let ui_enabled = std::env::var_os(LOCAL_REGISTRY_DISABLE_UI_ENV).is_none();
    #[cfg(not(feature = "webui"))]
    let ui_enabled = false;

    let mut app = build_app_router(ui_enabled).with_state(state);

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
    #[cfg(feature = "webui")]
    if ui_enabled && LocalRegistryUiAssets::get("index.html").is_none() {
        println!("⚠️  Web UI assets are missing. Rebuild with `cargo build --features webui` after installing npm deps in apps/ato-store-local.");
    }
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("Local registry server failed")?;
    Ok(())
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
                    lock_id: release.lock_id.clone(),
                    closure_digest: release.closure_digest.clone(),
                    publish_metadata: release.publish_metadata.clone(),
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
    let lock_id = get_optional_header(&headers, "x-ato-lock-id");
    let closure_digest = get_optional_header(&headers, "x-ato-closure-digest");
    let publish_metadata = match parse_publish_metadata_headers(&headers) {
        Ok(value) => value,
        Err(err) => return json_error(StatusCode::BAD_REQUEST, "invalid_publish_metadata", &err),
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
                        lock_id: existing_release.lock_id.clone(),
                        closure_digest: existing_release.closure_digest.clone(),
                        publish_metadata: existing_release.publish_metadata.clone(),
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
        lock_id.as_deref(),
        closure_digest.as_deref(),
        publish_metadata.as_ref(),
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
            lock_id,
            closure_digest,
            publish_metadata,
        }),
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
mod tests;
