use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct PublishArtifactArgs {
    pub artifact_path: PathBuf,
    pub scoped_id: String,
    pub registry_url: String,
    pub force_large_payload: bool,
    pub allow_existing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishArtifactResult {
    pub scoped_id: String,
    pub version: String,
    pub artifact_url: String,
    pub file_name: String,
    pub sha256: String,
    pub blake3: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub already_existed: bool,
}

#[derive(Debug)]
struct ArtifactPayload {
    publisher: String,
    slug: String,
    version: String,
    file_name: String,
    bytes: Vec<u8>,
    sha256: String,
    blake3: String,
    v3_manifest: Option<capsule_core::capsule_v3::CapsuleManifestV3>,
}

#[derive(Debug, Clone)]
struct V3SyncPayload {
    publisher: String,
    slug: String,
    version: String,
    manifest: capsule_core::capsule_v3::CapsuleManifestV3,
}

impl ArtifactPayload {
    fn v3_sync_payload(&self) -> Option<V3SyncPayload> {
        self.v3_manifest.as_ref().map(|manifest| V3SyncPayload {
            publisher: self.publisher.clone(),
            slug: self.slug.clone(),
            version: self.version.clone(),
            manifest: manifest.clone(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct ArtifactManifestInfo {
    pub name: String,
    pub version: String,
    pub repository_owner: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RegistryErrorPayload {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct SyncChunkDescriptor {
    pub(crate) raw_hash: String,
    pub(crate) raw_size: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct SyncNegotiateRequest {
    pub(crate) artifact_hash: String,
    pub(crate) schema_version: u32,
    pub(crate) chunks: Vec<SyncChunkDescriptor>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct SyncNegotiateResponse {
    pub(crate) missing_chunks: Vec<String>,
    #[serde(default)]
    pub(crate) total_chunks: usize,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ChunkUploadResponse {
    pub(crate) raw_hash: String,
    pub(crate) inserted: bool,
    pub(crate) zstd_size: u64,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct SyncCommitRequest {
    pub(crate) publisher: String,
    pub(crate) slug: String,
    pub(crate) version: String,
    pub(crate) manifest: capsule_core::capsule_v3::CapsuleManifestV3,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct SyncCommitResponse {
    pub(crate) scoped_id: String,
    pub(crate) version: String,
    pub(crate) artifact_hash: String,
    pub(crate) chunk_count: usize,
    pub(crate) total_raw_size: u64,
}

#[derive(Debug, Error)]
pub enum PublishArtifactError {
    #[error("Artifact upload conflict (409 version_exists): {message}")]
    VersionExists { message: String },
    #[error("Artifact upload failed ({status}): {message}")]
    UploadFailed { status: u16, message: String },
}

pub fn publish_artifact(args: PublishArtifactArgs) -> Result<PublishArtifactResult> {
    let base_url = normalize_registry_url(&args.registry_url)?;
    crate::payload_guard::ensure_payload_size(
        &args.artifact_path,
        args.force_large_payload,
        "--force-large-payload",
    )?;
    let payload = load_artifact_payload(&args.artifact_path, &args.scoped_id)?;
    let v3_sync_payload = payload.v3_sync_payload();
    let endpoint = build_upload_endpoint(
        &base_url,
        &payload.publisher,
        &payload.slug,
        &payload.version,
        &payload.file_name,
        args.allow_existing,
    );

    let request = crate::registry_http::blocking_client_builder(&base_url)
        .build()
        .context("Failed to create registry upload client")?
        .put(&endpoint)
        .header("content-type", "application/octet-stream")
        .header("x-ato-sha256", &payload.sha256)
        .header("x-ato-blake3", &payload.blake3);

    let request = if let Some(token) = read_ato_token() {
        request.header("authorization", format!("Bearer {}", token))
    } else {
        request
    };

    let response = request
        .body(payload.bytes)
        .send()
        .map_err(|err| anyhow::anyhow!("Failed to upload artifact to {}: {}", endpoint, err))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        let error = classify_upload_failure(status, &body);
        return Err(error.into());
    }

    let result = response
        .json::<PublishArtifactResult>()
        .context("Invalid local registry upload response")?;
    sync_v3_chunks_if_present(&base_url, v3_sync_payload.as_ref())
        .with_context(|| "Failed to finalize payload v3 metadata for uploaded release")?;
    Ok(result)
}

pub fn inspect_artifact_manifest(path: &Path) -> Result<ArtifactManifestInfo> {
    if !path.exists() {
        bail!("Artifact not found: {}", path.display());
    }
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| !ext.eq_ignore_ascii_case("capsule"))
        .unwrap_or(true)
    {
        bail!("--artifact must point to a .capsule file");
    }

    let bytes = std::fs::read(path)
        .with_context(|| format!("Failed to read artifact: {}", path.display()))?;
    let manifest = extract_manifest_from_capsule(&bytes)?;
    let parsed = capsule_core::types::CapsuleManifest::from_toml(&manifest)
        .map_err(|err| anyhow::anyhow!("Failed to parse capsule.toml from artifact: {}", err))?;

    Ok(ArtifactManifestInfo {
        name: parsed.name,
        version: parsed.version,
        repository_owner: extract_repository_owner(&manifest),
    })
}

fn build_upload_endpoint(
    base_url: &str,
    publisher: &str,
    slug: &str,
    version: &str,
    file_name: &str,
    allow_existing: bool,
) -> String {
    let mut endpoint = format!(
        "{}/v1/local/capsules/{}/{}/{}?file_name={}",
        base_url,
        urlencoding::encode(publisher),
        urlencoding::encode(slug),
        urlencoding::encode(version),
        urlencoding::encode(file_name)
    );
    if allow_existing {
        endpoint.push_str("&allow_existing=true");
    }
    endpoint
}

fn load_artifact_payload(path: &Path, scoped_id: &str) -> Result<ArtifactPayload> {
    if !path.exists() {
        bail!("Artifact not found: {}", path.display());
    }
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| !ext.eq_ignore_ascii_case("capsule"))
        .unwrap_or(true)
    {
        bail!("--artifact must point to a .capsule file");
    }

    let scoped = crate::install::parse_capsule_ref(scoped_id)?;
    let bytes = std::fs::read(path)
        .with_context(|| format!("Failed to read artifact: {}", path.display()))?;
    let manifest = extract_manifest_from_capsule(&bytes)?;
    let v3_manifest = extract_payload_v3_manifest_from_capsule(&bytes)?;
    let parsed = capsule_core::types::CapsuleManifest::from_toml(&manifest)
        .map_err(|err| anyhow::anyhow!("Failed to parse capsule.toml from artifact: {}", err))?;

    if parsed.name != scoped.slug {
        bail!(
            "--scoped-id slug '{}' must match artifact manifest.name '{}'",
            scoped.slug,
            parsed.name
        );
    }

    let file_name = format!("{}-{}.capsule", scoped.slug, parsed.version);

    Ok(ArtifactPayload {
        publisher: scoped.publisher,
        slug: scoped.slug,
        version: parsed.version,
        file_name,
        sha256: compute_sha256(&bytes),
        blake3: compute_blake3(&bytes),
        bytes,
        v3_manifest,
    })
}

fn extract_manifest_from_capsule(bytes: &[u8]) -> Result<String> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive
        .entries()
        .context("Failed to read .capsule archive entries")?;

    for entry in entries {
        let mut entry = entry.context("Invalid .capsule entry")?;
        let entry_path = entry
            .path()
            .context("Failed to read archive entry path")?
            .to_string_lossy()
            .to_string();
        if entry_path == "capsule.toml" {
            let mut manifest = String::new();
            entry
                .read_to_string(&mut manifest)
                .context("Failed to read capsule.toml from artifact")?;
            return Ok(manifest);
        }
    }

    bail!("Invalid artifact: capsule.toml not found in .capsule archive")
}

fn extract_payload_v3_manifest_from_capsule(
    bytes: &[u8],
) -> Result<Option<capsule_core::capsule_v3::CapsuleManifestV3>> {
    let mut archive = tar::Archive::new(Cursor::new(bytes));
    let entries = archive
        .entries()
        .context("Failed to read .capsule archive entries")?;

    for entry in entries {
        let mut entry = entry.context("Invalid .capsule entry")?;
        let entry_path = entry
            .path()
            .context("Failed to read archive entry path")?
            .to_string_lossy()
            .to_string();
        if entry_path != capsule_core::capsule_v3::V3_PAYLOAD_MANIFEST_PATH {
            continue;
        }

        let mut manifest_bytes = Vec::new();
        entry
            .read_to_end(&mut manifest_bytes)
            .context("Failed to read payload.v3.manifest.json from artifact")?;
        let manifest: capsule_core::capsule_v3::CapsuleManifestV3 =
            serde_json::from_slice(&manifest_bytes)
                .context("Failed to parse payload.v3.manifest.json from artifact")?;
        capsule_core::capsule_v3::verify_artifact_hash(&manifest)
            .context("Invalid payload.v3.manifest.json artifact_hash")?;
        return Ok(Some(manifest));
    }

    Ok(None)
}

fn sync_v3_chunks_if_present(base_url: &str, payload: Option<&V3SyncPayload>) -> Result<()> {
    let Some(payload) = payload else {
        return Ok(());
    };

    let cas = match capsule_core::capsule_v3::CasProvider::from_env() {
        capsule_core::capsule_v3::CasProvider::Enabled(store) => store,
        capsule_core::capsule_v3::CasProvider::Disabled(reason) => {
            capsule_core::capsule_v3::CasProvider::log_disabled_once(
                "publish_v3_chunk_sync",
                &reason,
            );
            return Ok(());
        }
    };

    let client = crate::registry_http::blocking_client_builder(base_url)
        .timeout(Duration::from_secs(30))
        .build()
        .context("Failed to create v3 sync client")?;
    let token = read_ato_token();

    let negotiate_request = SyncNegotiateRequest {
        artifact_hash: payload.manifest.artifact_hash.clone(),
        schema_version: payload.manifest.schema_version,
        chunks: payload
            .manifest
            .chunks
            .iter()
            .map(|chunk| SyncChunkDescriptor {
                raw_hash: chunk.raw_hash.clone(),
                raw_size: chunk.raw_size,
            })
            .collect(),
    };

    let negotiate_endpoint = format!("{}/v1/sync/negotiate", base_url);
    let mut negotiate = client.post(&negotiate_endpoint).json(&negotiate_request);
    if let Some(token) = token.as_deref() {
        negotiate = negotiate.bearer_auth(token);
    }
    let negotiate_response = negotiate.send().map_err(|err| {
        anyhow::anyhow!(
            "Failed to negotiate v3 payload sync via {}: {}",
            negotiate_endpoint,
            err
        )
    })?;
    if is_sync_not_supported_status(negotiate_response.status()) {
        return Ok(());
    }
    if !negotiate_response.status().is_success() {
        let status = negotiate_response.status();
        let body = negotiate_response.text().unwrap_or_default();
        bail!(
            "v3 sync negotiate failed ({}): {}",
            status.as_u16(),
            body.trim()
        );
    }
    let negotiate_body = negotiate_response
        .json::<SyncNegotiateResponse>()
        .context("Invalid v3 sync negotiate response")?;

    for chunk in payload.manifest.chunks.iter().filter(|chunk| {
        negotiate_body
            .missing_chunks
            .iter()
            .any(|hash| hash == &chunk.raw_hash)
    }) {
        let chunk_path = cas
            .chunk_path(&chunk.raw_hash)
            .with_context(|| format!("Failed to resolve local CAS chunk {}", chunk.raw_hash))?;
        let bytes = std::fs::read(&chunk_path)
            .with_context(|| format!("Failed to read local CAS chunk {}", chunk.raw_hash))?;
        let chunk_endpoint = format!(
            "{}/v1/chunks/{}",
            base_url,
            urlencoding::encode(&chunk.raw_hash)
        );
        let mut request = client
            .put(&chunk_endpoint)
            .header("content-type", "application/zstd")
            .header("x-raw-size", chunk.raw_size.to_string())
            .body(bytes);
        if let Some(token) = token.as_deref() {
            request = request.bearer_auth(token);
        }
        let response = request.send().map_err(|err| {
            anyhow::anyhow!(
                "Failed to upload v3 chunk {} via {}: {}",
                chunk.raw_hash,
                chunk_endpoint,
                err
            )
        })?;
        if is_sync_not_supported_status(response.status()) {
            return Ok(());
        }
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            bail!(
                "v3 chunk upload failed for {} ({}) : {}",
                chunk.raw_hash,
                status.as_u16(),
                body.trim()
            );
        }
        response
            .json::<ChunkUploadResponse>()
            .context("Invalid v3 chunk upload response")?;
    }

    let commit_endpoint = format!("{}/v1/sync/commit", base_url);
    let mut commit = client.post(&commit_endpoint).json(&SyncCommitRequest {
        publisher: payload.publisher.clone(),
        slug: payload.slug.clone(),
        version: payload.version.clone(),
        manifest: payload.manifest.clone(),
    });
    if let Some(token) = token.as_deref() {
        commit = commit.bearer_auth(token);
    }
    let commit_response = commit.send().map_err(|err| {
        anyhow::anyhow!(
            "Failed to commit v3 payload sync via {}: {}",
            commit_endpoint,
            err
        )
    })?;
    if is_sync_not_supported_status(commit_response.status()) {
        return Ok(());
    }
    if !commit_response.status().is_success() {
        let status = commit_response.status();
        let body = commit_response.text().unwrap_or_default();
        bail!(
            "v3 sync commit failed ({}): {}",
            status.as_u16(),
            body.trim()
        );
    }
    commit_response
        .json::<SyncCommitResponse>()
        .context("Invalid v3 sync commit response")?;

    Ok(())
}

fn is_sync_not_supported_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED
    )
}

fn normalize_registry_url(raw: &str) -> Result<String> {
    crate::registry_http::normalize_registry_url(raw, "--registry")
}

fn classify_upload_failure(status: StatusCode, body: &str) -> PublishArtifactError {
    let parsed = serde_json::from_str::<RegistryErrorPayload>(body).ok();
    if is_version_exists_conflict(status, parsed.as_ref(), body) {
        let message = parsed
            .as_ref()
            .and_then(|v| v.message.as_deref())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("same version is already published")
            .to_string();
        return PublishArtifactError::VersionExists { message };
    }

    let message = parsed
        .as_ref()
        .and_then(|v| v.message.as_deref())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| body.trim())
        .to_string();

    PublishArtifactError::UploadFailed {
        status: status.as_u16(),
        message,
    }
}

fn is_version_exists_conflict(
    status: StatusCode,
    parsed: Option<&RegistryErrorPayload>,
    raw_body: &str,
) -> bool {
    if status != StatusCode::CONFLICT {
        return false;
    }

    if parsed
        .and_then(|v| v.error.as_deref())
        .map(|v| v.eq_ignore_ascii_case("version_exists"))
        .unwrap_or(false)
    {
        return true;
    }

    let message = parsed
        .and_then(|v| v.message.as_deref())
        .unwrap_or(raw_body)
        .to_ascii_lowercase();
    message.contains("same version is already published")
        || message.contains("version_exists")
        || message.contains("sha256 mismatch")
}

fn extract_repository_owner(manifest_raw: &str) -> Option<String> {
    let raw = crate::publish_preflight::find_manifest_repository(manifest_raw)?;
    let normalized = crate::publish_preflight::normalize_repository_value(&raw).ok()?;
    let (owner, _) = normalized.split_once('/')?;
    let owner = normalize_segment(owner);
    if owner.is_empty() {
        None
    } else {
        Some(owner)
    }
}

fn normalize_segment(input: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;

    for ch in input.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            prev_dash = false;
            continue;
        }
        if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }

    out.trim_matches('-').to_string()
}

fn read_ato_token() -> Option<String> {
    crate::auth::current_session_token()
}

fn compute_blake3(data: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(data);
    let hash = hasher.finalize();
    format!("blake3:{}", hex::encode(hash.as_bytes()))
}

fn compute_sha256(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    #![allow(dead_code)]

    use super::*;
    use std::collections::HashMap;
    use std::ffi::{OsStr, OsString};
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    use axum::extract::{Path as AxumPath, State};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::{post, put};
    use axum::{Json, Router};
    use capsule_core::capsule_v3::{set_artifact_hash, CapsuleManifestV3, ChunkMeta};
    use tar::Builder;
    use tokio::sync::Mutex as AsyncMutex;
    use tokio::time::sleep;

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_lock() -> &'static Mutex<()> {
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<&OsStr>) -> Self {
            let previous = std::env::var_os(key);
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[derive(Clone)]
    struct SyncMockState {
        missing_chunks: Vec<String>,
        fixed_put_status: Option<StatusCode>,
        per_hash_transient_failures: Arc<AsyncMutex<HashMap<String, usize>>>,
        uploaded_hashes: Arc<AsyncMutex<Vec<String>>>,
        put_attempts: Arc<AsyncMutex<HashMap<String, usize>>>,
        negotiate_calls: Arc<AtomicUsize>,
        commit_calls: Arc<AtomicUsize>,
        put_calls: Arc<AtomicUsize>,
        inflight_puts: Arc<AtomicUsize>,
        max_inflight_puts: Arc<AtomicUsize>,
        put_delay: Duration,
    }

    impl SyncMockState {
        fn new(missing_chunks: Vec<String>) -> Self {
            Self {
                missing_chunks,
                fixed_put_status: None,
                per_hash_transient_failures: Arc::new(AsyncMutex::new(HashMap::new())),
                uploaded_hashes: Arc::new(AsyncMutex::new(Vec::new())),
                put_attempts: Arc::new(AsyncMutex::new(HashMap::new())),
                negotiate_calls: Arc::new(AtomicUsize::new(0)),
                commit_calls: Arc::new(AtomicUsize::new(0)),
                put_calls: Arc::new(AtomicUsize::new(0)),
                inflight_puts: Arc::new(AtomicUsize::new(0)),
                max_inflight_puts: Arc::new(AtomicUsize::new(0)),
                put_delay: Duration::from_millis(0),
            }
        }

        fn with_fixed_put_status(mut self, status: StatusCode) -> Self {
            self.fixed_put_status = Some(status);
            self
        }

        fn with_put_delay(mut self, delay: Duration) -> Self {
            self.put_delay = delay;
            self
        }
    }

    struct InflightGuard {
        state: SyncMockState,
    }

    impl Drop for InflightGuard {
        fn drop(&mut self) {
            self.state.inflight_puts.fetch_sub(1, Ordering::SeqCst);
        }
    }

    async fn sync_mock_negotiate(
        State(state): State<SyncMockState>,
        _body: axum::body::Bytes,
    ) -> impl IntoResponse {
        state.negotiate_calls.fetch_add(1, Ordering::SeqCst);
        Json(serde_json::json!({
            "missing_chunks": state.missing_chunks,
        }))
    }

    async fn sync_mock_put_chunk(
        State(state): State<SyncMockState>,
        AxumPath(raw_hash): AxumPath<String>,
        _body: axum::body::Bytes,
    ) -> impl IntoResponse {
        state.put_calls.fetch_add(1, Ordering::SeqCst);
        let current = state.inflight_puts.fetch_add(1, Ordering::SeqCst) + 1;
        let _guard = InflightGuard {
            state: state.clone(),
        };
        update_max(&state.max_inflight_puts, current);

        let attempt = {
            let mut attempts = state.put_attempts.lock().await;
            let entry = attempts.entry(raw_hash.clone()).or_insert(0);
            *entry += 1;
            *entry
        };

        if !state.put_delay.is_zero() {
            sleep(state.put_delay).await;
        }

        if let Some(status) = state.fixed_put_status {
            return (status, Json(serde_json::json!({"error":"forced"}))).into_response();
        }

        let should_fail = {
            let mut failures = state.per_hash_transient_failures.lock().await;
            if let Some(remaining) = failures.get_mut(&raw_hash) {
                if *remaining > 0 {
                    *remaining -= 1;
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };
        if should_fail {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error":"transient","attempt":attempt})),
            )
                .into_response();
        }

        state.uploaded_hashes.lock().await.push(raw_hash);
        Json(serde_json::json!({"ok": true})).into_response()
    }

    async fn sync_mock_commit(
        State(state): State<SyncMockState>,
        _body: axum::body::Bytes,
    ) -> impl IntoResponse {
        state.commit_calls.fetch_add(1, Ordering::SeqCst);
        Json(serde_json::json!({"ok": true}))
    }

    fn update_max(max: &AtomicUsize, candidate: usize) {
        let mut current = max.load(Ordering::SeqCst);
        while candidate > current {
            match max.compare_exchange(current, candidate, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }

    async fn start_sync_mock_server(state: SyncMockState) -> (String, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/v1/sync/negotiate", post(sync_mock_negotiate))
            .route("/v1/chunks/:raw_hash", put(sync_mock_put_chunk))
            .route("/v1/sync/commit", post(sync_mock_commit))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve mock");
        });
        (format!("http://{}", addr), handle)
    }

    fn compress_chunk(data: &[u8]) -> Vec<u8> {
        let mut encoder = zstd::Encoder::new(Vec::new(), 3).expect("encoder");
        encoder.write_all(data).expect("write");
        encoder.finish().expect("finish")
    }

    fn build_v3_sync_test_payload(
        cas: &capsule_core::capsule_v3::CasStore,
        chunk_count: usize,
    ) -> (V3SyncPayload, Vec<String>) {
        let mut chunks = Vec::new();
        let mut hashes = Vec::new();

        for i in 0..chunk_count {
            let raw = vec![(i % 251) as u8; 2_048 + (i % 7) * 13];
            let raw_hash = capsule_core::capsule_v3::manifest::blake3_digest(&raw);
            let zstd = compress_chunk(&raw);
            cas.put_chunk_zstd(&raw_hash, &zstd)
                .expect("write local CAS chunk");
            hashes.push(raw_hash.clone());
            chunks.push(ChunkMeta {
                raw_hash,
                raw_size: raw.len() as u32,
                zstd_size_hint: Some(zstd.len() as u32),
            });
        }

        let mut manifest = CapsuleManifestV3::new(chunks);
        set_artifact_hash(&mut manifest).expect("artifact hash");

        (
            V3SyncPayload {
                publisher: "local".to_string(),
                slug: "sync-app".to_string(),
                version: "1.0.0".to_string(),
                manifest,
            },
            hashes,
        )
    }

    fn test_capsule_bytes(name: &str, version: &str) -> Vec<u8> {
        let manifest = format!(
            r#"schema_version = "0.2"
name = "{name}"
version = "{version}"
type = "app"
default_target = "cli"

[targets.cli]
runtime = "source"
driver = "deno"
entrypoint = "main.ts"
"#
        );
        let mut buf = Vec::<u8>::new();
        {
            let mut builder = Builder::new(&mut buf);
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            header.set_size(manifest.len() as u64);
            header.set_cksum();
            builder
                .append_data(&mut header, "capsule.toml", manifest.as_bytes())
                .expect("append manifest");

            let sig = r#"{"signed":false}"#;
            let mut sig_header = tar::Header::new_gnu();
            sig_header.set_mode(0o644);
            sig_header.set_size(sig.len() as u64);
            sig_header.set_cksum();
            builder
                .append_data(&mut sig_header, "signature.json", sig.as_bytes())
                .expect("append signature");
            builder.finish().expect("finish tar");
        }
        buf
    }

    fn test_capsule_bytes_with_v3_manifest(name: &str, version: &str) -> Vec<u8> {
        let bytes = test_capsule_bytes(name, version);
        let mut v3 = CapsuleManifestV3::new(vec![ChunkMeta {
            raw_hash: "blake3:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                .to_string(),
            raw_size: 0,
            zstd_size_hint: Some(0),
        }]);
        // Keep valid non-zero chunk size for validation path.
        v3.chunks[0].raw_size = 1;
        v3.total_raw_size = 1;
        set_artifact_hash(&mut v3).expect("set artifact hash");

        let mut rebuilt = Vec::<u8>::new();
        {
            let mut builder = Builder::new(&mut rebuilt);
            let mut archive = tar::Archive::new(Cursor::new(bytes));
            let entries = archive.entries().expect("entries");
            for entry in entries {
                let mut entry = entry.expect("entry");
                let path = entry.path().expect("path").to_string_lossy().to_string();
                let mut content = Vec::new();
                entry.read_to_end(&mut content).expect("read entry");
                let mut header = tar::Header::new_gnu();
                header.set_mode(0o644);
                header.set_size(content.len() as u64);
                header.set_cksum();
                builder
                    .append_data(&mut header, path, Cursor::new(content))
                    .expect("append existing entry");
            }

            let manifest_bytes = serde_jcs::to_vec(&v3).expect("serialize v3");
            let mut v3_header = tar::Header::new_gnu();
            v3_header.set_mode(0o644);
            v3_header.set_size(manifest_bytes.len() as u64);
            v3_header.set_cksum();
            builder
                .append_data(
                    &mut v3_header,
                    capsule_core::capsule_v3::V3_PAYLOAD_MANIFEST_PATH,
                    Cursor::new(manifest_bytes),
                )
                .expect("append v3 manifest");
            builder.finish().expect("finish tar");
        }
        rebuilt
    }

    #[test]
    fn extract_manifest_from_capsule_succeeds() {
        let bytes = test_capsule_bytes("sample-capsule", "1.0.0");
        let manifest = extract_manifest_from_capsule(&bytes).expect("extract manifest");
        assert!(manifest.contains("name = \"sample-capsule\""));
        assert!(manifest.contains("version = \"1.0.0\""));
    }

    #[test]
    fn slug_mismatch_is_rejected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("sample-capsule.capsule");
        std::fs::write(&path, test_capsule_bytes("sample-capsule", "1.0.0")).expect("write");

        let err = load_artifact_payload(&path, "koh0920/another-slug").expect_err("must fail");
        assert!(err
            .to_string()
            .contains("must match artifact manifest.name"));
    }

    #[test]
    fn hash_generation_is_stable() {
        let data = b"capsule-bytes";
        let s1 = compute_sha256(data);
        let s2 = compute_sha256(data);
        let b1 = compute_blake3(data);
        let b2 = compute_blake3(data);
        assert_eq!(s1, s2);
        assert_eq!(b1, b2);
        assert!(s1.starts_with("sha256:"));
        assert!(b1.starts_with("blake3:"));
    }

    #[test]
    fn build_upload_endpoint_appends_allow_existing() {
        let endpoint = build_upload_endpoint(
            "http://127.0.0.1:8787",
            "local",
            "demo-app",
            "1.0.0",
            "demo-app-1.0.0.capsule",
            true,
        );
        assert!(endpoint.contains("allow_existing=true"));
        assert!(endpoint.contains("file_name=demo-app-1.0.0.capsule"));
    }

    #[test]
    fn build_upload_endpoint_omits_allow_existing_by_default() {
        let endpoint = build_upload_endpoint(
            "http://127.0.0.1:8787",
            "local",
            "demo-app",
            "1.0.0",
            "demo-app-1.0.0.capsule",
            false,
        );
        assert!(!endpoint.contains("allow_existing="));
    }

    #[test]
    fn build_upload_endpoint_uses_canonical_api_base() {
        let endpoint = build_upload_endpoint(
            "https://api.ato.run",
            "koh0920",
            "demo-app",
            "1.0.0",
            "demo-app-1.0.0.capsule",
            false,
        );

        assert!(
            endpoint.starts_with("https://api.ato.run/v1/local/capsules/koh0920/demo-app/1.0.0")
        );
    }

    #[test]
    fn classify_upload_failure_detects_version_exists_from_status_and_message() {
        let err = classify_upload_failure(
            StatusCode::CONFLICT,
            r#"{"error":"other","message":"same version is already published"}"#,
        );
        assert!(matches!(err, PublishArtifactError::VersionExists { .. }));
    }
}
